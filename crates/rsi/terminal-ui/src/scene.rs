//! Bounded display sources shared by linked and portable renderers.
use crate::{
    Answer, Edit, Input, Menu,
    editor::Editor,
    render,
    transcript::{Anchor, Viewport},
};
use rsi_agent_session_protocol::SessionHeader;
use rsi_ai_protocol::ModelRef;
use rsi_user_questions_protocol::QuestionRequest;
use serde::{Deserialize, Serialize};
use std::cell::RefCell;
use unicode_segmentation::UnicodeSegmentation as _;

pub const MAXIMUM_SCENE_BYTES: usize = 16 * 1024 * 1024;
const DRAFT_WINDOW: usize = 64 * 1024;
#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
/// Bounded editor display window, with a cursor relative to its UTF-8 text.
pub struct Draft {
    pub text: String,
    pub cursor: usize,
}
impl Draft {
    pub fn capture(editor: &Editor) -> Result<Self, &'static str> {
        let requested = editor.cursor.saturating_sub(DRAFT_WINDOW / 2);
        let start = editor
            .text
            .grapheme_indices(true)
            .map(|(offset, _)| offset)
            .chain(std::iter::once(editor.text.len()))
            .find(|offset| *offset >= requested)
            .unwrap_or(editor.cursor);
        let end = editor.text[start..]
            .grapheme_indices(true)
            .map(|(offset, _)| start + offset)
            .chain(std::iter::once(editor.text.len()))
            .take_while(|offset| *offset <= start + DRAFT_WINDOW)
            .last()
            .unwrap_or(start);
        if start == end && !editor.text.is_empty() {
            return Err("draft grapheme exceeds display window");
        }
        let cursor = editor
            .cursor
            .checked_sub(start)
            .filter(|cursor| *cursor <= end - start)
            .ok_or("draft grapheme exceeds display window")?;
        Ok(Self {
            text: editor.text[start..end].into(),
            cursor,
        })
    }
    fn restore(self) -> Result<Editor, &'static str> {
        if self.text.len() > DRAFT_WINDOW || !self.text.is_char_boundary(self.cursor) {
            return Err("invalid draft window");
        }
        let mut editor = Editor::with_text(self.text, DRAFT_WINDOW);
        editor.cursor = self.cursor;
        Ok(editor)
    }
}
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct OwnedMenu {
    title: String,
    items: Vec<String>,
    selected: usize,
}
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct OwnedAnswer {
    scroll: usize,
    request: QuestionRequest,
    answered: usize,
    editor: Draft,
}
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct OwnedEdit {
    label: String,
    editor: Draft,
}
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
#[allow(clippy::struct_excessive_bools)] // Mirrors independent display facts.
pub struct SessionScene {
    activity: Option<crate::Activity>,
    header: SessionHeader,
    workspace_label: String,
    transcript: Viewport,
    editor: Draft,
    model: Option<ModelRef>,
    reasoning_effort: Option<rsi_ai_protocol::ReasoningEffortId>,
    menu_revision: u64,
    todos: Option<rsi_agent_todo::TodoList>,
    metrics: Option<rsi_conversation::SessionMetrics>,
    metrics_complete: bool,
    model_unavailable: bool,
    completion: Option<Completion>,
    menu: Option<OwnedMenu>,
    answer: Option<OwnedAnswer>,
    ui_edit: Option<OwnedEdit>,
    detail: Option<String>,
    detail_offset: usize,
    selection: Option<(Anchor, Anchor)>,
    top: Option<Anchor>,
    fold_focus: Option<(Anchor, u16)>,
    status: String,
    actual_model: Option<String>,
    active: bool,
    busy: bool,
    remote: bool,
    questions: usize,
    approvals: usize,
}
impl SessionScene {
    pub fn capture(input: &Input<'_>, width: u16, height: u16) -> Result<Self, &'static str> {
        Self::capture_cached(
            input,
            width,
            height,
            &mut crate::transcript::FoldCache::default(),
        )
    }
    fn capture_cached(
        input: &Input<'_>,
        width: u16,
        height: u16,
        cache: &mut crate::transcript::FoldCache,
    ) -> Result<Self, &'static str> {
        let (transcript, top) = Viewport::capture_cached(
            input.transcript,
            input.top,
            width,
            height,
            input.activity.as_ref(),
            input.fold_focus.map(|(anchor, _)| anchor),
            cache,
        );
        let scene = Self {
            activity: input.activity.clone(),
            fold_focus: input.fold_focus,
            header: input.header.clone(),
            workspace_label: input.workspace_label.into(),
            transcript,
            editor: Draft::capture(input.editor)?,
            model: input.model.cloned(),
            reasoning_effort: input.reasoning_effort.cloned(),
            menu_revision: input.menu_revision,
            todos: input.todos.cloned(),
            metrics: input.metrics.cloned(),
            metrics_complete: input.metrics_complete,
            model_unavailable: input.model_unavailable,
            completion: input.completion.cloned(),
            menu: input.menu.as_ref().map(|menu| OwnedMenu {
                title: menu.title.into(),
                items: menu.items.iter().map(|label| (*label).into()).collect(),
                selected: menu.selected,
            }),
            answer: input
                .answer
                .as_ref()
                .map(|answer| {
                    Ok::<_, &'static str>(OwnedAnswer {
                        scroll: answer.scroll,
                        request: answer.request.clone(),
                        answered: answer.answered,
                        editor: Draft::capture(answer.editor)?,
                    })
                })
                .transpose()?,
            ui_edit: input
                .ui_edit
                .as_ref()
                .map(|edit| {
                    Ok::<_, &'static str>(OwnedEdit {
                        label: edit.label.into(),
                        editor: Draft::capture(edit.editor)?,
                    })
                })
                .transpose()?,
            detail: input.detail.map(str::to_owned),
            detail_offset: input.detail_offset,
            selection: input.selection,
            top,
            status: input.status.into(),
            actual_model: input.actual_model.map(str::to_owned),
            active: input.active,
            busy: input.busy,
            remote: input.remote,
            questions: input.questions,
            approvals: input.approvals,
        };
        scene.validate()?;
        Ok(scene)
    }
    fn validate(&self) -> Result<(), &'static str> {
        if self.fold_focus.is_some_and(|(anchor, row)| {
            anchor.source.seq == 0 || anchor.offset > crate::transcript::MAX_TEXT || row >= 256
        }) || self
            .completion
            .as_ref()
            .is_some_and(|popup| popup.validate().is_err())
            || self
                .answer
                .as_ref()
                .is_some_and(|answer| answer.answered >= answer.request.questions.len())
            || self
                .metrics
                .as_ref()
                .is_some_and(|metrics| metrics.validate().is_err())
            || self.workspace_label.len() > 8192
            || self.status.len() > 8192
            || self.actual_model.as_ref().is_some_and(|s| s.len() > 8192)
            || self.detail.as_ref().is_some_and(|s| s.len() > 1024 * 1024)
            || self.menu.as_ref().is_some_and(|m| {
                m.items.len() > 1024
                    || m.title.len() > 8192
                    || m.items.iter().map(String::len).sum::<usize>() > 256 * 1024
                    || (!m.items.is_empty() && m.selected >= m.items.len())
            })
            || self.ui_edit.as_ref().is_some_and(|e| e.label.len() > 8192)
        {
            return Err("scene display bounds");
        }
        Ok(())
    }
}
/// One fullscreen base and one non-recursive focused dialog.
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Scene {
    pub surface: Surface,
    dialog: Option<Box<ApplicationScene>>,
}

/// Complete application display; an unattached view carries no invented Session identity.
#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Surface {
    Session(Box<SessionScene>),
    Application(Box<ApplicationScene>),
}
/// Labels only: the resident controller retains command identity and authority.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Completion {
    pub revision: u64,
    pub items: Vec<(String, String)>,
    pub selected: usize,
}
impl Completion {
    fn validate(&self) -> Result<(), &'static str> {
        if self.items.len() > 1024
            || self
                .items
                .iter()
                .any(|(label, description)| label.len() > 512 || description.len() > 256)
            || (!self.items.is_empty() && self.selected >= self.items.len())
        {
            return Err("completion bounds");
        }
        Ok(())
    }
}
/// Redacted application-owned screen. Secrets supply a fixed mask only.
#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ApplicationScene {
    pub composer_prefix: Option<String>,
    pub revision: u64,
    pub detail: Option<String>,
    pub detail_offset: usize,
    pub title: String,
    pub explanation: String,
    pub items: Vec<String>,
    pub selected: usize,
    pub input: Draft,
    pub field: Option<String>,
    pub hint: String,
    pub progress: Option<String>,
    pub receipts: Vec<String>,
    pub completion: Option<Completion>,
    pub status: String,
}
impl From<ApplicationScene> for Scene {
    fn from(scene: ApplicationScene) -> Self {
        Self {
            surface: Surface::Application(Box::new(scene)),
            dialog: None,
        }
    }
}
impl ApplicationScene {
    fn validate(&self) -> Result<(), &'static str> {
        if self.composer_prefix.as_ref().is_some_and(|prefix| {
            prefix.len() > 8192
                || self.field.is_none()
                || self.detail.is_some()
                || self.completion.is_some()
        }) || self.detail.as_ref().is_some_and(|text| text.len() > 8192)
            || self.title.len() > 8192
            || self.explanation.len() > 8192
            || self.status.len() > 8192
            || self.field.as_ref().is_some_and(|s| s.len() > 512)
            || self.hint.len() > 512
            || self.progress.as_ref().is_some_and(|s| s.len() > 8192)
            || self.receipts.len() > 8
            || self.receipts.iter().any(|s| s.len() > 8192)
            || self
                .completion
                .as_ref()
                .is_some_and(|p| p.validate().is_err())
            || self.input.text.len() > DRAFT_WINDOW
            || !self.input.text.is_char_boundary(self.input.cursor)
            || self.items.len() > 8192
            || self.items.iter().map(String::len).sum::<usize>() > 2 * 1024 * 1024
            || (!self.items.is_empty() && self.selected >= self.items.len())
        {
            return Err("application scene bounds");
        }
        Ok(())
    }
    fn panel(&self) -> crate::dialog::Panel<'_> {
        crate::dialog::Panel {
            title: &self.title,
            explanation: &self.explanation,
            items: self.items.iter().map(String::as_str).collect(),
            selected: self.selected,
            field: self.field.as_deref().map(|label| crate::dialog::Field {
                label,
                text: &self.input.text,
                cursor: self.input.cursor,
            }),
            detail: self.detail.as_deref(),
            offset: self.detail_offset,
            hint: &self.hint,
            status: &self.status,
            feedback: self
                .progress
                .as_deref()
                .or_else(|| self.receipts.last().map(String::as_str))
                .unwrap_or_default(),
            completion: self.completion.as_ref(),
            revision: self.revision,
        }
    }
    fn render(
        self,
        width: u16,
        height: u16,
    ) -> Result<(ratatui::buffer::Buffer, render::View), &'static str> {
        crate::wire::dimensions(width, height)?;
        self.validate()?;
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(width, height))
                .map_err(|_| "application renderer")?;
        let mut view = render::View::default();
        terminal
            .draw(|frame| crate::dialog::draw(frame, &self.panel(), false, &mut view))
            .map_err(|_| "application renderer")?;
        Ok((terminal.backend().buffer().clone(), view))
    }
}
/// One controller's bounded cache for unchanged folded source windows.
#[derive(Debug, Default)]
pub struct SceneCapture {
    folds: crate::transcript::FoldCache,
}
impl SceneCapture {
    /// Captures the current frame without rewrapping unchanged process bodies.
    pub fn capture(
        &mut self,
        input: &Input<'_>,
        width: u16,
        height: u16,
    ) -> Result<Scene, &'static str> {
        SessionScene::capture_cached(input, width, height, &mut self.folds).map(|scene| Scene {
            surface: Surface::Session(Box::new(scene)),
            dialog: None,
        })
    }
}
impl Scene {
    pub fn with_dialog(mut self, dialog: Self) -> Result<Self, &'static str> {
        let Surface::Application(content) = dialog.surface else {
            return Err("dialog must be an application surface");
        };
        if dialog.dialog.is_some() {
            return Err("nested dialog scene");
        }
        self.dialog = Some(content);
        Ok(self)
    }
    pub fn capture(input: &Input<'_>, width: u16, height: u16) -> Result<Self, &'static str> {
        SessionScene::capture(input, width, height).map(|scene| Self {
            surface: Surface::Session(Box::new(scene)),
            dialog: None,
        })
    }
    fn validate(&self) -> Result<(), &'static str> {
        if let Some(dialog) = &self.dialog {
            dialog.validate()?;
        }
        match &self.surface {
            Surface::Session(scene) => scene.validate(),
            Surface::Application(scene) => scene.validate(),
        }
    }
    pub fn encode(&self) -> Result<Vec<u8>, &'static str> {
        crate::wire::json(self, MAXIMUM_SCENE_BYTES)
    }
    pub fn decode(bytes: &[u8]) -> Result<Self, &'static str> {
        if bytes.len() > MAXIMUM_SCENE_BYTES {
            return Err("scene source bound");
        }
        let scene: Self = serde_json::from_slice(bytes).map_err(|_| "invalid scene source")?;
        scene.validate()?;
        Ok(scene)
    }
    pub fn render(
        self,
        width: u16,
        height: u16,
    ) -> Result<(ratatui::buffer::Buffer, render::View), &'static str> {
        Renderer::default().render(self, width, height)
    }
}
/// One presentation generation's private layout cache.
#[derive(Debug, Default)]
pub struct Renderer {
    layout: RefCell<render::LayoutCache>,
    previous: crate::transcript::Transcript,
}
impl Renderer {
    pub fn render(
        &mut self,
        scene: Scene,
        width: u16,
        height: u16,
    ) -> Result<(ratatui::buffer::Buffer, render::View), &'static str> {
        scene.validate()?;
        let (buffer, mut view) = self.render_surface(scene.surface, width, height)?;
        let Some(dialog) = scene.dialog else {
            return Ok((buffer, view));
        };
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(width, height))
                .map_err(|_| "dialog renderer")?;
        terminal
            .draw(|frame| {
                *frame.buffer_mut() = buffer.clone();
                if let Some(prefix) = &dialog.composer_prefix {
                    crate::dialog::draw_inline(frame, &dialog.panel(), prefix, &mut view);
                } else {
                    crate::dialog::draw(frame, &dialog.panel(), true, &mut view);
                }
            })
            .map_err(|_| "dialog renderer")?;
        Ok((terminal.backend().buffer().clone(), view))
    }
    #[allow(clippy::too_many_lines)] // Restores the validated scene fields and publishes one matched buffer/view.
    fn render_surface(
        &mut self,
        surface: Surface,
        width: u16,
        height: u16,
    ) -> Result<(ratatui::buffer::Buffer, render::View), &'static str> {
        let scene = match surface {
            Surface::Application(scene) => return scene.render(width, height),
            Surface::Session(scene) => *scene,
        };
        let SessionScene {
            activity,
            header,
            workspace_label,
            transcript,
            editor,
            model,
            reasoning_effort,
            menu_revision,
            todos,
            metrics,
            metrics_complete,
            model_unavailable,
            completion,
            menu,
            answer,
            ui_edit,
            detail,
            detail_offset,
            selection,
            top,
            fold_focus,
            status,
            actual_model,
            active,
            busy,
            remote,
            questions,
            approvals,
        } = scene;
        crate::wire::dimensions(width, height)?;
        transcript.validate_width(width)?;
        let mut transcript = transcript.restore()?;
        transcript.reuse_layout_revisions(&self.previous);
        let editor = editor.restore()?;
        let answer = match answer {
            Some(answer) => Some((
                answer.scroll,
                answer.request,
                answer.answered,
                answer.editor.restore()?,
            )),
            None => None,
        };
        let edit = match ui_edit {
            Some(edit) => Some((edit.label, edit.editor.restore()?)),
            None => None,
        };

        let input = Input {
            fold_focus,
            activity,
            header: &header,
            workspace_label: &workspace_label,
            transcript: &transcript,
            editor: &editor,
            model: model.as_ref(),
            reasoning_effort: reasoning_effort.as_ref(),
            menu_revision,
            todos: todos.as_ref(),
            metrics: metrics.as_ref(),
            metrics_complete,
            model_unavailable,
            completion: completion.as_ref(),
            menu: menu.as_ref().map(|m| Menu {
                title: &m.title,
                items: m.items.iter().map(String::as_str).collect(),
                selected: m.selected,
            }),
            answer: answer
                .as_ref()
                .map(|(scroll, request, answered, editor)| Answer {
                    scroll: *scroll,
                    request,
                    answered: *answered,
                    editor,
                }),
            ui_edit: edit.as_ref().map(|(label, editor)| Edit { label, editor }),
            detail: detail.as_deref(),
            detail_offset,
            selection,
            top,
            status: &status,
            actual_model: actual_model.as_deref(),
            active,
            busy,
            remote,
            questions,
            approvals,
        };
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(width, height))
                .map_err(|_| "render backend")?;
        let mut view = render::View::default();
        terminal
            .draw(|frame| view = render::draw(frame, &input, &mut self.layout.borrow_mut()))
            .map_err(|_| "render backend")?;
        self.previous = transcript;
        Ok((terminal.backend().buffer().clone(), view))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_state::{TestState, input};
    use rsi_agent_session_protocol::{
        AgentPresetId, FrozenAgentSettings, SessionFact, SessionFactBody, SessionId, TurnId,
    };
    fn state() -> TestState {
        TestState::new(
            SessionHeader::new(
                SessionId::new("scene").unwrap(),
                1,
                "/workspace",
                AgentPresetId::new("default").unwrap(),
                FrozenAgentSettings::new(
                    "default",
                    "system",
                    ModelRef::new("fixture", "text").unwrap(),
                    rsi_sandbox::SandboxMode::WorkspaceWrite,
                    false,
                )
                .unwrap(),
            )
            .unwrap(),
            false,
        )
    }
    fn add(state: &mut TestState, seq: u64, text: &str) {
        state.transcript.apply(
            &SessionFact::new(
                seq,
                seq,
                SessionFactBody::TurnAccepted {
                    reasoning_effort: None,
                    turn_id: TurnId::new(format!("turn-{seq}")).unwrap(),
                    text: text.into(),
                    model: None,
                    sandbox: rsi_sandbox::SandboxMode::WorkspaceWrite,
                    require_approval: false,
                },
            )
            .unwrap(),
        );
    }
    #[test]
    #[allow(clippy::too_many_lines)] // The same original sources are compared at all four geometries and fold states.
    fn folded_scene_preserves_large_process_head_tail_count_and_hidden_anchor() {
        let mut state = state();
        for seq in 1..=80 {
            let mut text = String::new();
            for i in 0..400 {
                std::fmt::Write::write_fmt(
                    &mut text,
                    format_args!("source {seq:02}-{i:03} 中文 e\u{301} literal data\n"),
                )
                .unwrap();
            }
            state.transcript.apply(
                &SessionFact::new(
                    seq,
                    seq,
                    SessionFactBody::ModelEvent {
                        turn_id: TurnId::new("reasoning-turn").unwrap(),
                        effect_id: rsi_agent_session_protocol::EffectId::new(format!(
                            "reasoning-effect-{}",
                            (seq - 1) / 8
                        ))
                        .unwrap(),
                        purpose: rsi_agent_session_protocol::ModelEventPurpose::Conversation,
                        event: rsi_ai_protocol::LanguageEvent::ContentDelta {
                            index: 0,
                            delta: rsi_ai_protocol::ContentDelta::Reasoning(text),
                        },
                    },
                )
                .unwrap(),
            );
        }
        let block = &state.transcript.blocks[0];
        assert!(
            state
                .transcript
                .blocks
                .iter()
                .map(crate::transcript::Block::bytes)
                .sum::<usize>()
                > Viewport::MAXIMUM_TEXT
        );
        let middle = Some(block.pieces[4].anchor(0));
        let tail = Some(block.pieces.back().unwrap().anchor(0));
        let mut rejected_gaps = 0;
        let mut capture = SceneCapture::default();
        for (width, height) in [(110, 35), (80, 24), (42, 12), (28, 9)] {
            for top in [None, middle, tail] {
                state.top = top;
                let mut full =
                    ratatui::Terminal::new(ratatui::backend::TestBackend::new(width, height))
                        .unwrap();
                let mut direct = render::View::default();
                full.draw(|frame| {
                    direct =
                        render::draw(frame, &input(&state), &mut render::LayoutCache::default());
                })
                .unwrap();
                let source = capture
                    .capture(&input(&state), width, height)
                    .unwrap()
                    .encode()
                    .unwrap();
                let builds = capture.folds.builds;
                assert_eq!(
                    capture
                        .capture(&input(&state), width, height)
                        .unwrap()
                        .encode()
                        .unwrap(),
                    source
                );
                assert_eq!(
                    capture.folds.builds, builds,
                    "unchanged captures must reuse source wrapping"
                );
                assert!(
                    source.len() < 64 * 1024,
                    "folded source must not serialize hidden process text"
                );
                let (buffer, view) = Scene::decode(&source)
                    .unwrap()
                    .render(width, height)
                    .unwrap();
                assert_eq!(&buffer, full.backend().buffer(), "{width}x{height} {top:?}");
                assert_eq!(view.hits, direct.hits);
                if let (Some(first), Some(last)) = (view.hits.first(), view.hits.last()) {
                    let copy = view.selected_source(&state.transcript, first.3, last.4);
                    rejected_gaps += usize::from(copy.is_err());
                    assert_eq!(
                        copy,
                        direct.selected_source(&state.transcript, first.3, last.4)
                    );
                    let first_in_row = view.hits.iter().find(|hit| hit.0 == last.0).unwrap();
                    assert_eq!(
                        view.selected_source(&state.transcript, first_in_row.3, last.4)
                            .unwrap(),
                        state.transcript.selected(first_in_row.3, last.4).unwrap()
                    );
                }
                assert!(view.sources_belong_to(state.header.session_id(), &state.transcript));
                assert!(
                    Scene::decode(&source)
                        .unwrap()
                        .render(width + 1, height)
                        .is_err()
                );
            }
        }
        assert!(rejected_gaps > 0);
        state.top = middle;
        for block in &mut state.transcript.blocks {
            block.completed = true;
        }
        let source = Scene::capture(&input(&state), 80, 24)
            .unwrap()
            .encode()
            .unwrap();
        let (buffer, view) = Scene::decode(&source).unwrap().render(80, 24).unwrap();
        assert!(
            buffer
                .content
                .iter()
                .map(ratatui::buffer::Cell::symbol)
                .collect::<String>()
                .contains("Thinking")
        );
        assert!(view.hits.is_empty());
        state.transcript.blocks[0].collapsed = false;
        let source = Scene::capture(&input(&state), 80, 24)
            .unwrap()
            .encode()
            .unwrap();
        let (_, view) = Scene::decode(&source).unwrap().render(80, 24).unwrap();
        assert!(view.hits.iter().any(|(_, _, _, a, _)| Some(*a) == middle));
    }

    #[test]
    fn scene_windows_retain_exact_sources_and_cache_without_transferring_whole_history() {
        let mut state = state();
        for seq in 1..=16 {
            add(&mut state, seq, &"中 e\u{301} 👩🏽‍💻\n".repeat(10000));
        }
        let original_bytes: usize = state
            .transcript
            .blocks
            .iter()
            .map(crate::transcript::Block::bytes)
            .sum();
        assert!(original_bytes > Viewport::MAXIMUM_TEXT);
        state
            .editor
            .insert(&"中 e\u{301} 👩🏽‍💻".repeat(10000))
            .unwrap();
        let draft = state.editor.text.clone();
        let mut renderer = Renderer::default();
        for top in [None, state.transcript.blocks[2].anchor(12)] {
            state.top = top;
            let bytes = Scene::capture(&input(&state), 80, 24)
                .unwrap()
                .encode()
                .unwrap();
            let decoded = Scene::decode(&bytes).unwrap();
            assert!(
                matches!(&decoded.surface, Surface::Session(scene) if scene.editor.text.len() <= DRAFT_WINDOW)
            );
            let (_, view) = renderer.render(decoded, 80, 24).unwrap();
            assert!(view.sources_belong_to(state.header.session_id(), &state.transcript));
            assert!(!view.hits.is_empty());
            assert!(
                renderer
                    .previous
                    .blocks
                    .iter()
                    .map(crate::transcript::Block::bytes)
                    .sum::<usize>()
                    <= Viewport::MAXIMUM_TEXT
            );
            let builds = renderer.layout.borrow().builds;
            renderer
                .render(Scene::decode(&bytes).unwrap(), 80, 24)
                .unwrap();
            assert_eq!(renderer.layout.borrow().builds, builds);
            assert_eq!(state.editor.text, draft);
        }
    }
    #[test]
    fn question_index_and_oversized_grapheme_are_rejected_without_panicking() {
        let mut state = state();
        let mut value =
            serde_json::to_value(Scene::capture(&input(&state), 80, 24).unwrap()).unwrap();
        value["surface"]["answer"] = serde_json::json!({
            "scroll": usize::MAX, "answered": usize::MAX,
            "request": {"id":"q", "session_id":"s", "turn_id":"t",
                "questions":[{"id":"a", "prompt":"Question", "options":[]}]},
            "editor":{"text":"", "cursor":0}
        });
        assert!(Scene::decode(&serde_json::to_vec(&value).unwrap()).is_err());
        value["surface"]["answer"]["answered"] = serde_json::json!(0);
        for key in ["detail_offset", "questions", "approvals"] {
            value["surface"][key] = serde_json::json!(usize::MAX);
        }
        assert!(
            Scene::decode(&serde_json::to_vec(&value).unwrap())
                .unwrap()
                .render(80, 24)
                .is_ok()
        );
        state.editor = Editor::with_text(
            format!("a{}", "\u{301}".repeat(DRAFT_WINDOW)),
            crate::MAX_TEXT,
        );
        assert!(Scene::capture(&input(&state), 80, 24).is_err());
    }
    #[test]
    fn external_scene_rejects_invalid_draft_source_mapping_and_closed_fields() {
        let mut state = state();
        add(&mut state, 1, "hello");
        let scene = Scene::capture(&input(&state), 80, 24).unwrap();
        let value = serde_json::to_value(scene).unwrap();
        for (pointer, replacement) in [
            ("/surface/editor/cursor", serde_json::json!(usize::MAX)),
            (
                "/surface/transcript/blocks/0/pieces/0/mapping/0/1",
                serde_json::json!(usize::MAX),
            ),
        ] {
            let mut invalid = value.clone();
            *invalid.pointer_mut(pointer).unwrap() = replacement;
            assert!(
                Scene::decode(&serde_json::to_vec(&invalid).unwrap())
                    .unwrap()
                    .render(80, 24)
                    .is_err()
            );
        }
        let mut unknown = value;
        unknown["context"] = serde_json::json!("ambient");
        assert!(Scene::decode(&serde_json::to_vec(&unknown).unwrap()).is_err());
    }
}

#[cfg(test)]
mod application_tests {
    use super::*;
    #[test]
    fn inline_choices_reuse_bottom_editor_and_grow_up_without_dimming_history() {
        for (width, height) in [(110, 35), (80, 24), (42, 12), (28, 9)] {
            let base = || {
                Scene::from(ApplicationScene {
                    title: "Conversation context".into(),
                    input: Draft {
                        text: "saved draft".into(),
                        cursor: 11,
                    },
                    field: Some(String::new()),
                    hint: "Enter sends".into(),
                    ..ApplicationScene::default()
                })
            };
            let (original, original_view) = base().render(width, height).unwrap();
            let editor = original_view.editor.unwrap();
            for count in [16usize, 1, 0, 16] {
                let scene = base()
                    .with_dialog(Scene::from(ApplicationScene {
                        composer_prefix: Some("/effort ".into()),
                        title: "Reasoning effort".into(),
                        field: Some("Filter choices".into()),
                        items: (0..count).map(|i| format!("effort-{i}")).collect(),
                        selected: count.saturating_sub(1),
                        hint: "Enter select · Esc back".into(),
                        ..ApplicationScene::default()
                    }))
                    .unwrap();
                let (cells, view) = Scene::decode(&scene.encode().unwrap())
                    .unwrap()
                    .render(width, height)
                    .unwrap();
                view.validate(width, height).unwrap();
                assert_eq!(view.editor, Some(editor));
                assert!(view.choices_area.bottom() <= editor.y);
                assert!(view.choices_area.height >= 1 && view.choices_area.height <= 8);
                assert_eq!(
                    cells.content.iter().filter(|c| c.symbol() == "▏").count(),
                    1
                );
                if view.dialog.unwrap().y > 0 {
                    assert_eq!(
                        cells[(0, 0)],
                        original[(0, 0)],
                        "uncovered history stays unchanged"
                    );
                }
                for &(y, index) in &view.choices {
                    assert_eq!(view.choice_at(view.choices_area.x, y), Some(index));
                    assert_eq!(view.choice_at(editor.x, y), None);
                }
            }
        }
    }
    #[test]
    fn dialog_keeps_base_geometry_and_only_exposes_the_focused_layer() {
        for (width, height) in [(110, 35), (80, 24), (42, 12), (28, 9)] {
            let base = || {
                Scene::from(ApplicationScene {
                    title: "Background workspace".into(),
                    input: Draft {
                        text: "retained draft".into(),
                        cursor: 14,
                    },
                    field: Some(String::new()),
                    hint: "Enter submit".into(),
                    ..ApplicationScene::default()
                })
            };
            let (original, base_view) = base().render(width, height).unwrap();
            let base_editor = base_view.editor.unwrap();
            let mut geometry = None;
            for count in [20usize, 1, 0, 20] {
                let scene = base()
                    .with_dialog(Scene::from(ApplicationScene {
                        title: "Reasoning effort".into(),
                        field: Some("Filter choices".into()),
                        items: (0..count).map(|i| format!("effort-{i}")).collect(),
                        selected: count.saturating_sub(1),
                        hint: "Enter select · Esc back".into(),
                        ..ApplicationScene::default()
                    }))
                    .unwrap();
                let (cells, view) = Scene::decode(&scene.encode().unwrap())
                    .unwrap()
                    .render(width, height)
                    .unwrap();
                view.validate(width, height).unwrap();
                let dialog = view.dialog.unwrap();
                let editor = view.editor.unwrap();
                assert_eq!(*geometry.get_or_insert((dialog, editor)), (dialog, editor));
                assert!(editor.y > dialog.y && editor.bottom() <= view.area.y);
                assert!(view.area.height >= 1);
                assert!(view.hits.is_empty() && view.footer.is_empty());
                assert_eq!(
                    cells
                        .content
                        .iter()
                        .filter(|cell| cell.symbol() == "▏")
                        .count(),
                    1
                );
                if dialog.bottom() <= base_editor.y {
                    for x in base_editor.x..base_editor.right() {
                        assert_eq!(
                            cells[(x, base_editor.y)].symbol(),
                            original[(x, base_editor.y)].symbol()
                        );
                    }
                }
                for &(y, index) in &view.choices {
                    assert_eq!(view.choice_at(view.choices_area.x, y), Some(index));
                    assert_eq!(view.choice_at(dialog.x, y), None);
                    assert_eq!(view.choice_at(dialog.right() - 1, y), None);
                }
            }
        }
    }

    #[test]
    fn filtering_results_cannot_relocate_the_application_input() {
        let mut positions = Vec::new();
        for count in [8, 1, 0, 8] {
            let (cells, _) = Scene::from(ApplicationScene {
                title: "Reasoning effort".into(),
                field: Some("Filter choices".into()),
                items: (0..count).map(|i| format!("effort-{i}")).collect(),
                hint: "Enter select · Esc back".into(),
                ..ApplicationScene::default()
            })
            .render(110, 35)
            .unwrap();
            positions.push(
                cells
                    .content
                    .iter()
                    .position(|cell| cell.symbol() == "▏")
                    .unwrap(),
            );
        }
        assert!(
            positions.iter().all(|position| *position == positions[0]),
            "input moved with result count: {positions:?}"
        );
    }

    #[test]
    fn application_form_keeps_its_input_next_to_context() {
        let form = Scene::from(ApplicationScene {
            title: "API key".into(),
            explanation: "deepseek · https://api.deepseek.com".into(),
            field: Some("Key".into()),
            hint: "Enter continue · Esc back".into(),
            ..ApplicationScene::default()
        });
        let (cells, view) = Scene::from(ApplicationScene::default())
            .with_dialog(form)
            .unwrap()
            .render(110, 30)
            .unwrap();
        assert!(view.dialog.is_some());
        let cursor = cells
            .content
            .iter()
            .position(|cell| cell.symbol() == "▏")
            .unwrap();
        assert!(
            cursor / 110 < 10,
            "input must not be stranded at the bottom of a large form"
        );
    }
    #[test]
    fn completion_uses_neutral_selection_bands_and_keeps_narrow_descriptions() {
        let (cells, view) = Scene::from(ApplicationScene {
            title: "RSI".into(),
            field: Some("Input".into()),
            completion: Some(Completion {
                revision: 1,
                items: vec![
                    ("/help".into(), "Commands and shortcuts".into()),
                    ("/model".into(), "Choose a model".into()),
                ],
                selected: 0,
            }),
            ..ApplicationScene::default()
        })
        .render(42, 12)
        .unwrap();
        let (_, area, _) = view.completion.unwrap();
        for y in area.y..area.bottom() {
            for x in area.x..area.right() {
                assert_eq!(
                    cells[(x, y)].bg,
                    ratatui::style::Color::Indexed(if y == area.y { 237 } else { 235 })
                );
            }
        }
        let text: String = cells
            .content
            .iter()
            .map(ratatui::buffer::Cell::symbol)
            .collect();
        assert!(text.contains("Choose a model"));
    }
    #[test]
    fn compact_completion_preserves_input_selection_error_and_acknowledged_hits() {
        for (width, height) in [(110, 30), (80, 24), (42, 12)] {
            let scene = Scene::from(ApplicationScene {
                revision: 9,
                title: "Login".into(),
                field: Some("Input".into()),
                input: Draft {
                    text: "/".into(),
                    cursor: 1,
                },
                completion: Some(Completion {
                    revision: 7,
                    items: (0..20)
                        .map(|i| (format!("/command-{i}"), "description".into()))
                        .collect(),
                    selected: 17,
                }),
                status: "invalid endpoint".into(),
                hint: "Enter · Esc back".into(),
                ..ApplicationScene::default()
            });
            let (cells, view) = scene.render(width, height).unwrap();
            view.validate(width, height).unwrap();
            let text = cells
                .content
                .iter()
                .map(ratatui::buffer::Cell::symbol)
                .collect::<String>();
            assert!(text.contains("/command-17"));
            assert!(text.contains("invalid endpoint"));
            assert!(text.contains("Input"));
            let (revision, area, from) = view.completion.unwrap();
            assert_eq!(revision, 7);
            assert!(area.height <= 8);
            assert!(from <= 17 && from + usize::from(area.height) > 17);
        }
    }
    #[test]
    fn v12_schema_rejects_stale_v11_and_completion_overflow() {
        let request = crate::wire::Request {
            identity: crate::wire::Identity {
                attachment: 0,
                presentation: 1,
                revision: 1,
            },
            width: 80,
            height: 24,
            bytes: 1,
        };
        let mut model: serde_json::Value =
            serde_json::from_slice(&crate::wire::request_header(&request).unwrap()).unwrap();
        assert_eq!(model["schema"]["version"], 12);
        model["schema"]["version"] = serde_json::json!(11);
        assert!(crate::wire::parse_header(&serde_json::to_vec(&model).unwrap()).is_err());
        let scene = Scene::from(ApplicationScene {
            completion: Some(Completion {
                revision: 1,
                items: vec![("x".repeat(513), String::new())],
                selected: 0,
            }),
            ..ApplicationScene::default()
        });
        assert!(Scene::decode(&scene.encode().unwrap()).is_err());
    }
    #[test]
    fn unattached_scene_needs_no_session_and_neutralizes_terminal_controls() {
        let scene = Scene::from(ApplicationScene {
            title: "setup\x1b".into(),
            explanation: "login".into(),
            items: vec!["one\r".into()],
            selected: 0,
            input: Draft {
                text: "draft\x1b\u{202e}".into(),
                cursor: 0,
            },
            status: "status\x1b".into(),
            ..ApplicationScene::default()
        });
        let bytes = scene.encode().unwrap();
        assert!(!String::from_utf8_lossy(&bytes).contains("session_id"));
        let (buffer, view) = Scene::decode(&bytes).unwrap().render(80, 24).unwrap();
        assert!(view.hits.is_empty());
        assert!(
            buffer
                .content
                .iter()
                .all(|cell| !cell.symbol().chars().any(char::is_control))
        );
        let mut invalid: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        invalid["selected"] = serde_json::json!(1);
        assert!(Scene::decode(&serde_json::to_vec(&invalid).unwrap()).is_err());
    }
}
