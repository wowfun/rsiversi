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
        SessionFactBody::ModelAttemptSettled {
            purpose: ModelEventPurpose::Conversation,
            attempt,
            ..
        } => {
            send_events(id, peer, attempt.events()).await?;
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

/// One serial Session attempt, with bounded deduplication across live and committed delivery.
#[derive(Default)]
pub(super) struct ModelDelivery {
    current: Option<rsi_agent_session_protocol::EffectId>,
    settled: Option<rsi_agent_session_protocol::EffectId>,
    cursor: rsi_agent_session_protocol::ModelEventCursor,
}
impl ModelDelivery {
    fn select(&mut self, effect: &rsi_agent_session_protocol::EffectId) {
        if self.current.as_ref() != Some(effect) {
            self.current = Some(effect.clone());
            self.cursor = rsi_agent_session_protocol::ModelEventCursor::default();
        }
    }
    pub(super) async fn live(
        &mut self,
        id: &str,
        peer: &PeerHandle,
        snapshot: &rsi_agent_turn_protocol::ModelLiveSnapshot,
    ) -> Result<(), Failure> {
        if snapshot.purpose != ModelEventPurpose::Conversation
            || self.current.as_ref() != Some(&snapshot.effect_id)
            || self.settled.as_ref() == Some(&snapshot.effect_id)
            || !matches!(
                snapshot.status,
                rsi_agent_turn_protocol::ModelLiveStatus::Streaming
            )
        {
            return Ok(());
        }
        send_events(id, peer, snapshot.prefix.events_after(&mut self.cursor)).await
    }
    pub(super) async fn fact(
        &mut self,
        id: &str,
        peer: &PeerHandle,
        fact: &SessionFact,
    ) -> Result<(), Failure> {
        if let SessionFactBody::ModelStarted { effect_id, .. } = fact.body() {
            self.select(effect_id);
        }
        if let SessionFactBody::ModelAttemptSettled {
            effect_id,
            attempt,
            purpose: ModelEventPurpose::Conversation,
            ..
        } = fact.body()
        {
            self.select(effect_id);
            send_events(id, peer, attempt.events_after(&mut self.cursor)).await?;
            self.settled = Some(effect_id.clone());
            Ok(())
        } else {
            fact_update(id, fact, peer, false).await
        }
    }
}
async fn send_events(
    id: &str,
    peer: &PeerHandle,
    events: impl Iterator<Item = rsi_agent_session_protocol::Result<(u64, LanguageEvent)>>,
) -> Result<(), Failure> {
    let mut pending = String::new();
    for item in events {
        let (_, event) = item.map_err(|_| Failure::Backend)?;
        if let LanguageEvent::ContentDelta {
            delta: ContentDelta::Text(chunk),
            ..
        } = event
        {
            let mut rest = chunk.as_str();
            while !rest.is_empty() {
                let mut end = rest.len().min(32 * 1024 - pending.len());
                while !rest.is_char_boundary(end) {
                    end -= 1;
                }
                if end == 0 {
                    text(id, peer, "agent_message_chunk", &pending).await?;
                    pending.clear();
                    continue;
                }
                pending.push_str(&rest[..end]);
                rest = &rest[end..];
                if pending.len() >= 32 * 1024 {
                    text(id, peer, "agent_message_chunk", &pending).await?;
                    pending.clear();
                }
            }
        } else if !pending.is_empty() {
            text(id, peer, "agent_message_chunk", &pending).await?;
            pending.clear();
        }
    }
    text(id, peer, "agent_message_chunk", &pending).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use rsi_acp::{Peer, StreamTransport};
    use rsi_acp_protocol::Message;

    #[tokio::test]
    #[allow(clippy::too_many_lines)] // One peer lifecycle checks Unicode boundaries, coalescing, replay and cleanup.
    async fn unicode_live_and_settled_delivery_deduplicate_across_attempts_without_reasoning() {
        use rsi_agent_session_protocol::{
            EffectId, ModelAttemptBuilder, ModelAttemptLog, SessionId, TurnId,
        };
        use rsi_agent_turn_protocol::{ModelLiveSnapshot, ModelLiveStatus};
        use rsi_ai_protocol::{ContentStart, FinishReason};
        let (left, right) = tokio::io::duplex(65536);
        let (lr, lw) = tokio::io::split(left);
        let (rr, rw) = tokio::io::split(right);
        let mut client = Peer::start(StreamTransport::new(lr, lw));
        let server = Peer::start(StreamTransport::new(rr, rw));
        let peer = server.handle();
        let (drained, wait_drained) = tokio::sync::oneshot::channel();
        let reader = tokio::spawn(async move {
            let mut text = String::new();
            let mut sizes = Vec::new();
            loop {
                let event = client.next().await.unwrap();
                let Message::Notification { method, params, .. } = event.message else {
                    panic!("notification");
                };
                if method == "test/end" {
                    break;
                }
                assert_eq!(params["update"]["sessionUpdate"], "agent_message_chunk");
                let chunk = params["update"]["content"]["text"].as_str().unwrap();
                sizes.push(chunk.len());
                text.push_str(chunk);
            }
            wait_drained.await.unwrap();
            client.close().await.unwrap();
            (text, sizes)
        });
        let first = "界🦀".repeat(16000);
        let second = "next 界";
        let mut delivery = ModelDelivery::default();
        let mut stale = None;
        for (ordinal, part) in [(0u64, first.as_str()), (1, second)] {
            let turn = TurnId::new("turn").unwrap();
            let effect = EffectId::new(format!("effect-{ordinal}")).unwrap();
            let mut builder = ModelAttemptBuilder::new();
            builder
                .push(
                    0,
                    &LanguageEvent::ContentStarted {
                        index: 0,
                        content: ContentStart::Reasoning,
                    },
                )
                .unwrap();
            builder
                .push(
                    1,
                    &LanguageEvent::ContentDelta {
                        index: 0,
                        delta: ContentDelta::Reasoning("private reasoning".into()),
                    },
                )
                .unwrap();
            builder
                .push(2, &LanguageEvent::ContentFinished { index: 0 })
                .unwrap();
            builder
                .push(
                    3,
                    &LanguageEvent::ContentStarted {
                        index: 1,
                        content: ContentStart::Text,
                    },
                )
                .unwrap();
            builder
                .push(
                    4,
                    &LanguageEvent::ContentDelta {
                        index: 1,
                        delta: ContentDelta::Text(part.into()),
                    },
                )
                .unwrap();
            let live = ModelLiveSnapshot {
                session_id: SessionId::new("session").unwrap(),
                turn_id: turn.clone(),
                effect_id: effect.clone(),
                purpose: ModelEventPurpose::Conversation,
                revision: 1,
                prefix: builder.prefix(),
                status: ModelLiveStatus::Streaming,
            };
            // Live can win the race with its admission Fact; settlement must fill any missed prefix.
            delivery.live("session", &peer, &live).await.unwrap();
            let started = SessionFact::new(
                ordinal * 3 + 1,
                1,
                SessionFactBody::ModelStarted {
                    turn_id: turn.clone(),
                    effect_id: effect.clone(),
                },
            )
            .unwrap();
            delivery.fact("session", &peer, &started).await.unwrap();
            if let Some(previous) = &stale {
                delivery.live("session", &peer, previous).await.unwrap();
            }
            if ordinal == 0 {
                delivery.live("session", &peer, &live).await.unwrap();
                delivery.live("session", &peer, &live).await.unwrap();
            }
            builder
                .push(5, &LanguageEvent::ContentFinished { index: 1 })
                .unwrap();
            builder
                .push(
                    6,
                    &LanguageEvent::Finished {
                        reason: FinishReason::Stop,
                        replay: None,
                    },
                )
                .unwrap();
            let attempt: ModelAttemptLog = builder.finish(true).unwrap();
            let settled = SessionFact::new(
                ordinal * 3 + 2,
                2,
                SessionFactBody::ModelAttemptSettled {
                    turn_id: turn,
                    effect_id: effect,
                    purpose: ModelEventPurpose::Conversation,
                    attempt,
                },
            )
            .unwrap();
            delivery.fact("session", &peer, &settled).await.unwrap();
            delivery.live("session", &peer, &live).await.unwrap();
            stale = Some(live);
        }
        peer.notify("test/end", &json!({})).await.unwrap();
        peer.drain().await.unwrap();
        drained.send(()).unwrap();
        let (actual, sizes) = reader.await.unwrap();
        server.close().await.unwrap();
        assert_eq!(actual, format!("{first}{second}"));
        assert!(sizes.iter().all(|size| *size > 0 && *size <= 32 * 1024));
        assert!(sizes.len() < 10, "small fragments must be coalesced");
    }

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
