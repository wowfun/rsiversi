use super::{ApiError, Arc, BoxFuture, Document, Navigation, NavigationEntry, Result};
use futures_util::{StreamExt, stream};
use rsi_agent_store_protocol::StoreOrderSeed;
use rsi_navigation_api::{
    OrderMember, OrderMembership, OrderScope, OrderSeed, SummaryPage, SummaryRequest,
};

impl Navigation {
    /// Reads complete manual-order membership from one Store snapshot.
    pub fn order_seed(
        self: &Arc<Self>,
        origin: &rsi_api_protocol::CallOrigin,
        scope: OrderScope,
    ) -> Result<BoxFuture<'static, Result<OrderSeed>>> {
        let visibility = self.resolver.visibility(origin)?;
        let origin = origin.clone();
        self.run(move |owner| {
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
                        let mut selected = Vec::new();
                        let mut leases = Vec::new();
                        let mut remap = std::collections::BTreeMap::new();
                        let mut visible_groups = Vec::new();
                        for member in members {
                            let Some(lease) = owner.scope_view(&member.session, &origin).await?
                            else {
                                continue;
                            };
                            leases.push(lease);
                            let group = if let Some(group) = remap.get(&member.group) {
                                *group
                            } else {
                                let group = u16::try_from(visible_groups.len())
                                    .map_err(|_| ApiError::Capacity)?;
                                visible_groups.push(
                                    groups
                                        .get(usize::from(member.group))
                                        .ok_or(ApiError::Unavailable)?
                                        .clone(),
                                );
                                remap.insert(member.group, group);
                                group
                            };
                            let metadata = document
                                .records
                                .get(&member.session)
                                .cloned()
                                .unwrap_or_default();
                            selected.push(OrderMember {
                                last_activity_ms: member.last_activity_ms.to_string(),
                                group,
                                session: member.session,
                                pinned: metadata.pinned,
                                archived: metadata.archived,
                            });
                        }
                        super::finish_scope(&origin, &leases)?;
                        OrderMembership::Available {
                            groups: visible_groups,
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
    /// Reads up to 64 exact summaries in requested order under one metadata revision.
    pub fn summaries(
        self: &Arc<Self>,
        origin: &rsi_api_protocol::CallOrigin,
        request: SummaryRequest,
    ) -> Result<BoxFuture<'static, Result<SummaryPage>>> {
        request.validate()?;
        let visibility = self.resolver.visibility(origin)?;
        let origin = origin.clone();
        self.run(move |owner| {
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
                let mut leases = Vec::new();
                for row in &mut rows {
                    if let Some(candidate) = row {
                        if let Some(lease) =
                            owner.scope_view(&candidate.session_id, &origin).await?
                        {
                            leases.push(lease);
                        } else {
                            *row = None;
                        }
                    }
                }
                let mut entries = Vec::with_capacity(rows.len());
                let lookups = owner.workspace_lookups(rows.iter().map(|row| {
                    row.as_ref()
                        .filter(|row| visibility.locations().contains(row.coordinates.location()))
                        .map(|row| &row.coordinates)
                }));
                let mut rows = stream::iter(&rows).zip(lookups);
                while let Some((row, workspace)) = rows.next().await {
                    entries.push(
                        if let Some(row) = row.as_ref().filter(|row| {
                            visibility.locations().contains(row.coordinates.location())
                        }) {
                            let workspace = workspace?;
                            Some(NavigationEntry {
                                metadata: document
                                    .records
                                    .get(&row.session_id)
                                    .cloned()
                                    .unwrap_or_default(),
                                session: row.session_id.clone(),
                                created_at_ms: row.created_at_ms.to_string(),
                                last_activity_ms: row.last_activity_ms.to_string(),
                                location: row.coordinates.location().clone(),
                                path: row.coordinates.path().into(),
                                workspace,
                            })
                        } else {
                            None
                        },
                    );
                }
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
