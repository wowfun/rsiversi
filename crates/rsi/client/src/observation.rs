use async_trait::async_trait;
use futures_util::StreamExt;
use rsi_agent_turn_protocol::{ObservationCursor, SessionObservation};
use rsi_meta_execution::Execution;
use rsi_session_protocol::{InteractionSnapshot, ProjectionSnapshot, SessionError, SessionHandle};
use std::time::Duration;

/// Domain observation failure, independent of a renderer's error vocabulary.
#[derive(Debug, thiserror::Error)]
pub enum ObservationFailure {
    /// Session or API admission/open failure.
    #[error(transparent)]
    Session(#[from] SessionError),
    /// A live observation ended without continued service.
    #[error("Session observation ended")]
    Ended,
    /// The renderer no longer accepts updates.
    #[error("observation sink stopped")]
    SinkStopped,
}
impl ObservationFailure {
    fn is_capacity(&self) -> bool {
        matches!(
            self,
            Self::Session(
                SessionError::Capacity | SessionError::Api(rsi_api_protocol::ApiError::Capacity)
            )
        )
    }
}

/// Identifies the independent stream whose observation has stopped.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ObservationKind {
    /// Durable Fact/control replay.
    Facts,
    /// Live interaction replacement snapshots.
    Interactions,
    /// Pure extension-state replacement snapshots, including fresh drafts.
    Projections,
}

/// One explicit delivery boundary. Returning success acknowledges the exact item.
#[async_trait]
pub trait ObservationSink: std::fmt::Debug + Send + Sync {
    /// Delivers a record before the controller advances its replay cursor.
    async fn observation(&self, update: SessionObservation) -> Result<(), ObservationFailure>;
    /// Delivers a replacement interaction snapshot.
    async fn interactions(&self, snapshot: InteractionSnapshot) -> Result<(), ObservationFailure>;
    /// Delivers a complete replacement and its final-clone retention lease.
    async fn projections(&self, snapshot: ProjectionSnapshot) -> Result<(), ObservationFailure>;
    /// Reports an impending retry without acknowledging any domain record.
    async fn reconnecting(
        &self,
        kind: ObservationKind,
        error: &ObservationFailure,
    ) -> Result<(), ObservationFailure>;
    /// Reports final failure; the renderer owns recovery guidance and presentation.
    async fn stopped(&self, kind: ObservationKind, error: &ObservationFailure);
}

/// Surface-local renderer capability consumed by the ordinary controller plugin.
#[derive(Debug)]
pub struct ObservationSinkContract;
impl rsi_meta::LocalContract for ObservationSinkContract {
    const KEY: &'static str = "rsi.client.observation-sink";
    type Service = dyn ObservationSink;
}

struct Retry {
    delay: Duration,
    failures: u8,
}
impl Default for Retry {
    fn default() -> Self {
        Self {
            delay: Duration::from_millis(250),
            failures: 0,
        }
    }
}
impl Retry {
    async fn after(
        &mut self,
        error: ObservationFailure,
        kind: ObservationKind,
        sink: &dyn ObservationSink,
        execution: &Execution,
    ) -> Result<(), ObservationFailure> {
        if matches!(error, ObservationFailure::SinkStopped) {
            return Err(error);
        }
        if !error.is_capacity() {
            self.failures += 1;
        }
        if self.failures >= 5 {
            return Err(error);
        }
        sink.reconnecting(kind, &error).await?;
        execution.sleep(self.delay).await;
        self.delay = (self.delay * 2).min(Duration::from_secs(2));
        Ok(())
    }
}

/// Drives exact-cursor observation until cancellation by its owner or final failure.
pub async fn observe_session(
    handle: &dyn SessionHandle,
    mut cursor: ObservationCursor,
    sink: &dyn ObservationSink,
    execution: &Execution,
) -> Result<(), ObservationFailure> {
    let mut retry = Retry::default();
    loop {
        let result: Result<(), ObservationFailure> = async {
            let mut stream = handle.observe(cursor).await?;
            while let Some(update) = stream.next().await {
                let update = update?;
                let mut delivered = cursor;
                match &update {
                    SessionObservation::Control { record, .. } => {
                        delivered.control_seq = record.seq();
                    }
                    SessionObservation::Fact { fact, .. } => delivered.fact_seq = fact.seq(),
                }
                sink.observation(update).await?;
                cursor = delivered;
                retry = Retry::default();
            }
            Err(ObservationFailure::Ended)
        }
        .await;
        if let Err(error) = result {
            retry
                .after(error, ObservationKind::Facts, sink, execution)
                .await?;
        }
    }
}

/// Drives replacement interaction snapshots using the same bounded retry policy.
pub async fn observe_interactions(
    handle: &dyn SessionHandle,
    sink: &dyn ObservationSink,
    execution: &Execution,
) -> Result<(), ObservationFailure> {
    let mut retry = Retry::default();
    loop {
        let result: Result<(), ObservationFailure> = async {
            let mut stream = handle.observe_interactions().await?;
            while let Some(snapshot) = stream.next().await {
                sink.interactions(snapshot?).await?;
                retry = Retry::default();
            }
            Err(ObservationFailure::Ended)
        }
        .await;
        if let Err(error) = result {
            retry
                .after(error, ObservationKind::Interactions, sink, execution)
                .await?;
        }
    }
}

/// Drives independent complete projection baselines and replacements.
pub async fn observe_projections(
    handle: &dyn SessionHandle,
    sink: &dyn ObservationSink,
    execution: &Execution,
) -> Result<(), ObservationFailure> {
    projection_updates(handle, sink, execution, None).await
}

pub(crate) async fn projection_updates(
    handle: &dyn SessionHandle,
    sink: &dyn ObservationSink,
    execution: &Execution,
    cache: Option<&tokio::sync::watch::Sender<Option<ProjectionSnapshot>>>,
) -> Result<(), ObservationFailure> {
    let mut retry = Retry::default();
    loop {
        let result: Result<(), ObservationFailure> = async {
            let mut stream = handle.observe_projections().await?;
            while let Some(snapshot) = stream.next().await {
                let snapshot = snapshot?;
                sink.projections(snapshot.clone()).await?;
                if let Some(cache) = cache {
                    cache.send_replace(Some(snapshot));
                }
                retry = Retry::default();
            }
            Err(ObservationFailure::Ended)
        }
        .await;
        if let Err(error) = result {
            retry
                .after(error, ObservationKind::Projections, sink, execution)
                .await?;
        }
    }
}
