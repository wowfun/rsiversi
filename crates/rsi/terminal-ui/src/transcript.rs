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
        (offset <= self.text.len() && self.text.is_char_boundary(offset)).then_some(offset)
    }

    fn metadata(&self) -> usize {
        self.mapping.capacity() * std::mem::size_of::<Mapping>()
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum Role {
    User,
    Assistant,
    Reasoning,
    Tool,
    Status,
}

#[derive(Clone, Debug)]
pub struct Block {
    pub key: String,
    pub layout_revision: std::sync::Arc<()>,
    pub title: String,
    pub role: Role,
    pub pieces: VecDeque<Piece>,
    sources: SourceIndex,
    pub collapsed: bool,
    pub outputs: [Option<String>; 2],
    pub first: u64,
    pub discarded: bool,
    text_bytes: usize,
    map_bytes: usize,
    pub tool: Option<ToolState>,
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
            + self.sources.owned_bytes()
            + self.tool.as_ref().map_or(0, ToolState::owned_bytes)
            + self.title.capacity()
            + self.pieces.capacity() * std::mem::size_of::<Piece>()
            + self.map_bytes
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
    pub(crate) fn reuse_layout_revisions(&mut self, previous: &Self) {
        for block in &mut self.blocks {
            if let Some(old) = previous.blocks.iter().find(|old| {
                old.key == block.key
                    && old.role == block.role
                    && old.collapsed == block.collapsed
                    && old.pieces == block.pieces
            }) {
                block.layout_revision = old.layout_revision.clone();
            }
        }
    }

    pub fn apply(&mut self, fact: &SessionFact) {
        self.project(fact);
        if !self.blocks.is_sorted_by_key(|block| block.first) {
            self.blocks.sort_by_key(|block| block.first);
        }
        self.trim(false);
    }

    pub fn apply_history(&mut self, fact: &SessionFact) {
        self.project(fact);
        self.blocks.sort_by_key(|block| block.first);
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
        if BlockIdentity::tool(fact).is_some() {
            self.project_tool(fact);
            return;
        }
        let seq = fact.seq();
        let mut add = |key: String, title: String, role, field, text: &str| {
            let source = Source { seq, field };
            self.add(key, &title, role, Piece::new(source, text, 0, WINDOW));
        };
        match fact.body() {
            SessionFactBody::TurnAccepted { turn_id, text, .. } => add(
                BlockIdentity::TurnInput { turn: turn_id }.key(),
                "You".into(),
                Role::User,
                FactField::TurnInput,
                text,
            ),
            SessionFactBody::InputMessageEntered {
                source, content, ..
            } => {
                let (key, title, role) = match source {
                    InputMessageSource::Human { message_id } => (
                        BlockIdentity::Message {
                            message: message_id,
                        }
                        .key(),
                        "You",
                        Role::User,
                    ),
                    InputMessageSource::Agent { message_id, .. }
                    | InputMessageSource::Completion { message_id, .. } => (
                        BlockIdentity::Message {
                            message: message_id,
                        }
                        .key(),
                        "Agent message",
                        Role::Status,
                    ),
                    _ => return,
                };
                for (index, content) in content.iter().enumerate() {
                    if let AgentMessageContent::Image { media } = content {
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
                ..
            } => {
                let (role, title, text, field) = match delta {
                    ContentDelta::Text(text) => {
                        (Role::Assistant, "Assistant", text, FactField::ModelText)
                    }
                    ContentDelta::Reasoning(text) => (
                        Role::Reasoning,
                        "Reasoning",
                        text,
                        FactField::ModelReasoning,
                    ),
                    ContentDelta::ToolArguments(_) => return,
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
            }
            SessionFactBody::TurnTerminal { turn_id, outcome } => {
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
    }

    fn project_tool(&mut self, fact: &SessionFact) {
        let key = BlockIdentity::tool(fact).expect("Tool Fact").key();
        let index = self.block_index(key.clone(), "Tool", Role::Tool, fact.seq());
        let block = &mut self.blocks[index];
        let tool = if let Some(tool) = &mut block.tool {
            tool.observe(fact);
            tool
        } else {
            block
                .tool
                .insert(ToolState::from_fact(fact).expect("Tool Fact"))
        };
        let title = crate::terminal_text(&format!(
            "{}{}",
            tool.title(),
            if !tool.intent_present && tool.phase != rsi_conversation::ToolPhase::Rejected {
                " · intent not loaded"
            } else {
                ""
            }
        ));
        block.title.clone_from(&title);
        block.first = block.first.min(fact.seq());
        block.outputs = tool
            .outputs
            .each_ref()
            .map(|output| output.as_ref().map(|output| output.as_str().to_owned()));
        let source = |field| Source {
            seq: fact.seq(),
            field,
        };
        match fact.body() {
            SessionFactBody::ToolIntent { arguments, .. } => {
                self.add(
                    key,
                    &title,
                    Role::Tool,
                    Piece::json(source(FactField::ToolArguments), arguments, 0),
                );
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
                key,
                layout_revision: std::sync::Arc::new(()),
                title: crate::terminal_text(title),
                role,
                pieces: VecDeque::new(),
                sources: SourceIndex::default(),
                collapsed: matches!(role, Role::Tool | Role::Reasoning),
                outputs: [None, None],
                first: seq,
                tool: None,
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
        let window = Viewport::capture(&transcript, Some(top), 24)
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
        let window = Viewport::capture(&transcript, Some(stale), 24)
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
        };
        let result = SessionFact::new(6, 1, result_body(identity)).unwrap();
        let mut live = Transcript::default();
        live.apply(&intent);
        assert_eq!(live.blocks[0].title, "bash · prepared");
        live.apply(&started);
        assert_eq!(live.blocks[0].title, "bash · running");
        live.apply(&result);
        assert_eq!(live.blocks[0].title, "bash · command failed");
        let mut history = Transcript::default();
        history.apply(&result);
        history.apply_history(&intent);
        history.apply_history(&started);
        history.apply_history(&intent);
        assert_eq!(history.blocks[0].title, live.blocks[0].title);
        assert_eq!(history.blocks[0].text(), live.blocks[0].text());
        assert_eq!(history.blocks[0].first, 4);
        assert!(history.blocks[0].outputs[0].is_none());
        let other =
            ToolResultIdentity::new("other-owner", "invoke", "call", "a".repeat(64)).unwrap();
        history.apply(&SessionFact::new(7, 1, result_body(other)).unwrap());
        assert_eq!(history.blocks.len(), 2);
        assert_eq!(
            history.blocks[1].title,
            "Tool · command failed · intent not loaded"
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

/// A portable viewport keeps source coordinates while discarding controller metadata.
#[derive(Debug, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Viewport {
    blocks: Vec<WindowBlock>,
}
#[derive(Debug, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct WindowBlock {
    key: String,
    title: String,
    role: Role,
    collapsed: bool,
    discarded: bool,
    pieces: Vec<WindowPiece>,
}
#[derive(Debug, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct WindowPiece {
    source: Source,
    text: String,
    start: usize,
    omitted: bool,
    truncated_after: bool,
    mapping: Vec<(usize, usize)>,
}
impl Viewport {
    pub const MAXIMUM_TEXT: usize = 512 * 1024;
    pub fn capture(transcript: &Transcript, top: Option<Anchor>, height: u16) -> Self {
        let position = top.map(|top| transcript.locate(top).unwrap_or((0, 0)));
        let mut remaining = Self::MAXIMUM_TEXT;
        let mut blocks = Vec::new();
        let count = (usize::from(height) * 2).clamp(1, MAX_BLOCKS);
        let range: Box<dyn Iterator<Item = usize>> = match position {
            Some((index, _)) => Box::new(index..transcript.blocks.len()),
            None => Box::new((0..transcript.blocks.len()).rev()),
        };
        for index in range.take(count) {
            let block = &transcript.blocks[index];
            let mut pieces = Vec::new();
            let mut skipped = 0;
            let indexes: Box<dyn Iterator<Item = usize>> = if position.is_some() {
                Box::new(0..block.pieces.len())
            } else {
                Box::new((0..block.pieces.len()).rev())
            };
            for i in indexes {
                let piece = &block.pieces[i];
                let end = skipped + piece.text.len();
                if position.is_some_and(|(at, offset)| at == index && end < offset) {
                    skipped = end;
                    continue;
                }
                if piece.text.len() > remaining {
                    break;
                }
                remaining -= piece.text.len();
                pieces.push(WindowPiece {
                    source: piece.source,
                    text: piece.text.clone(),
                    start: piece.start,
                    omitted: piece.omitted,
                    truncated_after: piece.truncated_after,
                    mapping: piece
                        .mapping
                        .iter()
                        .map(|run| (run.display, run.source))
                        .collect(),
                });
                skipped = end;
            }
            if position.is_none() {
                pieces.reverse();
            }
            if pieces.is_empty() && !block.pieces.is_empty() {
                break;
            }
            blocks.push(WindowBlock {
                key: block.key.clone(),
                title: block.title.clone(),
                role: block.role,
                collapsed: block.collapsed,
                discarded: block.discarded,
                pieces,
            });
            if remaining == 0 {
                break;
            }
        }
        if position.is_none() {
            blocks.reverse();
        }
        Self { blocks }
    }
    pub fn restore(self) -> Result<Transcript, &'static str> {
        if self.blocks.len() > MAX_BLOCKS {
            return Err("viewport block bound");
        }
        let mut result = Transcript::default();
        let mut text_bytes = 0usize;
        let mut map_bytes = 0usize;
        let mut keys = std::collections::BTreeSet::new();
        for block in self.blocks {
            if block.key.len() > 4096
                || block.title.len() > 4096
                || block.pieces.len() > MAXIMUM_BLOCK_SOURCES
                || !keys.insert(block.key.clone())
                || crate::terminal_text(&block.title) != block.title
            {
                return Err("invalid viewport block");
            }
            let index = result.block_index(block.key, &block.title, block.role, 1);
            let target = &mut result.blocks[index];
            target.collapsed = block.collapsed;
            target.discarded = block.discarded;
            for piece in block.pieces {
                text_bytes = text_bytes
                    .checked_add(piece.text.len())
                    .ok_or("viewport overflow")?;
                map_bytes = map_bytes
                    .checked_add(piece.mapping.len() * size_of::<Mapping>())
                    .ok_or("viewport overflow")?;
                if text_bytes > Self::MAXIMUM_TEXT
                    || map_bytes > MAX_METADATA
                    || piece.source.seq == 0
                    || piece.mapping.first().copied() != Some((0, piece.start))
                    || crate::terminal_text(&piece.text) != piece.text
                {
                    return Err("invalid viewport piece");
                }
                let mut previous = None;
                for &(display, source) in &piece.mapping {
                    if !piece.text.is_char_boundary(display)
                        || source < piece.start
                        || source.checked_add(piece.text.len()).is_none()
                        || previous.is_some_and(|(d, s)| display <= d || source < s)
                    {
                        return Err("invalid source mapping");
                    }
                    previous = Some((display, source));
                }
                if !matches!(
                    target
                        .sources
                        .insert(piece.source)
                        .map_err(|_| "invalid source index")?,
                    SourceAdmission::Inserted(_)
                ) {
                    return Err("duplicate viewport source");
                }
                let mapping = piece
                    .mapping
                    .into_iter()
                    .map(|(display, source)| Mapping { display, source })
                    .collect();
                let piece = Piece {
                    source: piece.source,
                    text: piece.text,
                    start: piece.start,
                    omitted: piece.omitted,
                    truncated_after: piece.truncated_after,
                    mapping,
                };
                target.text_bytes += piece.text.capacity();
                target.map_bytes += piece.metadata();
                target.first = target.first.min(piece.source.seq);
                target.pieces.push_back(piece);
            }
        }
        Ok(result)
    }
}
