use super::{
    Anchor, Block, MAX_BLOCKS, MAX_METADATA, MAX_TEXT, MAXIMUM_BLOCK_SOURCES, Mapping, Piece,
    ProcessOutcome, Role, Source, SourceAdmission, Transcript,
};
use std::collections::VecDeque;

/// A portable viewport keeps source coordinates while discarding controller metadata.
#[derive(Debug, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Viewport {
    blocks: Vec<WindowBlock>,
    width: u16,
}
#[derive(Debug, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
#[allow(clippy::struct_excessive_bools)] // Serialized independent Block flags.
struct WindowBlock {
    elapsed_ms: Option<u64>,
    running: bool,
    outcome: Option<ProcessOutcome>,
    key: String,
    title: String,
    role: Role,
    collapsed: bool,
    completed: bool,
    concise: bool,
    fold: Option<FoldWindow>,
    discarded: bool,
    pieces: Vec<WindowPiece>,
}
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
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
    pub fn capture(
        transcript: &Transcript,
        top: Option<Anchor>,
        width: u16,
        height: u16,
    ) -> (Self, Option<Anchor>) {
        Self::capture_cached(
            transcript,
            top,
            width,
            height,
            None,
            None,
            &mut FoldCache::default(),
        )
    }
    pub(crate) fn capture_cached(
        transcript: &Transcript,
        top: Option<Anchor>,
        width: u16,
        height: u16,
        activity: Option<&crate::Activity>,
        focus: Option<Anchor>,
        cache: &mut FoldCache,
    ) -> (Self, Option<Anchor>) {
        let position = focus
            .and_then(|anchor| transcript.locate(anchor))
            .map(|(index, _)| (index.saturating_sub(usize::from(height)), 0))
            .or_else(|| top.map(|top| transcript.locate(top).unwrap_or((0, 0))));
        let mut projected_top = top;
        let mut remaining = Self::MAXIMUM_TEXT;
        let mut blocks = Vec::new();
        let count = (usize::from(height) * 2).clamp(1, MAX_BLOCKS);
        let range: Box<dyn Iterator<Item = usize>> = match position {
            Some((index, _)) => Box::new(index..transcript.blocks.len()),
            None => Box::new((0..transcript.blocks.len()).rev()),
        };
        for index in range.take(count) {
            let block = &transcript.blocks[index];
            let (mut pieces, fold) = if block.collapsed
                && !matches!(block.role, Role::User | Role::Assistant | Role::Metadata)
            {
                cache.window(block, width)
            } else {
                let mut pieces = Vec::new();
                let mut skipped = 0;
                let indexes: Box<dyn Iterator<Item = usize>> = if position.is_some() {
                    Box::new(0..block.pieces.len())
                } else {
                    Box::new((0..block.pieces.len()).rev())
                };
                let mut budget = remaining;
                for i in indexes {
                    let piece = &block.pieces[i];
                    let end = skipped + piece.text.len();
                    if position.is_some_and(|(at, offset)| at == index && end < offset) {
                        skipped = end;
                        continue;
                    }
                    if piece.text.len() > budget {
                        break;
                    }
                    budget -= piece.text.len();
                    pieces.push(window_piece(
                        piece,
                        std::slice::from_ref(&(0..piece.text.len())),
                    ));
                    skipped = end;
                }
                if position.is_none() {
                    pieces.reverse();
                }
                (pieces, None)
            };
            let bytes: usize = pieces.iter().map(|piece| piece.text.len()).sum();
            if bytes > remaining || (pieces.is_empty() && !block.pieces.is_empty()) {
                break;
            }
            remaining -= bytes;
            if let Some(fold) = &fold
                && position.is_some_and(|(at, offset)| {
                    at == index && (fold.from_offset..fold.to_offset).contains(&offset)
                })
            {
                projected_top = Some(fold.from);
            }
            let (elapsed_ms, running) = block.clock.display(activity);
            let title = match block.tool.as_ref().filter(|_| !block.completed && !running) {
                Some(tool) if tool.phase == rsi_conversation::ToolPhase::Running => {
                    crate::terminal_text(&super::tool_title(tool, true))
                }
                _ => block.title.clone(),
            };
            blocks.push(WindowBlock {
                elapsed_ms,
                running,
                outcome: matches!(block.role, Role::Reasoning | Role::Tool)
                    .then_some(block.outcome)
                    .flatten(),
                key: block.key.clone(),
                title,
                role: block.role,
                collapsed: block.collapsed,
                completed: block.completed,
                concise: block.concise,
                discarded: block.discarded,
                fold,
                pieces: std::mem::take(&mut pieces),
            });
            if remaining == 0 {
                break;
            }
        }
        if position.is_none() {
            blocks.reverse();
        }
        (Self { blocks, width }, projected_top)
    }
    pub(crate) fn validate_width(&self, width: u16) -> Result<(), &'static str> {
        if self.width != width {
            return Err("viewport capture width changed");
        }
        Ok(())
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
                || (block.outcome.is_some()
                    && (!matches!(block.role, Role::Reasoning | Role::Tool)
                        || !block.completed
                        || block.running))
            {
                return Err("invalid viewport block");
            }
            let index = result.block_index(block.key, &block.title, block.role, 1);
            let target = &mut result.blocks[index];
            target.clock.elapsed_ms = block.elapsed_ms;
            target.clock.running = block.running;
            target.outcome = block.outcome;
            target.collapsed = block.collapsed;
            target.completed = block.completed;
            target.concise = block.concise;
            if let Some(fold) = &block.fold
                && (!block.collapsed
                    || matches!(block.role, Role::User | Role::Assistant | Role::Metadata)
                    || fold.rows_hidden == 0
                    || fold.rows_hidden > MAX_TEXT + 1
                    || fold.from.source.seq == 0
                    || fold.to.source.seq == 0
                    || fold.from_offset > fold.to_offset
                    || fold.to_offset > MAX_TEXT)
            {
                return Err("invalid folded source window");
            }
            target.fold = block.fold;
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

/// Discardable compressed source windows, bounded separately from renderer layouts.
#[derive(Debug, Default)]
pub(crate) struct FoldCache {
    entries: VecDeque<FoldEntry>,
    bytes: usize,
    #[cfg(test)]
    pub builds: usize,
}
#[derive(Debug)]
struct FoldEntry {
    key: String,
    revision: std::sync::Arc<()>,
    width: u16,
    summary: bool,
    pieces: Vec<WindowPiece>,
    fold: Option<FoldWindow>,
}
impl FoldEntry {
    fn bytes(&self) -> usize {
        size_of::<Self>()
            + self.key.capacity()
            + self.pieces.capacity() * size_of::<WindowPiece>()
            + self
                .pieces
                .iter()
                .map(|piece| {
                    piece.text.capacity() + piece.mapping.capacity() * size_of::<(usize, usize)>()
                })
                .sum::<usize>()
    }
}
impl FoldCache {
    fn window(&mut self, block: &Block, width: u16) -> (Vec<WindowPiece>, Option<FoldWindow>) {
        const MAXIMUM: usize = 1024 * 1024;
        if let Some(index) = self.entries.iter().position(|entry| {
            entry.key == block.key
                && std::sync::Arc::ptr_eq(&entry.revision, &block.layout_revision)
                && entry.width == width
                && entry.summary == block.summary_only()
        }) {
            let entry = self.entries.remove(index).expect("located cache entry");
            let window = (entry.pieces.clone(), entry.fold.clone());
            self.entries.push_back(entry);
            return window;
        }
        #[cfg(test)]
        {
            self.builds += 1;
        }
        let (pieces, fold) = folded(block, width);
        let entry = FoldEntry {
            key: block.key.clone(),
            revision: block.layout_revision.clone(),
            width,
            summary: block.summary_only(),
            pieces: pieces.clone(),
            fold: fold.clone(),
        };
        let bytes = entry.bytes();
        if bytes <= MAXIMUM {
            // Retire obsolete versions of this block before admitting its new window.
            self.entries.retain(|entry| entry.key != block.key);
            self.bytes = self.entries.iter().map(FoldEntry::bytes).sum();
            while self.entries.len() >= 512 || self.bytes + bytes > MAXIMUM {
                self.bytes -= self
                    .entries
                    .pop_front()
                    .expect("nonempty cache under pressure")
                    .bytes();
            }
            self.entries.push_back(entry);
            self.bytes += bytes;
        }
        (pieces, fold)
    }
}

/// Exact original gap, independent of the bounded serialized head/tail text.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct FoldWindow {
    pub rows_hidden: usize,
    pub from: Anchor,
    pub to: Anchor,
    from_offset: usize,
    to_offset: usize,
}

fn folded(block: &Block, width: u16) -> (Vec<WindowPiece>, Option<FoldWindow>) {
    let text = block.text();
    let mut heads = Vec::new();
    let mut tails = VecDeque::new();
    let mut count = 0usize;
    crate::render::each_row(&text, usize::from(width).max(1), |start, end| {
        if heads.len() < crate::render::layout::FOLD_HEAD_ROWS + 2 {
            heads.push((start, end));
        }
        if tails.len() == crate::render::layout::FOLD_TAIL_ROWS {
            tails.pop_front();
        }
        tails.push_back((start, end));
        count += 1;
        true
    });
    let (ranges, fold) = if block.summary_only() && !text.is_empty() {
        (
            std::iter::once(0..heads[0].1).collect::<Vec<_>>(),
            Some(FoldWindow {
                rows_hidden: count,
                from: block.anchor(0).unwrap(),
                to: block.anchor(text.len()).unwrap(),
                from_offset: 0,
                to_offset: text.len(),
            }),
        )
    } else if let Some(gap) = crate::render::layout::fold_rows(count) {
        let from = heads[gap.start].0;
        let to = tails[0].0;
        (
            vec![0..heads[gap.start + 1].0, to..text.len()],
            Some(FoldWindow {
                rows_hidden: gap.len(),
                from: block.anchor(from).unwrap(),
                to: block.anchor(to).unwrap(),
                from_offset: from,
                to_offset: to,
            }),
        )
    } else {
        (std::iter::once(0..text.len()).collect(), None)
    };
    let mut pieces = Vec::new();
    let mut at = 0;
    for piece in &block.pieces {
        let local: Vec<_> = ranges
            .iter()
            .filter_map(|range| {
                let start = range.start.max(at);
                let end = range.end.min(at + piece.text.len());
                (start < end || (text.is_empty() && at == 0))
                    .then_some(start.saturating_sub(at)..end.saturating_sub(at))
            })
            .collect();
        if !local.is_empty() {
            pieces.push(window_piece(piece, &local));
        }
        at += piece.text.len();
    }
    // A summary over an initial empty row still needs its source anchor.
    if pieces.is_empty()
        && let Some(piece) = block.pieces.front()
    {
        pieces.push(window_piece(piece, std::slice::from_ref(&(0..0))));
    }
    (pieces, fold)
}
fn window_piece(piece: &Piece, ranges: &[std::ops::Range<usize>]) -> WindowPiece {
    let mut text = String::new();
    let mut mapping: Vec<(usize, usize)> = Vec::new();
    for range in ranges {
        let base = text.len();
        let source = piece.anchor(range.start).offset;
        if mapping.last().is_some_and(|(display, _)| *display == base) {
            mapping.pop();
        }
        mapping.push((base, source));
        mapping.extend(
            piece
                .mapping
                .iter()
                .filter(|run| run.display > range.start && run.display <= range.end)
                .map(|run| (base + run.display - range.start, run.source)),
        );
        text.push_str(&piece.text[range.clone()]);
    }
    WindowPiece {
        source: piece.source,
        start: piece.anchor(ranges[0].start).offset,
        omitted: piece.omitted
            || ranges.len() > 1
            || ranges[0].start > 0
            || ranges.last().unwrap().end < piece.text.len(),
        truncated_after: piece.truncated_after || ranges.last().unwrap().end < piece.text.len(),
        text,
        mapping,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn process_outcomes_require_settled_process_blocks_on_the_wire() {
        let mut transcript = Transcript::default();
        transcript.add(
            "reasoning".into(),
            "Thinking",
            Role::Reasoning,
            Piece::new(
                Source {
                    seq: 1,
                    field: rsi_conversation::FactField::ModelReasoning,
                },
                "source",
                0,
                super::super::WINDOW,
            ),
        );
        let mut wire =
            serde_json::to_value(Viewport::capture(&transcript, None, 80, 24).0).unwrap();
        wire["blocks"][0]["outcome"] = serde_json::json!("success");
        for completed in [false, true] {
            for running in [false, true] {
                wire["blocks"][0]["completed"] = serde_json::json!(completed);
                wire["blocks"][0]["running"] = serde_json::json!(running);
                let viewport: Viewport = serde_json::from_value(wire.clone()).unwrap();
                assert_eq!(viewport.restore().is_ok(), completed && !running);
            }
        }
        wire["blocks"][0]["outcome"] = serde_json::json!("unrecognized");
        assert!(serde_json::from_value::<Viewport>(wire).is_err());
    }

    #[test]
    fn compressed_cache_reuses_only_exact_revisions_and_evicts_under_both_bounds() {
        let mut transcript = Transcript::default();
        transcript.add(
            "source".into(),
            "Thinking",
            Role::Reasoning,
            Piece::new(
                Source {
                    seq: 1,
                    field: rsi_conversation::FactField::ModelReasoning,
                },
                &"original text\n".repeat(1000),
                0,
                super::super::WINDOW,
            ),
        );
        let mut block = transcript.blocks[0].clone();
        let mut cache = FoldCache::default();
        let expected = serde_json::to_value(cache.window(&block, 80)).unwrap();
        assert_eq!(
            serde_json::to_value(cache.window(&block, 80)).unwrap(),
            expected
        );
        assert_eq!(cache.builds, 1);
        let _ = cache.window(&block, 28);
        assert_eq!(cache.builds, 2);
        block.layout_revision = std::sync::Arc::new(());
        let _ = cache.window(&block, 28);
        assert_eq!(cache.builds, 3);
        block.completed = true;
        let (pieces, fold) = cache.window(&block, 28);
        assert_eq!(cache.builds, 4);
        assert_eq!(pieces.len(), 1);
        assert_eq!(fold.unwrap().from.offset, 0);
        for index in 0..600 {
            block.key = format!("cached-{index}");
            let _ = cache.window(&block, 80);
            assert!(cache.entries.len() <= 512 && cache.bytes <= 1024 * 1024);
        }
        assert!(!cache.entries.iter().any(|entry| entry.key == "source"));
        let before = cache.builds;
        block.key = "source".into();
        let _ = cache.window(&block, 80);
        assert_eq!(cache.builds, before + 1);
    }
}
