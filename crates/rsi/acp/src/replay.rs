use crate::{Attached, Failure};
use rsi_acp::PeerHandle;
use rsi_agent_session_protocol::{
    AgentMessageContent, InputMessageSource, ModelEventPurpose, SessionFact, SessionFactBody,
};
use rsi_ai_protocol::{ContentDelta, LanguageEvent};
use serde_json::{Value, json};

pub(super) async fn history(session: &Attached, peer: &PeerHandle) -> Result<(), Failure> {
    let horizon = session
        .handle
        .history_before(None, 1)
        .await
        .map_err(|_| Failure::Backend)?
        .durable_seq;
    let mut after = 0;
    while after < horizon {
        let before = after
            .saturating_add(65)
            .min(horizon.checked_add(1).ok_or(Failure::Backend)?);
        let page = session
            .handle
            .history_before(
                Some(before),
                usize::try_from(before - after - 1).map_err(|_| Failure::Backend)?,
            )
            .await
            .map_err(|_| Failure::Backend)?;
        if page.facts.is_empty() {
            return Err(Failure::Backend);
        }
        for fact in page.facts {
            if fact.seq() != after + 1 || fact.seq() >= before {
                return Err(Failure::Backend);
            }
            fact_update(session.id.as_str(), &fact, peer, true).await?;
            after = fact.seq();
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use rsi_acp::{Peer, StreamTransport};
    use rsi_acp_protocol::Message;

    #[tokio::test]
    async fn rejected_tool_update_preserves_identity_and_explanation() {
        let fact: SessionFact = serde_json::from_value(json!({
            "seq": 1, "timestamp_ms": 1, "type": "tool_rejected", "turn_id": "turn",
            "effect_id": "rejected", "origin": {"kind": "model", "effect_id": "model"},
            "program_role": "callable",
            "identity": {
                "owner_id": "owner", "invocation_id": "invocation", "call_id": "call",
                "request_sha256": "a".repeat(64)
            },
            "name": "bash", "arguments": {"command": "false"},
            "rejection": {
                "type": "policy_denied", "contribution_id": "policy", "reason": "fixture denied"
            }
        }))
        .unwrap();
        let (left, right) = tokio::io::duplex(65536);
        let (lr, lw) = tokio::io::split(left);
        let (rr, rw) = tokio::io::split(right);
        let mut client = Peer::start(StreamTransport::new(lr, lw));
        let server = Peer::start(StreamTransport::new(rr, rw));
        fact_update("session", &fact, &server.handle(), false)
            .await
            .unwrap();
        server.handle().drain().await.unwrap();
        let event = client.next().await.unwrap();
        let Message::Notification { params, .. } = event.message else {
            panic!("expected notification")
        };
        let update = &params["update"];
        assert_eq!(update["name"], "bash");
        assert_eq!(update["rawInput"]["command"], "false");
        assert_eq!(update["status"], "failed");
        assert_eq!(update["rawOutput"]["is_error"], true);
        assert_eq!(update["rawOutput"]["rejection"]["reason"], "fixture denied");
        assert!(
            update["content"][0]["content"]["text"]
                .as_str()
                .unwrap()
                .contains("fixture denied")
        );
        client.close().await.unwrap();
        server.close().await.unwrap();
    }
}

async fn update(id: &str, peer: &PeerHandle, update: Value) -> Result<(), Failure> {
    peer.notify("session/update", &json!({"sessionId":id,"update":update}))
        .await
        .map_err(|_| Failure::Backend)
}
async fn text(id: &str, peer: &PeerHandle, kind: &str, mut text: &str) -> Result<(), Failure> {
    while !text.is_empty() {
        let mut end = text.len().min(32 * 1024);
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        let (chunk, rest) = text.split_at(end);
        update(
            id,
            peer,
            json!({"sessionUpdate":kind,"content":{"type":"text","text":chunk}}),
        )
        .await?;
        text = rest;
    }
    Ok(())
}
pub(super) async fn fact_update(
    id: &str,
    fact: &SessionFact,
    peer: &PeerHandle,
    replay: bool,
) -> Result<(), Failure> {
    match fact.body() {
        SessionFactBody::TurnAccepted { text: input, .. } if replay => {
            text(id, peer, "user_message_chunk", input).await?;
        }
        SessionFactBody::InputMessageEntered {
            source: InputMessageSource::Human { .. },
            content,
            ..
        } if replay => {
            for block in content {
                if let AgentMessageContent::Text { text: input } = block {
                    text(id, peer, "user_message_chunk", input).await?;
                }
            }
        }
        SessionFactBody::ModelEvent {
            purpose: ModelEventPurpose::Conversation,
            event:
                LanguageEvent::ContentDelta {
                    delta: ContentDelta::Text(chunk),
                    ..
                },
            ..
        } => {
            text(id, peer, "agent_message_chunk", chunk).await?;
        }
        SessionFactBody::ToolIntent {
            effect_id,
            name,
            arguments,
            ..
        } => {
            update(
                id,
                peer,
                json!({
                    "sessionUpdate": "tool_call", "toolCallId": effect_id.as_str(),
                    "name": name, "title": name, "kind": "other", "status": "pending",
                    "rawInput": arguments
                }),
            )
            .await?;
        }
        SessionFactBody::ToolStarted { effect_id, .. } => {
            update(id, peer, json!({"sessionUpdate":"tool_call_update","toolCallId":effect_id.as_str(),"status":"in_progress"})).await?;
        }
        SessionFactBody::ToolRejected {
            effect_id,
            name,
            arguments,
            rejection,
            ..
        } => {
            let explanation = serde_json::to_string(rejection).map_err(|_| Failure::Backend)?;
            update(
                id,
                peer,
                json!({
                    "sessionUpdate": "tool_call", "toolCallId": effect_id.as_str(),
                    "name": name, "title": name, "kind": "other", "status": "failed",
                    "rawInput": arguments,
                    "content": [
                        {"type": "content", "content": {"type": "text", "text": explanation}}
                    ],
                    "rawOutput": {"rejection": rejection, "is_error": true}
                }),
            )
            .await?;
        }
        SessionFactBody::ToolResult {
            effect_id, result, ..
        } => {
            update(id, peer, json!({"sessionUpdate":"tool_call_update","toolCallId":effect_id.as_str(),"status":if result.is_error { "failed" } else { "completed" },"rawOutput":result})).await?;
        }
        _ => {}
    }
    Ok(())
}
