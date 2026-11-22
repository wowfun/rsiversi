use crate::{InteractionSnapshot, ProjectionSnapshot, Result, SessionError, SessionHandle};
use futures_util::{Stream, StreamExt, stream};
use rsi_agent_turn_protocol::{ObservationCursor, ObservedModel, SessionObservation};
use serde::{Deserialize, Serialize};
use std::{pin::Pin, sync::Arc};

/// Independently opened sources in an attached conversation.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[repr(u8)]
pub enum SurfaceSource {
    /// Durable Facts and controls, sharing their replay cursors.
    History = 0,
    /// Unsettled model output.
    Model = 1,
    /// Pending questions and approvals.
    Interactions = 2,
    /// Extension projection snapshots.
    Projections = 3,
    /// Process-local Goal driver state.
    Goal = 4,
}

/// Read sources sharing one Session target and acknowledged durable cursors.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(try_from = "ViewRequest")]
pub struct SurfaceView {
    sources: u8,
    cursor: ObservationCursor,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ViewRequest {
    sources: u8,
    cursor: ObservationCursor,
}
impl TryFrom<ViewRequest> for SurfaceView {
    type Error = &'static str;
    fn try_from(input: ViewRequest) -> std::result::Result<Self, Self::Error> {
        if input.sources == 0 || input.sources & !Self::ALL != 0 {
            return Err("invalid surface sources");
        }
        Ok(Self {
            sources: input.sources,
            cursor: input.cursor,
        })
    }
}
impl SurfaceView {
    const DRAFT: u8 = SurfaceSource::Projections.bit() | SurfaceSource::Goal.bit();
    const PUBLISHED: u8 = SurfaceSource::History.bit()
        | SurfaceSource::Model.bit()
        | SurfaceSource::Interactions.bit();
    const ALL: u8 = Self::DRAFT | Self::PUBLISHED;
    /// State available before publication.
    pub fn draft() -> Self {
        Self {
            sources: Self::DRAFT,
            cursor: ObservationCursor::default(),
        }
    }
    /// Additional streams after a draft publishes.
    pub fn published(cursor: ObservationCursor) -> Self {
        Self {
            sources: Self::PUBLISHED,
            cursor,
        }
    }
    /// All observations for an already durable conversation.
    pub fn attached(cursor: ObservationCursor) -> Self {
        Self {
            sources: Self::ALL,
            cursor,
        }
    }
    /// Independent reconnect after a logical source failure.
    pub fn one(self, source: SurfaceSource) -> Self {
        Self {
            sources: source.bit(),
            ..self
        }
    }
    /// Remaining sources when a source has ended.
    pub fn without(self, source: SurfaceSource) -> Option<Self> {
        let sources = self.sources & !source.bit();
        (sources != 0).then_some(Self { sources, ..self })
    }
    /// Last acknowledged durable positions.
    pub fn cursor(self) -> ObservationCursor {
        self.cursor
    }
    /// Whether this bound view follows a logical source.
    pub fn includes(self, source: SurfaceSource) -> bool {
        self.sources & source.bit() != 0
    }
    /// Retain the selected sources and update their durable cursors.
    pub fn resume(self, cursor: ObservationCursor) -> Self {
        Self { cursor, ..self }
    }
}
impl SurfaceSource {
    const fn bit(self) -> u8 {
        1 << self as u8
    }
}
/// A delivered item from an authenticated conversation view.
#[derive(Debug)]
pub enum SurfaceUpdate {
    /// One durable record.
    History(SessionObservation),
    /// One retained model prefix.
    Model(ObservedModel),
    /// Complete current interactions.
    Interactions(InteractionSnapshot),
    /// Complete current projection state.
    Projections(ProjectionSnapshot),
    /// Current process-local Goal state.
    Goal(rsi_goal::GoalLiveState),
    /// One source stopped while its siblings remain attached.
    Stopped(SurfaceSource, SessionError),
}

/// Ordered items per source. Dropping the stream drops all of its sources.
pub type SurfaceStream = Pin<Box<dyn Stream<Item = Result<SurfaceUpdate>> + Send>>;

pub(super) fn observe<H: SessionHandle + ?Sized>(
    handle: Arc<H>,
    view: SurfaceView,
) -> SurfaceStream {
    type Channel<T> = Pin<Box<dyn Stream<Item = Result<T>> + Send>>;
    fn source<T: Send + 'static>(
        kind: SurfaceSource,
        open: impl Future<Output = Result<Channel<T>>> + Send + 'static,
        item: fn(T) -> SurfaceUpdate,
    ) -> SurfaceStream {
        Box::pin(async_stream::stream! {
            let error = match open.await {
                Ok(mut stream) => loop {
                    match stream.next().await {
                        Some(Ok(value)) => yield Ok(item(value)),
                        Some(Err(error)) => break error,
                        None => break SessionError::Backend("Session observation ended".into()),
                    }
                },
                Err(error) => error,
            };
            yield Ok(SurfaceUpdate::Stopped(kind, error));
        })
    }
    let mut sources = stream::SelectAll::new();
    if view.includes(SurfaceSource::Projections) {
        let session = handle.clone();
        sources.push(source(
            SurfaceSource::Projections,
            async move { session.observe_projections().await },
            SurfaceUpdate::Projections,
        ));
    }
    if view.includes(SurfaceSource::Goal) {
        let session = handle.clone();
        sources.push(source(
            SurfaceSource::Goal,
            async move { session.observe_goal().await },
            SurfaceUpdate::Goal,
        ));
    }
    if view.includes(SurfaceSource::History) {
        let session = handle.clone();
        sources.push(source(
            SurfaceSource::History,
            async move { session.observe(view.cursor()).await },
            SurfaceUpdate::History,
        ));
    }
    if view.includes(SurfaceSource::Interactions) {
        let session = handle.clone();
        sources.push(source(
            SurfaceSource::Interactions,
            async move { session.observe_interactions().await },
            SurfaceUpdate::Interactions,
        ));
    }
    if view.includes(SurfaceSource::Model) {
        sources.push(source(
            SurfaceSource::Model,
            async move { handle.observe_models().await },
            SurfaceUpdate::Model,
        ));
    }
    Box::pin(sources)
}
