//! Bounded extension reads retain the authoritative Session generation without a claim.

use super::*;
use rsi_agent_composition_protocol::{SessionProjectionAdapter, SessionProjectionContext};
use rsi_agent_session_protocol::{DomainStateView, ProjectionCursor, SessionProjectionSnapshot};
use rsi_agent_turn_protocol::SessionProjections;

pub(super) const MAXIMUM_CAPTURES: usize = 16;

enum Selection {
    Resident(Arc<SessionHeader>, AgentCompositionPin),
    Loading(Arc<SessionLoad>),
    Cold,
}

fn select_generation(inner: &KernelInner, session_id: &SessionId) -> TurnResult<Selection> {
    let state = lock_state(inner);
    if !state.accepting {
        return Err(TurnError::ShuttingDown);
    }
    if let Some(session) = state.sessions.get(session_id) {
        return Ok(Selection::Resident(
            session.header.clone(),
            session.composition.clone(),
        ));
    }
    if state.fresh_reservations.contains(session_id) {
        return Err(TurnError::Invalid(
            "projection selected an unpublished Session".into(),
        ));
    }
    Ok(state
        .loading_sessions
        .get(session_id)
        .map_or(Selection::Cold, |load| Selection::Loading(load.clone())))
}

#[async_trait]
impl SessionProjections for AgentKernel {
    fn watch_projection_changes(
        &self,
        session_id: &SessionId,
    ) -> TurnResult<rsi_agent_turn_protocol::SessionProjectionChanges> {
        let observer = ObserverLease::acquire(&self.inner)?;
        let watch = self.inner.session_changes.session(session_id);
        let inner = Arc::downgrade(&self.inner);
        Ok(stream::unfold(
            (watch, inner, observer),
            |(mut watch, owner, observer)| async move {
                let inner = owner.upgrade()?;
                tokio::select! {
                    () = inner.stop_worker.cancelled() => None,
                    () = watch.changed() => Some(((), (watch, owner, observer))),
                }
            },
        )
        .boxed())
    }
    async fn projection_snapshot(
        &self,
        session_id: &SessionId,
    ) -> TurnResult<SessionProjectionSnapshot> {
        if self.inner.submission_admission.closed.is_cancelled() {
            return Err(TurnError::ShuttingDown);
        }
        let _admission = self
            .inner
            .projection_admission
            .clone()
            .try_acquire_owned()
            .map_err(|_| TurnError::ObserverCapacity)?;
        let cancellation = self.inner.submission_admission.closed.child_token();
        let _guard = cancellation.clone().drop_guard();
        tokio::select! {
            biased;
            () = cancellation.cancelled() => Err(TurnError::ShuttingDown),
            result = tokio::time::timeout(Duration::from_secs(30), self.capture_projection(session_id, cancellation.clone())) => result.map_err(|_| TurnError::Invalid("Session projection capture deadline elapsed".into()))?,
        }
    }
}

impl AgentKernel {
    async fn projection_generation(
        &self,
        session_id: &SessionId,
    ) -> TurnResult<(Arc<SessionHeader>, AgentCompositionPin)> {
        loop {
            match select_generation(&self.inner, session_id)? {
                Selection::Resident(header, pin) => return Ok((header, pin)),
                Selection::Loading(load) => {
                    load.wait().await?;
                    continue;
                }
                Selection::Cold => {}
            }
            let header = read_validated_header_bounded(&self.inner, session_id)
                .await
                .map_err(turn_store_error)?;
            let prepared = self.inner.composition.pin(header.agent_preset_id()).await;
            // Even failed cold resolution must yield to a concurrently published resident pin.
            match select_generation(&self.inner, session_id)? {
                Selection::Resident(header, pin) => return Ok((header, pin)),
                Selection::Loading(load) => {
                    load.wait().await?;
                }
                Selection::Cold => {
                    return Ok((Arc::new(header), prepared.map_err(turn_composition_error)?));
                }
            }
        }
    }

    async fn capture_projection(
        &self,
        session_id: &SessionId,
        cancellation: CancellationToken,
    ) -> TurnResult<SessionProjectionSnapshot> {
        let (header, pin) = self.projection_generation(session_id).await?;
        let page = observation::read_domain_states_bounded(&self.inner, session_id, None)
            .await
            .map_err(turn_store_error)?;
        let context = SessionProjectionContext::new(
            header,
            ProjectionCursor::Durable {
                fact_seq: page.durable_fact_seq,
                control_seq: page.durable_control_seq,
            },
            page.states
                .into_iter()
                .map(|state| DomainStateView {
                    revision: state.head.revision,
                    snapshot: state.snapshot,
                })
                .collect(),
        )
        .map_err(|error| TurnError::Invalid(error.to_string()))?;
        SessionProjectionAdapter::new(pin)
            .snapshot(
                &context,
                &rsi_meta::Execution::native(tokio::runtime::Handle::current()),
                cancellation,
            )
            .await
            .map_err(|error| TurnError::Invalid(error.to_string()))
    }
}
