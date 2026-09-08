//! Partial historical projection; no model-context replay or retained observation leases.
use super::super::{ContentDelta, LanguageEvent, SessionFact, SessionFactBody, ToolContent};
use rsi_agent_session_protocol::{AgentMessageContent, InputMessageSource};
pub(super) use rsi_conversation::SourceRef as Source;
use rsi_conversation::{FactField, FieldWindow, ToolOutcome};
use std::collections::VecDeque;

pub(super) const MAX_BLOCKS: usize = 512;
pub(super) const MAX_TEXT: usize = 4 * 1024 * 1024;
pub(super) const MAX_METADATA: usize = 8 * 1024 * 1024;
pub(super) const WINDOW: usize = 256 * 1024;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct Anchor {
    pub(super) source: Source,
    pub(super) offset: usize,
}

#[derive(Clone, Copy, Debug)]
struct Mapping {
    display: usize,
    source: usize,
}

#[derive(Clone, Debug)]
pub(super) struct Piece {
    pub(super) source: Source,
    pub(super) text: String,
    pub(super) omitted: bool,
    pub(super) truncated_after: bool,
    pub(super) start: usize,
    mapping: Vec<Mapping>,
}

impl Piece {
    fn json(source: Source, value: &impl serde::Serialize, start: usize) -> Self {
        Self::from_window(
            source,
            &FieldWindow::json(value, start, WINDOW).expect("bounded linked JSON serialization"),
        )
    }

    fn from_window(source: Source, window: &FieldWindow) -> Self {
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
            let filtered = super::super::terminal_character(character);
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

    pub(super) fn anchor(&self, offset: usize) -> Anchor {
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

    pub(super) fn display_offset(&self, anchor: Anchor) -> Option<usize> {
        if anchor.source != self.source || anchor.offset < self.start {
            return None;
        }
        let run = &self.mapping[self
            .mapping
            .partition_point(|run| run.source <= anchor.offset)
            .saturating_sub(1)];
        let offset = run.display + anchor.offset.saturating_sub(run.source);
        (offset <= self.text.len() && self.text.is_char_boundary(offset)).then_some(offset)
    }

    fn metadata(&self) -> usize {
        self.mapping.capacity() * std::mem::size_of::<Mapping>()
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum Role {
    User,
    Assistant,
    Reasoning,
    Tool,
    Status,
}

#[derive(Clone, Debug)]
pub(super) struct Block {
    pub(super) key: String,
    pub(super) title: String,
    pub(super) role: Role,
    pub(super) pieces: VecDeque<Piece>,
    pub(super) collapsed: bool,
    pub(super) outputs: [Option<String>; 2],
    pub(super) first: u64,
    pub(super) discarded: bool,
    text_bytes: usize,
    map_bytes: usize,
    status_seq: u64,
}

/// One per-render index; source lookup does not rescan all streamed pieces per cell.
pub(super) struct AnchorIndex<'a>(Vec<(usize, &'a Piece)>);
impl AnchorIndex<'_> {
    pub(super) fn anchor(&self, offset: usize) -> Option<Anchor> {
        let index = self
            .0
            .partition_point(|(start, _)| *start <= offset)
            .checked_sub(1)?;
        let (start, piece) = self.0[index];
        (offset - start <= piece.text.len()).then(|| piece.anchor(offset - start))
    }
}

impl Block {
    pub(super) fn anchor_index(&self) -> AnchorIndex<'_> {
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
    pub(super) fn text(&self) -> String {
        self.pieces
            .iter()
            .map(|piece| piece.text.as_str())
            .collect()
    }
    pub(super) fn bytes(&self) -> usize {
        self.text_bytes
    }
    fn metadata(&self) -> usize {
        self.key.capacity()
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
    pub(super) fn anchor(&self, mut offset: usize) -> Option<Anchor> {
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
    pub(super) fn offset(&self, anchor: Anchor) -> Option<usize> {
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
pub(super) struct Transcript {
    pub(super) blocks: Vec<Block>,
    pub(super) earlier: bool,
}

impl Transcript {
    pub(super) fn apply(&mut self, fact: &SessionFact) {
        self.project(fact);
        if !self.blocks.is_sorted_by_key(|block| block.first) {
            self.blocks.sort_by_key(|block| block.first);
        }
        self.trim(false);
    }

    pub(super) fn apply_history(&mut self, fact: &SessionFact) {
        self.project(fact);
        self.blocks.sort_by_key(|block| block.first);
        self.trim(true);
    }

    pub(super) fn window(fact: &SessionFact, source: Source, start: usize) -> Option<Piece> {
        let window = rsi_conversation::select_field(fact, source)?
            .window(start, WINDOW)
            .ok()?;
        Some(Piece::from_window(source, &window))
    }

    #[allow(clippy::too_many_lines)] // One exhaustive projection owns the supported Fact payload fields.
    fn project(&mut self, fact: &SessionFact) {
        let seq = fact.seq();
        let mut add = |key: String, title: String, role, field, text: &str| {
            let source = Source { seq, field };
            self.add(key, &title, role, Piece::new(source, text, 0, WINDOW));
        };
        match fact.body() {
            SessionFactBody::TurnAccepted { turn_id, text, .. } => add(
                format!("input:{turn_id}"),
                "You".into(),
                Role::User,
                FactField::TurnInput,
                text,
            ),
            SessionFactBody::InputMessageEntered {
                source, content, ..
            } => {
                let (key, title, role) = match source {
                    InputMessageSource::Human { message_id } => {
                        (format!("input:{message_id}"), "You", Role::User)
                    }
                    InputMessageSource::Agent { message_id, .. }
                    | InputMessageSource::Completion { message_id, .. } => {
                        (format!("input:{message_id}"), "Agent message", Role::Status)
                    }
                    _ => return,
                };
                for (index, content) in content.iter().enumerate() {
                    if let AgentMessageContent::Text { text } = content {
                        add(
                            key.clone(),
                            title.into(),
                            role,
                            FactField::InputText {
                                index: u16::try_from(index)
                                    .expect("validated message content index"),
                            },
                            text,
                        );
                    }
                }
            }
            SessionFactBody::ModelEvent {
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
                    format!("model:{effect_id}:{index}"),
                    title.into(),
                    role,
                    field,
                    text,
                );
            }
            SessionFactBody::ToolIntent {
                effect_id,
                name,
                arguments,
                ..
            } => {
                let key = format!("tool:{effect_id}");
                self.add(
                    key.clone(),
                    &format!("{name} · running"),
                    Role::Tool,
                    Piece::json(
                        Source {
                            seq,
                            field: FactField::ToolArguments,
                        },
                        arguments,
                        0,
                    ),
                );
                if let Some(block) = self.blocks.iter_mut().find(|block| block.key == key) {
                    let status = block
                        .title
                        .split_once(" · ")
                        .map_or("running", |(_, status)| status);
                    block.title = super::super::terminal_text(&format!("{name} · {status}"));
                }
            }
            SessionFactBody::ToolResult {
                effect_id, result, ..
            } => {
                let key = format!("tool:{effect_id}");
                let outcome = match ToolOutcome::from_result(result) {
                    ToolOutcome::Completed => "completed",
                    ToolOutcome::ToolFailed => "tool failed",
                    ToolOutcome::ProcessFailed => "command failed",
                };
                for (index, content) in result.content.iter().enumerate() {
                    if let ToolContent::Text { text } = content {
                        add(
                            key.clone(),
                            format!("Tool · {outcome}"),
                            Role::Tool,
                            FactField::ToolText {
                                index: u16::try_from(index).expect("validated Tool content index"),
                            },
                            text,
                        );
                    }
                }
                if result.content.is_empty() {
                    self.add(
                        key.clone(),
                        &format!("Tool · {outcome}"),
                        Role::Tool,
                        Piece::json(
                            Source {
                                seq,
                                field: FactField::ToolValue,
                            },
                            &result.value,
                            0,
                        ),
                    );
                }
                if let Some(block) = self.blocks.iter_mut().find(|block| block.key == key) {
                    if seq >= block.status_seq {
                        let name = block.title.split(" · ").next().unwrap_or("Tool");
                        block.title = format!("{name} · {outcome}");
                        block.status_seq = seq;
                    }
                    block.outputs = ["stdout", "stderr"].map(|stream| {
                        result
                            .value
                            .get(stream)?
                            .get("full_output")?
                            .as_str()
                            .map(str::to_owned)
                    });
                }
            }
            SessionFactBody::TurnTerminal { turn_id, outcome } => {
                self.add(
                    format!("outcome:{turn_id}"),
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

    fn add(&mut self, key: String, title: &str, role: Role, piece: Piece) {
        let position = self.blocks.iter().position(|block| block.key == key);
        let index = position.unwrap_or_else(|| {
            self.blocks.push(Block {
                key,
                title: super::super::terminal_text(title),
                role,
                pieces: VecDeque::new(),
                collapsed: matches!(role, Role::Tool | Role::Reasoning),
                outputs: [None, None],
                first: piece.source.seq,
                status_seq: piece.source.seq,
                discarded: false,
                text_bytes: 0,
                map_bytes: 0,
            });
            self.blocks.len() - 1
        });
        let block = &mut self.blocks[index];
        // Older pages arrive in ascending order too; a missing interior source
        // is distinct from replaying an already retained source.
        if block
            .pieces
            .binary_search_by_key(&piece.source, |piece| piece.source)
            .is_ok()
        {
            return;
        }
        let old = block
            .pieces
            .back()
            .is_some_and(|last| piece.source < last.source);
        while !block.pieces.is_empty()
            && (block.text_bytes + piece.text.capacity() > WINDOW || block.pieces.len() >= 4096)
        {
            let removed = if old {
                block.pieces.pop_back()
            } else {
                block.pieces.pop_front()
            }
            .expect("nonempty pieces");
            block.text_bytes -= removed.text.capacity();
            block.map_bytes -= removed.metadata();
            block.discarded = true;
            self.earlier = true;
        }
        block.text_bytes += piece.text.capacity();
        block.map_bytes += piece.metadata();
        block.first = block.first.min(piece.source.seq);
        let position = block
            .pieces
            .partition_point(|retained| retained.source < piece.source);
        block.pieces.insert(position, piece);
    }

    pub(super) fn budgets(&self) -> (usize, usize) {
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
                if let Some(piece) = if older {
                    block.pieces.pop_back()
                } else {
                    block.pieces.pop_front()
                } {
                    block.text_bytes -= piece.text.capacity();
                    block.map_bytes -= piece.metadata();
                    block.discarded = true;
                }
                block.pieces.shrink_to_fit();
            } else {
                self.blocks
                    .remove(if older { self.blocks.len() - 1 } else { 0 });
                self.blocks.shrink_to_fit();
            }
        }
    }

    pub(super) fn locate(&self, anchor: Anchor) -> Option<(usize, usize)> {
        self.blocks
            .iter()
            .enumerate()
            .find_map(|(index, block)| block.offset(anchor).map(|offset| (index, offset)))
    }

    pub(super) fn selected(&self, start: Anchor, end: Anchor) -> Result<String, &'static str> {
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

pub(super) fn json_window(value: &impl serde::Serialize) -> String {
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
