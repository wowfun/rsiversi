//! Fullscreen presentation over Session. The Kernel remains the execution authority.
mod clipboard;
mod commands;
mod editor;
mod input;
mod prompts;
mod render;
mod state;
mod terminal;
#[cfg(test)]
mod tests;
mod transcript;
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
    services: Services,
    command: SessionCommand,
    application_work: ApplicationWork,
    context: rsi_meta::Context,
) -> u8 {
    match run_inner(services, command, application_work, context).await {
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

struct Attachment {
    handle: Arc<dyn SessionHandle>,
    header: SessionHeader,
    inspection: Option<StoreSessionInspection>,
    page: Option<rsi_session_protocol::SessionHistoryPage>,
}

async fn attachment(handle: Arc<dyn SessionHandle>, durable: bool) -> Result<Attachment> {
    let header = handle.header().await.map_err(error)?;
    let inspection = if durable {
        Some(read(|| handle.inspect()).await?)
    } else {
        None
    };
    let page = match &inspection {
        Some(snapshot) => Some(
            read(|| handle.history_before(snapshot.durable_fact_seq.checked_add(1), 128)).await?,
        ),
        None => None,
    };
    Ok(Attachment {
        handle,
        header,
        inspection,
        page,
    })
}

enum Update {
    Completions {
        prefix: String,
        names: Result<Vec<String>>,
    },
    Ui(rsi_ui::BoundView),
    Attached(Box<Attachment>),
    History(rsi_session_protocol::SessionHistoryPage),
    Inspect(Box<StoreSessionInspection>),
    Menu(Menu),
    Recent(rsi_session_protocol::RecentSessionPage),
    Models(rsi_ai_protocol::LanguageModelPage),
    Message(rsi_agent_session_protocol::AgentMessage),
    Output(rsi_process::OutputPage),
    Detail(String),
    Notice(String),
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
    model: Option<ModelRef>,
    owned: BTreeSet<MessageId>,
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum Durability {
    Draft,
    Durable,
}

struct Client {
    prompts: prompts::Prompts,
    ui: ui::Bindings,
    application: Arc<dyn SessionService>,
    output_cache: Arc<dyn rsi_process::ProcessOutputCache>,
    model_catalog: Arc<dyn rsi_ai_protocol::LanguageModels>,
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
    drafts: BTreeMap<SessionId, SavedSession>,
    recent: Option<rsi_session_protocol::RecentSessionCursor>,
    models: Option<ModelRef>,
}

impl Client {
    fn new(
        services: Services,
        attached: Attachment,
        controller: Arc<rsi_client::SessionController>,
        surface_target: Arc<rsi_ui::UiTarget>,
    ) -> Self {
        let Services {
            application,
            output_cache,
            model_catalog,
            workspace,
            lifetime,
            ui,
            ui_target,
        } = services;
        let mut client = Self {
            prompts: prompts::Prompts::default(),
            ui: ui::Bindings {
                registry: ui,
                application: ui_target,
                surface: surface_target,
            },
            application,
            output_cache,
            model_catalog,
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
            live_transcript: None,
            history: History {
                backfill: attached.page.is_some(),
                ..History::default()
            },
            inspecting: false,
            drafts: BTreeMap::new(),
            recent: None,
            models: None,
        };
        client.state.active = client
            .inspection
            .as_ref()
            .is_some_and(|snapshot| snapshot.active_turn_id.is_some());
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
            self.state.notice("Beginning of retained history");
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
        if boundary
            || self.history.pages >= 8
            || self.history.bytes >= 16 * 1024 * 1024
            || !page.has_more
        {
            self.history.backfill = false;
        }
    }

    fn submit(&mut self, delivery: MessageDelivery, retry: bool) {
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
            if self.state.editor.text.trim().is_empty() {
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
            self.submission.request = Some(SubmitInput {
                delivery,
                message_id,
                content: vec![MessageInput::Text {
                    text: self.state.editor.take(),
                }],
                model: if delivery == MessageDelivery::NextTurn {
                    self.state.model.clone()
                } else {
                    None
                },
                sandbox: None,
            });
            self.remember_prompt();
        }
        let Some(request) = self.submission.request.clone() else {
            self.state.notice("No unresolved submission");
            return;
        };
        self.owned.insert(request.message_id.clone());
        let controller = self.controller.clone();
        let command = self.command.clone();
        self.submission.busy = true;
        let was_rejected = self.submission.rejected;
        self.submission.rejected = false;
        self.state
            .notice(format!("Submitting {}", request.message_id));
        self.submission.busy = self.spawn_as(WorkKind::Submit, async move {
            if !retry && let [MessageInput::Text { text }] = request.content.as_slice() {
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
        self.state.notice("Cancellation requested; drafts retained");
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
                "Cancellation accepted for {count} targets; draft retained"
            )))
        });
        self.cancellation_queued = !self.cancelling;
    }

    fn scroll(&mut self, view: &render::View, up: bool) {
        if let Some(answer) = &mut self.state.answer {
            answer.scroll = if up {
                answer.scroll.saturating_sub(3)
            } else {
                answer.scroll.saturating_add(3)
            };
            return;
        }
        if self.state.detail.is_some() {
            self.state.detail_offset = if up {
                self.state.detail_offset.saturating_sub(3)
            } else {
                self.state.detail_offset.saturating_add(3)
            };
            return;
        }
        if !view.belongs_to(&self.state) {
            return;
        }
        if view.is_empty() {
            self.history(true);
            return;
        }
        let Some((first_block, first_offset)) = view.location(&self.state, 0) else {
            return;
        };
        if up {
            let block = &self.state.transcript.blocks[first_block];
            let text = block.text();
            let offset = text[..first_offset]
                .char_indices()
                .rev()
                .nth(usize::from(view.area.width).saturating_mul(3))
                .map_or(0, |(i, _)| i);
            let (index, offset) = if first_offset == 0 && first_block > 0 {
                (first_block - 1, 0)
            } else {
                (first_block, offset)
            };
            self.state.top = self.state.transcript.blocks[index].anchor(offset);
            if index == 0 && offset == 0 {
                self.history(true);
            }
        } else if let Some((index, offset)) = view.location(&self.state, 3) {
            self.state.top = self.state.transcript.blocks[index].anchor(offset);
        } else {
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
        if let Some(live) = self.live_transcript.take() {
            self.state.transcript = live;
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

    fn copy(&mut self) {
        if self.tasks.len() >= 8 {
            self.state.notice("Copy queue is busy");
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
            self.state.notice("Select transcript text first");
            return;
        };
        match self.state.transcript.selected(a, b) {
            Ok(text) => self.spawn(async move { Ok(Update::Copy(clipboard::copy(text).await)) }),
            Err(message) => self.state.notice(message),
        }
    }

    #[allow(clippy::too_many_lines)] // Central dispatch keeps the finite UI actions and authority checks visible together.
    fn action(&mut self, action: Action) -> bool {
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
            Action::RecallPrompt(id) => self.recall_prompt(id),
            Action::CompleteCommand(prefix, name) => self.insert_completion(&prefix, &name),
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
                        .filter_map(|input| {
                            if let MessageInput::Text { text } = input {
                                Some(text.as_str())
                            } else {
                                None
                            }
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
                    self.state.notice("No unresolved submission");
                }
            }
            Action::DiscardRejected => {
                if self.submission.rejected {
                    self.submission.request = None;
                    self.submission.rejected = false;
                    self.state.escape();
                    self.state
                        .notice("Rejected input discarded; editor draft retained");
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
                    let registered = workspace.get_or_create(&cwd).await.map_err(error)?;
                    let handle = application
                        .create(CreateSession {
                            workspace_id: registered.id,
                            session_id,
                            agent_preset_id: None,
                            workspace_trust: WorkspaceTrust::Untrusted,
                        })
                        .await
                        .map_err(error)?;
                    attachment(handle, false)
                        .await
                        .map(|attached| Update::Attached(Box::new(attached)))
                });
            }
            Action::Attach(id) => self.spawn(async move {
                attachment(read(|| application.attach(&id)).await?, true)
                    .await
                    .map(|attached| Update::Attached(Box::new(attached)))
            }),
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
            Action::Models | Action::MoreModels => {
                if matches!(action, Action::Models) {
                    self.models = None;
                }
                let after = self.models.clone();
                let model_catalog = self.model_catalog.clone();
                self.spawn(async move {
                    model_catalog
                        .list_models(after.as_ref(), 64)
                        .await
                        .map(Update::Models)
                        .map_err(error)
                });
            }
            Action::Model(model) => {
                self.state.model = model;
                self.state
                    .notice("Model selected for explicit NextTurn inputs");
            }
            Action::Queue | Action::Agents => self.spawn(async move {
                let snapshot = read(|| handle.inspect()).await?;
                let (title, items) = if matches!(action, Action::Queue) {
                    (
                        "Pending inputs",
                        snapshot
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
                    )
                } else {
                    (
                        "Agents · history inspection",
                        snapshot
                            .tree
                            .descendants
                            .into_iter()
                            .map(|child| {
                                (
                                    format!("{} · {}", child.task_name, child.status.session_id),
                                    Action::Child(child.status.session_id),
                                )
                            })
                            .collect(),
                    )
                };
                Ok(Update::Menu(Menu {
                    title: title.into(),
                    items,
                    selected: 0,
                }))
            }),
            Action::Child(id) => self.spawn_detail(async move {
                let child = read(|| application.attach(&id)).await?;
                let page = read(|| child.history_before(None, 128)).await?;
                let mut transcript = transcript::Transcript::default();
                for fact in &page.facts {
                    transcript.apply(fact);
                }
                let text = transcript
                    .blocks
                    .iter()
                    .map(|block| format!("{}\n{}", block.title, block.text()))
                    .collect::<Vec<_>>()
                    .join("\n\n");
                Ok(Update::Detail(format!(
                    "Child {id} · latest 128 Facts · history only\n\n{text}"
                )))
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
                        Ok(Update::Notice(format!(
                            "Approval response accepted: {accepted}"
                        )))
                    });
                } else {
                    self.state
                        .notice("Approval changed; reopen the current request");
                }
            }
            Action::Detail => {
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
        let raw = answer.editor.text.clone();
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
                Ok(Update::Notice(format!("Answer accepted: {accepted}")))
            });
        }
    }
}

#[allow(clippy::too_many_lines)] // One loop owns generation-tagged results and observer handoff.
async fn run_inner(
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
    let resumed = command.resume.is_some();
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
            workspace_trust: if command.trust_workspace {
                WorkspaceTrust::Trusted
            } else {
                WorkspaceTrust::Untrusted
            },
        },
    };
    let attached = tokio::select! { biased;
        () = application_work.stop.cancelled() => return Ok(()),
        attached = async {
            attachment(resolve_application_handle(&application, &workspace, selection).await?, resumed).await
        } => attached?,
    };
    let mut terminal = terminal::Terminal::enter(&application_work.tasks).map_err(error)?;
    let stopped = application_work.stop.child_token();
    let mut input = input::spawn(stopped.clone(), &application_work.tasks).map_err(error)?;
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
    let mut ui_changes = client.ui.registry.changes();
    let (width, height) = terminal::size();
    let mut screen =
        ratatui::Terminal::new(ratatui::backend::TestBackend::new(width, height)).map_err(error)?;
    let mut view = render::View::default();
    let mut frame_revision = 0_u64;
    let mut tick = tokio::time::interval(Duration::from_millis(33));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut inspect_tick = tokio::time::interval(Duration::from_secs(1));
    inspect_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut dirty = true;
    let mut terminate = Box::pin(termination());
    let result: Result<()> = async {
        loop {
            client.state.busy = client.submission.busy || !client.tasks.is_empty();
            client.state.editor.set_retention_limit((4 * 1024 * 1024usize).saturating_sub(client.drafts.values().map(|saved| saved.editor.retained_bytes()).sum()));
            if let Some(answer) = &mut client.state.answer { answer.editor.limit = rsi_user_questions_protocol::MAXIMUM_QUESTION_BYTES.saturating_sub(answer.answers.iter().map(String::len).sum()); }
            tokio::select! {
                () = application_work.stop.cancelled() => break,
                changed = ui_changes.changed() => {
                    if changed.is_ok() { client.ui_changed(); dirty = true; }
                },
                signal = tokio::signal::ctrl_c() => { signal.map_err(error)?; client.cancel(); dirty = true; },
                () = &mut terminate => break,
                changed = terminal.presented.changed() => {
                    changed.map_err(error)?;
                    if let Some(frame) = terminal.presented.borrow_and_update().as_ref() {
                        view = if frame.generation == client.generation {
                            frame.view.clone()
                        } else {
                            render::View::default()
                        };
                    }
                },
                incoming = input.recv() => {
                    if terminal.presented.borrow().as_ref().is_none_or(|frame| frame.generation != client.generation) {
                        view = render::View::default();
                    }
                    dirty = true;
                    match incoming.unwrap_or(input::Input::Closed) {
                        input::Input::Closed => break,
                        input::Input::Rejected(message) => client.state.notice(message),
                        input::Input::Terminal(termina::Event::Paste(text)) => {
                            if client.state.ui_paste(&text) { continue; }
                            if let Err(message) = client.state.answer.as_mut().map_or(&mut client.state.editor, |answer| &mut answer.editor).insert(&text) { client.state.notice(message); }
                        },
                        input::Input::Terminal(termina::Event::Key(key)) if key.kind != KeyEventKind::Release => {
                            let control = key.modifiers.contains(Modifiers::CONTROL);
                            if control && key.code == KeyCode::Char('c') { client.cancel(); continue; }
                            if control && key.code == KeyCode::Char('y') { client.copy(); continue; }
                            if key.code == KeyCode::Escape { client.state.escape(); continue; }
                            if control && key.code == KeyCode::Char('p') { client.action_menu(); continue; }
                            if control && key.code == KeyCode::Char('r') && client.state.ui_edit.is_none() && client.state.answer.is_none() && client.state.detail.is_none() { client.prompt_menu(); continue; }
                            if control && key.code == KeyCode::Char('d') && client.state.editor.text.is_empty() && client.state.answer.is_none() && client.submission.request.is_none() { break; }
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
                            else if key.code == KeyCode::End && !control { client.follow_live(); }
                            else if client.state.detail.is_some() && matches!(key.code, KeyCode::Up | KeyCode::Down) { client.scroll(&view, key.code == KeyCode::Up); }
                            else if client.state.detail.is_some() && key.code == KeyCode::Enter { client.state.menu.clone_from(&client.state.detail_actions); }
                            else if client.state.detail.is_some() && matches!(key.code, KeyCode::Right | KeyCode::Left) {
                                let action = if key.code == KeyCode::Right { client.state.detail_next.clone() } else { client.state.detail_previous.clone() };
                                if let Some(action) = action { client.action(action); }
                            }
                            else if key.code == KeyCode::Tab && client.state.detail.is_none() && client.state.answer.is_none() && client.complete_command() {}
                            else if key.code == KeyCode::Tab { client.state.focused = (client.state.focused+1) % client.state.transcript.blocks.len().max(1); if let Some(block) = client.state.transcript.blocks.get_mut(client.state.focused) { block.collapsed = !block.collapsed; client.state.top = block.anchor(0); } }
                            else if key.code == KeyCode::Enter && !key.modifiers.contains(Modifiers::SHIFT) {
                                if client.state.answer.is_some() { client.answer(); } else { client.submit(MessageDelivery::NextTurn, false); }
                            } else if control && key.code == KeyCode::Char('o') { client.submit(MessageDelivery::Steer, false); }
                            else if let Err(message) = client.state.answer.as_mut().map_or(&mut client.state.editor, |answer| &mut answer.editor).key(key) { client.state.notice(message); }
                        },
                        input::Input::Terminal(termina::Event::Mouse(mouse)) => match mouse.kind {
                            _ if client.state.menu.is_some() || client.state.answer.is_some() || client.state.detail.is_some() => {
                                if let Some(menu) = &mut client.state.menu {
                                    match mouse.kind {
                                        MouseEventKind::ScrollUp => menu.selected = menu.selected.saturating_sub(1),
                                        MouseEventKind::ScrollDown => menu.selected = (menu.selected+1).min(menu.items.len().saturating_sub(1)),
                                        MouseEventKind::Down(MouseButton::Left) if mouse.row >= 4 => {
                                            let from = menu.selected.saturating_sub(usize::from(terminal::size().1.saturating_sub(6))/2);
                                            let index = from + usize::from(mouse.row-4);
                                            if index < menu.items.len() { menu.selected = index; }
                                        },
                                        _ => {},
                                    }
                                } else if matches!(mouse.kind, MouseEventKind::ScrollUp | MouseEventKind::ScrollDown) { client.scroll(&view, mouse.kind == MouseEventKind::ScrollUp); }
                            },
                            MouseEventKind::ScrollUp => client.scroll(&view, true), MouseEventKind::ScrollDown => client.scroll(&view, false),
                            MouseEventKind::Down(MouseButton::Left) => {
                                if view.belongs_to(&client.state) && let Some(anchor) = view.hit(mouse.column, mouse.row, false) { client.state.selection = Some((anchor,anchor)); client.state.focused = client.state.transcript.locate(anchor).map_or(0, |(index,_)| index); }
                            },
                            MouseEventKind::Drag(MouseButton::Left) => if view.belongs_to(&client.state) && let Some(anchor) = view.hit(mouse.column, mouse.row, true) && let Some((_,end)) = &mut client.state.selection { *end = anchor; },
                            MouseEventKind::Up(MouseButton::Left) => client.copy(), _ => {},
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
                    if work.view_revision != client.state.view_revision && matches!(&work.result, Ok(Update::Completions { .. } | Update::Ui(_) | Update::Menu(_) | Update::Recent(_) | Update::Models(_) | Update::Detail(_) | Update::Message(_) | Update::Window(_) | Update::Output(_) | Update::Attached(_))) { continue; }
                    match work.result {
                        Ok(Update::Completions { prefix, names }) => client.command_completions(&prefix, names),
                        Ok(Update::Ui(view)) => client.show_ui(view),
                        Ok(Update::Command(result)) => client.command_finished(result),
                        Err(problem) => {
                            if matches!(work.kind, WorkKind::Detail) { client.ui_failed(); }
                            if matches!(work.kind, WorkKind::History) { client.history.backfill = false; }
                            client.state.notice(problem.to_string());
                        },
                        Ok(Update::Notice(message)) => client.state.notice(message),
                        Ok(Update::Copy(delivery)) => {
                            client.state.notice(delivery.status);
                            if let Some(osc) = delivery.osc && terminal.commands.try_send(osc).is_err() { client.state.notice("Copy failed: terminal command queue is busy"); }
                        },
                        Ok(Update::Menu(menu)) => client.state.menu = Some(menu),
                        Ok(Update::Recent(page)) => {
                            client.recent = page.sessions.last().map(rsi_session_protocol::SessionSummary::cursor);
                            let mut items = page.sessions.into_iter().map(|summary| (format!("{} · {}", summary.header.session_id(), summary.header.canonical_cwd()), Action::Attach(summary.header.session_id().clone()))).collect::<Vec<_>>();
                            if page.has_more { items.push(("More sessions…".into(), Action::MoreRecent)); }
                            client.state.menu = Some(Menu { title: "Recent sessions".into(), selected: 0, items });
                        },
                        Ok(Update::Models(page)) => {
                            client.models = page.models.last().cloned();
                            let mut items = vec![("Session default".into(), Action::Model(None))];
                            items.extend(page.models.into_iter().map(|model| (format!("{}/{}", model.deployment(), model.model()), Action::Model(Some(model)))));
                            if page.has_more { items.push(("More models…".into(), Action::MoreModels)); }
                            client.state.menu = Some(Menu { title: "Model for explicit NextTurn".into(), selected: 0, items });
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
                        Ok(Update::Detail(text)) => { client.state.open_detail(super::terminal_text(&text)); },
                        Ok(Update::Window(piece)) => {
                            let source = piece.source; let next = piece.anchor(piece.text.len()).offset;
                            client.state.open_detail(piece.text);
                            client.state.detail_previous = (piece.start > 0).then_some(Action::Window(source, piece.start.saturating_sub(transcript::WINDOW)));
                            client.state.detail_next = (next > piece.start && piece.truncated_after).then_some(Action::Window(source, next));
                            client.state.notice(format!("Fact {} field {} · bytes {}..{next} · ←/→ source windows · Ctrl+Y copies this window", source.seq, source.field, piece.start));
                        },
                        Ok(Update::Inspect(snapshot)) => {
                            client.inspecting = false; client.state.active = snapshot.active_turn_id.is_some();
                            let pending: BTreeSet<_> = snapshot.pending.iter().map(|message| message.message_id.clone()).collect();
                            client.owned.retain(|id| pending.contains(id) || client.submission.request.as_ref().is_some_and(|request| request.message_id == *id));
                            client.inspection = Some(*snapshot);
                        },
                        Ok(Update::History(page)) => client.page(page),
                        Ok(Update::Submitted(receipt)) => {
                            client.submission.busy = false;
                            match receipt {
                                Ok(receipt) => {
                                    client.state.notice(format!("Accepted {} · {:?}", receipt.message_id, receipt.state));
                                    client.submission.request = None;
                                    client.durability = Durability::Durable;
                                    if client.submission.cancel_when_accepted { client.submission.cancel_when_accepted = false; client.cancel(); }
                                    client.inspect();
                                },
                                Err(problem) => {
                                    client.submission.rejected = !matches!(problem, SessionError::MessageOutcomeUnknown {..});
                                    if client.submission.rejected {
                                        if let Some(request) = &client.submission.request { client.owned.remove(&request.message_id); }
                                        client.submission.cancel_when_accepted = false;
                                        if client.state.editor.text.is_empty()
                                            && let Some(request) = client.submission.request.take()
                                            && let Some(MessageInput::Text { text }) = request.content.into_iter().next() { client.state.editor = editor::Editor::with_text(text, input::MAX_TEXT); }
                                    }
                                    client.state.notice(format!("Submission failed: {problem}. Input retained in the editor or Actions → Pending / rejected submission."));
                                },
                            }
                        },
                        Ok(Update::Attached(attached)) => {
                            if client.submission.request.is_some() { client.state.notice("Session switch deferred until submission is resolved"); continue; }
                            let next = match surfaces.open(attached.header.session_id(), attached.inspection.as_ref().map(|snapshot| ObservationCursor { fact_seq: snapshot.durable_fact_seq, control_seq: snapshot.durable_control_seq }), client.generation + 1).await {
                                Ok(next) => next,
                                Err(problem) => { client.state.notice(problem.to_string()); continue; },
                            };
                            if let Some(observer) = observer.take() { observer.stop().await?; }
                            client.controller = next.controller.clone();
                            client.ui.surface = next.ui_target.clone().ok_or_else(|| error("TUI surface target is unavailable"))?;
                            client.projections = None; client.projection_notice.clear(); client.extension_view = None;
                            observer = Some(next);
                            client.tasks.clear(); client.generation += 1;
                            let old = client.state.header.session_id().clone();
                            let draft = std::mem::take(&mut client.state.editor); let model = client.state.model.take();
                            let owned = std::mem::take(&mut client.owned);
                            let command = std::mem::take(&mut client.command);
                            if !draft.text.is_empty() || draft.has_edits() || model.is_some() || !owned.is_empty() || command.view().pending.is_some() || command.view().receipt.is_some() { client.drafts.insert(old, SavedSession { editor: draft, model, owned, command }); }
                            client.handle = attached.handle; client.state = State::new(attached.header, client.state.remote);
                            if let Some(saved) = client.drafts.remove(client.state.header.session_id()) { client.state.editor = saved.editor; client.state.model = saved.model; client.owned = saved.owned; client.command = saved.command; }
                            client.cancelling = false; client.cancellation_queued = false; client.interactions = None; client.inspection = attached.inspection; client.durability = if client.inspection.is_some() { Durability::Durable } else { Durability::Draft }; client.history.before = None; client.live_transcript = None;
                            client.state.active = client.inspection.as_ref().is_some_and(|snapshot| snapshot.active_turn_id.is_some());
                            client.history.loading = false; client.inspecting = false; client.history.pages = 0; client.history.bytes = 0; client.history.backfill = true;
                            if let Some(page) = attached.page { client.page(page); }
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
                            match fact.body() { SessionFactBody::TurnAccepted {..} | SessionFactBody::MessageTurnAccepted {..} => client.state.active = true, SessionFactBody::TurnTerminal {..} => { client.state.active = false; client.inspect(); }, _ => {} }
                            client.live_fact(&fact);
                        },
                        CliRenderMessage::Event(CliEvent::Projections { snapshot }) => {
                            client.projections = Some(snapshot); client.projection_notice.clear();
                            client.refresh_extensions();
                        },
                        CliRenderMessage::Event(CliEvent::Interactions { snapshot }) => {
                            client.state.questions = snapshot.questions().len(); client.state.approvals = snapshot.approvals().len();
                            if client.state.answer.as_ref().is_some_and(|answer| !snapshot.questions().contains(&answer.request)) { client.state.answer = None; client.state.notice("Question settled or withdrawn; answer draft closed"); }
                            client.interactions = Some(snapshot);
                        },
                        CliRenderMessage::Event(CliEvent::Control { record, .. }) => match record.body() {
                            AgentControlRecordBody::MessageAccepted { message, delivery, target, .. } if client.owned.contains(&message.message_id) => client.state.notice(format!("Accepted {} · {delivery:?} → {target:?}", message.message_id)),
                            AgentControlRecordBody::MessagePromoted { message_id } if client.owned.contains(message_id) => client.state.notice(format!("{message_id} promoted to NextTurn; Session default applies to Steer")),
                            _ => {},
                        },
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
                    if client.cancellation_queued && !client.cancelling && client.tasks.len() < 12 { client.cancel(); }
                    if terminal.failed() { return Err(error("Terminal writer stopped")); }
                    let (width,height) = terminal::size();
                    if screen.size().map_err(error)? != ratatui::layout::Size::new(width,height) { render::resize(&mut screen, width, height).map_err(error)?; dirty = true; }
                    if dirty {
                        let mut next_view = render::View::default();
                        screen.draw(|frame| next_view = render::draw(frame, &client.state)).map_err(error)?;
                        frame_revision = frame_revision.checked_add(1).ok_or_else(|| error("Terminal frame revision exhausted"))?;
                        terminal.frames.send_replace(Some(Arc::new(terminal::RenderedFrame {
                            generation: client.generation,
                            revision: frame_revision,
                            buffer: screen.backend().buffer().clone(),
                            view: next_view,
                        })));
                        dirty = false;
                    }
                    if client.history.backfill && !client.history.loading { client.history(false); }
                },
            }
        }
        Ok(())
    }.await;
    stopped.cancel();
    client.tasks.clear();
    let cleanup = match observer {
        Some(observer) => observer.stop().await,
        None => Ok(()),
    };
    let shell_cleanup = surfaces.close().await;
    terminal.close().await.map_err(error)?;
    result.and(cleanup).and(shell_cleanup)
}

async fn termination() {
    #[cfg(unix)]
    if let (Ok(mut terminate), Ok(mut hangup)) = (
        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()),
        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::hangup()),
    ) {
        tokio::select! { _ = terminate.recv() => {}, _ = hangup.recv() => {} }
        return;
    }
    std::future::pending::<()>().await;
}
