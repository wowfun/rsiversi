use futures_util::StreamExt as _;
pub(crate) use rsi_api_client::{
    response_content_type as content_type, response_identity as identity,
};
use rsi_api_protocol::{ApiError, ApiMessage, ApiStream, ByteBudget, ByteReservation, Result};
use rsi_meta::Execution;

pub(crate) fn invalid() -> ApiError {
    ApiError::Invalid("invalid API peer response".into())
}
pub(crate) fn uncertain(error: ApiError, mutation: bool) -> ApiError {
    if mutation {
        ApiError::OutcomeUnknown
    } else {
        error
    }
}
fn body(response: reqwest::Response) -> rsi_api_client::ResponseBytes {
    Box::pin(
        response
            .bytes_stream()
            .map(|chunk| chunk.map_err(|_| invalid())),
    )
}
pub(crate) async fn finite(
    response: reqwest::Response,
    capacity: Option<ByteReservation>,
    mutation: bool,
    retained: &ByteBudget,
) -> Result<ApiMessage> {
    let status = response.status().as_u16();
    let headers = response.headers().clone();
    rsi_api_client::decode_response(
        status,
        &headers,
        body(response),
        capacity,
        mutation,
        retained,
    )
    .await
}
pub(crate) fn stream(
    response: reqwest::Response,
    budget: ByteBudget,
    retained: ByteBudget,
    maximum: usize,
    execution: Execution,
) -> ApiStream {
    rsi_api_client::decode_event_stream(body(response), budget, retained, maximum, execution)
}
