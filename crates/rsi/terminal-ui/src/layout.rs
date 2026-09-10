//! Presentation-owned body layouts; no durable records or source leases.
use super::{MarkdownStyles, each_row, markdown_styles};
use crate::transcript::{Block, Role, Transcript};
use std::{
    collections::{BTreeMap, VecDeque},
    sync::Arc,
};

const MAX_ENTRIES: usize = 512;
const MAX_BYTES: usize = 32 * 1024 * 1024;

#[derive(Debug)]
pub struct Layout {
    pub text: String,
    pub styles: MarkdownStyles,
    ends: Vec<u32>,
}
impl Layout {
    fn new(block: &Block, width: u16) -> Self {
        let text = block.text();
        let mut ends = Vec::new();
        each_row(&text, usize::from(width).max(1), |_, end| {
            ends.push(u32::try_from(end).expect("bounded projected block"));
            true
        });
        let styles = if block.role == Role::Assistant {
            markdown_styles(&text)
        } else {
            Vec::new()
        };
        Self { text, styles, ends }
    }
    pub fn rows(&self) -> impl ExactSizeIterator<Item = (usize, usize)> + '_ {
        self.ends.iter().enumerate().map(move |(index, end)| {
            let previous = index.checked_sub(1).map(|index| self.ends[index] as usize);
            let start = previous.map_or(0, |end| {
                end + usize::from(self.text.as_bytes().get(end) == Some(&b'\n'))
            });
            let end = *end as usize;
            (start, end)
        })
    }
    pub fn first_row(&self, offset: usize) -> usize {
        self.ends.partition_point(|end| (*end as usize) < offset)
    }
    fn bytes(&self) -> usize {
        self.text.capacity()
            + self.ends.capacity() * size_of::<u32>()
            + self.styles.capacity() * size_of::<(std::ops::Range<usize>, ratatui::style::Style)>()
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
    entries: VecDeque<Entry>,
    bytes: usize,
    #[cfg(test)]
    pub builds: usize,
}
impl LayoutCache {
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
        let layout = Arc::new(Layout::new(block, width));
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
    fn compact_rows_preserve_newlines_tabs_and_grapheme_boundaries() {
        for (text, expected) in [
            ("ab\nc\t界", vec![(0, 2), (3, 5), (5, 8)]),
            ("\n\n", vec![(0, 0), (1, 1), (2, 2)]),
            ("e\u{301}界", vec![(0, 6)]),
        ] {
            let transcript = transcript(text);
            let layout = Layout::new(&transcript.blocks[0], 4);
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
