//! Bounded public reply projection for an exact child's ordinary completion.
use super::*;
use rsi_ai_protocol::{ContentBlock, FinishReason, LanguageAssembler, LanguageEvent};
const SCAN_BYTES: usize = 4 * 1024 * 1024;
const REPLY_BYTES: usize = rsi_agent_session_protocol::MAXIMUM_COMPLETION_MESSAGE_BYTES;
const SCAN_PAGES: usize = 128;

pub(super) async fn read(
    inner: &KernelInner,
    session: &SessionId,
    turn: &TurnId,
    through: u64,
) -> Option<String> {
    match read_projection(inner, session, turn, through).await {
        Ok(reply) => reply,
        Err(_) => Some(
            "[Child reply omitted: its history could not be read. Inspect the child history.]"
                .into(),
        ),
    }
}
async fn read_projection(
    inner: &KernelInner,
    session: &SessionId,
    turn: &TurnId,
    through: u64,
) -> TurnResult<Option<String>> {
    let mut before = through
        .checked_add(1)
        .ok_or_else(|| TurnError::Invariant("reply cursor overflow".into()))?;
    let mut scanned = 0usize;
    let mut events = Vec::new();
    for _ in 0..SCAN_PAGES {
        let (limit, bytes) = store_reads::page_limit(inner, MAXIMUM_FACTS_PER_READ);
        let (page, _permit, _lease) =
            store_reads::read(inner, session, bytes, true, move |store, id| async move {
                store.read_facts_before(&id, before, limit).await
            })
            .await
            .map_err(turn_store_error)?;
        page.validate().map_err(turn_store_error)?;
        if page.before_seq != before {
            return Err(TurnError::Invariant(
                "reply page changed its requested cursor".into(),
            ));
        }
        for fact in page.facts.into_iter().rev() {
            scanned = scanned.saturating_add(fact.encoded_len());
            if scanned > SCAN_BYTES {
                return Ok(Some(omitted()));
            }
            before = fact.seq();
            if fact.body().turn_id() != turn {
                continue;
            }
            match fact.body() {
                SessionFactBody::ModelIntent {
                    effect_id,
                    purpose: rsi_agent_session_protocol::ModelPurpose::Conversation,
                    ..
                } => {
                    let mut reply = Reply::default();
                    reply.start(effect_id.clone());
                    for fact in events.into_iter().rev() {
                        if let SessionFactBody::ModelEvent {
                            effect_id, event, ..
                        } = fact
                        {
                            reply.event(&effect_id, &event);
                        }
                    }
                    return Ok(reply.text);
                }
                SessionFactBody::ModelEvent {
                    purpose: rsi_agent_session_protocol::ModelEventPurpose::Conversation,
                    ..
                } => events.push(fact.into_body()),
                SessionFactBody::TurnAccepted { .. } => return Ok(None),
                _ => {}
            }
        }
        if !page.has_more {
            return Ok(None);
        }
    }
    Ok(Some(omitted()))
}
fn omitted() -> String {
    "[Child reply omitted: its final attempt exceeds the 4 MiB or 128-page completion scan bound. Inspect the child history.]".into()
}
// The reservation covers the complete serialized AgentMessage, including identifiers
// and JSON escaping. The source projection's raw-byte bound alone cannot enforce it.
pub(super) fn bound_message(message: &mut AgentMessage) -> TurnResult<()> {
    use rsi_agent_session_protocol::MAXIMUM_COMPLETION_MESSAGE_BYTES;
    const MARKER: &str =
        "\n[Child reply truncated to fit the completion message; inspect the child history.]";
    let [AgentMessageContent::Text { text }] = message.content.as_mut_slice() else {
        return Err(TurnError::Invariant(
            "ordinary completion must contain one text block".into(),
        ));
    };
    let safe = std::mem::take(text)
        .replace('\0', "\\0")
        .replace('\u{7f}', "\\x7f");
    let overhead = serde_json::to_vec(message)
        .map_err(|e| TurnError::Invariant(e.to_string()))?
        .len();
    let maximum = MAXIMUM_COMPLETION_MESSAGE_BYTES.saturating_sub(overhead);
    let cost = |text: &str| text.chars().map(json_character_bytes).sum::<usize>();
    let bounded = if cost(&safe) <= maximum {
        safe
    } else {
        let left = maximum.checked_sub(cost(MARKER)).ok_or_else(|| {
            TurnError::Invariant("completion metadata exhausts its reservation".into())
        })?;
        let mut used = 0;
        let mut end = 0;
        for (offset, character) in safe.char_indices() {
            let next = used + json_character_bytes(character);
            if next > left {
                break;
            }
            used = next;
            end = offset + character.len_utf8();
        }
        format!("{}{MARKER}", &safe[..end])
    };
    message.content[0] = AgentMessageContent::Text { text: bounded };
    message
        .validate()
        .map_err(|e| TurnError::Invariant(e.to_string()))
}
fn json_character_bytes(character: char) -> usize {
    match character {
        '"' | '\\' | '\n' | '\r' | '\t' | '\u{8}' | '\u{c}' => 2,
        '\u{0}'..='\u{1f}' => 6,
        _ => character.len_utf8(),
    }
}
#[derive(Default)]
struct Reply {
    active: Option<(EffectId, LanguageAssembler)>,
    text: Option<String>,
}
impl Reply {
    fn start(&mut self, id: EffectId) {
        self.text = None;
        self.active = Some((id, LanguageAssembler::new()));
    }
    fn event(&mut self, id: &EffectId, event: &LanguageEvent) {
        let Some((active, assembler)) = &mut self.active else {
            return;
        };
        if active != id {
            return;
        }
        if assembler.push(event).is_err() {
            self.active = None;
            return;
        }
        if !matches!(
            event,
            LanguageEvent::Finished { .. } | LanguageEvent::Failed { .. }
        ) {
            return;
        }
        let (_, assembler) = self.active.take().expect("observed active response");
        let Ok(output) = assembler.finish() else {
            return;
        };
        if output.finish_reason != FinishReason::Stop
            || output
                .content
                .iter()
                .any(|block| matches!(block, ContentBlock::ToolCall(_)))
        {
            return;
        }
        let mut text = String::new();
        let mut truncated = false;
        for block in output.content {
            if let ContentBlock::Text { text: part } = block {
                let left = REPLY_BYTES.saturating_sub(text.len());
                let mut end = part.len().min(left);
                while !part.is_char_boundary(end) {
                    end -= 1;
                }
                text.push_str(&part[..end]);
                truncated |= end < part.len();
            }
        }
        if truncated {
            text.push_str("\n[Child reply truncated at 8 KiB; inspect the child history for the complete answer.]");
        }
        if !text.trim().is_empty() {
            self.text = Some(text);
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use rsi_ai_protocol::{ContentDelta, ContentStart, FinishReason};
    #[test]
    fn truncated_or_filtered_final_attempt_is_not_a_complete_reply() {
        for reason in [
            FinishReason::MaxTokens,
            FinishReason::ContentFilter,
            FinishReason::Cancelled,
        ] {
            let mut reply = Reply::default();
            let first = EffectId::new("first").unwrap();
            reply.start(first.clone());
            output(&mut reply, &first, "old answer");
            let last = EffectId::new("last").unwrap();
            reply.start(last.clone());
            for event in [
                LanguageEvent::ContentStarted {
                    index: 0,
                    content: ContentStart::Text,
                },
                LanguageEvent::ContentDelta {
                    index: 0,
                    delta: ContentDelta::Text("partial answer".into()),
                },
                LanguageEvent::ContentFinished { index: 0 },
                LanguageEvent::Finished {
                    reason: reason.clone(),
                    replay: None,
                },
            ] {
                reply.event(&last, &event);
            }
            assert!(reply.text.is_none(), "{reason:?}");
        }
    }
    fn output(reply: &mut Reply, id: &EffectId, text: &str) {
        for event in [
            LanguageEvent::ContentStarted {
                index: 0,
                content: ContentStart::Reasoning,
            },
            LanguageEvent::ContentDelta {
                index: 0,
                delta: ContentDelta::Reasoning("private reasoning".into()),
            },
            LanguageEvent::ContentFinished { index: 0 },
            LanguageEvent::ContentStarted {
                index: 1,
                content: ContentStart::Text,
            },
            LanguageEvent::ContentDelta {
                index: 1,
                delta: ContentDelta::Text(text.into()),
            },
            LanguageEvent::ContentFinished { index: 1 },
            LanguageEvent::Finished {
                reason: FinishReason::Stop,
                replay: None,
            },
        ] {
            reply.event(id, &event);
        }
    }
    #[test]
    fn failed_final_attempt_discards_partial_text_and_earlier_success() {
        let mut reply = Reply::default();
        let first = EffectId::new("first").unwrap();
        reply.start(first.clone());
        output(&mut reply, &first, "earlier answer");
        let last = EffectId::new("failed").unwrap();
        reply.start(last.clone());
        for event in [
            LanguageEvent::ContentStarted {
                index: 0,
                content: ContentStart::Text,
            },
            LanguageEvent::ContentDelta {
                index: 0,
                delta: ContentDelta::Text("partial answer".into()),
            },
            LanguageEvent::ContentFinished { index: 0 },
            LanguageEvent::Failed {
                error: rsi_ai_protocol::AiError::new(
                    rsi_ai_protocol::ErrorKind::Transport,
                    rsi_ai_protocol::ErrorPhase::Stream,
                    rsi_ai_protocol::DispatchStatus::Unknown,
                    "fixture failure",
                )
                .unwrap(),
                replay: None,
            },
        ] {
            reply.event(&last, &event);
        }
        assert!(reply.text.is_none());
        assert!(reply.active.is_none());
    }
    #[test]
    fn empty_or_tool_producing_final_attempt_never_reuses_an_earlier_answer() {
        for tool in [false, true] {
            let mut reply = Reply::default();
            let first = EffectId::new("first").unwrap();
            let last = EffectId::new("last").unwrap();
            reply.start(first.clone());
            output(&mut reply, &first, "earlier answer");
            reply.start(last.clone());
            if tool {
                for event in [
                    LanguageEvent::ContentStarted {
                        index: 0,
                        content: ContentStart::ToolCall {
                            id: "call".into(),
                            name: "read".into(),
                            kind: rsi_ai_protocol::ToolCallKind::Function,
                        },
                    },
                    LanguageEvent::ContentDelta {
                        index: 0,
                        delta: ContentDelta::ToolArguments("{}".into()),
                    },
                    LanguageEvent::ContentFinished { index: 0 },
                    LanguageEvent::Finished {
                        reason: FinishReason::ToolCalls,
                        replay: None,
                    },
                ] {
                    reply.event(&last, &event);
                }
            } else {
                output(&mut reply, &last, " ");
            }
            assert!(reply.text.is_none());
        }
    }
    #[test]
    fn encoded_completion_cost_matches_json_and_preserves_the_reservation() {
        let sample = (0..=0x7f)
            .map(|c| char::from_u32(c).unwrap())
            .chain("界🦀\u{2028}".chars())
            .collect::<String>();
        assert_eq!(
            sample.chars().map(json_character_bytes).sum::<usize>() + 2,
            serde_json::to_string(&sample).unwrap().len()
        );
        for text in [
            "界".repeat(4000),
            "\"\\\t\n".repeat(4000),
            sample.repeat(100),
        ] {
            let mut message = AgentMessage {
                message_id: MessageId::new("message").unwrap(),
                source: AgentMessageSource::Completion {
                    child_session_id: SessionId::new("child").unwrap(),
                    activation_id: rsi_agent_session_protocol::ActivationId::new("activation")
                        .unwrap(),
                    outcome: rsi_agent_session_protocol::ActivationOutcome::Completed {
                        result: None,
                    },
                },
                content: vec![AgentMessageContent::Text { text }],
                options: MessageOptions::default(),
            };
            bound_message(&mut message).unwrap();
            message.validate().unwrap();
            assert!(
                serde_json::to_vec(&message).unwrap().len()
                    <= rsi_agent_session_protocol::MAXIMUM_COMPLETION_MESSAGE_BYTES
            );
            assert!(
                serde_json::to_string(&message)
                    .unwrap()
                    .contains("truncated")
            );
        }
    }
    #[test]
    fn final_reply_excludes_reasoning_and_previous_requests_and_bounds_utf8() {
        let mut reply = Reply::default();
        let first = EffectId::new("first").unwrap();
        let next = EffectId::new("next").unwrap();
        reply.start(first.clone());
        output(&mut reply, &first, "public answer");
        assert_eq!(reply.text.as_deref(), Some("public answer"));
        reply.start(next.clone());
        assert!(reply.text.is_none());
        output(&mut reply, &first, "wrong effect");
        assert!(reply.text.is_none());
        output(&mut reply, &next, &"界".repeat(4000));
        let text = reply.text.unwrap();
        assert!(text.starts_with('界'));
        assert!(text.contains("truncated"));
        assert!(text.len() < REPLY_BYTES + 100);
        assert!(!text.contains("private reasoning"));
    }
    #[test]
    fn complete_message_has_one_intact_notice_after_both_projection_bounds() {
        for part in ["x", "界", "\"\\\n"] {
            let mut reply = Reply::default();
            let effect = EffectId::new("model").unwrap();
            reply.start(effect.clone());
            output(&mut reply, &effect, &part.repeat(REPLY_BYTES + 1));
            let mut message = AgentMessage {
                message_id: MessageId::new("message").unwrap(),
                source: AgentMessageSource::Completion {
                    child_session_id: SessionId::new("child").unwrap(),
                    activation_id: rsi_agent_session_protocol::ActivationId::new("activation")
                        .unwrap(),
                    outcome: rsi_agent_session_protocol::ActivationOutcome::Completed {
                        result: None,
                    },
                },
                content: vec![AgentMessageContent::Text {
                    text: reply.text.unwrap(),
                }],
                options: MessageOptions::default(),
            };
            bound_message(&mut message).unwrap();
            let AgentMessageContent::Text { text } = &message.content[0] else {
                unreachable!()
            };
            assert_eq!(text.matches("[Child reply truncated").count(), 1);
            assert!(text.ends_with(
                "[Child reply truncated to fit the completion message; inspect the child history.]"
            ));
            assert!(!text.contains("8 KiB"));
        }
    }
}
