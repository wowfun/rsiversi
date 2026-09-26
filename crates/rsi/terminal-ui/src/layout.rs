//! Presentation-owned body layouts; no durable records or source leases.
use super::{MarkdownStyles, each_row};
use crate::transcript::{Block, Role, Transcript};
use std::{
    collections::{BTreeMap, VecDeque},
    sync::Arc,
};

const MAX_ENTRIES: usize = 512;
const MAX_BYTES: usize = 32 * 1024 * 1024;

pub(crate) const FOLD_HEAD_ROWS: usize = 2;
pub(crate) const FOLD_TAIL_ROWS: usize = 3;
pub(crate) fn fold_rows(total: usize) -> Option<std::ops::Range<usize>> {
    (total > FOLD_HEAD_ROWS + FOLD_TAIL_ROWS).then(|| FOLD_HEAD_ROWS..total - FOLD_TAIL_ROWS)
}

#[derive(Debug)]
pub struct Layout {
    pub text: String,
    pub styles: MarkdownStyles,
    ends: Vec<u32>,
    command_row: Option<usize>,
    markdown: Option<Arc<crate::markdown::Document>>,
}
impl Layout {
    fn new(block: &Block, width: u16, markdown: bool) -> Self {
        let width = width.saturating_sub(if block.role == Role::User { 2 } else { 0 });
        let original = block.text();
        let markdown = (markdown
            && matches!(block.role, Role::Assistant | Role::Reasoning)
            && !block.collapsed)
            .then(|| {
                block.markdown.clone().or_else(|| {
                    (!block.discarded && !block.pieces.front().is_some_and(|piece| piece.omitted))
                        .then(|| crate::markdown::parse(&original, width).map(Arc::new))
                        .flatten()
                })
            })
            .flatten();
        let text = markdown.as_ref().map_or(original, |doc| doc.text.clone());
        let command_end: usize = block
            .pieces
            .iter()
            .take_while(|piece| piece.source.field == rsi_conversation::FactField::ToolCommand)
            .map(|piece| piece.text.len())
            .sum();
        let command_end = (command_end > 0 && command_end < text.len()).then_some(command_end);
        let mut ends = Vec::new();
        let mut command_row = None;
        let split = command_end.unwrap_or(text.len());
        for (start, end) in [(0, split), (split, text.len())] {
            if start == end {
                continue;
            }
            each_row(&text[start..end], usize::from(width).max(1), |_, offset| {
                ends.push(u32::try_from(start + offset).expect("bounded projected block"));
                true
            });
            if Some(end) == command_end {
                if text[..end].ends_with('\n') {
                    ends.pop();
                } else {
                    command_row = Some(ends.len());
                }
            }
        }
        if ends.is_empty() {
            ends.push(0);
        }
        let styles = markdown.as_ref().map_or_else(Vec::new, |doc| {
            doc.runs
                .iter()
                .map(|run| (run.display.clone(), run.style))
                .collect()
        });
        Self {
            text,
            styles,
            ends,
            command_row,
            markdown,
        }
    }
    fn row_start(&self, index: usize) -> usize {
        index.checked_sub(1).map_or(0, |previous| {
            let end = self.ends[previous] as usize;
            end + usize::from(
                Some(index) != self.command_row && self.text.as_bytes().get(end) == Some(&b'\n'),
            )
        })
    }
    pub fn rows(&self) -> impl ExactSizeIterator<Item = (usize, usize)> + DoubleEndedIterator + '_ {
        self.ends
            .iter()
            .enumerate()
            .map(move |(index, end)| (self.row_start(index), *end as usize))
    }
    pub fn first_row(&self, offset: usize) -> usize {
        let offset = self
            .markdown
            .as_ref()
            .map_or(offset, |doc| doc.display_offset(offset));
        // Search starts so wrapping and the display-only command/result break
        // preserve their distinct rows, even beside empty source lines.
        let mut from = 0;
        let mut to = self.ends.len();
        while from < to {
            let middle = from + (to - from) / 2;
            if self.row_start(middle) <= offset {
                from = middle + 1;
            } else {
                to = middle;
            }
        }
        from.saturating_sub(1)
    }
    pub fn source_offset(&self, display: usize) -> Option<usize> {
        self.markdown
            .as_ref()
            .map_or(Some(display), |doc| doc.source_offset(display))
    }
    pub fn source_range(&self, display: std::ops::Range<usize>) -> Option<std::ops::Range<usize>> {
        self.markdown.as_ref().map_or_else(
            || Some(display.clone()),
            |doc| doc.source_range(display.clone()),
        )
    }
    pub(crate) fn hidden_range(&self, block: &Block) -> Option<(usize, usize)> {
        if !block.collapsed || matches!(block.role, Role::User | Role::Assistant | Role::Metadata) {
            return None;
        }
        if block.summary_only() {
            return Some((0, self.text.len()));
        }
        fold_rows(self.rows().len())
            .map(|rows| (self.row_start(rows.start), self.row_start(rows.end)))
    }
    fn bytes(&self) -> usize {
        self.text.capacity()
            + self.ends.capacity() * size_of::<u32>()
            + self.styles.capacity() * size_of::<(std::ops::Range<usize>, ratatui::style::Style)>()
            + self.markdown.as_ref().map_or(0, |doc| doc.bytes())
            + size_of::<Self>()
    }
}
#[derive(Debug)]
struct Entry {
    key: String,
    revision: Arc<()>,
    width: u16,
    collapsed: bool,
    layout: Arc<Layout>,
}
impl Entry {
    fn bytes(&self) -> usize {
        self.key.capacity() + self.layout.bytes() + size_of::<Self>()
    }
}
#[derive(Debug, Default)]
pub struct LayoutCache {
    markdown: bool,
    entries: VecDeque<Entry>,
    bytes: usize,
    #[cfg(test)]
    pub builds: usize,
    #[cfg(test)]
    pub enumerated_rows: usize,
}
impl LayoutCache {
    pub fn set_markdown(&mut self, enabled: bool) {
        if self.markdown != enabled {
            self.entries.clear();
            self.bytes = 0;
            self.markdown = enabled;
        }
    }

    pub fn retain(&mut self, transcript: &Transcript) {
        let current: BTreeMap<_, _> = transcript
            .blocks
            .iter()
            .map(|block| (block.key.as_str(), &block.layout_revision))
            .collect();
        self.entries.retain(|entry| {
            current
                .get(entry.key.as_str())
                .is_some_and(|revision| Arc::ptr_eq(revision, &entry.revision))
        });
        self.bytes = self.entries.iter().map(Entry::bytes).sum();
    }
    pub(crate) fn get(&mut self, block: &Block, width: u16) -> Arc<Layout> {
        if let Some(index) = self.entries.iter().position(|entry| entry.key == block.key) {
            let entry = self.entries.remove(index).expect("located cache entry");
            if Arc::ptr_eq(&entry.revision, &block.layout_revision)
                && entry.width == width
                && entry.collapsed == block.collapsed
            {
                let layout = entry.layout.clone();
                self.entries.push_back(entry);
                return layout;
            }
            self.bytes -= entry.bytes();
        }
        #[cfg(test)]
        {
            self.builds += 1;
        }
        let layout = Arc::new(Layout::new(block, width, self.markdown));
        let entry = Entry {
            key: block.key.clone(),
            revision: block.layout_revision.clone(),
            width,
            collapsed: block.collapsed,
            layout: layout.clone(),
        };
        let bytes = entry.bytes();
        if bytes <= MAX_BYTES {
            while self.entries.len() >= MAX_ENTRIES || self.bytes + bytes > MAX_BYTES {
                let removed = self.entries.pop_front().expect("cache retention exceeded");
                self.bytes -= removed.bytes();
            }
            self.bytes += bytes;
            self.entries.push_back(entry);
        }
        layout
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rsi_agent_session_protocol::{SessionFact, SessionFactBody, TurnId};
    fn transcript(text: &str) -> Transcript {
        let mut transcript = Transcript::default();
        transcript.apply(
            &SessionFact::new(
                1,
                1,
                SessionFactBody::TurnAccepted {
                    reasoning_effort: None,
                    turn_id: TurnId::new("turn").unwrap(),
                    text: text.into(),
                    model: None,
                    sandbox: rsi_sandbox::SandboxMode::WorkspaceWrite,
                    require_approval: false,
                },
            )
            .unwrap(),
        );
        transcript
    }
    #[test]
    fn command_result_break_preserves_source_newlines_and_row_anchors() {
        use rsi_agent_session_protocol::EffectId;
        use rsi_tools_protocol::{ToolContent, ToolResult, ToolResultIdentity};
        for (command, result, expected) in [
            ("cmd", "out", vec!["cmd", "out"]),
            ("cmd\n", "out", vec!["cmd", "out"]),
            ("cmd", "\nout", vec!["cmd", "", "out"]),
            ("cmd\n", "\nout", vec!["cmd", "", "out"]),
        ] {
            let mut transcript = Transcript::default();
            let turn_id = TurnId::new("turn").unwrap();
            let effect_id = EffectId::new("tool").unwrap();
            let identity =
                ToolResultIdentity::new("owner", "invoke", "call", "a".repeat(64)).unwrap();
            transcript.apply(
                &SessionFact::new(
                    1,
                    1,
                    SessionFactBody::ToolIntent {
                        turn_id: turn_id.clone(),
                        effect_id: effect_id.clone(),
                        identity: identity.clone(),
                        origin: rsi_agent_session_protocol::ToolOrigin::Model {
                            effect_id: EffectId::new("model").unwrap(),
                        },
                        program_role: rsi_tools_protocol::ToolProgramRole::Unavailable,
                        name: "bash".into(),
                        arguments: serde_json::json!({"command":command}),
                        approval: None,
                        parallel_safe: false,
                    },
                )
                .unwrap(),
            );
            transcript.apply(
                &SessionFact::new(
                    2,
                    2,
                    SessionFactBody::ToolResult {
                        turn_id,
                        effect_id,
                        identity,
                        result: ToolResult::new(
                            serde_json::json!({"exit_code":0}),
                            vec![ToolContent::Text {
                                text: result.into(),
                            }],
                            false,
                        )
                        .unwrap(),
                        conclusion: None,
                    },
                )
                .unwrap(),
            );
            let block = &transcript.blocks[0];
            let layout = Layout::new(block, 80, false);
            assert_eq!(
                layout.text,
                format!("{command}{result}"),
                "display breaks do not mutate source"
            );
            assert_eq!(
                layout
                    .rows()
                    .map(|(from, to)| &layout.text[from..to])
                    .collect::<Vec<_>>(),
                expected
            );
            for (row, (start, _)) in layout.rows().enumerate() {
                assert_eq!(layout.first_row(start), row);
                let anchor = block.anchor(start).unwrap();
                assert_eq!(block.offset(anchor), Some(start));
            }
        }
    }
    #[test]
    fn row_starts_round_trip_across_wraps_newlines_and_graphemes() {
        for text in [
            "abcde\nfghij",
            "abcdefghij",
            "界界界\nabc",
            "\n\n",
            "a\tbcdef",
        ] {
            let transcript = transcript(text);
            let layout = Layout::new(&transcript.blocks[0], 6, false);
            for (index, (start, _)) in layout.rows().enumerate() {
                assert_eq!(layout.first_row(start), index, "{text:?}: {start}");
            }
        }
    }
    #[test]
    fn compact_rows_preserve_newlines_tabs_and_grapheme_boundaries() {
        for (text, expected) in [
            ("ab\nc\t界", vec![(0, 2), (3, 5), (5, 8)]),
            ("\n\n", vec![(0, 0), (1, 1), (2, 2)]),
            ("e\u{301}界", vec![(0, 6)]),
        ] {
            let transcript = transcript(text);
            let layout = Layout::new(&transcript.blocks[0], 6, false);
            assert_eq!(layout.rows().collect::<Vec<_>>(), expected);
        }
    }
    #[test]
    fn layout_lru_bounds_retention_and_releases_removed_blocks() {
        let transcript = transcript(&"\n".repeat(256 * 1024));
        let mut block = transcript.blocks[0].clone();
        let mut cache = LayoutCache::default();
        let mut latest = None;
        for index in 0..40 {
            block.key = format!("bounded-{index}");
            latest = Some(cache.get(&block, 1));
            assert!(cache.bytes <= MAX_BYTES);
            assert!(cache.entries.len() <= MAX_ENTRIES);
        }
        assert!(cache.entries.len() < 40, "byte budget evicted old layouts");
        let latest = latest.unwrap();
        assert!(Arc::ptr_eq(&cache.get(&block, 1), &latest));
        assert_eq!(cache.builds, 40);
        block.key = "bounded-0".into();
        cache.get(&block, 1);
        assert_eq!(cache.builds, 41, "eviction recomputes exact content");
        cache.retain(&Transcript::default());
        assert!(cache.entries.is_empty());
        assert_eq!(cache.bytes, 0);
    }
}
