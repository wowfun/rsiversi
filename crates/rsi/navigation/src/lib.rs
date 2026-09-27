//! Host-owned Session navigation metadata and bounded durable queries.
#![deny(unsafe_code)]
#![warn(missing_docs)]
#![allow(clippy::missing_errors_doc)]
use async_trait::async_trait;
use futures_util::{StreamExt, future::BoxFuture};
use rsi_agent_session_protocol::SessionId;
use rsi_api_protocol::{
    ApiError, ApiRegistrarContract, ConnectionDescriptionContract, HostEpoch, Result,
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
use rsi_storage_domain::{Domain, DomainFacilityContract, DomainSpec};
use rsi_workspace_protocol::{
    WorkspaceError, WorkspaceId, WorkspaceRegistry, WorkspaceRegistryContract,
};
use serde::{Deserialize, Serialize};
use sha2::Digest as _;
use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex},
};
use tokio::sync::Semaphore;
use tokio_util::task::TaskTracker;
mod attention;
mod endpoint;
pub use attention::AttentionFactory;

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Document {
    revision: u64,
    records: BTreeMap<SessionId, SessionMetadata>,
}
impl Document {
    fn validate(&self) -> Result<()> {
        for record in self.records.values() {
            record.validate()?;
        }
        if self.records.values().filter(|record| record.pinned).count() > 64 {
            return Err(ApiError::Invalid(
                "at most 64 sessions may be pinned".into(),
            ));
        }
        if self.records.len() > 8192
            || serde_json::to_vec(&serde_json::json!({"metadata":self}))
                .map_err(|_| ApiError::Invalid("invalid navigation document".into()))?
                .len()
                > 8 * 1024 * 1024
        {
            return Err(ApiError::Invalid(
                "navigation metadata exceeds 8192 records or 8 MiB".into(),
            ));
        }
        Ok(())
    }
}
#[derive(Debug)]
struct State {
    closed: bool,
    uncertain: bool,
    document: Arc<Document>,
}
/// One Host's navigation owner; it never owns Session execution.
#[derive(Debug)]
pub struct Navigation {
    domain: Arc<dyn Domain>,
    session: Arc<dyn SessionService>,
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
        let state = self.state.lock().expect("navigation state poisoned");
        if state.closed {
            return Err(ApiError::ShuttingDown);
        }
        if state.uncertain {
            return Err(ApiError::OutcomeUnknown);
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
        filter: NavigationFilter,
        after: Option<NavigationCursor>,
    ) -> Result<BoxFuture<'static, Result<NavigationPage>>> {
        filter.validate()?;
        self.run(move |owner| Box::pin(async move { owner.scan(filter, after).await }))
    }
    async fn scan(
        &self,
        filter: NavigationFilter,
        after: Option<NavigationCursor>,
    ) -> Result<NavigationPage> {
        let document = self
            .state
            .lock()
            .expect("navigation state poisoned")
            .document
            .clone();
        if let Some(after) = &after
            && (after.filter != filter
                || after.host_epoch != self.epoch
                || revision(&after.metadata_revision)? != document.revision
                || after.after.created_at_ms == 0)
        {
            return Err(ApiError::Invalid(
                "navigation changed; refresh this search".into(),
            ));
        }
        let page = self
            .session
            .list_recent(after.as_ref().map(|after| &after.after), 256)
            .await
            .map_err(session_error)?;
        let mut entries = Vec::new();
        let mut scanned = 0;
        let mut cursor = None;
        let query = filter.query.to_lowercase();
        for row in &page.sessions {
            scanned += 1;
            cursor = Some(row.cursor());
            let id = row.header.session_id();
            let metadata = document.records.get(id).cloned().unwrap_or_default();
            if metadata.archived != filter.archived || metadata.pinned {
                continue;
            }
            let path = row.header.canonical_cwd();
            if !matches_query(&query, id, &metadata, Some(path)) {
                continue;
            }
            let workspace = self.workspace_for(path).await?;
            if !filter.workspace.matches(workspace.as_ref()) {
                continue;
            }
            entries.push(NavigationEntry {
                session: id.clone(),
                created_at_ms: row.header.created_at_ms().to_string(),
                path: path.into(),
                workspace,
                metadata,
            });
            if entries.len() == 64 {
                break;
            }
        }
        let more = scanned < page.sessions.len() || page.has_more;
        Ok(NavigationPage {
            metadata_revision: document.revision.to_string(),
            entries,
            scanned: u16::try_from(scanned).expect("bounded Session page"),
            next: if more {
                cursor.map(|after| NavigationCursor {
                    filter,
                    host_epoch: self.epoch.clone(),
                    metadata_revision: document.revision.to_string(),
                    after,
                })
            } else {
                None
            },
        })
    }
    async fn workspace_for(&self, path: &str) -> Result<Option<WorkspaceId>> {
        let derived = WorkspaceId::parse(hex::encode(sha2::Sha256::digest(path.as_bytes())))
            .map_err(|_| ApiError::Backend("invalid Session workspace identity".into()))?;
        match self.workspace.get(&derived).await {
            Ok(record) => Ok(Some(record.id)),
            Err(WorkspaceError::Unknown(_)) => Ok(None),
            Err(_) => Err(ApiError::Unavailable),
        }
    }
    /// Reads all pinned Headers with no attach or activity side effects.
    ///
    /// # Panics
    /// Panics if a prior panic poisoned navigation state.
    pub fn pinned(
        self: &Arc<Self>,
        filter: NavigationFilter,
    ) -> Result<BoxFuture<'static, Result<PinnedPage>>> {
        filter.validate()?;
        self.run(move |owner| {
            Box::pin(async move {
                let document = owner
                    .state
                    .lock()
                    .expect("navigation state poisoned")
                    .document
                    .clone();
                let mut available = Vec::new();
                let mut missing = Vec::new();
                let query = filter.query.to_lowercase();
                let reads: Vec<_> = document
                    .records
                    .iter()
                    .filter(|(_, metadata)| metadata.pinned && metadata.archived == filter.archived)
                    .map(|(id, metadata)| {
                        let session = owner.session.clone();
                        let id = id.clone();
                        let metadata = metadata.clone();
                        async move {
                            let result = session.read_header(&id).await;
                            (id, metadata, result)
                        }
                    })
                    .collect();
                let mut headers = futures_util::stream::iter(reads).buffered(4);
                while let Some((id, metadata, result)) = headers.next().await {
                    let header = match result {
                        Ok(header) => header,
                        Err(SessionError::NotFound(_)) => {
                            if filter.workspace == WorkspaceFilter::All
                                && matches_query(&query, &id, &metadata, None)
                            {
                                missing.push(PinnedEntry::Missing {
                                    session: id.clone(),
                                    metadata: metadata.clone(),
                                });
                            }
                            continue;
                        }
                        Err(error) => return Err(session_error(error)),
                    };
                    let path = header.canonical_cwd();
                    if !matches_query(&query, &id, &metadata, Some(path)) {
                        continue;
                    }
                    let workspace = owner.workspace_for(path).await?;
                    if !filter.workspace.matches(workspace.as_ref()) {
                        continue;
                    }
                    available.push((
                        header.created_at_ms(),
                        id.clone(),
                        PinnedEntry::Available {
                            entry: NavigationEntry {
                                session: id.clone(),
                                created_at_ms: header.created_at_ms().to_string(),
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
                match owner.session.read_header(&session).await {
                    Ok(_) => {}
                    Err(SessionError::NotFound(_))
                        if document.records.get(&session).is_some_and(|old| {
                            metadata == SessionMetadata::default() || old.pinned && !metadata.pinned
                        }) => {}
                    Err(error) => return Err(session_error(error)),
                }
                if metadata == SessionMetadata::default() {
                    document.records.remove(&session);
                } else {
                    document.records.insert(session.clone(), metadata.clone());
                }
                document.revision = document
                    .revision
                    .checked_add(1)
                    .ok_or_else(|| ApiError::Invalid("navigation revision exhausted".into()))?;
                document.validate()?;
                if owner
                    .domain
                    .put(
                        "metadata",
                        serde_json::to_value(&document)
                            .map_err(|_| ApiError::Invalid("invalid navigation document".into()))?,
                    )
                    .await
                    .is_err()
                {
                    owner
                        .state
                        .lock()
                        .expect("navigation state poisoned")
                        .uncertain = true;
                    return Err(ApiError::OutcomeUnknown);
                }
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
        let mut records = domain.snapshot().await;
        if records.len() > 1 || records.keys().any(|key| key != "metadata") {
            return Err(MetaError::Activation("invalid navigation records".into()));
        }
        let document: Document = records
            .remove("metadata")
            .map_or_else(|| Ok(Document::default()), serde_json::from_value)
            .map_err(|_| MetaError::Activation("invalid navigation document".into()))?;
        document.validate().map_err(activation)?;
        let owner = Arc::new(Navigation {
            domain,
            session: plan.local::<SessionContract>()?,
            workspace: plan.local::<WorkspaceRegistryContract>()?,
            epoch: plan
                .local::<ConnectionDescriptionContract>()?
                .host_epoch
                .clone(),
            state: Mutex::new(State {
                closed: false,
                uncertain: false,
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
