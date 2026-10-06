//! Host-owned Session navigation metadata and bounded durable queries.
#![deny(unsafe_code)]
#![warn(missing_docs)]
#![allow(clippy::missing_errors_doc)]
use async_trait::async_trait;
use futures_util::{FutureExt, StreamExt, future::BoxFuture, stream};
use rsi_agent_session_protocol::{ExecutionCoordinates, SessionId};
use rsi_agent_store_protocol::{SessionStore, SessionStoreContract};
use rsi_api_protocol::{
    ApiError, ApiRegistrarContract, CallOrigin, ConnectionDescriptionContract, HostEpoch, Result,
};
use rsi_meta::{
    ActivationPlan, ConfigValue, Execution, LocalContract, MetaError, PluginFactory,
    PreparedActivation,
};
use rsi_navigation_api::{
    MetadataReceipt, NavigationCursor, NavigationEntry, NavigationFilter, NavigationPage,
    PinnedEntry, PinnedPage, SessionMetadata, WorkspaceFilter, matches_query, revision,
};
use rsi_session_protocol::{SessionContract, SessionError, SessionService};
use rsi_storage_domain::storage_error;
use rsi_storage_domain::{
    Domain, DomainFacilityContract, DomainSpec, RecordObjectSize, encoded_entry_bytes,
};
use rsi_workspace_protocol::{
    WorkspaceError, WorkspaceId, WorkspaceRegistry, WorkspaceRegistryContract,
};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex},
};
use tokio::sync::Semaphore;
use tokio_util::task::TaskTracker;
mod attention;
mod endpoint;
mod order;
pub use attention::AttentionFactory;

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Document {
    revision: u64,
    records: BTreeMap<SessionId, SessionMetadata>,
    #[serde(skip)]
    accounting: Option<MetadataSize>,
}
#[derive(Clone, Copy, Debug, Default)]
struct MetadataSize {
    records: RecordObjectSize,
    pins: usize,
}
impl MetadataSize {
    fn check(self, revision: u64) -> Result<()> {
        if self.pins > 64 {
            return Err(ApiError::Invalid(
                "at most 64 sessions may be pinned".into(),
            ));
        }
        // The Domain record wrapper is intentional: it counts toward the 8 MiB bound.
        let bytes = b"{\"metadata\":{\"revision\":,\"records\":}}".len()
            + revision.to_string().len()
            + self.records.bytes();
        if self.records.records() > 8192 || bytes > 8 * 1024 * 1024 {
            return Err(ApiError::Invalid(
                "navigation metadata exceeds 8192 records or 8 MiB".into(),
            ));
        }
        Ok(())
    }
}
fn metadata_bytes(id: &SessionId, metadata: &SessionMetadata) -> Result<usize> {
    encoded_entry_bytes(
        id.as_str(),
        serde_json::to_vec(metadata)
            .map_err(|_| ApiError::Invalid("invalid navigation metadata".into()))?
            .len(),
    )
    .map_err(storage_error)
}
impl Document {
    fn validate(&mut self) -> Result<()> {
        let mut accounting = MetadataSize::default();
        for (id, record) in &self.records {
            record.validate()?;
            accounting.records = accounting
                .records
                .with_entry(None, metadata_bytes(id, record)?)
                .map_err(storage_error)?;
            accounting.pins += usize::from(record.pinned);
        }
        accounting.check(self.revision)?;
        self.accounting = Some(accounting);
        Ok(())
    }
    fn edit(&mut self, session: &SessionId, metadata: &SessionMetadata) -> Result<()> {
        if self.accounting.is_none() {
            self.validate()?;
        }
        let mut accounting = self.accounting.expect("validated metadata accounting");
        let previous = self.records.get(session);
        let previous_bytes = previous
            .map(|old| metadata_bytes(session, old))
            .transpose()?;
        accounting.pins -= usize::from(previous.is_some_and(|old| old.pinned));
        let remove = *metadata == SessionMetadata::default();
        if remove {
            if let Some(bytes) = previous_bytes {
                accounting.records = accounting
                    .records
                    .without_entry(bytes)
                    .map_err(storage_error)?;
            }
        } else {
            accounting.records = accounting
                .records
                .with_entry(previous_bytes, metadata_bytes(session, metadata)?)
                .map_err(storage_error)?;
            accounting.pins += usize::from(metadata.pinned);
        }
        let revision = self
            .revision
            .checked_add(1)
            .ok_or_else(|| ApiError::Invalid("navigation revision exhausted".into()))?;
        accounting.check(revision)?;
        if remove {
            self.records.remove(session);
        } else {
            self.records.insert(session.clone(), metadata.clone());
        }
        self.revision = revision;
        self.accounting = Some(accounting);
        Ok(())
    }
}
#[derive(Debug)]
struct State {
    closed: bool,
    document: Arc<Document>,
}
/// One Host's navigation owner; it never owns Session execution.
#[derive(Debug)]
pub struct Navigation {
    domain: Arc<dyn Domain>,
    session: Arc<dyn SessionService>,
    resolver: Arc<dyn rsi_execution::ExecutionResolver>,
    store: Arc<dyn SessionStore>,
    protection: Option<Arc<dyn rsi_session_protocol::SessionProtection>>,
    cursors: Mutex<CursorBook>,
    workspace: Arc<dyn WorkspaceRegistry>,
    epoch: HostEpoch,
    state: Mutex<State>,
    slots: Arc<Semaphore>,
    writer: Arc<Semaphore>,
    tasks: TaskTracker,
    execution: Execution,
}
impl Navigation {
    fn run<T: Send + 'static>(
        self: &Arc<Self>,
        work: impl FnOnce(Arc<Self>) -> BoxFuture<'static, Result<T>>,
    ) -> Result<BoxFuture<'static, Result<T>>> {
        self.domain.ensure_available().map_err(storage_error)?;
        let state = self.state.lock().expect("navigation state poisoned");
        if state.closed {
            return Err(ApiError::ShuttingDown);
        }
        let permit = self
            .slots
            .clone()
            .try_acquire_owned()
            .map_err(|_| ApiError::Capacity)?;
        let future = work(self.clone());
        let task = self.execution.spawn(self.tasks.track_future(async move {
            let _permit = permit;
            future.await
        }));
        drop(state);
        Ok(Box::pin(async move {
            task.await.map_err(|_| ApiError::OutcomeUnknown)?
        }))
    }
    /// Admits a bounded filtered scan; empty results may still have a continuation.
    pub fn query(
        self: &Arc<Self>,
        origin: &CallOrigin,
        filter: NavigationFilter,
        after: Option<NavigationCursor>,
    ) -> Result<BoxFuture<'static, Result<NavigationPage>>> {
        filter.validate()?;
        let visibility = self.resolver.visibility(origin)?;
        let origin = origin.clone();
        self.run(move |owner| {
            Box::pin(async move { owner.scan(visibility, origin, filter, after).await })
        })
    }
    #[expect(
        clippy::too_many_lines,
        reason = "one bounded scan keeps visibility, cursor and per-page workspace lookup together"
    )]
    async fn scan(
        &self,
        visibility: rsi_execution::ExecutionVisibility,
        origin: CallOrigin,
        filter: NavigationFilter,
        after: Option<NavigationCursor>,
    ) -> Result<NavigationPage> {
        self.domain.ensure_available().map_err(storage_error)?;
        let document = self.document();
        if let Some(after) = &after
            && (after.filter != filter
                || after.host_epoch != self.epoch
                || revision(&after.metadata_revision)? != document.revision
                || after
                    .after
                    .as_ref()
                    .is_some_and(|key| key.last_activity_ms == 0))
        {
            return Err(ApiError::Invalid(
                "navigation changed; refresh this search".into(),
            ));
        }
        let coordinates = match &filter.workspace {
            WorkspaceFilter::Registered { id } => {
                let record = match self.workspace.get(id).await {
                    Ok(record) => record,
                    Err(WorkspaceError::Unknown(_)) => {
                        return Ok(NavigationPage {
                            metadata_revision: document.revision.to_string(),
                            newest: None,
                            entries: Vec::new(),
                            scanned: 0,
                            next: None,
                        });
                    }
                    Err(_) => return Err(ApiError::Unavailable),
                };
                if !visibility
                    .locations()
                    .contains(record.coordinates.location())
                {
                    return Err(ApiError::Unauthorized);
                }
                Some(record.coordinates)
            }
            _ => None,
        };
        let cut = after
            .as_ref()
            .map(|after| {
                self.cursors
                    .lock()
                    .map_err(|_| ApiError::Unavailable)?
                    .read(&origin, after)
            })
            .transpose()?;
        let page = self
            .store
            .list_session_activity(
                visibility.locations(),
                coordinates.as_ref(),
                cut.as_ref(),
                256,
            )
            .await
            .map_err(|_| ApiError::Unavailable)?;
        let mut entries = Vec::new();
        let mut leases = Vec::new();
        let mut scanned = 0;
        let mut cursor = None;
        let mut visible_cut = after.as_ref().and_then(|c| c.after.clone());
        let query = filter.query.to_lowercase();
        let default_metadata = SessionMetadata::default();
        let lookups = self.workspace_lookups(page.sessions.iter().map(|row| {
            let metadata = document
                .records
                .get(&row.session_id)
                .unwrap_or(&default_metadata);
            (metadata.archived == filter.archived
                && !metadata.pinned
                && matches_query(
                    &query,
                    &row.session_id,
                    metadata,
                    Some(row.coordinates.path()),
                ))
            .then_some(&row.coordinates)
        }));
        let mut rows = stream::iter(&page.sessions).zip(lookups);
        while let Some((row, workspace)) = rows.next().await {
            scanned += 1;
            cursor = Some(row.cursor());
            let id = &row.session_id;
            let metadata = document.records.get(id).cloned().unwrap_or_default();
            if metadata.archived != filter.archived || metadata.pinned {
                continue;
            }
            let path = row.coordinates.path();
            if !matches_query(&query, id, &metadata, Some(path)) {
                continue;
            }
            let workspace = workspace?;
            if !filter.workspace.matches(workspace.as_ref()) {
                continue;
            }
            let Some(lease) = self.scope_view(id, &origin).await? else {
                continue;
            };
            leases.push(lease);
            visible_cut = Some(row.cursor());
            entries.push(NavigationEntry {
                session: id.clone(),
                created_at_ms: row.created_at_ms.to_string(),
                last_activity_ms: row.last_activity_ms.to_string(),
                location: row.coordinates.location().clone(),
                path: path.into(),
                workspace,
                metadata,
            });
            if entries.len() == 64 {
                break;
            }
        }
        drop(rows);
        let more = scanned < page.sessions.len() || page.has_more;
        let newest = match page.newest {
            Some(key) => {
                if let Some(lease) = self.scope_view(&key.session_id, &origin).await? {
                    leases.push(lease);
                    Some(key)
                } else {
                    None
                }
            }
            None => None,
        };
        finish_scope(&origin, &leases)?;
        Ok(NavigationPage {
            newest,
            metadata_revision: document.revision.to_string(),
            entries,
            scanned: u16::try_from(scanned).map_err(|_| ApiError::Unavailable)?,
            next: if more {
                cursor
                    .map(|cut| {
                        self.cursors
                            .lock()
                            .map_err(|_| ApiError::Unavailable)?
                            .issue(
                                &origin,
                                filter,
                                self.epoch.clone(),
                                document.revision.to_string(),
                                cut,
                                visible_cut.clone(),
                                after.as_ref(),
                            )
                    })
                    .transpose()?
            } else {
                if let Some(after) = &after {
                    self.cursors
                        .lock()
                        .map_err(|_| ApiError::Unavailable)?
                        .release(&origin, after)?;
                }
                None
            },
        })
    }
    async fn workspace_for(
        &self,
        coordinates: &ExecutionCoordinates,
    ) -> Result<Option<WorkspaceId>> {
        let derived = WorkspaceId::from_coordinates(coordinates);
        match self.workspace.get(&derived).await {
            Ok(record) => Ok(Some(record.id)),
            Err(WorkspaceError::Unknown(_)) => Ok(None),
            Err(_) => Err(ApiError::Unavailable),
        }
    }
    // Shared futures retain one result per coordinate without starting unpolled lookups.
    fn workspace_lookups<'a>(
        &'a self,
        coordinates: impl IntoIterator<Item = Option<&'a ExecutionCoordinates>>,
    ) -> stream::BoxStream<'a, Result<Option<WorkspaceId>>> {
        let mut distinct = BTreeMap::new();
        let reads = coordinates
            .into_iter()
            .map(|coordinates| {
                let read = coordinates.map(|coordinates| {
                    distinct
                        .entry(coordinates.clone())
                        .or_insert_with(|| self.workspace_for(coordinates).boxed().shared())
                        .clone()
                });
                async move {
                    match read {
                        Some(read) => read.await,
                        None => Ok(None),
                    }
                }
                .boxed()
            })
            .collect::<Vec<_>>();
        stream::iter(reads).buffered(4).boxed()
    }
    /// Reads all pinned summaries with no attach or activity side effects.
    ///
    /// # Panics
    /// Panics if a prior panic poisoned navigation state.
    #[expect(
        clippy::too_many_lines,
        reason = "Keep cursor issuance and authorized delivery in one owning operation"
    )]
    pub fn pinned(
        self: &Arc<Self>,
        origin: CallOrigin,
        filter: NavigationFilter,
    ) -> Result<BoxFuture<'static, Result<PinnedPage>>> {
        filter.validate()?;
        let visibility = self.resolver.visibility(&origin)?;
        self.run(move |owner| {
            Box::pin(async move {
                owner.domain.ensure_available().map_err(storage_error)?;
                let document = owner
                    .state
                    .lock()
                    .expect("navigation state poisoned")
                    .document
                    .clone();
                let mut available = Vec::new();
                let mut leases = Vec::new();
                let mut missing = Vec::new();
                let query = filter.query.to_lowercase();
                let selected = document
                    .records
                    .iter()
                    .filter(|(_, metadata)| metadata.pinned && metadata.archived == filter.archived)
                    .map(|(id, metadata)| (id.clone(), metadata.clone()))
                    .collect::<Vec<_>>();
                let ids = selected
                    .iter()
                    .map(|(id, _)| id.clone())
                    .collect::<Vec<_>>();
                let rows = owner
                    .store
                    .session_activity_summaries(&ids)
                    .await
                    .map_err(|_| ApiError::Unavailable)?;
                let lookups = owner.workspace_lookups(rows.iter().zip(&selected).map(
                    |(row, (id, metadata))| {
                        row.as_ref()
                            .filter(|row| {
                                visibility.locations().contains(row.coordinates.location())
                                    && matches_query(
                                        &query,
                                        id,
                                        metadata,
                                        Some(row.coordinates.path()),
                                    )
                            })
                            .map(|row| &row.coordinates)
                    },
                ));
                let mut rows = stream::iter(selected.iter().zip(&rows)).zip(lookups);
                while let Some((((id, metadata), row), workspace)) = rows.next().await {
                    let Some(row) = row else {
                        let Some(lease) = owner.scope_view(id, &origin).await? else {
                            continue;
                        };
                        leases.push(lease);
                        if matches!(origin, CallOrigin::Local)
                            && filter.workspace == WorkspaceFilter::All
                            && matches_query(&query, id, metadata, None)
                        {
                            missing.push(PinnedEntry::Missing {
                                session: id.clone(),
                                metadata: metadata.clone(),
                            });
                        }
                        continue;
                    };
                    if !visibility.locations().contains(row.coordinates.location()) {
                        continue;
                    }
                    let Some(lease) = owner.scope_view(id, &origin).await? else {
                        continue;
                    };
                    leases.push(lease);
                    let path = row.coordinates.path();
                    if !matches_query(&query, id, metadata, Some(path)) {
                        continue;
                    }
                    let workspace = workspace?;
                    if !filter.workspace.matches(workspace.as_ref()) {
                        continue;
                    }
                    available.push((
                        row.last_activity_ms,
                        id.clone(),
                        PinnedEntry::Available {
                            entry: NavigationEntry {
                                session: id.clone(),
                                created_at_ms: row.created_at_ms.to_string(),
                                last_activity_ms: row.last_activity_ms.to_string(),
                                location: row.coordinates.location().clone(),
                                path: path.into(),
                                workspace,
                                metadata: metadata.clone(),
                            },
                        },
                    ));
                }
                available.sort_by(|a, b| (&b.0, &b.1).cmp(&(&a.0, &a.1)));
                let mut entries: Vec<_> = available.into_iter().map(|(_, _, row)| row).collect();
                entries.extend(missing.into_iter().rev());
                finish_scope(&origin, &leases)?;
                Ok(PinnedPage {
                    metadata_revision: document.revision.to_string(),
                    entries,
                })
            })
        })
    }
    /// Replaces navigation metadata once and retains the write independently of its waiter.
    ///
    /// # Panics
    /// Panics if an earlier panic poisoned this owner's state lock.
    pub fn replace(
        self: &Arc<Self>,
        origin: CallOrigin,
        session: SessionId,
        expected: &str,
        mut metadata: SessionMetadata,
    ) -> Result<BoxFuture<'static, Result<MetadataReceipt>>> {
        if metadata.archived {
            metadata.pinned = false;
        }
        metadata.validate()?;
        let expected = revision(expected)?;
        let permit = self
            .writer
            .clone()
            .try_acquire_owned()
            .map_err(|_| ApiError::Capacity)?;
        self.run(move |owner| {
            Box::pin(async move {
                let _permit = permit;
                let mut document = owner
                    .state
                    .lock()
                    .expect("navigation state poisoned")
                    .document
                    .as_ref()
                    .clone();
                if document.revision != expected {
                    return Err(ApiError::Invalid(
                        "navigation revision conflict; refresh before editing".into(),
                    ));
                }
                let _admission = match owner.session.read_header(&session).await {
                    Ok(header) => {
                        let _lease = owner
                            .scope_view(&session, &origin)
                            .await?
                            .ok_or(ApiError::Unauthorized)?;
                        Some(
                            owner
                                .resolver
                                .admit(&origin, header.coordinates().location())?,
                        )
                    }
                    Err(SessionError::NotFound(_))
                        if matches!(origin, CallOrigin::Local)
                            && document.records.get(&session).is_some_and(|old| {
                                metadata == SessionMetadata::default()
                                    || old.pinned && !metadata.pinned
                            }) =>
                    {
                        None
                    }
                    Err(error) => return Err(session_error(error)),
                };
                document.edit(&session, &metadata)?;
                owner
                    .domain
                    .put(
                        "metadata",
                        serde_json::to_value(&document)
                            .map_err(|_| ApiError::Invalid("invalid navigation document".into()))?,
                    )
                    .await
                    .map_err(storage_error)?;
                let receipt = MetadataReceipt {
                    revision: document.revision.to_string(),
                    session,
                    metadata,
                };
                owner
                    .state
                    .lock()
                    .expect("navigation state poisoned")
                    .document = Arc::new(document);
                Ok(receipt)
            })
        })
    }
    async fn close(&self) {
        {
            let mut state = self.state.lock().expect("navigation state poisoned");
            state.closed = true;
            self.slots.close();
            self.writer.close();
            self.tasks.close();
        }
        self.tasks.wait().await;
    }
}
fn session_error(error: SessionError) -> ApiError {
    match error {
        SessionError::Api(error) => error,
        SessionError::NotFound(_) => ApiError::Invalid("Session is not available".into()),
        SessionError::Capacity => ApiError::Capacity,
        SessionError::ShuttingDown => ApiError::ShuttingDown,
        _ => ApiError::Unavailable,
    }
}
/// Nominal Host navigation capability.
#[derive(Debug)]
pub struct NavigationContract;
impl LocalContract for NavigationContract {
    const KEY: &'static str = "rsi.navigation";
    type Service = Navigation;
}
/// Ordinary Host plugin with an explicit Storage route.
#[derive(Clone, Debug, Default)]
pub struct NavigationFactory;
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Config {
    backend: String,
}
#[async_trait]
impl PluginFactory for NavigationFactory {
    fn prepare(&self, config: &ConfigValue) -> rsi_meta::Result<PreparedActivation> {
        let input: Config = serde_json::from_value(config.clone())
            .map_err(|_| MetaError::InvalidInput("invalid navigation configuration".into()))?;
        if input.backend.is_empty() || input.backend.len() > 256 {
            return Err(MetaError::InvalidInput("invalid navigation backend".into()));
        }
        Ok(PreparedActivation::new(config.clone())
            .requiring_local::<DomainFacilityContract>()
            .requiring_local::<SessionContract>()
            .requiring_local::<rsi_execution::ExecutionResolverContract>()
            .requiring_local::<SessionStoreContract>()
            .requiring_local::<WorkspaceRegistryContract>()
            .requiring_local::<ConnectionDescriptionContract>()
            .requiring_local::<ApiRegistrarContract>())
    }
    async fn activate(&self, plan: ActivationPlan) -> rsi_meta::Result<()> {
        let input: Config =
            serde_json::from_value(plan.config().as_ref().clone()).map_err(activation)?;
        let domain = plan
            .local::<DomainFacilityContract>()?
            .open(DomainSpec {
                id: "rsi.navigation".into(),
                backend: input.backend,
                version: 1,
                maximum_records: 1,
                maximum_bytes: 8 * 1024 * 1024,
            })
            .await
            .map_err(activation)?;
        let mut records = domain.snapshot().await.map_err(activation)?;
        if records.len() > 1 || records.keys().any(|key| key != "metadata") {
            return Err(MetaError::Activation("invalid navigation records".into()));
        }
        let mut document: Document = records
            .remove("metadata")
            .map_or_else(|| Ok(Document::default()), serde_json::from_value)
            .map_err(|_| MetaError::Activation("invalid navigation document".into()))?;
        document.validate().map_err(activation)?;
        let owner = Arc::new(Navigation {
            domain,
            session: plan.local::<SessionContract>()?,
            resolver: plan.local::<rsi_execution::ExecutionResolverContract>()?,
            store: plan.local::<SessionStoreContract>()?,
            cursors: Mutex::new(CursorBook::default()),
            protection: Some(Arc::new({
                let context = plan.context().clone();
                rsi_session_protocol::SessionProtectionLookup::new(move || {
                    context.lookup_local::<rsi_session_protocol::SessionProtectionContract>()
                })
            })),
            workspace: plan.local::<WorkspaceRegistryContract>()?,
            epoch: plan
                .local::<ConnectionDescriptionContract>()?
                .host_epoch
                .clone(),
            state: Mutex::new(State {
                closed: false,
                document: Arc::new(document),
            }),
            slots: Arc::new(Semaphore::new(8)),
            writer: Arc::new(Semaphore::new(1)),
            tasks: TaskTracker::new(),
            execution: plan.context().runtime().execution().clone(),
        });
        let registrations = endpoint::register(
            plan.local::<ApiRegistrarContract>()?.as_ref(),
            owner.clone(),
        )
        .map_err(activation)?;
        let supply = plan
            .context()
            .provide_local::<NavigationContract>(owner.clone())?;
        plan.defer(
            "drain navigation",
            Box::new(move || {
                Box::pin(async move {
                    drop(supply);
                    futures_util::join!(
                        owner.close(),
                        futures_util::future::join_all(
                            registrations
                                .into_iter()
                                .map(rsi_api_protocol::ApiRegistration::close)
                        )
                    );
                    Ok(())
                })
            }),
        )
    }
}
fn activation(error: impl std::fmt::Display) -> MetaError {
    MetaError::Activation(error.to_string())
}

#[cfg(test)]
mod tests;

impl Navigation {
    async fn scope_view(
        &self,
        id: &SessionId,
        origin: &CallOrigin,
    ) -> Result<Option<tokio_util::sync::CancellationToken>> {
        let Some(header) = visible_header(self.store.header(id).await)? else {
            return Ok(None);
        };
        Ok(match header.protection() {
            None => Some(tokio_util::sync::CancellationToken::new()),
            Some(scope) => match self.protection.as_ref().map(|p| p.view(scope, origin)) {
                None
                | Some(Err(rsi_session_protocol::SessionError::Api(ApiError::Unauthorized))) => {
                    None
                }
                Some(Ok(lease)) => Some(lease),
                Some(Err(_)) => return Err(ApiError::Unavailable),
            },
        })
    }
}
fn visible_header(
    result: rsi_agent_store_protocol::Result<rsi_agent_session_protocol::SessionHeader>,
) -> Result<Option<rsi_agent_session_protocol::SessionHeader>> {
    match result {
        Ok(header) => Ok(Some(header)),
        Err(rsi_agent_store_protocol::StoreError::NotFound(_)) => Ok(None),
        Err(_) => Err(ApiError::Unavailable),
    }
}
fn finish_scope(origin: &CallOrigin, leases: &[tokio_util::sync::CancellationToken]) -> Result<()> {
    if leases
        .iter()
        .any(tokio_util::sync::CancellationToken::is_cancelled)
        || matches!(origin,CallOrigin::Device(d)if d.revoked.is_cancelled())
    {
        Err(ApiError::Unauthorized)
    } else {
        Ok(())
    }
}

#[derive(Debug, Default)]
struct CursorBook {
    cuts: std::collections::VecDeque<ScanCut>,
}
#[derive(Debug)]
struct ScanCut {
    principal: String,
    issued: NavigationCursor,
    cut: rsi_agent_store_protocol::StoreActivityCursor,
    expires: tokio::time::Instant,
}
impl CursorBook {
    fn principal(origin: &CallOrigin) -> String {
        match origin {
            CallOrigin::Local => "local".into(),
            CallOrigin::Device(d) => d.id.as_str().into(),
        }
    }
    #[expect(
        clippy::too_many_arguments,
        reason = "Keep one complete ownership operation or acceptance scenario together"
    )]
    fn issue(
        &mut self,
        origin: &CallOrigin,
        filter: NavigationFilter,
        host_epoch: HostEpoch,
        metadata_revision: String,
        cut: rsi_agent_store_protocol::StoreActivityCursor,
        visible_cut: Option<rsi_agent_store_protocol::StoreActivityCursor>,
        predecessor: Option<&NavigationCursor>,
    ) -> Result<NavigationCursor> {
        let now = tokio::time::Instant::now();
        self.cuts.retain(|entry| entry.expires > now);
        let principal = Self::principal(origin);
        if let Some(cursor) = predecessor {
            self.read(origin, cursor)?;
        }
        let own = self
            .cuts
            .iter()
            .filter(|entry| entry.principal == principal)
            .count();
        if self.cuts.len() == 128 && own == 0 {
            return Err(ApiError::Capacity);
        }
        let mut entropy = [0u8; 16];
        getrandom::fill(&mut entropy).map_err(|_| ApiError::Unavailable)?;
        let token = hex::encode(entropy);
        let cursor = NavigationCursor {
            filter,
            host_epoch,
            metadata_revision,
            token: token.clone(),
            after: visible_cut,
        };
        if let Some(cursor) = predecessor {
            self.release(origin, cursor)?;
        } else if own >= 8 || self.cuts.len() == 128 {
            let first = self
                .cuts
                .iter()
                .position(|entry| entry.principal == principal)
                .expect("principal owns cuts");
            self.cuts.remove(first);
        }
        self.cuts.push_back(ScanCut {
            principal,
            issued: cursor.clone(),
            cut,
            expires: now + std::time::Duration::from_mins(5),
        });
        Ok(cursor)
    }
    fn read(
        &self,
        origin: &CallOrigin,
        cursor: &NavigationCursor,
    ) -> Result<rsi_agent_store_protocol::StoreActivityCursor> {
        self.cuts
            .iter()
            .find(|entry| {
                entry.principal == Self::principal(origin)
                    && &entry.issued == cursor
                    && entry.expires > tokio::time::Instant::now()
            })
            .map(|entry| entry.cut.clone())
            .ok_or(ApiError::Invalid(
                "Navigation cursor expired or belongs to a different caller; refresh explicitly"
                    .into(),
            ))
    }
    fn release(&mut self, origin: &CallOrigin, cursor: &NavigationCursor) -> Result<()> {
        self.read(origin, cursor)?;
        self.cuts.retain(|entry| &entry.issued != cursor);
        Ok(())
    }
}
