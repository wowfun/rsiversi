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
                    StoreOrderSeed::Available { members, groups } => OrderMembership::Available {
                        groups,
                        members: members
                            .into_iter()
                            .map(|member| {
                                let session = member.session;
                                let metadata =
                                    document.records.get(&session).cloned().unwrap_or_default();
                                OrderMember {
                                    last_activity_ms: member.last_activity_ms.to_string(),
                                    group: member.group,
                                    session,
                                    pinned: metadata.pinned,
                                    archived: metadata.archived,
                                }
                            })
                            .collect(),
                    },
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
        self.run(move |owner| {
            Box::pin(async move {
                let document = owner.document();
                if request.metadata_revision != document.revision.to_string() {
                    return Err(ApiError::Invalid(
                        "Navigation changed; refresh the complete order seed".into(),
                    ));
                }
                let rows = owner
                    .store
                    .session_activity_summaries(&request.sessions)
                    .await
                    .map_err(|_| ApiError::Unavailable)?;
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
