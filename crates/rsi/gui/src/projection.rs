#[path = "spans.rs"]
mod spans;
#[path = "turns.rs"]
mod turns;

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

#[derive(Clone, Debug)]
pub(crate) struct Block {
    pub(crate) revision: std::sync::Arc<()>,
    pub key: String,
    message_id: Option<rsi_agent_session_protocol::MessageId>,
    pub role: &'static str,
    pub title: String,
    pub text: String,
    pub clipped: bool,
    pub tool: Option<ToolState>,
    request: Option<rsi_conversation::RequestPresentation>,
    tool_start_seq: Option<u64>,
    tool_argument_bytes: usize,
    sources: SourceIndex,
    source_bytes: VecDeque<usize>,
    first_seq: u64,
    markdown: std::sync::OnceLock<Option<Vec<crate::markdown::Node>>>,
    #[cfg(test)]
    markdown_parses: std::sync::Arc<std::sync::atomic::AtomicUsize>,
}
impl Serialize for Block {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeStruct;
        let markdown = (self.role == "assistant")
            .then(|| {
                self.markdown
                    .get_or_init(|| {
                        #[cfg(test)]
                        self.markdown_parses
                            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                        crate::markdown::parse(&self.text)
                    })
                    .as_ref()
            })
            .flatten();
        let mut view = serializer.serialize_struct(
            "Block",
            7 + usize::from(self.tool.is_some()) + usize::from(markdown.is_some()),
        )?;
        view.serialize_field("key", &self.key)?;
        view.serialize_field("role", &self.role)?;
        view.serialize_field("title", &self.title)?;
        view.serialize_field("text", &self.text)?;
        view.serialize_field("clipped", &self.clipped)?;
        view.serialize_field("sources", &self.sources.len())?;
        view.serialize_field(
            "external",
            &self
                .tool
                .as_ref()
                .and_then(ToolState::external_conversation),
        )?;
        if let Some(tool) = &self.tool {
            view.serialize_field("tool", tool)?;
        }
        if let Some(markdown) = markdown {
            view.serialize_field("markdown", &markdown)?;
        }
        view.end()
    }
}
impl Block {
    fn refresh_tool_sources(&mut self, fact: &SessionFact) {
        let tool = self.tool.as_ref().expect("Tool block");
        let mut sources = SourceIndex::default();
        for source in [tool.arguments, tool.result, tool.rejection]
            .into_iter()
            .flatten()
        {
            sources.insert(source).expect("bounded Tool provenance");
        }
        // Preserve current result content sources when backfilling its older intent.
        for source in self.sources.iter().filter(|source| {
            matches!(
                source.field,
                FactField::ToolText { .. } | FactField::ToolImage { .. }
            ) && Some(source.seq) == tool.result.map(|source| source.seq)
        }) {
            sources.insert(source).expect("bounded Tool content");
        }
        if let SessionFactBody::ToolResult { result, .. } = fact.body()
            && tool.result.is_some_and(|source| source.seq == fact.seq())
        {
            for (index, content) in result.content.iter().enumerate() {
                let index = u16::try_from(index).expect("validated Tool content index");
                let field = match content {
                    ToolContent::Text { .. } => FactField::ToolText { index },
                    ToolContent::Image { .. } => FactField::ToolImage { index },
                };
                sources
                    .insert(SourceRef {
                        seq: fact.seq(),
                        field,
                    })
                    .expect("bounded Tool content");
            }
        }
        self.sources = sources;
    }

    pub(crate) fn sources(&self) -> SourceIndex {
        self.sources.clone()
    }
}

#[derive(Clone, Debug, Default)]
pub(crate) struct Transcript {
    pub queue: rsi_client::QueueProjection,
    pub turns: turns::Index,
    pub blocks: VecDeque<Block>,
    pub omitted: bool,
    pub active: Option<TurnId>,
    pub status: String,
    pub seq: u64,
}

#[derive(Serialize)]
pub(crate) struct TranscriptView<'a, B, T> {
    blocks: B,
    turns: T,
    omitted: bool,
    active: &'a Option<TurnId>,
    status: &'a str,
}
impl Serialize for Transcript {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self.view(&self.blocks, &self.turns.view)
            .serialize(serializer)
    }
}
impl Transcript {
    pub(crate) fn view<B, T>(&self, blocks: B, turns: T) -> TranscriptView<'_, B, T> {
        TranscriptView {
            blocks,
            turns,
            omitted: self.omitted,
            active: &self.active,
            status: &self.status,
        }
    }
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
        let existed = index.is_some();
        let index = index.unwrap_or_else(|| {
            self.blocks.push_back(Block {
                revision: std::sync::Arc::new(()),
                key,
                message_id: None,
                role,
                title: short(title, 512).into(),
                text: String::new(),
                clipped: false,
                tool: None,
                request: None,
                tool_start_seq: None,
                tool_argument_bytes: 0,
                sources: SourceIndex::default(),
                source_bytes: VecDeque::new(),
                first_seq: self.seq,
                markdown: std::sync::OnceLock::new(),
                #[cfg(test)]
                markdown_parses: std::sync::Arc::default(),
            });
            self.blocks.len() - 1
        });
        let block = &mut self.blocks[index];
        if !block.sources.is_empty() {
            return;
        }
        let title = short(title, 512);
        let available = if append {
            MAX_BLOCK_BYTES.saturating_sub(block.text.len())
        } else {
            MAX_BLOCK_BYTES
        };
        let copied = short(text, available);
        let clipped = (append && block.clipped) || text.len() > available;
        if existed
            && block.title == title
            && block.clipped == clipped
            && if append {
                copied.is_empty()
            } else {
                block.text == copied
            }
        {
            return;
        }
        block.markdown.take();
        block.revision = std::sync::Arc::new(());
        block.title = title.into();
        if !append {
            block.text.clear();
            block.first_seq = self.seq;
        }
        block.text.push_str(copied);
        block.clipped = clipped;
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
                            + block
                                .request
                                .as_ref()
                                .map_or(0, rsi_conversation::RequestPresentation::owned_bytes)
                            + block.sources.owned_bytes()
                            + block.source_bytes.capacity() * std::mem::size_of::<usize>()
                    })
                    .sum::<usize>()
                > MAX_METADATA
        {
            if self.blocks.pop_front().is_none() {
                // Empty retained allocation must not keep the metadata loop over budget.
                self.blocks.shrink_to_fit();
                break;
            }
            self.omitted = true;
        }
    }
    fn message(
        &mut self,
        key: &str,
        role: &'static str,
        title: &str,
        content: &[AgentMessageContent],
        fact: Option<&SessionFact>,
    ) {
        for (index, item) in content.iter().enumerate() {
            let text = match item {
                AgentMessageContent::Text { text } => text.as_str(),
                AgentMessageContent::Image { .. } => "[Image]",
                AgentMessageContent::Reference { reference } => reference.preview.as_str(),
            };
            let key = format!("{key}:{index}");
            if let Some(fact) = fact {
                let seq = fact.seq();
                let index = u16::try_from(index).expect("validated input content index");
                let field = match item {
                    AgentMessageContent::Text { .. } => FactField::InputText { index },
                    AgentMessageContent::Image { .. } => FactField::InputImage { index },
                    AgentMessageContent::Reference { .. } => FactField::InputReference { index },
                };
                let source = SourceRef { seq, field };
                let image = rsi_conversation::MediaSource::select(fact, source)
                    .map(rsi_conversation::MediaSource::label);
                self.put_source(key, role, title, source, image.as_deref().unwrap_or(text));
            } else {
                self.add(key, role, title, text, false);
            }
        }
    }
    fn message_key(&self, id: &rsi_agent_session_protocol::MessageId) -> String {
        self.blocks
            .iter()
            .find(|block| block.message_id.as_ref() == Some(id))
            .and_then(|block| block.key.rsplit_once(':').map(|(key, _)| key.to_owned()))
            .unwrap_or_else(|| BlockIdentity::Message { message: id }.key())
    }
    fn tag_message(&mut self, key: &str, id: &rsi_agent_session_protocol::MessageId) {
        for block in &mut self.blocks {
            if block
                .key
                .rsplit_once(':')
                .is_some_and(|(prefix, _)| prefix == key)
            {
                block.message_id = Some(id.clone());
            }
        }
    }
    pub fn observation(&mut self, update: &SessionObservation) {
        if let SessionObservation::Control { record, .. } = update {
            self.queue.observe(record);
        }
        match update {
            SessionObservation::Fact { fact, .. } => self.fact(fact),
            SessionObservation::Control { record, .. } => match record.body() {
                AgentControlRecordBody::MessageSuccessor {
                    predecessor_id,
                    successor_id,
                    ..
                } => {
                    for block in &mut self.blocks {
                        if block.message_id.as_ref() == Some(predecessor_id) {
                            block.message_id = Some(successor_id.clone());
                        }
                    }
                }
                AgentControlRecordBody::MessageAccepted { message, .. } => {
                    let human = matches!(
                        message.source,
                        rsi_agent_session_protocol::AgentMessageSource::Human
                    );
                    let key = self.message_key(&message.message_id);
                    self.blocks.retain(|block| {
                        block.message_id.as_ref() != Some(&message.message_id)
                            || block
                                .key
                                .rsplit_once(':')
                                .and_then(|(_, index)| index.parse::<usize>().ok())
                                .is_some_and(|index| index < message.content.len())
                    });
                    self.message(
                        &key,
                        if human { "user" } else { "status" },
                        if human { "You" } else { "Agent message" },
                        &message.content,
                        None,
                    );
                    self.tag_message(&key, &message.message_id);
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
        if matches!(update, SessionObservation::Control { .. }) {
            self.turns.retain(&self.blocks, self.active.as_ref());
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
        if block.sources.iter().any(|source| source.seq == fact.seq())
            || block.tool_start_seq == Some(fact.seq())
        {
            return;
        }
        block.revision = std::sync::Arc::new(());
        if matches!(fact.body(), SessionFactBody::ToolStarted { .. }) {
            block.tool_start_seq = Some(fact.seq());
        }
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
            for (index, content) in result.content.iter().enumerate() {
                let source = SourceRef {
                    seq: fact.seq(),
                    field: FactField::ToolImage {
                        index: u16::try_from(index).expect("validated Tool content index"),
                    },
                };
                let image = rsi_conversation::MediaSource::select(fact, source)
                    .map(rsi_conversation::MediaSource::label);
                let text = match content {
                    ToolContent::Text { text } => text.as_str(),
                    ToolContent::Image { .. } => image.as_deref().expect("image content"),
                };
                {
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
        block.refresh_tool_sources(fact);
        self.blocks
            .make_contiguous()
            .sort_by_key(|block| block.first_seq);
        self.trim();
    }

    #[allow(clippy::too_many_lines)] // One closed Fact-to-block projection preserves its common sequence fence.
    pub fn fact(&mut self, fact: &SessionFact) {
        self.fact_blocks(fact);
        self.turns.observe(fact, &self.blocks);
    }
    #[allow(clippy::too_many_lines)] // Closed Fact-to-block projection with one shared sequence fence.
    fn fact_blocks(&mut self, fact: &SessionFact) {
        self.project_request(fact);
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
                let agent_title = match source {
                    InputMessageSource::Agent {
                        source_session_id, ..
                    } => format!("Message from {source_session_id}"),
                    InputMessageSource::Completion {
                        child_session_id, ..
                    } => format!("Completion from {child_session_id}"),
                    _ => String::new(),
                };
                let (id, role, title) = match source {
                    InputMessageSource::Human { message_id } => (message_id, "user", "You"),
                    InputMessageSource::Continuation { message_id, .. } => {
                        (message_id, "status", "Goal continuation")
                    }
                    InputMessageSource::Agent { message_id, .. }
                    | InputMessageSource::Completion { message_id, .. } => {
                        (message_id, "status", agent_title.as_str())
                    }
                    _ => return,
                };
                let key = self.message_key(id);
                self.message(&key, role, title, content, Some(fact));
                self.tag_message(&key, id);
            }
            SessionFactBody::ImageOutput {
                turn_id,
                effect_id,
                index,
                ..
            } => {
                let source = source(FactField::ImageOutput);
                let image =
                    rsi_conversation::MediaSource::select(fact, source).expect("image Fact");
                self.put_source(
                    BlockIdentity::Image {
                        turn: turn_id,
                        effect: effect_id,
                        index: *index,
                    }
                    .key(),
                    "assistant",
                    "Image",
                    source,
                    &image.label(),
                );
            }
            SessionFactBody::ModelEvent {
                turn_id,
                effect_id,
                event: LanguageEvent::ContentDelta { index, delta },
                purpose,
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
                let (role, title) = if *purpose
                    == rsi_agent_session_protocol::ModelEventPurpose::ContextCompaction
                {
                    ("status", "Context compaction")
                } else {
                    (role, title)
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
            SessionFactBody::TurnTerminal {
                turn_id, outcome, ..
            } => {
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

impl Transcript {
    fn project_request(&mut self, fact: &SessionFact) {
        let (SessionFactBody::ModelIntent {
            turn_id: turn,
            effect_id: effect,
            ..
        }
        | SessionFactBody::ModelStarted {
            turn_id: turn,
            effect_id: effect,
        }
        | SessionFactBody::ModelEvent {
            turn_id: turn,
            effect_id: effect,
            event:
                LanguageEvent::Usage { .. }
                | LanguageEvent::Finished { .. }
                | LanguageEvent::Failed { .. },
            ..
        }) = fact.body()
        else {
            return;
        };
        let key = serde_json::to_string(&("request", turn, effect)).expect("typed identities");
        let existing = self.blocks.iter().any(|block| block.key == key);
        if !existing {
            self.add(key.clone(), "metadata", "Request", "", false);
        }
        if let Some(block) = self.blocks.iter_mut().find(|block| block.key == key) {
            let request = block.request.get_or_insert_with(Default::default);
            request.observe(fact);
            let title = request.title();
            if block.title != title {
                block.title = title;
                block.revision = std::sync::Arc::new(());
            }
            block.first_seq = if !existing || block.first_seq == 0 {
                fact.seq()
            } else {
                block.first_seq.min(fact.seq())
            };
        }
        self.blocks
            .make_contiguous()
            .sort_by_key(|block| block.first_seq);
        self.trim();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trimming_releases_an_empty_allocation_larger_than_the_metadata_budget() {
        let mut transcript = Transcript::default();
        transcript
            .blocks
            .reserve(MAX_METADATA / std::mem::size_of::<Block>() + 1);
        assert!(transcript.blocks.capacity() * std::mem::size_of::<Block>() > MAX_METADATA);
        transcript.trim();
        assert!(transcript.blocks.is_empty());
        assert_eq!(transcript.blocks.capacity(), 0);
    }

    #[test]
    fn queue_successor_updates_one_user_block_and_claim_adds_sources_without_duplicates() {
        use rsi_agent_session_protocol::{
            AgentControlRecord, AgentMessage, AgentMessageSource, MessageDelivery, MessageId,
            MessageOptions, MessageTarget, QueueSlot, SessionId, StepId,
        };
        let mut transcript = Transcript::default();
        let original = MessageId::new("original").unwrap();
        let successor = MessageId::new("successor").unwrap();
        let session = SessionId::new("session").unwrap();
        let mut message = AgentMessage {
            message_id: original.clone(),
            source: AgentMessageSource::Human,
            content: vec![
                AgentMessageContent::Text {
                    text: "old first".into(),
                },
                AgentMessageContent::Text {
                    text: "old second".into(),
                },
            ],
            options: MessageOptions::default(),
        };
        let acceptance = |message| AgentControlRecordBody::MessageAccepted {
            message,
            delivery: MessageDelivery::NextTurn,
            bound_turn_id: None,
            root_session_id: session.clone(),
            target: MessageTarget::NextTurn,
            wake_required: true,
        };
        let observe = |transcript: &mut Transcript, seq, body| {
            let record = AgentControlRecord::new(seq, seq, body).unwrap();
            let retention = rsi_agent_turn_protocol::ObservationRetention::default();
            let record = retention
                .retain_controls(vec![std::sync::Arc::new(record)])
                .unwrap()
                .pop()
                .unwrap();
            transcript.observation(&SessionObservation::Control {
                record,
                durable_control_seq: seq,
            });
        };
        observe(&mut transcript, 1, acceptance(message.clone()));
        let key = transcript.blocks[0].key.clone();
        observe(
            &mut transcript,
            2,
            AgentControlRecordBody::MessageSuccessor {
                predecessor_id: original.clone(),
                successor_id: successor.clone(),
                slot: QueueSlot::initial(&original, 1, 1),
            },
        );
        message.message_id = successor.clone();
        message.content = vec![AgentMessageContent::Text {
            text: "new content".into(),
        }];
        observe(&mut transcript, 3, acceptance(message.clone()));
        assert_eq!(transcript.blocks.len(), 1);
        assert_eq!(transcript.blocks[0].key, key);
        assert_eq!(transcript.blocks[0].text, "new content");
        let queue = transcript.queue.view(None);
        assert_eq!(queue.len(), 1);
        assert_eq!(queue[0].message.queue_slot.id.as_str(), "original");
        assert_eq!(queue[0].message.message_id, successor);
        let fact = SessionFact::new(
            1,
            4,
            SessionFactBody::InputMessageEntered {
                turn_id: TurnId::new("turn").unwrap(),
                step_id: StepId::new("step").unwrap(),
                source: InputMessageSource::Human {
                    message_id: successor,
                },
                content: message.content,
            },
        )
        .unwrap();
        transcript.fact(&fact);
        assert_eq!(transcript.blocks.len(), 1);
        assert_eq!(transcript.blocks[0].key, key);
        assert_eq!(transcript.blocks[0].sources.len(), 1);
    }

    #[test]
    #[allow(clippy::too_many_lines)] // Three Tool outcomes share lifecycle, revision, and backfill assertions.
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
                    origin: rsi_agent_session_protocol::ToolOrigin::Model {
                        effect_id: EffectId::new("source-model").unwrap(),
                    },

                    program_role: rsi_tools_protocol::ToolProgramRole::Unavailable,
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
            let started = SessionFact::new(
                11,
                1,
                SessionFactBody::ToolStarted {
                    turn_id: TurnId::new("turn").unwrap(),
                    effect_id: EffectId::new("effect").unwrap(),
                    identity: identity.clone(),
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
                    conclusion: None,
                },
            )
            .unwrap();
            let mut transcript = Transcript::default();
            transcript.fact(&intent);
            let prepared_revision = transcript.blocks[0].revision.clone();
            let prepared_text = transcript.blocks[0].text.clone();
            transcript.fact(&started);
            assert!(!std::sync::Arc::ptr_eq(
                &prepared_revision,
                &transcript.blocks[0].revision
            ));
            assert!(
                transcript.blocks[0]
                    .title
                    .starts_with("bash · running · cargo test")
            );
            assert_eq!(transcript.blocks[0].text, prepared_text);
            let running_revision = transcript.blocks[0].revision.clone();
            transcript.fact(&started);
            assert!(std::sync::Arc::ptr_eq(
                &running_revision,
                &transcript.blocks[0].revision
            ));
            transcript.fact(&result);
            let completed_revision = transcript.blocks[0].revision.clone();
            transcript.fact(&result);
            assert!(std::sync::Arc::ptr_eq(
                &completed_revision,
                &transcript.blocks[0].revision
            ));
            let block = &transcript.blocks[0];
            assert_eq!(transcript.blocks.len(), 1);
            assert!(
                block
                    .title
                    .starts_with(&format!("bash · {expected_status} · cargo test"))
            );
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
                result: None,
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

#[cfg(test)]
#[path = "projection_media_tests.rs"]
mod media_tests;

#[cfg(all(test, target_os = "linux"))]
#[path = "projection_performance.rs"]
mod performance;
