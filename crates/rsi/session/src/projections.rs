use super::{HandleState, LocalSessionHandle, Result, SessionError, map_turn_error};
use futures_util::StreamExt as _;
use rsi_agent_composition_protocol::{SessionProjectionAdapter, SessionProjectionContext};
use rsi_agent_session_protocol::{
    CommandRevision, DomainRevision, DomainStateView, ProjectionCursor,
};
use rsi_session_protocol::{ProjectionSnapshot, ProjectionStream};
use std::{sync::Arc, time::Duration};

impl LocalSessionHandle {
    pub(super) fn projection_changed(&self) {
        self.projection_changes.send_replace(());
    }

    async fn capture_projection(&self) -> Result<ProjectionSnapshot> {
        let reservation = self.projection_retention.reserve_capture()?;
        let cancellation = self.projection_stopped.child_token();
        let _guard = cancellation.clone().drop_guard();
        let deadline = self.execution.deadline_after(Duration::from_secs(30));
        tokio::select! {
            biased;
            () = cancellation.cancelled() => Err(SessionError::ShuttingDown),
            result = deadline.timeout(async {
                let _activity = self.begin_activity()?;
                self.reconcile_fresh_read().await?;
                let captured = {
                    let state = self.state.lock().await;
                    match &*state {
                        HandleState::Fresh(draft) => {
                            let CommandRevision::Draft { revision } = draft.revision() else { unreachable!("draft revision") };
                            let context = SessionProjectionContext::new(
                                Arc::new(draft.header().clone()), ProjectionCursor::Draft { revision },
                                draft.baseline().initial_states().into_iter().map(|snapshot| DomainStateView { revision: DomainRevision::new(0), snapshot }).collect(),
                            ).map_err(|error| SessionError::Invalid(error.to_string()))?;
                            Some((SessionProjectionAdapter::new(draft.composition().clone()), context))
                        }
                        HandleState::Attached(_) => None,
                        HandleState::Expired => return Err(SessionError::NotFound("draft lease".into())),
                    }
                };
                let snapshot = if let Some((adapter, context)) = captured {
                    adapter.snapshot(&context, &self.execution, cancellation.clone()).await.map_err(|error| SessionError::Invalid(error.to_string()))?
                } else {
                    self.projection_service.projection_snapshot(self.session_id()).await.map_err(map_turn_error)?
                };
                reservation.retain(snapshot)
            }) => result.map_err(|_| SessionError::Backend("Session projection capture deadline elapsed".into()))?,
        }
    }

    pub(super) async fn projection_stream(&self) -> Result<ProjectionStream> {
        let mut draft_changes = self.projection_changes.subscribe();
        let mut durable_changes = self
            .projection_service
            .watch_projection_changes(self.session_id())
            .map_err(map_turn_error)?;
        let initial = self.capture_projection().await?;
        let header = initial.snapshot().header_sha256().to_owned();
        let mut cursor = initial.snapshot().cursor();
        let handle = self.clone();
        Ok(Box::pin(async_stream::try_stream! {
            yield initial;
            loop {
                tokio::select! {
                    biased;
                    () = handle.projection_stopped.cancelled() => break,
                    changed = draft_changes.changed() => { if changed.is_err() { break; } }
                    changed = durable_changes.next() => { if changed.is_none() { break; } }
                }
                // Mark before capture: a mutation during the callback remains unseen.
                draft_changes.borrow_and_update();
                let next = handle.capture_projection().await?;
                if next.snapshot().header_sha256() != header { break; }
                let next_cursor = next.snapshot().cursor();
                if !next_cursor.can_follow(cursor) {
                    Err(SessionError::Backend("Session projection cursor regressed".into()))?;
                }
                if next_cursor != cursor {
                    cursor = next_cursor;
                    yield next;
                }
            }
        }))
    }
}
