use super::{MAX_BLOCK_BYTES, Transcript, short};
use rsi_conversation::{MAXIMUM_BLOCK_SOURCES, SourceAdmission, SourceRef};

impl Transcript {
    pub(super) fn put_source(
        &mut self,
        key: String,
        role: &'static str,
        title: &str,
        source: SourceRef,
        text: &str,
    ) {
        let position = self
            .blocks
            .iter()
            .position(|block| block.key == key)
            .or_else(|| {
                self.add(key, role, title, "", false);
                self.blocks.len().checked_sub(1)
            });
        let Some(block) = position.and_then(|position| self.blocks.get_mut(position)) else {
            return;
        };
        if block.sources.position(source).is_some() {
            return;
        }
        if block.sources.is_empty() {
            // Entered Message data replaces its transient accepted-control preview.
            block.text.clear();
            block.clipped = false;
        }
        let text_window = short(text, MAX_BLOCK_BYTES);
        let older = block
            .sources
            .iter()
            .next_back()
            .is_some_and(|last| source < last);
        while !block.sources.is_empty()
            && (block.text.len() + text_window.len() > MAX_BLOCK_BYTES
                || block.sources.len() >= MAXIMUM_BLOCK_SOURCES)
        {
            let position = if older { block.sources.len() - 1 } else { 0 };
            block.sources.remove(position).expect("retained source");
            let length = block
                .source_bytes
                .remove(position)
                .expect("matching rendered span");
            if older {
                block.text.truncate(block.text.len() - length);
            } else {
                block.text.drain(..length);
            }
            block.clipped = true;
        }
        let SourceAdmission::Inserted(position) = block
            .sources
            .insert(source)
            .expect("bounded valid presentation source")
        else {
            unreachable!("duplicate checked before admission")
        };
        let offset = if position == block.source_bytes.len() {
            block.text.len()
        } else {
            block.source_bytes.iter().take(position).sum()
        };
        block.text.insert_str(offset, text_window);
        block.source_bytes.insert(position, text_window.len());
        block.clipped |= text_window.len() < text.len();
        block.title = short(title, 512).into();
        block.first_seq = block.sources.get(0).expect("inserted source").seq;
        self.blocks
            .make_contiguous()
            .sort_by_key(|block| block.first_seq);
        self.trim();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rsi_agent_session_protocol::{
        AgentMessageContent, EffectId, InputMessageSource, MessageId, SessionFact, SessionFactBody,
        StepId, TurnId, TurnOutcome,
    };
    use rsi_ai_protocol::{ContentDelta, LanguageEvent};
    use rsi_conversation::{BlockIdentity, FactField};

    fn delta(seq: u64, text: &str) -> SessionFact {
        SessionFact::new(
            seq,
            1,
            SessionFactBody::ModelEvent {
                turn_id: TurnId::new("turn").unwrap(),
                effect_id: EffectId::new("model").unwrap(),
                event: LanguageEvent::ContentDelta {
                    index: 0,
                    delta: ContentDelta::Text(text.into()),
                },
            },
        )
        .unwrap()
    }
    #[test]
    fn missing_interior_delta_is_inserted_once_without_regressing_terminal_status() {
        let mut transcript = Transcript::default();
        for (seq, text) in [(3, "丙"), (1, "甲"), (2, "乙"), (3, "丙"), (2, "乙")] {
            transcript.fact(&delta(seq, text));
        }
        assert_eq!(transcript.blocks.len(), 1);
        assert_eq!(transcript.blocks[0].text, "甲乙丙");
        assert_eq!(transcript.blocks[0].sources.len(), 3);
        assert_eq!(transcript.seq, 3);
        transcript.fact(
            &SessionFact::new(
                5,
                1,
                SessionFactBody::TurnTerminal {
                    turn_id: TurnId::new("turn").unwrap(),
                    outcome: TurnOutcome::Completed,
                },
            )
            .unwrap(),
        );
        transcript.fact(
            &SessionFact::new(
                4,
                1,
                SessionFactBody::MessageTurnAccepted {
                    turn_id: TurnId::new("turn").unwrap(),
                    activation_id: rsi_agent_session_protocol::ActivationId::new("activation")
                        .unwrap(),
                    message_ids: vec![MessageId::new("input").unwrap()],
                    model: None,
                    sandbox: rsi_sandbox::SandboxMode::ReadOnly,
                    require_approval: false,
                },
            )
            .unwrap(),
        );
        assert_eq!(transcript.status, "Completed");
        assert!(transcript.active.is_none());
        assert_eq!(transcript.seq, 5);
    }

    #[test]
    fn direct_turn_and_claimed_message_cannot_collide_and_control_preview_reconciles() {
        let mut transcript = Transcript::default();
        let turn = TurnId::new("same").unwrap();
        let message = MessageId::new("same").unwrap();
        let entered = vec![AgentMessageContent::Text {
            text: "claimed input".into(),
        }];
        let key = BlockIdentity::Message { message: &message }.key();
        transcript.message(&key, "user", "You", &entered, None);
        transcript.fact(
            &SessionFact::new(
                2,
                1,
                SessionFactBody::InputMessageEntered {
                    turn_id: turn.clone(),
                    step_id: StepId::new("step").unwrap(),
                    source: InputMessageSource::Human {
                        message_id: message,
                    },
                    content: entered.clone(),
                },
            )
            .unwrap(),
        );
        transcript.message(&key, "user", "You", &entered, None);
        transcript.put_source(
            BlockIdentity::TurnInput { turn: &turn }.key(),
            "user",
            "You",
            SourceRef {
                seq: 1,
                field: FactField::TurnInput,
            },
            "direct input",
        );
        assert_eq!(transcript.blocks.len(), 2);
        assert_eq!(transcript.blocks[0].text, "direct input");
        assert_eq!(transcript.blocks[1].text, "claimed input");
        assert_eq!(transcript.blocks[1].sources.len(), 1);
    }

    #[test]
    fn span_eviction_forgets_only_removed_sources_and_bounds_unicode_text() {
        let mut transcript = Transcript::default();
        for seq in 1..=20_000 {
            transcript.fact(&delta(seq, "界"));
        }
        let block = &transcript.blocks[0];
        assert_eq!(block.sources.len(), MAXIMUM_BLOCK_SOURCES);
        assert_eq!(block.source_bytes.len(), block.sources.len());
        assert_eq!(block.text.len(), 3 * MAXIMUM_BLOCK_SOURCES);
        assert!(block.clipped);
        assert!(
            block
                .sources
                .position(SourceRef {
                    seq: 1,
                    field: FactField::ModelText
                })
                .is_none()
        );
        transcript.fact(&delta(1, "旧"));
        assert!(transcript.blocks[0].text.starts_with('旧'));
        assert_eq!(transcript.blocks[0].sources.get(0).unwrap().seq, 1);
        assert!(
            transcript.blocks[0]
                .sources
                .position(SourceRef {
                    seq: 20_000,
                    field: FactField::ModelText
                })
                .is_none()
        );
        transcript.fact(&delta(20_001, &"😀".repeat(MAX_BLOCK_BYTES)));
        let block = &transcript.blocks[0];
        assert_eq!(block.sources.len(), 1);
        assert_eq!(block.text.len(), MAX_BLOCK_BYTES);
        assert_eq!(block.source_bytes.iter().sum::<usize>(), block.text.len());
        assert!(block.text.chars().all(|character| character == '😀'));
    }

    #[test]
    fn metadata_pressure_evicts_old_blocks_before_the_text_budget_is_full() {
        let mut transcript = Transcript::default();
        for block in 0..40 {
            for span in 0..1024 {
                transcript.put_source(
                    format!("block-{block}"),
                    "assistant",
                    "Assistant",
                    SourceRef {
                        seq: block * 1024 + span + 1,
                        field: FactField::ModelText,
                    },
                    "界",
                );
            }
        }
        assert!(transcript.omitted);
        assert!(transcript.blocks.len() < 40);
        assert_eq!(transcript.blocks.back().unwrap().key, "block-39");
        assert!(
            transcript
                .blocks
                .iter()
                .map(|block| block.text.capacity())
                .sum::<usize>()
                < super::super::MAX_TEXT
        );
        assert!(
            transcript
                .blocks
                .iter()
                .all(|block| block.source_bytes.len() == block.sources.len())
        );
    }
}
