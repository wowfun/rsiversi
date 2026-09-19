//! Partial historical projection; no model-context replay or retained observation leases.
use rsi_agent_session_protocol::{AgentMessageContent, InputMessageSource};
use rsi_agent_session_protocol::{SessionFact, SessionFactBody};
use rsi_ai_protocol::{ContentDelta, LanguageEvent};
pub use rsi_conversation::SourceRef as Source;
use rsi_conversation::{
    BlockIdentity, FactField, FieldWindow, MAXIMUM_BLOCK_SOURCES, SourceAdmission, SourceIndex,
    ToolState,
};
use rsi_tools_protocol::ToolContent;
use std::collections::VecDeque;

pub const MAX_BLOCKS: usize = 512;
pub const MAX_TEXT: usize = 4 * 1024 * 1024;
pub const MAX_METADATA: usize = 8 * 1024 * 1024;
pub const WINDOW: usize = 256 * 1024;

fn request_key(
    turn: &rsi_agent_session_protocol::TurnId,
    effect: &rsi_agent_session_protocol::EffectId,
) -> String {
    serde_json::to_string(&("request", turn, effect)).expect("typed identifiers")
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Anchor {
    pub source: Source,
    pub offset: usize,
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct Mapping {
    display: usize,
    source: usize,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Piece {
    pub source: Source,
    pub text: String,
    pub omitted: bool,
    pub truncated_after: bool,
    pub start: usize,
    mapping: Vec<Mapping>,
}

impl Piece {
    fn json(source: Source, value: &impl serde::Serialize, start: usize) -> Self {
        Self::from_window(
            source,
            &FieldWindow::json(value, start, WINDOW).expect("bounded linked JSON serialization"),
        )
    }

    pub fn from_window(source: Source, window: &FieldWindow) -> Self {
        let mut piece = Self::new(source, &window.text, 0, WINDOW);
        for run in &mut piece.mapping {
            run.source += window.start;
        }
        piece.start = window.start;
        piece.truncated_after |= window.more;
        piece.omitted |= window.start > 0 || window.more;
        piece
    }

    fn new(source: Source, text: &str, start: usize, limit: usize) -> Self {
        let mut start = start.min(text.len());
        while !text.is_char_boundary(start) {
            start += 1;
        }
        let mut safe = String::new();
        let mut mapping = vec![Mapping {
            display: 0,
            source: start,
        }];
        let mut consumed = start;
        for (offset, character) in text[start..].char_indices() {
            let filtered = crate::terminal_character(character);
            if safe.len() + filtered.len_utf8() > limit {
                break;
            }
            safe.push(filtered);
            consumed = start + offset + character.len_utf8();
            if filtered != character {
                mapping.push(Mapping {
                    display: safe.len(),
                    source: consumed,
                });
            }
        }
        safe.shrink_to_fit();
        mapping.shrink_to_fit();
        Self {
            source,
            text: safe,
            omitted: start > 0 || consumed < text.len(),
            truncated_after: consumed < text.len(),
            start,
            mapping,
        }
    }

    pub fn anchor(&self, offset: usize) -> Anchor {
        let offset = offset.min(self.text.len());
        let run = &self.mapping[self
            .mapping
            .partition_point(|run| run.display <= offset)
            .saturating_sub(1)];
        Anchor {
            source: self.source,
            offset: run.source + offset - run.display,
        }
    }

    pub fn display_offset(&self, anchor: Anchor) -> Option<usize> {
        if anchor.source != self.source || anchor.offset < self.start {
            return None;
        }
        let run = &self.mapping[self
            .mapping
            .partition_point(|run| run.source <= anchor.offset)
            .saturating_sub(1)];
        let offset = run
            .display
            .checked_add(anchor.offset.saturating_sub(run.source))?;
        (offset <= self.text.len()
            && self.text.is_char_boundary(offset)
            && self.anchor(offset) == anchor)
            .then_some(offset)
    }

    fn metadata(&self) -> usize {
        self.mapping.capacity() * std::mem::size_of::<Mapping>()
    }
}

fn user_time(timestamp_ms: u64) -> String {
    let minutes = timestamp_ms / 60_000;
    format!("{:02}:{:02}", (minutes / 60) % 24, minutes % 60)
}

pub(crate) fn tool_title(tool: &ToolState, interrupted: bool) -> String {
    use rsi_conversation::{ToolOutcome, ToolPhase};
    let name = tool.name.as_deref().unwrap_or("Tool");
    let (prepared, running, settled, subject) = match (name, tool.argument_summary.as_deref()) {
        ("bash", Some(command)) => ("Run", "Running", "Ran", command.to_owned()),
        ("directory_list", Some(path)) => ("List", "Listing", "Listed", path.to_owned()),
        ("file_read", Some(path)) => ("Read", "Reading", "Read", path.to_owned()),
        (_, summary) => (
            "Call",
            "Calling",
            "Called",
            summary.map_or_else(|| name.into(), |text| format!("{name}({text})")),
        ),
    };
    let title = if interrupted {
        format!("Interrupted: {} {subject}", prepared.to_lowercase())
    } else {
        match tool.phase {
            ToolPhase::Prepared => format!("{prepared} {subject}"),
            ToolPhase::Running => format!("{running} {subject}"),
            ToolPhase::Settled(ToolOutcome::Completed) => format!("{settled} {subject}"),
            ToolPhase::Settled(_) => format!("Failed to {} {subject}", prepared.to_lowercase()),
            ToolPhase::Rejected => format!("Rejected: {} {subject}", prepared.to_lowercase()),
        }
    };
    if !tool.intent_present && tool.phase != ToolPhase::Rejected {
        format!("{title} (intent not loaded)")
    } else {
        title
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum Role {
    User,
    Assistant,
    Reasoning,
    Tool,
    Status,
    Metadata,
    /// Local presentation annotation, with no durable source or model content.
    Notice,
    /// Local error annotation with the same source-free lifetime as a notice.
    Error,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ProcessOutcome {
    Success,
    Failed,
    Interrupted,
}

impl ProcessOutcome {
    fn from_model(event: &LanguageEvent) -> Option<Self> {
        match event {
            LanguageEvent::Finished { reason, .. } => Some(match reason {
                rsi_ai_protocol::FinishReason::Stop | rsi_ai_protocol::FinishReason::ToolCalls => {
                    Self::Success
                }
                rsi_ai_protocol::FinishReason::Cancelled
                | rsi_ai_protocol::FinishReason::MaxTokens
                | rsi_ai_protocol::FinishReason::ContentFilter => Self::Interrupted,
            }),
            LanguageEvent::Failed { error, .. } => {
                Some(if error.kind() == rsi_ai_protocol::ErrorKind::Cancelled {
                    Self::Interrupted
                } else {
                    Self::Failed
                })
            }
            _ => None,
        }
    }

    fn from_tool(phase: rsi_conversation::ToolPhase) -> Option<Self> {
        use rsi_conversation::{ToolOutcome, ToolPhase};
        match phase {
            ToolPhase::Settled(ToolOutcome::Completed) => Some(Self::Success),
            ToolPhase::Settled(ToolOutcome::ToolFailed | ToolOutcome::ProcessFailed)
            | ToolPhase::Rejected => Some(Self::Failed),
            ToolPhase::Prepared | ToolPhase::Running => None,
        }
    }
}

#[derive(Clone, Debug)]
#[allow(clippy::struct_excessive_bools)] // Independent presentation flags; folds do not change lifecycle or retention.
pub struct Block {
    pub(crate) markdown: Option<std::sync::Arc<crate::markdown::Document>>,
    pub(crate) clock: ProcessClock,
    pub(crate) outcome: Option<ProcessOutcome>,
    pub key: String,
    pub layout_revision: std::sync::Arc<()>,
    pub title: String,
    pub role: Role,
    pub pieces: VecDeque<Piece>,
    sources: SourceIndex,
    pub collapsed: bool,
    pub completed: bool,
    pub(crate) concise: bool,
    pub(crate) fold: Option<viewport::FoldWindow>,
    pub outputs: [Option<String>; 2],
    pub first: u64,
    last: u64,
    request_key: Option<String>,
    pub discarded: bool,
    text_bytes: usize,
    map_bytes: usize,
    pub tool: Option<ToolState>,
    request: Option<rsi_conversation::RequestPresentation>,
    time_ms: Option<u64>,
}

#[derive(Clone, Debug, Default)]
pub(crate) struct ProcessClock {
    turn: Option<rsi_agent_session_protocol::TurnId>,
    started: Option<u64>,
    ended: Option<u64>,
    pub elapsed_ms: Option<u64>,
    pub running: bool,
}
impl ProcessClock {
    pub fn display(&self, activity: Option<&crate::Activity>) -> (Option<u64>, bool) {
        let Some(started) = self.started else {
            return (self.elapsed_ms, self.running);
        };
        if let Some(ended) = self.ended {
            return (ended.checked_sub(started), false);
        }
        match activity.filter(|activity| self.turn.as_ref() == Some(&activity.turn_id)) {
            Some(activity) => (activity.now_ms.checked_sub(started), true),
            None => (None, false),
        }
    }
}

/// One per-render index; source lookup does not rescan all streamed pieces per cell.
#[derive(Debug)]
pub struct AnchorIndex<'a>(Vec<(usize, &'a Piece)>);
impl AnchorIndex<'_> {
    pub fn anchor(&self, offset: usize) -> Option<Anchor> {
        let index = self
            .0
            .partition_point(|(start, _)| *start <= offset)
            .checked_sub(1)?;
        let (start, piece) = self.0[index];
        (offset - start <= piece.text.len()).then(|| piece.anchor(offset - start))
    }
}

impl Block {
    fn settle_process(&mut self, outcome: ProcessOutcome, timestamp_ms: u64) {
        self.outcome = Some(outcome);
        self.completed = true;
        self.clock.ended.get_or_insert(timestamp_ms);
    }

    fn order(&self) -> (u64, u8) {
        let metadata = self.role == Role::Metadata;
        (
            if metadata { self.last } else { self.first },
            if matches!(self.role, Role::Notice | Role::Error) {
                2
            } else {
                u8::from(metadata)
            },
        )
    }
    pub(crate) fn summary_only(&self) -> bool {
        self.collapsed && ((self.role == Role::Reasoning && self.completed) || self.concise)
    }
    fn evict_piece(&mut self, back: bool) -> bool {
        let Some(last) = self.pieces.len().checked_sub(1) else {
            return false;
        };
        let position = if back { last } else { 0 };
        self.layout_revision = std::sync::Arc::new(());
        let piece = self.pieces.remove(position).expect("retained piece");
        assert_eq!(self.sources.remove(position), Some(piece.source));
        self.text_bytes -= piece.text.capacity();
        self.map_bytes -= piece.metadata();
        self.discarded = true;
        true
    }
    pub fn anchor_index(&self) -> AnchorIndex<'_> {
        let mut offset = 0;
        AnchorIndex(
            self.pieces
                .iter()
                .map(|piece| {
                    let start = offset;
                    offset += piece.text.len();
                    (start, piece)
                })
                .collect(),
        )
    }
    pub fn sources(&self) -> &SourceIndex {
        &self.sources
    }
    pub fn request_seq(&self) -> Option<u64> {
        self.request
            .as_ref()
            .and_then(rsi_conversation::RequestPresentation::intent_seq)
    }
    pub fn text(&self) -> String {
        self.pieces
            .iter()
            .map(|piece| piece.text.as_str())
            .collect()
    }
    pub fn bytes(&self) -> usize {
        self.text_bytes
    }
    fn metadata(&self) -> usize {
        self.key.capacity()
            + self.request_key.as_ref().map_or(0, String::capacity)
            + self.sources.owned_bytes()
            + self.tool.as_ref().map_or(0, ToolState::owned_bytes)
            + self
                .request
                .as_ref()
                .map_or(0, rsi_conversation::RequestPresentation::owned_bytes)
            + self.title.capacity()
            + self.pieces.capacity() * std::mem::size_of::<Piece>()
            + self.map_bytes
            + self
                .markdown
                .as_ref()
                .map_or(0, |document| document.bytes())
            + self
                .outputs
                .iter()
                .flatten()
                .map(String::capacity)
                .sum::<usize>()
    }
    pub fn anchor(&self, mut offset: usize) -> Option<Anchor> {
        for (index, piece) in self.pieces.iter().enumerate() {
            if offset < piece.text.len()
                || index + 1 == self.pieces.len() && offset == piece.text.len()
            {
                return Some(piece.anchor(offset));
            }
            offset -= piece.text.len();
        }
        None
    }
    pub fn offset(&self, anchor: Anchor) -> Option<usize> {
        let mut offset = 0;
        for piece in &self.pieces {
            if let Some(found) = piece.display_offset(anchor) {
                return Some(offset + found);
            }
            offset += piece.text.len();
        }
        None
    }
}

#[derive(Clone, Debug, Default)]
pub struct Transcript {
    pub blocks: Vec<Block>,
    pub earlier: bool,
}

impl Transcript {
    /// Adds bounded local display text without creating a Fact or source anchor.
    #[allow(clippy::missing_panics_doc)] // The checked positive notice count proves its first index exists.
    pub fn push_notice(&mut self, id: u64, text: &str) {
        let seq = self
            .blocks
            .iter()
            .map(|block| block.last)
            .max()
            .unwrap_or(0);
        let mut text = crate::terminal_text(&text.chars().take(2048).collect::<String>());
        text.truncate(text.floor_char_boundary(4096));
        self.block_index(format!("local-notice-{id}"), &text, Role::Notice, seq);
        while self
            .blocks
            .iter()
            .filter(|block| matches!(block.role, Role::Notice | Role::Error))
            .count()
            > 32
        {
            let first = self
                .blocks
                .iter()
                .position(|block| matches!(block.role, Role::Notice | Role::Error))
                .unwrap();
            self.blocks.remove(first);
        }
        self.blocks.sort_by_key(Block::order);
        self.trim(false);
    }
    pub fn push_error(&mut self, id: u64, text: &str) {
        self.push_notice(id, text);
        if let Some(block) = self
            .blocks
            .iter_mut()
            .find(|block| block.key == format!("local-notice-{id}"))
        {
            block.role = Role::Error;
        }
    }
    pub(crate) fn reuse_layout_revisions(&mut self, previous: &Self) {
        let previous: std::collections::BTreeMap<_, _> = previous
            .blocks
            .iter()
            .map(|block| (block.key.as_str(), block))
            .collect();
        for block in &mut self.blocks {
            if let Some(old) = previous.get(block.key.as_str()).filter(|old| {
                old.role == block.role
                    && old.collapsed == block.collapsed
                    && old.markdown == block.markdown
                    && old.pieces == block.pieces
            }) {
                block.layout_revision = old.layout_revision.clone();
            }
        }
    }

    pub fn apply(&mut self, fact: &SessionFact) {
        self.project(fact);
        if !self.blocks.is_sorted_by_key(Block::order) {
            self.blocks.sort_by_key(Block::order);
        }
        self.trim(false);
    }

    pub fn apply_history(&mut self, fact: &SessionFact) {
        self.project(fact);
        self.blocks.sort_by_key(Block::order);
        self.trim(true);
    }

    #[cfg(test)]
    pub fn window(fact: &SessionFact, source: Source, start: usize) -> Option<Piece> {
        let window = rsi_conversation::select_field(fact, source)?
            .window(start, WINDOW)
            .ok()?;
        Some(Piece::from_window(source, &window))
    }

    #[allow(clippy::too_many_lines)] // One exhaustive projection owns the supported Fact payload fields.
    fn project(&mut self, fact: &SessionFact) {
        self.project_request(fact);
        if BlockIdentity::tool(fact).is_some() {
            self.project_tool(fact);
            return;
        }
        if let SessionFactBody::ModelEvent {
            turn_id,
            effect_id,
            event: LanguageEvent::ContentFinished { index },
            ..
        } = fact.body()
        {
            let key = BlockIdentity::Model {
                turn: turn_id,
                effect: effect_id,
                index: *index,
            }
            .key();
            if let Some(block) = self.blocks.iter_mut().find(|block| block.key == key) {
                block.completed = true;
                block.clock.ended = Some(fact.timestamp_ms());
            }
        }
        let seq = fact.seq();
        let mut add = |key: String, title: String, role, field, text: &str| {
            let source = Source { seq, field };
            self.add(key, &title, role, Piece::new(source, text, 0, WINDOW));
        };
        match fact.body() {
            SessionFactBody::TurnAccepted { turn_id, text, .. } => add(
                BlockIdentity::TurnInput { turn: turn_id }.key(),
                user_time(fact.timestamp_ms()),
                Role::User,
                FactField::TurnInput,
                text,
            ),
            SessionFactBody::InputMessageEntered {
                source, content, ..
            } => {
                let time = user_time(fact.timestamp_ms());
                let agent_title = match source {
                    InputMessageSource::Agent {
                        source_session_id, ..
                    } => format!("Message from {source_session_id}"),
                    InputMessageSource::Completion {
                        child_session_id, ..
                    } => format!("Completion from {child_session_id}"),
                    _ => String::new(),
                };
                let (key, title, role) = match source {
                    InputMessageSource::Human { message_id } => (
                        BlockIdentity::Message {
                            message: message_id,
                        }
                        .key(),
                        time.as_str(),
                        Role::User,
                    ),
                    InputMessageSource::Agent { message_id, .. }
                    | InputMessageSource::Completion { message_id, .. } => (
                        BlockIdentity::Message {
                            message: message_id,
                        }
                        .key(),
                        agent_title.as_str(),
                        Role::Status,
                    ),
                    _ => return,
                };
                for (index, content) in content.iter().enumerate() {
                    if let AgentMessageContent::Reference { reference } = content {
                        self.add(
                            key.clone(),
                            title,
                            role,
                            Piece::new(
                                Source {
                                    seq,
                                    field: FactField::InputReference {
                                        index: u16::try_from(index).expect("bounded content"),
                                    },
                                },
                                &reference.preview,
                                0,
                                WINDOW,
                            ),
                        );
                    } else if let AgentMessageContent::Image { media } = content {
                        let source = Source {
                            seq,
                            field: FactField::InputImage {
                                index: u16::try_from(index)
                                    .expect("validated message content index"),
                            },
                        };
                        self.add(key.clone(), title, role, Piece::json(source, media, 0));
                    } else if let AgentMessageContent::Text { text } = content {
                        self.add(
                            key.clone(),
                            title,
                            role,
                            Piece::new(
                                Source {
                                    seq,
                                    field: FactField::InputText {
                                        index: u16::try_from(index)
                                            .expect("validated message content index"),
                                    },
                                },
                                text,
                                0,
                                WINDOW,
                            ),
                        );
                    }
                }
            }
            SessionFactBody::ImageOutput {
                turn_id,
                effect_id,
                index,
                ..
            } => {
                let source = Source {
                    seq,
                    field: FactField::ImageOutput,
                };
                let image =
                    rsi_conversation::MediaSource::select(fact, source).expect("image Fact");
                self.add(
                    BlockIdentity::Image {
                        turn: turn_id,
                        effect: effect_id,
                        index: *index,
                    }
                    .key(),
                    "Image",
                    Role::Assistant,
                    Piece::json(source, image.media, 0),
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
                        (Role::Assistant, "Assistant", text, FactField::ModelText)
                    }
                    ContentDelta::Reasoning(text) => {
                        (Role::Reasoning, "Thinking", text, FactField::ModelReasoning)
                    }
                    ContentDelta::ToolArguments(_) => return,
                };
                let (role, title) = if *purpose
                    == rsi_agent_session_protocol::ModelEventPurpose::ContextCompaction
                {
                    (Role::Status, "Context compaction")
                } else {
                    (role, title)
                };
                add(
                    BlockIdentity::Model {
                        turn: turn_id,
                        effect: effect_id,
                        index: *index,
                    }
                    .key(),
                    title.into(),
                    role,
                    field,
                    text,
                );
                if role == Role::Reasoning {
                    let key = BlockIdentity::Model {
                        turn: turn_id,
                        effect: effect_id,
                        index: *index,
                    }
                    .key();
                    if let Some(block) = self.blocks.iter_mut().find(|block| block.key == key) {
                        block.clock.turn = Some(turn_id.clone());
                        block.clock.started = Some(
                            block
                                .clock
                                .started
                                .map_or(fact.timestamp_ms(), |old| old.min(fact.timestamp_ms())),
                        );
                    }
                }
            }
            SessionFactBody::TurnTerminal {
                turn_id, outcome, ..
            } => {
                if matches!(outcome, rsi_agent_session_protocol::TurnOutcome::Completed) {
                    return;
                }
                self.add(
                    BlockIdentity::Terminal { turn: turn_id }.key(),
                    "Turn result",
                    Role::Status,
                    Piece::json(
                        Source {
                            seq,
                            field: FactField::TurnOutcome,
                        },
                        outcome,
                        0,
                    ),
                );
            }
            SessionFactBody::ModelEvent {
                event: LanguageEvent::Failed { error, .. },
                ..
            } => {
                add(
                    format!("error:{seq}"),
                    "Model error".into(),
                    Role::Status,
                    FactField::ModelFailure,
                    &error.to_string(),
                );
            }
            _ => {}
        }
        let content = match fact.body() {
            SessionFactBody::ModelEvent {
                turn_id,
                effect_id,
                event: LanguageEvent::ContentDelta { index, .. },
                ..
            } => Some((
                BlockIdentity::Model {
                    turn: turn_id,
                    effect: effect_id,
                    index: *index,
                }
                .key(),
                turn_id,
                effect_id,
            )),
            SessionFactBody::ImageOutput {
                turn_id,
                effect_id,
                index,
                ..
            } => Some((
                BlockIdentity::Image {
                    turn: turn_id,
                    effect: effect_id,
                    index: *index,
                }
                .key(),
                turn_id,
                effect_id,
            )),
            _ => None,
        };
        if let Some((key, turn, effect)) = content {
            let request_key = request_key(turn, effect);
            let completion = self
                .blocks
                .iter()
                .find(|block| block.key == request_key)
                .and_then(|block| block.outcome.zip(block.clock.ended));
            let Some(block) = self.blocks.iter_mut().find(|block| block.key == key) else {
                return;
            };
            let answer = block.role == Role::Assistant;
            block.request_key.get_or_insert_with(|| request_key.clone());
            if block.role == Role::Reasoning
                && let Some((outcome, timestamp)) = completion
            {
                block.settle_process(outcome, timestamp);
            }
            if answer
                && let Some(metadata) = self
                    .blocks
                    .iter_mut()
                    .find(|block| block.key == request_key)
            {
                metadata.completed = metadata.time_ms.is_some() && !metadata.concise;
            }
        }
    }

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
        | SessionFactBody::ToolIntent {
            turn_id: turn,
            source_model_effect_id: effect,
            ..
        }
        | SessionFactBody::ImageOutput {
            turn_id: turn,
            effect_id: effect,
            ..
        }
        | SessionFactBody::ModelEvent {
            turn_id: turn,
            effect_id: effect,
            ..
        }) = fact.body()
        else {
            return;
        };
        let key = request_key(turn, effect);
        if !matches!(
            fact.body(),
            SessionFactBody::ModelIntent { .. }
                | SessionFactBody::ModelStarted { .. }
                | SessionFactBody::ModelEvent {
                    event: LanguageEvent::Usage { .. }
                        | LanguageEvent::Finished { .. }
                        | LanguageEvent::Failed { .. },
                    ..
                }
        ) && !self.blocks.iter().any(|block| block.key == key)
        {
            return;
        }
        let last = self
            .blocks
            .iter()
            .filter(|block| block.request_key.as_ref() == Some(&key))
            .map(|block| block.last)
            .max()
            .unwrap_or(0)
            .max(fact.seq());
        let has_tools = self
            .blocks
            .iter()
            .any(|block| block.role == Role::Tool && block.request_key.as_ref() == Some(&key));
        let has_answer = self
            .blocks
            .iter()
            .any(|block| block.role == Role::Assistant && block.request_key.as_ref() == Some(&key));
        let index = self.block_index(key.clone(), "Request", Role::Metadata, fact.seq());
        let block = &mut self.blocks[index];
        if let SessionFactBody::ModelEvent { event, .. } = fact.body()
            && let Some(outcome) = ProcessOutcome::from_model(event)
        {
            block.outcome = Some(outcome);
            block.clock.ended = Some(fact.timestamp_ms());
        }
        let request = block.request.get_or_insert_with(Default::default);
        request.observe(fact);
        block.concise |= has_tools;
        if let SessionFactBody::ModelEvent {
            event: LanguageEvent::Finished { reason, .. },
            purpose,
            ..
        } = fact.body()
        {
            block.concise |= *reason == rsi_ai_protocol::FinishReason::ToolCalls;
            block.completed = has_answer
                && !block.concise
                && *purpose == rsi_agent_session_protocol::ModelEventPurpose::Conversation;
            if *purpose == rsi_agent_session_protocol::ModelEventPurpose::Conversation {
                block.time_ms = Some(fact.timestamp_ms());
            }
        }
        block.title = crate::terminal_text(&block.time_ms.map_or_else(
            || request.title(),
            |time| format!("{} · {}", user_time(time), request.title()),
        ));
        block.first = block.first.min(fact.seq());
        block.last = block.last.max(last);
        if let Some((outcome, timestamp)) = block.outcome.zip(block.clock.ended) {
            for content in &mut self.blocks {
                if content.role == Role::Reasoning && content.request_key.as_ref() == Some(&key) {
                    content.settle_process(outcome, timestamp);
                }
            }
        }
    }

    fn project_tool(&mut self, fact: &SessionFact) {
        if let SessionFactBody::ToolIntent {
            turn_id,
            source_model_effect_id,
            ..
        } = fact.body()
        {
            let key = request_key(turn_id, source_model_effect_id);
            if let Some(metadata) = self.blocks.iter_mut().find(|block| block.key == key) {
                metadata.concise = true;
                metadata.completed = false;
            }
        }
        let key = BlockIdentity::tool(fact).expect("Tool Fact").key();
        let index = self.block_index(key.clone(), "Tool", Role::Tool, fact.seq());
        let block = &mut self.blocks[index];
        match fact.body() {
            SessionFactBody::ToolStarted { turn_id, .. } => {
                block.clock.turn = Some(turn_id.clone());
                block.clock.started = Some(fact.timestamp_ms());
            }
            SessionFactBody::ToolResult { .. } => block.clock.ended = Some(fact.timestamp_ms()),
            _ => {}
        }
        if let SessionFactBody::ToolIntent {
            turn_id,
            source_model_effect_id,
            ..
        } = fact.body()
        {
            block.request_key = Some(request_key(turn_id, source_model_effect_id));
        }
        let tool = if let Some(tool) = &mut block.tool {
            tool.observe(fact);
            tool
        } else {
            block
                .tool
                .insert(ToolState::from_fact(fact).expect("Tool Fact"))
        };
        let title = crate::terminal_text(&tool_title(tool, false));
        block.title.clone_from(&title);
        block.concise = tool.argument_summary.is_some();
        block.completed = matches!(
            tool.phase,
            rsi_conversation::ToolPhase::Settled(_) | rsi_conversation::ToolPhase::Rejected
        );
        block.outcome = ProcessOutcome::from_tool(tool.phase);
        block.first = block.first.min(fact.seq());
        block.outputs = tool
            .outputs
            .each_ref()
            .map(|output| output.as_ref().map(|output| output.as_str().to_owned()));
        let source = |field| Source {
            seq: fact.seq(),
            field,
        };
        if let Some(command) = rsi_conversation::select_field(fact, source(FactField::ToolCommand))
        {
            let window = command.window(0, WINDOW).expect("bounded command window");
            self.add(
                key.clone(),
                &title,
                Role::Tool,
                Piece::from_window(source(FactField::ToolCommand), &window),
            );
        }
        match fact.body() {
            SessionFactBody::ToolIntent { arguments, .. } => {
                if !self.blocks[index].concise {
                    self.add(
                        key,
                        &title,
                        Role::Tool,
                        Piece::json(source(FactField::ToolArguments), arguments, 0),
                    );
                }
            }
            SessionFactBody::ToolRejected {
                arguments,
                rejection,
                ..
            } => {
                self.add(
                    key.clone(),
                    &title,
                    Role::Tool,
                    Piece::json(source(FactField::ToolArguments), arguments, 0),
                );
                self.add(
                    key,
                    &title,
                    Role::Tool,
                    Piece::json(source(FactField::ToolRejection), rejection, 0),
                );
            }
            SessionFactBody::ToolResult { result, .. } => {
                self.project_tool_result(key, &title, fact.seq(), result);
            }
            _ => {}
        }
    }

    fn project_tool_result(
        &mut self,
        key: String,
        title: &str,
        seq: u64,
        result: &rsi_tools_protocol::ToolResult,
    ) {
        let source = |field| Source { seq, field };
        let mut text_present = false;
        for (index, content) in result.content.iter().enumerate() {
            if let ToolContent::Image { media } = content {
                text_present = true;
                self.add(
                    key.clone(),
                    title,
                    Role::Tool,
                    Piece::json(
                        source(FactField::ToolImage {
                            index: u16::try_from(index).expect("validated Tool content index"),
                        }),
                        media,
                        0,
                    ),
                );
            } else if let ToolContent::Text { text } = content {
                text_present = true;
                self.add(
                    key.clone(),
                    title,
                    Role::Tool,
                    Piece::new(
                        source(FactField::ToolText {
                            index: u16::try_from(index).expect("validated Tool content index"),
                        }),
                        text,
                        0,
                        WINDOW,
                    ),
                );
            }
        }
        if !text_present {
            self.add(
                key,
                title,
                Role::Tool,
                Piece::json(source(FactField::ToolValue), &result.value, 0),
            );
        }
    }

    fn block_index(&mut self, key: String, title: &str, role: Role, seq: u64) -> usize {
        let position = self.blocks.iter().position(|block| block.key == key);
        position.unwrap_or_else(|| {
            self.blocks.push(Block {
                markdown: None,
                clock: ProcessClock::default(),
                outcome: None,
                key,
                layout_revision: std::sync::Arc::new(()),
                title: crate::terminal_text(title),
                role,
                pieces: VecDeque::new(),
                sources: SourceIndex::default(),
                collapsed: matches!(role, Role::Tool | Role::Reasoning),
                completed: false,
                concise: false,
                fold: None,
                outputs: [None, None],
                first: seq,
                last: seq,
                request_key: None,
                tool: None,
                request: None,
                time_ms: None,
                discarded: false,
                text_bytes: 0,
                map_bytes: 0,
            });
            self.blocks.len() - 1
        })
    }
    fn add(&mut self, key: String, title: &str, role: Role, piece: Piece) {
        let index = self.block_index(key, title, role, piece.source.seq);
        let block = &mut self.blocks[index];
        if block.sources.position(piece.source).is_some() {
            return;
        }
        let old = block
            .pieces
            .back()
            .is_some_and(|last| piece.source < last.source);
        while !block.pieces.is_empty()
            && (block.text_bytes + piece.text.capacity() > WINDOW
                || block.sources.len() >= MAXIMUM_BLOCK_SOURCES)
        {
            block.evict_piece(old);
            self.earlier = true;
        }
        block.text_bytes += piece.text.capacity();
        block.map_bytes += piece.metadata();
        block.first = block.first.min(piece.source.seq);
        block.last = block.last.max(piece.source.seq);
        let SourceAdmission::Inserted(position) = block
            .sources
            .insert(piece.source)
            .expect("bounded valid presentation source")
        else {
            unreachable!("duplicate checked before admission")
        };
        block.layout_revision = std::sync::Arc::new(());
        block.pieces.insert(position, piece);
    }

    pub fn budgets(&self) -> (usize, usize) {
        (
            self.blocks.iter().map(Block::bytes).sum(),
            self.blocks.capacity() * std::mem::size_of::<Block>()
                + self.blocks.iter().map(Block::metadata).sum::<usize>(),
        )
    }

    fn trim(&mut self, older: bool) {
        loop {
            let (text, metadata) = self.budgets();
            if self.blocks.len() <= MAX_BLOCKS && text <= MAX_TEXT && metadata <= MAX_METADATA {
                break;
            }
            self.earlier |= !older;
            if self.blocks.len() == 1 {
                let block = &mut self.blocks[0];
                block.evict_piece(older);
                block.pieces.shrink_to_fit();
            } else {
                self.blocks
                    .remove(if older { self.blocks.len() - 1 } else { 0 });
                self.blocks.shrink_to_fit();
            }
        }
    }

    /// Validates a frame against one source index, preserving exact gap/UTF-8 checks.
    pub fn contains_anchors(&self, anchors: impl IntoIterator<Item = Anchor>) -> bool {
        let mut pieces: Vec<_> = self.blocks.iter().flat_map(|block| &block.pieces).collect();
        pieces.sort_unstable_by_key(|piece| piece.source);
        anchors.into_iter().all(|anchor| {
            let from = pieces.partition_point(|piece| piece.source < anchor.source);
            pieces[from..]
                .iter()
                .take_while(|piece| piece.source == anchor.source)
                .any(|piece| piece.display_offset(anchor).is_some())
        })
    }

    pub fn locate(&self, anchor: Anchor) -> Option<(usize, usize)> {
        self.blocks
            .iter()
            .enumerate()
            .find_map(|(index, block)| block.offset(anchor).map(|offset| (index, offset)))
    }

    pub fn selected(&self, start: Anchor, end: Anchor) -> Result<String, &'static str> {
        let a = self
            .locate(start)
            .ok_or("Selection starts in unloaded text; reload it before copying")?;
        let b = self
            .locate(end)
            .ok_or("Selection ends in unloaded text; reload it before copying")?;
        let (a, b) = if a <= b { (a, b) } else { (b, a) };
        let mut text = String::new();
        for index in a.0..=b.0 {
            let block = &self.blocks[index];
            let body = block.text();
            let from = if index == a.0 { a.1 } else { 0 };
            let to = if index == b.0 { b.1 } else { body.len() };
            let mut offset = 0;
            for piece in &block.pieces {
                let end = offset + piece.text.len();
                if piece.start > 0 && (index > a.0 || from < offset) && to > offset
                    || piece.truncated_after && from < end && (to > end || index < b.0)
                {
                    return Err(
                        "Selection crosses omitted text; use the detail view or select a smaller range",
                    );
                }
                offset = end;
            }
            if block.discarded && index > a.0 {
                return Err("Selection crosses evicted text; reload the source before copying");
            }
            if index != a.0 {
                text.push_str("\n\n");
            }
            if text.len().saturating_add(to - from) > MAX_TEXT {
                return Err("Selection exceeds 4 MiB; nothing was copied");
            }
            text.push_str(&body[from..to]);
        }
        Ok(text)
    }
}

/// # Panics
/// Panics if a linked serializer rejects its own value.
pub fn json_window(value: &impl serde::Serialize) -> String {
    let window = FieldWindow::json(value, 0, WINDOW).expect("bounded linked JSON serialization");
    let mut text = window.text;
    if window.more {
        text.push_str("\n[JSON display window truncated]");
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    fn process_model_fact(seq: u64, effect: &str, event: LanguageEvent) -> SessionFact {
        use rsi_agent_session_protocol::{EffectId, TurnId};
        SessionFact::new(
            seq,
            seq * 1000,
            SessionFactBody::ModelEvent {
                turn_id: TurnId::new("turn").unwrap(),
                effect_id: EffectId::new(effect).unwrap(),
                purpose: rsi_agent_session_protocol::ModelEventPurpose::Conversation,
                event,
            },
        )
        .unwrap()
    }

    #[test]
    fn reasoning_outcomes_follow_their_request_in_live_and_reverse_history() {
        use rsi_ai_protocol::{AiError, DispatchStatus, ErrorKind, ErrorPhase, FinishReason};
        let finished = |reason| LanguageEvent::Finished {
            reason,
            replay: None,
        };
        let failed = |kind| LanguageEvent::Failed {
            error: AiError::new(
                kind,
                ErrorPhase::Stream,
                DispatchStatus::Dispatched,
                "fixture",
            )
            .unwrap(),
            replay: None,
        };
        for (terminal, expected) in [
            (None, None),
            (
                Some(finished(FinishReason::Stop)),
                Some(ProcessOutcome::Success),
            ),
            (
                Some(finished(FinishReason::ToolCalls)),
                Some(ProcessOutcome::Success),
            ),
            (
                Some(finished(FinishReason::Cancelled)),
                Some(ProcessOutcome::Interrupted),
            ),
            (
                Some(finished(FinishReason::MaxTokens)),
                Some(ProcessOutcome::Interrupted),
            ),
            (
                Some(finished(FinishReason::ContentFilter)),
                Some(ProcessOutcome::Interrupted),
            ),
            (
                Some(failed(ErrorKind::Server)),
                Some(ProcessOutcome::Failed),
            ),
            (
                Some(failed(ErrorKind::Cancelled)),
                Some(ProcessOutcome::Interrupted),
            ),
        ] {
            let reasoning = || LanguageEvent::ContentDelta {
                index: 0,
                delta: ContentDelta::Reasoning("Private reasoning".into()),
            };
            let mut facts = vec![
                process_model_fact(1, "subject", reasoning()),
                process_model_fact(2, "subject", LanguageEvent::ContentFinished { index: 0 }),
            ];
            if let Some(terminal) = terminal {
                facts.push(process_model_fact(3, "subject", terminal));
            }
            facts.extend([
                process_model_fact(4, "other", reasoning()),
                process_model_fact(5, "other", finished(FinishReason::Stop)),
            ]);
            for reverse in [false, true] {
                let mut transcript = Transcript::default();
                let order: Vec<_> = if reverse {
                    facts.iter().rev().collect()
                } else {
                    facts.iter().collect()
                };
                for fact in order {
                    transcript.apply_history(fact);
                }
                let processes: Vec<_> = transcript
                    .blocks
                    .iter()
                    .filter(|block| block.role == Role::Reasoning)
                    .collect();
                assert_eq!(processes.len(), 2);
                assert_eq!(processes[0].outcome, expected);
                assert_eq!(processes[1].outcome, Some(ProcessOutcome::Success));
                let restored = Viewport::capture(&transcript, None, 80, 24)
                    .0
                    .restore()
                    .unwrap();
                let outcomes: Vec<_> = restored
                    .blocks
                    .iter()
                    .filter(|block| block.role == Role::Reasoning)
                    .map(|block| block.outcome)
                    .collect();
                assert_eq!(outcomes, [expected, Some(ProcessOutcome::Success)]);
            }
        }
    }

    #[test]
    fn tool_marker_outcomes_use_result_semantics_and_rejections() {
        use rsi_agent_session_protocol::{EffectId, TurnId};
        use rsi_tools_protocol::{ToolResult, ToolResultIdentity};
        let identity = ToolResultIdentity::new("owner", "invoke", "call", "a".repeat(64)).unwrap();
        for (value, is_error, expected) in [
            (
                serde_json::json!({"exit_code":0}),
                false,
                ProcessOutcome::Success,
            ),
            (
                serde_json::json!({"exit_code":7}),
                false,
                ProcessOutcome::Failed,
            ),
            (
                serde_json::json!({"signal":15}),
                false,
                ProcessOutcome::Failed,
            ),
            (
                serde_json::json!({"exit_code":0}),
                true,
                ProcessOutcome::Failed,
            ),
        ] {
            let mut transcript = Transcript::default();
            transcript.apply(
                &SessionFact::new(
                    1,
                    1,
                    SessionFactBody::ToolResult {
                        turn_id: TurnId::new("turn").unwrap(),
                        effect_id: EffectId::new("effect").unwrap(),
                        identity: identity.clone(),
                        result: ToolResult::new(value, vec![], is_error).unwrap(),
                        conclusion: None,
                    },
                )
                .unwrap(),
            );
            assert_eq!(transcript.blocks[0].outcome, Some(expected));
        }
        let mut transcript = Transcript::default();
        transcript.apply(
            &SessionFact::new(
                1,
                1,
                SessionFactBody::ToolRejected {
                    turn_id: TurnId::new("turn").unwrap(),
                    effect_id: EffectId::new("effect").unwrap(),
                    identity,
                    name: "bash".into(),
                    arguments: serde_json::json!({"command":"denied"}),
                    rejection: rsi_agent_session_protocol::ToolRejection::PolicyDenied {
                        contribution_id: rsi_agent_session_protocol::ContributionId::new(
                            "fixture.policy",
                        )
                        .unwrap(),
                        reason: "denied".into(),
                    },
                },
            )
            .unwrap(),
        );
        assert_eq!(transcript.blocks[0].outcome, Some(ProcessOutcome::Failed));
    }

    #[test]
    fn viewport_capture_does_not_report_resident_eviction_when_scrolling() {
        let mut transcript = Transcript::default();
        for seq in 1..=3 {
            transcript.add(
                "response".into(),
                "Assistant",
                Role::Assistant,
                piece(seq, "hello"),
            );
        }
        let top = transcript.blocks[0].anchor(11).unwrap();
        let window = Viewport::capture(&transcript, Some(top), 80, 24)
            .0
            .restore()
            .unwrap();
        assert!(window.blocks[0].pieces.len() < transcript.blocks[0].pieces.len());
        assert!(
            !window.blocks[0].discarded,
            "viewport clipping is not resident eviction"
        );
    }
    #[test]
    fn stale_viewport_anchor_starts_at_oldest_retained_content() {
        let mut transcript = Transcript::default();
        for seq in 1..=100 {
            transcript.add(
                seq.to_string(),
                "Assistant",
                Role::Assistant,
                piece(seq, "hello"),
            );
        }
        let stale = Anchor {
            source: Source {
                seq: 0,
                field: FactField::ModelText,
            },
            offset: 0,
        };
        let window = Viewport::capture(&transcript, Some(stale), 80, 24)
            .0
            .restore()
            .unwrap();
        assert_eq!(window.blocks[0].key, transcript.blocks[0].key);
    }
    #[test]
    fn external_source_coordinate_overflow_is_rejected() {
        let source = Source {
            seq: 1,
            field: FactField::TurnInput,
        };
        let piece = Piece::from_window(source, &FieldWindow::text("\x1bhello", 0, 64).unwrap());
        assert_eq!(
            piece.display_offset(Anchor {
                source,
                offset: usize::MAX
            }),
            None
        );
    }

    fn piece(seq: u64, text: &str) -> Piece {
        Piece::new(
            Source {
                seq,
                field: FactField::ModelText,
            },
            text,
            0,
            WINDOW,
        )
    }

    #[test]
    fn ascending_history_page_fills_missing_interior_sources_without_losing_live_selection() {
        let mut transcript = Transcript::default();
        for seq in [3, 4] {
            transcript.add(
                "response".into(),
                "Assistant",
                Role::Assistant,
                piece(seq, &seq.to_string()),
            );
        }
        let start = transcript.blocks[0].anchor(0).unwrap();
        let end = transcript.blocks[0].anchor(2).unwrap();
        for seq in [1, 2, 3, 4] {
            transcript.add(
                "response".into(),
                "Assistant",
                Role::Assistant,
                piece(seq, &seq.to_string()),
            );
        }
        assert_eq!(transcript.blocks[0].text(), "1234");
        assert_eq!(transcript.selected(start, end).unwrap(), "34");
        assert_eq!(transcript.blocks[0].anchor(2).unwrap().source.seq, 3);
    }

    #[test]
    fn sanitization_selection_and_prepend_preserve_source_identity() {
        let mut transcript = Transcript::default();
        transcript.add(
            "response".into(),
            "Assistant",
            Role::Assistant,
            piece(10, "A\x1b]52;c;evil\x07中👩"),
        );
        transcript.add(
            "response".into(),
            "Assistant",
            Role::Assistant,
            piece(11, "🏽‍💻e"),
        );
        transcript.add(
            "response".into(),
            "Assistant",
            Role::Assistant,
            piece(12, "\u{301}"),
        );
        let block = &transcript.blocks[0];
        let a = block.anchor(0).unwrap();
        let b = block.anchor(block.text().len()).unwrap();
        let expected = "A�]52;c;evil�中👩🏽‍💻e\u{301}";
        assert_eq!(transcript.selected(a, b).unwrap(), expected);
        transcript.add(
            "response".into(),
            "Assistant",
            Role::Assistant,
            piece(9, "prefix "),
        );
        assert_eq!(transcript.selected(a, b).unwrap(), expected);
        assert_eq!(transcript.selected(b, a).unwrap(), expected);
    }

    #[test]
    fn selection_inside_loaded_window_is_complete_but_crossing_a_gap_is_not() {
        let mut transcript = Transcript::default();
        transcript.add(
            "large".into(),
            "Assistant",
            Role::Assistant,
            piece(1, &"x".repeat(WINDOW + 10)),
        );
        let a = transcript.blocks[0].anchor(0).unwrap();
        let b = transcript.blocks[0].anchor(10).unwrap();
        assert_eq!(transcript.selected(a, b).unwrap(), "xxxxxxxxxx");
        transcript.add("next".into(), "Assistant", Role::Assistant, piece(2, "end"));
        let c = transcript.blocks[1].anchor(3).unwrap();
        assert!(transcript.selected(a, c).is_err());
    }

    #[test]
    fn micro_deltas_and_large_cards_keep_budgets_and_latest_text() {
        let mut transcript = Transcript::default();
        for seq in 1..=20_000 {
            transcript.add(
                "stream".into(),
                "Assistant",
                Role::Assistant,
                piece(seq, "abcdefgh"),
            );
            transcript.trim(false);
        }
        for seq in 20_001..=20_030 {
            transcript.add(
                format!("large:{seq}"),
                "Tool",
                Role::Tool,
                piece(seq, &"x".repeat(WINDOW)),
            );
            transcript.trim(false);
        }
        let (text, metadata) = transcript.budgets();
        assert!(
            text <= MAX_TEXT && metadata <= MAX_METADATA && transcript.blocks.len() <= MAX_BLOCKS
        );
        assert!(!transcript.blocks.last().unwrap().text().is_empty());
        for block in &transcript.blocks {
            assert_eq!(
                block.sources.iter().collect::<Vec<_>>(),
                block
                    .pieces
                    .iter()
                    .map(|piece| piece.source)
                    .collect::<Vec<_>>()
            );
        }
        assert!(transcript.earlier);
    }

    #[test]
    fn source_window_mapping_accounts_for_replaced_controls() {
        let raw = "\x1b中a\u{202e}👩🏽‍💻end";
        let piece = Piece::new(
            Source {
                seq: 7,
                field: FactField::InputText { index: 2 },
            },
            raw,
            0,
            WINDOW,
        );
        for (offset, _) in piece.text.char_indices() {
            let anchor = piece.anchor(offset);
            assert!(raw.is_char_boundary(anchor.offset));
            assert_eq!(piece.display_offset(anchor), Some(offset));
        }
        assert_eq!(piece.anchor(piece.text.len()).offset, raw.len());
    }

    #[test]
    fn tool_only_request_metadata_follows_its_explicit_source_effect_in_both_read_orders() {
        use rsi_agent_session_protocol::{EffectId, TurnId};
        let turn = TurnId::new("turn").unwrap();
        let source = EffectId::new("source").unwrap();
        let model = SessionFact::new(
            1,
            1,
            SessionFactBody::ModelStarted {
                turn_id: turn.clone(),
                effect_id: source.clone(),
            },
        )
        .unwrap();
        let unrelated = SessionFact::new(
            2,
            2,
            SessionFactBody::ModelStarted {
                turn_id: turn.clone(),
                effect_id: EffectId::new("unrelated").unwrap(),
            },
        )
        .unwrap();
        let intent = SessionFact::new(
            3,
            3,
            SessionFactBody::ToolIntent {
                turn_id: turn.clone(),
                effect_id: EffectId::new("tool").unwrap(),
                source_model_effect_id: source.clone(),
                identity: rsi_tools_protocol::ToolResultIdentity::new(
                    "owner",
                    "invoke",
                    "call",
                    "a".repeat(64),
                )
                .unwrap(),
                name: "bash".into(),
                arguments: serde_json::json!({"command":"true"}),
                approval: None,
                parallel_safe: false,
            },
        )
        .unwrap();
        let finished = SessionFact::new(
            4,
            4,
            SessionFactBody::ModelEvent {
                turn_id: turn.clone(),
                effect_id: source.clone(),
                purpose: rsi_agent_session_protocol::ModelEventPurpose::Conversation,
                event: LanguageEvent::Finished {
                    reason: rsi_ai_protocol::FinishReason::ToolCalls,
                    replay: None,
                },
            },
        )
        .unwrap();
        for reverse in [false, true] {
            let mut transcript = Transcript::default();
            let mut facts = vec![&model, &unrelated, &intent, &finished];
            if reverse {
                facts.reverse();
            }
            for fact in facts {
                transcript.apply_history(fact);
            }
            assert_eq!(
                transcript
                    .blocks
                    .iter()
                    .map(|block| block.role)
                    .collect::<Vec<_>>(),
                vec![Role::Metadata, Role::Tool, Role::Metadata]
            );
            assert_eq!(transcript.blocks[2].key, request_key(&turn, &source));
            assert!(
                transcript
                    .blocks
                    .iter()
                    .filter(|block| block.role == Role::Metadata)
                    .all(|block| !block.completed)
            );
            assert_eq!(
                transcript.blocks[0].key,
                request_key(&turn, &EffectId::new("unrelated").unwrap())
            );
        }
    }

    fn assert_tool_action_titles(mut preview: ToolState) {
        for (name, argument, running, completed) in [
            (
                "bash",
                "pwd && ls -la",
                "Running pwd && ls -la",
                "Ran pwd && ls -la",
            ),
            ("directory_list", ".", "Listing .", "Listed ."),
            (
                "file_read",
                "src/main.rs",
                "Reading src/main.rs",
                "Read src/main.rs",
            ),
            (
                "spawn_agent",
                "review",
                "Calling spawn_agent(review)",
                "Called spawn_agent(review)",
            ),
        ] {
            preview.name = Some(name.into());
            preview.argument_summary = Some(argument.into());
            preview.phase = rsi_conversation::ToolPhase::Running;
            assert_eq!(tool_title(&preview, false), running);
            preview.phase =
                rsi_conversation::ToolPhase::Settled(rsi_conversation::ToolOutcome::Completed);
            assert_eq!(tool_title(&preview, false), completed);
        }
    }

    #[test]
    fn tool_backfill_keeps_result_status_and_exact_source_without_pairing_another_owner() {
        use rsi_agent_session_protocol::{EffectId, TurnId};
        use rsi_tools_protocol::{ToolResult, ToolResultIdentity};
        let identity = ToolResultIdentity::new("owner", "invoke", "call", "a".repeat(64)).unwrap();
        let turn_id = TurnId::new("turn").unwrap();
        let effect_id = EffectId::new("effect").unwrap();
        let intent = SessionFact::new(
            4,
            1,
            SessionFactBody::ToolIntent {
                source_model_effect_id: EffectId::new("source-model").unwrap(),
                turn_id: turn_id.clone(),
                effect_id: effect_id.clone(),
                identity: identity.clone(),
                name: "bash".into(),
                arguments: serde_json::json!({"command":"exit 7"}),
                approval: None,
                parallel_safe: false,
            },
        )
        .unwrap();
        let started = SessionFact::new(
            5,
            1,
            SessionFactBody::ToolStarted {
                turn_id: turn_id.clone(),
                effect_id: effect_id.clone(),
                identity: identity.clone(),
            },
        )
        .unwrap();
        let result_body = |identity| SessionFactBody::ToolResult {
            turn_id: turn_id.clone(),
            effect_id: effect_id.clone(),
            identity,
            result: ToolResult::new(
                serde_json::json!({"exit_code":7,"stdout":{"full_output":"x".repeat(1024*1024)}}),
                vec![ToolContent::Text {
                    text: "exit status: 7".into(),
                }],
                false,
            )
            .unwrap(),
            conclusion: None,
        };
        let result = SessionFact::new(6, 1, result_body(identity)).unwrap();
        let mut live = Transcript::default();
        live.apply(&intent);
        assert_eq!(live.blocks[0].title, "Run exit 7");
        assert_tool_action_titles(live.blocks[0].tool.clone().unwrap());
        live.apply(&started);
        assert_eq!(live.blocks[0].title, "Running exit 7");
        live.apply(&result);
        assert_eq!(live.blocks[0].outcome, Some(ProcessOutcome::Failed));
        assert_eq!(live.blocks[0].title, "Failed to run exit 7");
        assert!(
            !live.blocks[0].text().contains("command"),
            "typed summary replaces duplicate JSON arguments"
        );
        assert_eq!(
            live.blocks[0].tool.as_ref().unwrap().arguments.unwrap().seq,
            4
        );
        let mut history = Transcript::default();
        history.apply(&result);
        history.apply_history(&intent);
        history.apply_history(&started);
        history.apply_history(&intent);
        assert_eq!(history.blocks[0].title, live.blocks[0].title);
        assert_eq!(history.blocks[0].outcome, live.blocks[0].outcome);
        assert_eq!(history.blocks[0].text(), live.blocks[0].text());
        assert_eq!(history.blocks[0].first, 4);
        assert!(history.blocks[0].outputs[0].is_none());
        let other =
            ToolResultIdentity::new("other-owner", "invoke", "call", "a".repeat(64)).unwrap();
        history.apply(&SessionFact::new(7, 1, result_body(other)).unwrap());
        assert_eq!(history.blocks.len(), 2);
        assert_eq!(
            history.blocks[1].title,
            "Failed to call Tool (intent not loaded)"
        );
        assert!(history.blocks[1].tool.as_ref().unwrap().arguments.is_none());
        assert!(history.budgets().1 < MAX_METADATA);
    }

    #[test]
    fn legal_fact_larger_than_display_budget_has_exact_recoverable_windows() {
        let text = "中".repeat(1024 * 1024);
        let result = rsi_tools_protocol::ToolResult::new(
            serde_json::json!({"large":"v".repeat(3*1024*1024)}),
            vec![ToolContent::Text { text: text.clone() }],
            false,
        )
        .unwrap();
        let fact = SessionFact::new(
            7,
            7,
            SessionFactBody::ToolResult {
                turn_id: rsi_agent_session_protocol::TurnId::new("turn").unwrap(),
                effect_id: rsi_agent_session_protocol::EffectId::new("effect").unwrap(),
                identity: rsi_tools_protocol::ToolResultIdentity::new(
                    "owner",
                    "turn",
                    "call",
                    "a".repeat(64),
                )
                .unwrap(),
                result,
                conclusion: None,
            },
        )
        .unwrap();
        assert!(fact.encoded_len() > MAX_TEXT);
        let mut transcript = Transcript::default();
        transcript.apply(&fact);
        assert!(transcript.budgets().0 <= WINDOW);
        let source = Source {
            seq: 7,
            field: FactField::ToolText { index: 0 },
        };
        let first = Transcript::window(&fact, source, 0).unwrap();
        let next = first.anchor(first.text.len()).offset;
        let second = Transcript::window(&fact, source, next).unwrap();
        assert_eq!(second.start, next);
        assert_eq!(second.text, text[next..next + second.text.len()]);
        assert!(
            Transcript::window(
                &fact,
                Source {
                    seq: 8,
                    field: FactField::ToolText { index: 0 }
                },
                0
            )
            .is_none()
        );
    }

    #[test]
    fn json_source_windows_advance_beyond_the_initial_prefix_without_full_serialization() {
        let value = serde_json::json!({"body":"中a".repeat(WINDOW)});
        let complete = serde_json::to_string_pretty(&value).unwrap();
        let source = Source {
            seq: 1,
            field: FactField::ToolArguments,
        };
        let mut offset = 0;
        let mut recovered = String::new();
        loop {
            let piece = Piece::json(source, &value, offset);
            assert_eq!(piece.start, offset);
            offset = piece.anchor(piece.text.len()).offset;
            recovered.push_str(&piece.text);
            if !piece.truncated_after {
                break;
            }
        }
        assert_eq!(recovered, complete);
    }
    #[test]
    fn indexed_anchors_preserve_sanitized_unicode_and_piece_boundaries() {
        let mut transcript = Transcript::default();
        for (index, text) in ["A\x1b中", "", "e", "\u{301}👩", "🏽‍💻", ""]
            .iter()
            .enumerate()
        {
            transcript.add(
                "one".into(),
                "Assistant",
                Role::Assistant,
                piece(index as u64 + 1, text),
            );
        }
        let block = &transcript.blocks[0];
        let text = block.text();
        let indexed = block.anchor_index();
        for offset in text
            .char_indices()
            .map(|(offset, _)| offset)
            .chain([text.len(), text.len() + 1])
        {
            assert_eq!(indexed.anchor(offset), block.anchor(offset));
        }
    }
}

#[cfg(test)]
#[path = "transcript_media_tests.rs"]
mod media_tests;

#[path = "viewport.rs"]
mod viewport;
pub(crate) use viewport::FoldCache;
pub use viewport::Viewport;
