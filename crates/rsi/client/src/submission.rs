use rsi_agent_turn_protocol::MessageReceipt;
use rsi_session_protocol::{Result, SessionError, SessionHandle, SubmitInput};

/// Reconciles one message identity before considering a bounded identical retry.
pub async fn submit_with_reconciliation(
    handle: &dyn SessionHandle,
    request: SubmitInput,
) -> Result<MessageReceipt> {
    let message_id = request.message_id.clone();
    let unknown = match handle.submit(request.clone()).await {
        Err(error @ SessionError::MessageOutcomeUnknown { .. }) => error,
        result => return result,
    };
    match handle.message_status(&message_id).await {
        Ok(receipt) => return Ok(receipt),
        Err(SessionError::NotFound(_)) => {}
        Err(_) => return Err(unknown),
    }
    match handle.submit(request).await {
        Err(error @ SessionError::MessageOutcomeUnknown { .. }) => {
            handle.message_status(&message_id).await.or(Err(error))
        }
        result => result,
    }
}
