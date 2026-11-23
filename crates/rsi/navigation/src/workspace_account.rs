use super::{
    ApiError, Arc, BoxFuture, CallOrigin, Document, DocumentChange, MetadataSize, Navigation,
    Result, SessionId, WorkspaceAccount, WorkspaceError, WorkspaceId, encoded_entry_bytes,
    storage_error,
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

fn order_bytes(id: &WorkspaceId, account: &WorkspaceAccount) -> Result<usize> {
    let members = &account.members;
    // SessionId admits unescaped ASCII. Count the brackets, string quotes and separators.
    let strings: usize = members.iter().map(|id| id.as_str().len() + 2).sum();
    let bytes = br#"{"updated_at_ms":,"members":[]}"#.len()
        + account.updated_at_ms.checked_ilog10().unwrap_or(0) as usize
        + 1
        + strings
        + members.len().saturating_sub(1);
    encoded_entry_bytes(id.as_str(), bytes).map_err(storage_error)
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
                .is_some_and(|account| account.members.contains(session))
        })
    }
    pub(super) fn validate_orders(&self, accounting: &mut MetadataSize) -> Result<()> {
        let mut known = HashSet::new();
        for (id, order) in &self.workspace_orders {
            if order.updated_at_ms > rsi_workspace_protocol::MAXIMUM_WORKSPACE_TIMESTAMP_MS
                || order.members.iter().any(|id| !known.insert(id))
            {
                return Err(ApiError::Invalid(
                    "invalid Workspace Session account order".into(),
                ));
            }
            accounting.order_members += order.members.len();
            accounting.orders = accounting
                .orders
                .with_entry(None, order_bytes(id, order)?)
                .map_err(storage_error)?;
        }
        Ok(())
    }

    pub(super) fn project_orders(
        &self,
        changes: Vec<(WorkspaceId, Option<Vec<SessionId>>)>,
    ) -> Result<DocumentChange> {
        let revision = self
            .revision
            .checked_add(1)
            .ok_or_else(|| ApiError::Invalid("navigation revision exhausted".into()))?;
        let mut change = DocumentChange {
            revision,
            accounting: self.accounting.expect("validated metadata accounting"),
            records: vec![],
            orders: Vec::with_capacity(changes.len()),
        };
        for (id, members) in changes {
            let account = match members {
                Some(members) => Some(WorkspaceAccount {
                    updated_at_ms: account_time()?,
                    members,
                }),
                None => None,
            };
            change.set_account(self, id, account)?;
        }
        change.accounting.check(revision)?;
        Ok(change)
    }
}

pub(super) fn account_time() -> Result<u64> {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|_| ApiError::Unavailable)?
        .as_millis()
        .try_into()
        .map_err(|_| ApiError::Unavailable)
}

impl DocumentChange {
    pub(super) fn set_account(
        &mut self,
        document: &Document,
        id: WorkspaceId,
        account: Option<WorkspaceAccount>,
    ) -> Result<()> {
        let previous = document.workspace_orders.get(&id);
        let previous_bytes = previous.map(|row| order_bytes(&id, row)).transpose()?;
        self.accounting.order_members -= previous.map_or(0, |row| row.members.len());
        if let Some(next) = &account {
            self.accounting.order_members += next.members.len();
            self.accounting.orders = self
                .accounting
                .orders
                .with_entry(previous_bytes, order_bytes(&id, next)?)
                .map_err(storage_error)?;
        } else if let Some(bytes) = previous_bytes {
            self.accounting.orders = self
                .accounting
                .orders
                .without_entry(bytes)
                .map_err(storage_error)?;
        }
        self.orders.push((id, account));
        Ok(())
    }
}

impl Navigation {
    pub(super) async fn discard_accounts(
        self: &Arc<Self>,
        retired: Vec<WorkspaceId>,
    ) -> Result<()> {
        self.run_mutation(move |owner| {
            Box::pin(async move {
                let _writer = owner
                    .writer
                    .acquire()
                    .await
                    .map_err(|_| ApiError::ShuttingDown)?;
                let current = owner.document();
                let removals: Vec<_> = retired
                    .into_iter()
                    .filter(|id| current.workspace_orders.contains_key(id))
                    .map(|id| (id, None))
                    .collect();
                if !removals.is_empty() {
                    let change = current.project_orders(removals)?;
                    drop(current);
                    owner.publish(change).await?;
                }
                Ok(())
            })
        })?
        .await
    }

    pub(super) async fn reconcile_accounts(self: &Arc<Self>) -> Result<()> {
        let document = self.document();
        let mut retired = Vec::new();
        for id in document.workspace_orders.keys() {
            if matches!(
                self.workspace.get(id).await,
                Err(WorkspaceError::Unknown(_))
            ) {
                retired.push(id.clone());
            }
        }
        drop(document);
        if !retired.is_empty() {
            self.discard_accounts(retired).await?;
        }
        Ok(())
    }

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
            for (position, member) in order.members.iter().enumerate() {
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

    /// Last committed membership change of this registration, including an empty account.
    pub fn workspace_changed_at(&self, workspace: &WorkspaceId) -> Option<u64> {
        self.document()
            .workspace_orders
            .get(workspace)
            .map(|row| row.updated_at_ms)
    }

    /// Reads authorized identities in this registration's committed order.
    pub fn workspace_members(
        self: &Arc<Self>,
        origin: CallOrigin,
        workspace: &rsi_workspace_protocol::WorkspaceRecord,
    ) -> Result<BoxFuture<'static, Result<(Vec<SessionId>, Option<u64>)>>> {
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
                    for member in &members.members {
                        if let Some(row) =
                            super::visible_summary(owner.store.session_summary(member).await)?
                            && row.header.coordinates() == &coordinates
                            && owner.retain_header_view(&row.header, &origin, &mut leases)?
                        {
                            result.push(member.clone());
                        }
                    }
                }
                super::finish_scope(&origin, &leases)?;
                Ok((
                    result,
                    document
                        .workspace_orders
                        .get(&id)
                        .map(|row| row.updated_at_ms),
                ))
            })
        })
    }

    /// Tests membership in this registration's committed account.
    pub fn workspace_contains(&self, workspace: &WorkspaceId, session: &SessionId) -> bool {
        self.document()
            .workspace_orders
            .get(workspace)
            .is_some_and(|account| account.members.contains(session))
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
                    .map_or(&[][..], |row| row.members.as_slice());
                if members.contains(&session) {
                    return Ok(());
                }
                let mut changes = Vec::new();
                // A validated Navigation document gives each identity at most one owner.
                for (former, ids) in &current.workspace_orders {
                    if former != &workspace
                        && let Some(at) = ids.members.iter().position(|id| id == &session)
                    {
                        let mut remaining = ids.members.clone();
                        remaining.remove(at);
                        changes
                            .push((former.clone(), (!remaining.is_empty()).then_some(remaining)));
                        break;
                    }
                }
                let mut order = Vec::with_capacity(members.len() + 1);
                order.push(session);
                order.extend_from_slice(members);
                changes.push((workspace, Some(order)));
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
                let Some(account) = current.workspace_orders.get(&workspace) else {
                    return Ok(WorkspaceMoveOutcome::InvalidMember);
                };
                let members = &account.members;
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
                    let change = current.project_orders(vec![(workspace, Some(order))])?;
                    drop(current);
                    owner.publish(change).await?;
                }
                Ok(WorkspaceMoveOutcome::Applied(record))
            })
        })
    }
}
