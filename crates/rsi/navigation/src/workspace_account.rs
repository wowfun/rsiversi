use super::{
    ApiError, Arc, BoxFuture, CallOrigin, Document, DocumentChange, MetadataSize, Navigation,
    Result, SessionId, WorkspaceError, WorkspaceId, encoded_entry_bytes, storage_error,
};
use futures_util::StreamExt;
use std::collections::{HashMap, HashSet};

/// Account and partition metadata for an already authorized Session header.
#[derive(Debug, Default)]
pub struct WorkspaceCatalogMetadata {
    /// Current registration and committed position, when explicitly enrolled.
    pub account: Option<(WorkspaceId, usize)>,
    /// Whether the Session is archived.
    pub archived: bool,
    /// Whether the Session is pinned.
    pub pinned: bool,
    /// Latest partition change used for archive and pin ordering.
    pub partition_revision: Option<u64>,
}

/// Outcome of a shared Workspace Session account move.
#[derive(Debug, Eq, PartialEq)]
pub enum WorkspaceMoveOutcome {
    /// The order is durable, including an unchanged move.
    Applied(rsi_workspace_protocol::WorkspaceRecord),
    /// The selected Workspace is not registered.
    UnknownWorkspace,
    /// The Session or anchor does not belong to this account.
    InvalidMember,
}

pub(super) fn order_bytes(id: &WorkspaceId, members: &[SessionId]) -> Result<usize> {
    // SessionId admits unescaped ASCII. Count the brackets, string quotes and separators.
    let strings: usize = members.iter().map(|id| id.as_str().len() + 2).sum();
    encoded_entry_bytes(id.as_str(), 2 + strings + members.len().saturating_sub(1))
        .map_err(storage_error)
}

impl Document {
    pub(super) fn membership(
        &self,
        workspace: Option<WorkspaceId>,
        session: &SessionId,
    ) -> Option<WorkspaceId> {
        workspace.filter(|id| {
            self.workspace_orders
                .get(id)
                .is_some_and(|members| members.contains(session))
        })
    }
    pub(super) fn validate_orders(&self, accounting: &mut MetadataSize) -> Result<()> {
        let mut known = HashSet::new();
        for (id, order) in &self.workspace_orders {
            if order.is_empty() || order.iter().any(|id| !known.insert(id)) {
                return Err(ApiError::Invalid(
                    "invalid Workspace Session account order".into(),
                ));
            }
            accounting.order_members += order.len();
            accounting.orders = accounting
                .orders
                .with_entry(None, order_bytes(id, order)?)
                .map_err(storage_error)?;
        }
        Ok(())
    }

    pub(super) fn project_orders(
        &self,
        changes: Vec<(WorkspaceId, Vec<SessionId>)>,
    ) -> Result<DocumentChange> {
        let mut accounting = self.accounting.expect("validated metadata accounting");
        for (id, members) in &changes {
            let previous = self.workspace_orders.get(id);
            let prior_bytes = previous
                .map(|members| order_bytes(id, members))
                .transpose()?;
            accounting.order_members =
                accounting.order_members - previous.map_or(0, Vec::len) + members.len();
            accounting.orders = if members.is_empty() {
                prior_bytes
                    .map_or(Ok(accounting.orders), |bytes| {
                        accounting.orders.without_entry(bytes)
                    })
                    .map_err(storage_error)?
            } else {
                accounting
                    .orders
                    .with_entry(prior_bytes, order_bytes(id, members)?)
                    .map_err(storage_error)?
            };
        }
        let revision = self
            .revision
            .checked_add(1)
            .ok_or_else(|| ApiError::Invalid("navigation revision exhausted".into()))?;
        accounting.check(revision)?;
        Ok(DocumentChange {
            revision,
            accounting,
            records: vec![],
            orders: changes,
        })
    }
}

impl Navigation {
    /// Joins an authorized header page to the current accounts, in input order.
    pub async fn workspace_catalog_metadata(
        &self,
        sessions: &[rsi_agent_store_protocol::StoreRecentSession],
    ) -> Result<Vec<WorkspaceCatalogMetadata>> {
        let document = self.document();
        let mut rows: Vec<_> = sessions
            .iter()
            .map(|session| {
                let metadata = document.records.get(session.header.session_id());
                WorkspaceCatalogMetadata {
                    account: None,
                    archived: metadata.is_some_and(|row| row.archived),
                    pinned: metadata.is_some_and(|row| row.pinned),
                    partition_revision: metadata.and_then(|row| row.partition_revision),
                }
            })
            .collect();
        if sessions.is_empty() || document.workspace_orders.is_empty() {
            return Ok(rows);
        }
        let page: HashMap<_, _> = sessions
            .iter()
            .enumerate()
            .map(|(i, session)| (session.header.session_id(), i))
            .collect();
        let mut candidates = Vec::new();
        for (id, order) in &document.workspace_orders {
            for (position, member) in order.iter().enumerate() {
                if let Some(&index) = page.get(member) {
                    rows[index].account = Some((id.clone(), position));
                    candidates.push(index);
                }
            }
        }
        // A replacement registration at the same directory has a new identity.
        drop(document);
        let registrations = self.workspace_lookups(
            candidates
                .iter()
                .map(|&index| Some(sessions[index].header.coordinates())),
        );
        let mut checked = futures_util::stream::iter(candidates.iter().copied()).zip(registrations);
        while let Some((index, registered)) = checked.next().await {
            let registered = registered?;
            if rows[index].account.as_ref().map(|(id, _)| id) != registered.as_ref() {
                rows[index].account = None;
            }
        }
        Ok(rows)
    }

    /// Reads authorized identities and activity times in this registration's committed order.
    pub fn workspace_members(
        self: &Arc<Self>,
        origin: CallOrigin,
        workspace: &rsi_workspace_protocol::WorkspaceRecord,
    ) -> Result<BoxFuture<'static, Result<Vec<(SessionId, u64)>>>> {
        let admission = self
            .resolver
            .admit(&origin, workspace.coordinates.location())?;
        let id = workspace.id.clone();
        let coordinates = workspace.coordinates.clone();
        self.run_read(move |owner| {
            Box::pin(async move {
                let _admission = admission;
                let document = owner.document();
                let mut result = Vec::new();
                let mut leases = Vec::new();
                if let Some(members) = document.workspace_orders.get(&id) {
                    for member in members {
                        if let Some(row) =
                            super::visible_summary(owner.store.session_summary(member).await)?
                            && row.header.coordinates() == &coordinates
                            && owner.retain_header_view(&row.header, &origin, &mut leases)?
                        {
                            result.push((member.clone(), row.last_activity_ms));
                        }
                    }
                }
                super::finish_scope(&origin, &leases)?;
                Ok(result)
            })
        })
    }

    /// Tests membership in this registration's committed account.
    pub fn workspace_contains(&self, workspace: &WorkspaceId, session: &SessionId) -> bool {
        self.document()
            .workspace_orders
            .get(workspace)
            .is_some_and(|members| members.contains(session))
    }

    /// Attaches an authorized durable Session to the registration for its execution directory.
    pub fn attach_workspace_session(
        self: &Arc<Self>,
        origin: CallOrigin,
        workspace: WorkspaceId,
        session: SessionId,
    ) -> Result<BoxFuture<'static, Result<()>>> {
        self.run_mutation(move |owner| {
            Box::pin(async move {
                let _writer = owner
                    .writer
                    .acquire()
                    .await
                    .map_err(|_| ApiError::ShuttingDown)?;
                let header = owner
                    .metadata_header(&session)
                    .await?
                    .ok_or(ApiError::Unavailable)?;
                let _admission = owner.admit_metadata(&header, &origin)?;
                let registration =
                    owner
                        .workspace
                        .get(&workspace)
                        .await
                        .map_err(|error| match error {
                            WorkspaceError::Unknown(_) => {
                                ApiError::Invalid("unknown Workspace".into())
                            }
                            _ => ApiError::Unavailable,
                        })?;
                if registration.coordinates != *header.coordinates() {
                    return Err(ApiError::Invalid(
                        "Session belongs to another directory".into(),
                    ));
                }
                let current = owner.document();
                let members = current
                    .workspace_orders
                    .get(&workspace)
                    .map_or(&[][..], Vec::as_slice);
                if members.contains(&session) {
                    return Ok(());
                }
                let mut changes = Vec::new();
                // A validated Navigation document gives each identity at most one owner.
                for (former, ids) in &current.workspace_orders {
                    if former != &workspace
                        && let Some(at) = ids.iter().position(|id| id == &session)
                    {
                        let mut remaining = ids.clone();
                        remaining.remove(at);
                        changes.push((former.clone(), remaining));
                        break;
                    }
                }
                let mut order = Vec::with_capacity(members.len() + 1);
                order.push(session);
                order.extend_from_slice(members);
                changes.push((workspace, order));
                let change = current.project_orders(changes)?;
                drop(current);
                owner.publish(change).await?;
                Ok(())
            })
        })
    }

    /// Moves a durable Session inside its exact shared Workspace account.
    /// Uses the ordinary writer; an accepted commit survives loss of its waiter.
    pub fn insert_workspace_session_before(
        self: &Arc<Self>,
        origin: CallOrigin,
        workspace: WorkspaceId,
        session: SessionId,
        before: Option<SessionId>,
    ) -> Result<BoxFuture<'static, Result<WorkspaceMoveOutcome>>> {
        self.run_mutation(move |owner| {
            Box::pin(async move {
                let _permit = owner
                    .writer
                    .acquire()
                    .await
                    .map_err(|_| ApiError::ShuttingDown)?;
                let record = match owner.workspace.get(&workspace).await {
                    Ok(record) => record,
                    Err(WorkspaceError::Unknown(_)) => {
                        return Ok(WorkspaceMoveOutcome::UnknownWorkspace);
                    }
                    Err(_) => return Err(ApiError::Unavailable),
                };
                let visibility = owner.resolver.visibility(&origin)?;
                if !visibility
                    .locations()
                    .contains(record.coordinates.location())
                {
                    return Err(ApiError::Unauthorized);
                }
                let current = owner.document();
                let Some(members) = current.workspace_orders.get(&workspace) else {
                    return Ok(WorkspaceMoveOutcome::InvalidMember);
                };
                let mut admissions = [None, None];
                let mut positions = [0, members.len()];
                for (index, id) in std::iter::once(&session)
                    .chain(before.as_ref().filter(|id| *id != &session))
                    .enumerate()
                {
                    let Some(position) = members.iter().position(|member| member == id) else {
                        return Ok(WorkspaceMoveOutcome::InvalidMember);
                    };
                    positions[index] = position;
                    let Some(header) = owner.metadata_header(id).await? else {
                        return Ok(WorkspaceMoveOutcome::InvalidMember);
                    };
                    if header.coordinates() != &record.coordinates {
                        return Ok(WorkspaceMoveOutcome::InvalidMember);
                    }
                    // The retained authority must cover the commit, not only the membership read.
                    admissions[index] = Some(owner.admit_metadata(&header, &origin)?);
                }
                if before.as_ref() == Some(&session) {
                    return Ok(WorkspaceMoveOutcome::Applied(record));
                }
                let [from, at] = positions;
                if from + 1 != at {
                    let mut order = members.clone();
                    let session = order.remove(from);
                    order.insert(if at > from { at - 1 } else { at }, session);
                    let change = current.project_orders(vec![(workspace, order)])?;
                    drop(current);
                    owner.publish(change).await?;
                }
                Ok(WorkspaceMoveOutcome::Applied(record))
            })
        })
    }
}
