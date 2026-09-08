//! Native terminal application plugins over independent domain capabilities.
#![deny(unsafe_code)]
#![warn(missing_docs)]
#![allow(clippy::missing_errors_doc)]
mod arguments;
mod devices;
mod headless_commands;
mod plugin;
mod session_cli;
mod surfaces;
mod tui;
mod work;
use work::ApplicationWork;
#[cfg(test)]
mod tests;
use arguments::Command;
use arguments::{output_value, run_preset_value, session_value, usage};
pub use devices::HELP as DEVICES_HELP;
pub use plugin::{CliFactory, DevicesFactory, HeadlessFactory, TuiFactory};
use rsi_agent_session_protocol::{
    AgentControlRecordBody, AgentPresetId, MAXIMUM_AGENT_MESSAGE_CONTENT_BLOCKS,
    MAXIMUM_TURN_TEXT_BYTES, MessageId, SessionFact, SessionFactBody, SessionId, TurnId,
    TurnOutcome, WorkspaceTrust,
};
use rsi_agent_turn_protocol::{CancelTarget, MessageState, ObservationCursor, SessionObservation};
use rsi_ai_protocol::{ContentDelta, LanguageEvent, ModelRef};
use rsi_application::RsiError;
use rsi_application::arguments::{path_value, set_flag, set_option, utf8};
use rsi_sandbox::SandboxMode;
use rsi_session_protocol::{
    CreateSession, SessionError, SessionHandle, SessionInput as MessageInput, SessionService,
    SubmitInput,
};
use rsi_tools_protocol::ToolContent;
use serde::Serialize;
use serde_json::Value;
use std::{
    collections::BTreeMap,
    ffi::OsString,
    future::Future as _,
    io::{Read as _, Write},
    path::PathBuf,
    sync::Arc,
    task::Poll,
    time::Duration,
};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;
type Result<T, E = RsiError> = std::result::Result<T, E>;

/// Native application arguments; the launcher handles Profile selection separately.
pub const HELP: &str = "Session command options (headless):\n  --commands | --command INVOCATION_JSON | --command-status REQUEST_ID\n  --command may precede TASK; list and status do not submit a message.\nUsage:\n\
  rsi --profile headless TASK|--stdin [--cwd PATH] [--resume SESSION|--session-id SESSION]\n\
      [--message-id MESSAGE] [-i|--image PATH]... [--agent-preset ID]\n\
      [--deployment ID --model ID] [--sandbox read-only|workspace-write|danger-full-access]\n\
      [--trust-workspace] [--output text|jsonl]\n\
  rsi --profile cli [--cwd PATH] [--resume SESSION|--history SESSION|--list|--session-id SESSION]\n\
      [--agent-preset ID] [--trust-workspace] [--output text|jsonl]\n\
  rsi --profile tui [--cwd PATH] [--resume SESSION|--session-id SESSION]\n\
      [--agent-preset ID] [--trust-workspace]\n";

#[derive(Clone, Debug)]
pub(crate) struct SessionCommand {
    list: bool,
    history: Option<SessionId>,
    cwd: Option<PathBuf>,
    resume: Option<SessionId>,
    session_id: Option<SessionId>,
    agent_preset: Option<AgentPresetId>,
    trust_workspace: bool,
    output: OutputMode,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) enum OutputMode {
    #[default]
    Text,
    Jsonl,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum SessionSelection {
    Fresh {
        cwd: PathBuf,
        session_id: Option<SessionId>,
        agent_preset_id: Option<AgentPresetId>,
        workspace_trust: WorkspaceTrust,
    },
    Resume {
        session_id: SessionId,
        cwd: Option<PathBuf>,
    },
}

#[derive(Clone, Debug)]
pub(crate) struct HeadlessTurnOptions {
    pub(crate) task: String,
    pub(crate) session: SessionSelection,
    pub(crate) message_id: Option<MessageId>,
    pub(crate) images: Vec<PathBuf>,
    pub(crate) model: Option<ModelRef>,
    pub(crate) sandbox: Option<SandboxMode>,
    pub(crate) output: OutputMode,
}

#[derive(Clone, Debug)]
pub(crate) enum CliEvent {
    Interactions {
        snapshot: rsi_session_protocol::InteractionSnapshot,
    },
    Notice {
        kind: &'static str,
        value: Value,
    },
    Message {
        session_id: SessionId,
        message_id: MessageId,
        accepted_control_seq: u64,
    },
    Turn {
        session_id: SessionId,
        message_id: MessageId,
        turn_id: TurnId,
        entered_fact_seq: u64,
    },
    Control {
        session_id: SessionId,
        record: rsi_agent_turn_protocol::ObservedControl,
        durable_control_seq: u64,
    },
    Fact {
        session_id: SessionId,
        fact: rsi_agent_turn_protocol::ObservedFact,
        durable_seq: u64,
    },
    Outcome {
        session_id: SessionId,
        turn_id: TurnId,
        outcome: TurnOutcome,
        durable_seq: u64,
    },
}

#[derive(Serialize)]
struct InteractionEnvelope<'a> {
    version: u32,
    #[serde(rename = "type")]
    kind: &'static str,
    data: &'a rsi_session_protocol::InteractionSnapshot,
}

impl CliEvent {
    fn json_line(&self) -> std::result::Result<String, serde_json::Error> {
        match self {
            Self::Interactions { snapshot } => serde_json::to_string(&InteractionEnvelope {
                version: 4,
                kind: "interactions",
                data: snapshot,
            }),
            Self::Notice { kind, value } => {
                serde_json::to_string(&serde_json::json!({"version":4,"type":kind,"data":value}))
            }
            Self::Message {
                session_id,
                message_id,
                accepted_control_seq,
            } => serde_json::to_string(&MessageEnvelope {
                version: 4,
                kind: "message",
                session_id,
                message_id,
                accepted_control_seq: *accepted_control_seq,
            }),
            Self::Turn {
                session_id,
                message_id,
                turn_id,
                entered_fact_seq,
            } => serde_json::to_string(&TurnEnvelope {
                version: 4,
                kind: "turn",
                session_id,
                message_id,
                turn_id,
                entered_fact_seq: *entered_fact_seq,
            }),
            Self::Control {
                session_id,
                record,
                durable_control_seq,
            } => serde_json::to_string(&ControlEnvelope {
                version: 4,
                kind: "control",
                data: ControlData {
                    session_id,
                    record,
                    durable_control_seq: *durable_control_seq,
                },
            }),
            Self::Fact {
                session_id,
                fact,
                durable_seq,
            } => serde_json::to_string(&LiveFactEnvelope {
                version: 4,
                kind: "fact",
                session_id,
                fact,
                durable_seq: *durable_seq,
            }),
            Self::Outcome {
                session_id,
                turn_id,
                outcome,
                durable_seq,
            } => serde_json::to_string(&OutcomeEnvelope {
                version: 4,
                kind: "outcome",
                session_id,
                turn_id,
                outcome,
                durable_seq: *durable_seq,
            }),
        }
    }
}

#[derive(Serialize)]
struct ControlEnvelope<'a> {
    version: u32,
    #[serde(rename = "type")]
    kind: &'static str,
    data: ControlData<'a>,
}

#[derive(Serialize)]
struct ControlData<'a> {
    session_id: &'a SessionId,
    record: &'a rsi_agent_session_protocol::AgentControlRecord,
    durable_control_seq: u64,
}

#[derive(Serialize)]
pub(crate) struct MessageEnvelope<'a> {
    version: u32,
    #[serde(rename = "type")]
    kind: &'static str,
    session_id: &'a SessionId,
    message_id: &'a MessageId,
    accepted_control_seq: u64,
}

#[derive(Serialize)]
pub(crate) struct TurnEnvelope<'a> {
    version: u32,
    #[serde(rename = "type")]
    kind: &'static str,
    session_id: &'a SessionId,
    message_id: &'a MessageId,
    turn_id: &'a TurnId,
    entered_fact_seq: u64,
}

#[derive(Serialize)]
pub(crate) struct LiveFactEnvelope<'a> {
    version: u32,
    #[serde(rename = "type")]
    kind: &'static str,
    session_id: &'a SessionId,
    fact: &'a SessionFact,
    durable_seq: u64,
}

#[derive(Serialize)]
pub(crate) struct OutcomeEnvelope<'a> {
    version: u32,
    #[serde(rename = "type")]
    kind: &'static str,
    session_id: &'a SessionId,
    turn_id: &'a TurnId,
    outcome: &'a TurnOutcome,
    durable_seq: u64,
}

impl SessionCommand {
    pub(crate) fn parse(arguments: Vec<OsString>) -> Result<Self> {
        let mut command = Self {
            list: false,
            history: None,
            cwd: None,
            resume: None,
            session_id: None,
            agent_preset: None,
            trust_workspace: false,
            output: OutputMode::Text,
        };
        let mut arguments = arguments.into_iter();
        let mut output_set = false;
        while let Some(argument) = arguments.next() {
            let argument = utf8(argument)?;
            match argument.as_str() {
                "--list" => set_flag(&mut command.list, "--list")?,
                "--history" => set_option(
                    &mut command.history,
                    session_value(&mut arguments, "--history")?,
                    "--history",
                )?,
                "--cwd" => set_option(
                    &mut command.cwd,
                    path_value(&mut arguments, "--cwd")?,
                    "--cwd",
                )?,
                "--resume" => set_option(
                    &mut command.resume,
                    session_value(&mut arguments, "--resume")?,
                    "--resume",
                )?,
                "--session-id" => set_option(
                    &mut command.session_id,
                    session_value(&mut arguments, "--session-id")?,
                    "--session-id",
                )?,
                "--agent-preset" => set_option(
                    &mut command.agent_preset,
                    run_preset_value(&mut arguments)?,
                    "--agent-preset",
                )?,
                "--trust-workspace" => {
                    set_flag(&mut command.trust_workspace, "--trust-workspace")?;
                }
                "--output" => {
                    if output_set {
                        return Err(usage("duplicate --output"));
                    }
                    output_set = true;
                    command.output = output_value(&mut arguments)?;
                }
                option => {
                    return Err(usage(format!(
                        "unknown Session application argument `{option}`"
                    )));
                }
            }
        }
        if (u8::from(command.list)
            + u8::from(command.history.is_some())
            + u8::from(command.resume.is_some()))
            > 1
        {
            return Err(usage(
                "--list, --history and --resume are mutually exclusive",
            ));
        }
        if (command.list || command.history.is_some())
            && (command.cwd.is_some()
                || command.session_id.is_some()
                || command.agent_preset.is_some()
                || command.trust_workspace)
        {
            return Err(usage(
                "read-only Session commands cannot change creation settings",
            ));
        }
        if command.resume.is_some() && command.session_id.is_some() {
            return Err(usage("--resume and --session-id are mutually exclusive"));
        }
        if command.resume.is_some() && command.agent_preset.is_some() {
            return Err(usage("--resume and --agent-preset are mutually exclusive"));
        }
        if command.resume.is_some() && command.trust_workspace {
            return Err(usage(
                "--trust-workspace cannot change an existing Session's immutable authority",
            ));
        }
        Ok(command)
    }
}

pub(crate) async fn resolve_application_handle(
    application: &Arc<dyn SessionService>,
    workspace: &Arc<dyn rsi_workspace_protocol::WorkspaceRegistry>,
    session: SessionSelection,
) -> Result<Arc<dyn SessionHandle>> {
    match session {
        SessionSelection::Fresh {
            cwd,
            session_id,
            agent_preset_id,
            workspace_trust,
        } => {
            let session_id = session_id.map_or_else(generated_cli_session_id, Ok)?;
            let cwd = std::path::absolute(cwd)
                .map_err(|error| RsiError::Boot(format!("workspace: {error}")))?;
            let registered = workspace
                .get_or_create(&cwd)
                .await
                .map_err(|error| RsiError::Boot(error.to_string()))?;
            application
                .create(CreateSession {
                    workspace_id: registered.id,
                    session_id,
                    agent_preset_id,
                    workspace_trust,
                })
                .await
                .map_err(|error| RsiError::Boot(error.to_string()))
        }
        SessionSelection::Resume { session_id, cwd } => {
            let handle = application
                .attach(&session_id)
                .await
                .map_err(|error| RsiError::Boot(error.to_string()))?;
            if let Some(cwd) = cwd {
                let canonical = tokio::fs::canonicalize(cwd)
                    .await
                    .map_err(|error| RsiError::Boot(format!("workspace: {error}")))?;
                let header = handle
                    .header()
                    .await
                    .map_err(|error| RsiError::Run(error.to_string()))?;
                if canonical.to_str() != Some(header.canonical_cwd()) {
                    return Err(RsiError::Boot(
                        "--cwd does not match the durable Session workspace".into(),
                    ));
                }
            }
            Ok(handle)
        }
    }
}

pub(crate) const CLI_RENDER_CHANNEL_CAPACITY: usize = 32;
pub(crate) const TURN_COMPLETION_CHANNEL_CAPACITY: usize = 1;

#[derive(Debug)]
pub(crate) enum CliRenderMessage {
    Event(CliEvent),
    Observed { generation: u64, event: CliEvent },
    FinishLine,
}

#[derive(Debug)]
pub(crate) struct MessageTaskFinished {
    pub(crate) result: Result<TurnOutcome>,
    cancellation_requested: bool,
}

#[cfg(test)]
pub(crate) use rsi_client::submit_with_reconciliation;

#[derive(Debug)]
struct CliMessageSink {
    handle: Arc<dyn SessionHandle>,
    renderer: tokio::sync::mpsc::Sender<CliRenderMessage>,
    cancellation: CancellationToken,
    stopped: CancellationToken,
    work: ApplicationWork,
    interactions: std::sync::Mutex<Option<session_cli::InteractionWatcher>>,
}
#[async_trait::async_trait]
impl rsi_client::MessageSink for CliMessageSink {
    async fn event(
        &self,
        event: rsi_client::MessageEvent,
    ) -> Result<(), rsi_client::MessageRunError> {
        use rsi_client::MessageEvent;
        let terminal = matches!(event, MessageEvent::Outcome { .. });
        let event = match event {
            MessageEvent::Accepted(receipt) => CliEvent::Message {
                session_id: receipt.session_id,
                message_id: receipt.message_id,
                accepted_control_seq: receipt.accepted_control_seq,
            },
            MessageEvent::Claimed {
                session_id,
                message_id,
                turn_id,
                entered_fact_seq,
            } => {
                *self
                    .interactions
                    .lock()
                    .expect("message interaction watcher poisoned") =
                    Some(session_cli::spawn_interactions(
                        self.handle.clone(),
                        self.renderer.clone(),
                        &self.stopped,
                        &self.work,
                    ));
                CliEvent::Turn {
                    session_id,
                    message_id,
                    turn_id,
                    entered_fact_seq,
                }
            }
            MessageEvent::Fact {
                session_id,
                fact,
                durable_seq,
            } => CliEvent::Fact {
                session_id,
                fact,
                durable_seq,
            },
            MessageEvent::Outcome {
                session_id,
                turn_id,
                outcome,
                durable_seq,
            } => CliEvent::Outcome {
                session_id,
                turn_id,
                outcome,
                durable_seq,
            },
        };
        send_cli_event(&self.renderer, &self.stopped, &self.cancellation, event)
            .await
            .map_err(|_| rsi_client::MessageRunError::SinkStopped)?;
        if terminal {
            send_finish_line(&self.renderer, &self.stopped)
                .await
                .map_err(|_| rsi_client::MessageRunError::SinkStopped)?;
        }
        Ok(())
    }
}

pub(crate) async fn drive_application_turn(
    handle: Arc<dyn SessionHandle>,
    request: SubmitInput,
    cancellation: CancellationToken,
    rendering_stopped: CancellationToken,
    renderer: tokio::sync::mpsc::Sender<CliRenderMessage>,
    completion: tokio::sync::mpsc::Sender<MessageTaskFinished>,
    work: ApplicationWork,
) {
    let result = {
        let sink = CliMessageSink {
            handle: handle.clone(),
            renderer: renderer.clone(),
            cancellation: cancellation.clone(),
            stopped: rendering_stopped.clone(),
            work,
            interactions: std::sync::Mutex::new(None),
        };
        rsi_client::drive_message(handle.as_ref(), request, &cancellation, &sink)
            .await
            .map_err(|error| RsiError::Run(error.to_string()))
    };
    let _ = completion
        .send(MessageTaskFinished {
            result,
            cancellation_requested: cancellation.is_cancelled(),
        })
        .await;
}

pub(crate) async fn send_cli_event(
    renderer: &tokio::sync::mpsc::Sender<CliRenderMessage>,
    rendering_stopped: &CancellationToken,
    turn_cancellation: &CancellationToken,
    event: CliEvent,
) -> Result<()> {
    if rendering_stopped.is_cancelled() {
        return Ok(());
    }
    let terminal = matches!(&event, CliEvent::Outcome { .. })
        || matches!(&event, CliEvent::Fact { fact, .. } if matches!(fact.body(), SessionFactBody::TurnTerminal { .. }));
    tokio::select! {
        biased;
        () = rendering_stopped.cancelled() => Ok(()),
        result = renderer.send(CliRenderMessage::Event(event)) => result
            .map_err(|_| RsiError::Run("terminal renderer stopped before the turn ended".into())),
        () = turn_cancellation.cancelled(), if !terminal => Ok(()),
    }
}

pub(crate) async fn send_finish_line(
    renderer: &tokio::sync::mpsc::Sender<CliRenderMessage>,
    rendering_stopped: &CancellationToken,
) -> Result<()> {
    if rendering_stopped.is_cancelled() {
        return Ok(());
    }
    tokio::select! {
        biased;
        () = rendering_stopped.cancelled() => Ok(()),
        result = renderer.send(CliRenderMessage::FinishLine) => result
            .map_err(|_| RsiError::Run("terminal renderer stopped before the turn ended".into())),
    }
}

#[derive(Debug, Default)]
pub(crate) struct CliRenderState {
    wrote_text: bool,
    text_ends_newline: bool,
}

impl CliRenderState {
    fn write(
        &mut self,
        stdout: &mut impl Write,
        stderr: &mut impl Write,
        output: OutputMode,
        event: &CliEvent,
    ) -> Result<()> {
        if output == OutputMode::Text {
            write_status_event(stderr, event)?;
        }
        write_live_event(
            stdout,
            output,
            event,
            &mut self.wrote_text,
            &mut self.text_ends_newline,
        )
    }

    fn finish_line(&mut self, stdout: &mut impl Write, output: OutputMode) -> Result<()> {
        if output == OutputMode::Text && self.wrote_text && !self.text_ends_newline {
            writeln!(stdout)
                .and_then(|()| stdout.flush())
                .map_err(|error| RsiError::Run(format!("stdout write failed: {error}")))?;
            self.text_ends_newline = true;
        }
        Ok(())
    }
}

pub(crate) struct CliRenderer {
    finished: tokio::sync::oneshot::Receiver<Result<()>>,
    stop: CancellationToken,
}
impl Drop for CliRenderer {
    fn drop(&mut self) {
        self.stop.cancel();
    }
}

pub(crate) fn spawn_cli_renderer(
    output: OutputMode,
    mut receiver: tokio::sync::mpsc::Receiver<CliRenderMessage>,
    work: &ApplicationWork,
) -> CliRenderer {
    let (outcome, finished) = tokio::sync::oneshot::channel();
    let token = work.tasks.token();
    let stop = work.stop.child_token();
    let worker_stop = stop.clone();
    let runtime = tokio::runtime::Handle::current();
    std::thread::spawn(move || {
        let _token = token;
        let mut renderer = CliRenderState::default();
        let result = (|| {
            #[cfg(unix)]
            let (mut stdout, mut stderr) = (
                work::Output::new(std::io::stdout(), worker_stop.clone()),
                work::Output::new(std::io::stderr(), worker_stop.clone()),
            );
            #[cfg(unix)]
            let (mut stdout, mut stderr) = (
                stdout
                    .as_mut()
                    .map_err(|error| RsiError::Run(format!("stdout: {error}")))?,
                stderr
                    .as_mut()
                    .map_err(|error| RsiError::Run(format!("stderr: {error}")))?,
            );
            #[cfg(not(unix))]
            let (mut stdout, mut stderr) = (std::io::stdout(), std::io::stderr());
            while let Some(message) = runtime.block_on(async {
                tokio::select! { biased;
                    () = worker_stop.cancelled() => None,
                    message = receiver.recv() => message,
                }
            }) {
                match message {
                    CliRenderMessage::Event(event) | CliRenderMessage::Observed { event, .. } => {
                        renderer.write(&mut stdout, &mut stderr, output, &event)?;
                    }
                    CliRenderMessage::FinishLine => renderer.finish_line(&mut stdout, output)?,
                }
            }
            Ok(())
        })();
        let _ = outcome.send(result);
    });
    CliRenderer { finished, stop }
}

pub(crate) async fn join_cli_renderer(mut renderer: CliRenderer) -> Result<()> {
    (&mut renderer.finished)
        .await
        .map_err(|_| RsiError::Run("terminal renderer panicked".into()))?
}

#[allow(clippy::too_many_lines)] // Keep signal, presentation and observer shutdown in one ownership scope.
pub(crate) async fn run_headless_application(
    application: Arc<dyn SessionService>,
    workspace: Arc<dyn rsi_workspace_protocol::WorkspaceRegistry>,
    media: Arc<dyn rsi_media_protocol::Media>,
    command: Command,
    work: ApplicationWork,
) -> u8 {
    let mut running = Box::pin(async {
        let has_task = command.stdin || command.positional.is_some();
        let task = match command.task(&work).await {
            Ok(task) => task,
            Err(error) => return report_error(&error),
        };
        let options = match command.options(task) {
            Ok(options) => options,
            Err(error) => return report_error(&error),
        };
        let handle =
            match resolve_application_handle(&application, &workspace, options.session).await {
                Ok(handle) => handle,
                Err(error) => return report_error(&error),
            };
        if let Some(extension) = &command.extension {
            let exit =
                headless_commands::run(extension, handle.as_ref(), options.output, &work).await;
            if exit != 0 || !has_task {
                return exit;
            }
        }
        let content =
            match headless_inputs(options.task, options.images, media.as_ref(), &work).await {
                Ok(content) => content,
                Err(error) => return report_error(&error),
            };
        let message_id = match options.message_id.map_or_else(generated_cli_message_id, Ok) {
            Ok(id) => id,
            Err(error) => return report_error(&error),
        };
        let cancellation = CancellationToken::new();
        let signal = match arm_signal(cancellation.clone(), &work).await {
            Ok(signal) => signal,
            Err(error) => return report_error(&error),
        };
        let rendering_stopped = CancellationToken::new();
        let (renderer, render_receiver) = tokio::sync::mpsc::channel(CLI_RENDER_CHANNEL_CAPACITY);
        let render_task = spawn_cli_renderer(options.output, render_receiver, &work);
        let (completion, mut finished) =
            tokio::sync::mpsc::channel(TURN_COMPLETION_CHANNEL_CAPACITY);
        let turn_work = work.clone();
        let turn =
            tokio_util::task::AbortOnDropHandle::new(work.tasks.spawn(drive_application_turn(
                handle,
                SubmitInput {
                    delivery: rsi_agent_session_protocol::MessageDelivery::NextTurn,
                    message_id,
                    content,
                    model: options.model,
                    sandbox: options.sandbox,
                },
                cancellation.clone(),
                rendering_stopped.clone(),
                renderer,
                completion,
                turn_work,
            )));
        // A signal covers the whole presentation lifecycle: the producer can be
        // blocked on a full render queue, or the model can already be terminal.
        let mut graceful = Box::pin(async move {
            let completed = finished.recv().await;
            let flushed = join_cli_renderer(render_task).await;
            (completed, flushed)
        });
        let mut cancelled = false;
        let (completed, render_result) = tokio::select! { biased;
            () = cancellation.cancelled() => {
                cancelled = true;
                tokio::time::timeout(Duration::from_secs(1), &mut graceful).await
                    .unwrap_or_else(|_| {
                        turn.abort();
                        (None, Ok(()))
                    })
            },
            result = &mut graceful => result,
        };
        drop(graceful);
        rendering_stopped.cancel();
        let exit = if cancelled {
            130
        } else if let Err(error) = render_result {
            report_error(&error)
        } else if let Some(MessageTaskFinished {
            result,
            cancellation_requested,
            ..
        }) = completed
        {
            match result {
                Ok(outcome) => {
                    if !cancellation_requested {
                        report_terminal_diagnostic(&outcome);
                    }
                    if cancellation_requested {
                        130
                    } else {
                        u8::from(outcome != TurnOutcome::Completed)
                    }
                }
                Err(error) => report_error(&error),
            }
        } else {
            report_error(&RsiError::Run("turn worker exited".into()))
        };
        signal.abort();
        let _ = signal.await;
        let _ = turn.await;
        exit
    });
    tokio::select! { biased; () = work.stop.cancelled() => 0, exit = &mut running => exit }
}

async fn headless_inputs(
    task: String,
    images: Vec<PathBuf>,
    media: &dyn rsi_media_protocol::Media,
    work: &ApplicationWork,
) -> Result<Vec<MessageInput>> {
    let mut content = vec![MessageInput::Text { text: task }];
    for source in load_cli_images(images, work).await? {
        let media = media
            .import_image(source)
            .await
            .map_err(|error| RsiError::Run(format!("image upload: {error}")))?;
        content.push(MessageInput::Image { media });
    }
    Ok(content)
}

pub(crate) fn generated_cli_session_id() -> Result<SessionId> {
    let mut entropy = [0_u8; 16];
    getrandom::fill(&mut entropy)
        .map_err(|error| RsiError::Boot(format!("OS entropy failed: {error}")))?;
    SessionId::new(format!("session-{:032x}", u128::from_le_bytes(entropy)))
        .map_err(|error| RsiError::Boot(error.to_string()))
}

pub(crate) fn generated_cli_message_id() -> Result<MessageId> {
    let mut entropy = [0_u8; 16];
    getrandom::fill(&mut entropy)
        .map_err(|error| RsiError::Boot(format!("OS entropy failed: {error}")))?;
    MessageId::new(format!("message-{:032x}", u128::from_le_bytes(entropy)))
        .map_err(|error| RsiError::Boot(error.to_string()))
}

pub(crate) const MAXIMUM_CLI_IMAGE_BYTES: usize = 64 * 1024 * 1024;

pub(crate) async fn load_cli_images(
    paths: Vec<PathBuf>,
    work: &ApplicationWork,
) -> Result<Vec<bytes::Bytes>> {
    if paths.len().saturating_add(1) > MAXIMUM_AGENT_MESSAGE_CONTENT_BLOCKS {
        return Err(usage(format!(
            "one message may contain at most {} images alongside its task",
            MAXIMUM_AGENT_MESSAGE_CONTENT_BLOCKS.saturating_sub(1)
        )));
    }
    let token = work.tasks.token();
    tokio::task::spawn_blocking(move || {
        let _token = token;
        read_cli_images(paths)
    })
    .await
    .map_err(|error| RsiError::Boot(format!("image input worker failed: {error}")))?
}

pub(crate) fn read_cli_images(paths: Vec<PathBuf>) -> Result<Vec<bytes::Bytes>> {
    let mut total = 0_usize;
    let mut images = Vec::with_capacity(paths.len());
    for path in paths {
        let file = open_cli_image(&path).map_err(|error| {
            RsiError::Boot(format!(
                "image `{}` cannot be opened: {error}",
                path.display()
            ))
        })?;
        let metadata = file.metadata().map_err(|error| {
            RsiError::Boot(format!(
                "image `{}` metadata is unavailable: {error}",
                path.display()
            ))
        })?;
        if !metadata.is_file() {
            return Err(usage(format!(
                "image `{}` is not a regular file",
                path.display()
            )));
        }
        #[cfg(unix)]
        {
            use std::os::fd::AsFd as _;
            let flags = rustix::fs::fcntl_getfl(file.as_fd()).map_err(|error| {
                RsiError::Boot(format!(
                    "image `{}` flags are unavailable: {error}",
                    path.display()
                ))
            })?;
            rustix::fs::fcntl_setfl(file.as_fd(), flags & !rustix::fs::OFlags::NONBLOCK).map_err(
                |error| {
                    RsiError::Boot(format!(
                        "image `{}` blocking mode cannot be restored: {error}",
                        path.display()
                    ))
                },
            )?;
        }
        let remaining = MAXIMUM_CLI_IMAGE_BYTES.saturating_sub(total);
        if metadata.len() > u64::try_from(remaining).unwrap_or(u64::MAX) {
            return Err(usage(format!(
                "input images exceed {MAXIMUM_CLI_IMAGE_BYTES} aggregate bytes"
            )));
        }
        let mut bytes = Vec::with_capacity(usize::try_from(metadata.len()).unwrap_or(remaining));
        file.take(
            u64::try_from(remaining)
                .unwrap_or(u64::MAX)
                .saturating_add(1),
        )
        .read_to_end(&mut bytes)
        .map_err(|error| {
            RsiError::Boot(format!(
                "image `{}` cannot be read: {error}",
                path.display()
            ))
        })?;
        if bytes.is_empty() {
            return Err(usage(format!("image `{}` is empty", path.display())));
        }
        if bytes.len() > remaining {
            return Err(usage(format!(
                "input images exceed {MAXIMUM_CLI_IMAGE_BYTES} aggregate bytes"
            )));
        }
        total = total
            .checked_add(bytes.len())
            .ok_or_else(|| usage("input image byte total overflowed"))?;
        images.push(bytes.into());
    }
    Ok(images)
}

pub(crate) fn open_cli_image(path: &std::path::Path) -> std::io::Result<std::fs::File> {
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.custom_flags(libc::O_CLOEXEC | libc::O_NONBLOCK | libc::O_NOFOLLOW);
    }
    options.open(path)
}

#[derive(Debug)]
pub(crate) enum SessionInput {
    Line(String),
    TooLarge,
    InvalidUtf8,
    Error(String),
    Eof,
}

pub(crate) const SESSION_INPUT_CHANNEL_CAPACITY: usize = 1;

#[cfg(test)]
pub(crate) fn spawn_session_input_reader(
    mut reader: impl std::io::BufRead + Send + 'static,
) -> tokio::sync::mpsc::Receiver<SessionInput> {
    let (sender, receiver) = tokio::sync::mpsc::channel(SESSION_INPUT_CHANNEL_CAPACITY);
    std::thread::spawn(move || forward_session_input(&mut reader, &sender));
    receiver
}

pub(crate) fn forward_session_input(
    reader: &mut impl std::io::BufRead,
    sender: &tokio::sync::mpsc::Sender<SessionInput>,
) {
    loop {
        let input = read_bounded_stdin_line(reader);
        let terminal = matches!(input, SessionInput::Eof | SessionInput::Error(_));
        if sender.blocking_send(input).is_err() || terminal {
            break;
        }
    }
}

pub(crate) fn read_bounded_stdin_line(reader: &mut impl std::io::BufRead) -> SessionInput {
    let mut bytes = Vec::new();
    let mut oversized = false;
    loop {
        let available = match reader.fill_buf() {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(error) => return SessionInput::Error(error.to_string()),
        };
        if available.is_empty() {
            if bytes.is_empty() && !oversized {
                return SessionInput::Eof;
            }
            break;
        }
        let consumed = available
            .iter()
            .position(|byte| *byte == b'\n')
            .map_or(available.len(), |index| index + 1);
        if !oversized {
            if bytes.len().saturating_add(consumed) > MAXIMUM_TURN_TEXT_BYTES + 1 {
                oversized = true;
                bytes.clear();
            } else {
                bytes.extend_from_slice(&available[..consumed]);
            }
        }
        let ended = available[..consumed].last() == Some(&b'\n');
        reader.consume(consumed);
        if ended {
            break;
        }
    }
    if oversized {
        return SessionInput::TooLarge;
    }
    while matches!(bytes.last(), Some(b'\n' | b'\r')) {
        bytes.pop();
    }
    String::from_utf8(bytes).map_or(SessionInput::InvalidUtf8, SessionInput::Line)
}

pub(crate) async fn arm_signal(
    cancellation: CancellationToken,
    work: &ApplicationWork,
) -> Result<JoinHandle<()>> {
    let (armed_tx, armed_rx) = tokio::sync::oneshot::channel();
    let stop = work.stop.clone();
    let task = work.tasks.spawn(async move {
        let mut signal = Box::pin(tokio::signal::ctrl_c());
        let initial = std::future::poll_fn(|context| {
            Poll::Ready(match signal.as_mut().poll(context) {
                Poll::Ready(result) => Some(result),
                Poll::Pending => None,
            })
        })
        .await;
        match initial {
            Some(Ok(())) => {
                cancellation.cancel();
                let _ignored = armed_tx.send(Ok(()));
            }
            Some(Err(error)) => {
                let _ignored = armed_tx.send(Err(error));
            }
            None => {
                let _ignored = armed_tx.send(Ok(()));
                tokio::select! { biased; () = stop.cancelled() => {}, result = signal => if result.is_ok() { cancellation.cancel(); } }
            }
        }
    });
    armed_rx
        .await
        .map_err(|_| RsiError::Boot("SIGINT listener exited before registration".into()))?
        .map_err(|error| RsiError::Boot(format!("failed to register SIGINT listener: {error}")))?;
    Ok(task)
}

pub(crate) fn report_terminal_diagnostic(outcome: &TurnOutcome) {
    match outcome {
        TurnOutcome::Failed { code, message }
        | TurnOutcome::PartialFailed { code, message, .. } => eprintln!("{code}: {message}"),
        TurnOutcome::Interrupted { reason, .. } => eprintln!("interrupted: {reason}"),
        TurnOutcome::BudgetExceeded {
            dimension,
            consumed,
            limit,
        } => {
            eprintln!("turn budget exceeded for {dimension:?}: consumed {consumed}, limit {limit}");
        }
        TurnOutcome::Completed | TurnOutcome::Cancelled => {}
    }
}

pub(crate) fn write_live_event(
    stdout: &mut impl Write,
    mode: OutputMode,
    event: &CliEvent,
    wrote_text: &mut bool,
    text_ends_newline: &mut bool,
) -> Result<()> {
    match mode {
        OutputMode::Jsonl => write_jsonl_event(stdout, event),
        OutputMode::Text => write_text_event(stdout, event, wrote_text, text_ends_newline),
    }
}

pub(crate) fn write_jsonl_event(stdout: &mut impl Write, event: &CliEvent) -> Result<()> {
    let line = event
        .json_line()
        .map_err(|error| RsiError::Run(error.to_string()))?;
    stdout
        .write_all(line.as_bytes())
        .and_then(|()| stdout.write_all(b"\n"))
        .and_then(|()| stdout.flush())
        .map_err(|error| stdout_write_error(&error))
}

pub(crate) fn write_text_event(
    stdout: &mut impl Write,
    event: &CliEvent,
    wrote_text: &mut bool,
    text_ends_newline: &mut bool,
) -> Result<()> {
    if let CliEvent::Notice { kind, value } = event
        && is_query_result(kind)
    {
        let text = serde_json::to_string_pretty(value)
            .map_err(|error| RsiError::Run(error.to_string()))?;
        if *wrote_text && !*text_ends_newline {
            write_text_delta(stdout, "\n", wrote_text, text_ends_newline)?;
        }
        write_text_delta(stdout, &text, wrote_text, text_ends_newline)?;
        return write_text_delta(stdout, "\n", wrote_text, text_ends_newline);
    }
    let CliEvent::Fact { fact, .. } = event else {
        return Ok(());
    };
    match fact.body() {
        SessionFactBody::ModelEvent {
            event:
                LanguageEvent::ContentDelta {
                    delta: ContentDelta::Text(text),
                    ..
                },
            ..
        } => write_text_delta(stdout, text, wrote_text, text_ends_newline),
        SessionFactBody::ToolResult { result, .. } => {
            for content in &result.content {
                if let ToolContent::Image { media } = content {
                    write_media_reference(
                        stdout,
                        media.id.as_str(),
                        wrote_text,
                        text_ends_newline,
                    )?;
                }
            }
            Ok(())
        }
        SessionFactBody::ImageOutput { media, .. } => {
            write_media_reference(stdout, media.id.as_str(), wrote_text, text_ends_newline)
        }
        _ => Ok(()),
    }
}

/// Neutralizes terminal and bidi controls while preserving line breaks and joiners.
pub fn terminal_text(text: &str) -> String {
    text.chars().map(terminal_character).collect()
}

fn terminal_character(character: char) -> char {
    if character.is_control() && !matches!(character, '\n' | '\t')
        || matches!(character, '\u{061c}' | '\u{200e}'..='\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}')
    {
        '\u{fffd}'
    } else {
        character
    }
}

fn write_status_event(stderr: &mut impl Write, event: &CliEvent) -> Result<()> {
    let text = match event {
        CliEvent::Notice { kind, .. } if is_query_result(kind) => return Ok(()),
        CliEvent::Control { .. } => return Ok(()),
        CliEvent::Interactions { snapshot } => format!(
            "interactions: {}",
            serde_json::to_string(snapshot).map_err(|error| RsiError::Run(error.to_string()))?
        ),
        CliEvent::Notice { kind, value } => format!("{kind}: {value}"),
        CliEvent::Message {
            message_id,
            accepted_control_seq,
            ..
        } => format!("accepted: {message_id} (control {accepted_control_seq})"),
        CliEvent::Turn { turn_id, .. } => format!("turn: {turn_id}"),
        CliEvent::Outcome { outcome, .. } => format!("outcome: {outcome:?}"),
        CliEvent::Fact { fact, .. } => match fact.body() {
            SessionFactBody::ToolIntent {
                name, arguments, ..
            } => format!("tool {name}: {arguments}"),
            SessionFactBody::ToolResult { result, .. } => result
                .content
                .iter()
                .filter_map(|content| match content {
                    ToolContent::Text { text } => Some(text.as_str()),
                    ToolContent::Image { .. } => None,
                })
                .collect::<Vec<_>>()
                .join("\n"),
            _ => return Ok(()),
        },
    };
    writeln!(stderr, "{}", terminal_text(&text))
        .and_then(|()| stderr.flush())
        .map_err(|error| RsiError::Run(format!("stderr write failed: {error}")))
}

fn is_query_result(kind: &str) -> bool {
    matches!(
        kind,
        "sessions"
            | "history"
            | "inspection"
            | "output"
            | "approvals"
            | "questions"
            | "commands"
            | "command_result"
    )
}

pub(crate) fn write_text_delta(
    stdout: &mut impl Write,
    text: &str,
    wrote_text: &mut bool,
    text_ends_newline: &mut bool,
) -> Result<()> {
    stdout
        .write_all(terminal_text(text).as_bytes())
        .and_then(|()| stdout.flush())
        .map_err(|error| stdout_write_error(&error))?;
    *wrote_text = true;
    *text_ends_newline = text.ends_with('\n');
    Ok(())
}

pub(crate) fn write_media_reference(
    stdout: &mut impl Write,
    media_id: &str,
    wrote_text: &mut bool,
    text_ends_newline: &mut bool,
) -> Result<()> {
    if *wrote_text && !*text_ends_newline {
        stdout
            .write_all(b"\n")
            .map_err(|error| stdout_write_error(&error))?;
    }
    writeln!(stdout, "media:{media_id}")
        .and_then(|()| stdout.flush())
        .map_err(|error| stdout_write_error(&error))?;
    *wrote_text = true;
    *text_ends_newline = true;
    Ok(())
}

pub(crate) fn stdout_write_error(error: &std::io::Error) -> RsiError {
    RsiError::Run(format!("stdout write failed: {error}"))
}

/// Writes and flushes one plain launcher or terminal line.
pub fn write_text_line(value: &str) -> Result<()> {
    let stdout = std::io::stdout();
    let mut stdout = stdout.lock();
    writeln!(stdout, "{value}")
        .and_then(|()| stdout.flush())
        .map_err(|error| stdout_write_error(&error))
}

pub(crate) fn report_error(error: &RsiError) -> u8 {
    eprintln!("error: {error}");
    error.exit_code()
}
