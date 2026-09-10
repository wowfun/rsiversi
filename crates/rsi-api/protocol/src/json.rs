use crate::{
    ApiClient, ApiContext, ApiError, ApiHandler, ApiMessage, ApiOutput, ApiResponseCapacity,
    OperationEffect, OperationSpec, Result, RetainedBytes,
};
use async_trait::async_trait;
use serde::{Serialize, de::DeserializeOwned};
use std::{fmt, future::Future, marker::PhantomData, sync::Arc};

/// Constructs a typed finite domain handler; the outer result carries API failures.
/// Domain request DTOs own field strictness and semantic validation. Object DTOs
/// must reject unknown fields (for Serde derives, use `deny_unknown_fields`);
/// this generic adapter only enforces bounded JSON decoding and cannot impose
/// that policy on a custom `Deserialize` implementation.
pub fn json_handler<I, O, E, F, Fut>(call: F) -> Arc<dyn ApiHandler>
where
    I: DeserializeOwned + Send + 'static,
    O: Serialize + Send + 'static,
    E: Serialize + Send + 'static,
    F: Fn(ApiContext, I) -> Fut + Send + Sync + 'static,
    Fut: Future<Output = Result<std::result::Result<O, E>>> + Send + 'static,
{
    Arc::new(JsonHandler {
        call,
        types: PhantomData,
    })
}
struct JsonHandler<I, O, E, F> {
    call: F,
    types: PhantomData<fn(I) -> (O, E)>,
}
impl<I, O, E, F> fmt::Debug for JsonHandler<I, O, E, F> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("JsonHandler(..)")
    }
}
#[async_trait]
impl<I, O, E, F, Fut> ApiHandler for JsonHandler<I, O, E, F>
where
    I: DeserializeOwned + Send + 'static,
    O: Serialize + Send + 'static,
    E: Serialize + Send + 'static,
    F: Fn(ApiContext, I) -> Fut + Send + Sync + 'static,
    Fut: Future<Output = Result<std::result::Result<O, E>>> + Send + 'static,
{
    async fn invoke(
        &self,
        context: ApiContext,
        input: RetainedBytes,
        output: ApiResponseCapacity,
    ) -> Result<ApiOutput> {
        let request: I = serde_json::from_slice(input.as_bytes())
            .map_err(|_| ApiError::Invalid("invalid domain request".into()))?;
        let ApiResponseCapacity::Finite(reservation) = output else {
            return Err(ApiError::Backend(
                "finite JSON handler requires finite response admission".into(),
            ));
        };
        match (self.call)(context, request).await? {
            Ok(reply) => Ok(ApiOutput::Reply(ApiMessage {
                json: reservation.encode(&reply).map_err(encoding_failed)?,
                binary: None,
            })),
            Err(error) => Err(ApiError::Domain(
                reservation.encode(&error).map_err(encoding_failed)?,
            )),
        }
    }
}
fn encoding_failed(error: ApiError) -> ApiError {
    if error == ApiError::Capacity {
        error
    } else {
        ApiError::Backend("domain result encoding failed".into())
    }
}

/// Sends a typed finite request and preserves domain failures separately from API failures.
pub async fn call_json<I: Serialize + Sync, O: DeserializeOwned, E: DeserializeOwned>(
    client: &dyn ApiClient,
    operation: &OperationSpec,
    request: &I,
) -> Result<std::result::Result<O, E>> {
    let input = client
        .input_budget(operation.class)
        .encode(request, operation.maximum_request_bytes)?;
    let result = client.call(operation, input).await;
    let malformed = || {
        if operation.effect == OperationEffect::Mutation {
            ApiError::OutcomeUnknown
        } else {
            ApiError::Invalid("invalid domain response".into())
        }
    };
    match result {
        Ok(ApiOutput::Reply(message)) if message.binary.is_none() => {
            serde_json::from_slice(message.json.as_bytes())
                .map(Ok)
                .map_err(|_| malformed())
        }
        Ok(_) => Err(malformed()),
        Err(ApiError::Domain(bytes)) => serde_json::from_slice(bytes.as_bytes())
            .map(Err)
            .map_err(|_| malformed()),
        Err(error) => Err(error),
    }
}
