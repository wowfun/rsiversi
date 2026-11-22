use crate::{
    client::{self, Handle},
    client_stream, server,
    wire::{self, HandleRequest, Operation, Target},
};
use futures_util::StreamExt;
use rsi_agent_turn_protocol::SessionObservation;
use rsi_api_protocol::{ApiError, ApiMessage, ApiOutput, ByteBudget, RetainedBytes};
use rsi_session_protocol::{
    SessionService, SurfaceSource, SurfaceStream, SurfaceUpdate, SurfaceView,
};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use std::{collections::BTreeSet, sync::Arc};

// A borrowed payload lets the decoder reserve the source's retention before materializing it.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Frame<'a> {
    target: Target,
    source: SurfaceSource,
    #[serde(default)]
    stopped: bool,
    #[serde(borrow)]
    body: &'a serde_json::value::RawValue,
}
#[derive(Serialize)]
struct Outgoing<'a, T> {
    target: &'a Target,
    source: SurfaceSource,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    stopped: bool,
    body: T,
}
fn operation(source: SurfaceSource) -> Operation {
    match source {
        SurfaceSource::History => Operation::Observe,
        SurfaceSource::Model => Operation::ModelObserve,
        SurfaceSource::Interactions => Operation::Interactions,
        SurfaceSource::Projections => Operation::Projections,
        SurfaceSource::Goal => Operation::GoalObserve,
        SurfaceSource::BackgroundJobs => Operation::SurfaceObserve,
    }
}

#[derive(Serialize, Deserialize)]
#[serde(
    tag = "kind",
    content = "reason",
    rename_all = "snake_case",
    deny_unknown_fields
)]
enum SourceFailure {
    Domain(wire::Failure),
    Backend,
}

fn source_failure(
    error: rsi_session_protocol::SessionError,
) -> rsi_api_protocol::Result<SourceFailure> {
    match wire::failure(error) {
        Ok(reason) => Ok(SourceFailure::Domain(reason)),
        Err(ApiError::Capacity) => Ok(SourceFailure::Domain(wire::Failure::Capacity {})),
        Err(error @ (ApiError::Unauthorized | ApiError::ShuttingDown)) => Err(error),
        Err(_) => Ok(SourceFailure::Backend),
    }
}

fn frame<T: Serialize>(
    budget: &ByteBudget,
    target: &Target,
    source: SurfaceSource,
    stopped: bool,
    body: T,
) -> rsi_api_protocol::Result<ApiMessage> {
    let json = budget.encode(
        &Outgoing {
            target,
            source,
            stopped,
            body,
        },
        wire::surface_frame_limit(source),
    )?;
    Ok(ApiMessage { json, binary: None })
}
pub(super) async fn serve(
    service: &dyn SessionService,
    input: RetainedBytes,
    budget: ByteBudget,
) -> rsi_api_protocol::Result<ApiOutput> {
    let request: HandleRequest<SurfaceView> = serde_json::from_slice(input.as_bytes())
        .map_err(|_| ApiError::Invalid("invalid Session surface observation request".into()))?;
    let result = async {
        server::handle(service, &request.target)
            .await?
            .observe_surface(request.input)
            .await
    }
    .await;
    let mut source = match wire::domain(result)? {
        Ok(source) => source,
        Err(error) => return Err(ApiError::Domain(budget.encode(&error, 8192)?)),
    };
    let target = request.target;
    Ok(ApiOutput::Stream(Box::pin(async_stream::try_stream! {
        let mut model = rsi_agent_turn_protocol::ModelLiveEncoder::default();
        while let Some(update) = source.next().await {
            let update = match wire::domain(update)? {
                Ok(update) => update,
                Err(error) => Err(ApiError::Domain(budget.encode(&error, 8192)?))?,
            };
            let message = match update {
                SurfaceUpdate::History(record) => frame(
                    &budget,
                    &target,
                    SurfaceSource::History,
                    false,
                    history_body(&record),
                )?,
                SurfaceUpdate::Interactions(value) => {
                    frame(&budget, &target, SurfaceSource::Interactions, false, value)?
                }
                SurfaceUpdate::Projections(value) => {
                    if value.snapshot().session_id() != &target.session_id
                        || value.snapshot().header_sha256() != target.header_key
                    {
                        Err(ApiError::Backend(
                            "Session projection binding changed".into(),
                        ))?;
                    }
                    frame(&budget, &target, SurfaceSource::Projections, false, value)?
                }
                SurfaceUpdate::BackgroundJobs(rows) => frame(&budget, &target, SurfaceSource::BackgroundJobs, false, rows)?,
                SurfaceUpdate::Goal(value) => {
                    frame(&budget, &target, SurfaceSource::Goal, false, value)?
                }
                SurfaceUpdate::Model(snapshot) => {
                    for page in model.pages(&snapshot) {
                        yield frame(&budget, &target, SurfaceSource::Model, false, page)?;
                    }
                    continue;
                }
                SurfaceUpdate::Stopped(kind, reason) => {
                    let failure = source_failure(reason)?;
                    frame(&budget, &target, kind, true, failure)?
                }
            };
            yield message;
        }
    })))
}

pub(super) fn history_body(
    record: &SessionObservation,
) -> wire::Observation<
    &rsi_agent_session_protocol::AgentControlRecord,
    &rsi_agent_session_protocol::SessionFact,
> {
    match record {
        SessionObservation::Control {
            record,
            fact_cursor,
            durable_control_seq,
        } => wire::Observation::Control {
            record: &**record,
            fact_cursor: *fact_cursor,
            durable_control_seq: *durable_control_seq,
        },
        SessionObservation::Fact {
            fact,
            durable_fact_seq,
        } => wire::Observation::Fact {
            fact: &**fact,
            durable_fact_seq: *durable_fact_seq,
        },
    }
}

fn body<T: DeserializeOwned>(frame: &Frame<'_>) -> rsi_session_protocol::Result<T> {
    serde_json::from_str(frame.body.get()).map_err(|_| client::malformed(Operation::SurfaceObserve))
}
pub(super) async fn open(
    handle: Arc<Handle>,
    view: SurfaceView,
) -> rsi_session_protocol::Result<SurfaceStream> {
    let operation = Operation::SurfaceObserve;
    let handle = handle.frozen();
    let (target, mut stream) = client_stream::open(&handle, operation, view).await?;
    let mut owners = BTreeSet::from([handle.session_id.to_string()]);
    let mut history = client_stream::History::new(view.cursor());
    let mut model = client_stream::Models::default();
    let mut projection_cursor = None;
    let mut remaining = Some(view);
    Ok(Box::pin(async_stream::try_stream! {
        let state = &handle.state;
        while let Some(message) = stream.next().await {
            let message = message.map_err(|e| client_stream::stream_error(operation, e))?;
            let frame: Frame<'_> = serde_json::from_slice(message.json.as_bytes())
                .map_err(|_| client::malformed(operation))?;
            if message.binary.is_some()
                || frame.target != target
                || message.json.len() > wire::surface_frame_limit(frame.source)
                || !remaining.is_some_and(|sources| sources.includes(frame.source))
            {
                Err(client::malformed(operation))?;
            }
            let update = if frame.stopped {
                remaining = remaining.and_then(|sources| sources.without(frame.source));
                let error = match body(&frame)? {
                    SourceFailure::Domain(failure) => {
                        client::failure(self::operation(frame.source), failure)?
                    }
                    SourceFailure::Backend => {
                        rsi_session_protocol::SessionError::Backend("Session observation failed".into())
                    }
                };
                let error = if frame.source == SurfaceSource::Model {
                    std::mem::take(&mut model).finish().err().unwrap_or(error)
                } else {
                    error
                };
                SurfaceUpdate::Stopped(frame.source, error)
            } else {
                match frame.source {
                    SurfaceSource::History => {
                        SurfaceUpdate::History(history.retain(state, body(&frame)?)?)
                    }
                    SurfaceSource::Model => {
                        let Some(snapshot) = model.page(state, &target, body(&frame)?)? else {
                            continue;
                        };
                        SurfaceUpdate::Model(snapshot)
                    }
                    SurfaceSource::Interactions => {
                        let values: client_stream::Interactions = body(&frame)?;
                        drop(message);
                        yield SurfaceUpdate::Interactions(values.retain(&handle, &mut owners).await?);
                        continue;
                    }
                    SurfaceSource::Projections => {
                        let admission = state.projections.reserve_decode()?;
                        SurfaceUpdate::Projections(client_stream::retain_projection(
                            admission,
                            &target,
                            &mut projection_cursor,
                            body(&frame)?,
                        )?)
                    }
                    SurfaceSource::BackgroundJobs => {
                        let rows: Vec<rsi_session_protocol::JobRosterRow> = body(&frame)?;
                        if rows.len() > rsi_session_protocol::MAXIMUM_BACKGROUND_JOBS || rows.iter().any(|r| r.validate().is_err()) {
                            Err(client::malformed(operation))?;
                        }
                        SurfaceUpdate::BackgroundJobs(rows)
                    }
                    SurfaceSource::Goal => {
                        let value: rsi_goal::GoalLiveState = body(&frame)?;
                        value.validate().map_err(|_| client::malformed(operation))?;
                        SurfaceUpdate::Goal(value)
                    }
                }
            };
            drop(message);
            yield update;
        }
        model.finish()?;
    }))
}
