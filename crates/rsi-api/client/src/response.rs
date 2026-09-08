use crate::{
    FiniteDecoder, SseDecoder, SseEvent, decode_error, finite_response_head, invalid,
    response_content_length, response_content_type as content_type, validate_error_status,
};
use futures_util::{Stream, StreamExt};
use rsi_api_protocol::{ApiError, ApiMessage, ApiStream, ByteBudget, ByteReservation, Result};
use rsi_meta_execution::Execution;
use std::{pin::Pin, time::Duration};

/// Transport-supplied immutable chunks; byte owners may include bridge scratch admission.
pub type ResponseBytes = Pin<Box<dyn Stream<Item = Result<bytes::Bytes>> + Send + 'static>>;
fn uncertain(error: ApiError, mutation: bool) -> ApiError {
    if mutation {
        ApiError::OutcomeUnknown
    } else {
        error
    }
}
/// Decodes one finite HTTP response under already admitted body capacity.
/// The transport owns the absolute exchange deadline and I/O cancellation.
pub async fn decode_response(
    status: u16,
    headers: &http::HeaderMap,
    mut source: ResponseBytes,
    capacity: Option<ByteReservation>,
    mutation: bool,
    retained: &ByteBudget,
) -> Result<ApiMessage> {
    if !matches!(status, 200 | 422) {
        let error = common_error(status, headers, source)
            .await
            .map_err(|error| uncertain(error, mutation))?;
        if matches!(error, ApiError::Backend(_)) {
            return Err(uncertain(error, mutation));
        }
        return Err(error);
    }
    let result = async {
        let capacity = capacity.ok_or_else(invalid)?;
        let head = finite_response_head(status, headers)?;
        let mut decoder = FiniteDecoder::new(head.encoding, capacity, head.length)?;
        while let Some(chunk) = source.next().await {
            let chunk = chunk?;
            decoder.push(&chunk)?;
        }
        decoder.finish_into(retained)
    }
    .await
    .map_err(|error| uncertain(error, mutation))?;
    if status == 422 {
        return Err(ApiError::Domain(result.json));
    }
    Ok(result)
}
async fn common_error(
    status: u16,
    headers: &http::HeaderMap,
    mut source: ResponseBytes,
) -> Result<ApiError> {
    content_type(headers, "application/json")?;
    let length = response_content_length(headers)?;
    if length.is_some_and(|length| length > 128) {
        return Err(invalid());
    }
    let mut buffer = [0; 128];
    let mut used = 0;
    while let Some(chunk) = source.next().await {
        let chunk = chunk?;
        if chunk.len() > buffer.len() - used {
            return Err(invalid());
        }
        buffer[used..used + chunk.len()].copy_from_slice(&chunk);
        used += chunk.len();
    }
    if length.is_some_and(|length| length != used) {
        return Err(invalid());
    }
    let error = decode_error(&buffer[..used])?;
    validate_error_status(status, &error)?;
    Ok(error)
}

/// Decodes SSE until explicit end and EOF, bounding the post-end EOF wait.
/// The source and its I/O owner drop when the returned stream is cancelled.
pub fn decode_event_stream(
    mut source: ResponseBytes,
    budget: ByteBudget,
    retained: ByteBudget,
    maximum: usize,
    execution: Execution,
) -> ApiStream {
    Box::pin(async_stream::try_stream! {
        let mut decoder = SseDecoder::new(budget, retained, maximum)?;
        let mut terminal = None;
        let mut ending = None;
        loop {
            let chunk = if let Some(deadline) = &ending {
                let deadline: &rsi_meta_execution::Deadline = deadline;
                deadline.timeout(source.next()).await.map_err(|_| invalid())?
            } else { source.next().await };
            let Some(chunk) = chunk else { break; };
            let chunk = chunk?;
            let mut bytes = chunk.as_ref();
            while !bytes.is_empty() {
                let (consumed, event) = decoder.push(bytes)?;
                bytes = &bytes[consumed..];
                match event {
                    Some(SseEvent::Item(message)) => yield message,
                    Some(SseEvent::End(error)) => { terminal = error; ending = Some(execution.deadline_after(Duration::from_secs(5))); }
                    None => {},
                }
            }
        }
        decoder.finish()?;
        if let Some(error) = terminal { Err(error)?; }
    })
}
