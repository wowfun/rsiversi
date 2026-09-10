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
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Draft {
    text: String,
    cursor: usize,
}
impl Draft {
    fn capture(editor: &Editor) -> Result<Self, &'static str> {
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
pub struct Scene {
    header: SessionHeader,
    transcript: Viewport,
    editor: Draft,
    model: Option<ModelRef>,
    enter_submit: bool,
    menu: Option<OwnedMenu>,
    answer: Option<OwnedAnswer>,
    ui_edit: Option<OwnedEdit>,
    detail: Option<String>,
    detail_offset: usize,
    selection: Option<(Anchor, Anchor)>,
    top: Option<Anchor>,
    status: String,
    actual_model: Option<String>,
    active: bool,
    busy: bool,
    remote: bool,
    questions: usize,
    approvals: usize,
}
impl Scene {
    pub fn capture(input: &Input<'_>, height: u16) -> Result<Self, &'static str> {
        let scene = Self {
            header: input.header.clone(),
            transcript: Viewport::capture(input.transcript, input.top, height),
            editor: Draft::capture(input.editor)?,
            model: input.model.cloned(),
            enter_submit: input.enter_submit,
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
            top: input.top,
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
        if self
            .answer
            .as_ref()
            .is_some_and(|answer| answer.answered >= answer.request.questions.len())
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
        let Scene {
            header,
            transcript,
            editor,
            model,
            enter_submit,
            menu,
            answer,
            ui_edit,
            detail,
            detail_offset,
            selection,
            top,
            status,
            actual_model,
            active,
            busy,
            remote,
            questions,
            approvals,
        } = scene;
        crate::wire::dimensions(width, height)?;
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
            header: &header,
            transcript: &transcript,
            editor: &editor,
            model: model.as_ref(),
            enter_submit,
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
            let bytes = Scene::capture(&input(&state), 24)
                .unwrap()
                .encode()
                .unwrap();
            let decoded = Scene::decode(&bytes).unwrap();
            assert!(decoded.editor.text.len() <= DRAFT_WINDOW);
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
        let mut value = serde_json::to_value(Scene::capture(&input(&state), 24).unwrap()).unwrap();
        value["answer"] = serde_json::json!({
            "scroll": usize::MAX, "answered": usize::MAX,
            "request": {"id":"q", "session_id":"s", "turn_id":"t",
                "questions":[{"id":"a", "prompt":"Question", "options":[]}]},
            "editor":{"text":"", "cursor":0}
        });
        assert!(Scene::decode(&serde_json::to_vec(&value).unwrap()).is_err());
        value["answer"]["answered"] = serde_json::json!(0);
        for key in ["detail_offset", "questions", "approvals"] {
            value[key] = serde_json::json!(usize::MAX);
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
        assert!(Scene::capture(&input(&state), 24).is_err());
    }
    #[test]
    fn external_scene_rejects_invalid_draft_source_mapping_and_closed_fields() {
        let mut state = state();
        add(&mut state, 1, "hello");
        let scene = Scene::capture(&input(&state), 24).unwrap();
        let value = serde_json::to_value(scene).unwrap();
        for (pointer, replacement) in [
            ("/editor/cursor", serde_json::json!(usize::MAX)),
            (
                "/transcript/blocks/0/pieces/0/mapping/0/1",
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
