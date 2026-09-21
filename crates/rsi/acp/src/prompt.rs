use crate::{Attached, Failure, SETTLEMENT, replay};
use futures_util::StreamExt as _;
use rsi_acp::PeerHandle;
use rsi_acp_protocol::schema;
use rsi_agent_session_protocol::{
    MessageDelivery, MessageId, SessionFactBody, SessionId, TurnId, TurnOutcome,
};
use rsi_agent_turn_protocol::{
    CancelTarget, ControlledWorkStatus, MessageState, ObservationCursor, SessionObservation,
    TurnService,
};
use rsi_approval_protocol::ApprovalDecision;
use rsi_session_protocol::{SessionInput, SubmitInput};
use serde_json::json;
use std::sync::atomic::Ordering;
use tokio_util::sync::CancellationToken;

pub(super) fn content(blocks: Vec<schema::ContentBlock>) -> Result<Vec<SessionInput>, Failure> {
    let content = blocks
        .into_iter()
        .map(|block| match block {
            schema::ContentBlock::Text(text) => Ok(SessionInput::Text { text: text.text }),
            schema::ContentBlock::ResourceLink(link) => Ok(SessionInput::Text {
                text: format!("{}: {}", link.name, link.uri),
            }),
            _ => Err(Failure::Parameters),
        })
        .collect::<Result<Vec<_>, _>>()?;
    rsi_session_protocol::validate_session_input(&content).map_err(|_| Failure::Parameters)?;
    Ok(content)
}

async fn permissions(session: &Attached, peer: &PeerHandle) -> Result<(), Failure> {
    let mut stream = session
        .handle
        .observe_interactions()
        .await
        .map_err(|_| Failure::Backend)?;
    while let Some(snapshot) = stream.next().await {
        let snapshot = snapshot.map_err(|_| Failure::Backend)?;
        if !snapshot.questions().is_empty() {
            return Err(Failure::Backend);
        }
        for approval in snapshot.approvals() {
            // A coalesced snapshot may predate an answer to an earlier request.
            if !session
                .handle
                .pending_approvals()
                .await
                .map_err(|_| Failure::Backend)?
                .iter()
                .any(|current| current.id == approval.id && current.subject == approval.subject)
            {
                continue;
            }
            let params = json!({"sessionId":session.id.as_str(),"toolCall":{"toolCallId":approval.subject.effect_id(),"title":approval.action,"kind":"other","status":"pending","rawInput":approval.review.as_ref().map(|review| &review.arguments)},"options":[{"optionId":"allow-once","name":"Allow once","kind":"allow_once"},{"optionId":"reject-once","name":"Reject once","kind":"reject_once"}]});
            let response = peer
                .request_permission(&params)
                .await
                .map_err(|_| Failure::Backend)?;
            let result = response.result().map_err(|_| Failure::Backend)?;
            let outcome = result.get("outcome").ok_or(Failure::Parameters)?;
            let decision = match (
                outcome.get("outcome").and_then(serde_json::Value::as_str),
                outcome.get("optionId").and_then(serde_json::Value::as_str),
            ) {
                (Some("selected"), Some("allow-once")) => ApprovalDecision::AllowOnce,
                (Some("selected"), Some("reject-once")) | (Some("cancelled"), None) => {
                    ApprovalDecision::Deny
                }
                _ => return Err(Failure::Parameters),
            };
            let owner =
                SessionId::new(approval.subject.session_id()).map_err(|_| Failure::Backend)?;
            session
                .handle
                .answer_approval(&owner, &approval.id, decision)
                .await
                .map_err(|_| Failure::Backend)?;
        }
    }
    Err(Failure::Backend)
}

async fn settlement(
    session: &Attached,
    turns: &dyn TurnService,
    turn: &TurnId,
) -> Result<(), Failure> {
    let work = turns
        .controlled_work(&session.id, turn)
        .map_err(|_| Failure::Backend)?
        .ok_or(Failure::Backend)?;
    match tokio::time::timeout(SETTLEMENT, work.wait(CancellationToken::new())).await {
        Ok(ControlledWorkStatus::Settled) => {
            session.settled.store(true, Ordering::Release);
            Ok(())
        }
        _ => Err(Failure::Backend),
    }
}

struct Submission {
    message: MessageId,
    cursor: ObservationCursor,
    turn: Option<TurnId>,
}

async fn observe(
    session: &Attached,
    turns: &dyn TurnService,
    submission: &Submission,
    peer: Option<&PeerHandle>,
    cancellation: CancellationToken,
) -> Result<schema::PromptResponse, Failure> {
    let mut updates = session
        .handle
        .observe(submission.cursor)
        .await
        .map_err(|_| Failure::Backend)?;
    let mut turn = submission.turn.clone();
    let mut cancel_deadline = None;
    loop {
        let observation = tokio::select! { biased;
            () = cancellation.cancelled(), if cancel_deadline.is_none() => {
                cancel_deadline = Some(tokio::time::Instant::now() + SETTLEMENT);
                tokio::time::timeout(SETTLEMENT, session.handle.cancel(CancelTarget::Message(submission.message.clone()), None)).await.map_err(|_| Failure::Timeout)?.map_err(|_| Failure::Backend)?;
                continue;
            }
            () = async { match cancel_deadline { Some(deadline) => tokio::time::sleep_until(deadline).await, None => std::future::pending().await } } => return Err(Failure::Timeout),
            update = updates.next() => update.ok_or(Failure::Backend)?.map_err(|_| Failure::Backend)?,
        };
        match observation {
            SessionObservation::Control { .. } if turn.is_none() => {
                match session
                    .handle
                    .message_status(&submission.message)
                    .await
                    .map_err(|_| Failure::Backend)?
                    .state
                {
                    MessageState::Pending => {}
                    MessageState::Claimed { turn_id, .. } => turn = Some(turn_id),
                    MessageState::Discarded { .. } => {
                        session.settled.store(true, Ordering::Release);
                        return Ok(schema::PromptResponse::new(schema::StopReason::Cancelled));
                    }
                }
            }
            SessionObservation::Fact { fact, .. } => {
                if let SessionFactBody::MessageTurnAccepted {
                    message_ids,
                    turn_id,
                    ..
                } = fact.body()
                    && message_ids.contains(&submission.message)
                {
                    turn = Some(turn_id.clone());
                }
                if turn.as_ref() != Some(fact.body().turn_id()) {
                    continue;
                }
                if let Some(peer) = peer {
                    replay::fact_update(session.id.as_str(), &fact, peer, false).await?;
                }
                if let SessionFactBody::TurnTerminal {
                    turn_id, outcome, ..
                } = fact.body()
                {
                    settlement(session, turns, turn_id).await?;
                    let reason = match outcome {
                        TurnOutcome::Completed => schema::StopReason::EndTurn,
                        TurnOutcome::Cancelled => schema::StopReason::Cancelled,
                        TurnOutcome::BudgetExceeded { .. } => schema::StopReason::MaxTurnRequests,
                        _ => return Err(Failure::Backend),
                    };
                    if let Some(peer) = peer {
                        peer.drain().await.map_err(|_| Failure::Backend)?;
                    }
                    return Ok(schema::PromptResponse::new(reason));
                }
            }
            SessionObservation::Control { .. } => {}
        }
    }
}

pub(super) async fn run(
    session: &Attached,
    turns: &dyn TurnService,
    content: Vec<SessionInput>,
    peer: &PeerHandle,
    cancellation: CancellationToken,
) -> Result<schema::PromptResponse, Failure> {
    let before = session
        .handle
        .history_before(None, 1)
        .await
        .map_err(|_| Failure::Backend)?
        .durable_seq;
    let mut entropy = [0_u8; 16];
    getrandom::fill(&mut entropy).map_err(|_| Failure::Backend)?;
    let message =
        MessageId::new(format!("acp-{}", hex::encode(entropy))).map_err(|_| Failure::Backend)?;
    session.settled.store(false, Ordering::Release);
    let receipt = session
        .handle
        .submit(SubmitInput {
            delivery: MessageDelivery::NextTurn,
            message_id: message.clone(),
            content,
            model: None,
            reasoning_effort: None,
            sandbox: None,
        })
        .await
        .map_err(|_| Failure::Backend)?;
    let turn = match receipt.state {
        MessageState::Claimed { turn_id, .. } => Some(turn_id),
        _ => None,
    };
    let submission = Submission {
        message,
        cursor: ObservationCursor {
            control_seq: receipt.accepted_control_seq - 1,
            fact_seq: before,
        },
        turn,
    };
    let result = tokio::select! {
        result = observe(session, turns, &submission, Some(peer), cancellation.clone()) => result,
        result = permissions(session, peer) => result.and(Err(Failure::Backend)),
    };
    if result.is_err() && !session.settled.load(Ordering::Acquire) {
        // Native work remains owned when protocol observation or permission fails.
        // Retry only observation/cancel, never the submitted message.
        cancellation.cancel();
        let _cleanup = tokio::time::timeout(
            SETTLEMENT,
            observe(session, turns, &submission, None, cancellation),
        )
        .await;
    }
    result
}
