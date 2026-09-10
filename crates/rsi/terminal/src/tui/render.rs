//! Borrow resident state without copying its transcript or exporting action authority.
use super::state::State;
#[derive(Clone, Debug, Default)]
pub(super) struct View(pub(super) rsi_terminal_ui::render::View);
impl std::ops::Deref for View {
    type Target = rsi_terminal_ui::render::View;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}
impl View {
    pub(super) fn location(&self, state: &State, row: usize) -> Option<(usize, usize)> {
        self.0
            .location(state.header.session_id(), &state.transcript, row)
    }
    pub(super) fn belongs_to(&self, state: &State) -> bool {
        self.0.belongs_to(state.header.session_id())
    }
}
#[cfg(test)]
pub(super) fn draw(frame: &mut ratatui::Frame<'_>, state: &State) -> View {
    use rsi_terminal_ui::{scene::Scene, wire};
    let area = frame.area();
    let source = Scene::capture(&input(state), area.height)
        .unwrap()
        .encode()
        .unwrap();
    let request = wire::Request {
        identity: wire::Identity {
            attachment: 0,
            presentation: 1,
            revision: 1,
        },
        width: area.width,
        height: area.height,
        bytes: source.len(),
    };
    request.validate().unwrap();
    let (buffer, view) = Scene::decode(&source)
        .unwrap()
        .render(area.width, area.height)
        .unwrap();
    let encoded = wire::encode(request.identity, &buffer, &view).unwrap();
    let (buffer, view) = wire::decode(&encoded, &request).unwrap();
    *frame.buffer_mut() = buffer;
    View(view)
}
pub(super) fn input(state: &State) -> rsi_terminal_ui::Input<'_> {
    rsi_terminal_ui::Input {
        header: &state.header,
        transcript: &state.transcript,
        editor: &state.editor,
        model: state.model.as_ref(),
        enter_submit: state.input_preferences.enter_submit,
        menu: state.menu.as_ref().map(|menu| rsi_terminal_ui::Menu {
            title: &menu.title,
            items: menu.items.iter().map(|(label, _)| label.as_str()).collect(),
            selected: menu.selected,
        }),
        answer: state.answer.as_ref().map(|answer| rsi_terminal_ui::Answer {
            scroll: answer.scroll,
            request: &answer.request,
            answered: answer.answers.len(),
            editor: &answer.editor,
        }),
        ui_edit: state.ui_edit.as_ref().map(|edit| rsi_terminal_ui::Edit {
            label: &edit.label,
            editor: &edit.editor,
        }),
        detail: state.detail.as_deref(),
        detail_offset: state.detail_offset,
        selection: state.selection,
        top: state.top,
        status: &state.status,
        actual_model: state.actual_model.as_deref(),
        active: state.active,
        busy: state.busy,
        remote: state.remote,
        questions: state.questions,
        approvals: state.approvals,
    }
}

/// Resident recovery never depends on the failed presentation's code or source map.
#[derive(Default)]
pub(super) struct Recovery(Option<tokio::time::Instant>);
impl Recovery {
    pub(super) fn ready(&self) -> bool {
        self.0
            .is_none_or(|deadline| tokio::time::Instant::now() >= deadline)
    }
    pub(super) fn failed(&mut self) {
        self.0 = Some(tokio::time::Instant::now() + std::time::Duration::from_millis(250));
    }
    pub(super) fn reset(&mut self) {
        self.0 = None;
    }
}
pub(super) fn failure_frame(
    identity: rsi_terminal_ui::wire::Identity,
    area: ratatui::layout::Rect,
    previous: Option<&super::terminal::RenderedFrame>,
    diagnostic: &str,
) -> super::terminal::RenderedFrame {
    use ratatui::{
        buffer::Buffer,
        layout::Rect,
        widgets::{Paragraph, Widget as _, Wrap},
    };
    let mut buffer = previous
        .filter(|frame| {
            frame.generation == identity.attachment
                && frame.presentation == identity.presentation
                && frame.buffer.area == area
        })
        .map_or_else(|| Buffer::empty(area), |frame| frame.buffer.clone());
    let height = area.height.min(2);
    let message_area = Rect::new(0, area.height - height, area.width, height);
    for y in message_area.y..message_area.bottom() {
        for x in 0..area.width {
            buffer[(x, y)].reset();
        }
    }
    Paragraph::new(format!(
        "Renderer: {} · retrying",
        rsi_terminal_ui::terminal_text(diagnostic)
    ))
    .wrap(Wrap { trim: false })
    .render(message_area, &mut buffer);
    super::terminal::RenderedFrame {
        generation: identity.attachment,
        presentation: identity.presentation,
        revision: identity.revision,
        buffer,
        view: View::default(),
    }
}

#[cfg(test)]
mod recovery_tests {
    use super::*;
    #[test]
    fn failed_replacement_does_not_relabel_cells_from_an_old_presentation() {
        let request = rsi_terminal_ui::wire::Request {
            identity: rsi_terminal_ui::wire::Identity {
                attachment: 1,
                presentation: 2,
                revision: 3,
            },
            width: 80,
            height: 24,
            bytes: 1,
        };
        let mut previous = failure_frame(
            request.identity,
            ratatui::layout::Rect::new(0, 0, request.width, request.height),
            None,
            "old",
        );
        previous.buffer[(0, 0)].set_symbol("Z");
        let retained = failure_frame(
            request.identity,
            ratatui::layout::Rect::new(0, 0, request.width, request.height),
            Some(&previous),
            "same",
        );
        assert_eq!(retained.buffer[(0, 0)].symbol(), "Z");
        previous.presentation = 1;
        let replaced = failure_frame(
            request.identity,
            ratatui::layout::Rect::new(0, 0, request.width, request.height),
            Some(&previous),
            "new",
        );
        assert_eq!(replaced.buffer[(0, 0)].symbol(), " ");
    }
    #[tokio::test(start_paused = true)]
    async fn failure_retries_without_input_but_does_not_spin() {
        let mut recovery = Recovery::default();
        assert!(recovery.ready());
        recovery.failed();
        assert!(!recovery.ready());
        tokio::time::advance(std::time::Duration::from_millis(249)).await;
        assert!(!recovery.ready());
        tokio::time::advance(std::time::Duration::from_millis(1)).await;
        assert!(recovery.ready());
        recovery.failed();
        recovery.reset();
        assert!(recovery.ready());
    }
    #[test]
    fn diagnostic_is_visible_without_a_renderer_and_has_no_source_authority() {
        let request = rsi_terminal_ui::wire::Request {
            identity: rsi_terminal_ui::wire::Identity {
                attachment: 1,
                presentation: 2,
                revision: 3,
            },
            width: 80,
            height: 24,
            bytes: 0,
        };
        let frame = failure_frame(
            request.identity,
            ratatui::layout::Rect::new(0, 0, request.width, request.height),
            None,
            "fixture failed",
        );
        let text: String = frame
            .buffer
            .content
            .iter()
            .map(ratatui::buffer::Cell::symbol)
            .collect();
        assert!(text.contains("Renderer: fixture failed"));
        assert_eq!(frame.generation, 1);
        assert_eq!(frame.revision, 3);
        assert!(frame.view.is_empty());
        assert!(frame.view.hits.is_empty());
    }
}
