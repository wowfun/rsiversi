//! Fullscreen presentation over Session. The Kernel remains the execution authority.
mod clipboard;
mod commands;
mod external;
mod home;
mod plugins;
mod profiles;
mod setup;
mod slash;
mod terminals;
use rsi_terminal_ui::editor;
mod files;
mod history_search;
mod input;
mod job_preview;
mod prompts;
mod references;
mod render;
mod request_details;
mod retention;
mod state;
mod terminal;
#[cfg(test)]
mod tests;
use rsi_terminal_ui::transcript;
mod ui;

use super::*;
use futures_util::{StreamExt as _, stream::FuturesUnordered};
use rsi_agent_session_protocol::{MessageDelivery, SessionHeader};
use rsi_agent_store_protocol::StoreSessionInspection;
use state::{Action, Menu, State};
use std::collections::BTreeSet;
use termina::event::{KeyCode, KeyEventKind, Modifiers, MouseButton, MouseEventKind};
use tokio::sync::mpsc;

pub(super) fn parse(arguments: Vec<OsString>) -> Result<SessionCommand> {
    if arguments
        .iter()
        .any(|arg| matches!(arg.to_str(), Some("--list" | "--history" | "--output")))
    {
        return Err(usage(
            "tui uses its action menu for listing and history; --output is unavailable",
        ));
    }
    let command = SessionCommand::parse(arguments)?;
    terminal::check().map_err(usage)?;
    Ok(command)
}

pub(super) struct Services {
    pub ui: Arc<rsi_ui::Ui>,
    pub ui_target: Arc<rsi_ui::UiTarget>,
    pub application: Arc<dyn SessionService>,
    pub output_cache: Arc<dyn rsi_process::ProcessOutputCache>,
    pub model_catalog: Arc<dyn rsi_ai_protocol::LanguageModels>,
    pub workspace: Arc<dyn rsi_workspace_protocol::WorkspaceRegistry>,
    pub lifetime: rsi_client::ConnectionLifetime,
}

pub(super) async fn run(
    presentation: Option<rsi_host::Profile>,
    services: Services,
    command: SessionCommand,
    application_work: ApplicationWork,
    context: rsi_meta::Context,
) -> u8 {
    match Box::pin(run_inner(
        presentation,
        services,
        command,
        application_work,
        context,
    ))
    .await
    {
        Ok(()) => 0,
        Err(error) => report_error(&error),
    }
}

fn error(error: impl std::fmt::Display) -> RsiError {
    RsiError::Run(error.to_string())
}

async fn read<T, F: std::future::Future<Output = rsi_session_protocol::Result<T>>>(
    request: impl FnMut() -> F,
) -> Result<T> {
    let execution = rsi_meta::Execution::native(tokio::runtime::Handle::current());
    rsi_client::read_with_capacity_retry(&execution, request)
        .await
        .map_err(error)
}

async fn select_workspace(
    workspace: &dyn rsi_workspace_protocol::WorkspaceRegistry,
    cwd: &std::path::Path,
) -> rsi_workspace_protocol::Result<rsi_workspace_protocol::WorkspaceRecord> {
    let mut delay = std::time::Duration::from_millis(50);
    for attempt in 0..5 {
        match workspace.get_or_create(cwd).await {
            Err(rsi_workspace_protocol::WorkspaceError::Api(
                rsi_api_protocol::ApiError::Capacity,
            )) if attempt < 4 => {
                tokio::time::sleep(delay).await;
                delay *= 2;
            }
            result => return result,
        }
    }
    unreachable!("the final attempt returns its result")
}

struct Attachment {
    handle: Arc<dyn SessionHandle>,
    header: SessionHeader,
    inspection: Option<StoreSessionInspection>,
    page: Option<rsi_session_protocol::SessionHistoryPage>,
    live_page: Option<rsi_session_protocol::SessionHistoryPage>,
}

async fn attachment(
    handle: Arc<dyn SessionHandle>,
    durable: bool,
    top: Option<transcript::Anchor>,
) -> Result<Attachment> {
    let header = handle
        .header()
        .await
        .map_err(|failure| error(format!("Session header read failed: {failure}")))?;
    let inspection = if durable {
        Some(read(|| handle.inspect()).await?)
    } else {
        None
    };
    let page = match &inspection {
        Some(snapshot) => Some(
            read(|| {
                handle.history_before(
                    top.map_or(snapshot.durable_fact_seq, |anchor| anchor.source.seq)
                        .checked_add(1),
                    128,
                )
            })
            .await?,
        ),
        None => None,
    };
    let live_page = match (&inspection, top) {
        (Some(snapshot), Some(anchor)) if anchor.source.seq < snapshot.durable_fact_seq => Some(
            read(|| handle.history_before(snapshot.durable_fact_seq.checked_add(1), 128)).await?,
        ),
        _ => None,
    };
    Ok(Attachment {
        handle,
        header,
        inspection,
        page,
        live_page,
    })
}

fn live_window(page: rsi_session_protocol::SessionHistoryPage) -> transcript::Transcript {
    let mut transcript = transcript::Transcript::default();
    for fact in page.facts {
        transcript.apply(&fact);
    }
    transcript.earlier = page.has_more;
    transcript
}

enum Update {
    AttentionRead,
    HistorySearch(Box<rsi_history_api::Request>, Box<rsi_history_api::Reply>),
    Ui(rsi_ui::BoundView),
    Plugins(Box<rsi_workbench_ui::PluginsView>),
    Attached(Box<Attachment>),
    History(rsi_session_protocol::SessionHistoryPage),
    Inspect(Box<StoreSessionInspection>),
    Metrics(Box<rsi_session_protocol::Result<rsi_session_protocol::MetricsRead>>),
    TreeMetrics(rsi_session_protocol::TreeMetricsRead),
    Evidence(rsi_session_protocol::EvidencePage),
    Manifest(String),
    Preview(rsi_session_protocol::Result<rsi_agent_turn_protocol::JobPreviewPage>),
    Menu(Menu),
    Recent(rsi_session_protocol::RecentSessionPage),
    Reference(rsi_agent_session_protocol::ReferenceTextPage),
    FilePicker(rsi_session_files_ui::FilePickerPage),
    Message(rsi_agent_session_protocol::AgentMessage),
    Output(rsi_process::OutputPage),
    Notice(String),
    ModelSelected(rsi_agent_session_protocol::ModelSelection),
    Copy(clipboard::Delivery),
    Submitted(rsi_session_protocol::Result<rsi_agent_turn_protocol::MessageReceipt>),
    Command(rsi_session_protocol::Result<rsi_agent_session_protocol::SessionCommandReceipt>),
    Window(transcript::Piece),
}

#[derive(Clone, Copy)]
enum WorkKind {
    Read,
    Detail,
    Inspect,
    History,
    Submit,
    Cancel,
}
struct Work {
    generation: u64,
    view_revision: u64,
    history_revision: u64,
    kind: WorkKind,
    result: Result<Update>,
}
impl Work {
    fn superseded(&self, client: &Client) -> bool {
        self.generation != client.generation
            || matches!(self.kind, WorkKind::History)
                && self.history_revision != client.history.revision
            || matches!(self.kind, WorkKind::Detail)
                && self.view_revision != client.state.view_revision
    }
}
type RenderJob = std::pin::Pin<
    Box<
        dyn std::future::Future<
                Output = (
                    rsi_terminal_ui::wire::Request,
                    std::result::Result<crate::presentation::Frame, String>,
                ),
            > + Send,
    >,
>;
type Task = std::pin::Pin<Box<dyn std::future::Future<Output = Work> + Send>>;

#[derive(Default)]
struct Submission {
    request: Option<SubmitInput>,
    busy: bool,
    cancel_when_accepted: bool,
    rejected: bool,
}
#[derive(Default)]
struct History {
    revision: u64,
    before: Option<u64>,
    loading: bool,
    pages: usize,
    bytes: usize,
    backfill: bool,
}

struct SavedSession {
    command: Arc<rsi_client::CommandSubmission>,
    editor: editor::Editor,
    references: Vec<rsi_agent_session_protocol::FrozenReference>,
    reference_bytes: usize,
    model: Option<ModelRef>,
    reasoning_effort: Option<rsi_ai_protocol::ReasoningEffortId>,
    top: Option<transcript::Anchor>,
    folds: std::collections::VecDeque<(String, bool)>,
    last_used: u64,
    owned: BTreeSet<MessageId>,
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum Durability {
    Draft,
    Durable,
}

#[allow(clippy::struct_excessive_bools)] // Independent asynchronous request lanes have separate pending flags.
struct Client {
    text_history: Option<rsi_history_api::Client>,
    history_query: Option<rsi_history_api::Request>,
    plugins: Option<Arc<rsi_workbench_ui::PluginsFeature>>,
    files: Option<
        Arc<<rsi_session_files_ui::FilesBrowserContract as rsi_meta::LocalContract>::Service>,
    >,
    setup_command: Option<setup::Command>,
    integration_credential: Option<setup::IntegrationCredential>,
    prompts: prompts::Prompts,
    ui: ui::Bindings,
    application: Arc<dyn SessionService>,
    output_cache: Arc<dyn rsi_process::ProcessOutputCache>,
    workspace: Arc<dyn rsi_workspace_protocol::WorkspaceRegistry>,
    handle: Arc<dyn SessionHandle>,
    controller: Arc<rsi_client::SessionController>,
    durability: Durability,
    state: State,
    generation: u64,
    tasks: FuturesUnordered<Task>,
    owned: BTreeSet<MessageId>,
    submission: Submission,
    command: Arc<rsi_client::CommandSubmission>,
    cancelling: bool,
    cancellation_queued: bool,
    inspection: Option<StoreSessionInspection>,
    interactions: Option<rsi_session_protocol::InteractionSnapshot>,
    projections: Option<rsi_session_protocol::ProjectionSnapshot>,
    projection_notice: String,
    extension_view: Option<commands::ExtensionView>,
    history: History,
    live_transcript: Option<transcript::Transcript>,
    inspecting: bool,
    metrics_loading: bool,
    drafts: BTreeMap<SessionId, SavedSession>,
    recent: Option<rsi_session_protocol::RecentSessionCursor>,
}

impl Client {
    fn switch_session_draft(&mut self, header: SessionHeader) {
        let old = self.state.header.session_id().clone();
        let draft = std::mem::take(&mut self.state.editor);
        let references = std::mem::take(&mut self.state.references);
        let model = self.state.model.take();
        let owned = std::mem::take(&mut self.owned);
        let command = std::mem::take(&mut self.command);
        let top = self.state.top;
        let folds = std::mem::take(&mut self.state.folds);
        let reasoning_effort = self.state.reasoning_effort.clone();
        self.drafts.insert(
            old,
            SavedSession {
                editor: draft,
                references,
                reference_bytes: self.state.reference_bytes,
                model,
                reasoning_effort,
                top,
                folds,
                last_used: self.generation,
                owned,
                command,
            },
        );
        let markdown = self.state.markdown;
        self.state = State::new(header, self.state.remote);
        self.state.markdown = markdown;
        if let Some(saved) = self.drafts.remove(self.state.header.session_id()) {
            self.state.editor = saved.editor;
            self.state.references = saved.references;
            self.state.refresh_references();
            self.state.model = saved.model;
            self.state.reasoning_effort = saved.reasoning_effort;
            self.state.top = saved.top;
            self.state.folds = saved.folds;
            self.owned = saved.owned;
            self.command = saved.command;
        }
    }

    fn toggle_fold_at(&mut self, view: &render::View, x: u16, y: u16) -> bool {
        if view.0.choice_revision != self.state.view_revision {
            return false;
        }
        let Some(index) =
            view.0
                .fold_at(self.state.header.session_id(), &self.state.transcript, x, y)
        else {
            return false;
        };
        self.state.selection = None;
        self.retain_visible_top(view);
        if let Some(anchor) = self.state.transcript.blocks[index].anchor(0) {
            self.state.top.get_or_insert(anchor);
            self.state.fold_focus = Some((anchor, y.saturating_sub(view.0.area.y)));
        }
        self.state.focused = index;
        self.state.toggle_fold();
        true
    }

    fn retain_visible_top(&mut self, view: &render::View) {
        if self.state.top.is_none()
            && let Some((block, offset)) = view.location(&self.state, 0)
        {
            self.state.top = self.state.transcript.blocks[block].anchor(offset);
        }
    }

    fn new(
        services: Services,
        attached: Attachment,
        controller: Arc<rsi_client::SessionController>,
        surface_target: Arc<rsi_ui::UiTarget>,
    ) -> Self {
        let Services {
            application,
            output_cache,
            model_catalog: _,
            workspace,
            lifetime,
            ui,
            ui_target,
        } = services;
        let mut client = Self {
            files: None,
            plugins: None,
            text_history: None,
            history_query: None,
            setup_command: None,
            integration_credential: None,
            prompts: prompts::Prompts::default(),
            ui: ui::Bindings {
                registry: ui,
                application: ui_target,
                surface: surface_target,
            },
            application,
            output_cache,
            workspace,
            handle: attached.handle,
            controller,
            durability: if attached.inspection.is_some() {
                Durability::Durable
            } else {
                Durability::Draft
            },
            state: State::new(
                attached.header,
                lifetime == rsi_client::ConnectionLifetime::Remote,
            ),
            generation: 0,
            tasks: FuturesUnordered::new(),
            owned: BTreeSet::new(),
            submission: Submission::default(),
            command: Arc::default(),
            cancelling: false,
            cancellation_queued: false,
            inspection: attached.inspection,
            interactions: None,
            projections: None,
            projection_notice: String::new(),
            extension_view: None,
            live_transcript: attached.live_page.map(live_window),
            history: History {
                backfill: attached.page.is_some(),
                ..History::default()
            },
            inspecting: false,
            metrics_loading: false,
            drafts: BTreeMap::new(),
            recent: None,
        };
        client.state.active = client
            .inspection
            .as_ref()
            .is_some_and(|snapshot| snapshot.active_turn_id.is_some());
        client.state.live_turn = client
            .inspection
            .as_ref()
            .and_then(|snapshot| snapshot.active_turn_id.clone());
        if let Some(page) = attached.page {
            client.page(page);
        }
        client
    }
    fn spawn(&mut self, task: impl std::future::Future<Output = Result<Update>> + Send + 'static) {
        self.spawn_as(WorkKind::Read, task);
    }

    fn spawn_detail(
        &mut self,
        task: impl std::future::Future<Output = Result<Update>> + Send + 'static,
    ) {
        let stop = self.state.detail_stop.clone();
        self.spawn_as(WorkKind::Detail, async move {
            tokio::select! {
                biased;
                () = stop.cancelled() => Err(error("Detail closed")),
                result = task => result,
            }
        });
    }

    fn spawn_as(
        &mut self,
        kind: WorkKind,
        task: impl std::future::Future<Output = Result<Update>> + Send + 'static,
    ) -> bool {
        let limit = match kind {
            WorkKind::Cancel => 12,
            WorkKind::Submit => 10,
            WorkKind::Read | WorkKind::Detail | WorkKind::Inspect | WorkKind::History => 8,
        };
        if self.tasks.len() >= limit {
            self.state
                .notice("Client request queue is busy; input retained");
            return false;
        }
        let generation = self.generation;
        let view_revision = self.state.view_revision;
        let history_revision = self.history.revision;
        self.tasks.push(Box::pin(async move {
            Work {
                generation,
                view_revision,
                history_revision,
                kind,
                result: task.await,
            }
        }));
        true
    }

    fn inspect(&mut self) {
        if self.inspecting || self.tasks.len() >= 8 {
            return;
        }
        self.inspecting = true;
        let handle = self.handle.clone();
        self.spawn_as(WorkKind::Inspect, async move {
            read(|| handle.inspect())
                .await
                .map(|snapshot| Update::Inspect(Box::new(snapshot)))
        });
    }

    fn refresh_metrics(&mut self) {
        if self.metrics_loading || self.tasks.len() >= 8 {
            return;
        }
        self.metrics_loading = true;
        let handle = self.handle.clone();
        self.spawn(async move { Ok(Update::Metrics(Box::new(handle.metrics().await))) });
    }

    fn history(&mut self, manual: bool) {
        if self.history.loading || self.tasks.len() >= 8 {
            return;
        }
        if manual {
            self.history.before = self
                .state
                .transcript
                .blocks
                .iter()
                .flat_map(|block| block.pieces.iter())
                .map(|piece| piece.source.seq)
                .min()
                .filter(|seq| *seq > 1)
                .or(self.history.before);
        }
        let Some(before) = self.history.before else {
            self.history.backfill = false;
            return;
        };
        if manual {
            if self.live_transcript.is_none() {
                self.live_transcript = Some(self.state.transcript.clone());
            }
            self.history.pages = 0;
            self.history.bytes = 0;
            self.history.backfill = true;
        }
        self.history.loading = true;
        let handle = self.handle.clone();
        self.spawn_as(WorkKind::History, async move {
            read(|| handle.history_before(Some(before), 128))
                .await
                .map(Update::History)
        });
    }

    fn page(&mut self, page: rsi_session_protocol::SessionHistoryPage) {
        self.history.loading = false;
        self.history.pages += 1;
        self.history.bytes = self.history.bytes.saturating_add(
            page.facts
                .iter()
                .map(SessionFact::encoded_len)
                .sum::<usize>(),
        );
        self.history.before = page
            .has_more
            .then(|| page.facts.first().map_or(page.before_seq, SessionFact::seq));
        let boundary = page.facts.iter().any(|fact| {
            matches!(
                fact.body(),
                SessionFactBody::TurnAccepted { .. } | SessionFactBody::MessageTurnAccepted { .. }
            )
        });
        if self.live_transcript.is_some() {
            self.state.transcript.earlier = page.has_more;
        }
        for fact in page.facts {
            self.state.model_fact(&fact);
            if self.live_transcript.is_some() {
                self.state.transcript.apply_history(&fact);
            } else {
                self.state.transcript.apply(&fact);
            }
        }
        self.state.transcript.earlier |= page.has_more;
        self.state.apply_folds();
        if boundary
            || self.history.pages >= 8
            || self.history.bytes >= 16 * 1024 * 1024
            || !page.has_more
        {
            self.history.backfill = false;
        }
    }

    #[allow(clippy::too_many_lines)] // Application grammar precedes the existing durable submission state machine.
    fn submit(&mut self, delivery: MessageDelivery, retry: bool) {
        if !retry
            && !slash::literal(self.state.editor.text())
            && self.state.editor.text().split_whitespace().next() == Some("/model-selection")
        {
            self.state
                .notice("Unknown command /model-selection. Use /model or /effort.");
            return;
        }
        if !retry && let Some(command) = setup::command(self.state.editor.text()) {
            if self.state.editor.cursor() != self.state.editor.text().len()
                || matches!(command, setup::Command::Invalid)
            {
                self.state.notice("Invalid application command or cursor not at end. Draft retained; /help lists usage.");
                return;
            }
            self.state.editor.take();
            self.setup_command = Some(command);
            return;
        }

        if let Some(pending) = self.command.view().pending {
            self.state.notice(format!(
                "Command {} is unresolved; use Actions → Session command result",
                pending.request_id
            ));
            return;
        }
        if self.submission.busy {
            self.state
                .notice("Submission is still being reconciled; draft retained");
            return;
        }
        if !retry {
            if self.submission.request.is_some() {
                self.state.notice("Resolve the previous submission through Actions → Retry; this draft is retained");
                return;
            }
            if self.state.editor.text().trim().is_empty() && self.state.references.is_empty() {
                return;
            }
            if delivery == MessageDelivery::Steer && !self.state.active {
                self.state.notice(
                    "No active turn in the latest snapshot; use Enter for the selected model",
                );
                return;
            }
            if self.owned.len()
                + self
                    .drafts
                    .values()
                    .map(|saved| saved.owned.len())
                    .sum::<usize>()
                >= 1024
            {
                self.state
                    .notice("Client pending-input limit reached; wait or cancel inputs");
                return;
            }
            let message_id = match generated_cli_message_id() {
                Ok(id) => id,
                Err(problem) => {
                    self.state.notice(problem.to_string());
                    return;
                }
            };
            let content = self.state.reference_input();
            if let Err(problem) = rsi_session_protocol::validate_session_input(&content) {
                self.state.notice(problem.to_string());
                return;
            }
            self.submission.request = Some(SubmitInput {
                reasoning_effort: None,
                delivery,
                message_id,
                content,
                model: None,
                sandbox: None,
            });
            self.state.editor.take();
            self.state.references.clear();
            self.state.refresh_references();
            self.remember_prompt();
        }
        let Some(request) = self.submission.request.clone() else {
            self.state.info("No unresolved submission");
            return;
        };
        self.owned.insert(request.message_id.clone());
        let controller = self.controller.clone();
        let command = self.command.clone();
        self.submission.busy = true;
        let was_rejected = self.submission.rejected;
        self.submission.rejected = false;
        self.submission.busy = self.spawn_as(WorkKind::Submit, async move {
            if !retry
                && let [MessageInput::Text { text }] = request.content.as_slice()
                && !slash::literal(text)
            {
                let id = rsi_agent_session_protocol::DomainRequestId::new(format!(
                    "command-{}",
                    request.message_id
                ))
                .map_err(error)?;
                match command.try_slash(&controller, text, id).await {
                    Ok(Some(receipt)) => return Ok(Update::Command(Ok(receipt))),
                    Ok(None) => {}
                    Err(error) => return Ok(Update::Command(Err(error))),
                }
            }
            Ok(Update::Submitted(if retry {
                controller.retry(request).await
            } else {
                controller.submit(request).await
            }))
        });
        self.submission.rejected = !self.submission.busy && (!retry || was_rejected);
    }

    fn cancel(&mut self) {
        self.cancellation_queued = true;
        self.submission.cancel_when_accepted |= self.submission.request.is_some();
        if self.cancelling {
            return;
        }
        let handle = self.handle.clone();
        let owned = self.owned.clone();
        self.state.info("Cancelling…");
        self.cancelling = self.spawn_as(WorkKind::Cancel, async move {
            let snapshot = read(|| handle.inspect()).await?;
            let mut count = 0;
            if let Some(turn) = snapshot.active_turn_id {
                count += usize::from(
                    handle
                        .cancel(CancelTarget::Turn(turn), None)
                        .await
                        .map_err(error)?
                        .accepted,
                );
            }
            for pending in snapshot.pending {
                if owned.contains(&pending.message_id) {
                    count += usize::from(
                        handle
                            .cancel(CancelTarget::Message(pending.message_id), None)
                            .await
                            .map_err(error)?
                            .accepted,
                    );
                }
            }
            Ok(Update::Notice(format!(
                "Cancellation accepted for {count} targets"
            )))
        });
        self.cancellation_queued = !self.cancelling;
    }

    fn scroll(&mut self, view: &render::View, up: bool) {
        if self.state.has_dialog() && view.0.dialog.is_none() {
            return;
        }
        if let Some(answer) = &mut self.state.answer {
            answer.scroll = if up {
                answer.scroll.saturating_sub(3)
            } else {
                answer.scroll.saturating_add(3).min(view.0.scroll_max)
            };
            return;
        }
        if self.state.detail.is_some() {
            self.state.detail_offset = if up {
                self.state.detail_offset.saturating_sub(3)
            } else {
                self.state
                    .detail_offset
                    .saturating_add(3)
                    .min(view.0.scroll_max)
            };
            return;
        }
        if !view.belongs_to(&self.state) {
            return;
        }
        if view.is_empty() {
            if up {
                self.history(true);
            }
            return;
        }
        if !view.sources_belong_to(self.state.header.session_id(), &self.state.transcript) {
            return;
        }
        if !up && view.at_bottom() {
            return;
        }
        self.state.fold_focus = None;
        if let Some(anchor) =
            view.scroll_anchor(self.state.header.session_id(), &self.state.transcript, up)
        {
            self.state.top = Some(anchor);
            if up && self.state.transcript.locate(anchor) == Some((0, 0)) {
                self.history(true);
            }
        } else if !up {
            self.follow_live();
        }
    }

    fn message_detail(&mut self, message: rsi_agent_session_protocol::AgentMessage) {
        self.state
            .open_detail(super::terminal_text(&transcript::json_window(&message)));
        self.state.menu = None;
        self.state.detail_actions = Some(Menu {
            title: format!("{} · immutable accepted input", message.message_id),
            selected: 0,
            items: vec![(
                "Cancel this input".into(),
                Action::CancelMessage(message.message_id),
            )],
        });
        self.state
            .notice("Immutable accepted body · Enter opens cancellation");
    }

    fn live_fact(&mut self, fact: &SessionFact) {
        self.live_transcript
            .as_mut()
            .unwrap_or(&mut self.state.transcript)
            .apply(fact);
    }

    fn follow_live(&mut self) {
        self.state.fold_focus = None;
        if let Some(live) = self.live_transcript.take() {
            self.state.transcript = live;
            self.state.apply_folds();
            self.state.selection = None;
            self.state.focused = 0;
        }
        self.state.top = None;
        self.history.revision = self
            .history
            .revision
            .checked_add(1)
            .expect("history revision exhausted");
        self.history.loading = false;
        self.history.backfill = false;
        self.history.before = None;
    }

    fn copy(&mut self, view: &render::View) {
        if self.tasks.len() >= 8 {
            self.state.notice("Copy queue is busy");
            return;
        }
        if let Some(menu) = &self.state.menu {
            if let Some((_, Action::Attach(id))) = menu.items.get(menu.selected) {
                let text = id.to_string();
                self.spawn(async move { Ok(Update::Copy(clipboard::copy(text).await)) });
            }
            return;
        }
        if self.state.ui_edit.is_some() || self.state.answer.is_some() {
            return;
        }
        if let Some(detail) = &self.state.detail {
            let text = super::terminal_text(detail);
            if text.len() > transcript::MAX_TEXT {
                self.state.notice("Detail exceeds the 4 MiB copy limit");
                return;
            }
            self.spawn(async move { Ok(Update::Copy(clipboard::copy(text).await)) });
            return;
        }
        let Some((a, b)) = self.state.selection else {
            return;
        };
        if !view.belongs_to(&self.state) || view.0.choice_revision != self.state.view_revision {
            self.state
                .info("Copy will be available after the current frame is displayed");
            return;
        }
        match view.0.selected_source(&self.state.transcript, a, b) {
            Ok(text) => self.spawn(async move { Ok(Update::Copy(clipboard::copy(text).await)) }),
            Err(message) => self.state.notice(message),
        }
    }

    #[allow(clippy::too_many_lines)] // Central dispatch keeps the finite UI actions and authority checks visible together.
    fn select_model(&mut self, selection: rsi_agent_session_protocol::ModelSelection) {
        let controller = self.controller.clone();
        let command = self.command.clone();
        self.spawn(async move {
            let id = rsi_agent_session_protocol::DomainRequestId::new(
                rsi_ui::fresh_identity("model-selection").map_err(error)?,
            )
            .map_err(error)?;
            let arguments = rsi_agent_session_protocol::CommandArguments::new(
                serde_json::to_value(&selection).map_err(error)?,
            )
            .map_err(error)?;
            command
                .execute(&controller, "model-selection", arguments, id)
                .await
                .map_err(error)?;
            Ok(Update::ModelSelected(selection))
        });
    }

    #[allow(clippy::too_many_lines)] // Exhaustive controller actions share the same attachment and outcome guards.
    fn action(&mut self, action: Action) -> bool {
        self.state.clear_info();
        if self.tasks.len() >= 8 && !matches!(action, Action::Exit | Action::Retry) {
            self.state
                .notice("Client requests are busy; try again shortly");
            return false;
        }
        if matches!(action, Action::UiEdit(..) | Action::UiInvoke(..)) {
            self.ui_action(action);
            return false;
        }
        self.state.close_ui();
        let handle = self.handle.clone();
        let application = self.application.clone();
        self.state.invalidate_detail();
        self.extension_view = None;
        match action {
            action @ (Action::References
            | Action::ReferenceSources(_)
            | Action::CaptureReference(_)
            | Action::PreviewReference(..)
            | Action::AddReference(_)
            | Action::RemoveReference(_)) => self.reference_action(action),
            Action::HistoryRequest(request) => self.history_request(*request),
            Action::FilePicker(request) => self.file_picker(request),
            Action::InsertFile(locator) => self.insert_file(&locator),
            Action::Help => self.state.slash.open_help(),
            Action::Terminals => self.terminal_menu(None, false),
            Action::TerminalStatus(terminal) => self.terminal_status(terminal),
            Action::CloseTerminal(id) => self.terminal_menu(Some(id), false),
            Action::CloseTerminals => self.terminal_menu(None, true),
            Action::Jobs(request) => self.job_menu(request),
            Action::Preview(request) => {
                self.state
                    .open_detail("Reading live command output…".into());
                self.state.preview = Some(job_preview::Preview::new(request));
                self.poll_preview(tokio::time::Instant::now());
            }
            Action::Plugins(command) => self.plugins(command),
            Action::Profiles => self.setup_command = Some(setup::Command::Profiles),
            Action::ExternalOpen(id) => self.setup_command = Some(setup::Command::ExternalOpen(id)),
            Action::External => self.setup_command = Some(setup::Command::External),
            Action::Attention => self.setup_command = Some(setup::Command::Attention),
            Action::IntegrationCredential(target) => self.integration_credential = Some(target),
            Action::Login => self.setup_command = Some(setup::Command::Login(None)),
            Action::SetupModels => self.setup_command = Some(setup::Command::Models),
            Action::RecallPrompt(id) => self.recall_prompt(id),
            Action::UiSurface(reference) => self.ui_surface(&reference),
            Action::UiCard => self.ui_card(),
            Action::UiEdit(..) | Action::UiInvoke(..) => {
                unreachable!("UI edit/actions dispatched above")
            }
            Action::Commands => self.command_menu(),
            Action::CommandResult => self.command_result(),
            Action::Extensions => self.extension_menu(),
            Action::Extension(producer) => self.extension_detail(&producer),
            Action::CommandHelp(name, description) => {
                self.state.open_detail(format!("/{name}\n{description}"));
                self.state
                    .notice("Type the command in the composer and press Enter");
            }
            Action::Exit => return true,
            Action::Retry => self.submit(MessageDelivery::NextTurn, true),
            Action::Submission => {
                if let Some(request) = &self.submission.request {
                    let body = request
                        .content
                        .iter()
                        .filter_map(|input| match input {
                            MessageInput::Text { text } => Some(text.clone()),
                            MessageInput::Reference { reference } => Some(format!(
                                "Reference {} · through record {}\n{}",
                                reference.metadata.source,
                                reference.metadata.through_seq(),
                                reference.preview
                            )),
                            MessageInput::Image { .. } => None,
                        })
                        .collect::<Vec<_>>()
                        .join("\n");
                    self.state.open_detail(super::terminal_text(&body));
                    let mut items = vec![(
                        "Retry the exact same input and identity".into(),
                        Action::Retry,
                    )];
                    if self.submission.rejected {
                        items.push((
                            "Discard this rejected input; keep editor draft".into(),
                            Action::DiscardRejected,
                        ));
                    }
                    self.state.detail_actions = Some(Menu {
                        title: format!("{} · Enter actions · Ctrl+Y copy", request.message_id),
                        selected: 0,
                        items,
                    });
                } else {
                    self.state.info("No unresolved submission");
                }
            }
            Action::DiscardRejected => {
                if self.submission.rejected {
                    self.submission.request = None;
                    self.submission.rejected = false;
                    self.state.escape();
                }
            }
            Action::New | Action::Attach(_) if self.submission.request.is_some() => self
                .state
                .notice("Resolve the outstanding submission before changing sessions"),
            Action::New if self.drafts.len() >= 64 => self.state.notice(
                "64 saved Sessions retained; open an existing Session or restart this application",
            ),
            Action::Attach(ref id) if self.drafts.len() >= 64 && !self.drafts.contains_key(id) => {
                self.state
                    .notice("64 session drafts retained; open an existing saved session first");
            }
            Action::New => {
                let cwd = PathBuf::from(self.state.header.canonical_cwd());
                let workspace = self.workspace.clone();
                self.spawn(async move {
                    let session_id = super::generated_cli_session_id().map_err(error)?;
                    let registered =
                        select_workspace(workspace.as_ref(), &cwd)
                            .await
                            .map_err(|failure| {
                                error(format!("Workspace selection failed: {failure}"))
                            })?;
                    let handle = application
                        .create(CreateSession {
                            workspace_id: registered.id,
                            session_id,
                            agent_preset_id: None,
                        })
                        .await
                        .map_err(|failure| error(format!("Session creation failed: {failure}")))?;
                    attachment(handle, false, None)
                        .await
                        .map(|attached| Update::Attached(Box::new(attached)))
                });
            }
            Action::Attach(id) => {
                let top = self.drafts.get(&id).and_then(|saved| saved.top);
                self.spawn(async move {
                    attachment(read(|| application.attach(&id)).await?, true, top)
                        .await
                        .map(|attached| Update::Attached(Box::new(attached)))
                });
            }
            Action::Parent => {
                if let Some(origin) = self.state.header.fork_origin() {
                    return self.action(Action::Attach(origin.parent_session_id.clone()));
                }
                self.state.info("This is the root session");
            }
            Action::Todos => {
                let text = self.state.todos.as_ref().map_or_else(
                    || "Task list is not available yet".into(),
                    |list| {
                        if list.items().is_empty() {
                            return "No tasks".into();
                        }
                        list.items()
                            .iter()
                            .map(|item| {
                                format!(
                                    "{} {}",
                                    match item.status() {
                                        rsi_agent_todo::TodoStatus::Pending => "[ ]",
                                        rsi_agent_todo::TodoStatus::InProgress => "[>]",
                                        rsi_agent_todo::TodoStatus::Completed => "[x]",
                                    },
                                    item.content()
                                )
                            })
                            .collect::<Vec<_>>()
                            .join("\n")
                    },
                );
                self.state.open_detail(text);
            }
            Action::Metrics => {
                let text = self.state.metrics.as_ref().map_or_else(
                    || "Usage is not available yet".into(),
                    |metrics| {
                        request_details::metrics_text(
                            metrics,
                            !self.state.header.settings().pricing().is_empty(),
                        )
                    },
                );
                self.state.open_detail(text);
            }
            Action::TreeMetrics(refresh) => {
                self.spawn_detail(async move {
                    Ok(Update::TreeMetrics(
                        handle.tree_metrics(refresh).await.map_err(error)?,
                    ))
                });
            }
            Action::Requests(before) => {
                let handle = self.handle.clone();
                self.spawn_detail(async move {
                    let page = handle.history_before(before, 128).await.map_err(error)?;
                    let mut items = page
                        .facts
                        .iter()
                        .rev()
                        .filter_map(|fact| {
                            let SessionFactBody::ModelIntent { snapshot, .. } = fact.body() else {
                                return None;
                            };
                            let effort = snapshot
                                .language_settings
                                .as_ref()
                                .and_then(|settings| settings.effective_reasoning_effort.as_ref())
                                .map_or_else(String::new, |effort| format!(" / {effort}"));
                            Some((
                                format!("{}{} · request {}", snapshot.model, effort, fact.seq()),
                                Action::RequestSections(fact.seq()),
                            ))
                        })
                        .collect::<Vec<_>>();
                    if page.has_more
                        && let Some(first) = page.facts.first()
                    {
                        items.push((
                            "Earlier requests".into(),
                            Action::Requests(Some(first.seq())),
                        ));
                    }
                    Ok(Update::Menu(Menu {
                        title: if items.is_empty() {
                            "No requests in this history page".into()
                        } else {
                            "Inspect request".into()
                        },
                        items,
                        selected: 0,
                    }))
                });
            }
            Action::RequestSections(seq) => {
                use rsi_agent_session_protocol::EvidenceSection;
                self.state.menu = Some(Menu {
                    title: format!("Request {seq}"),
                    selected: 0,
                    items: [
                        ("Configuration", EvidenceSection::Configuration),
                        ("System instructions", EvidenceSection::System),
                        ("Tools", EvidenceSection::Tools),
                    ]
                    .into_iter()
                    .map(|(name, section)| {
                        (
                            name.into(),
                            Action::Evidence(rsi_session_protocol::EvidenceRead {
                                intent_seq: seq,
                                section,
                                offset: 0,
                                maximum_bytes: u32::try_from(transcript::WINDOW)
                                    .expect("source page fits u32"),
                            }),
                        )
                    })
                    .collect(),
                });
                self.state
                    .menu
                    .as_mut()
                    .expect("request sections")
                    .items
                    .push((
                        "Content types and sizes".into(),
                        Action::RequestManifest(seq),
                    ));
            }
            Action::RequestManifest(seq) => {
                self.spawn_detail(async move {
                    let page = handle
                        .history_before(seq.checked_add(1), 1)
                        .await
                        .map_err(error)?;
                    let fact = page
                        .facts
                        .first()
                        .filter(|fact| fact.seq() == seq)
                        .ok_or_else(|| error("Request source is unavailable"))?;
                    let SessionFactBody::ModelIntent { evidence, .. } = fact.body() else {
                        return Err(error("Source is not a model request"));
                    };
                    let text = match evidence {
                        rsi_agent_session_protocol::RequestEvidence::Available {
                            manifest, ..
                        } => format!(
                            "Request {seq} · ordinary content\n{}",
                            manifest
                                .iter()
                                .map(|item| format!(
                                    "{:?}: {} items · {} bytes",
                                    item.kind, item.count, item.bytes
                                ))
                                .collect::<Vec<_>>()
                                .join("\n")
                        ),
                        rsi_agent_session_protocol::RequestEvidence::Unavailable { reason } => {
                            format!("Request evidence unavailable: {reason:?}")
                        }
                    };
                    Ok(Update::Manifest(text))
                });
            }
            Action::Evidence(request) => {
                let handle = self.handle.clone();
                self.spawn_detail(async move {
                    Ok(Update::Evidence(
                        handle.evidence(request).await.map_err(error)?,
                    ))
                });
            }
            Action::Agents => {
                let handle = self.handle.clone();
                self.spawn(async move {
                    let snapshot = read(|| handle.inspect()).await?;
                    let items = snapshot
                        .tree
                        .descendants
                        .into_iter()
                        .map(|child| {
                            let phase = if child.status.has_open_turn {
                                "working"
                            } else if child.status.has_active_activation
                                || child.status.has_waking_message
                            {
                                "waiting"
                            } else {
                                "idle"
                            };
                            (
                                format!(
                                    "{} · {} · {phase}",
                                    child
                                        .path
                                        .segments()
                                        .iter()
                                        .map(u16::to_string)
                                        .collect::<Vec<_>>()
                                        .join("."),
                                    child.task_name
                                ),
                                Action::Attach(child.status.session_id),
                            )
                        })
                        .collect();
                    Ok(Update::Menu(Menu {
                        title: "Subagent sessions".into(),
                        items,
                        selected: 0,
                    }))
                });
            }
            Action::Recent | Action::MoreRecent => {
                if matches!(action, Action::Recent) {
                    self.recent = None;
                }
                let cursor = self.recent.clone();
                self.spawn(async move {
                    let page = read(|| application.list_recent(cursor.as_ref(), 64)).await?;
                    Ok(Update::Recent(page))
                });
            }
            Action::Queue => self.spawn(async move {
                let snapshot = read(|| handle.inspect()).await?;
                Ok(Update::Menu(Menu {
                    title: "Pending inputs".into(),
                    items: snapshot
                        .pending
                        .into_iter()
                        .map(|message| {
                            (
                                format!(
                                    "{} · {:?} → {:?}",
                                    message.message_id, message.delivery, message.target
                                ),
                                Action::Message(message),
                            )
                        })
                        .collect(),
                    selected: 0,
                }))
            }),
            Action::Message(message) => {
                self.state.menu = Some(Menu {
                    title: format!("{} · {:?}", message.message_id, message.target),
                    selected: 0,
                    items: vec![(
                        "Cancel this pending input".into(),
                        Action::CancelMessage(message.message_id.clone()),
                    )],
                });
                self.spawn_detail(async move {
                    read(|| handle.read_message(&message.message_id, message.accepted_control_seq))
                        .await
                        .map(Update::Message)
                });
            }
            Action::CancelMessage(id) => self.spawn(async move {
                let result = handle
                    .cancel(CancelTarget::Message(id), None)
                    .await
                    .map_err(error)?;
                Ok(Update::Notice(format!(
                    "Cancellation accepted: {}",
                    result.accepted
                )))
            }),
            Action::Questions => {
                self.state.menu = Some(Menu {
                    title: "Live questions".into(),
                    selected: 0,
                    items: self
                        .interactions
                        .as_ref()
                        .map_or_else(Vec::new, |snapshot| {
                            snapshot
                                .questions()
                                .iter()
                                .map(|request| {
                                    (
                                        format!("{} · {}", request.id, request.questions[0].prompt),
                                        Action::Question(request.clone()),
                                    )
                                })
                                .collect()
                        }),
                });
            }
            Action::Approvals => {
                self.state.menu = Some(Menu {
                    title: "Live approvals".into(),
                    selected: 0,
                    items: self
                        .interactions
                        .as_ref()
                        .map_or_else(Vec::new, |snapshot| {
                            snapshot
                                .approvals()
                                .iter()
                                .map(|request| {
                                    (
                                        format!("{} · {}", request.action, request.reason),
                                        Action::Approval(request.clone()),
                                    )
                                })
                                .collect()
                        }),
                });
            }
            Action::Question(request) => {
                if self
                    .interactions
                    .as_ref()
                    .is_some_and(|snapshot| snapshot.questions().contains(&request))
                {
                    self.state.answer = Some(state::Answer {
                        scroll: 0,
                        request,
                        answers: Vec::new(),
                        editor: editor::Editor::default(),
                    });
                } else {
                    self.state.notice("Question is no longer live");
                }
            }
            Action::Approval(request) => {
                if self
                    .interactions
                    .as_ref()
                    .is_some_and(|snapshot| snapshot.approvals().contains(&request))
                {
                    self.state.open_detail(transcript::json_window(&request));
                    self.state.detail_actions = Some(Menu {
                        title: format!("{} · {}", request.action, request.reason),
                        selected: 0,
                        items: vec![
                            (
                                "Deny".into(),
                                Action::Decide(
                                    request.clone(),
                                    rsi_approval_protocol::ApprovalDecision::Deny,
                                ),
                            ),
                            (
                                "Allow once".into(),
                                Action::Decide(
                                    request,
                                    rsi_approval_protocol::ApprovalDecision::AllowOnce,
                                ),
                            ),
                        ],
                    });
                    self.state
                        .notice("Review the prepared request; Enter opens Deny / Allow once");
                } else {
                    self.state.notice("Approval is no longer live");
                }
            }
            Action::Decide(request, decision) => {
                self.state.escape();
                if self
                    .interactions
                    .as_ref()
                    .is_some_and(|snapshot| snapshot.approvals().contains(&request))
                {
                    self.spawn(async move {
                        let owner = SessionId::new(request.subject.session_id()).map_err(error)?;
                        let accepted = handle
                            .answer_approval(&owner, &request.id, decision)
                            .await
                            .map_err(error)?;
                        Ok(Update::Notice(if accepted {
                            String::new()
                        } else {
                            "Approval is no longer live; reopen approvals".into()
                        }))
                    });
                } else {
                    self.state
                        .notice("Approval changed; reopen the current request");
                }
            }
            Action::Detail => {
                if let Some(block) = self.state.transcript.blocks.get(self.state.focused)
                    && block.role == transcript::Role::Metadata
                {
                    return self.action(
                        block
                            .request_seq()
                            .map_or(Action::Requests(None), Action::RequestSections),
                    );
                }
                if let Some(block) = self.state.transcript.blocks.get(self.state.focused) {
                    let mut items = block
                        .pieces
                        .iter()
                        .filter(|piece| piece.omitted)
                        .take(64)
                        .map(|piece| {
                            (
                                format!(
                                    "Fact {} · field {} · first window",
                                    piece.source.seq, piece.source.field
                                ),
                                Action::Window(piece.source, 0),
                            )
                        })
                        .collect::<Vec<_>>();
                    if let Some(tool) = &block.tool {
                        if let Some(id) = tool.external_conversation() {
                            items.push((
                                "Open external conversation".into(),
                                Action::ExternalOpen(
                                    rsi_acp_protocol::observation::ConversationId::new(id)
                                        .expect("validated delegation hint"),
                                ),
                            ));
                        }
                        if !block.completed {
                            items.push(("Live command output".into(), Action::Jobs(None)));
                        }
                        for (label, source) in [
                            ("Arguments", tool.arguments),
                            ("Result value", tool.result),
                            ("Rejection", tool.rejection),
                        ] {
                            if let Some(source) = source {
                                items.push((
                                    format!("{label} · Fact {}", source.seq),
                                    Action::Window(source, 0),
                                ));
                            }
                        }
                    }
                    for (stream, output) in ["stdout", "stderr"].into_iter().zip(&block.outputs) {
                        if let Some(output) = output {
                            items.push((
                                format!("Full {stream}"),
                                Action::Output(output.clone(), 0),
                            ));
                        }
                    }
                    self.state.open_detail(block.text());
                    if !items.is_empty() {
                        self.state.menu = Some(Menu {
                            title: "Source reads".into(),
                            selected: 0,
                            items,
                        });
                    }
                }
            }
            Action::Window(source, offset) => {
                let controller = self.controller.clone();
                let stop = self.state.detail_stop.clone();
                self.spawn_as(WorkKind::Detail, async move {
                    let window = controller
                        .source_window(source, offset, transcript::WINDOW, stop)
                        .await
                        .map_err(error)?;
                    Ok(Update::Window(transcript::Piece::from_window(
                        source, &window,
                    )))
                });
            }
            Action::Output(id, offset) => {
                let output_cache = self.output_cache.clone();
                self.spawn_detail(async move {
                    let page = output_cache
                        .read(&id, offset, 16 * 1024)
                        .await
                        .map_err(error)?;
                    Ok(Update::Output(page))
                });
            }
        }
        false
    }

    fn answer(&mut self) {
        if self.tasks.len() >= 8 {
            self.state
                .notice("Client requests are busy; answer draft retained");
            return;
        }
        let Some(answer) = &mut self.state.answer else {
            return;
        };
        if !self
            .interactions
            .as_ref()
            .is_some_and(|snapshot| snapshot.questions().contains(&answer.request))
        {
            self.state.answer = None;
            self.state
                .notice("Question was settled or withdrawn; answer was not sent");
            return;
        }
        let raw = answer.editor.text().to_owned();
        let question = &answer.request.questions[answer.answers.len()];
        let text = raw
            .trim()
            .parse::<usize>()
            .ok()
            .and_then(|i| i.checked_sub(1))
            .and_then(|i| question.options.get(i))
            .cloned()
            .unwrap_or(raw);
        if text.trim().is_empty() {
            return;
        }
        answer.answers.push(text);
        answer.scroll = 0;
        let _ = answer.editor.take();
        if answer.answers.len() == answer.request.questions.len() {
            let reply = rsi_user_questions_protocol::QuestionAnswer {
                answers: answer.answers.clone(),
            };
            if let Err(problem) = reply.validate_for(&answer.request) {
                answer.answers.pop();
                self.state.notice(problem.to_string());
                return;
            }
            let request = answer.request.clone();
            let handle = self.handle.clone();
            self.state.answer = None;
            self.spawn(async move {
                let accepted = handle
                    .answer_question(&request.id, reply)
                    .await
                    .map_err(error)?;
                Ok(Update::Notice(if accepted {
                    String::new()
                } else {
                    "Question is no longer live; reopen questions".into()
                }))
            });
        }
    }
}

#[allow(clippy::too_many_lines)] // One loop owns generation-tagged results and observer handoff.
async fn run_inner(
    presentation: Option<rsi_host::Profile>,
    services: Services,
    command: SessionCommand,
    application_work: ApplicationWork,
    context: rsi_meta::Context,
) -> Result<()> {
    let Services {
        ui,
        ui_target,
        application,
        output_cache,
        model_catalog,
        workspace,
        lifetime: mode,
    } = services;

    let selection = match command.resume {
        Some(session_id) => SessionSelection::Resume {
            session_id,
            cwd: command.cwd,
        },
        None => SessionSelection::Fresh {
            cwd: command
                .cwd
                .map_or_else(std::env::current_dir, Ok)
                .map_err(error)?,
            session_id: command.session_id,
            agent_preset_id: command.agent_preset,
        },
    };
    let mut presentation = crate::presentation::Owner::start(&context, presentation).await?;
    let mut presentation_changes = presentation.changes();
    let mut terminate = Box::pin(termination().map_err(error)?);
    let mut terminal = terminal::Terminal::enter(&application_work.tasks).map_err(error)?;
    let stopped = application_work.stop.child_token();
    let mut input = input::spawn(stopped.clone(), &application_work.tasks).map_err(error)?;
    let mut setup = setup::Ui::new(
        context.lookup_local::<rsi_workbench_ui::SetupFeatureContract>(),
        model_catalog.clone(),
    );
    let mut markdown = true;
    let mut external = external::Ui::new(&context);
    let mut profiles = profiles::Ui::new(&context);
    let startup = home::run(
        &context,
        &application,
        &workspace,
        selection,
        &mut setup,
        &mut external,
        &mut profiles,
        &mut terminal,
        &mut presentation,
        &mut input,
        &stopped,
        &mut terminate,
        model_catalog.as_ref(),
        &mut markdown,
    )
    .await;
    let (attached, draft) = match startup {
        Ok(Some(attached)) => attached,
        outcome => {
            stopped.cancel();
            external.shutdown().await;
            profiles.shutdown().await;
            let cleanup = close_rendering(terminal.close(), presentation.close()).await;
            return outcome.map(|_| ()).and(cleanup);
        }
    };
    let (events, mut receiver) = mpsc::channel(super::CLI_RENDER_CHANNEL_CAPACITY);
    let surfaces = crate::surfaces::TerminalSurfaces::start(&context, &events).await?;
    let mut observer = Some(
        surfaces
            .open(
                attached.header.session_id(),
                attached
                    .inspection
                    .as_ref()
                    .map(|snapshot| ObservationCursor {
                        fact_seq: snapshot.durable_fact_seq,
                        control_seq: snapshot.durable_control_seq,
                    }),
                0,
            )
            .await?,
    );
    let mut client = Client::new(
        Services {
            application,
            output_cache,
            model_catalog,
            workspace,
            lifetime: mode,
            ui,
            ui_target,
        },
        attached,
        observer
            .as_ref()
            .expect("initial surface")
            .controller
            .clone(),
        observer
            .as_ref()
            .expect("initial surface")
            .ui_target
            .clone()
            .ok_or_else(|| error("TUI surface target is unavailable"))?,
    );
    client.text_history = context
        .lookup_local::<rsi_api_protocol::ApiClientContract>()
        .and_then(|api| rsi_history_api::Client::new(api).ok());
    client.plugins = context.lookup_local::<rsi_workbench_ui::PluginsFeatureContract>();
    client.files = observer.as_ref().expect("initial surface").files.clone();
    let mut ui_changes = client.ui.registry.membership_changes();
    client.state.editor = draft;
    client.state.markdown = markdown;
    client.refresh_metrics();
    let mut dimensions = terminal::size();
    let mut view = render::View::default();
    let mut scene_capture = rsi_terminal_ui::scene::SceneCapture::default();
    let mut frame_revision = terminal
        .frames
        .borrow()
        .as_ref()
        .map_or(0, |frame| frame.revision);
    let mut presentation_epoch = terminal
        .frames
        .borrow()
        .as_ref()
        .map_or(1, |frame| frame.presentation + 1);
    let mut rendering: Option<RenderJob> = None;
    let mut rendering_stop = stopped.child_token();
    let mut tick = tokio::time::interval(Duration::from_millis(33));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut inspect_tick = tokio::time::interval(Duration::from_secs(1));
    inspect_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut dirty = true;
    let mut recovery = render::Recovery::default();
    let mut navigation_active = (external.active, profiles.active);
    let result: Result<()> = async {
        loop {
            client.enforce_fold_budget();
            if let Some(id) = external.native.take() {client.action(Action::Attach(id));dirty=true;}
            if external.focus.as_ref().is_some_and(|(position,target)| matches!(&position.conversation,rsi_conversation::ConversationIdentity::Native(id) if id==client.state.header.session_id()) && (target.is_none() || client.interactions.is_some())) {
                let (position,target)=external.focus.take().expect("selected attention");
                if let Some(target)=target {
                    let action=client.interactions.as_ref().and_then(|snapshot|match target {
                        rsi_session_protocol::ActivityRequest::Approval {turn,request} => snapshot.approvals().iter().find(|p|p.id==request && p.subject.turn_id()==turn.as_str() && p.subject.session_id()==client.state.header.session_id().as_str()).cloned().map(Action::Approval),
                        rsi_session_protocol::ActivityRequest::Question {turn,request} => snapshot.questions().iter().find(|p|p.id==request && p.turn_id==turn.as_str() && p.session_id==client.state.header.session_id().as_str()).cloned().map(Action::Question),
                    });
                    if let Some(action)=action {client.action(action);}else {client.state.notice("Request is no longer pending");}
                }
                let acknowledgment=external.acknowledge(position);
                client.spawn(async move {acknowledgment.await.map_err(error)?;Ok(Update::AttentionRead)});dirty=true;
            }
            if let Some(target) = client.integration_credential.take()
                && let Some(feature) = client.plugins.clone() { setup.open_integration(target, feature); }
            if let Some(command) = client.setup_command.take() {
                match command { setup::Command::Export(options) => {
                    let handle = client.handle.clone();
                    client.state.info("Exporting session…");
                    client.spawn(async move { let path = crate::export::save(handle, options).await.map_err(error)?; Ok(Update::Notice(format!("Exported {}", path.display()))) });
                }, setup::Command::History(conversation,query) => client.search_history(conversation,query), setup::Command::Profiles => profiles.open(), setup::Command::ExternalOpen(id) => external.open_conversation(id), setup::Command::Attention => external.open_attention(), setup::Command::External => external.open(), setup::Command::Markdown(mode) => { client.state.markdown = mode.unwrap_or(!client.state.markdown); client.state.info(markdown_status(client.state.markdown)); }, setup::Command::Plugins => client.plugins(rsi_workbench_ui::PluginsCommand::Refresh), setup::Command::Effort => setup.open_effort(rsi_agent_session_protocol::ModelSelection { model: client.state.model.clone().unwrap_or_else(|| client.state.header.settings().default_model().clone()), reasoning_effort: client.state.reasoning_effort.clone() }), setup::Command::Quit => break, setup::Command::Help => client.state.slash.open_help(), setup::Command::New => {client.action(Action::New);}, setup::Command::Resume(id) => {client.action(id.map_or(Action::Recent, Action::Attach));}, setup::Command::Reference(id) => {client.action(id.map_or(Action::References, Action::CaptureReference));}, command => setup.open(command, true) }
                dirty = true;
            }
            if navigation_active != (external.active, profiles.active) {
                navigation_active = (external.active, profiles.active);
                rendering_stop.cancel(); rendering.take();
                rendering_stop = stopped.child_token();
                presentation_epoch = presentation_epoch.checked_add(1).ok_or_else(||error("presentation epoch exhausted"))?;
                view = render::View::default(); dirty = true;
            }
            if profiles.active || external.active || client.state.menu.is_some() || client.state.answer.is_some() || client.state.ui_edit.is_some() || client.state.detail.is_some() || setup.active {client.state.slash.hide();}
            else {client.state.slash.update(&client.state.editor, Some(&client.controller));}
            if let Some(selection) = setup.chosen.take() { client.select_model(selection); dirty = true; }
            client.state.busy = client.submission.busy;
            client.state.editor.set_retention_limit((4 * 1024 * 1024usize).saturating_sub(client.draft_reference_retention()));
            if let Some(answer) = &mut client.state.answer { answer.editor.limit = rsi_user_questions_protocol::MAXIMUM_QUESTION_BYTES.saturating_sub(answer.answers.iter().map(String::len).sum()); }
            tokio::select! {
                () = application_work.stop.cancelled() => break,
                () = external.next() => {dirty=true;},
                () = profiles.next() => {dirty=true;},
                () = setup.next() => { if !setup.active {client.state.notice(setup.notice());} dirty = true; },
                () = client.state.slash.next() => { if !client.state.slash.diagnostic.is_empty() {client.state.notice(client.state.slash.diagnostic.clone());} dirty = true; },
                change = presentation_changes.changed() => {
                    change.map_err(error)?;
                    rendering_stop.cancel(); rendering.take();
                    rendering_stop = stopped.child_token();
                    presentation_epoch = presentation_epoch.checked_add(1).ok_or_else(||error("presentation epoch exhausted"))?;
                    view = render::View::default(); dirty = true; recovery.reset();
                },
                (request, result) = async {match &mut rendering {Some(job)=>job.await,None=>std::future::pending().await}} => {
                    rendering.take();
                    if request.identity.attachment != client.generation || request.identity.presentation != presentation_epoch || (request.width,request.height) != terminal::size() { dirty = true; continue; }
                    let result = result.and_then(|(buffer,next_view)| {
                        if next_view.sources_belong_to(client.state.header.session_id(), &client.state.transcript) { Ok((buffer,next_view)) }
                        else { Err("Renderer returned a stale source map".into()) }
                    });
                    match result {
                        Ok((buffer,next_view)) => {
                            recovery.reset();
                            terminal.frames.send_replace(Some(Arc::new(terminal::RenderedFrame {generation:client.generation, presentation:presentation_epoch, revision:request.identity.revision,buffer,view:render::View(next_view)})));
                        }
                        Err(problem) => {
                            client.state.notice(problem); dirty = true; recovery.failed();
                            let frame = render::failure_frame(request.identity, ratatui::layout::Rect::new(0, 0, request.width, request.height), terminal.frames.borrow().as_deref(), &client.state.status);
                            terminal.frames.send_replace(Some(Arc::new(frame)));
                        }
                    }
                },
                changed = ui_changes.changed() => {
                    if changed.is_ok() { client.ui_changed(); dirty = true; }
                },
                () = &mut terminate => break,
                changed = terminal.presented.changed() => {
                    changed.map_err(error)?;
                    if let Some(frame) = terminal.presented.borrow_and_update().as_ref() {
                        view = if frame.generation == client.generation && frame.presentation == presentation_epoch {
                            frame.view.clone()
                        } else {
                            render::View::default()
                        };
                        client.state.slash.presented(&view);
                    }
                },
                incoming = input.recv() => {
                    if terminal.presented.borrow().as_ref().is_none_or(|frame| frame.generation != client.generation || frame.presentation != presentation_epoch || (frame.buffer.area.width,frame.buffer.area.height)!=terminal::size()) {
                        view = render::View::default();
                    }
                    dirty = true;
                    match incoming.unwrap_or(input::Input::Closed) {
                        input::Input::Closed => break,
                        input::Input::Rejected(message) => client.state.notice(message),
                        input::Input::Terminal(termina::Event::Paste(text)) => {
                            client.state.clear_info();
                            if profiles.active {profiles.paste(&text);continue;}
                            if external.active {external.paste(&text);continue;}
                            if setup.active { setup.paste(text); continue; }
                            if client.state.slash.paste(&text) {continue;}
                            if client.state.menu.is_some() { continue; }
                            if client.state.ui_paste(&text) { continue; }
                            if client.state.detail.is_some() { continue; }
                            if let Err(message) = client.state.answer.as_mut().map_or(&mut client.state.editor, |answer| &mut answer.editor).insert(&text) { client.state.notice(message); }
                        },
                        input::Input::Terminal(termina::Event::Key(key)) if key.kind != KeyEventKind::Release => {
                            client.state.clear_info();
                            if profiles.active {profiles.key(key);continue;}
                            if external.active {external.key(key);continue;}
                            if setup.active { setup.key(key); if !setup.active { client.state.notice(setup.notice()); } continue; }
                            let control = key.modifiers.contains(Modifiers::CONTROL);
                            if client.state.menu.is_none() && client.state.answer.is_none() && client.state.ui_edit.is_none() && client.state.detail.is_none() && client.state.slash.key(key, &mut client.state.editor) { continue; }
                            if control && key.code == KeyCode::Char('c') { if client.state.has_dialog() { client.state.escape(); } else { client.cancel(); } continue; }
                            if control && key.code == KeyCode::Char('y') { client.copy(&view); continue; }
                            if key.code == KeyCode::Escape { client.state.escape(); continue; }
                            if control && key.code == KeyCode::Char('p') { client.action_menu(); continue; }
                            if control && key.code == KeyCode::Char('r') && !client.state.has_dialog() { client.prompt_menu(); continue; }
                            if key.code == KeyCode::Char('@') && key.modifiers.contains(Modifiers::ALT) && !control && !client.state.has_dialog() && client.state.editor.text()[..client.state.editor.cursor()].chars().next_back().is_none_or(char::is_whitespace) {
                                if let Err(problem) = client.state.editor.insert("@") {client.state.notice(problem);} else {client.action(Action::FilePicker(rsi_session_files_ui::FilePickerRequest::Open {path:rsi_files_protocol::RelativePath::default(),file_kind:rsi_files_protocol::FileKind::Directory}));} continue;
                            }
                            if control && key.code == KeyCode::Char('d') && client.state.references.is_empty() && client.state.editor.text().is_empty() && client.state.answer.is_none() && client.state.ui_edit.is_none() && client.submission.request.is_none() { break; }
                            if let Some(menu) = &mut client.state.menu {
                                match key.code {
                                    KeyCode::Up => menu.selected = menu.selected.saturating_sub(1),
                                    KeyCode::Down => menu.selected = (menu.selected+1).min(menu.items.len().saturating_sub(1)),
                                    KeyCode::Enter => { let action = menu.items.get(menu.selected).map(|(_,action)| action.clone()); client.state.menu = None; if action.is_some_and(|action| client.action(action)) { break; } },
                                    _ => {},
                                }
                            } else if client.state.ui_key(key) {}
                            else if key.code == KeyCode::PageUp { client.scroll(&view, true); }
                            else if key.code == KeyCode::PageDown { client.scroll(&view, false); }
                            else if key.code == KeyCode::End && !control && !client.state.has_dialog() { client.follow_live(); }
                            else if key.code == KeyCode::End && client.state.detail.is_some() { client.state.detail_offset = view.0.scroll_max; }
                            else if client.state.detail.is_some() && matches!(key.code, KeyCode::Up | KeyCode::Down) { client.scroll(&view, key.code == KeyCode::Up); }
                            else if client.state.detail.is_some() && key.code == KeyCode::Enter { client.state.menu.clone_from(&client.state.detail_actions); }
                            else if client.state.detail.is_some() && matches!(key.code, KeyCode::Right | KeyCode::Left) {
                                let action = if key.code == KeyCode::Right { client.state.detail_next.clone() } else { client.state.detail_previous.clone() };
                                if let Some(action) = action { client.action(action); }
                            }
                            else if client.state.detail.is_some() { }
                            else if key.code == KeyCode::Tab && client.state.answer.is_none() { client.state.fold_focus = None; client.retain_visible_top(&view); client.state.focus_next_source(); }
                            else if key.code == KeyCode::Enter && !key.modifiers.contains(Modifiers::SHIFT) {
                                if client.state.answer.is_some() { client.answer(); }
                                else { client.submit(MessageDelivery::NextTurn, false); }
                            } else if control && key.code == KeyCode::Char('s') && client.state.answer.is_none() { client.submit(MessageDelivery::NextTurn, false);
                            } else if control && key.code == KeyCode::Char('o') && client.state.answer.is_none() { client.submit(MessageDelivery::Steer, false); }
                            else if let Err(message) = client.state.answer.as_mut().map_or(&mut client.state.editor, |answer| &mut answer.editor).key(key) { client.state.notice(message); }
                        },
                        input::Input::Terminal(termina::Event::Mouse(mouse)) if profiles.active => {profiles.mouse(mouse,&view.0);},
                        input::Input::Terminal(termina::Event::Mouse(mouse)) if external.active => {external.mouse(mouse,&view.0);},
                        input::Input::Terminal(termina::Event::Mouse(mouse)) if setup.active => {setup.mouse(mouse,&view.0);},
                        input::Input::Terminal(termina::Event::Mouse(mouse)) if client.state.slash.mouse(mouse, &view.0) => {},
                        input::Input::Terminal(termina::Event::Mouse(mouse)) => match mouse.kind {
                            _ if client.state.has_dialog() => {
                                if view.0.dialog.is_none_or(|area| !area.contains((mouse.column, mouse.row).into())) { continue; }
                                if let Some(menu) = &mut client.state.menu {
                                    match mouse.kind {
                                        MouseEventKind::ScrollUp => menu.selected = menu.selected.saturating_sub(1),
                                        MouseEventKind::ScrollDown => menu.selected = (menu.selected+1).min(menu.items.len().saturating_sub(1)),
                                        MouseEventKind::Down(MouseButton::Left) if view.0.choice_revision == client.state.view_revision => {
                                            if let Some(index) = view.0.choice_at(mouse.column, mouse.row) && index < menu.items.len() { menu.selected = index; }
                                        },
                                        _ => {},
                                    }
                                } else if matches!(mouse.kind, MouseEventKind::ScrollUp | MouseEventKind::ScrollDown) { client.scroll(&view, mouse.kind == MouseEventKind::ScrollUp); }
                            },
                            MouseEventKind::ScrollUp => client.scroll(&view, true), MouseEventKind::ScrollDown => client.scroll(&view, false),
                            MouseEventKind::Down(MouseButton::Left) => {
                                if view.belongs_to(&client.state) && let Some((_, action)) = view.0.footer.iter().find(|(area, _)| mouse.column >= area.x && mouse.column < area.right() && mouse.row == area.y) {
                                    match action { rsi_terminal_ui::render::FooterAction::Parent => { client.action(Action::Parent); }, rsi_terminal_ui::render::FooterAction::Model => setup.open(setup::Command::Models, true) }
                                    continue;
                                }
                                if client.toggle_fold_at(&view, mouse.column, mouse.row) { continue; }
                                if view.belongs_to(&client.state) && let Some(anchor) = view.hit(mouse.column, mouse.row, false) { client.state.fold_focus = None; client.state.top = view.0.location(client.state.header.session_id(), &client.state.transcript, 0).and_then(|(index, offset)| client.state.transcript.blocks[index].anchor(offset)); client.state.selection = Some((anchor,anchor)); client.state.focused = client.state.transcript.locate(anchor).map_or(0, |(index,_)| index); }
                            },
                            MouseEventKind::Drag(MouseButton::Left) => if view.belongs_to(&client.state) && let Some(anchor) = view.hit(mouse.column, mouse.row, true) && let Some((_,end)) = &mut client.state.selection { *end = anchor; },
                            MouseEventKind::Up(MouseButton::Left) => client.copy(&view), _ => {},
                        },
                        input::Input::Terminal(_) => {},
                    }
                },
                Some(work) = client.tasks.next(), if !client.tasks.is_empty() => {
                    if work.superseded(&client) { continue; }
                    dirty = true;
                    match work.kind {
                        WorkKind::Inspect => client.inspecting = false,
                        WorkKind::History => client.history.loading = false,
                        WorkKind::Cancel => client.cancelling = false,
                        WorkKind::Read | WorkKind::Detail | WorkKind::Submit => {},
                    }
                    if work.view_revision != client.state.view_revision && matches!(&work.result, Ok(Update::HistorySearch(..) | Update::Ui(_) | Update::Menu(_) | Update::Recent(_) | Update::Message(_) | Update::Window(_) | Update::Output(_) | Update::Attached(_))) { continue; }
                    match work.result {
                        Ok(Update::Ui(view)) => client.show_ui(view),
                        Ok(Update::Command(result)) => { client.command_finished(result); client.state.slash.invalidate(); },
                        Err(problem) => {
                            if matches!(work.kind, WorkKind::Detail) { client.ui_failed(); }
                            if matches!(work.kind, WorkKind::History) { client.history.backfill = false; }
                            client.state.notice(problem.to_string());
                        },
                        Ok(Update::Notice(message)) => client.state.info(message),
                        Ok(Update::ModelSelected(selection)) => {
                            client.state.info(format!("Switched to {} · {}", selection.model.model(), selection.reasoning_effort.as_ref().map_or("default", |effort| effort.as_str())));
                            client.refresh_metrics();
                        },
                        Ok(Update::Copy(delivery)) => {
                            if delivery.failed { client.state.notice(delivery.status); }
                            else if !delivery.status.is_empty() { client.state.info(delivery.status); }
                            if let Some(osc) = delivery.osc && terminal.commands.try_send(osc).is_err() { client.state.notice("Copy failed: terminal command queue is busy"); }
                        },
                        Ok(Update::Menu(menu)) => client.state.menu = Some(menu),
                        Ok(Update::Plugins(view)) => client.state.show_plugins(*view),
                        Ok(Update::HistorySearch(request,reply)) => client.show_history(&request,*reply),
                        Ok(Update::Reference(page)) => client.state.show_reference(page),
                        Ok(Update::FilePicker(page)) => client.state.show_file_picker(page),
                        Ok(Update::Recent(page)) => {
                            client.recent = page.sessions.last().map(rsi_session_protocol::SessionSummary::cursor);
                            let mut items = page.sessions.into_iter().map(|summary| (format!("{} · {}", summary.header.session_id(), summary.header.canonical_cwd()), Action::Attach(summary.header.session_id().clone()))).collect::<Vec<_>>();
                            if page.has_more { items.push(("More sessions…".into(), Action::MoreRecent)); }
                            client.state.menu = Some(Menu { title: "Recent sessions".into(), selected: 0, items });
                        },
                        Ok(Update::Message(message)) => {
                            client.message_detail(message);
                        },
                        Ok(Update::Output(page)) => {
                            client.state.open_detail(format!("Completed output {} · bytes {}..{} / {}\n{}", page.id, page.offset, page.next_offset, page.total_bytes, super::terminal_text(&String::from_utf8_lossy(&page.bytes))));
                            client.state.detail_previous = (page.offset > 0).then(|| Action::Output(page.id.clone(), page.offset.saturating_sub(16*1024)));
                            client.state.detail_next = (page.next_offset < page.total_bytes).then(|| Action::Output(page.id, page.next_offset));
                            client.state.notice("Output page · ←/→ pages · ↑/↓ scroll · Ctrl+Y copies this displayed page");
                        },
                        Ok(Update::AttentionRead) => {},
                        Ok(Update::Window(piece)) => {
                            let source = piece.source; let next = piece.anchor(piece.text.len()).offset;
                            client.state.open_detail(piece.text);
                            client.state.detail_previous = (piece.start > 0).then_some(Action::Window(source, piece.start.saturating_sub(transcript::WINDOW)));
                            client.state.detail_next = (next > piece.start && piece.truncated_after).then_some(Action::Window(source, next));
                            client.state.notice(format!("Fact {} field {} · bytes {}..{next} · ←/→ source windows · Ctrl+Y copies this window", source.seq, source.field, piece.start));
                        },
                        Ok(Update::Evidence(page)) => { request_details::show_evidence(&mut client.state,page); },
                        Ok(Update::Manifest(text)) => client.state.open_detail(text),
                        Ok(Update::TreeMetrics(read)) => request_details::show_tree_metrics(&mut client.state,&read),
                        Ok(Update::Preview(page)) => { job_preview::show(&mut client.state,page); },
                        Ok(Update::Inspect(snapshot)) => {
                            client.inspecting = false; client.state.active = snapshot.active_turn_id.is_some(); client.state.live_turn.clone_from(&snapshot.active_turn_id);
                            let pending: BTreeSet<_> = snapshot.pending.iter().map(|message| message.message_id.clone()).collect();
                            client.owned.retain(|id| pending.contains(id) || client.submission.request.as_ref().is_some_and(|request| request.message_id == *id));
                            client.inspection = Some(*snapshot);
                            client.refresh_metrics();
                        },
                        Ok(Update::Metrics(result)) => {
                            let result = *result;
                            client.metrics_loading = false;
                            if let Ok(metrics) = result {
                                let complete = metrics.complete;
                                client.state.metrics = Some(metrics);
                                if !complete { client.refresh_metrics(); }
                            } else { client.state.metrics = None; }
                        },
                        Ok(Update::History(page)) => client.page(page),
                        Ok(Update::Submitted(receipt)) => {
                            client.submission.busy = false;
                            match receipt {
                                Ok(_receipt) => {
                                    client.state.clear_info();
                                    client.submission.request = None;
                                    client.durability = Durability::Durable; client.state.slash.invalidate();
                                    if client.submission.cancel_when_accepted { client.submission.cancel_when_accepted = false; client.cancel(); }
                                    client.inspect();
                                },
                                Err(problem) => {
                                    client.submission.rejected = !matches!(problem, SessionError::MessageOutcomeUnknown {..});
                                    if client.submission.rejected {
                                        if let Some(request) = &client.submission.request { client.owned.remove(&request.message_id); }
                                        client.submission.cancel_when_accepted = false;
                                        if client.state.editor.text().is_empty()
                                            && let Some(request) = client.submission.request.take()
                                            && let Some(MessageInput::Text { text }) = request.content.into_iter().next() { client.state.editor = editor::Editor::with_text(text, input::MAX_TEXT); }
                                    }
                                    client.state.notice(format!("Submission failed: {problem}. Input retained in the editor or Actions → Pending / rejected submission."));
                                },
                            }
                        },
                        Ok(Update::Attached(attached)) => {
                            if client.submission.request.is_some() { client.state.notice("Resolve the outstanding submission before changing sessions"); continue; }
                            let next = match surfaces.open(attached.header.session_id(), attached.inspection.as_ref().map(|snapshot| ObservationCursor { fact_seq: snapshot.durable_fact_seq, control_seq: snapshot.durable_control_seq }), client.generation + 1).await {
                                Ok(next) => next,
                                Err(problem) => { client.state.notice(format!("Session observation failed: {problem}")); continue; },
                            };
                            if let Some(observer) = observer.take() { observer.stop().await?; }
                            let setup_was_active=setup.active;
                            setup.attachment_changed();
                            client.controller = next.controller.clone();
                            client.files.clone_from(&next.files);
                            client.ui.surface = next.ui_target.clone().ok_or_else(|| error("TUI surface target is unavailable"))?;
                            client.projections = None; client.projection_notice.clear(); client.extension_view = None;
                            observer = Some(next);
                            client.tasks.clear(); client.generation += 1;
                            client.handle = attached.handle;
                            client.switch_session_draft(attached.header);
                            if setup_was_active {client.state.notice(format!("Session changed. {}",setup.notice()));}
                            client.cancelling = false; client.cancellation_queued = false; client.interactions = None; client.inspection = attached.inspection; client.durability = if client.inspection.is_some() { Durability::Durable } else { Durability::Draft }; client.history.before = None; client.live_transcript = attached.live_page.map(live_window);
                            client.state.active = client.inspection.as_ref().is_some_and(|snapshot| snapshot.active_turn_id.is_some()); client.state.live_turn = client.inspection.as_ref().and_then(|snapshot| snapshot.active_turn_id.clone());
                            client.history.loading = false; client.inspecting = false; client.metrics_loading = false; client.history.pages = 0; client.history.bytes = 0; client.history.backfill = true;
                            if let Some(page) = attached.page { client.page(page); }
                            client.refresh_metrics();
                        },
                    }
                },
                Some(event) = receiver.recv() => {
                    let event = match event {
                        CliRenderMessage::Observed { generation, event } if generation == client.generation => CliRenderMessage::Event(event),
                        CliRenderMessage::Observed { .. } => continue,
                        event => event,
                    };
                    dirty = true;
                    match event {
                        CliRenderMessage::Event(CliEvent::Fact { fact, .. }) => {
                            client.state.model_fact(&fact);
                            match fact.body() { SessionFactBody::TurnAccepted {turn_id,..} | SessionFactBody::MessageTurnAccepted {turn_id,..} => { client.state.active = true; client.state.live_turn = Some(turn_id.clone()); }, SessionFactBody::TurnTerminal {..} => { client.state.active = false; client.state.live_turn = None; client.inspect(); }, _ => {} }
                            client.live_fact(&fact);
                        },
                        CliRenderMessage::Event(CliEvent::Projections { snapshot }) => {
                            if let Some(value) = snapshot.snapshot().entries().iter().find(|entry| entry.producer().as_str() == "rsi.model-selection.view").and_then(|entry| entry.view())
                                && let Ok(selection) = serde_json::from_value::<rsi_agent_session_protocol::ModelSelection>(value.value().clone()) {
                                    client.state.model = Some(selection.model);
                                    client.state.reasoning_effort = selection.reasoning_effort;
                            }
                            client.state.todos = snapshot.snapshot().entries().iter().find(|entry| entry.producer().as_str() == rsi_agent_todo::VIEW)
                                .and_then(|entry| entry.view()).and_then(|value| serde_json::from_value(value.value().clone()).ok());
                            client.projections = Some(snapshot); client.projection_notice.clear();
                            client.refresh_extensions();
                        },
                        CliRenderMessage::Event(CliEvent::Interactions { snapshot }) => {
                            client.state.questions = snapshot.questions().len(); client.state.approvals = snapshot.approvals().len();
                            if client.state.answer.as_ref().is_some_and(|answer| !snapshot.questions().contains(&answer.request)) { client.state.answer = None; client.state.notice("Question settled or withdrawn; answer draft closed"); }
                            client.interactions = Some(snapshot);
                        },
                        CliRenderMessage::Event(CliEvent::Control { record, .. }) => { client.state.slash.invalidate(); match record.body() {
                            AgentControlRecordBody::MessagePromoted { message_id } if client.owned.contains(message_id) => client.state.info("Queued input will start the next turn"),
                            _ => {},
                        } },
                        CliRenderMessage::Event(CliEvent::Notice { kind, value }) => {
                            let notice = format!("{kind}: {}", transcript::json_window(&value));
                            if matches!(kind, "projection_reconnecting" | "projection_stopped") { client.projection_notice.clone_from(&notice); client.refresh_extensions(); }
                            client.state.notice(notice);
                        },
                        _ => {},
                    }
                },
                problem = super::session_cli::observer_finished(&mut observer), if observer.is_some() => {
                    observer.take(); client.state.notice(problem.to_string()); dirty = true;
                },
                _ = inspect_tick.tick(), if observer.is_some() && client.durability == Durability::Durable => client.inspect(),
                _ = tick.tick() => {
                    let now_ms = u64::try_from(std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_millis()).unwrap_or(u64::MAX);
                    dirty |= client.state.tick_activity(now_ms);
                    client.poll_preview(tokio::time::Instant::now());
                    if client.cancellation_queued && !client.cancelling && client.tasks.len() < 12 { client.cancel(); }
                    if terminal.failed() { return Err(error("Terminal writer stopped")); }
                    let (width,height) = terminal::size();
                    if dimensions != (width,height) { dimensions=(width,height); dirty=true; }
                    if dirty && recovery.ready() && rendering.is_none() {
                        frame_revision = frame_revision.checked_add(1).ok_or_else(|| error("Terminal frame revision exhausted"))?;
                        let scene = match (if profiles.active {profiles.scene()} else if external.active {external.scene()}else{scene_capture.capture(&render::input(&client.state),width,height)}).and_then(|scene| { if setup.active { scene.with_dialog(setup.scene()?) } else if client.state.slash.help { scene.with_dialog(client.state.slash.scene()?) } else { Ok(scene) } }) {
                            Ok(scene) => scene,
                            Err(problem) => {
                                client.state.notice(problem); recovery.failed();
                                let identity = rsi_terminal_ui::wire::Identity{attachment:client.generation,presentation:presentation_epoch,revision:frame_revision};
                                let frame = render::failure_frame(identity, ratatui::layout::Rect::new(0, 0, width, height), terminal.frames.borrow().as_deref(), &client.state.status);
                                terminal.frames.send_replace(Some(Arc::new(frame)));
                                continue;
                            }
                        };
                        let request = rsi_terminal_ui::wire::Request {identity:rsi_terminal_ui::wire::Identity{attachment:client.generation,presentation:presentation_epoch,revision:frame_revision},width,height,bytes:0};
                        rendering=Some(presentation.render(request,scene,rendering_stop.clone()));
                        dirty=false;
                    }
                    if client.history.backfill && !client.history.loading { client.history(false); }
                },
            }
        }
        Ok(())
    }.await;
    stopped.cancel();
    external.shutdown().await;
    profiles.shutdown().await;
    rendering_stop.cancel();
    rendering.take();
    client.tasks.clear();
    let cleanup = match observer {
        Some(observer) => observer.stop().await,
        None => Ok(()),
    };
    let shell_cleanup = surfaces.close().await;
    let presentation_cleanup = close_rendering(terminal.close(), presentation.close()).await;
    result
        .and(cleanup)
        .and(shell_cleanup)
        .and(presentation_cleanup)
}

async fn close_rendering(
    output: impl std::future::Future<Output = std::io::Result<()>>,
    presentation: impl std::future::Future<Output = crate::Result<()>>,
) -> crate::Result<()> {
    let output = output.await.map_err(error);
    let presentation = presentation.await;
    output.and(presentation)
}

fn termination() -> std::io::Result<impl std::future::Future<Output = ()>> {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{SignalKind, signal};
        let mut interrupt = signal(SignalKind::interrupt())?;
        let mut terminate = signal(SignalKind::terminate())?;
        let mut hangup = signal(SignalKind::hangup())?;
        let mut quit = signal(SignalKind::quit())?;
        Ok(async move {
            tokio::select! { _ = interrupt.recv() => {}, _ = terminate.recv() => {}, _ = hangup.recv() => {}, _ = quit.recv() => {} }
        })
    }
    #[cfg(not(unix))]
    Ok(std::future::pending())
}

fn markdown_status(enabled: bool) -> &'static str {
    if enabled {
        "Markdown rendering on (this process)"
    } else {
        "Markdown rendering off (original text; this process)"
    }
}
