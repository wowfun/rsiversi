use super::{
    editor::Editor,
    transcript::{Anchor, Transcript},
};
use rsi_agent_session_protocol::{SessionHeader, SessionId};
use rsi_ai_protocol::ModelRef;

#[derive(Clone, Debug)]
pub(super) enum Action {
    Login,
    SetupModels,
    Plugins(rsi_workbench_ui::PluginsCommand),
    IntegrationCredential(super::setup::IntegrationCredential),
    RecallPrompt(u64),
    Help,
    UiSurface(rsi_ui::UiReference),
    UiCard,
    UiEdit(String, u64),
    UiInvoke(rsi_ui::UiReference, serde_json::Value, u64),
    New,
    Recent,
    References,
    FilePicker(rsi_session_files_ui::FilePickerRequest),
    InsertFile(String),
    ReferenceSources(Option<rsi_session_protocol::RecentSessionCursor>),
    CaptureReference(SessionId),
    PreviewReference(rsi_agent_session_protocol::FrozenReference, usize),
    AddReference(rsi_agent_session_protocol::FrozenReference),
    RemoveReference(String),
    Agents,
    Parent,
    Metrics,
    TreeMetrics(bool),
    Requests(Option<u64>),
    RequestSections(u64),
    RequestManifest(u64),
    Evidence(rsi_session_protocol::EvidenceRead),
    Todos,
    Terminals,
    TerminalStatus(rsi_session_protocol::terminal::Terminal),
    CloseTerminal(String),
    CloseTerminals,
    Jobs(Option<rsi_agent_turn_protocol::TurnJobsRequest>),
    Preview(rsi_agent_turn_protocol::JobPreviewRequest),
    Commands,
    CommandResult,
    CommandHelp(String, String),
    Extensions,
    Extension(String),
    Queue,
    Questions,
    Approvals,
    Detail,
    Retry,
    Submission,
    DiscardRejected,
    Exit,
    Attach(SessionId),
    MoreRecent,
    Message(rsi_agent_store_protocol::StorePendingMessage),
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
                (
                    "Draft references · capture, preview, remove".into(),
                    Action::References,
                ),
                (
                    "@ Workspace file · browse and insert path".into(),
                    Action::FilePicker(rsi_session_files_ui::FilePickerRequest::Open {
                        path: rsi_files_protocol::RelativePath::default(),
                        file_kind: rsi_files_protocol::FileKind::Directory,
                    }),
                ),
                ("Model and effort".into(), Action::SetupModels),
                (
                    "Plugins".into(),
                    Action::Plugins(rsi_workbench_ui::PluginsCommand::Refresh),
                ),
                ("Subagent sessions".into(), Action::Agents),
                ("Return to parent session".into(), Action::Parent),
                ("Session commands".into(), Action::Commands),
                ("Session command result".into(), Action::CommandResult),
                ("Pending inputs".into(), Action::Queue),
                ("Questions".into(), Action::Questions),
                ("Approvals".into(), Action::Approvals),
                ("Raw sources / full output".into(), Action::Detail),
                ("Pending / rejected submission".into(), Action::Submission),
                ("Request usage".into(), Action::Metrics),
                ("Agent tree usage".into(), Action::TreeMetrics(true)),
                ("Inspect requests".into(), Action::Requests(None)),
                ("Tasks".into(), Action::Todos),
                ("Session terminals".into(), Action::Terminals),
                ("Running command output".into(), Action::Jobs(None)),
                ("Extension state".into(), Action::Extensions),
                ("Exit".into(), Action::Exit),
                ("/help · commands and keys".into(), Action::Help),
                ("/login · provider credentials".into(), Action::Login),
                (
                    "/model · discover and configure".into(),
                    Action::SetupModels,
                ),
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

#[allow(clippy::struct_excessive_bools)] // Orthogonal view, lifecycle and read progress flags.
pub(super) struct State {
    pub markdown: bool,
    pub(super) activity: Option<rsi_terminal_ui::Activity>,
    pub(super) live_turn: Option<rsi_agent_session_protocol::TurnId>,
    turn_clock: Option<(u64, rsi_agent_session_protocol::TurnId, u64)>,
    pub(super) slash: super::slash::Ui,
    pub(super) ui_form: Option<super::ui::Form>,
    pub(super) ui_edit: Option<super::ui::Edit>,
    pub(super) view_revision: u64,
    pub(super) header: SessionHeader,
    pub(super) workspace_label: String,
    pub(super) transcript: Transcript,
    pub(super) editor: Editor,
    pub(super) references: Vec<rsi_agent_session_protocol::FrozenReference>,
    pub(super) reference_bytes: usize,
    pub(super) reference_status: String,
    pub(super) file_insert: Option<std::ops::Range<usize>>,
    pub(super) model: Option<ModelRef>,
    pub(super) reasoning_effort: Option<rsi_ai_protocol::ReasoningEffortId>,
    pub(super) menu: Option<Menu>,
    pub(super) answer: Option<Answer>,
    pub(super) detail: Option<String>,
    pub(super) detail_offset: usize,
    pub(super) detail_next: Option<Action>,
    pub(super) detail_previous: Option<Action>,
    pub(super) detail_actions: Option<Menu>,
    pub(super) detail_stop: tokio_util::sync::CancellationToken,
    pub(super) preview: Option<super::job_preview::Preview>,
    pub(super) selection: Option<(Anchor, Anchor)>,
    pub(super) top: Option<Anchor>,
    pub(super) fold_focus: Option<(Anchor, u16)>,
    pub(super) focused: usize,
    pub(super) folds: std::collections::VecDeque<(String, bool)>,
    pub(super) todos: Option<rsi_agent_todo::TodoList>,
    pub(super) metrics: Option<rsi_session_protocol::MetricsRead>,
    pub(super) status: String,
    status_is_error: bool,
    notice_id: u64,
    pub(super) actual_model: Option<String>,
    actual_model_seq: u64,
    pub(super) active: bool,
    pub(super) busy: bool,
    pub(super) remote: bool,
    pub(super) questions: usize,
    pub(super) approvals: usize,
}

impl State {
    pub(super) fn model_unavailable(&self) -> bool {
        self.metrics.as_ref().is_some_and(|read| {
            read.current_model.selection.model
                == *self
                    .model
                    .as_ref()
                    .unwrap_or(self.header.settings().default_model())
                && matches!(
                    read.current_model.availability,
                    rsi_session_protocol::ModelAvailability::Unavailable { .. }
                )
        })
    }
    pub(super) fn display_effort(&self) -> Option<&rsi_ai_protocol::ReasoningEffortId> {
        let requested = self.reasoning_effort.as_ref().or_else(|| {
            self.model
                .is_none()
                .then(|| self.header.settings().default_reasoning_effort())
                .flatten()
        });
        if let Some(read) = &self.metrics {
            let model = self
                .model
                .as_ref()
                .unwrap_or(self.header.settings().default_model());
            if &read.current_model.selection.model == model
                && read.current_model.selection.reasoning_effort.as_ref() == requested
            {
                return read.current_model.effective_effort().or(requested);
            }
        }
        requested
    }
    pub(super) fn invalidate_detail(&mut self) {
        self.preview = None;
        self.view_revision = self.view_revision.wrapping_add(1);
        self.detail_stop.cancel();
        self.detail_stop = tokio_util::sync::CancellationToken::new();
    }
    pub(super) fn open_detail(&mut self, text: String) {
        self.ui_form = None;
        self.ui_edit = None;
        self.detail = Some(text);
        self.detail_offset = 0;
        self.detail_next = None;
        self.detail_previous = None;
        self.detail_actions = None;
    }
    pub(super) fn new(header: SessionHeader, remote: bool) -> Self {
        Self {
            markdown: true,
            activity: None,
            live_turn: None,
            turn_clock: None,
            slash: super::slash::Ui::default(),
            ui_form: None,
            ui_edit: None,
            view_revision: 0,
            workspace_label: workspace_label(
                header.canonical_cwd(),
                std::env::home_dir().as_deref(),
            ),
            header,
            remote,
            transcript: Transcript::default(),
            editor: Editor::default(),
            references: Vec::new(),
            reference_bytes: 0,
            reference_status: String::new(),
            file_insert: None,
            model: None,
            reasoning_effort: None,
            menu: None,
            answer: None,
            detail: None,
            detail_offset: 0,
            detail_next: None,
            detail_previous: None,
            detail_actions: None,
            detail_stop: tokio_util::sync::CancellationToken::new(),
            preview: None,
            selection: None,
            top: None,
            fold_focus: None,
            focused: 0,
            folds: std::collections::VecDeque::new(),
            todos: None,
            metrics: None,
            status: String::new(),
            status_is_error: false,
            notice_id: 0,
            actual_model: None,
            actual_model_seq: 0,
            active: false,
            busy: false,
            questions: 0,
            approvals: 0,
        }
    }

    pub(super) fn focus_next_source(&mut self) {
        let count = self.transcript.blocks.len();
        if let Some(index) = (1..=count)
            .map(|offset| (self.focused + offset) % count)
            .find(|index| !self.transcript.blocks[*index].pieces.is_empty())
        {
            self.focused = index;
            self.toggle_fold();
        }
    }
    pub(super) fn toggle_fold(&mut self) {
        if let Some(block) = self.transcript.blocks.get_mut(self.focused) {
            if matches!(
                block.role,
                rsi_terminal_ui::transcript::Role::User
                    | rsi_terminal_ui::transcript::Role::Assistant
                    | rsi_terminal_ui::transcript::Role::Metadata
            ) {
                return;
            }
            block.collapsed = !block.collapsed;
            self.folds.retain(|(key, _)| key != &block.key);
            if self.folds.len() == 512 {
                self.folds.pop_front();
            }
            self.folds.push_back((block.key.clone(), block.collapsed));
            self.view_revision = self.view_revision.wrapping_add(1);
        }
    }
    pub(super) fn apply_folds(&mut self) {
        for block in &mut self.transcript.blocks {
            if let Some((_, collapsed)) = self.folds.iter().find(|(key, _)| key == &block.key) {
                block.collapsed = *collapsed;
            }
        }
    }

    pub(super) fn notice(&mut self, text: impl AsRef<str>) {
        self.feedback(text.as_ref(), true);
    }

    pub(super) fn info(&mut self, text: impl AsRef<str>) {
        self.feedback(text.as_ref(), false);
    }
    fn feedback(&mut self, text: &str, error: bool) {
        let text = super::super::terminal_text(&text.chars().take(2048).collect::<String>());
        if !text.is_empty() && text != self.status && !self.has_dialog() {
            self.notice_id = self.notice_id.wrapping_add(1);
            if error {
                self.transcript
                    .push_error(self.notice_id, &format!("! {text}"));
            } else {
                self.transcript.push_notice(self.notice_id, &text);
            }
        }
        self.status = text;
        self.status_is_error = error;
    }
    pub(super) fn clear_info(&mut self) {
        if !self.status_is_error {
            self.status.clear();
        }
    }

    pub(super) fn model_fact(&mut self, fact: &rsi_agent_session_protocol::SessionFact) {
        if let rsi_agent_session_protocol::SessionFactBody::TurnAccepted { turn_id, .. }
        | rsi_agent_session_protocol::SessionFactBody::MessageTurnAccepted { turn_id, .. } =
            fact.body()
            && self
                .turn_clock
                .as_ref()
                .is_none_or(|(seq, ..)| *seq < fact.seq())
        {
            self.turn_clock = Some((fact.seq(), turn_id.clone(), fact.timestamp_ms()));
        }
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

    pub(super) fn tick_activity(&mut self, now_ms: u64) -> bool {
        let next =
            self.live_turn
                .as_ref()
                .filter(|_| self.active)
                .map(|turn| rsi_terminal_ui::Activity {
                    turn_id: turn.clone(),
                    now_ms,
                    started_ms: self
                        .turn_clock
                        .as_ref()
                        .filter(|(_, clock_turn, _)| clock_turn == turn)
                        .map(|(_, _, time)| *time),
                });
        let changed =
            self.activity.as_ref().map(|a| a.now_ms / 120) != next.as_ref().map(|a| a.now_ms / 120);
        self.activity = next;
        changed
    }

    pub(super) fn has_dialog(&self) -> bool {
        self.menu.is_some()
            || self.detail.is_some()
            || self.answer.is_some()
            || self.ui_edit.is_some()
    }

    pub(super) fn escape(&mut self) {
        self.status.clear();
        self.status_is_error = false;
        if self.menu.take().is_some() {
            if self.detail.is_none() {
                self.file_insert = None;
            }
            if self.ui_form.is_none() {
                self.invalidate_detail();
            }
            return;
        }
        self.invalidate_detail();
        if self.ui_edit.take().is_some() {
            self.refresh_ui();
            return;
        }
        if self.detail.take().is_some() {
            self.file_insert = None;
            self.ui_form = None;
            self.detail_next = None;
            self.detail_previous = None;
            self.detail_actions = None;
            return;
        }
        if self.answer.take().is_some() {
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

fn workspace_label(cwd: &str, home: Option<&std::path::Path>) -> String {
    let Some(relative) = home
        .filter(|path| path.is_absolute())
        .and_then(|home| std::path::Path::new(cwd).strip_prefix(home).ok())
    else {
        return cwd.into();
    };
    if relative.as_os_str().is_empty() {
        "~".into()
    } else {
        format!("~/{}", relative.display())
    }
}

#[cfg(test)]
mod workspace_tests {
    use super::workspace_label;
    use std::path::Path;
    #[test]
    fn home_abbreviation_uses_path_components_and_preserves_workspace_authority() {
        let home = Some(Path::new("/home/kevin"));
        assert_eq!(workspace_label("/home/kevin", home), "~");
        assert_eq!(
            workspace_label("/home/kevin/Projects/你好", home),
            "~/Projects/你好"
        );
        assert_eq!(
            workspace_label("/home/kevin2/project", home),
            "/home/kevin2/project"
        );
        assert_eq!(workspace_label("/work/project", home), "/work/project");
        assert_eq!(
            workspace_label("/home/kevin/project", None),
            "/home/kevin/project"
        );
    }
}
