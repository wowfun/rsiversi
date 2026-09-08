use rsi_api_protocol::{ApiOutput, ApiStream, OperationSpec, Result};
use tokio::sync::oneshot;
use tokio_util::sync::CancellationToken;

pub(crate) async fn forward(
    source: ApiStream,
    result: oneshot::Sender<Result<ApiOutput>>,
    spec: &OperationSpec,
    retiring: &CancellationToken,
    revoked: &CancellationToken,
) {
    let (stream, driver) = rsi_api_protocol::supervised_stream(
        source,
        spec.maximum_response_bytes,
        retiring.clone(),
        revoked.clone(),
    );
    if result.send(Ok(ApiOutput::Stream(stream))).is_ok() {
        driver.await;
    }
}
