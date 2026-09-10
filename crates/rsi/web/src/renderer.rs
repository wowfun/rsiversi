use crate::projection::{Transcript, short};
use async_trait::async_trait;
use rsi_client::{ObservationFailure, ObservationKind, ObservationSink, ObservationSinkContract};
use rsi_meta::{
    ActivationPlan, ConfigValue, LocalContract, MetaError, PluginFactory, PreparedActivation,
};
use rsi_session_protocol::InteractionSnapshot;
use serde::{Deserialize, Serialize};
use std::sync::{Arc, Mutex};
use tokio::sync::watch;
use tokio_util::sync::CancellationToken;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RenderConfig {
    pub pane: u8,
    pub generation: u64,
}

#[derive(Clone, Debug, Serialize)]
pub(crate) struct Pending {
    pub id: String,
    pub owner: String,
    pub kind: &'static str,
    pub title: String,
}

#[derive(Debug, Default)]
pub(crate) struct RenderState {
    pub transcript: Transcript,
    pub history: Option<Transcript>,
    pub history_before: Option<u64>,
    pub history_more: bool,
    pub history_generation: u64,
    pub interactions: Option<InteractionSnapshot>,
    pub projections: Option<rsi_session_protocol::ProjectionSnapshot>,
    pub projection_notice: String,
    fact_notice: String,
    interaction_notice: String,
}

impl RenderState {
    pub fn notice(&self) -> String {
        [self.fact_notice.as_str(), self.interaction_notice.as_str()]
            .into_iter()
            .filter(|notice| !notice.is_empty())
            .collect::<Vec<_>>()
            .join("\n")
    }
    fn notice_mut(&mut self, kind: ObservationKind) -> &mut String {
        match kind {
            ObservationKind::Facts => &mut self.fact_notice,
            ObservationKind::Interactions => &mut self.interaction_notice,
            ObservationKind::Projections => &mut self.projection_notice,
        }
    }
}

#[derive(Debug)]
pub(crate) struct Renderer {
    pub state: Mutex<RenderState>,
    ready: watch::Sender<bool>,
    changed: watch::Sender<u64>,
    revision: Mutex<Arc<()>>,
    stop: CancellationToken,
}
impl Renderer {
    pub fn seed(&self, transcript: Transcript, before: Option<u64>, more: bool) {
        let mut state = self.state.lock().expect("Web renderer poisoned");
        if self.stop.is_cancelled() {
            return;
        }
        state.transcript = transcript;
        state.history_before = before;
        state.history_more = more;
        drop(state);
        self.ready.send_replace(true);
        self.changed();
    }
    pub fn revision(&self) -> Arc<()> {
        self.revision
            .lock()
            .expect("Web renderer revision poisoned")
            .clone()
    }
    pub fn changed(&self) {
        *self
            .revision
            .lock()
            .expect("Web renderer revision poisoned") = Arc::new(());
        self.changed
            .send_modify(|value| *value = value.saturating_add(1));
    }
    pub fn pending(&self) -> Vec<Pending> {
        let state = self.state.lock().expect("Web renderer poisoned");
        let Some(snapshot) = &state.interactions else {
            return Vec::new();
        };
        snapshot
            .questions()
            .iter()
            .map(|request| Pending {
                id: request.id.clone(),
                owner: request.session_id.clone(),
                kind: "question",
                title: short(&request.questions[0].prompt, 512).into(),
            })
            .chain(snapshot.approvals().iter().map(|request| Pending {
                id: request.id.clone(),
                owner: request.subject.session_id().into(),
                kind: "approval",
                title: short(&request.action, 512).into(),
            }))
            .collect()
    }
    async fn ready(&self) -> Result<(), ObservationFailure> {
        let mut receiver = self.ready.subscribe();
        loop {
            if self.stop.is_cancelled() {
                return Err(ObservationFailure::SinkStopped);
            }
            if *receiver.borrow_and_update() {
                return Ok(());
            }
            tokio::select! { biased;
                () = self.stop.cancelled() => return Err(ObservationFailure::SinkStopped),
                result = receiver.changed() => result.map_err(|_| ObservationFailure::SinkStopped)?,
            }
        }
    }
}
#[async_trait]
impl ObservationSink for Renderer {
    async fn observation(
        &self,
        update: rsi_agent_turn_protocol::SessionObservation,
    ) -> Result<(), ObservationFailure> {
        self.ready().await?;
        let mut state = self.state.lock().expect("Web renderer poisoned");
        if self.stop.is_cancelled() {
            return Err(ObservationFailure::SinkStopped);
        }
        state.transcript.observation(&update);
        state.fact_notice.clear();
        drop(state);
        self.changed();
        Ok(())
    }
    async fn interactions(&self, snapshot: InteractionSnapshot) -> Result<(), ObservationFailure> {
        self.ready().await?;
        let mut state = self.state.lock().expect("Web renderer poisoned");
        if self.stop.is_cancelled() {
            return Err(ObservationFailure::SinkStopped);
        }
        state.interactions = Some(snapshot);
        state.interaction_notice.clear();
        drop(state);
        self.changed();
        Ok(())
    }
    async fn projections(
        &self,
        snapshot: rsi_session_protocol::ProjectionSnapshot,
    ) -> Result<(), ObservationFailure> {
        self.ready().await?;
        let mut state = self.state.lock().expect("Web renderer poisoned");
        if self.stop.is_cancelled() {
            return Err(ObservationFailure::SinkStopped);
        }
        state.projections = Some(snapshot);
        state.projection_notice.clear();
        drop(state);
        self.changed();
        Ok(())
    }
    async fn reconnecting(
        &self,
        kind: ObservationKind,
        error: &ObservationFailure,
    ) -> Result<(), ObservationFailure> {
        self.ready().await?;
        let mut state = self.state.lock().expect("Web renderer poisoned");
        if self.stop.is_cancelled() {
            return Err(ObservationFailure::SinkStopped);
        }
        let notice = state.notice_mut(kind);
        *notice = short(&format!("Reconnecting {kind:?}: {error}"), 4096).into();
        drop(state);
        self.changed();
        Ok(())
    }
    async fn stopped(&self, kind: ObservationKind, error: &ObservationFailure) {
        if self.stop.is_cancelled() {
            return;
        }
        let mut state = self.state.lock().expect("Web renderer poisoned");
        if self.stop.is_cancelled() {
            return;
        }
        let notice = state.notice_mut(kind);
        *notice = short(
            &format!("{kind:?} observation stopped; reattach to continue: {error}"),
            4096,
        )
        .into();
        drop(state);
        self.changed();
    }
}

#[derive(Debug)]
pub(crate) struct RendererContract;
impl LocalContract for RendererContract {
    const KEY: &'static str = "rsi.web.renderer";
    type Service = Renderer;
}

#[derive(Debug)]
pub(crate) struct RendererFactory {
    pub changed: watch::Sender<u64>,
}
#[async_trait]
impl PluginFactory for RendererFactory {
    fn prepare(&self, desired: &ConfigValue) -> rsi_meta::Result<PreparedActivation> {
        let config: RenderConfig = serde_json::from_value(desired.clone())
            .map_err(|_| MetaError::InvalidInput("invalid Web renderer configuration".into()))?;
        if config.pane >= 2 || config.generation == 0 {
            return Err(MetaError::InvalidInput("invalid pane or generation".into()));
        }
        Ok(PreparedActivation::with_state(
            ConfigValue::Null,
            config,
            16,
        ))
    }
    async fn activate(&self, plan: ActivationPlan) -> rsi_meta::Result<()> {
        let renderer = Arc::new(Renderer {
            state: Mutex::new(RenderState::default()),
            revision: Mutex::default(),
            ready: watch::channel(false).0,
            changed: self.changed.clone(),
            stop: CancellationToken::new(),
        });
        let supplies = vec![
            plan.context()
                .provide_local::<ObservationSinkContract>(renderer.clone())?,
            plan.context()
                .provide_local::<RendererContract>(renderer.clone())?,
        ];
        plan.defer(
            "withdraw Web renderer",
            Box::new(move || {
                Box::pin(async move {
                    renderer.stop.cancel();
                    *renderer.state.lock().expect("Web renderer poisoned") = RenderState::default();
                    renderer.changed();
                    drop(supplies);
                    Ok(())
                })
            }),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rsi_agent_session_protocol::{SessionFact, SessionFactBody, TurnId, TurnOutcome};
    use rsi_agent_turn_protocol::{ObservationRetention, SessionObservation};
    #[tokio::test]
    async fn a_recovered_interaction_stream_clears_its_notice() {
        let renderer = Renderer {
            state: Mutex::new(RenderState::default()),
            revision: Mutex::default(),
            ready: watch::channel(true).0,
            changed: watch::channel(0).0,
            stop: CancellationToken::new(),
        };
        renderer
            .reconnecting(
                ObservationKind::Interactions,
                &ObservationFailure::Session(rsi_session_protocol::SessionError::Capacity),
            )
            .await
            .unwrap();
        assert!(
            renderer
                .state
                .lock()
                .unwrap()
                .notice()
                .contains("Reconnecting")
        );
        renderer
            .interactions(
                rsi_session_protocol::InteractionRetention::default()
                    .retain(vec![], vec![])
                    .unwrap(),
            )
            .await
            .unwrap();
        assert!(
            renderer.state.lock().unwrap().notice().is_empty(),
            "accepted replacement must clear stale reconnect notice"
        );
    }
    async fn verify_independent_recovery(
        renderer: &Renderer,
        update: &SessionObservation,
    ) -> rsi_session_protocol::InteractionRetention {
        renderer
            .reconnecting(ObservationKind::Facts, &ObservationFailure::Ended)
            .await
            .unwrap();
        renderer
            .reconnecting(ObservationKind::Interactions, &ObservationFailure::Ended)
            .await
            .unwrap();
        let interactions = rsi_session_protocol::InteractionRetention::default();
        renderer
            .interactions(interactions.retain(vec![], vec![]).unwrap())
            .await
            .unwrap();
        assert!(renderer.state.lock().unwrap().notice().contains("Facts"));
        assert!(
            !renderer
                .state
                .lock()
                .unwrap()
                .notice()
                .contains("Interactions")
        );
        assert!(interactions.retained_bytes() > 0);
        renderer.observation(update.clone()).await.unwrap();
        assert!(renderer.state.lock().unwrap().notice().is_empty());
        interactions
    }
    fn failed_projection(
        pool: &rsi_session_protocol::ProjectionRetention,
    ) -> rsi_session_protocol::ProjectionSnapshot {
        pool.reserve_capture()
            .unwrap()
            .retain(
                rsi_agent_session_protocol::SessionProjectionSnapshot::new(
                    rsi_agent_session_protocol::SessionId::new("fixture").unwrap(),
                    "a".repeat(64),
                    "b".repeat(64),
                    rsi_agent_session_protocol::ProjectionCursor::Draft { revision: 0 },
                    vec![
                        rsi_agent_session_protocol::ProjectionEntry::failed(
                            rsi_agent_session_protocol::ContributionId::new("fixture.failed")
                                .unwrap(),
                            "producer failed",
                        )
                        .unwrap(),
                    ],
                )
                .unwrap(),
            )
            .unwrap()
    }
    #[tokio::test]
    async fn real_renderer_holds_delivery_until_history_seed_and_fences_escaped_sink_on_withdrawal()
    {
        let runtime = rsi_meta::Runtime::with_execution(
            rsi_meta::RuntimeLimits::default(),
            rsi_meta::Execution::native(tokio::runtime::Handle::current()),
        )
        .unwrap();
        let (changed, _) = watch::channel(0);
        let fiber = runtime
            .root()
            .apply(
                rsi_meta::ResolvedFactory::linked(
                    "renderer",
                    "test",
                    rsi_meta::UpdateMode::Replayable,
                    Arc::new(RendererFactory { changed }),
                ),
                serde_json::json!({"pane":0,"generation":1}),
            )
            .await
            .unwrap();
        assert_eq!(fiber.snapshot().state, rsi_meta::FiberState::Active);
        let renderer = runtime.root().lookup_local::<RendererContract>().unwrap();
        let update = SessionObservation::Fact {
            fact: ObservationRetention::default()
                .retain_fact(Arc::new(
                    SessionFact::new(
                        2,
                        1,
                        SessionFactBody::TurnTerminal {
                            turn_id: TurnId::new("turn").unwrap(),
                            outcome: TurnOutcome::Completed,
                        },
                    )
                    .unwrap(),
                ))
                .unwrap(),
            durable_fact_seq: 100,
        };
        let mut delivery = Box::pin(renderer.observation(update.clone()));
        assert!(futures_util::poll!(delivery.as_mut()).is_pending());
        renderer.seed(Transcript::default(), Some(1), false);
        delivery.await.unwrap();
        assert_eq!(renderer.state.lock().unwrap().transcript.seq, 2);
        assert_eq!(
            renderer.state.lock().unwrap().transcript.status,
            "Completed"
        );
        let interactions = verify_independent_recovery(&renderer, &update).await;
        let pool = rsi_session_protocol::ProjectionRetention::default();
        let snapshot = failed_projection(&pool);
        renderer.projections(snapshot.clone()).await.unwrap();
        renderer
            .stopped(ObservationKind::Projections, &ObservationFailure::Ended)
            .await;
        {
            let state = renderer.state.lock().unwrap();
            assert!(
                state
                    .projection_notice
                    .contains("Projections observation stopped")
            );
            assert!(state.notice().is_empty());
            assert!(
                state.projections.as_ref().unwrap().snapshot().entries()[0]
                    .failure()
                    .is_some()
            );
        }
        renderer.observation(update.clone()).await.unwrap();
        renderer.projections(snapshot.clone()).await.unwrap();
        assert!(renderer.state.lock().unwrap().projection_notice.is_empty());
        assert_eq!(
            pool.retained_bytes(),
            snapshot.snapshot().encoded_len().unwrap()
        );
        assert!(runtime.shutdown().await.is_clean());
        assert!(renderer.state.lock().unwrap().projections.is_none());
        assert!(renderer.state.lock().unwrap().interactions.is_none());
        assert_eq!(interactions.retained_bytes(), 0);
        assert!(matches!(
            renderer.projections(snapshot).await,
            Err(ObservationFailure::SinkStopped)
        ));
        assert_eq!(pool.retained_bytes(), 0);
        assert!(matches!(
            renderer.observation(update).await,
            Err(ObservationFailure::SinkStopped)
        ));
    }
}
