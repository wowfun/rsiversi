use crate::submit_with_reconciliation;
use async_trait::async_trait;
use futures_util::StreamExt;
use rsi_agent_session_protocol::{
    AgentControlRecordBody, MessageDiscardReason, MessageId, SessionFactBody, SessionId, TurnId,
    TurnOutcome,
};
use rsi_agent_turn_protocol::{
    CancelTarget, MessageReceipt, MessageState, ObservationCursor, ObservedFact,
    SessionObservation, TurnError,
};
use rsi_session_protocol::{SessionError, SessionHandle, SubmitInput};
use tokio_util::sync::CancellationToken;

/// One message's accepted identity, durable claim, observed Fact or final outcome.
#[derive(Clone, Debug)]
pub enum MessageEvent {
    /// The exact accepted message receipt, including an already resolved claim.
    Accepted(MessageReceipt),
    /// The durable claim identifying the only Turn this driver may cancel.
    Claimed {
        /// Session owning the message.
        session_id: SessionId,
        /// Accepted message identity.
        message_id: MessageId,
        /// Claimed Turn identity.
        turn_id: TurnId,
        /// Observation boundary at which the Turn entered.
        entered_fact_seq: u64,
    },
    /// An observed Fact after the claimed Turn's entry boundary.
    Fact {
        /// Session owning this Fact.
        session_id: SessionId,
        /// Exact delivered Fact and its retained admission.
        fact: ObservedFact,
        /// Durable watermark; this is not a delivery cursor.
        durable_seq: u64,
    },
    /// The claimed Turn's terminal outcome, delivered after its terminal Fact.
    Outcome {
        /// Session owning the Turn.
        session_id: SessionId,
        /// Exact claimed Turn.
        turn_id: TurnId,
        /// Durable terminal result.
        outcome: TurnOutcome,
        /// Durable watermark associated with the terminal Fact.
        durable_seq: u64,
    },
}

/// Failure while following one exact message; no renderer-specific error values.
#[derive(Debug, thiserror::Error)]
pub enum MessageRunError {
    /// Admission, mutation or observation opening failed.
    #[error(transparent)]
    Session(#[from] SessionError),
    /// An opened observation failed.
    #[error(transparent)]
    Turn(#[from] TurnError),
    /// A finite transport ended before the required durable record.
    #[error("Session observation ended before {0}")]
    Ended(&'static str),
    /// The durable message was discarded before it could execute.
    #[error("message `{message_id}` was discarded before execution: {reason:?}")]
    Discarded {
        /// Exact discarded message.
        message_id: MessageId,
        /// Durable discard reason.
        reason: MessageDiscardReason,
    },
    /// Presentation no longer accepts updates.
    #[error("message sink stopped before the turn ended")]
    SinkStopped,
}

/// Explicit presentation seam for a single message's execution.
#[async_trait]
pub trait MessageSink: std::fmt::Debug + Send + Sync {
    /// Acknowledges delivery; interrupted Turns still deliver terminal events.
    async fn event(&self, event: MessageEvent) -> Result<(), MessageRunError>;
}

/// Follows a submission and its exact durable claim, including cancellation races.
///
/// The caller owns this future and any interaction subscription. Dropping it
/// ends observation, without implying cancellation of admitted server work.
#[allow(clippy::too_many_lines)] // One state transition loop owns the pending-to-claimed cancellation race.
pub async fn drive_message(
    handle: &dyn SessionHandle,
    request: SubmitInput,
    cancellation: &CancellationToken,
    sink: &dyn MessageSink,
) -> Result<TurnOutcome, MessageRunError> {
    let message_id = request.message_id.clone();
    let receipt = submit_with_reconciliation(handle, request).await?;
    sink.event(MessageEvent::Accepted(receipt.clone())).await?;
    let (turn_id, entered_fact_seq) = match &receipt.state {
        MessageState::Pending => {
            let mut message_cancellation_sent = false;
            let mut claim_observation = handle
                .observe(ObservationCursor {
                    control_seq: receipt.accepted_control_seq,
                    fact_seq: receipt.observed_fact_seq,
                })
                .await?;
            loop {
                tokio::select! { biased;
                    () = cancellation.cancelled(), if !message_cancellation_sent => {
                        message_cancellation_sent = true;
                        let cancelled = handle.cancel(CancelTarget::Message(message_id.clone()), None).await?;
                        if cancelled.accepted {
                            return Ok(TurnOutcome::Cancelled);
                        }
                    }
                    update = claim_observation.next() => {
                        let update = update.ok_or(MessageRunError::Ended("the message claim"))??;
                        if let SessionObservation::Control { record, .. } = update {
                            match record.body() {
                                AgentControlRecordBody::MessageClaimed { message_id: observed, turn_id, entered_fact_seq, .. }
                                    if observed == &message_id => break (turn_id.clone(), *entered_fact_seq),
                                AgentControlRecordBody::MessageDiscarded { message_id: observed, reason }
                                    if observed == &message_id => {
                                        if cancellation.is_cancelled() { return Ok(TurnOutcome::Cancelled); }
                                        return Err(MessageRunError::Discarded { message_id, reason: *reason });
                                    },
                                _ => {},
                            }
                        }
                    }
                }
            }
        }
        MessageState::Claimed {
            turn_id,
            entered_fact_seq,
            ..
        } => (turn_id.clone(), *entered_fact_seq),
        MessageState::Discarded { reason, .. } => {
            if cancellation.is_cancelled() {
                return Ok(TurnOutcome::Cancelled);
            }
            return Err(MessageRunError::Discarded {
                message_id,
                reason: *reason,
            });
        }
    };
    sink.event(MessageEvent::Claimed {
        session_id: receipt.session_id.clone(),
        message_id: receipt.message_id,
        turn_id: turn_id.clone(),
        entered_fact_seq,
    })
    .await?;
    let mut observation = handle
        .observe(ObservationCursor {
            control_seq: receipt.accepted_control_seq,
            fact_seq: entered_fact_seq,
        })
        .await?;
    let mut cancellation_sent = false;
    loop {
        tokio::select! { biased;
            () = cancellation.cancelled(), if !cancellation_sent => {
                cancellation_sent = true;
                handle.cancel(CancelTarget::Turn(turn_id.clone()), Some("client interrupt".into())).await?;
            }
            update = observation.next() => {
                let update = update.ok_or(MessageRunError::Ended("a terminal Fact"))??;
                if let SessionObservation::Fact { fact, durable_fact_seq } = update {
                    let terminal = match fact.body() {
                        SessionFactBody::TurnTerminal { turn_id: observed, outcome } if observed == &turn_id => Some(outcome.clone()),
                        _ => None,
                    };
                    sink.event(MessageEvent::Fact { session_id: receipt.session_id.clone(), fact, durable_seq: durable_fact_seq }).await?;
                    if let Some(outcome) = terminal {
                        sink.event(MessageEvent::Outcome { session_id: receipt.session_id, turn_id, outcome: outcome.clone(), durable_seq: durable_fact_seq }).await?;
                        return Ok(outcome);
                    }
                }
            }
        }
    }
}
