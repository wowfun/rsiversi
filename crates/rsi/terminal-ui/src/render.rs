//! Bounded visible rows with source positions; wrapping and editing share graphemes.
#[path = "footer.rs"]
mod footer;
#[path = "layout.rs"]
pub(crate) mod layout;
use super::{
    Input as State,
    transcript::{Anchor, Role, Transcript},
};
pub use layout::LayoutCache;
use ratatui::{
    Frame,
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Clear, Paragraph},
};
use std::collections::VecDeque;
use unicode_segmentation::UnicodeSegmentation as _;
use unicode_width::UnicodeWidthStr as _;

pub fn resize(
    screen: &mut ratatui::Terminal<ratatui::backend::TestBackend>,
    width: u16,
    height: u16,
) -> Result<(), std::convert::Infallible> {
    // draw() queries the backend size again; resizing only Terminal's buffers
    // would let its automatic resize restore the previous dimensions.
    screen.backend_mut().resize(width, height);
    screen.resize(Rect::new(0, 0, width, height))
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Row {
    block: usize,
    offset: usize,
    end: usize,
    pub title: bool,
    metadata: bool,
    spacer: Option<Spacer>,
    hidden: usize,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
enum Spacer {
    Blank,
    UserTop,
}

/// A fixed resident action; Session identity remains controller-owned.
#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FooterAction {
    Parent,
    Model,
}

#[derive(Clone, Debug, Default, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct View {
    markdown: bool,
    rows: Vec<Row>,
    session: Option<rsi_agent_session_protocol::SessionId>,
    anchors: Vec<Option<Anchor>>,
    pub area: Rect,
    pub hits: Vec<(u16, u16, u16, Anchor, Anchor)>,
    hidden_sources: Vec<(Anchor, Anchor)>,
    pub choices: Vec<(u16, usize)>,
    pub choice_revision: u64,
    pub choices_area: Rect,
    pub editor: Option<Rect>,
    pub dialog: Option<Rect>,
    pub scroll_max: usize,
    pub completion: Option<(u64, Rect, usize)>,
    pub footer: Vec<(Rect, FooterAction)>,
}

impl View {
    pub fn choice_at(&self, x: u16, y: u16) -> Option<usize> {
        let position = ratatui::layout::Position::new(x, y);
        if !self.choices_area.contains(position)
            || self
                .completion
                .is_some_and(|(_, area, _)| area.contains(position))
        {
            return None;
        }
        self.choices
            .iter()
            .find(|(row, _)| *row == y)
            .map(|(_, index)| *index)
    }
    pub fn validate(&self, width: u16, height: u16) -> Result<(), &'static str> {
        if [Some(self.choices_area), self.editor, self.dialog]
            .into_iter()
            .flatten()
            .any(|area| area.right() > width || area.bottom() > height)
            || self.scroll_max > crate::MAX_TEXT
            || self.dialog.is_some()
                && (!self.hits.is_empty() || !self.rows.is_empty() || !self.footer.is_empty())
            || self.hidden_sources.len() > usize::from(height) * 2
            || self
                .hidden_sources
                .iter()
                .any(|(a, b)| a.source.seq == 0 || b.source.seq == 0)
            || self.footer.len() > 2
            || self.footer.iter().any(|(area, _)| {
                area.height != 1
                    || area.width == 0
                    || area.right() > width
                    || area.bottom() > height
                    || area.bottom() < height.saturating_sub(1)
            })
            || self
                .footer
                .windows(2)
                .any(|pair| pair[0].0.right() > pair[1].0.x)
            || self.choices.len() > usize::from(height)
            || self.choices.iter().any(|(y, index)| {
                *y >= height
                    || *index >= 8192
                    || self.choices_area.width == 0
                    || *y < self.choices_area.y
                    || *y >= self.choices_area.bottom()
            })
            || self.completion.is_some_and(|(_, area, from)| {
                area.right() > width || area.bottom() > height || area.height > 8 || from >= 1024
            })
            || self.rows.len() > usize::from(height) * 2
            || self.anchors.len() != self.rows.len()
            || self.hits.len() > usize::from(width) * usize::from(height)
            || self.area.right() > width
            || self.area.bottom() > height
            || self.rows.iter().any(|r| {
                r.block >= crate::transcript::MAX_BLOCKS
                    || r.offset > r.end
                    || r.end > crate::transcript::MAX_TEXT
            })
            || self.hits.iter().any(|(y, x, w, a, b)| {
                *y >= height
                    || *w == 0
                    || u32::from(*x) + u32::from(*w) > u32::from(width)
                    || a.source.seq == 0
                    || b.source.seq == 0
            })
        {
            return Err("invalid source map bounds");
        }
        Ok(())
    }
    pub fn sources_belong_to(
        &self,
        session: &rsi_agent_session_protocol::SessionId,
        transcript: &Transcript,
    ) -> bool {
        (self.session.is_none() && self.rows.is_empty() && self.hits.is_empty())
            || (self.belongs_to(session)
                && transcript.contains_anchors(
                    self.anchors
                        .iter()
                        .flatten()
                        .copied()
                        .chain(self.hits.iter().flat_map(|(_, _, _, a, b)| [*a, *b])),
                ))
    }

    pub fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }
    pub fn at_bottom(&self) -> bool {
        self.rows.len() <= usize::from(self.area.height)
    }
    /// Copies exact retained source through the acknowledged frame's own fold map.
    pub fn selected_source(
        &self,
        transcript: &Transcript,
        start: Anchor,
        end: Anchor,
    ) -> Result<String, &'static str> {
        if ![start, end].into_iter().all(|anchor| {
            self.hits
                .iter()
                .any(|(_, _, _, a, b)| anchor == *a || anchor == *b)
        }) {
            return Err(
                "Selection is outside the displayed frame; select visible text or use full-source details",
            );
        }
        let a = transcript
            .locate(start)
            .ok_or("Reload selected source before copying")?;
        let b = transcript
            .locate(end)
            .ok_or("Reload selected source before copying")?;
        let (a, b) = if a <= b { (a, b) } else { (b, a) };
        for (from, to) in &self.hidden_sources {
            let from = transcript
                .locate(*from)
                .ok_or("Refresh the displayed source map before copying")?;
            let to = transcript
                .locate(*to)
                .ok_or("Refresh the displayed source map before copying")?;
            if a < to && b > from {
                return Err(
                    "Selection crosses folded text; expand the block or use full-source details",
                );
            }
        }
        transcript.selected(start, end)
    }
    pub fn location(
        &self,
        session: &rsi_agent_session_protocol::SessionId,
        transcript: &Transcript,
        row: usize,
    ) -> Option<(usize, usize)> {
        if self.session.as_ref() != Some(session) {
            return None;
        }
        transcript.locate(self.anchors.get(row).copied().flatten()?)
    }
    pub fn belongs_to(&self, session: &rsi_agent_session_protocol::SessionId) -> bool {
        self.session.as_ref() == Some(session)
    }
    /// Scroll through displayed source rows; decoration has no source position of its own.
    pub fn scroll_anchor(
        &self,
        session: &rsi_agent_session_protocol::SessionId,
        transcript: &Transcript,
        up: bool,
    ) -> Option<Anchor> {
        if !self.belongs_to(session) || !self.sources_belong_to(session, transcript) {
            return None;
        }
        let content = |row: &Row| row.spacer.is_none() && !row.metadata && row.hidden == 0;
        let first = self.rows.iter().enumerate().find_map(|(i, row)| {
            content(row)
                .then(|| self.location(session, transcript, i))
                .flatten()
        })?;
        if !up {
            // The last step uses the canonical tail layout, including metadata
            // and decoration that cannot be represented by a source anchor.
            let overflow = self
                .rows
                .len()
                .saturating_sub(usize::from(self.area.height));
            if overflow <= 3 {
                return None;
            }
            return self
                .rows
                .iter()
                .enumerate()
                .take(overflow + 1)
                .skip(3)
                .find_map(|(i, row)| {
                    let location = content(row)
                        .then(|| self.location(session, transcript, i))
                        .flatten()?;
                    (location > first).then(|| self.anchors[i]).flatten()
                });
        }
        let mut remaining = 3;
        let mut target = transcript.blocks[first.0].anchor(0);
        let mut cache = LayoutCache::default();
        cache.set_markdown(self.markdown);
        for index in (0..=first.0).rev() {
            let block = &transcript.blocks[index];
            if block.anchor(0).is_none() {
                continue;
            }
            let layout = cache.get(block, self.area.width);
            let hidden = layout::fold_rows(layout.rows().len()).filter(|_| block.collapsed);
            let offsets = std::iter::once(0)
                .chain(
                    layout
                        .rows()
                        .skip(1)
                        .enumerate()
                        .filter_map(|(i, (offset, _))| {
                            (!(block.summary_only()
                                || hidden.as_ref().is_some_and(|rows| rows.contains(&(i + 1)))))
                            .then(|| layout.source_offset(offset))
                            .flatten()
                        }),
                )
                .filter(|offset| index != first.0 || *offset < first.1);
            for offset in offsets.rev().take(remaining) {
                target = block.anchor(offset);
                remaining -= 1;
            }
            // Stop at this block's beginning before crossing its summary. In a
            // four-row viewport the previous user's padding can hide it entirely.
            if remaining == 0 || (index == first.0 && first.1 > 0) {
                break;
            }
        }
        target
    }
    pub fn hit(&self, x: u16, y: u16, end: bool) -> Option<Anchor> {
        self.hits
            .iter()
            .find(|(row, start, width, ..)| {
                *row == y && x >= *start && x < start.saturating_add(*width)
            })
            .map(|(_, _, _, a, b)| if end { *b } else { *a })
    }
    /// Resolves a displayed summary through its retained source, never its local block index.
    pub fn fold_at(
        &self,
        session: &rsi_agent_session_protocol::SessionId,
        transcript: &Transcript,
        x: u16,
        y: u16,
    ) -> Option<usize> {
        let position = ratatui::layout::Position::new(x, y);
        if !self.area.contains(position)
            || self
                .completion
                .is_some_and(|(_, area, _)| area.contains(position))
        {
            return None;
        }
        let row = usize::from(y - self.area.y);
        let displayed = self.rows.get(row)?;
        if !displayed.title || displayed.hidden > 0 {
            return None;
        }
        let (index, _) = self.location(session, transcript, row)?;
        (!matches!(
            transcript.blocks[index].role,
            Role::User | Role::Assistant | Role::Metadata
        ))
        .then_some(index)
    }
}

pub(crate) fn each_row(text: &str, width: usize, mut emit: impl FnMut(usize, usize) -> bool) {
    let mut start = 0;
    let mut cells = 0;
    for (offset, grapheme) in text.grapheme_indices(true) {
        let size = if grapheme == "\t" {
            4 - cells % 4
        } else {
            grapheme.width().max(1)
        };
        if grapheme == "\n" || (cells > 0 && cells + size > width) {
            if !emit(start, offset) {
                return;
            }
            start = if grapheme == "\n" { offset + 1 } else { offset };
            cells = 0;
        }
        if grapheme != "\n" {
            cells += size;
        }
    }
    let _ = emit(start, text.len());
}

// Only two screens of row metadata are retained, even for a large source window.
#[allow(clippy::too_many_lines)] // One bounded traversal owns folding, metadata gaps and pinned row geometry.
fn rows(
    transcript: &Transcript,
    cache: &mut LayoutCache,
    top: Option<Anchor>,
    focus: Option<(Anchor, u16)>,
    width: u16,
    height: u16,
) -> Vec<Row> {
    let limit = usize::from(height).max(1) * 2;
    let mut rows = VecDeque::with_capacity(limit);
    let focus = focus.and_then(|(anchor, row)| {
        transcript
            .locate(anchor)
            .map(|(index, _)| (index, usize::from(row.min(height.saturating_sub(1)))))
    });
    let top = if focus.is_some() { None } else { top };
    let location = top.and_then(|anchor| transcript.locate(anchor));
    let start_block = focus.map_or_else(
        || location.map_or(0, |(block, _)| block),
        |(index, _)| index.saturating_sub(usize::from(height)),
    );
    let fixed = std::cell::Cell::new(false);
    for (index, block) in transcript.blocks.iter().enumerate().skip(start_block) {
        if block.role == Role::Metadata && !block.completed {
            continue;
        }
        let prose = matches!(block.role, Role::User | Role::Assistant);
        let separated = rows.back().is_some_and(|row: &Row| row.spacer.is_some());
        let mut emit = |offset, end, title, hidden, metadata, spacer| {
            if let Some((target, row)) = focus
                && index == target
                && title
                && !fixed.get()
            {
                while rows.len() > row {
                    rows.pop_front();
                }
                fixed.set(true);
            }
            if rows.len() == limit {
                if top.is_some() || fixed.get() {
                    return false;
                }
                rows.pop_front();
            }
            rows.push_back(Row {
                block: index,
                offset,
                end,
                title,
                metadata,
                spacer,
                hidden,
            });
            true
        };
        if matches!(block.role, Role::Notice | Role::Error) {
            each_row(&block.title, usize::from(width).max(1), |start, end| {
                emit(start, end, true, 0, false, None)
            });
            continue;
        }
        let summary_only = block.summary_only();
        if (!prose || (index > 0 && !separated))
            && (summary_only || location.is_none_or(|(i, offset)| i != index || offset == 0))
            && !emit(0, 0, true, 0, false, None)
        {
            break;
        }
        if block.role == Role::Metadata {
            if !emit(0, 0, true, 0, false, Some(Spacer::Blank)) {
                break;
            }
            continue;
        }
        if block.summary_only() {
            continue;
        }
        let layout = cache.get(block, width);
        let total = layout.rows().len();
        let mut from = location
            .filter(|(i, _)| *i == index)
            .map_or(0, |(_, from)| layout.first_row(from));
        let gap = layout::fold_rows(total).filter(|_| block.collapsed && !prose);
        if let Some(gap) = &gap
            && gap.contains(&from)
        {
            from = gap.start;
        }
        let folded = gap.is_some();
        if top.is_none() && !fixed.get() && !folded {
            from = from.max(total.saturating_sub(limit));
        }
        if block.role == Role::User
            && from == 0
            && !emit(0, 0, false, 0, false, Some(Spacer::UserTop))
        {
            break;
        }
        // A fold presents its first two rows, one gap, and its last three rows.
        let head_end = gap.as_ref().map_or(total, |gap| gap.start + 1);
        let tail_start = gap.as_ref().map_or(total, |gap| gap.end.max(from));
        let visible = layout
            .rows()
            .enumerate()
            .skip(from)
            .take(head_end.saturating_sub(from))
            .chain(layout.rows().enumerate().skip(tail_start));
        for (row, (offset, end)) in visible {
            #[cfg(test)]
            {
                cache.enumerated_rows += 1;
            }
            let hidden = if let Some(gap) = &gap
                && row == gap.start
            {
                block
                    .fold
                    .as_ref()
                    .map_or(gap.len(), |fold| fold.rows_hidden)
            } else {
                0
            };
            if !emit(offset, end, false, hidden, false, None) {
                break;
            }
        }
        if block.role == Role::User
            && (!emit(layout.text.len(), layout.text.len(), true, 0, true, None)
                || !emit(
                    layout.text.len(),
                    layout.text.len(),
                    true,
                    0,
                    false,
                    Some(Spacer::Blank),
                ))
        {
            break;
        }
        if (top.is_some() || fixed.get()) && rows.len() >= limit {
            break;
        }
    }
    if top.is_none() && !fixed.get() {
        while rows.len() > usize::from(height) {
            rows.pop_front();
        }
    }
    rows.into_iter().collect()
}

/// Rejects source copying through text hidden by the current fold layout.
/// Exact source retention and the total copy bound are checked by Transcript.
#[allow(clippy::missing_panics_doc)] // Row count proves both indexed rows exist.
pub fn selected_visible(
    transcript: &Transcript,
    start: Anchor,
    end: Anchor,
    width: u16,
) -> Result<String, &'static str> {
    let a = transcript
        .locate(start)
        .ok_or("Reload selected source before copying")?;
    let b = transcript
        .locate(end)
        .ok_or("Reload selected source before copying")?;
    let (a, b) = if a <= b { (a, b) } else { (b, a) };
    let mut cache = LayoutCache::default();
    for index in a.0..=b.0 {
        let block = &transcript.blocks[index];
        if !block.collapsed || matches!(block.role, Role::User | Role::Assistant) {
            continue;
        }
        let layout = cache.get(block, width);
        let from = if index == a.0 { a.1 } else { 0 };
        let to = if index == b.0 { b.1 } else { layout.text.len() };
        let hidden = layout.hidden_range(block);
        if hidden.is_some_and(|(start, end)| from < end && to > start) {
            return Err(
                "Selection crosses folded text; expand the block or use full-source details",
            );
        }
    }
    transcript.selected(start, end)
}

#[allow(clippy::too_many_lines)] // Layout and source hit regions are computed from the same complete frame.
/// # Panics
/// Panics if borrowed editor or transcript values violate their in-process invariants.
pub fn draw(frame: &mut Frame<'_>, state: &State<'_>, cache: &mut LayoutCache) -> View {
    cache.set_markdown(state.markdown);
    let area = frame.area();
    frame.render_widget(Block::new().style(Style::default().bg(Color::Reset)), area);
    if area.width < 28 || area.height < 9 {
        frame.render_widget(Paragraph::new("Resize terminal: at least 28 × 9"), area);
        return View::default();
    }
    let draft = state.editor;
    let has_dialog = state.menu.is_some()
        || state.detail.is_some()
        || state.answer.is_some()
        || state.ui_edit.is_some();
    let mut dock = Vec::new();
    if state.questions > 0 || state.approvals > 0 {
        let mut waiting = Vec::new();
        if state.questions > 0 {
            waiting.push(format!("{} questions", state.questions));
        }
        if state.approvals > 0 {
            waiting.push(format!("{} approvals", state.approvals));
        }
        dock.push(waiting.join(" · "));
    }
    let tasks = state.todos.filter(|list| {
        list.items()
            .iter()
            .any(|item| item.status() != rsi_agent_todo::TodoStatus::Completed)
    });
    if let Some(list) = tasks {
        let completed = list
            .items()
            .iter()
            .filter(|item| item.status() == rsi_agent_todo::TodoStatus::Completed)
            .count();
        let active = list
            .items()
            .iter()
            .filter(|item| item.status() == rsi_agent_todo::TodoStatus::InProgress)
            .count();
        let current = list
            .items()
            .iter()
            .find(|item| item.status() == rsi_agent_todo::TodoStatus::InProgress)
            .or_else(|| {
                list.items()
                    .iter()
                    .find(|item| item.status() == rsi_agent_todo::TodoStatus::Pending)
            });
        let mut text = format!("Tasks {completed}/{}", list.items().len());
        if let Some(item) = current {
            text.push_str(" · ");
            text.push_str(item.content());
        }
        if active > 1 {
            let _ =
                std::fmt::Write::write_fmt(&mut text, format_args!(" (+{} active)", active - 1));
        }
        dock.push(text);
    }
    if state.busy || (state.active && state.activity.is_none() && tasks.is_none()) {
        dock.push(if state.busy {
            "Request pending".into()
        } else {
            state
                .actual_model
                .map_or_else(|| "Working…".into(), |model| format!("Working · {model}"))
        });
    }
    if !has_dialog && !state.status.is_empty() {
        dock.insert(0, crate::terminal_text(state.status));
    }
    let footer_height = 2;
    let flexible_height = area.height - 6 - footer_height;
    let dock_height = u16::try_from(dock.len())
        .unwrap_or(3)
        .min(3)
        .min(flexible_height);
    let editor_max = (flexible_height - dock_height + 1).min(6);
    let mut draft_rows = 0usize;
    let (display, caret) = composer_display(&draft.text, draft.cursor);
    each_row(
        &display,
        usize::from(area.width.saturating_sub(2)),
        |_, _| {
            draft_rows += 1;
            draft_rows < usize::from(editor_max)
        },
    );
    let editor_height = u16::try_from(draft_rows).unwrap_or(1).max(1) + 2;
    let body = Rect::new(
        0,
        0,
        area.width,
        area.height - footer_height - editor_height - dock_height,
    );
    let footer_y = area.height - footer_height;
    let footer = footer::build(state, area.width, footer_y);
    frame.render_widget(
        Paragraph::new(footer.text).style(muted()),
        Rect::new(0, footer_y, area.width, 1),
    );
    {
        let keys = if state.top.is_some() {
            "End follows output · /help · Ctrl+P actions"
        } else {
            "Ctrl+J adds a line · /help · Ctrl+P actions"
        };
        let keys = if keys.width() <= usize::from(area.width) {
            keys
        } else if state.top.is_some() {
            "End follows · /help"
        } else {
            "/help"
        };
        frame.render_widget(
            Paragraph::new(keys).style(muted()),
            Rect::new(0, area.height - 1, area.width, 1),
        );
    }
    for (row, text) in dock.iter().take(usize::from(dock_height)).enumerate() {
        frame.render_widget(
            Paragraph::new(ellipsis(text, usize::from(area.width))).style(muted()),
            Rect::new(
                0,
                body.bottom() + u16::try_from(row).unwrap_or(0),
                area.width,
                1,
            ),
        );
    }
    cache.retain(state.transcript);
    let rows = rows(
        state.transcript,
        cache,
        state.top,
        state.fold_focus,
        body.width,
        body.height,
    );
    let anchors = rows
        .iter()
        .map(|row| {
            let block = &state.transcript.blocks[row.block];
            let offset = if row.title || row.spacer.is_some() {
                Some(row.offset)
            } else {
                cache.get(block, body.width).source_offset(row.offset)
            };
            offset.and_then(|offset| block.anchor(offset))
        })
        .collect();
    let mut view = View {
        markdown: state.markdown,
        session: Some(state.header.session_id().clone()),
        anchors,
        rows,
        area: body,
        hits: Vec::new(),
        hidden_sources: Vec::new(),
        choices: Vec::new(),
        choice_revision: state.menu_revision,
        completion: None,
        footer: footer.actions,
        ..View::default()
    };
    if let (Some(first), Some(last)) = (view.rows.first(), view.rows.last()) {
        for block in state
            .transcript
            .blocks
            .iter()
            .skip(first.block)
            .take(last.block - first.block + 1)
        {
            if !block.collapsed
                || matches!(block.role, Role::User | Role::Assistant | Role::Metadata)
            {
                continue;
            }
            if let Some(fold) = &block.fold {
                view.hidden_sources.push((fold.from, fold.to));
                continue;
            }
            let layout = cache.get(block, body.width);
            let range = layout.hidden_range(block);
            if let Some((from, to)) = range
                && let (Some(a), Some(b)) = (block.anchor(from), block.anchor(to))
            {
                view.hidden_sources.push((a, b));
            }
        }
    }
    let selected = state
        .selection
        .and_then(|(a, b)| Some((state.transcript.locate(a)?, state.transcript.locate(b)?)))
        .map(|(a, b)| if a <= b { (a, b) } else { (b, a) });
    let mut text_cache: Option<(
        usize,
        std::sync::Arc<layout::Layout>,
        super::transcript::AnchorIndex<'_>,
    )> = None;
    for (row_index, row) in view.rows.iter().take(usize::from(body.height)).enumerate() {
        let block = &state.transcript.blocks[row.block];
        let y = body.y + u16::try_from(row_index).unwrap_or(0);
        if let Some(spacer) = row.spacer {
            if block.role == Role::User && spacer == Spacer::UserTop {
                frame.render_widget(
                    Paragraph::new("▄".repeat(usize::from(body.width)))
                        .style(Style::default().fg(Color::Indexed(235))),
                    Rect::new(body.x, y, body.width, 1),
                );
            }
            continue;
        }
        if row.metadata {
            let label = format!("• {}", block.title);
            frame.render_widget(
                Paragraph::new(label.clone()).style(Style::default().fg(Color::Indexed(247))),
                Rect::new(
                    body.x,
                    y,
                    u16::try_from(label.width())
                        .unwrap_or(body.width)
                        .min(body.width),
                    1,
                ),
            );
            continue;
        }
        let color = match block.role {
            Role::User | Role::Tool => Color::Gray,
            Role::Assistant => Color::Reset,
            Role::Reasoning | Role::Status | Role::Metadata | Role::Notice => Color::DarkGray,
            Role::Error => Color::LightRed,
        };
        if row.hidden > 0 {
            frame.render_widget(
                Paragraph::new(format!("… {} lines hidden", row.hidden)).style(muted()),
                Rect::new(body.x, y, body.width, 1),
            );
            continue;
        }
        if block.role == Role::User && !row.title {
            frame.render_widget(
                Block::new().style(user_style()),
                Rect::new(body.x, y, body.width, 1),
            );
        }
        if row.title && matches!(block.role, Role::User | Role::Assistant) {
            continue;
        }
        if row.title {
            if matches!(block.role, Role::Notice | Role::Error) {
                frame.render_widget(
                    Paragraph::new(&block.title[row.offset..row.end])
                        .style(Style::default().fg(color)),
                    Rect::new(body.x, y, body.width, 1),
                );
                continue;
            }
            if block.role == Role::Metadata {
                let label = format!("• {}", block.title);
                let width =
                    u16::try_from(label.width().min(usize::from(body.width))).unwrap_or(body.width);
                frame.render_widget(
                    Paragraph::new(label).style(Style::default().fg(Color::Indexed(247))),
                    Rect::new(body.x, y, width, 1),
                );
                continue;
            }
            let (elapsed_ms, running) = block.clock.display(state.activity.as_ref());
            let marker = if running {
                crate::activity_spinner_frame(elapsed_ms.unwrap_or(0))
            } else if block.collapsed {
                "▸"
            } else {
                "▾"
            };
            let marker_color = match block.outcome {
                Some(super::transcript::ProcessOutcome::Success) => Color::LightGreen,
                Some(super::transcript::ProcessOutcome::Failed) => Color::LightRed,
                Some(super::transcript::ProcessOutcome::Interrupted) | None => color,
            };
            let elapsed = elapsed_ms
                .filter(|ms| *ms >= 1000)
                .map(crate::elapsed_label);
            let label_width = body.width.saturating_sub(
                elapsed
                    .as_ref()
                    .map_or(0, |text| u16::try_from(text.width()).unwrap_or(0) + 2),
            );
            let label = format!(
                "{}{}",
                block.title,
                if block.discarded
                    || (block.fold.is_none() && block.pieces.iter().any(|piece| piece.omitted))
                {
                    "  [window; Actions → detail]"
                } else {
                    ""
                }
            );
            let style = if block.role == Role::Tool {
                Style::default().fg(color)
            } else {
                Style::default().fg(color).add_modifier(Modifier::BOLD)
            };
            frame.render_widget(
                Paragraph::new(Line::from(vec![
                    Span::styled(marker, style.fg(marker_color)),
                    Span::raw(" "),
                    Span::styled(
                        ellipsis(&label, usize::from(label_width.saturating_sub(2))),
                        style,
                    ),
                ])),
                Rect::new(body.x, y, label_width, 1),
            );
            if let Some(elapsed) = elapsed {
                let width = u16::try_from(elapsed.width()).unwrap_or(0).min(body.width);
                frame.render_widget(
                    Paragraph::new(elapsed).style(muted()),
                    Rect::new(body.right() - width, y, width, 1),
                );
            }
            continue;
        }
        if text_cache
            .as_ref()
            .is_none_or(|(index, ..)| *index != row.block)
        {
            text_cache = Some((
                row.block,
                cache.get(block, body.width),
                block.anchor_index(),
            ));
        }
        let layout = &text_cache.as_ref().expect("cached block").1;
        let text = &layout.text;
        let mut x = body.x;
        if block.role == Role::User {
            if row.offset == 0 {
                frame.render_widget(
                    Paragraph::new("›").style(user_style()),
                    Rect::new(x, y, 1, 1),
                );
            }
            x += 2;
        }
        for (offset, grapheme) in text[row.offset..row.end].grapheme_indices(true) {
            let offset = row.offset + offset;
            let width = if grapheme == "\t" {
                4 - usize::from(x - body.x - if block.role == Role::User { 2 } else { 0 }) % 4
            } else {
                grapheme.width().max(1)
            };
            let width = u16::try_from(width)
                .unwrap_or(1)
                .min(body.right().saturating_sub(x));
            if width == 0 {
                break;
            }
            let anchors = &text_cache.as_ref().expect("cached block").2;
            let source = layout.source_range(offset..offset + grapheme.len());
            if let Some(range) = &source
                && let (Some(a), Some(b)) = (anchors.anchor(range.start), anchors.anchor(range.end))
            {
                view.hits.push((y, x, width, a, b));
            }
            let highlight = source.as_ref().is_some_and(|range| {
                selected.is_some_and(|(a, b)| {
                    (row.block, range.end) > a && (row.block, range.start) < b
                })
            });
            let style = if highlight {
                Style::default().bg(Color::Cyan).fg(Color::Black)
            } else {
                let styles = &layout.styles;
                styles
                    .get(
                        styles
                            .partition_point(|(range, _)| range.start <= offset)
                            .saturating_sub(1),
                    )
                    .filter(|(range, _)| range.contains(&offset))
                    .map_or(Style::default().fg(color), |(_, style)| *style)
            };
            let style = if block.role == Role::User && !highlight {
                style.patch(user_style())
            } else {
                style
            };
            let display = if grapheme == "\t" {
                &"    "[..usize::from(width)]
            } else {
                grapheme
            };
            write_grapheme(
                frame.buffer_mut(),
                Rect::new(x, y, width, 1),
                display,
                style,
            );
            x += width;
        }
    }
    let edit_area = Rect::new(
        0,
        area.height - editor_height - footer_height,
        area.width,
        editor_height,
    );
    view.editor = Some(edit_area);
    composer_prepared(frame, edit_area, &display, caret, None);
    if let Some(popup) = state.completion {
        completion(frame, popup, edit_area.y, &mut view);
    }
    let mut panel = crate::dialog::Panel {
        revision: state.menu_revision,
        status: state.status,
        ..crate::dialog::Panel::default()
    };
    if let Some(menu) = &state.menu {
        panel.title = menu.title;
        panel.items.clone_from(&menu.items);
        panel.selected = menu.selected;
        panel.hint = "Enter select · Esc back";
        crate::dialog::draw(frame, &panel, true, &mut view);
    } else if let Some(edit) = &state.ui_edit {
        panel.title = "Edit field";
        panel.field = Some(crate::dialog::Field {
            label: edit.label,
            text: &edit.editor.text,
            cursor: edit.editor.cursor,
        });
        panel.detail = state.detail;
        panel.offset = state.detail_offset;
        panel.hint = "Enter save · Esc discard";
        crate::dialog::draw(frame, &panel, true, &mut view);
    } else if let Some(answer) = &state.answer
        && let Some(question) = answer.request.questions.get(answer.answered)
    {
        let text = format!(
            "{}\n\n{}",
            question.prompt,
            question
                .options
                .iter()
                .enumerate()
                .map(|(i, value)| format!("{}. {value}", i + 1))
                .collect::<Vec<_>>()
                .join("\n")
        );
        let title = format!(
            "Live question {}/{}",
            answer.answered + 1,
            answer.request.questions.len()
        );
        panel.title = &title;
        panel.detail = Some(&text);
        panel.offset = answer.scroll;
        panel.field = Some(crate::dialog::Field {
            label: "Option number or answer",
            text: &answer.editor.text,
            cursor: answer.editor.cursor,
        });
        panel.hint = "Enter answer · PgUp/Dn scroll · Esc back";
        crate::dialog::draw(frame, &panel, true, &mut view);
    } else if let Some(detail) = state.detail {
        panel.title = "Detail";
        panel.detail = Some(detail);
        panel.offset = state.detail_offset;
        panel.hint = "Enter actions · ←/→ pages · Esc back";
        crate::dialog::draw(frame, &panel, true, &mut view);
    }
    view
}

fn write_grapheme(buffer: &mut ratatui::buffer::Buffer, area: Rect, text: &str, style: Style) {
    buffer.set_stringn(area.x, area.y, text, usize::from(area.width), style);
    buffer.set_style(area, style);
}

pub(crate) fn editor_rows(text: &str, cursor: usize, width: usize, height: usize) -> String {
    let mut rows = VecDeque::with_capacity(height);
    let mut cursor_seen = false;
    each_row(text, width.max(1), |start, end| {
        if rows.len() == height {
            if cursor_seen {
                return false;
            }
            rows.pop_front();
        }
        rows.push_back(&text[start..end]);
        cursor_seen |= cursor >= start && cursor < end || cursor == text.len() && end == text.len();
        true
    });
    rows.into_iter().collect::<Vec<_>>().join("\n")
}

type MarkdownStyles = Vec<(std::ops::Range<usize>, Style)>;

fn user_style() -> Style {
    Style::default().fg(Color::Gray).bg(Color::Indexed(235))
}

pub(crate) fn muted() -> Style {
    Style::default().add_modifier(Modifier::DIM)
}
pub(crate) fn accent() -> Style {
    Style::default()
        .fg(Color::Gray)
        .add_modifier(Modifier::BOLD)
}

pub(crate) fn border() -> Style {
    Style::default().fg(Color::Indexed(239))
}

pub(crate) fn choice_style(selected: bool) -> Style {
    let style = Style::default().fg(Color::Gray).bg(Color::Indexed(235));
    if selected {
        style.bg(Color::Indexed(237)).add_modifier(Modifier::BOLD)
    } else {
        style
    }
}

/// Shared prompt geometry; editing stays with the resident application.
pub(crate) fn composer(
    frame: &mut Frame<'_>,
    area: Rect,
    text: &str,
    cursor: usize,
    title: Option<&str>,
) {
    let (display, caret) = composer_display(text, cursor);
    composer_prepared(frame, area, &display, caret, title);
}

fn composer_prepared(
    frame: &mut Frame<'_>,
    area: Rect,
    display: &str,
    caret: usize,
    title: Option<&str>,
) {
    if area.width < 5 || area.height < 3 {
        return;
    }
    let mut rule = Block::default()
        .borders(ratatui::widgets::Borders::TOP | ratatui::widgets::Borders::BOTTOM)
        .border_style(border());
    if let Some(title) = title.filter(|title| !title.is_empty()) {
        rule = rule.title(title);
    }
    frame.render_widget(rule, area);
    frame.render_widget(
        Paragraph::new("›").style(accent()),
        Rect::new(area.x, area.y + 1, 1, 1),
    );
    let visible = editor_rows(
        display,
        caret,
        usize::from(area.width - 2),
        usize::from(area.height - 2),
    );
    frame.render_widget(
        Paragraph::new(visible),
        Rect::new(area.x + 2, area.y + 1, area.width - 2, area.height - 2),
    );
}

fn composer_display(text: &str, cursor: usize) -> (String, usize) {
    let mut display = crate::terminal_text(&text[..cursor]);
    let caret = display.len();
    display.push('▏');
    display.push_str(&crate::terminal_text(&text[cursor..]));
    (display, caret)
}

pub(crate) fn ellipsis(text: &str, width: usize) -> String {
    let text = crate::terminal_text(text).replace(['\n', '\r', '\t'], " ");
    if text.width() <= width {
        return text;
    }
    let mut out = String::new();
    let mut used = 0;
    for grapheme in text.graphemes(true) {
        let size = grapheme.width();
        if used + size > width.saturating_sub(1) {
            break;
        }
        out.push_str(grapheme);
        used += size;
    }
    if width > 0 {
        out.push('…');
    }
    out
}

/// Overlay labels above the composer; the acknowledged frame owns its hit map.
pub(crate) fn completion(
    frame: &mut Frame<'_>,
    popup: &crate::scene::Completion,
    bottom: u16,
    view: &mut View,
) {
    let rows = bottom
        .saturating_sub(1)
        .min(8)
        .min(u16::try_from(popup.items.len()).unwrap_or(8));
    if rows == 0 {
        return;
    }
    let area = Rect::new(1, bottom - rows, frame.area().width.saturating_sub(2), rows);
    completion_in(frame, popup, area, view);
}

pub(crate) fn completion_in(
    frame: &mut Frame<'_>,
    popup: &crate::scene::Completion,
    area: Rect,
    view: &mut View,
) {
    let rows = area
        .height
        .min(u16::try_from(popup.items.len()).unwrap_or(8));
    if rows == 0 {
        return;
    }
    let area = Rect {
        height: rows,
        ..area
    };
    let from = popup
        .selected
        .saturating_sub(usize::from(rows) / 2)
        .min(popup.items.len().saturating_sub(usize::from(rows)));
    let width = usize::from(area.width);
    let names = popup
        .items
        .iter()
        .map(|(label, _)| label.width())
        .max()
        .unwrap_or(0)
        .min(24)
        .min(width / 2);
    frame.render_widget(Clear, area);
    for (row, (index, (label, description))) in popup
        .items
        .iter()
        .enumerate()
        .skip(from)
        .take(usize::from(rows))
        .enumerate()
    {
        let selected = index == popup.selected;
        let name = ellipsis(label, names);
        let padding = names.saturating_sub(name.width()) + 2;
        let description = ellipsis(description, width.saturating_sub(2 + names + 2));
        let style = choice_style(selected);
        let line = Line::from(vec![
            Span::styled(if selected { "› " } else { "  " }, style),
            Span::styled(name, style),
            Span::raw(" ".repeat(padding)),
            Span::styled(description, if selected { style } else { muted() }),
        ]);
        frame.render_widget(
            Paragraph::new(line).style(style),
            Rect::new(
                area.x,
                area.y + u16::try_from(row).unwrap_or(0),
                area.width,
                1,
            ),
        );
    }
    view.hits
        .retain(|(y, _, _, _, _)| *y < area.y || *y >= area.bottom());
    view.completion = Some((popup.revision, area, from));
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_state::{TestState as State, draw};
    use ratatui::backend::TestBackend;
    use rsi_agent_session_protocol::{
        EffectId, FrozenAgentSettings, SessionFact, SessionFactBody, SessionHeader, SessionId,
        TurnId,
    };
    use rsi_ai_protocol::{ContentDelta, LanguageEvent, ModelRef};

    fn state() -> State {
        let header = SessionHeader::new(
            SessionId::new("session-utf8").unwrap(),
            1,
            "/workspace/rsiversi",
            rsi_agent_session_protocol::AgentPresetId::new("default").unwrap(),
            FrozenAgentSettings::new(
                "default",
                "system",
                ModelRef::new("deepseek", "deepseek-chat").unwrap(),
                rsi_sandbox::SandboxMode::WorkspaceWrite,
                false,
            )
            .unwrap(),
        )
        .unwrap();
        State::new(header, false)
    }

    fn delta(state: &mut State, seq: u64, text: &str) {
        let fact = SessionFact::new(
            seq,
            seq,
            SessionFactBody::ModelEvent {
                purpose: rsi_agent_session_protocol::ModelEventPurpose::Conversation,
                turn_id: TurnId::new("turn").unwrap(),
                effect_id: EffectId::new("effect").unwrap(),
                event: LanguageEvent::ContentDelta {
                    index: 0,
                    delta: ContentDelta::Text(text.into()),
                },
            },
        )
        .unwrap();
        state.transcript.apply(&fact);
    }

    fn user(state: &mut State, seq: u64, text: &str) {
        state.transcript.apply(
            &SessionFact::new(
                seq,
                seq,
                SessionFactBody::TurnAccepted {
                    turn_id: TurnId::new("user-turn").unwrap(),
                    text: text.into(),
                    model: None,
                    reasoning_effort: None,
                    sandbox: rsi_sandbox::SandboxMode::WorkspaceWrite,
                    require_approval: false,
                },
            )
            .unwrap(),
        );
    }

    #[test]
    fn direct_grapheme_cells_match_paragraph_styles_and_clipping() {
        use ratatui::widgets::Widget as _;
        for text in ["a", "界", "e\u{301}", "👩‍💻", "    ", "\u{301}"] {
            for width in 1..=4 {
                let area = Rect::new(0, 0, width, 1);
                let style = Style::default().fg(Color::Black).bg(Color::Gray);
                let mut before = ratatui::buffer::Buffer::empty(area);
                let mut after = before.clone();
                Paragraph::new(text).style(style).render(area, &mut before);
                write_grapheme(&mut after, area, text, style);
                assert_eq!(after, before, "{text:?} at width {width}");
            }
        }
    }

    #[test]
    #[allow(clippy::too_many_lines)] // One request verifies streaming, backfill, time, geometry and copy authority.
    fn request_metadata_follows_streamed_content_and_reverse_history_backfill() {
        let start = SessionFact::new(
            2,
            1000,
            SessionFactBody::ModelStarted {
                turn_id: TurnId::new("turn").unwrap(),
                effect_id: EffectId::new("effect").unwrap(),
            },
        )
        .unwrap();
        let text = SessionFact::new(
            3,
            1100,
            SessionFactBody::ModelEvent {
                turn_id: TurnId::new("turn").unwrap(),
                effect_id: EffectId::new("effect").unwrap(),
                purpose: rsi_agent_session_protocol::ModelEventPurpose::Conversation,
                event: LanguageEvent::ContentDelta {
                    index: 0,
                    delta: ContentDelta::Text("Answer body".into()),
                },
            },
        )
        .unwrap();
        let mut state = state();
        user(&mut state, 1, "Question");
        state.transcript.apply(&start);
        state.transcript.apply(&text);
        assert_eq!(
            state
                .transcript
                .blocks
                .iter()
                .map(|b| b.role)
                .collect::<Vec<_>>(),
            vec![Role::User, Role::Assistant, Role::Metadata]
        );
        let mut history = Transcript::default();
        history.apply(&text);
        history.apply_history(&start);
        assert_eq!(
            history.blocks.iter().map(|b| b.role).collect::<Vec<_>>(),
            vec![Role::Assistant, Role::Metadata]
        );
        let mut before = ratatui::Terminal::new(TestBackend::new(80, 24)).unwrap();
        before
            .draw(|frame| {
                draw(frame, &state);
            })
            .unwrap();
        assert!(
            !before
                .backend()
                .buffer()
                .content
                .iter()
                .map(ratatui::buffer::Cell::symbol)
                .collect::<String>()
                .contains("Request ·")
        );
        let finished = SessionFact::new(
            4,
            1250,
            SessionFactBody::ModelEvent {
                turn_id: TurnId::new("turn").unwrap(),
                effect_id: EffectId::new("effect").unwrap(),
                purpose: rsi_agent_session_protocol::ModelEventPurpose::Conversation,
                event: LanguageEvent::Finished {
                    reason: rsi_ai_protocol::FinishReason::Stop,
                    replay: None,
                },
            },
        )
        .unwrap();
        state.transcript.apply(&finished);
        let mut reverse = Transcript::default();
        for fact in [&finished, &text, &start] {
            reverse.apply_history(fact);
        }
        let backfilled = reverse
            .blocks
            .iter()
            .find(|b| b.role == Role::Metadata)
            .unwrap();
        assert!(backfilled.completed);
        assert_eq!(
            backfilled.title,
            state.transcript.blocks.last().unwrap().title
        );
        let mut screen = ratatui::Terminal::new(TestBackend::new(80, 24)).unwrap();
        let mut view = View::default();
        screen.draw(|frame| view = draw(frame, &state)).unwrap();
        let cells = screen.backend().buffer();
        let lines = (0..24)
            .map(|y| (0..80).map(|x| cells[(x, y)].symbol()).collect::<String>())
            .collect::<Vec<_>>();
        let answer = lines
            .iter()
            .position(|row| row.contains("Answer body"))
            .unwrap();
        let metadata = lines.iter().rposition(|row| row.starts_with("• ")).unwrap();
        assert_eq!(metadata, answer + 1);
        assert!(lines[metadata + 1].trim().is_empty());
        assert!(lines[metadata].starts_with("• 00:00 · "));
        let y = u16::try_from(metadata).unwrap();
        assert_eq!(cells[(0, y)].bg, Color::Reset);
        assert_eq!(cells[(79, y)].bg, Color::Reset);
        assert!(view.hit(0, y, false).is_none());
        let first = view.hit(0, u16::try_from(answer).unwrap(), false).unwrap();
        let last = view.hit(10, u16::try_from(answer).unwrap(), true).unwrap();
        assert_eq!(
            view.selected_source(&state.transcript, first, last)
                .unwrap(),
            "Answer body"
        );
    }

    #[test]
    #[allow(clippy::too_many_lines)] // One lifecycle crosses the serialized scene at each clock/ownership boundary.
    fn portable_tool_motion_requires_its_live_turn_and_freezes_on_completion() {
        use rsi_tools_protocol::{ToolResult, ToolResultIdentity};
        let mut state = state();
        let turn_id = TurnId::new("running").unwrap();
        let effect_id = EffectId::new("tool").unwrap();
        let identity = ToolResultIdentity::new("owner", "invoke", "call", "a".repeat(64)).unwrap();
        state.transcript.apply(
            &SessionFact::new(
                1,
                1000,
                SessionFactBody::ToolIntent {
                    turn_id: turn_id.clone(),
                    effect_id: effect_id.clone(),
                    identity: identity.clone(),
                    source_model_effect_id: EffectId::new("model").unwrap(),
                    name: "bash".into(),
                    arguments: serde_json::json!({"command":"sleep 2"}),
                    approval: None,
                    parallel_safe: false,
                },
            )
            .unwrap(),
        );
        state.transcript.apply(
            &SessionFact::new(
                2,
                1000,
                SessionFactBody::ToolStarted {
                    turn_id: turn_id.clone(),
                    effect_id: effect_id.clone(),
                    identity: identity.clone(),
                },
            )
            .unwrap(),
        );
        let render = |state: &State, activity: Option<crate::Activity>| {
            let mut input = crate::test_state::input(state);
            input.active = activity.is_some();
            input.activity = activity;
            let scene = crate::scene::Scene::capture(&input, 80, 24).unwrap();
            let scene =
                serde_json::from_slice::<crate::scene::Scene>(&serde_json::to_vec(&scene).unwrap())
                    .unwrap();
            let (cells, _) = scene.render(80, 24).unwrap();
            (0..24)
                .map(|y| (0..80).map(|x| cells[(x, y)].symbol()).collect::<String>())
                .collect::<Vec<_>>()
        };
        let clock = |now_ms| crate::Activity {
            turn_id: turn_id.clone(),
            now_ms,
            started_ms: Some(1000),
        };
        let initial = render(&state, Some(clock(1000)));
        assert!(initial[0].starts_with("⠋ Running"));
        assert!(!initial[0].trim_end().ends_with("0s"));
        assert!(initial[22].contains("⠋ 0s"));
        let frame = render(&state, Some(clock(1120)));
        assert!(frame[0].starts_with("⠙"));
        assert!(frame[22].contains("⠙ 0s"));
        assert!(
            !render(&state, Some(clock(1999)))[0]
                .trim_end()
                .ends_with("0s")
        );
        assert!(render(&state, Some(clock(2000)))[0].ends_with("1s"));
        let foreign = render(
            &state,
            Some(crate::Activity {
                turn_id: TurnId::new("other").unwrap(),
                ..clock(5000)
            }),
        );
        assert!(foreign[0].contains("Interrupted:"));
        assert!(!foreign[0].trim_end().ends_with("4s"));
        let historical = render(&state, None);
        assert!(historical[0].starts_with("▸ Interrupted: run"));
        state.transcript.apply(
            &SessionFact::new(
                3,
                2250,
                SessionFactBody::ToolResult {
                    turn_id,
                    effect_id,
                    identity,
                    result: ToolResult::new(
                        serde_json::json!({"exit_code":0}),
                        vec![rsi_tools_protocol::ToolContent::Text {
                            text: "done".into(),
                        }],
                        false,
                    )
                    .unwrap(),
                    conclusion: None,
                },
            )
            .unwrap(),
        );
        let completed = render(&state, None);
        assert!(completed[0].starts_with("▸ Ran"));
        assert!(completed[0].ends_with("1s"));
        assert!(!completed[22].contains("1s"));
        for (ms, label) in [
            (0, "0s"),
            (59_999, "59s"),
            (60_000, "1m 0s"),
            (3_600_000, "1h 0m"),
        ] {
            assert_eq!(crate::elapsed_label(ms), label);
        }
        for (i, expected) in ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠋"]
            .into_iter()
            .enumerate()
        {
            assert_eq!(
                crate::activity_spinner_frame(u64::try_from(i).unwrap() * 120),
                expected
            );
        }
    }

    #[test]
    fn local_unicode_feedback_stays_within_the_portable_title_budget() {
        let mut state = state();
        state
            .transcript
            .push_notice(1, &"消息🦀\u{1b}".repeat(2048));
        let scene = crate::scene::Scene::capture(&crate::test_state::input(&state), 28, 9).unwrap();
        let (cells, view) = crate::scene::Scene::decode(&scene.encode().unwrap())
            .unwrap()
            .render(28, 9)
            .unwrap();
        assert!(cells.content.iter().any(|cell| cell.symbol() == "消"));
        assert!(view.hits.is_empty());
    }

    #[test]
    fn outcome_colors_only_the_existing_marker_and_preserves_fold_hits() {
        use super::super::transcript::ProcessOutcome;
        let mut state = state();
        delta(&mut state, 1, "original source");
        for role in [Role::Reasoning, Role::Tool] {
            let normal = if role == Role::Tool {
                Color::Gray
            } else {
                Color::DarkGray
            };
            for (outcome, color) in [
                (Some(ProcessOutcome::Success), Color::LightGreen),
                (Some(ProcessOutcome::Failed), Color::LightRed),
                (Some(ProcessOutcome::Interrupted), normal),
                (None, normal),
            ] {
                for collapsed in [false, true] {
                    let block = &mut state.transcript.blocks[0];
                    block.role = role;
                    block.title = "Process".into();
                    block.outcome = outcome;
                    block.completed = outcome.is_some();
                    block.collapsed = collapsed;
                    let scene =
                        crate::scene::Scene::capture(&crate::test_state::input(&state), 80, 24)
                            .unwrap();
                    let (cells, view) = crate::scene::Scene::decode(&scene.encode().unwrap())
                        .unwrap()
                        .render(80, 24)
                        .unwrap();
                    assert_eq!(cells[(0, 0)].symbol(), if collapsed { "▸" } else { "▾" });
                    assert_eq!(cells[(0, 0)].fg, color);
                    assert_eq!(cells[(2, 0)].fg, normal);
                    assert_eq!(cells[(0, 0)].bg, Color::Reset);
                    assert_eq!(
                        view.fold_at(state.header.session_id(), &state.transcript, 0, 0),
                        Some(0)
                    );
                    assert!(view.hit(0, 0, false).is_none());
                    if !collapsed {
                        let a = view.hit(0, 1, false).unwrap();
                        let b = view.hit(14, 1, true).unwrap();
                        assert_eq!(
                            view.selected_source(&state.transcript, a, b).unwrap(),
                            "original source"
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn user_time_is_first_below_message_without_background_or_copy_hits() {
        let mut state = state();
        state.transcript.apply(
            &SessionFact::new(
                1,
                86_340_000,
                SessionFactBody::TurnAccepted {
                    turn_id: TurnId::new("timed").unwrap(),
                    text: "Question".into(),
                    model: None,
                    reasoning_effort: None,
                    sandbox: rsi_sandbox::SandboxMode::WorkspaceWrite,
                    require_approval: false,
                },
            )
            .unwrap(),
        );
        let mut screen = ratatui::Terminal::new(TestBackend::new(80, 24)).unwrap();
        let mut view = View::default();
        screen.draw(|frame| view = draw(frame, &state)).unwrap();
        let cells = screen.backend().buffer();
        assert_eq!(
            (0..80)
                .map(|x| cells[(x, 2)].symbol())
                .collect::<String>()
                .trim(),
            "• 23:59"
        );
        assert!((0..80).all(|x| cells[(x, 1)].bg == Color::Indexed(235)));
        assert!((0..80).all(|x| cells[(x, 3)].symbol() == " " && cells[(x, 2)].bg == Color::Reset));
        assert!((0..80).all(|x| cells[(x, 0)].symbol() == "▄"
            && cells[(x, 0)].fg == Color::Indexed(235)
            && cells[(x, 0)].bg == Color::Reset));
        assert!((0..80).all(|x| view.hit(x, 0, false).is_none()));
        assert!((0..80).all(|x| view.hit(x, 2, false).is_none()));
        assert!(
            view.fold_at(state.header.session_id(), &state.transcript, 0, 2)
                .is_none()
        );
    }

    #[test]
    fn thinking_has_blank_rows_above_and_below_without_trimming_source_whitespace() {
        let mut state = state();
        user(&mut state, 1, "Question");
        let body = "Consider this\n\nCheck result";
        for (seq, index, delta) in [
            (2, 0, ContentDelta::Reasoning(body.into())),
            (3, 1, ContentDelta::Text("Answer".into())),
        ] {
            state.transcript.apply(
                &SessionFact::new(
                    seq,
                    seq,
                    SessionFactBody::ModelEvent {
                        turn_id: TurnId::new("turn").unwrap(),
                        effect_id: EffectId::new("effect").unwrap(),
                        purpose: rsi_agent_session_protocol::ModelEventPurpose::Conversation,
                        event: LanguageEvent::ContentDelta { index, delta },
                    },
                )
                .unwrap(),
            );
        }
        for completed in [false, true] {
            for collapsed in [false, true] {
                state.transcript.blocks[1].completed = completed;
                state.transcript.blocks[1].collapsed = collapsed;
                for (width, height) in [(80, 24), (28, 16)] {
                    let scene = crate::scene::Scene::capture(
                        &crate::test_state::input(&state),
                        width,
                        height,
                    )
                    .unwrap();
                    let (cells, view) = crate::scene::Scene::decode(&scene.encode().unwrap())
                        .unwrap()
                        .render(width, height)
                        .unwrap();
                    let line = |y| {
                        (0..width)
                            .map(|x| cells[(x, y)].symbol())
                            .collect::<String>()
                            .trim_end()
                            .to_owned()
                    };
                    assert_eq!(line(2), "• 00:00");
                    assert_eq!(line(3), "");
                    assert!(line(4).ends_with("Thinking"));
                    assert_eq!(
                        view.fold_at(state.header.session_id(), &state.transcript, 0, 4),
                        Some(1)
                    );
                    if completed && collapsed {
                        assert_eq!(line(5), "");
                        assert_eq!(line(6), "Answer");
                    } else {
                        assert_eq!(line(5), "Consider this");
                        assert_eq!(line(6), "");
                        assert_eq!(line(7), "Check result");
                        assert_eq!(line(8), "");
                        assert_eq!(line(9), "Answer");
                        let a = view.hit(0, 5, false).unwrap();
                        let b = view.hit(11, 7, true).unwrap();
                        assert_eq!(view.selected_source(&state.transcript, a, b).unwrap(), body);
                    }
                }
            }
        }
    }

    #[test]
    fn transcript_starts_at_origin_and_user_filler_has_no_copy_authority() {
        let mut state = state();
        user(
            &mut state,
            1,
            "Copy this
  source indentation",
        );
        delta(&mut state, 2, "Answer without a role label.");
        state.top = state.transcript.blocks[0].anchor(0);
        for (width, height) in [(110, 35), (80, 24), (42, 12), (28, 9)] {
            let mut screen = ratatui::Terminal::new(TestBackend::new(width, height)).unwrap();
            let mut view = View::default();
            screen.draw(|frame| view = draw(frame, &state)).unwrap();
            let cells = screen.backend().buffer();
            assert_eq!(view.area.x, 0);
            assert_eq!(view.area.y, 0);
            assert!(view.area.height >= 3);
            assert_eq!(cells[(0, 1)].symbol(), "›");
            assert_eq!(cells[(2, 1)].symbol(), "C");
            for x in 0..width {
                assert_eq!(cells[(x, 1)].fg, Color::Gray);
                assert_eq!(cells[(x, 1)].bg, Color::Indexed(235));
            }
            assert!(view.hit(0, 1, false).is_none());
            assert!(view.hit(2, 1, false).is_some());
            assert!(view.hit(20, 1, false).is_none());
            let text = cells
                .content
                .iter()
                .map(ratatui::buffer::Cell::symbol)
                .collect::<String>();
            assert!(!text.contains("You"));
            assert!(!text.contains("Assistant"));
            assert_eq!(view.area.height, height - 5);
            let a = view.hit(2, 1, false).unwrap();
            let b = view.hit(10, 1, true).unwrap();
            assert_eq!(
                selected_visible(&state.transcript, a, b, width).unwrap(),
                "Copy this"
            );
        }
        let block = &state.transcript.blocks[0];
        state.selection = Some((block.anchor(0).unwrap(), block.anchor(4).unwrap()));
        let mut screen = ratatui::Terminal::new(TestBackend::new(42, 12)).unwrap();
        screen
            .draw(|frame| {
                draw(frame, &state);
            })
            .unwrap();
        assert_eq!(screen.backend().buffer()[(2, 1)].bg, Color::Cyan);
        assert_eq!(screen.backend().buffer()[(2, 1)].fg, Color::Black);
        assert_eq!(screen.backend().buffer()[(6, 1)].bg, Color::Indexed(235));
    }

    #[test]
    fn footer_and_composer_keep_their_rows_before_and_after_first_input() {
        for (width, height) in [(110, 35), (80, 24), (42, 12), (28, 9)] {
            let mut state = state();
            let mut screen = ratatui::Terminal::new(TestBackend::new(width, height)).unwrap();
            let mut view = View::default();
            screen.draw(|frame| view = draw(frame, &state)).unwrap();
            let cells = screen.backend().buffer();
            assert!(
                cells.content[..usize::from(width * view.area.height)]
                    .iter()
                    .all(|cell| cell.symbol() == " ")
            );
            assert_eq!(view.area.height, height - 5);
            assert_eq!(cells[(0, height - 4)].symbol(), "›");
            assert_eq!(cells[(2, height - 4)].symbol(), "▏");
            let row = (0..width)
                .map(|x| cells[(x, height - 1)].symbol())
                .collect::<String>();
            assert!(!row.contains("Enter submits"));
            assert!(row.contains("/help"));
            assert!(view.hits.is_empty());
            view.validate(width, height).unwrap();
            assert_eq!(view.footer[0].0.y, height - 2);

            state.active = true;
            state.notice("Action failed");
            state.editor.insert(&"wrapped input\n".repeat(8)).unwrap();
            screen.draw(|frame| view = draw(frame, &state)).unwrap();
            assert!(view.area.height >= 3);
            assert!(
                screen
                    .backend()
                    .buffer()
                    .content
                    .iter()
                    .any(|cell| cell.symbol() == "▏")
            );
            view.validate(width, height).unwrap();

            let mut input = crate::test_state::input(&state);
            input.menu = Some(crate::Menu {
                title: "Choose",
                items: vec!["action"],
                selected: 0,
            });
            screen
                .draw(|frame| {
                    view = super::draw(frame, &input, &mut LayoutCache::default());
                })
                .unwrap();
            assert!(view.dialog.is_some());
            assert!(view.editor.is_none(), "menu has no editable field");
            assert!(view.footer.is_empty());
            view.validate(width, height).unwrap();

            state.active = false;
            state.status.clear();
            state.editor = crate::editor::Editor::default();
            user(&mut state, 1, "First message");
            screen.draw(|frame| view = draw(frame, &state)).unwrap();
            assert_eq!(view.area.height, height - 5);
            let cells = screen.backend().buffer();
            let row = (0..view.area.height)
                .find(|y| cells[(2, *y)].symbol() == "F")
                .unwrap();
            assert_eq!(cells[(0, row)].symbol(), "›");
        }
    }

    #[test]
    fn process_preview_has_source_gap_and_completion_preserves_manual_expansion() {
        let mut state = state();
        let body = (0..10)
            .map(|i| format!("line {i}"))
            .collect::<Vec<_>>()
            .join("\n");
        state.transcript.apply(
            &SessionFact::new(
                1,
                1,
                SessionFactBody::ModelEvent {
                    turn_id: TurnId::new("turn").unwrap(),
                    effect_id: EffectId::new("effect").unwrap(),
                    purpose: rsi_agent_session_protocol::ModelEventPurpose::Conversation,
                    event: LanguageEvent::ContentDelta {
                        index: 0,
                        delta: ContentDelta::Reasoning(body.clone()),
                    },
                },
            )
            .unwrap(),
        );
        let mut cache = LayoutCache::default();
        let visible = rows(&state.transcript, &mut cache, None, None, 42, 20);
        assert_eq!(visible.len(), 7); // summary, two first rows, gap, three last rows
        assert_eq!(visible[3].hidden, 5);
        let block = &state.transcript.blocks[0];
        let a = block.anchor(0).unwrap();
        let b = block.anchor(body.len()).unwrap();
        assert!(selected_visible(&state.transcript, a, b, 42).is_err());
        assert_eq!(
            selected_visible(&state.transcript, a, block.anchor(6).unwrap(), 42).unwrap(),
            "line 0"
        );
        state.transcript.apply(
            &SessionFact::new(
                2,
                2,
                SessionFactBody::ModelEvent {
                    turn_id: TurnId::new("turn").unwrap(),
                    effect_id: EffectId::new("effect").unwrap(),
                    purpose: rsi_agent_session_protocol::ModelEventPurpose::Conversation,
                    event: LanguageEvent::ContentFinished { index: 0 },
                },
            )
            .unwrap(),
        );
        assert_eq!(
            rows(&state.transcript, &mut cache, None, None, 42, 20).len(),
            1
        );
        state.transcript.blocks[0].collapsed = false;
        assert_eq!(
            rows(&state.transcript, &mut cache, None, None, 42, 20).len(),
            11
        );
        assert_eq!(selected_visible(&state.transcript, a, b, 42).unwrap(), body);
    }

    #[test]
    fn task_dock_omits_empty_and_completed_state_and_preserves_the_source_anchor() {
        use rsi_agent_todo::{TodoItem, TodoList, TodoStatus};
        for (width, height) in [(110, 35), (80, 24), (42, 12), (28, 9)] {
            let mut state = state();
            state.transcript.apply(
                &SessionFact::new(
                    1,
                    1,
                    SessionFactBody::TurnAccepted {
                        turn_id: TurnId::new("tasks").unwrap(),
                        text: "Readable source".into(),
                        model: None,
                        reasoning_effort: None,
                        sandbox: rsi_sandbox::SandboxMode::WorkspaceWrite,
                        require_approval: false,
                    },
                )
                .unwrap(),
            );
            let anchor = state.transcript.blocks[0].anchor(0).unwrap();
            state.top = Some(anchor);
            let mut screen = ratatui::Terminal::new(TestBackend::new(width, height)).unwrap();
            let mut before = None;
            screen
                .draw(|frame| before = Some(draw(frame, &state)))
                .unwrap();
            state.todos = Some(
                TodoList::new(vec![
                    TodoItem::new("Inspect source".into(), TodoStatus::InProgress).unwrap(),
                    TodoItem::new("Verify result".into(), TodoStatus::InProgress).unwrap(),
                ])
                .unwrap(),
            );
            let mut with_tasks = None;
            screen
                .draw(|frame| with_tasks = Some(draw(frame, &state)))
                .unwrap();
            let view = with_tasks.unwrap();
            assert_eq!(view.area.y, 0);
            assert!(view.area.height >= 3);
            assert_eq!(view.anchors.first().copied().flatten(), Some(anchor));
            assert!(
                view.hit(0, view.area.height, false).is_none(),
                "task row is not Transcript source"
            );
            let text = screen
                .backend()
                .buffer()
                .content
                .iter()
                .map(ratatui::buffer::Cell::symbol)
                .collect::<String>();
            assert!(text.contains("Tasks 0/2"));
            state.todos = Some(TodoList::default());
            let mut empty = None;
            screen
                .draw(|frame| empty = Some(draw(frame, &state)))
                .unwrap();
            let before = before.unwrap();
            assert_eq!(empty.unwrap().area, before.area);
            state.todos = Some(
                TodoList::new(vec![
                    TodoItem::new("Finished task".into(), TodoStatus::Completed).unwrap(),
                ])
                .unwrap(),
            );
            screen
                .draw(|frame| {
                    assert_eq!(draw(frame, &state).area, before.area);
                })
                .unwrap();
            state.active = true;
            screen
                .draw(|frame| {
                    draw(frame, &state);
                })
                .unwrap();
            assert!(
                screen
                    .backend()
                    .buffer()
                    .content
                    .iter()
                    .map(ratatui::buffer::Cell::symbol)
                    .collect::<String>()
                    .contains("Working")
            );
            assert!(
                !screen
                    .backend()
                    .buffer()
                    .content
                    .iter()
                    .map(ratatui::buffer::Cell::symbol)
                    .collect::<String>()
                    .contains("Tasks")
            );
        }
    }

    #[test]
    fn acknowledged_fold_map_survives_streaming_and_hidden_anchor_keeps_its_process_position() {
        let mut state = state();
        let text = (0..10)
            .map(|i| format!("line {i}"))
            .collect::<Vec<_>>()
            .join("\n");
        let delta = |seq, text| {
            SessionFact::new(
                seq,
                seq,
                SessionFactBody::ModelEvent {
                    turn_id: TurnId::new("turn").unwrap(),
                    effect_id: EffectId::new("effect").unwrap(),
                    purpose: rsi_agent_session_protocol::ModelEventPurpose::Conversation,
                    event: LanguageEvent::ContentDelta {
                        index: 0,
                        delta: ContentDelta::Reasoning(text),
                    },
                },
            )
            .unwrap()
        };
        state.transcript.apply(&delta(1, text.clone()));
        let mut screen = ratatui::Terminal::new(TestBackend::new(42, 20)).unwrap();
        let mut view = View::default();
        screen.draw(|frame| view = draw(frame, &state)).unwrap();
        let a = state.transcript.blocks[0]
            .anchor(text.find("line 7").unwrap())
            .unwrap();
        let b = state.transcript.blocks[0].anchor(text.len()).unwrap();
        let first = state.transcript.blocks[0].anchor(0).unwrap();
        assert_eq!(
            view.selected_source(&state.transcript, a, b).unwrap(),
            "line 7\nline 8\nline 9"
        );
        assert!(view.selected_source(&state.transcript, first, b).is_err());
        state
            .transcript
            .apply(&delta(2, "\nline 10\nline 11\nline 12".into()));
        assert!(
            selected_visible(&state.transcript, a, b, 42).is_err(),
            "the new fold has hidden the old tail"
        );
        assert_eq!(
            view.selected_source(&state.transcript, a, b).unwrap(),
            "line 7\nline 8\nline 9",
            "copy follows what was displayed"
        );
        let hidden = state.transcript.blocks[0]
            .anchor(text.find("line 4").unwrap())
            .unwrap();
        let mut cache = LayoutCache::default();
        let visible = rows(&state.transcript, &mut cache, Some(hidden), None, 42, 12);
        assert!(visible[0].hidden > 0);
        assert_eq!(visible[0].block, 0);
        state.transcript.blocks[0].completed = true;
        assert!(rows(&state.transcript, &mut cache, Some(hidden), None, 42, 12)[0].title);
        state.transcript.blocks[0].collapsed = false;
        let visible = rows(&state.transcript, &mut cache, Some(hidden), None, 42, 12);
        assert_eq!(visible[0].offset, text.find("line 4").unwrap());
    }

    #[test]
    fn footer_hits_cover_only_action_labels_and_narrow_child_effort_stays_bounded() {
        let mut state = state();
        let input = crate::test_state::input(&state);
        let footer = footer::build(&input, 110, 34);
        assert!(footer.text.starts_with("deepseek-chat · default"));
        assert_eq!(footer.actions[0].0.x, 0);
        state.reasoning_effort = Some(rsi_ai_protocol::ReasoningEffortId::new("high").unwrap());
        let input = crate::test_state::input(&state);
        let footer = footer::build(&input, 110, 34);
        let (area, kind) = footer
            .actions
            .iter()
            .find(|(_, kind)| *kind == FooterAction::Model)
            .unwrap();
        assert_eq!(*kind, FooterAction::Model);
        assert_eq!(
            area.width,
            u16::try_from("deepseek-chat · high".width()).unwrap()
        );
        let suffix = footer
            .text
            .chars()
            .skip(area.right() as usize)
            .collect::<String>();
        assert!(suffix.contains("/workspace/rsiversi"));
        let selection =
            rsi_agent_session_protocol::ModelSelection::baseline(state.header.settings());
        state.header = state
            .header
            .forked_child(
                SessionId::new("child-session").unwrap(),
                2,
                rsi_agent_session_protocol::ForkOrigin {
                    parent_session_id: state.header.session_id().clone(),
                    root_session_id: state.header.session_id().clone(),
                    path: rsi_agent_session_protocol::AgentPath::new(vec![1]).unwrap(),
                    task_name: "child".into(),
                    parent_header_fingerprint: state.header.fingerprint().unwrap(),
                    invoking_turn_id: TurnId::new("spawn").unwrap(),
                    resolved_after_seq: 0,
                    resolved_terminal_seq: 0,
                    terminal_prefix_sha256: "0".repeat(64),
                    resolved_terminal_control_seq: 0,
                    terminal_control_prefix_sha256: "0".repeat(64),
                    requested_turns: rsi_agent_session_protocol::ForkTurnSelection::None,
                    effective_turns: 0,
                },
                selection,
            )
            .unwrap();
        state.reasoning_effort =
            Some(rsi_ai_protocol::ReasoningEffortId::new("x".repeat(32)).unwrap());
        for unavailable in [false, true] {
            let mut input = crate::test_state::input(&state);
            input.model_unavailable = unavailable;
            let footer = footer::build(&input, 28, 8);
            assert!(footer.text.width() <= 28);
            assert_eq!(
                footer.actions[0],
                (Rect::new(0, 8, 1, 1), FooterAction::Parent)
            );
            assert!(footer.actions[0].0.right() <= footer.actions[1].0.x);
            if unavailable {
                assert!(footer.text.contains("unavailable"));
            } else {
                assert!(footer.text.contains(" · "));
            }
        }
    }

    #[test]
    fn composer_height_counts_the_caret_and_modal_frames_carry_no_transcript_hits() {
        let mut state = state();
        user(&mut state, 1, "visible body");
        state.editor = crate::editor::Editor::with_text("a".repeat(26), 1024);
        let mut screen = ratatui::Terminal::new(TestBackend::new(28, 9)).unwrap();
        let mut view = None;
        screen
            .draw(|frame| view = Some(draw(frame, &state)))
            .unwrap();
        assert_eq!(view.take().unwrap().area.height, 3);
        assert_eq!(screen.backend().buffer()[(0, 4)].symbol(), "›");
        assert_eq!(screen.backend().buffer()[(2, 4)].symbol(), "a");
        assert_eq!(screen.backend().buffer()[(2, 5)].symbol(), "▏");
        let mut input = crate::test_state::input(&state);
        input.menu_revision = 7;
        input.menu = Some(crate::Menu {
            title: "Choose",
            items: vec!["action"],
            selected: 0,
        });
        screen
            .draw(|frame| view = Some(super::draw(frame, &input, &mut LayoutCache::default())))
            .unwrap();
        let view = view.unwrap();
        assert_eq!(view.choice_revision, 7);
        assert!(view.hits.is_empty());
        assert!(view.rows.is_empty());
        assert!(view.footer.is_empty());
        view.validate(28, 9).unwrap();
    }

    #[test]
    fn dock_yields_to_minimum_transcript_and_composer_and_menu_publishes_rows() {
        let mut state = state();
        user(&mut state, 1, "Context");
        state.questions = 1;
        state.approvals = 1;
        state.active = true;
        state.notice("Recoverable failure");
        state.editor.insert(&"line\n".repeat(12)).unwrap();
        for (width, height) in [(110, 35), (80, 24), (42, 12), (28, 9)] {
            let mut screen = ratatui::Terminal::new(TestBackend::new(width, height)).unwrap();
            let mut view = View::default();
            screen.draw(|frame| view = draw(frame, &state)).unwrap();
            assert!(view.area.height >= 3);
            view.validate(width, height).unwrap();
        }
        let mut input = crate::test_state::input(&state);
        input.menu_revision = 71;
        input.menu = Some(crate::Menu {
            title: "Actions",
            items: vec!["first", "second", "third"],
            selected: 1,
        });
        let mut screen = ratatui::Terminal::new(TestBackend::new(28, 9)).unwrap();
        let mut view = View::default();
        screen
            .draw(|frame| view = super::draw(frame, &input, &mut LayoutCache::default()))
            .unwrap();
        assert_eq!(view.choice_revision, 71);
        assert_eq!(view.choices, vec![(1, 0), (2, 1), (3, 2)]);
        assert!(view.hits.is_empty());
    }

    #[test]
    fn long_menu_titles_preserve_controls_feedback_and_modal_hit_bounds() {
        let mut state = state();
        user(&mut state, 1, "visible body");
        state.notice("Action failed");
        let title = "command with long arguments ".repeat(40);
        let mut input = crate::test_state::input(&state);
        input.menu = Some(crate::Menu {
            title: &title,
            items: vec!["Read stdout"],
            selected: 0,
        });
        for (width, height) in [(110, 35), (80, 24), (42, 12), (28, 9)] {
            let mut screen = ratatui::Terminal::new(TestBackend::new(width, height)).unwrap();
            let mut view = View::default();
            screen
                .draw(|frame| view = super::draw(frame, &input, &mut LayoutCache::default()))
                .unwrap();
            let buffer = screen.backend().buffer();
            let row = |y| {
                (0..width)
                    .map(|x| buffer[(x, y)].symbol())
                    .collect::<String>()
            };
            let dialog = view.dialog.unwrap();
            assert!(row(dialog.bottom() - 2).contains("Enter"));
            assert!(row(dialog.bottom() - 2).contains("Esc"));
            assert!(row(dialog.bottom() - 3).contains("Action failed"));
            assert!(view.choice_at(0, view.choices[0].0).is_none());
            assert!(view.hits.is_empty() && view.footer.is_empty());
            view.validate(width, height).unwrap();
        }
    }

    #[test]
    fn cached_layout_reuses_unchanged_bodies_and_isolates_one_changed_block() {
        let mut state = state();
        state.transcript.apply(
            &SessionFact::new(
                1,
                1,
                SessionFactBody::TurnAccepted {
                    reasoning_effort: None,
                    turn_id: TurnId::new("input").unwrap(),
                    text: "first body".into(),
                    model: None,
                    sandbox: rsi_sandbox::SandboxMode::WorkspaceWrite,
                    require_approval: false,
                },
            )
            .unwrap(),
        );
        delta(&mut state, 2, "## Header\n\nUnicode e");
        let mut terminal = ratatui::Terminal::new(TestBackend::new(80, 24)).unwrap();
        terminal
            .draw(|frame| {
                draw(frame, &state);
            })
            .unwrap();
        assert_eq!(state.layout.borrow().builds, 2);
        let original = terminal.backend().buffer().clone();
        terminal
            .draw(|frame| {
                draw(frame, &state);
            })
            .unwrap();
        assert_eq!(
            state.layout.borrow().builds,
            2,
            "unchanged bodies perform zero layout"
        );
        assert_eq!(*terminal.backend().buffer(), original);
        state.notice("only status changed");
        terminal
            .draw(|frame| {
                draw(frame, &state);
            })
            .unwrap();
        assert_eq!(state.layout.borrow().builds, 2);
        delta(&mut state, 3, "\u{301} 界 👩🏽‍💻");
        terminal
            .draw(|frame| {
                draw(frame, &state);
            })
            .unwrap();
        assert_eq!(
            state.layout.borrow().builds,
            3,
            "one block update relayouts only that block"
        );
        state.transcript.blocks[0].collapsed = true;
        terminal
            .draw(|frame| {
                draw(frame, &state);
            })
            .unwrap();
        assert_eq!(state.layout.borrow().builds, 4);
        resize(&mut terminal, 42, 24).unwrap();
        terminal
            .draw(|frame| {
                draw(frame, &state);
            })
            .unwrap();
        assert_eq!(
            state.layout.borrow().builds,
            6,
            "width invalidates both blocks"
        );
        state.transcript.blocks.remove(0);
        terminal
            .draw(|frame| {
                draw(frame, &state);
            })
            .unwrap();
        assert_eq!(
            state.layout.borrow().builds,
            6,
            "stable block identity survives index changes"
        );
        let retained = state.transcript.clone();
        state = State::new(state.header.clone(), false);
        state.transcript = retained;
        terminal
            .draw(|frame| {
                draw(frame, &state);
            })
            .unwrap();
        assert_eq!(
            state.layout.borrow().builds,
            1,
            "new attachment starts a fresh cache"
        );
    }

    #[test]
    fn historical_mapping_and_eviction_invalidate_only_their_owned_layout() {
        let mut state = state();
        delta(&mut state, 2, "\u{301}界");
        let live = state.transcript.clone();
        let mut terminal = ratatui::Terminal::new(TestBackend::new(80, 24)).unwrap();
        terminal
            .draw(|frame| {
                draw(frame, &state);
            })
            .unwrap();
        delta(&mut state, 1, "e");
        let block = &state.transcript.blocks[0];
        let first = block.anchor(0).unwrap();
        let last = block.anchor(block.text().len()).unwrap();
        state.selection = Some((first, last));
        terminal
            .draw(|frame| {
                draw(frame, &state);
            })
            .unwrap();
        assert_eq!(state.layout.borrow().builds, 2);
        assert_eq!(
            state.transcript.selected(first, last).unwrap(),
            "e\u{301}界"
        );
        assert_eq!(first.source.seq, 1);
        assert_eq!(last.source.seq, 2);
        state.transcript = live;
        terminal
            .draw(|frame| {
                draw(frame, &state);
            })
            .unwrap();
        assert_eq!(state.layout.borrow().builds, 3);
        assert!(state.transcript.locate(first).is_none());
        delta(&mut state, 3, &"a".repeat(200 * 1024));
        terminal
            .draw(|frame| {
                draw(frame, &state);
            })
            .unwrap();
        delta(&mut state, 4, &"b".repeat(200 * 1024));
        terminal
            .draw(|frame| {
                draw(frame, &state);
            })
            .unwrap();
        assert_eq!(state.layout.borrow().builds, 5);
        assert!(
            state.transcript.locate(last).is_none(),
            "evicted source cannot retain a hit mapping"
        );
        assert!(state.transcript.blocks[0].discarded);
        assert!(state.transcript.blocks[0].text().starts_with('b'));
        terminal
            .draw(|frame| {
                draw(frame, &state);
            })
            .unwrap();
        assert_eq!(state.layout.borrow().builds, 5);
    }

    #[test]
    fn frame_source_index_matches_exact_lookup_and_rejects_stale_or_hidden_offsets() {
        let mut state = state();
        for seq in 1..=256 {
            user(&mut state, seq, "界e\u{301}\tline\nnext");
        }
        let transcript = &state.transcript;
        for block in &transcript.blocks {
            for piece in &block.pieces {
                for offset in 0..=piece.text.len() + 2 {
                    let anchor = Anchor {
                        source: piece.source,
                        offset,
                    };
                    assert_eq!(
                        transcript.contains_anchors([anchor]),
                        transcript.locate(anchor).is_some()
                    );
                }
            }
        }
        let mut screen = ratatui::Terminal::new(TestBackend::new(110, 35)).unwrap();
        let mut view = View::default();
        screen.draw(|frame| view = draw(frame, &state)).unwrap();
        assert!(view.sources_belong_to(state.header.session_id(), &state.transcript));
        view.hits[0].3.source.seq = u64::MAX;
        assert!(!view.sources_belong_to(state.header.session_id(), &state.transcript));
    }
    #[test]
    fn cached_tail_and_fold_skip_rows_outside_the_visible_window() {
        let mut state = state();
        delta(&mut state, 1, &"row\n".repeat(50_000));
        for collapsed in [false, true] {
            state.transcript.blocks[0].role = if collapsed {
                Role::Reasoning
            } else {
                Role::Assistant
            };
            state.transcript.blocks[0].collapsed = collapsed;
            let mut cache = LayoutCache::default();
            let visible = rows(&state.transcript, &mut cache, None, None, 28, 12);
            assert!(!visible.is_empty());
            assert!(
                cache.enumerated_rows <= 24,
                "visited {} cached rows for a 12-row screen",
                cache.enumerated_rows
            );
            if collapsed {
                assert!(visible.iter().any(|row| row.hidden > 0));
            }
        }
    }
    #[test]
    fn prose_remains_expanded_and_anchor_keeps_current_source_mapping() {
        let mut state = state();
        delta(&mut state, 1, "one\ntwo\nthree\nfour\nfive");
        let block = &mut state.transcript.blocks[0];
        block.collapsed = true;
        state.top = block.anchor(8);
        let mut cache = LayoutCache::default();
        let visible = rows(&state.transcript, &mut cache, state.top, None, 30, 5);
        assert_eq!(
            visible
                .iter()
                .map(|row| (row.offset, row.end))
                .collect::<Vec<_>>(),
            vec![(8, 13), (14, 18), (19, 23)]
        );
        let before = cache.builds;
        delta(&mut state, 2, "\nnext");
        let visible = rows(&state.transcript, &mut cache, state.top, None, 30, 5);
        assert_eq!(visible[0].offset, 8);
        assert_eq!(cache.builds, before + 1);
        let block = &state.transcript.blocks[0];
        assert_eq!(
            state
                .transcript
                .selected(block.anchor(8).unwrap(), block.anchor(18).unwrap())
                .unwrap(),
            "three\nfour"
        );
    }

    #[test]
    fn graphemes_cross_fact_boundaries_and_selection_survives_resize() {
        let mut state = state();
        delta(&mut state, 1, "中e");
        delta(&mut state, 2, "\u{301} 👩");
        delta(&mut state, 3, "🏽‍💻 end\nnext line");
        let block = &state.transcript.blocks[0];
        let a = block.anchor(0).unwrap();
        let b = block.anchor(block.text().len()).unwrap();
        state.selection = Some((a, b));
        for (width, height) in [(80, 24), (42, 12), (28, 9), (110, 35)] {
            let mut terminal = ratatui::Terminal::new(TestBackend::new(width, height)).unwrap();
            let mut view = View::default();
            terminal.draw(|frame| view = draw(frame, &state)).unwrap();
            assert!(view.rows.len() <= usize::from(view.area.height).max(1) * 2);
            let cells = terminal.backend().buffer();
            assert!(
                cells
                    .content
                    .iter()
                    .all(|cell| !cell.symbol().contains(['\x1b', '\r']))
            );
            assert_eq!(
                state.transcript.selected(a, b).unwrap(),
                "中e\u{301} 👩🏽‍💻 end\nnext line"
            );
        }
    }

    #[test]
    fn wrapped_editor_keeps_the_cursor_and_small_terminals_do_not_underflow() {
        let mut state = state();
        state.editor.insert(&"中文👩🏽‍💻".repeat(1000)).unwrap();
        let text = format!("{}▏", state.editor.text);
        let visible = editor_rows(&text, state.editor.text.len(), 35, 3);
        assert!(visible.contains('▏'));
        assert!(visible.lines().count() <= 3);
        for (width, height) in [(1, 1), (20, 8), (28, 9), (28, 10), (42, 12)] {
            let mut terminal = ratatui::Terminal::new(TestBackend::new(width, height)).unwrap();
            terminal
                .draw(|frame| {
                    draw(frame, &state);
                })
                .unwrap();
        }
    }

    #[test]
    fn visual_scenes_are_bounded_and_exportable() {
        let mut state = state();
        user(
            &mut state,
            1,
            "Fix the UTF-8 boundary bug and preserve the source indentation.",
        );
        state.reasoning_effort = Some(rsi_ai_protocol::ReasoningEffortId::new("high").unwrap());
        state.active = true;
        state.questions = 1;
        state.approvals = 1;
        state.actual_model = Some("actual deepseek/deepseek-chat".into());
        delta(
            &mut state,
            2,
            "I found the UTF-8 boundary bug in src/lib.rs.\n\nThe fix walks backward to a valid character boundary:\n\n```rust\nwhile !input.is_char_boundary(end) {\n    end -= 1;\n}\n```\n\n中文、combining e\u{301}, and 👩🏽‍💻 stay intact.\nTests cover empty input, byte limits, and usize::MAX.",
        );
        state
            .editor
            .insert("补充：请保留已有更改。\nRun the focused tests after the patch.")
            .unwrap();
        state.notice("Command completed · exit code 0 · durable Tool result recorded");
        for (width, height) in [(110, 35), (80, 24), (42, 12), (28, 9)] {
            let mut terminal = ratatui::Terminal::new(TestBackend::new(width, height)).unwrap();
            terminal
                .draw(|frame| {
                    draw(frame, &state);
                })
                .unwrap();
            let buffer = terminal.backend().buffer();
            if let Some(directory) = std::env::var_os("RSI_TUI_VISUAL_DIR") {
                let directory = std::path::PathBuf::from(directory);
                std::fs::create_dir_all(&directory).unwrap();
                let cells = buffer.content.iter().enumerate().map(|(index,cell)| serde_json::json!({"x":index % usize::from(width),"y":index / usize::from(width),"text":cell.symbol(),"fg":format!("{:?}",cell.fg),"bg":format!("{:?}",cell.bg)})).collect::<Vec<_>>();
                std::fs::write(
                    directory.join(format!("scene-{width}x{height}.json")),
                    serde_json::to_vec(
                        &serde_json::json!({"width":width,"height":height,"cells":cells}),
                    )
                    .unwrap(),
                )
                .unwrap();
            }
            assert_eq!(buffer.area.width, width);
            assert!(buffer.content.iter().any(|cell| cell.symbol() == "›"));
        }
    }

    #[test]
    fn resize_changes_the_backend_and_subsequent_frame_dimensions() {
        let state = state();
        let mut terminal = ratatui::Terminal::new(TestBackend::new(110, 35)).unwrap();
        for (width, height) in [(42, 12), (80, 24), (28, 9)] {
            resize(&mut terminal, width, height).unwrap();
            terminal
                .draw(|frame| {
                    assert_eq!(frame.area(), Rect::new(0, 0, width, height));
                    draw(frame, &state);
                })
                .unwrap();
            assert_eq!(
                terminal.backend().buffer().content.len(),
                usize::from(width) * usize::from(height)
            );
        }
    }
    #[test]
    #[ignore = "explicit redraw benchmark; timings are evidence, not a CI threshold"]
    fn fragmented_redraw_benchmark() {
        for pieces in [1, 4096] {
            let mut state = state();
            let text = "x".repeat(256 * 1024 / pieces);
            for seq in 1..=pieces {
                delta(&mut state, seq as u64, &text);
            }
            let mut terminal = ratatui::Terminal::new(TestBackend::new(110, 35)).unwrap();
            let start = std::time::Instant::now();
            for _ in 0..10 {
                terminal
                    .draw(|frame| {
                        draw(frame, &state);
                    })
                    .unwrap();
            }
            eprintln!("{pieces} pieces / 10 draws: {:?}", start.elapsed());
        }
    }
}
