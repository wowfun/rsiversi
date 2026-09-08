use crate::{
    client::{self, Handle},
    wire::{self, Failure, HandleReply, HandleRequest, Operation},
};
use futures_util::StreamExt as _;
use rsi_agent_session_protocol::{AgentControlRecord, SessionFact};
use rsi_agent_turn_protocol::{
    ObservationCursor, SessionObservation, SessionObservationStream, TurnError,
};
use rsi_api_protocol::{ApiError, ApiMessage, ApiOutput, ApiStream};
use rsi_session_protocol::{InteractionStream, SessionError};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use std::{collections::BTreeSet, sync::Arc};

async fn open<I: Serialize + Sync>(
    handle: &Handle,
    operation: Operation,
    input: I,
) -> rsi_session_protocol::Result<ApiStream> {
    let spec = operation.spec();
    let input = handle
        .state
        .api
        .input_budget(spec.class)
        .encode(
            &HandleRequest {
                target: handle.target(),
                input,
            },
            spec.maximum_request_bytes,
        )
        .map_err(SessionError::Api)?;
    match handle.state.api.call(&spec, input).await {
        Ok(ApiOutput::Stream(stream)) => Ok(stream),
        Ok(ApiOutput::Reply(_)) => Err(client::malformed(operation)),
        Err(error) => Err(stream_error(operation, error)),
    }
}
fn stream_error(operation: Operation, error: ApiError) -> SessionError {
    match error {
        ApiError::Domain(body) => serde_json::from_slice::<Failure>(body.as_bytes())
            .map_err(|_| client::malformed(operation))
            .and_then(|failure| client::failure(operation, failure))
            .unwrap_or_else(|error| error),
        error => SessionError::Api(error),
    }
}
fn decode<T: DeserializeOwned>(
    handle: &Handle,
    operation: Operation,
    message: &ApiMessage,
) -> rsi_session_protocol::Result<T> {
    if message.binary.is_some() {
        return Err(client::malformed(operation));
    }
    let reply: HandleReply<T> = serde_json::from_slice(message.json.as_bytes())
        .map_err(|_| client::malformed(operation))?;
    if reply.target != handle.target() {
        return Err(client::malformed(operation));
    }
    Ok(reply.body)
}
fn turn_error(error: &SessionError) -> TurnError {
    match error {
        SessionError::Capacity | SessionError::Api(ApiError::Capacity) => {
            TurnError::ObserverCapacity
        }
        SessionError::ShuttingDown | SessionError::Api(ApiError::ShuttingDown) => {
            TurnError::ShuttingDown
        }
        _ => TurnError::Invariant("invalid or failed remote Session observation".into()),
    }
}
pub(super) async fn observe(
    handle: &Handle,
    cursor: ObservationCursor,
) -> rsi_session_protocol::Result<SessionObservationStream> {
    let handle = handle.frozen();
    let mut source = open(&handle, Operation::Observe, cursor).await?;
    let mut observed = cursor;
    let mut durable = cursor;
    Ok(Box::pin(async_stream::try_stream! {
        while let Some(message) = source.next().await {
            let message = message.map_err(|error| turn_error(&stream_error(Operation::Observe, error)))?;
            let update: wire::Observation<AgentControlRecord, SessionFact> = decode(&handle, Operation::Observe, &message).map_err(|error| turn_error(&error))?;
            let retained = match update {
                wire::Observation::Control { record, durable_control_seq } => {
                    advance(&mut observed.control_seq, &mut durable.control_seq, record.seq(), durable_control_seq)?;
                    let record = handle.state.observations.retain_controls(vec![Arc::new(record)])?.pop().expect("one admitted control");
                    SessionObservation::Control { record, durable_control_seq }
                }
                wire::Observation::Fact { fact, durable_fact_seq } => {
                    advance(&mut observed.fact_seq, &mut durable.fact_seq, fact.seq(), durable_fact_seq)?;
                    let fact = handle.state.observations.retain_fact(Arc::new(fact))?;
                    SessionObservation::Fact { fact, durable_fact_seq }
                }
            };
            drop(message);
            yield retained;
        }
    }))
}
fn advance(
    observed: &mut u64,
    durable: &mut u64,
    sequence: u64,
    watermark: u64,
) -> rsi_agent_turn_protocol::Result<()> {
    if observed.checked_add(1) != Some(sequence) || watermark < sequence || watermark < *durable {
        return Err(TurnError::Invariant(
            "remote Session sequence is discontinuous or regressing".into(),
        ));
    }
    *observed = sequence;
    *durable = watermark;
    Ok(())
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Interactions {
    approvals: Vec<rsi_approval_protocol::ApprovalRequest>,
    questions: Vec<rsi_user_questions_protocol::QuestionRequest>,
}
pub(super) async fn interactions(
    handle: &Handle,
) -> rsi_session_protocol::Result<InteractionStream> {
    let handle = handle.frozen();
    let mut source = open(&handle, Operation::Interactions, ()).await?;
    let mut verified = BTreeSet::from([handle.session_id.to_string()]);
    Ok(Box::pin(async_stream::try_stream! {
        while let Some(message) = source.next().await {
            let message = message.map_err(|error| stream_error(Operation::Interactions, error))?;
            let body: Interactions = decode(&handle, Operation::Interactions, &message)?;
            client::validate_questions(&body.questions, &handle.session_id)?;
            let retained = handle.state.interactions.retain(body.approvals, body.questions)?;
            drop(message);
            handle.verify_owners(retained.approvals(), &mut verified).await?;
            yield retained;
        }
    }))
}
