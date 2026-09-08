#[path = "spans.rs"]
mod spans;

use rsi_agent_session_protocol::{
    AgentControlRecordBody, AgentMessageContent, InputMessageSource, SessionFact, SessionFactBody,
    TurnId,
};
use rsi_agent_turn_protocol::SessionObservation;
use rsi_ai_protocol::{ContentDelta, LanguageEvent};
use rsi_conversation::{BlockIdentity, FactField, FieldWindow, SourceIndex, SourceRef, ToolState};
use rsi_tools_protocol::ToolContent;
use serde::Serialize;
use std::collections::VecDeque;

const MAX_BLOCKS: usize = 128;
const MAX_BLOCK_BYTES: usize = 128 * 1024;
const MAX_TEXT: usize = 1024 * 1024;
const MAX_METADATA: usize = 512 * 1024;

#[derive(Clone, Debug, Serialize)]
pub(crate) struct Block {
    pub key: String,
    pub role: &'static str,
    pub title: String,
    pub text: String,
    pub clipped: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool: Option<ToolState>,
    #[serde(skip)]
    tool_argument_bytes: usize,
    #[serde(skip)]
    sources: SourceIndex,
    #[serde(skip)]
    source_bytes: VecDeque<usize>,
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
                tool: None,
                tool_argument_bytes: 0,
                sources: SourceIndex::default(),
                source_bytes: VecDeque::new(),
                first_seq: self.seq,
            });
            self.blocks.len() - 1
        });
        let block = &mut self.blocks[index];
        if !block.sources.is_empty() {
            return;
        }
        block.title = short(title, 512).into();
        if !append {
            block.text.clear();
            block.clipped = false;
            block.first_seq = self.seq;
        }
        let available = MAX_BLOCK_BYTES.saturating_sub(block.text.len());
        block.text.push_str(short(text, available));
        block.clipped |= text.len() > available;
        self.trim();
    }
    fn trim(&mut self) {
        while self.blocks.len() > MAX_BLOCKS
            || self
                .blocks
                .iter()
                .map(|block| block.text.capacity())
                .sum::<usize>()
                > MAX_TEXT
            || self.blocks.capacity() * std::mem::size_of::<Block>()
                + self
                    .blocks
                    .iter()
                    .map(|block| {
                        block.key.capacity()
                            + block.title.capacity()
                            + block.tool.as_ref().map_or(0, ToolState::owned_bytes)
                            + block.sources.owned_bytes()
                            + block.source_bytes.capacity() * std::mem::size_of::<usize>()
                    })
                    .sum::<usize>()
                > MAX_METADATA
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
        seq: Option<u64>,
    ) {
        for (index, item) in content.iter().enumerate() {
            let text = match item {
                AgentMessageContent::Text { text } => text.as_str(),
                AgentMessageContent::Image { .. } => "[Image]",
            };
            let key = format!("{key}:{index}");
            if let Some(seq) = seq {
                let index = u16::try_from(index).expect("validated input content index");
                let field = match item {
                    AgentMessageContent::Text { .. } => FactField::InputText { index },
                    AgentMessageContent::Image { .. } => FactField::InputImage { index },
                };
                self.put_source(key, role, title, SourceRef { seq, field }, text);
            } else {
                self.add(key, role, title, text, false);
            }
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
                        &BlockIdentity::Message {
                            message: &message.message_id,
                        }
                        .key(),
                        if human { "user" } else { "status" },
                        if human { "You" } else { "Agent message" },
                        &message.content,
                        None,
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
    fn project_tool(&mut self, fact: &SessionFact) {
        let key = BlockIdentity::tool(fact).expect("Tool Fact").key();
        if !self.blocks.iter().any(|block| block.key == key) {
            self.add(key.clone(), "tool", "Tool", "", false);
        }
        let Some(block) = self.blocks.iter_mut().find(|block| block.key == key) else {
            return;
        };
        let old_arguments = block.tool.as_ref().and_then(|tool| tool.arguments);
        let old_result = block.tool.as_ref().and_then(|tool| tool.result);
        let tool = if let Some(tool) = &mut block.tool {
            tool.observe(fact);
            tool
        } else {
            block
                .tool
                .insert(ToolState::from_fact(fact).expect("Tool Fact"))
        };
        block.title = tool.title();
        block.first_seq = block.first_seq.min(fact.seq());
        if tool.arguments != old_arguments {
            let arguments = match fact.body() {
                SessionFactBody::ToolIntent { arguments, .. }
                | SessionFactBody::ToolRejected { arguments, .. } => Some(arguments),
                _ => None,
            };
            if let Some(arguments) = arguments {
                let window = FieldWindow::json(arguments, 0, MAX_BLOCK_BYTES / 2 - 2)
                    .expect("bounded arguments");
                let prefix = format!("{}\n\n", window.text);
                block
                    .text
                    .replace_range(..block.tool_argument_bytes, &prefix);
                block.tool_argument_bytes = prefix.len();
                block.clipped |= window.more;
            }
        }
        if tool.result != old_result
            && let SessionFactBody::ToolResult { result, .. } = fact.body()
        {
            block.text.truncate(block.tool_argument_bytes);
            let maximum = MAX_BLOCK_BYTES / 2;
            let mut remaining = maximum;
            let mut has_text = false;
            for content in &result.content {
                if let ToolContent::Text { text } = content {
                    has_text = true;
                    let copied = short(text, remaining);
                    block.text.push_str(copied);
                    block.clipped |= copied.len() < text.len();
                    remaining -= copied.len();
                }
            }
            if !has_text {
                let window = FieldWindow::json(&result.value, 0, maximum).expect("bounded result");
                block.text.push_str(&window.text);
                block.clipped |= window.more;
            }
        }
        self.blocks
            .make_contiguous()
            .sort_by_key(|block| block.first_seq);
        self.trim();
    }

    #[allow(clippy::too_many_lines)] // One closed Fact-to-block projection preserves its common sequence fence.
    pub fn fact(&mut self, fact: &SessionFact) {
        if BlockIdentity::tool(fact).is_some() {
            self.seq = self.seq.max(fact.seq());
            self.project_tool(fact);
            return;
        }
        let latest = fact.seq() > self.seq;
        self.seq = self.seq.max(fact.seq());
        let source = |field| SourceRef {
            seq: fact.seq(),
            field,
        };
        match fact.body() {
            SessionFactBody::TurnAccepted { turn_id, text, .. } => {
                if latest {
                    self.active = Some(turn_id.clone());
                    self.status = "Running".into();
                }
                self.put_source(
                    BlockIdentity::TurnInput { turn: turn_id }.key(),
                    "user",
                    "You",
                    source(FactField::TurnInput),
                    text,
                );
            }
            SessionFactBody::MessageTurnAccepted { turn_id, .. } => {
                if latest {
                    self.active = Some(turn_id.clone());
                    self.status = "Running".into();
                }
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
                self.message(
                    &BlockIdentity::Message { message: id }.key(),
                    role,
                    title,
                    content,
                    Some(fact.seq()),
                );
            }
            SessionFactBody::ModelEvent {
                turn_id,
                effect_id,
                event: LanguageEvent::ContentDelta { index, delta },
                ..
            } => {
                let (role, title, text, field) = match delta {
                    ContentDelta::Text(text) => {
                        ("assistant", "Assistant", text, FactField::ModelText)
                    }
                    ContentDelta::Reasoning(text) => {
                        ("reasoning", "Reasoning", text, FactField::ModelReasoning)
                    }
                    ContentDelta::ToolArguments(_) => return,
                };
                self.put_source(
                    BlockIdentity::Model {
                        turn: turn_id,
                        effect: effect_id,
                        index: *index,
                    }
                    .key(),
                    role,
                    title,
                    source(field),
                    text,
                );
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
                if latest && self.active.as_ref().is_none_or(|active| active == turn_id) {
                    self.active = None;
                    self.status = status.into();
                }
                self.put_source(
                    BlockIdentity::Terminal { turn: turn_id }.key(),
                    "status",
                    status,
                    source(FactField::TurnOutcome),
                    &detail,
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
    fn completed_tool_keeps_name_arguments_and_both_sources() {
        use rsi_agent_session_protocol::EffectId;
        use rsi_tools_protocol::{ToolResult, ToolResultIdentity};
        for (exit_code, expected_status, command) in [
            (0, "completed", "cargo test".to_owned()),
            (7, "command failed", "cargo test".to_owned()),
            (
                0,
                "completed",
                format!("cargo test {}", "界".repeat(MAX_BLOCK_BYTES / 3)),
            ),
        ] {
            let identity =
                ToolResultIdentity::new("owner", "invoke", "call", "a".repeat(64)).unwrap();
            let intent = SessionFact::new(
                10,
                1,
                SessionFactBody::ToolIntent {
                    turn_id: TurnId::new("turn").unwrap(),
                    effect_id: EffectId::new("effect").unwrap(),
                    identity: identity.clone(),
                    name: "bash".into(),
                    arguments: serde_json::json!({"command": command}),
                    approval: None,
                    parallel_safe: false,
                },
            )
            .unwrap();
            let result = SessionFact::new(
                12,
                1,
                SessionFactBody::ToolResult {
                    turn_id: TurnId::new("turn").unwrap(),
                    effect_id: EffectId::new("effect").unwrap(),
                    identity,
                    result: ToolResult::new(
                        serde_json::json!({"exit_code": exit_code}),
                        vec![ToolContent::Text {
                            text: "test output".into(),
                        }],
                        false,
                    )
                    .unwrap(),
                },
            )
            .unwrap();
            let mut transcript = Transcript::default();
            transcript.fact(&intent);
            transcript.fact(&result);
            transcript.fact(&result);
            let block = &transcript.blocks[0];
            assert_eq!(transcript.blocks.len(), 1);
            assert_eq!(block.title, format!("bash · {expected_status}"));
            assert!(block.text.contains("cargo test"));
            assert!(block.text.contains("test output"));
            assert!(block.text.len() <= MAX_BLOCK_BYTES);
            assert_eq!(block.clipped, command.len() > MAX_BLOCK_BYTES / 2);
            assert_eq!(transcript.history_before(), Some(10));
            let view = serde_json::to_value(block).unwrap();
            assert_eq!(view["tool"]["arguments"]["seq"], "10");
            assert_eq!(view["tool"]["result"]["seq"], "12");

            let mut suffix = Transcript::default();
            suffix.fact(&result);
            let suffix = serde_json::to_value(&suffix.blocks[0]).unwrap();
            assert!(suffix["tool"]["name"].is_null());
            assert!(suffix["tool"]["arguments"].is_null());
            assert_eq!(suffix["tool"]["result"]["seq"], "12");
            let mut recovered = Transcript::default();
            recovered.fact(&result);
            recovered.fact(&intent);
            recovered.fact(&intent);
            assert_eq!(recovered.blocks.len(), 1);
            assert_eq!(recovered.blocks[0].title, block.title);
            assert_eq!(recovered.blocks[0].text, block.text);
            assert_eq!(recovered.blocks[0].first_seq, 10);
        }
    }

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
                .filter(|block| block.key
                    == BlockIdentity::Terminal {
                        turn: &TurnId::new("turn").unwrap()
                    }
                    .key())
                .count(),
            1
        );
    }
}
