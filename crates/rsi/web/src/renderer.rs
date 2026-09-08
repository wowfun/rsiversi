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
    pub notice: String,
}

#[derive(Debug)]
pub(crate) struct Renderer {
    pub state: Mutex<RenderState>,
    ready: watch::Sender<bool>,
    changed: watch::Sender<u64>,
    stop: CancellationToken,
}
impl Renderer {
    pub fn seed(&self, transcript: Transcript, before: Option<u64>, more: bool) {
        let mut state = self.state.lock().expect("Web renderer poisoned");
        state.transcript = transcript;
        state.history_before = before;
        state.history_more = more;
        drop(state);
        self.ready.send_replace(true);
        self.changed();
    }
    pub fn changed(&self) {
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
        self.state
            .lock()
            .expect("Web renderer poisoned")
            .transcript
            .observation(&update);
        self.changed();
        Ok(())
    }
    async fn interactions(&self, snapshot: InteractionSnapshot) -> Result<(), ObservationFailure> {
        self.ready().await?;
        self.state
            .lock()
            .expect("Web renderer poisoned")
            .interactions = Some(snapshot);
        self.changed();
        Ok(())
    }
    async fn reconnecting(&self, error: &ObservationFailure) -> Result<(), ObservationFailure> {
        self.ready().await?;
        self.state.lock().expect("Web renderer poisoned").notice =
            short(&format!("Reconnecting: {error}"), 4096).into();
        self.changed();
        Ok(())
    }
    async fn stopped(&self, kind: ObservationKind, error: &ObservationFailure) {
        if self.stop.is_cancelled() {
            return;
        }
        self.state.lock().expect("Web renderer poisoned").notice = short(
            &format!("{kind:?} observation stopped; reattach to continue: {error}"),
            4096,
        )
        .into();
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
        assert!(runtime.shutdown().await.is_clean());
        assert!(matches!(
            renderer.observation(update).await,
            Err(ObservationFailure::SinkStopped)
        ));
    }
}
