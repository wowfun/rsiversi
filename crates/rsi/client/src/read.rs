use rsi_api_protocol::ApiError;
use rsi_meta_execution::Execution;
use rsi_session_protocol::{Result, SessionError};
use std::{future::Future, time::Duration};

/// Retries a read after transient capacity rejection, within its caller's work slot.
///
/// The caller must supply only a read operation, retain its request parameters,
/// and own cancellation. At most five attempts use 50/100/200/400 ms backoff;
/// each operation retains its own I/O deadline. No task or queue is created.
/// Other failures, including an unknown outcome, return without another attempt.
pub async fn read_with_capacity_retry<T, F: Future<Output = Result<T>>>(
    execution: &Execution,
    mut read: impl FnMut() -> F,
) -> Result<T> {
    let mut delay = Duration::from_millis(50);
    for attempt in 0..5 {
        match read().await {
            Err(SessionError::Capacity | SessionError::Api(ApiError::Capacity)) if attempt < 4 => {
                execution.sleep(delay).await;
                delay *= 2;
            }
            result => return result,
        }
    }
    unreachable!("the final attempt returns its result")
}
