//! Shared fullscreen page/dialog geometry. Display data never carries action authority.
use crate::{render, scene::Completion};
use ratatui::{
    Frame,
    layout::Rect,
    style::{Color, Modifier, Style},
    text::Line,
    widgets::{Block, Clear, Paragraph, Wrap},
};
use unicode_width::UnicodeWidthStr as _;

pub(crate) struct Field<'a> {
    pub label: &'a str,
    pub text: &'a str,
    pub cursor: usize,
}
#[derive(Default)]
pub(crate) struct Panel<'a> {
    pub title: &'a str,
    pub explanation: &'a str,
    pub items: Vec<&'a str>,
    pub selected: usize,
    pub field: Option<Field<'a>>,
    pub detail: Option<&'a str>,
    pub offset: usize,
    pub hint: &'a str,
    pub feedback: &'a str,
    pub status: &'a str,
    pub completion: Option<&'a Completion>,
    pub revision: u64,
}

fn bounds(area: Rect) -> Rect {
    let width = if area.width < 42 {
        area.width
    } else {
        area.width.saturating_sub(4).min(96)
    };
    let height = if area.height < 14 {
        area.height
    } else {
        area.height.saturating_sub(4).min(22)
    };
    Rect::new(
        area.x + (area.width - width) / 2,
        area.y + (area.height - height) / 2,
        width,
        height,
    )
}

pub(crate) fn draw_inline(
    frame: &mut Frame<'_>,
    panel: &Panel<'_>,
    prefix: &str,
    view: &mut render::View,
) {
    let Some(editor) = view.editor.filter(|area| area.y >= 2) else {
        return;
    };
    let screen = frame.area();
    let context = if panel.status.is_empty() {
        panel.feedback
    } else {
        panel.status
    };
    let feedback_rows = u16::from(!context.is_empty()).min(editor.y.saturating_sub(2));
    let rows = u16::try_from(panel.items.len().max(1))
        .unwrap_or(8)
        .min(8)
        .min(editor.y - 1 - feedback_rows);
    let top = editor.y - rows - 1 - feedback_rows;
    let band = Rect::new(editor.x, top, editor.width, editor.y - top);
    frame.render_widget(Clear, band);
    frame.render_widget(
        Paragraph::new(render::ellipsis(
            panel.title,
            usize::from(band.width.saturating_sub(2)),
        ))
        .style(render::muted()),
        Rect::new(band.x + 1, top, band.width.saturating_sub(2), 1),
    );
    let body = Rect::new(editor.x + 1, top + 1, editor.width.saturating_sub(2), rows);
    *view = render::View::default();
    view.editor = Some(editor);
    view.dialog = Some(Rect::new(band.x, top, band.width, screen.bottom() - top));
    view.area = body;
    view.choices_area = body;
    view.choice_revision = panel.revision;
    if panel.items.is_empty() {
        frame.render_widget(
            Paragraph::new(if context.is_empty() { "No matches" } else { "" })
                .style(render::muted()),
            body,
        );
    } else {
        choices(frame, panel, body, view);
    }
    if feedback_rows > 0 {
        frame.render_widget(
            Paragraph::new(render::ellipsis(context, usize::from(band.width)))
                .style(render::muted()),
            Rect::new(band.x, editor.y - 1, band.width, 1),
        );
    }
    frame.render_widget(Clear, editor);
    if let Some(field) = &panel.field {
        let text = format!("{prefix}{}", field.text);
        render::composer(frame, editor, &text, prefix.len() + field.cursor, None);
    }
    let hint = if panel.hint.width() <= usize::from(screen.width) {
        panel.hint
    } else {
        "Enter select · Esc back"
    };
    let footer = Rect::new(screen.x, screen.bottom() - 1, screen.width, 1);
    frame.render_widget(Clear, footer);
    frame.render_widget(Paragraph::new(hint).style(render::muted()), footer);
}

fn choices(frame: &mut Frame<'_>, panel: &Panel<'_>, body: Rect, view: &mut render::View) {
    let visible = usize::from(body.height);
    let from = panel
        .selected
        .saturating_sub(visible / 2)
        .min(panel.items.len().saturating_sub(visible));
    for (row, (index, text)) in panel
        .items
        .iter()
        .enumerate()
        .skip(from)
        .take(visible)
        .enumerate()
    {
        let y = body.y + u16::try_from(row).unwrap_or(0);
        frame.render_widget(
            Paragraph::new(format!(
                "{} {}",
                if index == panel.selected { "›" } else { " " },
                render::ellipsis(
                    &crate::terminal_text(text),
                    usize::from(body.width.saturating_sub(2))
                )
            ))
            .style(render::choice_style(index == panel.selected)),
            Rect::new(body.x, y, body.width, 1),
        );
        view.choices.push((y, index));
    }
}

fn dim_background(frame: &mut Frame<'_>, view: &render::View) {
    if let Some(editor) = view.editor {
        for y in editor.y..editor.bottom() {
            for x in editor.x..editor.right() {
                let cell = &mut frame.buffer_mut()[(x, y)];
                if cell.symbol() == "▏" {
                    cell.set_symbol(" ");
                }
            }
        }
    }
    for cell in &mut frame.buffer_mut().content {
        cell.set_style(Style::default().add_modifier(Modifier::DIM));
    }
}

#[allow(clippy::too_many_lines)] // One geometry calculation supplies painting, caret, scroll and hit rectangles.
pub(crate) fn draw(frame: &mut Frame<'_>, panel: &Panel<'_>, modal: bool, view: &mut render::View) {
    let screen = frame.area();
    if screen.width < 28 || screen.height < 9 {
        *view = render::View::default();
        frame.render_widget(Paragraph::new("Resize terminal: at least 28 × 9"), screen);
        return;
    }
    if modal {
        dim_background(frame, view);
    }
    *view = render::View::default();
    view.choice_revision = panel.revision;
    let area = if modal { bounds(screen) } else { screen };
    let inner = if modal {
        view.dialog = Some(area);
        frame.render_widget(Clear, area);
        let title = render::ellipsis(
            &crate::terminal_text(panel.title),
            usize::from(area.width.saturating_sub(4)),
        );
        let block = Block::bordered()
            .border_style(render::border())
            .title(Line::from(title).style(Style::default().add_modifier(Modifier::BOLD)));
        let inner = block.inner(area);
        frame.render_widget(block, area);
        inner
    } else {
        frame.render_widget(
            Paragraph::new(render::ellipsis(panel.title, usize::from(area.width)))
                .style(Style::default().add_modifier(Modifier::BOLD)),
            Rect::new(area.x, area.y, area.width, 1),
        );
        Rect::new(area.x, area.y + 1, area.width, area.height - 1)
    };
    let field_height = if panel.field.is_some() { 3 } else { 0 };
    // Input, one body row and the exit hint win over optional context/feedback.
    let mut spare = inner.height.saturating_sub(field_height + 2);
    let feedback_height = u16::from(!panel.feedback.is_empty()).min(spare);
    spare -= feedback_height;
    let status = Paragraph::new(crate::terminal_text(panel.status))
        .wrap(Wrap { trim: false })
        .style(Style::default().fg(Color::Gray));
    let status_height = if panel.status.is_empty() {
        0
    } else {
        u16::try_from(status.line_count(inner.width).min(2))
            .unwrap_or(2)
            .min(spare)
    };
    spare -= status_height;
    let context = Paragraph::new(crate::terminal_text(panel.explanation))
        .wrap(Wrap { trim: false })
        .style(render::muted());
    let context_height = if panel.explanation.is_empty() {
        0
    } else {
        u16::try_from(context.line_count(inner.width).min(2))
            .unwrap_or(2)
            .min(spare)
    };
    let hint_y = inner.bottom() - 1;
    let input_y = if modal {
        inner.y + context_height
    } else {
        hint_y - field_height
    };
    let feedback_y = if modal {
        hint_y - status_height - feedback_height
    } else {
        input_y - status_height - feedback_height
    };
    let body_y = inner.y + context_height + if modal { field_height } else { 0 };
    let body_bottom = feedback_y;
    let body = Rect::new(
        inner.x,
        body_y,
        inner.width,
        body_bottom.saturating_sub(body_y),
    );
    frame.render_widget(
        context,
        Rect::new(inner.x, inner.y, inner.width, context_height),
    );
    if let Some(field) = &panel.field {
        let editor = Rect::new(inner.x, input_y, inner.width, field_height);
        let label = render::ellipsis(
            &crate::terminal_text(field.label),
            usize::from(inner.width.saturating_sub(2)),
        );
        render::composer(frame, editor, field.text, field.cursor, Some(&label));
        view.editor = Some(editor);
    }
    view.area = body;
    if let Some(text) = panel.detail {
        let text = crate::terminal_text(text);
        let paragraph = Paragraph::new(text).wrap(Wrap { trim: false });
        let maximum = paragraph
            .line_count(body.width)
            .saturating_sub(usize::from(body.height));
        view.scroll_max = maximum;
        frame.render_widget(
            paragraph.scroll((
                u16::try_from(panel.offset.min(maximum)).unwrap_or(u16::MAX),
                0,
            )),
            body,
        );
    } else {
        view.choices_area = body;
        choices(frame, panel, body, view);
    }
    if let Some(completion) = panel.completion {
        if modal {
            let area = Rect::new(body.x, body.y, body.width, body.height.min(8));
            render::completion_in(frame, completion, area, view);
        } else {
            render::completion(frame, completion, input_y, view);
        }
    }
    if feedback_height > 0 {
        frame.render_widget(
            Paragraph::new(render::ellipsis(panel.feedback, usize::from(inner.width)))
                .style(render::muted()),
            Rect::new(inner.x, feedback_y, inner.width, 1),
        );
    }
    frame.render_widget(
        status,
        Rect::new(
            inner.x,
            feedback_y + feedback_height,
            inner.width,
            status_height,
        ),
    );
    // Keep Escape discoverable even where long operation-specific hints cannot fit.
    let hint = if panel.hint.width() <= usize::from(inner.width) {
        panel.hint
    } else if panel.field.is_some() || !panel.items.is_empty() {
        "Enter select · Esc back"
    } else {
        "↑/↓ scroll · Esc back"
    };
    frame.render_widget(
        Paragraph::new(render::ellipsis(hint, usize::from(inner.width))).style(render::muted()),
        Rect::new(inner.x, hint_y, inner.width, 1),
    );
}
