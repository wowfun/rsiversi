use crate::{
    client::{self, Handle},
    wire::{self, Failure, HandleReply, HandleRequest, Operation, Target},
};
use futures_util::StreamExt as _;
use rsi_agent_session_protocol::{AgentControlRecord, SessionFact};
use rsi_agent_turn_protocol::{ObservationCursor, SessionObservation, TurnError};
use rsi_api_protocol::{ApiError, ApiMessage, ApiOutput, ApiStream};
use rsi_session_protocol::SessionObservationStream;
use rsi_session_protocol::{InteractionStream, SessionError};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use std::{collections::BTreeSet, sync::Arc};

pub(super) async fn open<I: Serialize + Sync>(
    handle: &Handle,
    operation: Operation,
    input: I,
) -> rsi_session_protocol::Result<(Target, ApiStream)> {
    let spec = operation.spec();
    let request = HandleRequest {
        target: handle.target(),
        input,
    };
    let input = handle
        .state
        .api
        .input_budget(spec.class)
        .encode(&request, spec.maximum_request_bytes)
        .map_err(SessionError::Api)?;
    match handle.state.api.call(&spec, input).await {
        Ok(ApiOutput::Stream(stream)) => Ok((request.target, stream)),
        Ok(ApiOutput::Reply(_)) => Err(client::malformed(operation)),
        Err(error) => Err(stream_error(operation, error)),
    }
}
pub(super) fn stream_error(operation: Operation, error: ApiError) -> SessionError {
    match error {
        ApiError::Domain(body) => serde_json::from_slice::<Failure>(body.as_bytes())
            .map_err(|_| client::malformed(operation))
            .and_then(|failure| client::failure(operation, failure))
            .unwrap_or_else(|error| error),
        error => SessionError::Api(error),
    }
}
fn decode<T: DeserializeOwned>(
    target: &Target,
    operation: Operation,
    message: &ApiMessage,
) -> rsi_session_protocol::Result<T> {
    if message.binary.is_some() {
        return Err(client::malformed(operation));
    }
    let reply: HandleReply<T> = serde_json::from_slice(message.json.as_bytes())
        .map_err(|_| client::malformed(operation))?;
    if &reply.target != target {
        return Err(client::malformed(operation));
    }
    Ok(reply.body)
}
fn retention_error(error: &TurnError) -> SessionError {
    match error {
        TurnError::Capacity => SessionError::Capacity,
        _ => SessionError::Backend("Session observation retention failed".into()),
    }
}

pub(super) async fn scheduled_tasks(
    state: &client::State,
) -> rsi_session_protocol::Result<rsi_session_protocol::ScheduleCatalogStream> {
    use rsi_session_protocol::ScheduleCatalogUpdate;
    let operation = Operation::ScheduledTasksObserve;
    let spec = operation.spec();
    let input = state
        .api
        .input_budget(spec.class)
        .encode(&(), spec.maximum_request_bytes)
        .map_err(SessionError::Api)?;
    let ApiOutput::Stream(mut stream) = state
        .api
        .call(&spec, input)
        .await
        .map_err(|reason| stream_error(operation, reason))?
    else {
        return Err(client::malformed(operation));
    };
    Ok(Box::pin(async_stream::try_stream! {
        let mut available = 0u32;
        let mut previous = None;
        while let Some(message) = stream.next().await {
            let message = message.map_err(|reason| stream_error(operation, reason))?;
            if message.binary.is_some() { Err(client::malformed(operation))?; }
            let update: ScheduleCatalogUpdate = serde_json::from_slice(message.json.as_bytes())
                .map_err(|_| client::malformed(operation))?;
            match &update {
                ScheduleCatalogUpdate::Page { index, sessions } => {
                    if *index > available || previous.is_some_and(|old| old >= *index)
                        || sessions.is_empty() || sessions.iter().any(|group| group.reminders.is_empty())
                        || rsi_session_protocol::ScheduledSession::validate_page(sessions, None, None).is_err() {
                        Err(client::malformed(operation))?;
                    }
                    previous = Some(*index);
                    if *index == available { available = available.checked_add(1).ok_or_else(|| client::malformed(operation))?; }
                }
                ScheduleCatalogUpdate::Complete { pages } => {
                    if *pages > available || previous.is_some_and(|index| index >= *pages) { Err(client::malformed(operation))?; }
                    available = *pages;
                    previous = None;
                }
            }
            drop(message);
            yield update;
        }
        Err(client::malformed(operation))?;
    }))
}

pub(super) async fn terminal_output(
    handle: &Handle,
    request: &rsi_session_protocol::terminal::Request,
) -> rsi_session_protocol::Result<rsi_session_protocol::terminal::Reply> {
    let operation = Operation::TerminalOutput;
    let (target, mut source) = open(handle, operation, request).await?;
    let message = source
        .next()
        .await
        .ok_or_else(|| client::malformed(operation))?
        .map_err(|error| stream_error(operation, error))?;
    decode(&target, operation, &message)
}

pub(super) async fn goal(
    handle: &Handle,
) -> rsi_session_protocol::Result<rsi_session_protocol::GoalStream> {
    let operation = Operation::GoalObserve;
    let (target, mut source) = open(handle, operation, ()).await?;
    Ok(Box::pin(async_stream::try_stream! {
        while let Some(message) = source.next().await {
            let message = message.map_err(|error| stream_error(operation, error))?;
            let state: rsi_goal::GoalLiveState = decode(&target, operation, &message)?;
            state.validate().map_err(|_| client::malformed(operation))?;
            drop(message);
            yield state;
        }
    }))
}

pub(super) async fn draft(
    handle: &Handle,
) -> rsi_session_protocol::Result<rsi_session_protocol::DraftRevisionStream> {
    let operation = Operation::DraftObserve;
    let (target, mut source) = open(handle, operation, ()).await?;
    Ok(Box::pin(async_stream::try_stream! {
        let mut previous = None;
        while let Some(message) = source.next().await {
            let message = message.map_err(|error| stream_error(operation, error))?;
            let revision: Option<u64> = decode(&target, operation, &message)?;
            if let Some(next) = revision {
                if previous.is_some_and(|old| next <= old) { Err(client::malformed(operation))?; }
                previous = Some(next);
            }
            drop(message);
            yield revision;
            if revision.is_none() { return; }
        }
        Err(client::malformed(operation))?;
    }))
}

pub(super) async fn projections(
    handle: &Handle,
) -> rsi_session_protocol::Result<rsi_session_protocol::ProjectionStream> {
    let operation = Operation::Projections;
    let (target, mut source) = open(handle, operation, ()).await?;
    let state = handle.state.clone();
    Ok(Box::pin(async_stream::try_stream! {
        let mut cursor = None;
        while let Some(message) = source.next().await {
            let message = message.map_err(|error| stream_error(operation, error))?;
            let reservation = state.projections.reserve_decode()?;
            let snapshot: rsi_agent_session_protocol::SessionProjectionSnapshot = decode(&target, operation, &message)?;
            let retained = retain_projection(reservation, &target, &mut cursor, snapshot)?;
            drop(message);
            yield retained;
        }
    }))
}
pub(super) fn retain_projection(
    reservation: rsi_session_protocol::ProjectionCollection,
    target: &Target,
    cursor: &mut Option<rsi_agent_session_protocol::ProjectionCursor>,
    snapshot: rsi_agent_session_protocol::SessionProjectionSnapshot,
) -> rsi_session_protocol::Result<rsi_session_protocol::ProjectionSnapshot> {
    if snapshot.session_id() != &target.session_id
        || snapshot.header_sha256() != target.header_key
        || cursor.is_some_and(|previous| !snapshot.cursor().can_follow(previous))
    {
        return Err(client::malformed(Operation::Projections));
    }
    *cursor = Some(snapshot.cursor());
    reservation.retain(snapshot)
}

pub(super) async fn observe(
    handle: &Handle,
    cursor: ObservationCursor,
) -> rsi_session_protocol::Result<SessionObservationStream> {
    let (target, mut source) = open(handle, Operation::Observe, cursor).await?;
    let state = handle.state.clone();
    let mut history = History::new(cursor);
    Ok(Box::pin(async_stream::try_stream! {
        while let Some(message) = source.next().await {
            let message = message.map_err(|error| stream_error(Operation::Observe, error))?;
            let update: wire::Observation<AgentControlRecord, SessionFact> = decode(&target, Operation::Observe, &message)?;
            let retained = history.retain(&state, update)?;
            drop(message);
            yield retained;
        }
    }))
}
pub(super) struct History {
    observed: ObservationCursor,
    durable: ObservationCursor,
}
impl History {
    pub(super) fn new(cursor: ObservationCursor) -> Self {
        Self {
            observed: cursor,
            durable: cursor,
        }
    }
    pub(super) fn retain(
        &mut self,
        state: &client::State,
        update: wire::Observation<AgentControlRecord, SessionFact>,
    ) -> rsi_session_protocol::Result<SessionObservation> {
        Ok(match update {
            wire::Observation::Control {
                record,
                fact_cursor,
                durable_control_seq,
            } => {
                advance(
                    &mut self.observed.control_seq,
                    &mut self.durable.control_seq,
                    record.seq(),
                    durable_control_seq,
                )?;
                let record = state
                    .observations
                    .retain_controls(vec![Arc::new(record)])
                    .map_err(|error| retention_error(&error))?
                    .pop()
                    .expect("one admitted control");
                SessionObservation::Control {
                    record,
                    fact_cursor,
                    durable_control_seq,
                }
            }
            wire::Observation::Fact {
                fact,
                durable_fact_seq,
            } => {
                advance(
                    &mut self.observed.fact_seq,
                    &mut self.durable.fact_seq,
                    fact.seq(),
                    durable_fact_seq,
                )?;
                let fact = state
                    .observations
                    .retain_fact(Arc::new(fact))
                    .map_err(|error| retention_error(&error))?;
                SessionObservation::Fact {
                    fact,
                    durable_fact_seq,
                }
            }
        })
    }
}
fn advance(
    observed: &mut u64,
    durable: &mut u64,
    sequence: u64,
    watermark: u64,
) -> rsi_session_protocol::Result<()> {
    if observed.checked_add(1) != Some(sequence) || watermark < sequence || watermark < *durable {
        return Err(client::malformed(Operation::Observe));
    }
    *observed = sequence;
    *durable = watermark;
    Ok(())
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Interactions {
    approvals: Vec<rsi_approval_protocol::ApprovalRequest>,
    questions: Vec<rsi_user_questions_protocol::QuestionRequest>,
}
impl Interactions {
    pub(super) async fn retain(
        self,
        handle: &Handle,
        verified: &mut BTreeSet<String>,
    ) -> rsi_session_protocol::Result<rsi_session_protocol::InteractionSnapshot> {
        client::validate_questions(&self.questions, &handle.session_id)?;
        let retained = handle
            .state
            .interactions
            .retain(self.approvals, self.questions)?;
        handle.verify_owners(retained.approvals(), verified).await?;
        Ok(retained)
    }
}
pub(super) async fn interactions(
    handle: &Handle,
) -> rsi_session_protocol::Result<InteractionStream> {
    let handle = handle.frozen();
    let (target, mut source) = open(&handle, Operation::Interactions, ()).await?;
    let mut verified = BTreeSet::from([handle.session_id.to_string()]);
    Ok(Box::pin(async_stream::try_stream! {
        while let Some(message) = source.next().await {
            let message = message.map_err(|error| stream_error(Operation::Interactions, error))?;
            let body: Interactions = decode(&target, Operation::Interactions, &message)?;
            drop(message);
            let retained = body.retain(&handle, &mut verified).await?;
            yield retained;
        }
    }))
}

pub(super) async fn export(
    handle: &Handle,
    options: rsi_session_protocol::export::ExportOptions,
) -> rsi_session_protocol::Result<rsi_session_protocol::export::ExportStream> {
    use rsi_session_protocol::export::{ExportEvent, ExportVerifier};
    options.validate()?;
    let operation = Operation::Export;
    let (target, mut source) = open(handle, operation, &options).await?;
    Ok(Box::pin(async_stream::try_stream! {
        let mut verifier = ExportVerifier::default();
        while let Some(message) = source.next().await {
            let message = message.map_err(|error| stream_error(operation, error))?;
            let event: ExportEvent = decode(&target, operation, &message)?;
            verifier.accept(&event, Some((&target.session_id, &target.header_key, &options)))?;
            drop(message);
            yield event;
        }
        verifier.finish()?;
    }))
}

pub(super) async fn models(
    handle: &Handle,
) -> rsi_session_protocol::Result<rsi_session_protocol::ModelLiveStream> {
    let operation = Operation::ModelObserve;
    let (target, mut source) = open(handle, operation, ()).await?;
    let state = handle.state.clone();
    Ok(Box::pin(async_stream::try_stream! {
        let mut model = Models::default();
        while let Some(message) = source.next().await {
            let message = message.map_err(|error| stream_error(operation, error))?;
            let page: rsi_agent_turn_protocol::ModelLivePage = decode(&target, operation, &message)?;
            if let Some(snapshot) = model.page(&state, &target, page)? {
                drop(message);
                yield snapshot;
            }
        }
        model.finish()?;
    }))
}

#[derive(Default)]
pub(super) struct Models {
    decoder: rsi_agent_turn_protocol::ModelLiveDecoder,
    reservation: Option<rsi_agent_turn_protocol::ModelObservationReservation>,
    last: Option<(rsi_agent_session_protocol::EffectId, u64)>,
    // Shared prefix blocks remain charged while decoding the next revision.
    retained_prefix: Option<rsi_agent_turn_protocol::ObservedModel>,
}
impl Models {
    pub(super) fn page(
        &mut self,
        state: &client::State,
        target: &Target,
        page: rsi_agent_turn_protocol::ModelLivePage,
    ) -> rsi_session_protocol::Result<Option<rsi_agent_turn_protocol::ObservedModel>> {
        if page.session_id != target.session_id {
            Err(client::malformed(Operation::ModelObserve))?;
        }
        if page.index == 0 {
            if self.reservation.is_some() {
                Err(client::malformed(Operation::ModelObserve))?;
            }
            self.reservation = Some(
                state
                    .observations
                    .reserve_model_decode(page.encoded_bytes)
                    .map_err(|error| retention_error(&error))?,
            );
        }
        if let Some(snapshot) = self
            .decoder
            .push(page)
            .map_err(|_| client::malformed(Operation::ModelObserve))?
        {
            if self.last.as_ref().is_some_and(|(effect, revision)| {
                effect == &snapshot.effect_id && *revision >= snapshot.revision
            }) {
                Err(client::malformed(Operation::ModelObserve))?;
            }
            self.last = Some((snapshot.effect_id.clone(), snapshot.revision));
            let snapshot = self
                .reservation
                .take()
                .ok_or_else(|| client::malformed(Operation::ModelObserve))?
                .retain(snapshot)
                .map_err(|error| retention_error(&error))?;
            drop(self.retained_prefix.replace(snapshot.clone()));
            return Ok(Some(snapshot));
        }
        Ok(None)
    }
    pub(super) fn finish(&self) -> rsi_session_protocol::Result<()> {
        self.decoder
            .finish()
            .map_err(|_| client::malformed(Operation::ModelObserve))
    }
}
