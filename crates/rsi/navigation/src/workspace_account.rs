use super::{
    ApiError, Arc, BoxFuture, CallOrigin, Document, MetadataSize, Navigation, Result, SessionId,
    WorkspaceError, WorkspaceId, encoded_entry_bytes, storage_error,
};
use rsi_agent_store_protocol::{MAXIMUM_ORDER_MEMBERS, StoreOrderMember, StoreOrderSeed};
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

/// New members precede the saved subsequence; activity never participates.
pub(super) fn account_order<'a>(
    saved: &[SessionId],
    members: impl Iterator<Item = (&'a SessionId, u64)>,
) -> Vec<SessionId> {
    let mut members = members.collect::<Vec<_>>();
    members.sort_unstable_by(|(a, at), (b, bt)| bt.cmp(at).then_with(|| b.cmp(a)));
    if saved.is_empty() {
        return members.into_iter().map(|(id, _)| id.clone()).collect();
    }
    let saved_set = saved.iter().collect::<HashSet<_>>();
    let visible = members.iter().map(|(id, _)| *id).collect::<HashSet<_>>();
    members
        .iter()
        .filter(|(id, _)| !saved_set.contains(id))
        .map(|(id, _)| (*id).clone())
        .chain(saved.iter().filter(|id| visible.contains(id)).cloned())
        .collect()
}

fn order_bytes(id: &WorkspaceId, members: &[SessionId]) -> Result<usize> {
    encoded_entry_bytes(
        id.as_str(),
        serde_json::to_vec(members)
            .map_err(|_| ApiError::Unavailable)?
            .len(),
    )
    .map_err(storage_error)
}

impl Document {
    pub(super) fn validate_orders(&self, accounting: &mut MetadataSize) -> Result<()> {
        let mut known = HashSet::new();
        for (id, order) in &self.workspace_orders {
            if order.is_empty()
                || order.len() > MAXIMUM_ORDER_MEMBERS
                || order.iter().any(|id| !known.insert(id))
            {
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

    pub(super) fn edit_order(&mut self, id: &WorkspaceId, members: Vec<SessionId>) -> Result<()> {
        if self.accounting.is_none() {
            self.validate()?;
        }
        let mut accounting = self.accounting.expect("validated metadata accounting");
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
                .with_entry(prior_bytes, order_bytes(id, &members)?)
                .map_err(storage_error)?
        };
        let revision = self
            .revision
            .checked_add(1)
            .ok_or_else(|| ApiError::Invalid("navigation revision exhausted".into()))?;
        accounting.check(revision)?;
        if members.is_empty() {
            self.workspace_orders.remove(id);
        } else {
            self.workspace_orders.insert(id.clone(), members);
        }
        self.accounting = Some(accounting);
        self.revision = revision;
        Ok(())
    }
}

impl Navigation {
    /// Moves a durable Session inside its exact shared Workspace account.
    /// Uses the ordinary writer; an accepted commit survives loss of its waiter.
    ///
    /// # Panics
    /// Panics if an internal account projection drops a member just admitted from
    /// the same immutable Store snapshot.
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
                let StoreOrderSeed::Available { members, .. } = owner
                    .store
                    .session_order_seed(visibility.locations(), Some(&record.coordinates))
                    .await
                    .map_err(|_| ApiError::Unavailable)?
                else {
                    return Err(ApiError::Capacity);
                };
                let mut admissions = [None, None];
                for (index, id) in std::iter::once(&session)
                    .chain(before.as_ref().filter(|id| *id != &session))
                    .enumerate()
                {
                    if !members.iter().any(|member| &member.session == id) {
                        return Ok(WorkspaceMoveOutcome::InvalidMember);
                    }
                    let Some(header) = owner.metadata_header(id).await? else {
                        return Ok(WorkspaceMoveOutcome::InvalidMember);
                    };
                    // The retained authority must cover the commit, not only the membership read.
                    admissions[index] = Some(owner.admit_metadata(&header, &origin)?);
                }
                if before.as_ref() == Some(&session) {
                    return Ok(WorkspaceMoveOutcome::Applied);
                }
                let current = owner.document();
                let mut order = account_order(
                    current
                        .workspace_orders
                        .get(&workspace)
                        .map_or(&[], Vec::as_slice),
                    members.iter().map(
                        |StoreOrderMember {
                             session,
                             created_at_ms,
                             ..
                         }| (session, *created_at_ms),
                    ),
                );
                let from = order
                    .iter()
                    .position(|id| id == &session)
                    .expect("admitted account member");
                let at = before.as_ref().map_or(order.len(), |anchor| {
                    order
                        .iter()
                        .position(|id| id == anchor)
                        .expect("admitted account anchor")
                });
                if from + 1 != at {
                    let session = order.remove(from);
                    order.insert(if at > from { at - 1 } else { at }, session);
                    let mut updated = current.as_ref().clone();
                    updated.edit_order(&workspace, order)?;
                    owner.publish_document(updated).await?;
                }
                Ok(WorkspaceMoveOutcome::Applied)
            })
        })
    }
}
