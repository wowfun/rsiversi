use crate::{
    server::handle,
    wire::{self, HandleReply, HandleRequest, Operation},
};
use async_trait::async_trait;
use futures_util::StreamExt as _;
use rsi_agent_turn_protocol::SessionObservation;
use rsi_api_protocol::{
    ApiContext, ApiError, ApiHandler, ApiMessage, ApiOutput, ApiResponseCapacity, RetainedBytes,
};
use rsi_session_protocol::SessionService;
use std::sync::Arc;

#[derive(Debug)]
pub(super) struct Handler {
    pub service: Arc<dyn SessionService>,
    pub operation: Operation,
}
#[async_trait]
impl ApiHandler for Handler {
    async fn invoke(
        &self,
        _: ApiContext,
        input: RetainedBytes,
        output: ApiResponseCapacity,
    ) -> rsi_api_protocol::Result<ApiOutput> {
        let ApiResponseCapacity::Subscription { budget, maximum } = output else {
            return Err(ApiError::Backend(
                "Session subscription admission required".into(),
            ));
        };
        match self.operation {
            Operation::Projections => {
                projections(self.service.as_ref(), input, budget, maximum).await
            }
            Operation::Observe => {
                let request: wire::Observe = serde_json::from_slice(input.as_bytes())
                    .map_err(|_| ApiError::Invalid("invalid Session observation request".into()))?;
                let result = async {
                    handle(self.service.as_ref(), &request.target)
                        .await?
                        .observe(request.input)
                        .await
                }
                .await;
                let mut source = match wire::domain(result)? {
                    Ok(source) => source,
                    Err(failure) => {
                        return Err(ApiError::Domain(budget.encode(&failure, maximum)?));
                    }
                };
                Ok(ApiOutput::Stream(Box::pin(async_stream::try_stream! {
                    while let Some(update) = source.next().await {
                        let update = update.map_err(|error| turn_error(&error))?;
                        let reservation = budget.reserve(maximum)?;
                        let body = match &update {
                            SessionObservation::Control { record, durable_control_seq } => wire::Observation::Control { record: &**record, durable_control_seq: *durable_control_seq },
                            SessionObservation::Fact { fact, durable_fact_seq } => wire::Observation::Fact { fact: &**fact, durable_fact_seq: *durable_fact_seq },
                        };
                        let json = reservation.encode(&HandleReply { target: request.target.clone(), body }).map_err(|_| ApiError::Backend("Session observation encoding failed".into()))?;
                        yield ApiMessage { json, binary: None };
                    }
                })))
            }
            Operation::Interactions => {
                let request: HandleRequest<()> = serde_json::from_slice(input.as_bytes())
                    .map_err(|_| ApiError::Invalid("invalid Session interaction request".into()))?;
                let result = async {
                    handle(self.service.as_ref(), &request.target)
                        .await?
                        .observe_interactions()
                        .await
                }
                .await;
                let mut source = match wire::domain(result)? {
                    Ok(source) => source,
                    Err(failure) => {
                        return Err(ApiError::Domain(budget.encode(&failure, maximum)?));
                    }
                };
                Ok(ApiOutput::Stream(Box::pin(async_stream::try_stream! {
                    while let Some(snapshot) = source.next().await {
                        let snapshot = match wire::domain(snapshot)? { Ok(snapshot) => snapshot, Err(failure) => Err(ApiError::Domain(budget.encode(&failure, maximum)?))? };
                        let reservation = budget.reserve(maximum)?;
                        let json = reservation.encode(&HandleReply { target: request.target.clone(), body: snapshot }).map_err(|_| ApiError::Backend("Session interaction encoding failed".into()))?;
                        yield ApiMessage { json, binary: None };
                    }
                })))
            }
            _ => Err(ApiError::Backend(
                "invalid Session subscription metadata".into(),
            )),
        }
    }
}
async fn projections(
    service: &dyn SessionService,
    input: RetainedBytes,
    budget: rsi_api_protocol::ByteBudget,
    maximum: usize,
) -> rsi_api_protocol::Result<ApiOutput> {
    let request: HandleRequest<()> = serde_json::from_slice(input.as_bytes())
        .map_err(|_| ApiError::Invalid("invalid Session projection request".into()))?;
    let result = async {
        handle(service, &request.target)
            .await?
            .observe_projections()
            .await
    }
    .await;
    let mut source = match wire::domain(result)? {
        Ok(source) => source,
        Err(failure) => return Err(ApiError::Domain(budget.encode(&failure, maximum)?)),
    };
    Ok(ApiOutput::Stream(Box::pin(async_stream::try_stream! {
        while let Some(snapshot) = source.next().await {
            let snapshot = match wire::domain(snapshot)? {
                Ok(snapshot) => snapshot,
                Err(failure) => Err(ApiError::Domain(budget.encode(&failure, maximum)?))?,
            };
            if snapshot.snapshot().session_id() != &request.target.session_id
                || snapshot.snapshot().header_sha256() != request.target.header_key {
                Err(ApiError::Backend("Session projection binding changed".into()))?;
            }
            let json = budget.encode(&HandleReply { target: request.target.clone(), body: snapshot }, maximum)?;
            yield ApiMessage { json, binary: None };
        }
    })))
}
fn turn_error(error: &rsi_agent_turn_protocol::TurnError) -> ApiError {
    match error {
        rsi_agent_turn_protocol::TurnError::Capacity
        | rsi_agent_turn_protocol::TurnError::ObserverCapacity => ApiError::Capacity,
        rsi_agent_turn_protocol::TurnError::ShuttingDown => ApiError::ShuttingDown,
        _ => ApiError::Backend("Session observation failed".into()),
    }
}
