use crate::*;
use std::cell::RefCell;
pub struct TestState {
    pub header: SessionHeader,
    pub transcript: Transcript,
    pub layout: RefCell<render::LayoutCache>,
    pub editor: Editor,
    pub selection: Option<(Anchor, Anchor)>,
    pub top: Option<Anchor>,
    pub status: String,
    pub todos: Option<rsi_agent_todo::TodoList>,
    pub actual_model: Option<String>,
    pub reasoning_effort: Option<rsi_ai_protocol::ReasoningEffortId>,
    pub active: bool,
    pub remote: bool,
    pub questions: usize,
    pub approvals: usize,
}
impl TestState {
    pub fn new(header: SessionHeader, remote: bool) -> Self {
        Self {
            header,
            remote,
            transcript: Transcript::default(),
            layout: RefCell::default(),
            editor: Editor::default(),
            selection: None,
            top: None,
            status: String::new(),
            todos: None,
            actual_model: None,
            reasoning_effort: None,
            active: false,
            questions: 0,
            approvals: 0,
        }
    }
    pub fn notice(&mut self, text: &str) {
        self.status = terminal_text(text);
    }
}
pub fn draw(frame: &mut ratatui::Frame<'_>, state: &TestState) -> render::View {
    render::draw(frame, &input(state), &mut state.layout.borrow_mut())
}
pub fn input(state: &TestState) -> Input<'_> {
    Input {
        markdown: false,
        fold_focus: None,
        activity: None,
        header: &state.header,
        workspace_label: state.header.canonical_cwd(),
        transcript: &state.transcript,
        editor: &state.editor,
        model: None,
        reasoning_effort: state.reasoning_effort.as_ref(),
        menu_revision: 0,
        todos: state.todos.as_ref(),
        metrics: None,
        metrics_complete: false,
        model_unavailable: false,
        completion: None,
        menu: None,
        answer: None,
        ui_edit: None,
        detail: None,
        detail_offset: 0,
        selection: state.selection,
        top: state.top,
        status: &state.status,
        actual_model: state.actual_model.as_deref(),
        active: state.active,
        busy: false,
        remote: state.remote,
        questions: state.questions,
        approvals: state.approvals,
    }
}
