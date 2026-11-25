use super::MAX_BLOCK_BYTES;
use rsi_agent_session_protocol::{AgentMessageContent, SessionFact, SessionFactBody};
use rsi_conversation::{EnteredContent, EnteredInput};

#[derive(Clone, Copy, Debug)]
pub(crate) enum PromptRecall {
    Text,
    Recorded { offset: u32, count: u32 },
}

pub(super) fn prompt_recall(row: &EnteredInput<'_>) -> Option<PromptRecall> {
    if !row.is_human() {
        return None;
    }
    let (count, single) = match row.content {
        EnteredContent::Direct(text) => (1, Some(text)),
        EnteredContent::Tool([text])
        | EnteredContent::Blocks([AgentMessageContent::Text { text }]) => (1, Some(text.as_str())),
        EnteredContent::Tool(parts) if parts.iter().any(|text| !text.trim().is_empty()) => {
            (parts.len(), None)
        }
        EnteredContent::Blocks(parts)
            if parts.iter().any(|part| match part {
                AgentMessageContent::Text { text } => !text.trim().is_empty(),
                AgentMessageContent::Reference { .. } => true,
                _ => false,
            }) =>
        {
            (parts.len(), None)
        }
        _ => return None,
    };
    if let Some(text) = single {
        if text.trim().is_empty() {
            return None;
        }
        if text.len() <= MAX_BLOCK_BYTES {
            return Some(PromptRecall::Text);
        }
    }
    Some(PromptRecall::Recorded {
        offset: row.offset,
        count: u32::try_from(count).expect("bounded input parts"),
    })
}

pub(crate) fn recorded_text(fact: &SessionFact, offset: u32, count: u32) -> Option<String> {
    let start = offset as usize;
    let len = count as usize;
    let mut text = String::new();
    let append = |(index, part): (usize, &AgentMessageContent)| match part {
        AgentMessageContent::Text { text: part } => text.push_str(part),
        AgentMessageContent::Reference { reference } => {
            text.push_str(&reference.prompt_text(fact.seq(), index))
        }
        AgentMessageContent::File { .. } | AgentMessageContent::Image { .. } => {}
    };
    match fact.body() {
        SessionFactBody::TurnAccepted { text, .. } => return Some(text.clone()),
        SessionFactBody::InputMessageClaimed { content, .. } => {
            content
                .iter()
                .enumerate()
                .skip(start)
                .take(len)
                .for_each(append);
        }
        SessionFactBody::StepInputsPrepared { decision, .. } => {
            decision
                .authored_content()
                .enumerate()
                .skip(start)
                .take(len)
                .for_each(append);
        }
        SessionFactBody::ToolContextsClaimed { messages, .. } => {
            text.extend(
                messages
                    .iter()
                    .flat_map(|row| &row.message.text)
                    .skip(start)
                    .take(len)
                    .map(String::as_str),
            );
        }
        _ => return None,
    }
    Some(text)
}

#[cfg(test)]
mod tests {
    use super::*;
    use rsi_conversation::EnteredSource;

    #[test]
    fn a_recalled_reference_uses_its_exact_occurrence_as_plain_text() {
        use rsi_agent_session_protocol::{InputMessageSource, MessageId, StepId, TurnId};
        let reference = serde_json::from_value(serde_json::json!({
            "snapshot":{"capture_id":"capture","sha256":"d".repeat(64),"byte_len":500},
            "metadata":{
                "source":{"kind":"native","binding":{"session_id":"original","header_sha256":"a".repeat(64)}},
                "target":{"session_id":"current","header_sha256":"b".repeat(64)},
                "text_bytes":7,
                "capture":{"kind":"suffix","interval":{
                    "through_seq":"5","fact_prefix_sha256":"c".repeat(64),"scanned_after_seq":"1",
                    "retained_after_seq":"1","retained_through_seq":"5","scanned_bytes":32,"omissions":["fact_limit"]
                }}
            },
            "preview":"excerpt"
        })).unwrap();
        let fact = SessionFact::new(
            9,
            1,
            SessionFactBody::InputMessageClaimed {
                turn_id: TurnId::new("turn").unwrap(),
                step_id: StepId::new("step").unwrap(),
                source: InputMessageSource::Human {
                    message_id: MessageId::new("message").unwrap(),
                    attribution: None,
                },
                content: vec![
                    AgentMessageContent::Text {
                        text: "quoted: ".into(),
                    },
                    AgentMessageContent::Reference { reference },
                ],
            },
        )
        .unwrap();
        assert_eq!(
            recorded_text(&fact, 0, 2).unwrap(),
            concat!(
                "quoted: Referenced conversation data from original through record 5. Earlier material was omitted by capture limits.\n",
                "excerpt\nRead more with reference_read using recorded_session_id=\"current\", fact_seq=\"9\", content_index=1."
            )
        );
    }

    #[test]
    fn plain_prompts_use_the_loaded_block_until_it_clips() {
        let source = serde_json::json!({"kind":"schedule"});
        let row = EnteredInput {
            source: EnteredSource::Direct,
            seq: 11,
            offset: 0,
            index: 1,
            content: EnteredContent::Direct("line 1\nline 2"),
        };
        assert!(matches!(prompt_recall(&row), Some(PromptRecall::Text)));
        assert!(
            prompt_recall(&EnteredInput {
                source: EnteredSource::Application(&source),
                ..row
            })
            .is_none()
        );
        assert!(
            prompt_recall(&EnteredInput {
                content: EnteredContent::Direct(" \n "),
                ..row
            })
            .is_none()
        );
        let clipped = "a".repeat(MAX_BLOCK_BYTES + 1);
        assert!(matches!(
            prompt_recall(&EnteredInput {
                content: EnteredContent::Direct(&clipped),
                ..row
            }),
            Some(PromptRecall::Recorded {
                offset: 0,
                count: 1
            })
        ));
    }
}
