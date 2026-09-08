use rsi_agent_session_protocol::{
    AgentControlRecordBody, AgentMessageContent, InputMessageSource, SessionFact, SessionFactBody,
    TurnId,
};
use rsi_agent_turn_protocol::SessionObservation;
use rsi_ai_protocol::{ContentDelta, LanguageEvent};
use rsi_tools_protocol::ToolContent;
use serde::Serialize;
use std::collections::VecDeque;

const MAX_BLOCKS: usize = 128;
const MAX_BLOCK_BYTES: usize = 128 * 1024;
const MAX_TEXT: usize = 1024 * 1024;

#[derive(Clone, Debug, Serialize)]
pub(crate) struct Block {
    pub key: String,
    pub role: &'static str,
    pub title: String,
    pub text: String,
    pub clipped: bool,
    #[serde(skip)]
    first_seq: u64,
}
#[derive(Clone, Debug, Default, Serialize)]
pub(crate) struct Transcript {
    pub blocks: VecDeque<Block>,
    pub omitted: bool,
    pub active: Option<TurnId>,
    pub status: String,
    #[serde(skip)]
    pub seq: u64,
}
impl Transcript {
    pub fn history_before(&self) -> Option<u64> {
        self.blocks
            .iter()
            .map(|block| block.first_seq)
            .filter(|seq| *seq != 0)
            .min()
            .or_else(|| (self.seq != 0).then(|| self.seq.saturating_add(1)))
    }
    fn add(&mut self, key: String, role: &'static str, title: &str, text: &str, append: bool) {
        let index = self.blocks.iter().position(|block| block.key == key);
        let index = index.unwrap_or_else(|| {
            self.blocks.push_back(Block {
                key,
                role,
                title: short(title, 512).into(),
                text: String::new(),
                clipped: false,
                first_seq: self.seq,
            });
            self.blocks.len() - 1
        });
        let block = &mut self.blocks[index];
        block.title = short(title, 512).into();
        if !append {
            block.text.clear();
            block.clipped = false;
            block.first_seq = self.seq;
        }
        let available = MAX_BLOCK_BYTES.saturating_sub(block.text.len());
        block.text.push_str(short(text, available));
        block.clipped |= text.len() > available;
        while self.blocks.len() > MAX_BLOCKS
            || self
                .blocks
                .iter()
                .map(|block| block.text.len())
                .sum::<usize>()
                > MAX_TEXT
        {
            self.blocks.pop_front();
            self.omitted = true;
        }
    }
    fn message(
        &mut self,
        key: &str,
        role: &'static str,
        title: &str,
        content: &[AgentMessageContent],
    ) {
        for (index, item) in content.iter().enumerate() {
            let text = match item {
                AgentMessageContent::Text { text } => text.as_str(),
                AgentMessageContent::Image { .. } => "[Image]",
            };
            self.add(format!("{key}:{index}"), role, title, text, false);
        }
    }
    pub fn observation(&mut self, update: &SessionObservation) {
        match update {
            SessionObservation::Fact { fact, .. } => self.fact(fact),
            SessionObservation::Control { record, .. } => match record.body() {
                AgentControlRecordBody::MessageAccepted { message, .. } => {
                    let human = matches!(
                        message.source,
                        rsi_agent_session_protocol::AgentMessageSource::Human
                    );
                    self.message(
                        &format!("input:{}", message.message_id),
                        if human { "user" } else { "status" },
                        if human { "You" } else { "Agent message" },
                        &message.content,
                    );
                }
                AgentControlRecordBody::MessageDiscarded { message_id, reason } => {
                    self.add(
                        format!("discard:{message_id}"),
                        "status",
                        "Input discarded",
                        &format!("{reason:?}"),
                        false,
                    );
                }
                _ => {}
            },
        }
    }
    #[allow(clippy::too_many_lines)] // One closed Fact-to-block projection preserves its common sequence fence.
    pub fn fact(&mut self, fact: &SessionFact) {
        if fact.seq() <= self.seq {
            return;
        }
        self.seq = fact.seq();
        match fact.body() {
            SessionFactBody::TurnAccepted { turn_id, text, .. } => {
                self.active = Some(turn_id.clone());
                self.status = "Running".into();
                self.add(format!("input:{turn_id}"), "user", "You", text, false);
            }
            SessionFactBody::MessageTurnAccepted { turn_id, .. } => {
                self.active = Some(turn_id.clone());
                self.status = "Running".into();
            }
            SessionFactBody::InputMessageEntered {
                source, content, ..
            } => {
                let (id, role, title) = match source {
                    InputMessageSource::Human { message_id } => (message_id, "user", "You"),
                    InputMessageSource::Agent { message_id, .. }
                    | InputMessageSource::Completion { message_id, .. } => {
                        (message_id, "status", "Agent message")
                    }
                    _ => return,
                };
                self.message(&format!("input:{id}"), role, title, content);
            }
            SessionFactBody::ModelEvent {
                effect_id,
                event: LanguageEvent::ContentDelta { index, delta },
                ..
            } => {
                let (role, title, text) = match delta {
                    ContentDelta::Text(text) => ("assistant", "Assistant", text),
                    ContentDelta::Reasoning(text) => ("reasoning", "Reasoning", text),
                    ContentDelta::ToolArguments(_) => return,
                };
                self.add(
                    format!("model:{effect_id}:{index}"),
                    role,
                    title,
                    text,
                    true,
                );
            }
            SessionFactBody::ToolIntent {
                effect_id,
                name,
                arguments,
                ..
            } => {
                self.add(
                    format!("tool:{effect_id}"),
                    "tool",
                    &format!("{name} · running"),
                    &arguments.to_string(),
                    false,
                );
            }
            SessionFactBody::ToolResult {
                effect_id, result, ..
            } => {
                let failed = result.is_error
                    || result
                        .value
                        .get("exit_code")
                        .and_then(serde_json::Value::as_i64)
                        .is_some_and(|code| code != 0)
                    || result
                        .value
                        .get("signal")
                        .is_some_and(|value| !value.is_null());
                let title = if failed {
                    "Tool · failed"
                } else {
                    "Tool · completed"
                };
                let mut first = true;
                for content in &result.content {
                    if let ToolContent::Text { text } = content {
                        self.add(format!("tool:{effect_id}"), "tool", title, text, !first);
                        first = false;
                    }
                }
                if first {
                    self.add(
                        format!("tool:{effect_id}"),
                        "tool",
                        title,
                        &result.value.to_string(),
                        false,
                    );
                }
            }
            SessionFactBody::TurnTerminal { turn_id, outcome } => {
                let (status, detail) = match outcome {
                    rsi_agent_session_protocol::TurnOutcome::Completed => {
                        ("Completed", String::new())
                    }
                    rsi_agent_session_protocol::TurnOutcome::Cancelled => {
                        ("Cancelled", String::new())
                    }
                    rsi_agent_session_protocol::TurnOutcome::Failed { message, .. } => {
                        ("Failed", short(message, 4096).into())
                    }
                    rsi_agent_session_protocol::TurnOutcome::PartialFailed { message, .. } => {
                        ("Partially failed", short(message, 4096).into())
                    }
                    rsi_agent_session_protocol::TurnOutcome::Interrupted { reason, .. } => {
                        ("Interrupted", short(reason, 4096).into())
                    }
                    rsi_agent_session_protocol::TurnOutcome::BudgetExceeded {
                        consumed,
                        limit,
                        ..
                    } => ("Budget exceeded", format!("{consumed} / {limit}")),
                };
                if self.active.as_ref().is_none_or(|active| active == turn_id) {
                    self.active = None;
                    self.status = status.into();
                }
                self.add(
                    format!("terminal:{turn_id}"),
                    "status",
                    status,
                    &detail,
                    false,
                );
            }
            _ => {}
        }
    }
}
pub(crate) fn short(text: &str, maximum: usize) -> &str {
    let mut end = text.len().min(maximum);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    &text[..end]
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn projection_bounds_utf8_replacement_and_aggregate_history_without_losing_terminal_status() {
        let mut transcript = Transcript::default();
        transcript.add(
            "first".into(),
            "assistant",
            "Answer",
            &"界".repeat(MAX_BLOCK_BYTES),
            false,
        );
        assert!(transcript.blocks[0].clipped);
        assert!(transcript.blocks[0].text.len() <= MAX_BLOCK_BYTES);
        transcript.add("first".into(), "assistant", "Answer", "replacement", false);
        assert_eq!(transcript.blocks.len(), 1);
        assert_eq!(transcript.blocks[0].text, "replacement");
        assert!(!transcript.blocks[0].clipped);
        for index in 0..200 {
            transcript.add(
                index.to_string(),
                "tool",
                "Result",
                &"x".repeat(MAX_BLOCK_BYTES),
                false,
            );
        }
        assert!(transcript.omitted);
        assert!(transcript.blocks.len() <= MAX_BLOCKS);
        assert!(
            transcript
                .blocks
                .iter()
                .map(|block| block.text.len())
                .sum::<usize>()
                <= MAX_TEXT
        );
        let terminal = SessionFact::new(
            2,
            1,
            SessionFactBody::TurnTerminal {
                turn_id: TurnId::new("turn").unwrap(),
                outcome: rsi_agent_session_protocol::TurnOutcome::Cancelled,
            },
        )
        .unwrap();
        transcript.fact(&terminal);
        transcript.fact(&terminal);
        assert_eq!(transcript.status, "Cancelled");
        assert_eq!(
            transcript
                .blocks
                .iter()
                .filter(|block| block.key == "terminal:turn")
                .count(),
            1
        );
    }
}
