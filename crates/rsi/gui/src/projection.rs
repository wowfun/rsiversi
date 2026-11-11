#[path = "spans.rs"]
mod spans;
#[path = "turns.rs"]
mod turns;

use rsi_agent_session_protocol::{AgentControlRecordBody, SessionFact, SessionFactBody, TurnId};
use rsi_agent_turn_protocol::SessionObservation;
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
    live_model: rsi_conversation::LiveModelPreview,
    live_keys: Vec<String>,
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
    #[cfg(test)]
    pub(crate) fn test_source(&mut self, key: String, source: SourceRef, text: &str) {
        self.put_source(key, "assistant", "Assistant", source, text);
    }
    pub fn model(&mut self, snapshot: &rsi_agent_turn_protocol::ModelLiveSnapshot) {
        if !self
            .live_model
            .update(snapshot, MAX_BLOCK_BYTES, MAX_BLOCK_BYTES * 2)
            .unwrap_or(false)
        {
            return;
        }
        self.project_live_model();
    }
    fn project_live_model(&mut self) {
        let Some((turn, effect, purpose, preview)) = self.live_model.view() else {
            self.blocks
                .retain(|block| !self.live_keys.contains(&block.key) || !block.sources.is_empty());
            self.live_keys.clear();
            self.trim();
            return;
        };
        let turn = turn.clone();
        let effect = effect.clone();
        let parts: Vec<_> = preview
            .blocks()
            .map(|part| {
                (
                    BlockIdentity::Model {
                        turn: &turn,
                        effect: &effect,
                        index: u32::from(part.index),
                    }
                    .key(),
                    part.clone(),
                )
            })
            .collect();
        self.blocks.retain(|block| {
            !self.live_keys.contains(&block.key)
                || !block.sources.is_empty()
                || parts.iter().any(|(key, _)| key == &block.key)
        });
        self.live_keys.clear();
        for (key, part) in parts {
            if self
                .blocks
                .iter()
                .any(|block| block.key == key && !block.sources.is_empty())
            {
                continue;
            }
            let (role, title) =
                if purpose == rsi_agent_session_protocol::ModelEventPurpose::ContextCompaction {
                    ("status", "Context compaction")
                } else if matches!(part.field, FactField::ModelReasoning { .. }) {
                    ("reasoning", "Thinking")
                } else {
                    ("assistant", "Assistant")
                };
            self.add(key.clone(), role, title, &part.window.text, false);
            if let Some(block) = self.blocks.iter_mut().find(|block| block.key == key) {
                block.clipped = part.window.more;
            }
            self.live_keys.push(key);
        }
        self.turns.live(
            &turn,
            &effect,
            purpose,
            &self.live_keys,
            &self.blocks,
            self.seq,
        );
        self.trim();
    }
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
    pub(crate) fn merge_entered(&mut self, seq: u64, inputs: Self) {
        self.blocks
            .retain(|block| block.first_seq != seq || !block.key.starts_with("prepared:"));
        self.blocks.extend(inputs.blocks);
        self.blocks
            .make_contiguous()
            .sort_by_key(|block| block.first_seq);
        self.omitted |= inputs.omitted;
        self.trim();
    }

    pub(crate) async fn history_inputs(
        &mut self,
        handle: &dyn rsi_session_protocol::SessionHandle,
        facts: &[SessionFact],
    ) -> Result<(), rsi_session_protocol::SessionError> {
        for fact in facts {
            rsi_client::visit_prepared_inputs(handle, fact, facts, |row| self.entered(fact, row))
                .await?;
            self.fact(fact);
        }
        Ok(())
    }

    pub(crate) fn entered(&mut self, fact: &SessionFact, row: rsi_conversation::EnteredInput<'_>) {
        let attribution = row.attribution();
        let role = if attribution.is_some() {
            "status"
        } else {
            "user"
        };
        let title = attribution.as_deref().unwrap_or("You");
        let mut parts = row.parts().peekable();
        let seq = fact.seq();
        self.seq = self.seq.max(seq);
        if parts.peek().is_none() {
            self.add(
                format!("prepared:{seq}:{}:0", row.index),
                role,
                title,
                "",
                false,
            );
        }
        for (index, (source, value)) in parts.enumerate() {
            let image;
            let text = match value {
                rsi_conversation::InputPart::Text(text) => text,
                rsi_conversation::InputPart::Image(media) => {
                    image = rsi_conversation::MediaSource { source, media }.label();
                    &image
                }
                rsi_conversation::InputPart::File(file) => {
                    image = format!("File: {} ({} bytes)", file.name(), file.bytes());
                    &image
                }
            };
            self.put_source_at(
                format!("prepared:{seq}:{}:{index}", row.index),
                role,
                title,
                source,
                text,
                Some(seq),
            );
        }
    }

    pub fn observation(&mut self, update: &SessionObservation) {
        match update {
            SessionObservation::Fact { fact, .. } => self.fact(fact),
            SessionObservation::Control { record, .. } => {
                self.queue.observe(record);
                if let AgentControlRecordBody::MessageDiscarded { message_id, reason } =
                    record.body()
                {
                    self.add(
                        format!("discard:{message_id}"),
                        "status",
                        "Input discarded",
                        &format!("{reason:?}"),
                        false,
                    );
                }
            }
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
        if let SessionFactBody::ToolRejected { rejection, .. } = fact.body() {
            block.text.truncate(block.tool_argument_bytes);
            block.text.push_str(&rejection.message());
        } else if tool.result != old_result
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
        let admitted = if let SessionFactBody::ModelStarted { effect_id, .. } = fact.body() {
            self.live_model.started(fact.seq(), effect_id)
        } else {
            false
        };
        self.fact_blocks(fact);
        self.turns.observe(fact, &self.blocks);
        if admitted {
            self.project_live_model();
        }
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
            SessionFactBody::TurnAccepted { turn_id, .. }
            | SessionFactBody::MessageTurnAccepted { turn_id, .. } => {
                if latest {
                    self.active = Some(turn_id.clone());
                    self.status = "Running".into();
                }
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
            SessionFactBody::ModelAttemptSettled {
                turn_id,
                effect_id,
                purpose,
                attempt,
            } => {
                if self
                    .live_model
                    .view()
                    .is_some_and(|(turn, effect, _, _)| turn == turn_id && effect == effect_id)
                {
                    // Promote the whole response before per-source sorting; otherwise
                    // its first settled block moves behind its still-transient siblings.
                    for block in &mut self.blocks {
                        if self.live_keys.contains(&block.key) {
                            block.first_seq = fact.seq();
                        }
                    }
                }
                self.live_model.settle(fact.seq(), effect_id);
                let preview =
                    rsi_conversation::model_previews(attempt, MAX_BLOCK_BYTES, MAX_BLOCK_BYTES * 2);
                for part in preview.blocks() {
                    let (role, title) = if *purpose
                        == rsi_agent_session_protocol::ModelEventPurpose::ContextCompaction
                    {
                        ("status", "Context compaction")
                    } else if matches!(part.field, FactField::ModelReasoning { .. }) {
                        ("reasoning", "Reasoning")
                    } else {
                        ("assistant", "Assistant")
                    };
                    let key = BlockIdentity::Model {
                        turn: turn_id,
                        effect: effect_id,
                        index: u32::from(part.index),
                    }
                    .key();
                    self.put_source(
                        key.clone(),
                        role,
                        title,
                        source(part.field),
                        &part.window.text,
                    );
                    if part.window.more
                        && let Some(block) = self.blocks.iter_mut().find(|block| block.key == key)
                    {
                        block.clipped = true;
                    }
                }
            }
            SessionFactBody::TurnTerminal {
                turn_id, outcome, ..
            } => {
                let (status, detail) = match outcome {
                    rsi_agent_session_protocol::TurnOutcome::Completed => {
                        ("Completed", String::new())
                    }
                    rsi_agent_session_protocol::TurnOutcome::Blocked => {
                        ("Blocked", "Input preparation blocked the request".into())
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
                    rsi_agent_session_protocol::TurnOutcome::MaxTokens => (
                        "Output limit",
                        "Model output reached its token limit".into(),
                    ),
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
        | SessionFactBody::ModelAttemptSettled {
            turn_id: turn,
            effect_id: effect,
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
    #[allow(clippy::too_many_lines)] // One fixture compares a complete live and durable lifecycle.
    fn settlement_keeps_streamed_reasoning_before_its_answer() {
        use rsi_agent_session_protocol::{
            EffectId, ModelAttemptBuilder, ModelEventPurpose, SessionId,
        };
        use rsi_ai_protocol::{ContentDelta, ContentStart, FinishReason, LanguageEvent};
        let mut builder = ModelAttemptBuilder::new();
        for event in [
            LanguageEvent::ContentStarted {
                index: 0,
                content: ContentStart::Reasoning,
            },
            LanguageEvent::ContentDelta {
                index: 0,
                delta: ContentDelta::Reasoning("think first".into()),
            },
            LanguageEvent::ContentFinished { index: 0 },
            LanguageEvent::ContentStarted {
                index: 1,
                content: ContentStart::Text,
            },
            LanguageEvent::ContentDelta {
                index: 1,
                delta: ContentDelta::Text("answer second".into()),
            },
            LanguageEvent::ContentFinished { index: 1 },
        ] {
            builder.push(0, &event).unwrap();
        }
        let snapshot = rsi_agent_turn_protocol::ModelLiveSnapshot {
            session_id: SessionId::new("session").unwrap(),
            turn_id: TurnId::new("turn").unwrap(),
            effect_id: EffectId::new("effect").unwrap(),
            purpose: ModelEventPurpose::Conversation,
            revision: 1,
            prefix: builder.prefix(),
            status: rsi_agent_turn_protocol::ModelLiveStatus::Streaming,
        };
        let started = SessionFact::new(
            1,
            1,
            SessionFactBody::ModelStarted {
                turn_id: snapshot.turn_id.clone(),
                effect_id: snapshot.effect_id.clone(),
            },
        )
        .unwrap();
        builder
            .push(
                1,
                &LanguageEvent::Finished {
                    reason: FinishReason::Stop,
                    replay: None,
                },
            )
            .unwrap();
        let settled = SessionFact::new(
            2,
            2,
            SessionFactBody::ModelAttemptSettled {
                turn_id: snapshot.turn_id.clone(),
                effect_id: snapshot.effect_id.clone(),
                purpose: snapshot.purpose,
                attempt: builder.finish(true).unwrap(),
            },
        )
        .unwrap();
        let mut live = Transcript::default();
        live.fact(&started);
        live.model(&snapshot);
        live.fact(&settled);
        let mut cold = Transcript::default();
        cold.fact(&started);
        cold.fact(&settled);
        let content = |transcript: &Transcript| {
            transcript
                .blocks
                .iter()
                .filter(|block| matches!(block.role, "reasoning" | "assistant"))
                .map(|block| (block.key.clone(), block.text.clone()))
                .collect::<Vec<_>>()
        };
        assert_eq!(content(&live), content(&cold));
        assert_eq!(
            content(&live)
                .iter()
                .map(|(_, text)| text.as_str())
                .collect::<Vec<_>>(),
            ["think first", "answer second"]
        );
        let revisions: Vec<_> = live
            .blocks
            .iter()
            .map(|block| block.revision.clone())
            .collect();
        live.fact(&settled);
        assert_eq!(content(&live), content(&cold));
        assert!(
            live.blocks
                .iter()
                .zip(revisions)
                .all(|(block, revision)| std::sync::Arc::ptr_eq(&block.revision, &revision))
        );
    }

    #[test]
    fn started_fact_reveals_a_preview_that_arrived_first() {
        use rsi_agent_session_protocol::{
            EffectId, ModelAttemptBuilder, ModelEventPurpose, SessionId,
        };
        use rsi_ai_protocol::{ContentDelta, ContentStart, LanguageEvent};
        let mut builder = ModelAttemptBuilder::new();
        builder
            .push(
                0,
                &LanguageEvent::ContentStarted {
                    index: 0,
                    content: ContentStart::Text,
                },
            )
            .unwrap();
        builder
            .push(
                1,
                &LanguageEvent::ContentDelta {
                    index: 0,
                    delta: ContentDelta::Text("waiting".into()),
                },
            )
            .unwrap();
        let snapshot = rsi_agent_turn_protocol::ModelLiveSnapshot {
            session_id: SessionId::new("session").unwrap(),
            turn_id: TurnId::new("turn").unwrap(),
            effect_id: EffectId::new("effect").unwrap(),
            purpose: ModelEventPurpose::Conversation,
            revision: 1,
            prefix: builder.prefix(),
            status: rsi_agent_turn_protocol::ModelLiveStatus::Streaming,
        };
        let mut transcript = Transcript::default();
        transcript.model(&snapshot);
        assert!(transcript.blocks.is_empty());
        transcript.fact(
            &SessionFact::new(
                1,
                1,
                SessionFactBody::ModelStarted {
                    turn_id: snapshot.turn_id,
                    effect_id: snapshot.effect_id,
                },
            )
            .unwrap(),
        );
        let block = transcript
            .blocks
            .iter()
            .find(|block| block.role == "assistant")
            .unwrap();
        assert_eq!(block.text, "waiting");
        assert!(block.sources.is_empty());
    }

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
    fn queue_successor_updates_the_pending_slot_without_publishing_conversation_rows() {
        use rsi_agent_session_protocol::{
            AgentControlRecord, AgentMessage, AgentMessageContent, AgentMessageSource,
            InputMessageSource, MessageDelivery, MessageId, MessageOptions, MessageTarget,
            QueueSlot, SessionId, StepId,
        };
        let mut transcript = Transcript::default();
        let original = MessageId::new("original").unwrap();
        let successor = MessageId::new("successor").unwrap();
        let session = SessionId::new("session").unwrap();
        let mut message = AgentMessage {
            message_id: original.clone(),
            source: AgentMessageSource::Human { attribution: None },
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
        assert!(transcript.blocks.is_empty());
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
        assert!(transcript.blocks.is_empty());
        let queue = transcript.queue.view(None);
        assert_eq!(queue.len(), 1);
        assert_eq!(queue[0].message.queue_slot.id.as_str(), "original");
        assert_eq!(queue[0].message.message_id, successor);
        let fact = SessionFact::new(
            1,
            4,
            SessionFactBody::InputMessageClaimed {
                turn_id: TurnId::new("turn").unwrap(),
                step_id: StepId::new("step").unwrap(),
                source: InputMessageSource::Human {
                    attribution: None,
                    message_id: successor,
                },
                content: message.content,
            },
        )
        .unwrap();
        transcript.fact(&fact);
        assert!(
            transcript.blocks.is_empty(),
            "claim does not enter a message"
        );
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
    fn rejected_tool_displays_durable_reason_once_with_bounded_arguments() {
        use rsi_agent_session_protocol::{ContributionId, EffectId, ToolOrigin, ToolRejection};
        use rsi_approval_protocol::{ApprovalDecision, ApprovalOutcome};
        use rsi_tools_protocol::{ToolProgramRole, ToolResultIdentity};

        for (rejection, expected) in [
            (
                ToolRejection::PolicyDenied {
                    contribution_id: ContributionId::new("fixture.policy").unwrap(),
                    reason: "Mods refused this command: 拒绝执行".into(),
                },
                "Mods refused this command: 拒绝执行",
            ),
            (
                ToolRejection::ApprovalDenied {
                    outcome: ApprovalOutcome {
                        decision: ApprovalDecision::Deny,
                        answerer: "fixture.user".into(),
                        reason: None,
                    },
                },
                "Live approval denied the Tool call; it was not executed.",
            ),
            (
                ToolRejection::ApprovalDenied {
                    outcome: ApprovalOutcome {
                        decision: ApprovalDecision::Deny,
                        answerer: "fixture.user".into(),
                        reason: Some("Keep this file unchanged".into()),
                    },
                },
                "Live approval denied the Tool call: Keep this file unchanged",
            ),
        ] {
            let fact = SessionFact::new(
                12,
                1,
                SessionFactBody::ToolRejected {
                    additional_contexts: Vec::new(),
                    origin: ToolOrigin::Model {
                        effect_id: EffectId::new("source-model").unwrap(),
                    },
                    program_role: ToolProgramRole::Unavailable,
                    turn_id: TurnId::new("turn").unwrap(),
                    effect_id: EffectId::new("effect").unwrap(),
                    identity: ToolResultIdentity::new("owner", "invoke", "call", "a".repeat(64))
                        .unwrap(),
                    name: "pwsh".into(),
                    arguments: serde_json::json!({"command": "界".repeat(MAX_BLOCK_BYTES / 3)}),
                    rejection,
                },
            )
            .unwrap();
            let mut transcript = Transcript::default();
            transcript.fact(&fact);
            let block = &transcript.blocks[0];
            assert!(block.text.ends_with(expected));
            assert_eq!(block.text.matches(expected).count(), 1);
            assert!(block.clipped);
            assert!(block.text.len() <= MAX_BLOCK_BYTES);
            let view = serde_json::to_value(block).unwrap();
            assert_eq!(view["tool"]["phase"], "rejected");
            assert_eq!(view["tool"]["rejection"]["seq"], "12");
            assert!(view["tool"]["result"].is_null());
            let revision = block.revision.clone();
            transcript.fact(&fact);
            assert!(std::sync::Arc::ptr_eq(
                &revision,
                &transcript.blocks[0].revision
            ));
            assert_eq!(serde_json::to_value(&transcript.blocks[0]).unwrap(), view);
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
