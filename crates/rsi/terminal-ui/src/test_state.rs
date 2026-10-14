use crate::*;
use std::cell::RefCell;
pub fn attempt_fact(
    seq: u64,
    timestamp_ms: u64,
    turn: &str,
    effect: &str,
    deltas: Vec<rsi_ai_protocol::ContentDelta>,
    terminal: rsi_ai_protocol::LanguageEvent,
) -> rsi_agent_session_protocol::SessionFact {
    use rsi_ai_protocol::{ContentDelta, ContentStart, LanguageEvent, ToolCallKind};
    let mut events = Vec::new();
    for (index, delta) in deltas.into_iter().enumerate() {
        let index = u32::try_from(index).unwrap();
        let content = match &delta {
            ContentDelta::Text(_) => ContentStart::Text,
            ContentDelta::Reasoning(_) => ContentStart::Reasoning,
            ContentDelta::ToolArguments(_) => ContentStart::ToolCall {
                id: format!("call-{index}"),
                name: "tool".into(),
                kind: ToolCallKind::Function,
            },
        };
        events.extend([
            LanguageEvent::ContentStarted { index, content },
            LanguageEvent::ContentDelta { index, delta },
            LanguageEvent::ContentFinished { index },
        ]);
    }
    events.push(terminal);
    rsi_agent_session_protocol::SessionFact::new(
        seq,
        timestamp_ms,
        rsi_agent_session_protocol::SessionFactBody::ModelAttemptSettled {
            turn_id: rsi_agent_session_protocol::TurnId::new(turn).unwrap(),
            effect_id: rsi_agent_session_protocol::EffectId::new(effect).unwrap(),
            purpose: rsi_agent_session_protocol::ModelEventPurpose::Conversation,
            attempt: rsi_agent_session_protocol::ModelAttemptLog::from_events(
                events
                    .into_iter()
                    .enumerate()
                    .map(|(index, event)| (index as u64 * 10, event)),
                true,
            )
            .unwrap(),
        },
    )
    .unwrap()
}
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
