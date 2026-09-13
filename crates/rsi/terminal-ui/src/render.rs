//! Bounded visible rows with source positions; wrapping and editing share graphemes.
#[path = "layout.rs"]
mod layout;
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
    widgets::{Block, Borders, Clear, Paragraph},
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
}

#[derive(Clone, Debug, Default, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct View {
    rows: Vec<Row>,
    session: Option<rsi_agent_session_protocol::SessionId>,
    anchors: Vec<Option<Anchor>>,
    pub area: Rect,
    pub hits: Vec<(u16, u16, u16, Anchor, Anchor)>,
}

impl View {
    pub fn validate(&self, width: u16, height: u16) -> Result<(), &'static str> {
        if self.rows.len() > usize::from(height) * 2
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
                && self
                    .anchors
                    .iter()
                    .flatten()
                    .all(|a| transcript.locate(*a).is_some())
                && self.hits.iter().all(|(_, _, _, a, b)| {
                    transcript.locate(*a).is_some() && transcript.locate(*b).is_some()
                }))
    }

    pub fn is_empty(&self) -> bool {
        self.rows.is_empty()
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
    pub fn hit(&self, x: u16, y: u16, end: bool) -> Option<Anchor> {
        self.hits
            .iter()
            .find(|(row, start, width, ..)| {
                *row == y && x >= *start && x < start.saturating_add(*width)
            })
            .map(|(_, _, _, a, b)| if end { *b } else { *a })
    }
}

fn each_row(text: &str, width: usize, mut emit: impl FnMut(usize, usize) -> bool) {
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
fn rows(
    transcript: &Transcript,
    cache: &mut LayoutCache,
    top: Option<Anchor>,
    width: u16,
    height: u16,
) -> Vec<Row> {
    let limit = usize::from(height).max(1) * 2;
    let mut rows = VecDeque::with_capacity(limit);
    let location = top.and_then(|anchor| transcript.locate(anchor));
    let start_block = location.map_or(0, |(block, _)| block);
    for (index, block) in transcript.blocks.iter().enumerate().skip(start_block) {
        if location.is_none_or(|(i, offset)| i != index || offset == 0) {
            if rows.len() == limit {
                if top.is_some() {
                    break;
                }
                rows.pop_front();
            }
            rows.push_back(Row {
                block: index,
                offset: 0,
                end: 0,
                title: true,
            });
        }
        let layout = cache.get(block, width);
        let from = location
            .filter(|(i, _)| *i == index)
            .map_or(0, |(_, from)| layout.first_row(from));
        let end = if block.collapsed {
            (from + 2).min(layout.rows().len())
        } else {
            layout.rows().len()
        };
        let from = if top.is_none() {
            from.max(end.saturating_sub(limit))
        } else {
            from
        };
        for (offset, end) in layout.rows().skip(from).take(end - from) {
            if rows.len() == limit {
                if top.is_some() {
                    break;
                }
                rows.pop_front();
            }
            rows.push_back(Row {
                block: index,
                offset,
                end,
                title: false,
            });
            if top.is_some() && rows.len() >= limit {
                break;
            }
        }
        if top.is_some() && rows.len() >= limit {
            break;
        }
        while rows.len() > limit {
            rows.pop_front();
        }
    }
    if top.is_none() {
        while rows.len() > usize::from(height) {
            rows.pop_front();
        }
    }
    rows.into_iter().collect()
}

#[allow(clippy::too_many_lines)] // Layout and source hit regions are computed from the same complete frame.
/// # Panics
/// Panics if borrowed editor or transcript values violate their in-process invariants.
pub fn draw(frame: &mut Frame<'_>, state: &State<'_>, cache: &mut LayoutCache) -> View {
    let area = frame.area();
    frame.render_widget(Block::new().style(Style::default().bg(Color::Reset)), area);
    if area.width < 28 || area.height < 9 {
        frame.render_widget(Paragraph::new("Resize terminal: at least 28 × 9"), area);
        return View::default();
    }
    let draft = state.ui_edit.as_ref().map_or_else(
        || {
            state
                .answer
                .as_ref()
                .map_or(state.editor, |answer| answer.editor)
        },
        |edit| edit.editor,
    );
    let editor_height = u16::try_from(
        draft
            .text
            .lines()
            .count()
            .clamp(1, usize::from(area.height.saturating_sub(9)).clamp(1, 6)),
    )
    .unwrap_or(1)
        + 2;
    let body = Rect::new(
        1,
        3,
        area.width - 2,
        area.height.saturating_sub(6 + editor_height),
    );
    let model = state
        .model
        .unwrap_or(state.header.settings().default_model());
    let mode = if state.active { "RUNNING" } else { "READY" };
    let lifecycle = if state.remote {
        "exit detaches"
    } else {
        "exit interrupts"
    };
    let header = format!(
        " RSI · {mode} · {lifecycle} · {}",
        state.header.session_id()
    );
    frame.render_widget(
        Paragraph::new(crate::terminal_text(&header)).style(
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ),
        Rect::new(1, 0, area.width - 2, 1),
    );
    let location = format!(
        " {}  ·  next {}/{}",
        state.header.canonical_cwd(),
        model.deployment(),
        model.model()
    );
    frame.render_widget(
        Paragraph::new(crate::terminal_text(&location)).style(Style::default().fg(Color::DarkGray)),
        Rect::new(0, 1, area.width, 1),
    );
    let work = format!(
        " {}{}  ·  {} questions  ·  {} approvals  ·  {}",
        state.actual_model.unwrap_or("No model call yet"),
        if state.busy {
            "  [request pending]"
        } else {
            ""
        },
        state.questions,
        state.approvals,
        if state.remote {
            "daemon · exit detaches"
        } else {
            "embedded · exit interrupts"
        }
    );
    frame.render_widget(
        Paragraph::new(crate::terminal_text(&work)).style(Style::default().fg(Color::DarkGray)),
        Rect::new(0, 2, area.width, 1),
    );
    cache.retain(state.transcript);
    let rows = rows(
        state.transcript,
        cache,
        state.top,
        body.width.saturating_sub(2),
        body.height,
    );
    let mut cached = None;
    let anchors = rows
        .iter()
        .map(|row| {
            if cached.as_ref().is_none_or(|(index, _)| *index != row.block) {
                cached = Some((row.block, state.transcript.blocks[row.block].anchor_index()));
            }
            cached
                .as_ref()
                .expect("cached source index")
                .1
                .anchor(row.offset)
        })
        .collect();
    let mut view = View {
        session: Some(state.header.session_id().clone()),
        anchors,
        rows,
        area: body,
        hits: Vec::new(),
    };
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
        let color = match block.role {
            Role::User => Color::Cyan,
            Role::Assistant => Color::Reset,
            Role::Reasoning | Role::Status => Color::DarkGray,
            Role::Tool => Color::Yellow,
        };
        if row.title {
            let label = format!(
                "{} {}{}",
                if block.collapsed { "▸" } else { "▾" },
                block.title,
                if block.discarded || block.pieces.iter().any(|piece| piece.omitted) {
                    "  [window; Actions → detail]"
                } else {
                    ""
                }
            );
            frame.render_widget(
                Paragraph::new(label)
                    .style(Style::default().fg(color).add_modifier(Modifier::BOLD)),
                Rect::new(body.x, y, body.width, 1),
            );
            continue;
        }
        if text_cache
            .as_ref()
            .is_none_or(|(index, ..)| *index != row.block)
        {
            text_cache = Some((
                row.block,
                cache.get(block, body.width.saturating_sub(2)),
                block.anchor_index(),
            ));
        }
        let layout = &text_cache.as_ref().expect("cached block").1;
        let text = &layout.text;
        let mut x = body.x + 2;
        for (offset, grapheme) in text[row.offset..row.end].grapheme_indices(true) {
            let offset = row.offset + offset;
            let width = if grapheme == "\t" {
                4 - usize::from(x - body.x - 2) % 4
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
            let a = anchors.anchor(offset).expect("projected text has a source");
            let b = anchors
                .anchor(offset + grapheme.len())
                .expect("projected text has a source");
            view.hits.push((y, x, width, a, b));
            let highlight =
                selected.is_some_and(|(a, b)| (row.block, offset) >= a && (row.block, offset) < b);
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
            let display = if grapheme == "\t" {
                " ".repeat(usize::from(width))
            } else {
                grapheme.to_owned()
            };
            frame.render_widget(
                Paragraph::new(display).style(style),
                Rect::new(x, y, width, 1),
            );
            x += width;
        }
    }
    if state.transcript.blocks.is_empty() {
        let keys = if state.enter_submit {
            "Enter submits · Ctrl+J adds a line · Ctrl+P opens actions"
        } else {
            "Enter adds a line · Ctrl+S submits · Ctrl+P opens actions"
        };
        frame.render_widget(
            Paragraph::new(format!(
                "Describe a change, investigate a failure, or continue a session.\n\n{keys}"
            ))
            .style(Style::default().fg(Color::DarkGray)),
            body,
        );
    }
    let edit_area = Rect::new(
        0,
        area.height - editor_height - 3,
        area.width,
        editor_height,
    );
    let title = state.ui_edit.as_ref().map_or_else(
        || {
            state.answer.as_ref().map_or_else(
                || {
                    if state.enter_submit {
                        " Input · Enter send · Ctrl+S send · Ctrl+O steer ".into()
                    } else {
                        " Input · Enter line · Ctrl+S send · Ctrl+O steer ".into()
                    }
                },
                |answer| {
                    format!(
                        " Answer {}/{} · Enter accepts option number or text ",
                        answer.answered.saturating_add(1),
                        answer.request.questions.len()
                    )
                },
            )
        },
        |edit| format!(" {} · Enter accepts · Esc discards ", edit.label),
    );
    let mut editor_text = crate::terminal_text(&draft.text[..draft.cursor]);
    editor_text.push('▏');
    editor_text.push_str(&crate::terminal_text(&draft.text[draft.cursor..]));
    let cursor = crate::terminal_text(&draft.text[..draft.cursor]).len();
    let visible = editor_rows(
        &editor_text,
        cursor,
        usize::from(edit_area.width - 2),
        usize::from(editor_height - 2),
    );
    frame.render_widget(
        Paragraph::new(visible).block(
            Block::new()
                .borders(Borders::ALL)
                .title(title)
                .border_style(Style::default().fg(Color::Cyan)),
        ),
        edit_area,
    );
    frame.render_widget(
        Paragraph::new(state.status).style(Style::default().fg(Color::Yellow)),
        Rect::new(1, area.height - 3, area.width - 2, 1),
    );
    frame.render_widget(
        Paragraph::new(Line::from(vec![Span::raw(if area.width < 70 {
            " ^P actions  ^C cancel  ^Y copy  Esc close"
        } else {
            " ^P actions   ^C cancel   ^Y copy   ^Z undo   PgUp history   Tab card   Esc close"
        })]))
        .style(Style::default().fg(Color::DarkGray)),
        Rect::new(0, area.height - 2, area.width, 1),
    );
    if state.top.is_some() {
        frame.render_widget(
            Paragraph::new(" Browsing history · End follows new output"),
            Rect::new(0, area.height - 1, area.width, 1),
        );
    }
    if let Some(detail) = &state.detail {
        popup(
            frame,
            Rect {
                height: area.height.saturating_sub(if state.ui_edit.is_some() {
                    editor_height
                } else {
                    0
                }),
                ..area
            },
            "Detail · ↑/↓ scroll · Esc close",
            detail,
            state.detail_offset,
            true,
        );
    }
    if let Some(answer) = &state.answer
        && let Some(question) = answer.request.questions.get(answer.answered)
    {
        let text = format!(
            "{}\n\n{}\n\nType an option number or a free-text answer below.",
            question.prompt,
            question
                .options
                .iter()
                .enumerate()
                .map(|(i, s)| format!("{}. {s}", i + 1))
                .collect::<Vec<_>>()
                .join("\n")
        );
        let height = body.height;
        let text = crate::terminal_text(&text);
        let mut lines = Vec::new();
        let mut line = 0;
        each_row(
            &text,
            usize::from(body.width.saturating_sub(2)).max(1),
            |start, end| {
                if line >= answer.scroll {
                    lines.push(&text[start..end]);
                }
                line += 1;
                lines.len() < usize::from(height.saturating_sub(2))
            },
        );
        frame.render_widget(Clear, Rect::new(body.x, body.y, body.width, height));
        frame.render_widget(
            Paragraph::new(lines.join("\n"))
                .block(Block::bordered().title("Live question · PgUp/PgDn scroll")),
            Rect::new(body.x, body.y, body.width, height),
        );
    }
    if let Some(menu) = &state.menu {
        let height = usize::from(area.height.saturating_sub(6)).max(1);
        let from = menu.selected.saturating_sub(height / 2);
        let text = menu
            .items
            .iter()
            .enumerate()
            .skip(from)
            .take(height)
            .map(|(index, label)| {
                format!(
                    "{} {}",
                    if index == menu.selected { "›" } else { " " },
                    label
                )
            })
            .collect::<Vec<_>>()
            .join("\n");
        popup(
            frame,
            area,
            &format!("{} · Enter select · Esc close", menu.title),
            &text,
            0,
            false,
        );
    }
    view
}

fn editor_rows(text: &str, cursor: usize, width: usize, height: usize) -> String {
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

fn markdown_styles(text: &str) -> MarkdownStyles {
    use pulldown_cmark::{Event, Parser, Tag};
    Parser::new(text)
        .into_offset_iter()
        .filter_map(|(event, range)| {
            let style = match event {
                Event::Start(Tag::CodeBlock(_)) => {
                    Style::default().bg(Color::Rgb(30, 34, 41)).fg(Color::Gray)
                }
                Event::Start(Tag::Heading { .. }) => Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
                Event::Code(_) => Style::default().fg(Color::Cyan),
                _ => return None,
            };
            Some((range, style))
        })
        .take(1024)
        .collect()
}

fn popup(frame: &mut Frame<'_>, area: Rect, title: &str, text: &str, offset: usize, wrap: bool) {
    frame.render_widget(
        Clear,
        Rect::new(0, 3, area.width, area.height.saturating_sub(6)),
    );
    let popup = Rect::new(
        2,
        3,
        area.width.saturating_sub(4),
        area.height.saturating_sub(6),
    );
    let text = crate::terminal_text(text);
    let mut lines = Vec::with_capacity(usize::from(popup.height.saturating_sub(2)));
    let mut line = 0;
    each_row(
        &text,
        if wrap {
            usize::from(popup.width.saturating_sub(2)).max(1)
        } else {
            usize::MAX
        },
        |start, end| {
            if line >= offset {
                lines.push(&text[start..end]);
            }
            line += 1;
            lines.len() < usize::from(popup.height.saturating_sub(2))
        },
    );
    frame.render_widget(
        Paragraph::new(lines.join("\n")).block(
            Block::bordered()
                .title(crate::terminal_text(title))
                .border_style(Style::default().fg(Color::Cyan)),
        ),
        popup,
    );
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

    #[test]
    fn cached_layout_reuses_unchanged_bodies_and_isolates_one_changed_block() {
        let mut state = state();
        state.transcript.apply(
            &SessionFact::new(
                1,
                1,
                SessionFactBody::TurnAccepted {
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
    fn collapsed_anchor_beyond_initial_rows_keeps_current_source_mapping() {
        let mut state = state();
        delta(&mut state, 1, "one\ntwo\nthree\nfour\nfive");
        let block = &mut state.transcript.blocks[0];
        block.collapsed = true;
        state.top = block.anchor(8);
        let mut cache = LayoutCache::default();
        let visible = rows(&state.transcript, &mut cache, state.top, 30, 5);
        assert_eq!(
            visible
                .iter()
                .map(|row| (row.offset, row.end))
                .collect::<Vec<_>>(),
            vec![(8, 13), (14, 18)]
        );
        let before = cache.builds;
        delta(&mut state, 2, "\nnext");
        let visible = rows(&state.transcript, &mut cache, state.top, 30, 5);
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
        state.active = true;
        state.questions = 1;
        state.approvals = 1;
        state.actual_model = Some("actual deepseek/deepseek-chat".into());
        delta(
            &mut state,
            1,
            "I found the UTF-8 boundary bug in src/lib.rs.\n\nThe fix walks backward to a valid character boundary:\n\n```rust\nwhile !input.is_char_boundary(end) {\n    end -= 1;\n}\n```\n\n中文、combining e\u{301}, and 👩🏽‍💻 stay intact.\nTests cover empty input, byte limits, and usize::MAX.",
        );
        state
            .editor
            .insert("补充：请保留已有更改。\nRun the focused tests after the patch.")
            .unwrap();
        state.notice("Command completed · exit code 0 · durable Tool result recorded");
        for (width, height) in [(110, 35), (80, 24), (42, 12)] {
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
            assert!(buffer.content.iter().any(|cell| cell.symbol() == "R"));
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
