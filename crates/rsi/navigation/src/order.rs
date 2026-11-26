use super::{
    ApiError, Arc, BoxFuture, CallOrigin, Document, Navigation, Result, SessionId, navigation_entry,
};
use futures_util::{StreamExt, TryStreamExt, stream};
use rsi_agent_store_protocol::{MAXIMUM_ACCESS_SESSIONS, StoreOrderSeed};
use rsi_navigation_api::{
    OrderMember, OrderMembership, OrderScope, OrderSeed, SummaryPage, SummaryRequest,
};

impl Navigation {
    /// Reads complete manual-order membership from one Store snapshot.
    ///
    /// # Panics
    /// Panics if the Store violates its bounded membership contract or an internal
    /// account projection fails to assign an admitted member a position.
    pub fn order_seed(
        self: &Arc<Self>,
        origin: &rsi_api_protocol::CallOrigin,
        scope: OrderScope,
    ) -> Result<BoxFuture<'static, Result<OrderSeed>>> {
        let visibility = self.resolver.visibility(origin)?;
        let origin = origin.clone();
        self.run_read(move |owner| {
            Box::pin(async move {
                let document = owner.document();
                if scope.coordinates().is_some_and(|coordinates| {
                    !visibility.locations().contains(coordinates.location())
                }) {
                    return Err(ApiError::Unauthorized);
                }
                let membership = match owner
                    .store
                    .session_order_seed(visibility.locations(), scope.coordinates())
                    .await
                    .map_err(|_| ApiError::Unavailable)?
                {
                    StoreOrderSeed::TooLarge => OrderMembership::TooLarge,
                    StoreOrderSeed::Available { members, groups } => {
                        let mut created = Vec::new();
                        let mut selected = Vec::new();
                        let mut leases = Vec::new();
                        for batch in members.chunks(MAXIMUM_ACCESS_SESSIONS) {
                            let ids = batch.iter().map(|m| m.session.clone()).collect::<Vec<_>>();
                            let access = owner.visible_batch(&ids, &origin, &mut leases).await?;
                            for (member, grant) in batch.iter().zip(access) {
                                let Some((session, blank)) = grant else {
                                    continue;
                                };
                                created.push(member.created_at_ms);
                                let metadata = document.records.get(&member.session);
                                selected.push(OrderMember {
                                    blank,
                                    account_position: 0,
                                    last_activity_ms: member.last_activity_ms.to_string(),
                                    group: member.group,
                                    session,
                                    pinned: metadata.is_some_and(|row| row.pinned),
                                    partition_revision: metadata
                                        .and_then(|row| row.partition_revision)
                                        .map(|r| r.to_string()),
                                    archived: metadata.is_some_and(|row| row.archived),
                                });
                            }
                        }
                        let groups = owner
                            .order_accounts(&document, &groups, &created, &mut selected)
                            .await?;
                        super::finish_scope(&origin, &leases)?;
                        OrderMembership::Available {
                            groups,
                            members: selected,
                        }
                    }
                };
                let seed = OrderSeed {
                    scope,
                    host_epoch: owner.epoch.clone(),
                    metadata_revision: document.revision.to_string(),
                    membership,
                };
                seed.bounded().map_err(|_| ApiError::Unavailable)
            })
        })
    }
    async fn visible_batch(
        &self,
        sessions: &[SessionId],
        origin: &CallOrigin,
        leases: &mut Vec<tokio_util::sync::CancellationToken>,
    ) -> Result<Vec<Option<(SessionId, bool)>>> {
        if sessions.is_empty() {
            return Ok(Vec::new());
        }
        let rows = self
            .store
            .session_access(sessions)
            .await
            .map_err(super::store_read_error)?;
        rows.into_iter()
            .map(|row| {
                let Some(row) = super::visible_summary(row)? else {
                    return Ok(None);
                };
                Ok(self
                    .retain_scope_view(row.protection.as_ref(), origin, leases)?
                    .then_some((row.session_id, row.before_first_turn)))
            })
            .collect()
    }
    async fn order_accounts(
        &self,
        document: &Document,
        coordinates: &[rsi_agent_session_protocol::ExecutionCoordinates],
        created: &[u64],
        selected: &mut [OrderMember],
    ) -> Result<Vec<rsi_navigation_api::OrderGroup>> {
        let mut visible_coordinates = Vec::new();
        let mut selected_groups = std::collections::HashMap::new();
        for member in selected.iter() {
            selected_groups.entry(member.group).or_insert_with(|| {
                let index = visible_coordinates.len();
                visible_coordinates.push(&coordinates[usize::from(member.group)]);
                index
            });
        }
        let registrations = self
            .workspace_lookups(visible_coordinates.into_iter().map(Some))
            .try_collect::<Vec<_>>()
            .await?;
        let mut positions = std::collections::HashMap::new();
        for (index, registration) in registrations.iter().enumerate() {
            if let Some(ids) = registration
                .as_ref()
                .and_then(|id| document.workspace_orders.get(id))
            {
                positions.extend(
                    ids.members
                        .iter()
                        .enumerate()
                        .map(|(rank, id)| (id, (index, rank))),
                );
            }
        }
        let mut groups = Vec::new();
        let mut indices = std::collections::HashMap::new();
        let mut ranks = Vec::with_capacity(selected.len());
        for (index, member) in selected.iter_mut().enumerate() {
            let registered = selected_groups[&member.group];
            let rank = positions
                .get(&member.session)
                .filter(|(account, _)| *account == registered)
                .map(|(_, rank)| *rank);
            let group = *indices
                .entry((member.group, rank.is_some()))
                .or_insert_with(|| {
                    let group = u16::try_from(groups.len()).expect("bounded order membership");
                    groups.push(rsi_navigation_api::OrderGroup {
                        coordinates: coordinates[usize::from(member.group)].clone(),
                        workspace: rank.and(registrations[registered].clone()),
                    });
                    group
                });
            member.group = group;
            ranks.push((
                group,
                rank,
                std::cmp::Reverse(created[index]),
                std::cmp::Reverse(index),
            ));
        }
        ranks.sort_unstable();
        for account in ranks.chunk_by(|a, b| a.0 == b.0) {
            for (position, (_, _, _, std::cmp::Reverse(index))) in account.iter().enumerate() {
                selected[*index].account_position =
                    u16::try_from(position).expect("bounded order membership");
            }
        }
        Ok(groups)
    }
    /// Reads up to 64 exact summaries in requested order under one metadata revision.
    pub fn summaries(
        self: &Arc<Self>,
        origin: &rsi_api_protocol::CallOrigin,
        request: SummaryRequest,
    ) -> Result<BoxFuture<'static, Result<SummaryPage>>> {
        request.validate()?;
        let visibility = self.resolver.visibility(origin)?;
        let origin = origin.clone();
        self.run_read(move |owner| {
            Box::pin(async move {
                let document = owner.document();
                if request.metadata_revision != document.revision.to_string() {
                    return Err(ApiError::Invalid(
                        "Navigation changed; refresh the complete order seed".into(),
                    ));
                }
                let mut rows = owner
                    .store
                    .session_activity_summaries(&request.sessions)
                    .await
                    .map_err(|_| ApiError::Unavailable)?;
                for slot in &mut rows {
                    if slot.as_ref().is_some_and(|row| {
                        !visibility.locations().contains(row.coordinates.location())
                    }) {
                        *slot = None;
                    }
                }
                let ids = rows
                    .iter()
                    .flatten()
                    .map(|row| row.session_id.clone())
                    .collect::<Vec<_>>();
                let mut leases = Vec::new();
                let mut grants = owner
                    .visible_batch(&ids, &origin, &mut leases)
                    .await?
                    .into_iter();
                let default_metadata = rsi_navigation_api::SessionMetadata::default();
                let mut entries = Vec::with_capacity(rows.len());
                for slot in &mut rows {
                    let entry = if let Some(row) = slot.as_ref()
                        && let Some((_, blank)) = grants.next().flatten()
                    {
                        let metadata = document
                            .records
                            .get(&row.session_id)
                            .map_or(&default_metadata, |stored| &stored.metadata);
                        Some(navigation_entry(row, blank, None, metadata))
                    } else {
                        *slot = None;
                        None
                    };
                    entries.push(entry);
                }
                let lookups = owner
                    .workspace_lookups(rows.iter().flatten().map(|row| Some(&row.coordinates)));
                let mut admitted = stream::iter(entries.iter_mut().flatten()).zip(lookups);
                while let Some((entry, workspace)) = admitted.next().await {
                    entry.workspace = document.membership(workspace?, &entry.session);
                }
                drop(admitted);
                super::finish_scope(&origin, &leases)?;
                Ok(SummaryPage {
                    metadata_revision: request.metadata_revision,
                    entries,
                })
            })
        })
    }
    pub(super) fn document(&self) -> Arc<Document> {
        self.state
            .lock()
            .expect("navigation state poisoned")
            .document
            .clone()
    }
}
