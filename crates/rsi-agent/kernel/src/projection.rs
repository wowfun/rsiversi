//! Bounded extension reads retain the authoritative Session generation without a claim.

use super::*;
use futures_util::future::{BoxFuture, Shared, WeakShared};
use rsi_agent_composition_protocol::{SessionProjectionAdapter, SessionProjectionContext};
use rsi_agent_session_protocol::{DomainStateView, ProjectionCursor, SessionProjectionSnapshot};
use rsi_agent_turn_protocol::SessionProjections;

pub(super) const MAXIMUM_CAPTURES: usize = 16;

type Capture = BoxFuture<'static, TurnResult<SessionProjectionSnapshot>>;
pub(super) struct Flight {
    identity: Arc<()>,
    session: SessionId,
    header: String,
    pin: AgentCompositionPin,
    fact_seq: u64,
    control_seq: u64,
    capture: WeakShared<Capture>,
}
struct FlightCleanup {
    registry: Weak<Mutex<Vec<Flight>>>,
    identity: Arc<()>,
}
impl Drop for FlightCleanup {
    fn drop(&mut self) {
        if let Some(registry) = self.registry.upgrade() {
            // Drop the pin outside the registry: generation cleanup is not ours to poll.
            let removed = {
                let mut entries = registry.lock().expect("projection flights");
                entries
                    .iter()
                    .position(|entry| Arc::ptr_eq(&entry.identity, &self.identity))
                    .map(|index| entries.swap_remove(index))
            };
            drop(removed);
        }
    }
}

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
    fn activity_revision(&self) -> Option<u64> {
        self.inner.session_changes.revision()
    }
    fn resident_activity(&self) -> TurnResult<rsi_agent_turn_protocol::ResidentActivityPage> {
        let state = lock_state(&self.inner);
        if !state.accepting {
            return Err(TurnError::ShuttingDown);
        }
        Ok(rsi_agent_turn_protocol::ResidentActivityPage {
            entries: state
                .sessions
                .iter()
                .take(64)
                .map(|(id, session)| rsi_agent_turn_protocol::ResidentActivity {
                    session: id.clone(),
                    running: session.turns.values().any(|turn| turn.terminal.is_none()),
                })
                .collect(),
            has_more: state.sessions.len() > 64,
        })
    }
    fn resident_composition(
        &self,
        session_id: &SessionId,
    ) -> TurnResult<rsi_agent_turn_protocol::ResidentComposition> {
        use rsi_agent_turn_protocol::ResidentComposition;
        Ok(match select_generation(&self.inner, session_id)? {
            Selection::Resident(header, pin) => ResidentComposition::Resident {
                header,
                source_digest: pin.source_digest().into(),
                manifest: pin.manifest(),
            },
            Selection::Loading(_) => ResidentComposition::Loading,
            Selection::Cold => ResidentComposition::NotResident,
        })
    }
    fn watch_projection_changes(
        &self,
        session_id: &SessionId,
    ) -> TurnResult<rsi_agent_turn_protocol::SessionProjectionChanges> {
        let observer = ObserverLease::acquire(&self.inner, ObserverKind::Projection)?;
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
            .map_err(|_| TurnError::ProjectionCapacity)?;
        let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
        tokio::select! {
            biased;
            () = self.inner.submission_admission.closed.cancelled() => Err(TurnError::ShuttingDown),
            result = tokio::time::timeout_at(deadline, self.shared_projection(session_id, deadline)) => result.map_err(|_| capture_timeout())?,
        }
    }
}

impl AgentKernel {
    async fn shared_projection(
        &self,
        session: &SessionId,
        deadline: tokio::time::Instant,
    ) -> TurnResult<SessionProjectionSnapshot> {
        let (header, pin) = self.projection_generation(session).await?;
        let fingerprint = header
            .fingerprint()
            .map_err(|error| TurnError::Invalid(error.to_string()))?;
        let demand = self
            .inner
            .store
            .read_watermarks(session)
            .await
            .map_err(turn_store_error)?;
        let capture: Shared<Capture> = {
            let mut entries = self
                .inner
                .projection_flights
                .lock()
                .expect("projection flights");
            let existing = entries
                .iter()
                .find(|entry| {
                    entry.session == *session
                        && entry.header == fingerprint
                        && entry.pin.same_generation(&pin)
                        && entry.fact_seq == demand.durable_fact_seq
                        && entry.control_seq == demand.durable_control_seq
                })
                .and_then(|entry| entry.capture.upgrade());
            if let Some(capture) = existing {
                capture
            } else {
                // Cancellation may release caller admission before its final cleanup locks
                // this registry. Bound those retiring entries as well as live captures.
                if entries.len() >= MAXIMUM_CAPTURES {
                    return Err(TurnError::ProjectionCapacity);
                }
                let identity = Arc::new(());
                // Construct the guard outside the async body so an unpolled flight is cleaned too.
                let cleanup = FlightCleanup {
                    registry: Arc::downgrade(&self.inner.projection_flights),
                    identity: identity.clone(),
                };
                let kernel = self.clone();
                let session_id = session.clone();
                let capture_pin = pin.clone();
                let cancellation = self.inner.submission_admission.closed.child_token();
                let cancel = cancellation.clone().drop_guard();
                let capture = async move {
                    let _cancel = cancel;
                    let result = tokio::select! {
                        biased;
                        () = cancellation.cancelled() => Err(TurnError::ShuttingDown),
                        result = tokio::time::timeout_at(deadline, kernel.capture_projection(&session_id, header, capture_pin, demand.durable_fact_seq, demand.durable_control_seq, cancellation.clone())) => result.map_err(|_| capture_timeout()).and_then(std::convert::identity),
                    };
                    // Remove before publishing Ready; outstanding waiters may retain Shared output.
                    drop(cleanup);
                    result
                }.boxed().shared();
                entries.push(Flight {
                    identity,
                    session: session.clone(),
                    header: fingerprint,
                    pin,
                    fact_seq: demand.durable_fact_seq,
                    control_seq: demand.durable_control_seq,
                    capture: capture.downgrade().expect("unpolled capture"),
                });
                capture
            }
        };
        capture.await
    }

    pub(super) async fn projection_generation(
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
            let prepared = self.pin_cold_composition(&header).await;
            // Even failed cold resolution must yield to a concurrently published resident pin.
            match select_generation(&self.inner, session_id)? {
                Selection::Resident(header, pin) => return Ok((header, pin)),
                Selection::Loading(load) => {
                    load.wait().await?;
                }
                Selection::Cold => {
                    return Ok((Arc::new(header), prepared?));
                }
            }
        }
    }

    async fn capture_projection(
        &self,
        session_id: &SessionId,
        header: Arc<SessionHeader>,
        pin: AgentCompositionPin,
        minimum_fact_seq: u64,
        minimum_control_seq: u64,
        cancellation: CancellationToken,
    ) -> TurnResult<SessionProjectionSnapshot> {
        let page = observation::read_domain_states_bounded(&self.inner, session_id, None)
            .await
            .map_err(turn_store_error)?;
        if page.durable_fact_seq < minimum_fact_seq
            || page.durable_control_seq < minimum_control_seq
        {
            return Err(TurnError::Invalid(
                "projection capture precedes requested durable watermarks".into(),
            ));
        }
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

fn capture_timeout() -> TurnError {
    TurnError::Invalid("Session projection capture deadline elapsed".into())
}
