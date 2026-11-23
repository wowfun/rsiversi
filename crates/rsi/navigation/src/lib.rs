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
    ActivationPlan, ConfigValue, LocalContract, MetaError, PluginFactory, PreparedActivation,
};
use rsi_navigation_api::{
    ArchiveAction, MetadataReceipt, NavigationCursor, NavigationEntry, NavigationFilter,
    NavigationPage, PinnedEntry, PinnedPage, SessionMetadata, WorkspaceFilter, matches_query,
    revision,
};
use rsi_session_protocol::SessionError;
use rsi_storage::{RecordObjectSize, encoded_entry_bytes};
use rsi_storage_domain::storage_error;
use rsi_storage_domain::{Domain, DomainFacilityContract, DomainSpec, Schema};
use rsi_workspace_protocol::{
    WorkspaceError, WorkspaceId, WorkspaceRegistry, WorkspaceRegistryContract,
};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex},
};
use tokio::sync::Semaphore;
mod admission;
mod archive;
mod attention;
use admission::Admission;
mod endpoint;
mod order;
mod persistence;
mod workspace_account;
pub use attention::AttentionFactory;
use persistence::{DocumentChange, Revision, domain_schema, domain_spec, load_document};
pub use workspace_account::{WorkspaceCatalogMetadata, WorkspaceMoveOutcome};

/// Durable navigation payload version; retained corpora gate encoding changes.
pub const NAVIGATION_STORAGE_VERSION: u32 = 2;

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Document {
    revision: u64,
    records: BTreeMap<SessionId, StoredMetadata>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    workspace_orders: BTreeMap<WorkspaceId, Vec<SessionId>>,
    #[serde(skip)]
    accounting: Option<MetadataSize>,
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
struct StoredMetadata {
    #[serde(flatten)]
    metadata: SessionMetadata,
    #[serde(
        default,
        rename = "rank",
        alias = "pin_revision",
        skip_serializing_if = "Option::is_none"
    )]
    partition_revision: Option<u64>,
}
impl std::ops::Deref for StoredMetadata {
    type Target = SessionMetadata;

    fn deref(&self) -> &SessionMetadata {
        &self.metadata
    }
}
#[derive(Clone, Copy, Debug, Default)]
struct MetadataSize {
    records: RecordObjectSize,
    orders: RecordObjectSize,
    order_members: usize,
    pins: usize,
}
impl MetadataSize {
    fn bytes(self, revision: u64) -> usize {
        b"{\"metadata\":{\"revision\":,\"records\":}}".len()
            + revision.to_string().len()
            + self.records.bytes()
            + if self.orders.records() == 0 {
                0
            } else {
                b",\"workspace_orders\":".len() + self.orders.bytes()
            }
    }
    fn check(self, revision: u64) -> Result<()> {
        if self.pins > 64 {
            return Err(ApiError::Invalid(
                "at most 64 sessions may be pinned".into(),
            ));
        }
        // The Domain record wrapper is intentional: it counts toward the 8 MiB bound.
        if self.records.records() > 8192
            || self.order_members > 8192
            || self.bytes(revision) > 8 * 1024 * 1024
        {
            return Err(ApiError::Invalid(
                "navigation metadata exceeds 8192 records/order references or 8 MiB".into(),
            ));
        }
        Ok(())
    }
}
fn metadata_bytes(id: &SessionId, metadata: &StoredMetadata) -> Result<usize> {
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
            if record
                .partition_revision
                .is_some_and(|r| !(record.pinned || record.archived) || r == 0 || r > self.revision)
            {
                return Err(ApiError::Invalid(
                    "invalid stored partition revision".into(),
                ));
            }
            accounting.records = accounting
                .records
                .with_entry(None, metadata_bytes(id, record)?)
                .map_err(storage_error)?;
            accounting.pins += usize::from(record.pinned);
        }
        self.validate_orders(&mut accounting)?;
        accounting.check(self.revision)?;
        self.accounting = Some(accounting);
        Ok(())
    }
    fn project_metadata(
        &self,
        session: &SessionId,
        metadata: &SessionMetadata,
    ) -> Result<DocumentChange> {
        let mut accounting = self.accounting.expect("validated metadata accounting");
        let previous = self.records.get(session);
        let revision = self
            .revision
            .checked_add(1)
            .ok_or_else(|| ApiError::Invalid("navigation revision exhausted".into()))?;
        let stored = StoredMetadata {
            partition_revision: if metadata.pinned || metadata.archived {
                previous
                    .filter(|row| {
                        row.pinned == metadata.pinned && row.archived == metadata.archived
                    })
                    .map_or(Some(revision), |row| row.partition_revision)
            } else {
                None
            },
            metadata: metadata.clone(),
        };
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
                .with_entry(previous_bytes, metadata_bytes(session, &stored)?)
                .map_err(storage_error)?;
            accounting.pins += usize::from(metadata.pinned);
        }
        accounting.check(revision)?;
        Ok(DocumentChange {
            revision,
            accounting,
            records: vec![(session.clone(), (!remove).then_some(stored))],
            orders: vec![],
        })
    }
}
#[derive(Debug)]
struct State {
    document: Arc<Document>,
}
/// A point-in-time view of archive membership in the committed navigation document.
#[derive(Debug)]
pub struct ArchivedSessions(Arc<Document>);
impl ArchivedSessions {
    /// Whether this Session was archived at the captured revision.
    pub fn contains(&self, session: &SessionId) -> bool {
        self.0.records.get(session).is_some_and(|row| row.archived)
    }
}
/// Domain result for a pin intent on a published Session.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PinOutcome {
    /// The requested state is durable, including an idempotent retry.
    Applied,
    /// No published Session is available.
    Unknown,
    /// An archived Session cannot be pinned.
    Archived,
}
/// Workspace archive operations on one Session.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ArchiveIntent {
    /// Admit activity providers only for a known, currently unarchived Session.
    Inspect,
    /// Archive and clear any pin atomically.
    Archive,
    /// Archive before stopping active work.
    StopAndArchive,
    /// Clear only the archive flag.
    Restore,
}
/// Result at the Navigation owner's serialized revision.
#[derive(Debug, Eq, PartialEq)]
pub enum ArchiveOutcome {
    /// No published Session is available to archive.
    Unknown,
    /// The caller may proceed to check activity before archiving.
    Ready(Vec<rsi_navigation_api::ArchiveActivity>),
    /// The visible archive set, in entry order, with the mutation outcome.
    Applied {
        /// True only when this intent changed the durable metadata.
        changed: bool,
        /// Caller-visible Sessions in archive entry order at this revision.
        ids: Vec<SessionId>,
    },
}
#[async_trait]
impl rsi_agent_turn_protocol::SessionArchive for Navigation {
    async fn is_archived(
        &self,
        header: &rsi_agent_session_protocol::SessionHeader,
    ) -> rsi_agent_turn_protocol::Result<bool> {
        let archived = self.archived_sessions();
        if archived.contains(header.session_id()) {
            return Ok(true);
        }
        let Some(origin) = header.fork_origin() else {
            return Ok(false);
        };
        if archived.contains(&origin.root_session_id) {
            return Ok(true);
        }
        let mut parent = &origin.history.parent_session_id;
        if archived.contains(parent) {
            return Ok(true);
        }
        if origin.path.depth() <= 2 || !archived.0.records.values().any(|row| row.archived) {
            return Ok(false);
        }
        let mut ancestor;
        for _ in 2..origin.path.depth() {
            ancestor = match self.store.header(parent).await {
                Ok(header) => header,
                Err(rsi_agent_store_protocol::StoreError::NotFound(_)) => break,
                Err(error) => {
                    return Err(rsi_agent_turn_protocol::TurnError::Store(error.to_string()));
                }
            };
            let Some(lineage) = ancestor.fork_origin() else {
                break;
            };
            parent = &lineage.history.parent_session_id;
            if archived.contains(parent) {
                return Ok(true);
            }
        }
        Ok(false)
    }
}

/// One Host's navigation owner; it never owns Session execution.
#[derive(Debug)]
pub struct Navigation {
    archive_host: Option<(rsi_meta::Context, &'static str)>,
    domain: Domain,
    resolver: Arc<dyn rsi_execution::ExecutionResolver>,
    store: Arc<dyn SessionStore>,
    protection: Option<Arc<dyn rsi_session_protocol::SessionProtection>>,
    cursors: Mutex<CursorBook>,
    workspace: Arc<dyn WorkspaceRegistry>,
    epoch: HostEpoch,
    state: Mutex<State>,
    archive_changes: tokio::sync::watch::Sender<()>,
    writer: Arc<Semaphore>,
    admission: Arc<Admission>,
}
impl Navigation {
    /// Subscribes to committed archive membership changes, including purges.
    pub fn archive_changes(&self) -> tokio::sync::watch::Receiver<()> {
        self.archive_changes.subscribe()
    }

    /// Reads archive membership from the current committed metadata.
    ///
    /// # Panics
    /// Panics if an earlier Navigation writer poisoned the metadata lock.
    pub fn archived_sessions(&self) -> ArchivedSessions {
        ArchivedSessions(self.document())
    }
    fn run_mutation<T: Send + 'static>(
        self: &Arc<Self>,
        work: impl FnOnce(Arc<Self>) -> BoxFuture<'static, Result<T>>,
    ) -> Result<BoxFuture<'static, Result<T>>> {
        self.domain.ensure_available().map_err(storage_error)?;
        self.admission.run_mutation(|| work(self.clone()))
    }
    fn run_read<T: Send + 'static>(
        self: &Arc<Self>,
        work: impl FnOnce(Arc<Self>) -> BoxFuture<'static, Result<T>> + Send + 'static,
    ) -> Result<BoxFuture<'static, Result<T>>> {
        self.domain.ensure_available().map_err(storage_error)?;
        let owner = self.clone();
        self.admission.run_read(move || work(owner))
    }
    /// Admits a bounded filtered scan; empty results may still have a continuation.
    pub fn query(
        self: &Arc<Self>,
        origin: &CallOrigin,
        filter: NavigationFilter,
        after: Option<NavigationCursor>,
    ) -> Result<BoxFuture<'static, Result<NavigationPage>>> {
        self.query_for(origin, self.can_archive(origin)?, filter, after)
    }
    fn query_for(
        self: &Arc<Self>,
        origin: &CallOrigin,
        can_archive: bool,
        filter: NavigationFilter,
        after: Option<NavigationCursor>,
    ) -> Result<BoxFuture<'static, Result<NavigationPage>>> {
        filter.validate()?;
        let visibility = self.resolver.visibility(origin)?;
        let origin = origin.clone();
        self.run_read(move |owner| {
            Box::pin(async move {
                owner
                    .scan(visibility, origin, can_archive, filter, after)
                    .await
            })
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
        can_archive: bool,
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
                            can_archive,
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
                .map_or(&default_metadata, |row| &row.metadata);
            (filter.matches_archive(metadata.archived)
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
            let metadata = document
                .records
                .get(id)
                .map_or(&default_metadata, |row| &row.metadata);
            if !filter.matches_archive(metadata.archived) || metadata.pinned {
                continue;
            }
            let path = row.coordinates.path();
            if !matches_query(&query, id, metadata, Some(path)) {
                continue;
            }
            let workspace = document.membership(workspace?, id);
            if !filter.workspace.matches(workspace.as_ref()) {
                continue;
            }
            if !self.retain_session_view(id, &origin, &mut leases).await? {
                continue;
            }
            visible_cut = Some(row.cursor());
            entries.push(NavigationEntry {
                session: id.clone(),
                created_at_ms: row.created_at_ms.to_string(),
                last_activity_ms: row.last_activity_ms.to_string(),
                location: row.coordinates.location().clone(),
                path: path.into(),
                workspace,
                metadata: metadata.clone(),
            });
            if entries.len() == 64 {
                break;
            }
        }
        drop(rows);
        let more = scanned < page.sessions.len() || page.has_more;
        let newest = match page.newest {
            Some(key) => {
                if self
                    .retain_session_view(&key.session_id, &origin, &mut leases)
                    .await?
                {
                    Some(key)
                } else {
                    None
                }
            }
            None => None,
        };
        finish_scope(&origin, &leases)?;
        Ok(NavigationPage {
            can_archive,
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
        self.workspace
            .find(&rsi_workspace_protocol::WorkspaceKey::from_coordinates(
                coordinates,
            ))
            .await
            .map(|record| record.map(|record| record.id))
            .map_err(|_| ApiError::Unavailable)
    }
    // Shared futures retain one result per coordinate without starting unpolled lookups.
    fn workspace_lookups<'a>(
        &'a self,
        coordinates: impl Iterator<Item = Option<&'a ExecutionCoordinates>> + Send + 'a,
    ) -> stream::BoxStream<'a, Result<Option<WorkspaceId>>> {
        let mut distinct = BTreeMap::new();
        stream::iter(coordinates)
            .map(move |coordinates| {
                let read = coordinates.map(|coordinates| {
                    distinct
                        .entry(coordinates)
                        .or_insert_with(|| self.workspace_for(coordinates).boxed().shared())
                        .clone()
                });
                async move {
                    match read {
                        Some(read) => read.await,
                        None => Ok(None),
                    }
                }
            })
            .buffered(4)
            .boxed()
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
        self.run_read(move |owner| {
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
                    .filter(|(_, metadata)| {
                        metadata.pinned && filter.matches_archive(metadata.archived)
                    })
                    .collect::<Vec<_>>();
                let ids = selected
                    .iter()
                    .map(|(id, _)| (*id).clone())
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
                let mut rows = stream::iter(rows.iter().enumerate()).zip(lookups);
                while let Some(((index, row), workspace)) = rows.next().await {
                    let (id, metadata) = selected[index];
                    let Some(row) = row else {
                        if !owner.retain_session_view(id, &origin, &mut leases).await? {
                            continue;
                        }
                        if matches!(origin, CallOrigin::Local)
                            && filter.workspace == WorkspaceFilter::All
                            && matches_query(&query, id, metadata, None)
                        {
                            missing.push(PinnedEntry::Missing {
                                session: id.clone(),
                                metadata: metadata.metadata.clone(),
                            });
                        }
                        continue;
                    };
                    if !visibility.locations().contains(row.coordinates.location()) {
                        continue;
                    }
                    if !owner.retain_session_view(id, &origin, &mut leases).await? {
                        continue;
                    }
                    let path = row.coordinates.path();
                    if !matches_query(&query, id, metadata, Some(path)) {
                        continue;
                    }
                    let workspace = document.membership(workspace?, id);
                    if !filter.workspace.matches(workspace.as_ref()) {
                        continue;
                    }
                    available.push((
                        (metadata.partition_revision, row.last_activity_ms),
                        id,
                        PinnedEntry::Available {
                            entry: NavigationEntry {
                                session: id.clone(),
                                created_at_ms: row.created_at_ms.to_string(),
                                last_activity_ms: row.last_activity_ms.to_string(),
                                location: row.coordinates.location().clone(),
                                path: path.into(),
                                workspace,
                                metadata: metadata.metadata.clone(),
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
    fn edit(
        self: &Arc<Self>,
        origin: CallOrigin,
        credential: CallOrigin,
        session: SessionId,
        expected: &str,
        mut metadata: SessionMetadata,
        stop_activity: bool,
    ) -> Result<BoxFuture<'static, Result<rsi_navigation_api::MetadataEditResult>>> {
        if stop_activity && !metadata.archived {
            return Err(ApiError::Invalid("Only archiving can stop work".into()));
        }
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
        self.run_mutation(move |owner| {
            Box::pin(async move {
                let _permit = permit;
                let document = owner.document();
                if document.revision != expected {
                    return Err(ApiError::Invalid(
                        "navigation revision conflict; refresh before editing".into(),
                    ));
                }
                let was_archived = document
                    .records
                    .get(&session)
                    .is_some_and(|row| row.archived);
                if was_archived != metadata.archived || stop_activity {
                    owner.require_archive_owner(&credential)?;
                }
                let header = owner.metadata_header(&session).await?;
                let _admission = match &header {
                    Some(header) => Some(owner.admit_metadata(header, &origin)?),
                    None if matches!(origin, CallOrigin::Local)
                        && document.records.get(&session).is_some_and(|old| {
                            metadata == SessionMetadata::default() || old.pinned && !metadata.pinned
                        }) =>
                    {
                        None
                    }
                    None => return Err(ApiError::Invalid("Session is not available".into())),
                };
                drop(document);
                let updated = match owner
                    .commit_edit(
                        &credential,
                        &session,
                        header.as_ref(),
                        was_archived,
                        &metadata,
                        stop_activity,
                    )
                    .await?
                {
                    Ok(document) => document,
                    Err(activity) => return Ok(Err(activity)),
                };
                Ok(Ok(MetadataReceipt {
                    revision: updated.revision.to_string(),
                    session,
                    metadata,
                }))
            })
        })
    }
    /// Applies one pin intent under the same writer as metadata replacement.
    pub fn set_pin(
        self: &Arc<Self>,
        origin: CallOrigin,
        session: SessionId,
        pinned: bool,
    ) -> Result<BoxFuture<'static, Result<PinOutcome>>> {
        self.run_mutation(move |owner| {
            Box::pin(async move {
                let _permit = owner
                    .writer
                    .acquire()
                    .await
                    .map_err(|_| ApiError::ShuttingDown)?;
                let document = owner.document();
                let previous = document.records.get(&session);
                if !pinned && !previous.is_some_and(|row| row.pinned) {
                    return Ok(PinOutcome::Applied);
                }
                let Some(header) = owner.metadata_header(&session).await? else {
                    return Ok(if pinned {
                        PinOutcome::Unknown
                    } else {
                        PinOutcome::Applied
                    });
                };
                let _admission = owner.admit_metadata(&header, &origin)?;
                if pinned && previous.is_some_and(|row| row.archived) {
                    return Ok(PinOutcome::Archived);
                }
                if previous.is_some_and(|row| row.pinned) != pinned {
                    let mut metadata = previous.map(|row| row.metadata.clone()).unwrap_or_default();
                    metadata.pinned = pinned;
                    drop(document);
                    owner.write_metadata(&session, &metadata).await?;
                }
                Ok(PinOutcome::Applied)
            })
        })
    }

    /// Inspects or updates a single Session's archive membership.
    pub fn archive_intent(
        self: &Arc<Self>,
        origin: CallOrigin,
        session: SessionId,
        intent: ArchiveIntent,
    ) -> Result<BoxFuture<'static, Result<ArchiveOutcome>>> {
        self.require_archive_owner(&origin)?;
        let action = match intent {
            ArchiveIntent::Archive => ArchiveAction::Archive,
            ArchiveIntent::Restore => ArchiveAction::Restore,
            ArchiveIntent::StopAndArchive => ArchiveAction::StopAndArchive,
            ArchiveIntent::Inspect => {
                return self.run_read(move |owner| {
                    Box::pin(async move {
                        let Some(header) = owner.metadata_header(&session).await? else {
                            return Ok(ArchiveOutcome::Unknown);
                        };
                        let _admission = owner.admit_metadata(&header, &origin)?;
                        let current = owner.document();
                        if !current
                            .records
                            .get(&session)
                            .is_some_and(|row| row.archived)
                        {
                            return Ok(ArchiveOutcome::Ready(
                                owner.archive_activity(&header).await?,
                            ));
                        }
                        let visibility = owner.resolver.visibility(&origin)?;
                        let ids = owner
                            .visible_archives(&current, &origin, visibility)
                            .await?;
                        Ok(ArchiveOutcome::Applied {
                            changed: false,
                            ids,
                        })
                    })
                });
            }
        };
        self.run_mutation(move |owner| {
            Box::pin(async move {
                let visibility = owner.resolver.visibility(&origin)?;
                let (document, changed) = match owner
                    .apply_archive(&origin, &session, action)
                    .await?
                {
                    archive::ArchiveUpdate::Unknown(current)
                        if action == ArchiveAction::Restore =>
                    {
                        (current, false)
                    }
                    archive::ArchiveUpdate::Unknown(_) => return Ok(ArchiveOutcome::Unknown),
                    archive::ArchiveUpdate::Active(work) => return Ok(ArchiveOutcome::Ready(work)),
                    archive::ArchiveUpdate::Applied(document, changed) => (document, changed),
                };
                let ids = owner
                    .visible_archives(&document, &origin, visibility)
                    .await?;
                Ok(ArchiveOutcome::Applied { changed, ids })
            })
        })
    }

    async fn visible_archives(
        &self,
        document: &Document,
        origin: &CallOrigin,
        visibility: rsi_execution::ExecutionVisibility,
    ) -> Result<Vec<SessionId>> {
        let mut visible = Vec::new();
        let mut leases = Vec::new();
        for (id, record) in document.records.iter().filter(|(_, row)| row.archived) {
            let Some(header) = self.metadata_header(id).await? else {
                continue;
            };
            if visibility
                .locations()
                .contains(header.coordinates().location())
                && self.retain_header_view(&header, origin, &mut leases)?
            {
                visible.push((record.partition_revision, id.clone()));
            }
        }
        finish_scope(origin, &leases)?;
        visible.sort_unstable();
        Ok(visible.into_iter().map(|(_, id)| id).collect())
    }

    async fn write_metadata(
        &self,
        session: &SessionId,
        metadata: &SessionMetadata,
    ) -> Result<Arc<Document>> {
        let change = self
            .state
            .lock()
            .expect("navigation state poisoned")
            .document
            .project_metadata(session, metadata)?;
        self.publish(change).await
    }
    async fn publish(&self, change: DocumentChange) -> Result<Arc<Document>> {
        let metadata = self
            .domain
            .table::<StoredMetadata>("metadata")
            .map_err(storage_error)?;
        let orders = self
            .domain
            .table::<Vec<SessionId>>("orders")
            .map_err(storage_error)?;
        let mut batch = Vec::with_capacity(change.records.len() + change.orders.len() + 1);
        batch.push(
            self.domain
                .global::<Revision>()
                .map_err(storage_error)?
                .change(Revision {
                    revision: change.revision,
                })
                .map_err(storage_error)?,
        );
        for (id, value) in &change.records {
            batch.push(
                match value {
                    Some(value) => metadata.put_change(id.as_str(), value.clone()),
                    None => metadata.delete_change(id.as_str()),
                }
                .map_err(storage_error)?,
            );
        }
        for (id, members) in &change.orders {
            batch.push(
                if members.is_empty() {
                    orders.delete_change(id.as_str())
                } else {
                    orders.put_change(id.as_str(), members.clone())
                }
                .map_err(storage_error)?,
            );
        }
        self.domain.batch(batch).await.map_err(storage_error)?;
        let mut state = self.state.lock().expect("navigation state poisoned");
        let archive_changed = change.records.iter().any(|(id, next)| {
            state
                .document
                .records
                .get(id)
                .is_some_and(|row| row.archived)
                != next.as_ref().is_some_and(|row| row.archived)
        });
        change.apply(Arc::make_mut(&mut state.document));
        if archive_changed {
            self.archive_changes.send_replace(());
        }
        Ok(state.document.clone())
    }
    async fn close(&self) {
        self.writer.close();
        self.admission.close().await;
        self.domain.close().await;
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
#[derive(Debug)]
struct PurgeCleanup(std::sync::Weak<Navigation>);
#[async_trait]
impl rsi_session_protocol::SessionPurgeCleanup for PurgeCleanup {
    async fn status(
        &self,
        operation: &str,
    ) -> rsi_session_protocol::Result<rsi_session_protocol::CleanupStatus> {
        let owner = self.0.upgrade().ok_or(SessionError::ShuttingDown)?;
        let store = owner
            .store
            .maintenance()
            .ok_or(SessionError::Api(ApiError::Unavailable))?;
        let (request, _) = store
            .purge_binding(operation)
            .await
            .map_err(|e| SessionError::Backend(e.to_string()))?
            .ok_or_else(|| SessionError::NotFound(operation.into()))?;
        let state = owner.state.lock().expect("navigation state poisoned");
        let absent = request.targets.iter().all(|target| {
            !state.document.records.contains_key(&target.session_id)
                && !state
                    .document
                    .workspace_orders
                    .values()
                    .any(|ids| ids.contains(&target.session_id))
        });
        Ok(if absent {
            rsi_session_protocol::CleanupStatus::Complete
        } else {
            rsi_session_protocol::CleanupStatus::Pending
        })
    }

    async fn cleanup(&self, operation: &str) -> rsi_session_protocol::Result<()> {
        let owner = self.0.upgrade().ok_or(SessionError::ShuttingDown)?;
        let store = owner
            .store
            .maintenance()
            .ok_or(SessionError::Api(ApiError::Unavailable))?;
        let receipt = store
            .purge_status(operation)
            .await
            .map_err(|e| SessionError::Backend(e.to_string()))?
            .ok_or_else(|| SessionError::NotFound(operation.into()))?;
        if !receipt.complete {
            return Err(SessionError::Invalid("purge is not complete".into()));
        }
        let (request, _) = store
            .purge_binding(operation)
            .await
            .map_err(|e| SessionError::Backend(e.to_string()))?
            .ok_or_else(|| SessionError::NotFound(operation.into()))?;
        let permit = owner
            .writer
            .clone()
            .try_acquire_owned()
            .map_err(|_| SessionError::Capacity)?;
        owner
            .run_mutation(move |owner| {
                Box::pin(async move {
                    let _permit = permit;
                    let change = {
                        let state = owner.state.lock().expect("navigation state poisoned");
                        DocumentChange::purge(&state.document, &request.targets)?
                    };
                    if let Some(change) = change {
                        owner.publish(change).await?;
                    }
                    Ok(())
                })
            })
            .map_err(SessionError::Api)?
            .await
            .map_err(SessionError::Api)
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
#[derive(Clone, Debug)]
pub struct NavigationFactory {
    /// Slot assigned to the managed Host browser by the application.
    pub browser_slot: &'static str,
}
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
            .open(
                domain_spec(input.backend),
                domain_schema().map_err(activation)?,
            )
            .await
            .map_err(activation)?;
        let document = load_document(&domain).map_err(activation)?;
        let owner = Arc::new(Navigation {
            archive_host: Some((plan.context().clone(), self.browser_slot)),
            domain,
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
                document: Arc::new(document),
            }),
            archive_changes: tokio::sync::watch::channel(()).0,
            writer: Arc::new(Semaphore::new(1)),
            admission: Admission::new(8, plan.context().runtime().execution().clone()),
        });
        let registrations = endpoint::register(
            plan.local::<ApiRegistrarContract>()?.as_ref(),
            owner.clone(),
        )
        .map_err(activation)?;
        let supply = plan
            .context()
            .provide_local::<NavigationContract>(owner.clone())?;
        let archive = plan
            .context()
            .provide_local::<rsi_agent_turn_protocol::SessionArchiveContract>(owner.clone())?;
        let cleanup = plan
            .context()
            .provide_local::<rsi_session_protocol::SessionPurgeCleanupContract>(Arc::new(
                PurgeCleanup(Arc::downgrade(&owner)),
            ))?;
        plan.defer(
            "drain navigation",
            Box::new(move || {
                Box::pin(async move {
                    drop(supply);
                    drop(archive);
                    drop(cleanup);
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
    async fn metadata_header(
        &self,
        id: &SessionId,
    ) -> Result<Option<rsi_agent_session_protocol::SessionHeader>> {
        Ok(visible_summary(self.store.session_summary(id).await)?.map(|summary| summary.header))
    }

    async fn retain_session_view(
        &self,
        id: &SessionId,
        origin: &CallOrigin,
        leases: &mut Vec<tokio_util::sync::CancellationToken>,
    ) -> Result<bool> {
        let Some(header) = self.metadata_header(id).await? else {
            return Ok(false);
        };
        self.retain_header_view(&header, origin, leases)
    }

    fn retain_header_view(
        &self,
        header: &rsi_agent_session_protocol::SessionHeader,
        origin: &CallOrigin,
        leases: &mut Vec<tokio_util::sync::CancellationToken>,
    ) -> Result<bool> {
        let Some(scope) = header.protection() else {
            return Ok(true);
        };
        let Some(lease) = self.protection_view(scope, origin)? else {
            return Ok(false);
        };
        leases.push(lease);
        Ok(true)
    }

    fn admit_metadata(
        &self,
        header: &rsi_agent_session_protocol::SessionHeader,
        origin: &CallOrigin,
    ) -> Result<(
        rsi_execution::ExecutionOperation,
        Option<tokio_util::sync::CancellationToken>,
    )> {
        let lease = match header.protection() {
            Some(scope) => Some(
                self.protection_view(scope, origin)?
                    .ok_or(ApiError::Unauthorized)?,
            ),
            None => None,
        };
        let operation = self
            .resolver
            .admit(origin, header.coordinates().location())?;
        Ok((operation, lease))
    }

    fn protection_view(
        &self,
        scope: &rsi_agent_session_protocol::SessionProtectionScope,
        origin: &CallOrigin,
    ) -> Result<Option<tokio_util::sync::CancellationToken>> {
        match self.protection.as_ref().map(|p| p.view(scope, origin)) {
            None | Some(Err(rsi_session_protocol::SessionError::Api(ApiError::Unauthorized))) => {
                Ok(None)
            }
            Some(Ok(lease)) => Ok(Some(lease)),
            Some(Err(_)) => Err(ApiError::Unavailable),
        }
    }
}
fn visible_summary(
    result: rsi_agent_store_protocol::Result<rsi_agent_store_protocol::StoreRecentSession>,
) -> Result<Option<rsi_agent_store_protocol::StoreRecentSession>> {
    use rsi_agent_store_protocol::StoreError;
    match result {
        Ok(summary) => Ok(Some(summary)),
        Err(StoreError::NotFound(_)) => Ok(None),
        Err(StoreError::ReadCapacity | StoreError::ValidationBusy) => Err(ApiError::Capacity),
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
