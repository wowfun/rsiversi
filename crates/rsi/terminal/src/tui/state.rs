use super::{
    editor::Editor,
    transcript::{Anchor, Transcript},
};
use rsi_agent_session_protocol::{SessionHeader, SessionId};
use rsi_ai_protocol::ModelRef;

#[derive(Clone, Debug)]
pub(super) enum Action {
    New,
    Recent,
    Models,
    Commands,
    CommandResult,
    CommandHelp(String, String),
    Extensions,
    Extension(String),
    Queue,
    Agents,
    Questions,
    Approvals,
    Detail,
    Retry,
    Submission,
    DiscardRejected,
    Exit,
    Attach(SessionId),
    Model(Option<ModelRef>),
    MoreRecent,
    MoreModels,
    Message(rsi_agent_store_protocol::StorePendingMessage),
    Child(SessionId),
    Question(rsi_user_questions_protocol::QuestionRequest),
    Approval(rsi_approval_protocol::ApprovalRequest),
    Decide(
        rsi_approval_protocol::ApprovalRequest,
        rsi_approval_protocol::ApprovalDecision,
    ),
    CancelMessage(rsi_agent_session_protocol::MessageId),
    Output(String, u64),
    Window(super::transcript::Source, usize),
}

#[derive(Clone, Debug)]
pub(super) struct Menu {
    pub(super) title: String,
    pub(super) items: Vec<(String, Action)>,
    pub(super) selected: usize,
}

impl Menu {
    pub(super) fn actions() -> Self {
        Self {
            title: "Actions".into(),
            selected: 0,
            items: vec![
                ("New session".into(), Action::New),
                ("Recent sessions".into(), Action::Recent),
                ("Model for next turn".into(), Action::Models),
                ("Session commands".into(), Action::Commands),
                ("Session command result".into(), Action::CommandResult),
                ("Pending inputs".into(), Action::Queue),
                ("Agents".into(), Action::Agents),
                ("Questions".into(), Action::Questions),
                ("Approvals".into(), Action::Approvals),
                ("Focused card / full output".into(), Action::Detail),
                ("Pending / rejected submission".into(), Action::Submission),
                ("Extension state".into(), Action::Extensions),
                ("Exit".into(), Action::Exit),
            ],
        }
    }
}

#[derive(Debug)]
pub(super) struct Answer {
    pub(super) scroll: usize,
    pub(super) request: rsi_user_questions_protocol::QuestionRequest,
    pub(super) answers: Vec<String>,
    pub(super) editor: Editor,
}

pub(super) struct State {
    pub(super) view_revision: u64,
    pub(super) header: SessionHeader,
    pub(super) transcript: Transcript,
    pub(super) editor: Editor,
    pub(super) model: Option<ModelRef>,
    pub(super) menu: Option<Menu>,
    pub(super) answer: Option<Answer>,
    pub(super) detail: Option<String>,
    pub(super) detail_offset: usize,
    pub(super) detail_next: Option<Action>,
    pub(super) detail_previous: Option<Action>,
    pub(super) detail_actions: Option<Menu>,
    pub(super) detail_stop: tokio_util::sync::CancellationToken,
    pub(super) selection: Option<(Anchor, Anchor)>,
    pub(super) top: Option<Anchor>,
    pub(super) focused: usize,
    pub(super) status: String,
    pub(super) actual_model: Option<String>,
    actual_model_seq: u64,
    pub(super) active: bool,
    pub(super) busy: bool,
    pub(super) remote: bool,
    pub(super) questions: usize,
    pub(super) approvals: usize,
}

impl State {
    pub(super) fn invalidate_detail(&mut self) {
        self.view_revision = self.view_revision.wrapping_add(1);
        self.detail_stop.cancel();
        self.detail_stop = tokio_util::sync::CancellationToken::new();
    }
    pub(super) fn open_detail(&mut self, text: String) {
        self.detail = Some(text);
        self.detail_offset = 0;
        self.detail_next = None;
        self.detail_previous = None;
        self.detail_actions = None;
    }
    pub(super) fn new(header: SessionHeader, remote: bool) -> Self {
        Self {
            view_revision: 0,
            header,
            remote,
            transcript: Transcript::default(),
            editor: Editor::default(),
            model: None,
            menu: None,
            answer: None,
            detail: None,
            detail_offset: 0,
            detail_next: None,
            detail_previous: None,
            detail_actions: None,
            detail_stop: tokio_util::sync::CancellationToken::new(),
            selection: None,
            top: None,
            focused: 0,
            status: "Ready".into(),
            actual_model: None,
            actual_model_seq: 0,
            active: false,
            busy: false,
            questions: 0,
            approvals: 0,
        }
    }

    pub(super) fn notice(&mut self, text: impl AsRef<str>) {
        // Errors and labels are external text too. Keep status storage bounded.
        self.status =
            super::super::terminal_text(&text.as_ref().chars().take(2048).collect::<String>());
    }

    pub(super) fn model_fact(&mut self, fact: &rsi_agent_session_protocol::SessionFact) {
        if fact.seq() > self.actual_model_seq
            && let rsi_agent_session_protocol::SessionFactBody::ModelIntent { snapshot, .. } =
                fact.body()
        {
            self.actual_model_seq = fact.seq();
            self.actual_model = Some(format!(
                "actual {}/{}",
                snapshot.deployment_id, snapshot.model
            ));
        }
    }

    pub(super) fn escape(&mut self) {
        self.invalidate_detail();
        if self.menu.take().is_some() {
            return;
        }
        if self.detail.take().is_some() {
            self.detail_next = None;
            self.detail_previous = None;
            self.detail_actions = None;
            return;
        }
        if self.answer.take().is_some() {
            self.notice("Answer draft closed");
            return;
        }
        self.selection = None;
    }
}

impl Drop for State {
    fn drop(&mut self) {
        self.detail_stop.cancel();
    }
}
