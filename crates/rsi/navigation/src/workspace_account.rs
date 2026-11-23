use super::{
    ApiError, Arc, BoxFuture, CallOrigin, Document, DocumentChange, MetadataSize, Navigation,
    Result, SessionId, WorkspaceError, WorkspaceId, encoded_entry_bytes, storage_error,
};
use std::collections::HashSet;

/// Outcome of a shared Workspace Session account move.
#[derive(Debug, Eq, PartialEq)]
pub enum WorkspaceMoveOutcome {
    /// The order is durable, including an unchanged move.
    Applied,
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
                    return Ok(WorkspaceMoveOutcome::Applied);
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
                Ok(WorkspaceMoveOutcome::Applied)
            })
        })
    }
}
