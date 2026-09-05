use super::*;

pub(super) enum ParsedApplication {
    Headless(Command),
    Session(SessionCommand),
}

pub(super) async fn run_application(invocation: ApplicationInvocation) -> u8 {
    if invocation
        .arguments
        .iter()
        .take_while(|argument| argument.to_str() != Some("--"))
        .any(|argument| matches!(argument.to_str(), Some("-h" | "--help")))
    {
        print!(
            "Usage:\n  rsi --profile headless [TASK | --stdin] [HEADLESS OPTIONS]\n  rsi --profile session [--cwd PATH] [--resume SESSION | --session-id SESSION] [--agent-preset ID] [--trust-workspace] [--output text|jsonl]\n"
        );
        return 0;
    }
    let paths = match standard_paths() {
        Ok(paths) => paths,
        Err(error) => return report_error(&error),
    };
    let catalog = ProfileCatalog::new(paths.clone());
    let application_profile = match catalog.application(&invocation.profile) {
        Ok(profile) => profile,
        Err(error) => return report_error(&profile_management_error(error)),
    };
    let host_profile = match catalog.host(application_profile.profile.host_profile()) {
        Ok(profile) => profile,
        Err(error) => return report_error(&profile_management_error(error)),
    };
    let parsed = match application_profile.profile.application() {
        ApplicationKind::Headless => {
            let mut arguments = Vec::with_capacity(invocation.arguments.len() + 1);
            arguments.push(OsString::from("run"));
            arguments.extend(invocation.arguments);
            match Command::parse(arguments) {
                Ok(Parse::Run(command)) => ParsedApplication::Headless(command),
                Ok(_) => return report_error(&usage("invalid headless application arguments")),
                Err(error) => return report_error(&error),
            }
        }
        ApplicationKind::Session => match SessionCommand::parse(invocation.arguments) {
            Ok(command) => ParsedApplication::Session(command),
            Err(error) => return report_error(&error),
        },
    };
    let (composition, presets) = match prepare_standard_composition(paths).await {
        Ok(prepared) => prepared,
        Err(error) => return report_error(&error),
    };
    let connection = match connect_or_embed_session_host(composition, &host_profile).await {
        Ok(connection) => connection,
        Err(error) => {
            let exit = report_error(&error);
            return shutdown_agent_preset_manager(presets, exit, 2, "application bootstrap").await;
        }
    };
    let (presets, preset_shutdown_exit) =
        if connection.mode() == rsi::SessionHostConnectionMode::Remote {
            (
                None,
                shutdown_agent_preset_manager(presets, 0, 1, "remote application bootstrap").await,
            )
        } else {
            (Some(presets), 0)
        };
    let application = connection.application();
    let exit = match parsed {
        ParsedApplication::Headless(command) => {
            run_headless_application(application, command).await
        }
        ParsedApplication::Session(command) => {
            run_session_application(application, connection.mode(), command).await
        }
    };
    let exit = match connection.shutdown().await {
        Ok(()) => exit,
        Err(error) if exit == 0 => report_error(&error),
        Err(error) => {
            eprintln!("error: {error}");
            exit
        }
    };
    let exit = if exit == 0 {
        preset_shutdown_exit
    } else {
        exit
    };
    match presets {
        Some(presets) => shutdown_agent_preset_manager(presets, exit, 1, "application").await,
        None => exit,
    }
}

#[derive(Clone, Debug)]
pub(super) struct SessionCommand {
    cwd: Option<PathBuf>,
    resume: Option<SessionId>,
    session_id: Option<SessionId>,
    agent_preset: Option<AgentPresetId>,
    trust_workspace: bool,
    output: OutputMode,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(super) enum OutputMode {
    #[default]
    Text,
    Jsonl,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) enum SessionSelection {
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
pub(super) struct HeadlessTurnOptions {
    pub(super) task: String,
    pub(super) session: SessionSelection,
    pub(super) message_id: Option<MessageId>,
    pub(super) images: Vec<PathBuf>,
    pub(super) model: Option<ModelRef>,
    pub(super) sandbox: Option<SandboxMode>,
    pub(super) output: OutputMode,
}

#[derive(Clone, Debug)]
pub(super) enum CliEvent {
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
    Fact {
        session_id: SessionId,
        fact: Arc<SessionFact>,
        durable_seq: u64,
    },
    Outcome {
        session_id: SessionId,
        turn_id: TurnId,
        outcome: TurnOutcome,
        durable_seq: u64,
    },
}

impl CliEvent {
    fn json_line(&self) -> std::result::Result<String, serde_json::Error> {
        match self {
            Self::Message {
                session_id,
                message_id,
                accepted_control_seq,
            } => serde_json::to_string(&MessageEnvelope {
                version: 3,
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
                version: 3,
                kind: "turn",
                session_id,
                message_id,
                turn_id,
                entered_fact_seq: *entered_fact_seq,
            }),
            Self::Fact {
                session_id,
                fact,
                durable_seq,
            } => serde_json::to_string(&LiveFactEnvelope {
                version: 3,
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
                version: 3,
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
pub(super) struct MessageEnvelope<'a> {
    version: u32,
    #[serde(rename = "type")]
    kind: &'static str,
    session_id: &'a SessionId,
    message_id: &'a MessageId,
    accepted_control_seq: u64,
}

#[derive(Serialize)]
pub(super) struct TurnEnvelope<'a> {
    version: u32,
    #[serde(rename = "type")]
    kind: &'static str,
    session_id: &'a SessionId,
    message_id: &'a MessageId,
    turn_id: &'a TurnId,
    entered_fact_seq: u64,
}

#[derive(Serialize)]
pub(super) struct LiveFactEnvelope<'a> {
    version: u32,
    #[serde(rename = "type")]
    kind: &'static str,
    session_id: &'a SessionId,
    fact: &'a SessionFact,
    durable_seq: u64,
}

#[derive(Serialize)]
pub(super) struct OutcomeEnvelope<'a> {
    version: u32,
    #[serde(rename = "type")]
    kind: &'static str,
    session_id: &'a SessionId,
    turn_id: &'a TurnId,
    outcome: &'a TurnOutcome,
    durable_seq: u64,
}

impl SessionCommand {
    pub(super) fn parse(arguments: Vec<OsString>) -> rsi::Result<Self> {
        let mut command = Self {
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

pub(super) async fn resolve_application_handle(
    application: &Arc<dyn SessionApplication>,
    session: SessionSelection,
) -> rsi::Result<Arc<dyn SessionHandle>> {
    match session {
        SessionSelection::Fresh {
            cwd,
            session_id,
            agent_preset_id,
            workspace_trust,
        } => application
            .create(CreateSession {
                cwd,
                session_id,
                agent_preset_id,
                workspace_trust,
            })
            .await
            .map_err(|error| RsiError::Boot(error.to_string())),
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

pub(super) const CLI_RENDER_CHANNEL_CAPACITY: usize = 32;
pub(super) const TURN_COMPLETION_CHANNEL_CAPACITY: usize = 1;

#[derive(Debug)]
pub(super) enum CliRenderMessage {
    Event(CliEvent),
    FinishLine,
}

#[derive(Debug)]
pub(super) struct MessageTaskFinished {
    message_id: MessageId,
    result: rsi::Result<TurnOutcome>,
    cancellation_requested: bool,
}

pub(super) async fn submit_with_reconciliation(
    handle: &Arc<dyn SessionHandle>,
    request: SubmitInput,
) -> rsi_session::Result<rsi_agent_turn_protocol::MessageReceipt> {
    let message_id = request.message_id.clone();
    let unknown = match handle.submit(request.clone()).await {
        Err(error @ SessionApplicationError::MessageOutcomeUnknown { .. }) => error,
        result => return result,
    };
    match handle.message_status(&message_id).await {
        Ok(receipt) => return Ok(receipt),
        Err(SessionApplicationError::NotFound(_)) => {}
        Err(_) => return Err(unknown),
    }
    match handle.submit(request).await {
        Err(error @ SessionApplicationError::MessageOutcomeUnknown { .. }) => {
            handle.message_status(&message_id).await.or(Err(error))
        }
        result => result,
    }
}

#[allow(clippy::too_many_lines)] // One task owns the complete submit, render, cancel, and completion lifecycle.
pub(super) async fn drive_application_turn(
    handle: Arc<dyn SessionHandle>,
    request: SubmitInput,
    cancellation: CancellationToken,
    rendering_stopped: CancellationToken,
    renderer: tokio::sync::mpsc::Sender<CliRenderMessage>,
    completion: tokio::sync::mpsc::Sender<MessageTaskFinished>,
) {
    let message_id = request.message_id.clone();
    let result = async {
        let receipt = submit_with_reconciliation(&handle, request)
            .await
            .map_err(|error| RsiError::Run(error.to_string()))?;
        send_cli_event(
            &renderer,
            &rendering_stopped,
            &cancellation,
            CliEvent::Message {
                session_id: receipt.session_id.clone(),
                message_id: receipt.message_id.clone(),
                accepted_control_seq: receipt.accepted_control_seq,
            },
        )
        .await?;
        let mut message_cancellation_sent = false;
        let mut claim_observation = if matches!(receipt.state, MessageState::Pending) {
            Some(
                handle
                    .observe(ObservationCursor {
                        control_seq: receipt.accepted_control_seq,
                        fact_seq: receipt.observed_fact_seq,
                    })
                    .await
                    .map_err(|error| RsiError::Run(error.to_string()))?,
            )
        } else {
            None
        };
        let (turn_id, entered_fact_seq) = loop {
            match &receipt.state {
                MessageState::Pending => {
                    tokio::select! {
                        biased;
                        () = cancellation.cancelled(), if !message_cancellation_sent => {
                            message_cancellation_sent = true;
                            let cancelled = handle
                                .cancel(CancelTarget::Message(message_id.clone()), None)
                                .await
                                .map_err(|error| RsiError::Run(error.to_string()))?;
                            if cancelled.accepted {
                                return Ok(TurnOutcome::Cancelled);
                            }
                        }
                        update = futures_util::StreamExt::next(
                            claim_observation
                                .as_mut()
                                .expect("pending receipt owns a claim observation"),
                        ) => {
                            let update = update
                                .ok_or_else(|| RsiError::Run("Session observation ended before the message claim".into()))?
                                .map_err(|error| RsiError::Run(error.to_string()))?;
                            if let SessionObservation::Control { record, .. } = update {
                                match record.body() {
                                    AgentControlRecordBody::MessageClaimed {
                                        message_id: observed,
                                        turn_id,
                                        entered_fact_seq,
                                        ..
                                    } if observed == &message_id => {
                                        break (turn_id.clone(), *entered_fact_seq);
                                    }
                                    AgentControlRecordBody::MessageDiscarded {
                                        message_id: observed,
                                        reason,
                                    } if observed == &message_id => {
                                        if cancellation.is_cancelled() {
                                            return Ok(TurnOutcome::Cancelled);
                                        }
                                        return Err(RsiError::Run(format!(
                                            "message `{message_id}` was discarded before execution: {reason:?}"
                                        )));
                                    }
                                    _ => {}
                                }
                            }
                        }
                    }
                }
                MessageState::Claimed {
                    turn_id,
                    entered_fact_seq,
                    ..
                } => break (turn_id.clone(), *entered_fact_seq),
                MessageState::Discarded { reason, .. } => {
                    if cancellation.is_cancelled() {
                        return Ok(TurnOutcome::Cancelled);
                    }
                    return Err(RsiError::Run(format!(
                        "message `{message_id}` was discarded before execution: {reason:?}"
                    )));
                }
            }
        };
        drop(claim_observation);
        let mut cancellation_sent = false;
        send_cli_event(
            &renderer,
            &rendering_stopped,
            &cancellation,
            CliEvent::Turn {
                session_id: receipt.session_id.clone(),
                message_id: receipt.message_id.clone(),
                turn_id: turn_id.clone(),
                entered_fact_seq,
            },
        )
        .await?;
        let mut observation = handle
            .observe(ObservationCursor {
                control_seq: receipt.accepted_control_seq,
                fact_seq: entered_fact_seq,
            })
            .await
            .map_err(|error| RsiError::Run(error.to_string()))?;
        loop {
            tokio::select! {
                biased;
                () = cancellation.cancelled(), if !cancellation_sent => {
                    cancellation_sent = true;
                    handle.cancel(
                        CancelTarget::Turn(turn_id.clone()),
                        Some("client interrupt".into()),
                    ).await
                        .map_err(|error| RsiError::Run(error.to_string()))?;
                }
                update = futures_util::StreamExt::next(&mut observation) => {
                    let update = update
                        .ok_or_else(|| RsiError::Run("Session observation ended before a terminal Fact".into()))?
                        .map_err(|error| RsiError::Run(error.to_string()))?;
                    if let SessionObservation::Fact { fact, durable_fact_seq } = update {
                        let terminal = match fact.body() {
                            SessionFactBody::TurnTerminal { turn_id: observed, outcome }
                                if observed == &turn_id => Some(outcome.clone()),
                            _ => None,
                        };
                        send_cli_event(
                            &renderer,
                            &rendering_stopped,
                            &cancellation,
                            CliEvent::Fact {
                                session_id: receipt.session_id.clone(),
                                fact,
                                durable_seq: durable_fact_seq,
                            },
                        )
                        .await?;
                        if let Some(outcome) = terminal {
                            send_cli_event(
                                &renderer,
                                &rendering_stopped,
                                &cancellation,
                                CliEvent::Outcome {
                                    session_id: receipt.session_id,
                                    turn_id: turn_id.clone(),
                                    outcome: outcome.clone(),
                                    durable_seq: durable_fact_seq,
                                },
                            )
                            .await?;
                            send_finish_line(&renderer, &rendering_stopped, &cancellation).await?;
                            return Ok(outcome);
                        }
                    }
                }
            }
        }
    }
    .await;
    let _ = completion
        .send(MessageTaskFinished {
            message_id,
            result,
            cancellation_requested: cancellation.is_cancelled(),
        })
        .await;
}

pub(super) async fn send_cli_event(
    renderer: &tokio::sync::mpsc::Sender<CliRenderMessage>,
    rendering_stopped: &CancellationToken,
    turn_cancellation: &CancellationToken,
    event: CliEvent,
) -> rsi::Result<()> {
    if rendering_stopped.is_cancelled() {
        return Ok(());
    }
    tokio::select! {
        biased;
        () = rendering_stopped.cancelled() => Ok(()),
        result = renderer.send(CliRenderMessage::Event(event)) => result
            .map_err(|_| RsiError::Run("terminal renderer stopped before the turn ended".into())),
        () = turn_cancellation.cancelled() => Ok(()),
    }
}

pub(super) async fn send_finish_line(
    renderer: &tokio::sync::mpsc::Sender<CliRenderMessage>,
    rendering_stopped: &CancellationToken,
    turn_cancellation: &CancellationToken,
) -> rsi::Result<()> {
    if rendering_stopped.is_cancelled() {
        return Ok(());
    }
    tokio::select! {
        biased;
        () = rendering_stopped.cancelled() => Ok(()),
        result = renderer.send(CliRenderMessage::FinishLine) => result
            .map_err(|_| RsiError::Run("terminal renderer stopped before the turn ended".into())),
        () = turn_cancellation.cancelled() => Ok(()),
    }
}

#[derive(Debug, Default)]
pub(super) struct CliRenderState {
    wrote_text: bool,
    text_ends_newline: bool,
}

impl CliRenderState {
    fn write(&mut self, output: OutputMode, event: &CliEvent) -> rsi::Result<()> {
        let stdout = std::io::stdout();
        let mut stdout = stdout.lock();
        write_live_event(
            &mut stdout,
            output,
            event,
            &mut self.wrote_text,
            &mut self.text_ends_newline,
        )
    }

    fn finish_line(&mut self, output: OutputMode) -> rsi::Result<()> {
        if output == OutputMode::Text && self.wrote_text && !self.text_ends_newline {
            write_text_line("")?;
            self.text_ends_newline = true;
        }
        Ok(())
    }
}

pub(super) fn spawn_cli_renderer(
    output: OutputMode,
    mut receiver: tokio::sync::mpsc::Receiver<CliRenderMessage>,
) -> tokio::sync::oneshot::Receiver<rsi::Result<()>> {
    let (outcome, finished) = tokio::sync::oneshot::channel();
    std::thread::spawn(move || {
        let mut renderer = CliRenderState::default();
        let result = (|| {
            while let Some(message) = receiver.blocking_recv() {
                match message {
                    CliRenderMessage::Event(event) => renderer.write(output, &event)?,
                    CliRenderMessage::FinishLine => renderer.finish_line(output)?,
                }
            }
            Ok(())
        })();
        let _ = outcome.send(result);
    });
    finished
}

pub(super) async fn join_cli_renderer(
    renderer: tokio::sync::oneshot::Receiver<rsi::Result<()>>,
) -> rsi::Result<()> {
    renderer
        .await
        .map_err(|_| RsiError::Run("terminal renderer panicked".into()))?
}

pub(super) async fn run_headless_application(
    application: Arc<dyn SessionApplication>,
    command: Command,
) -> u8 {
    let task = match command.task().await {
        Ok(task) => task,
        Err(error) => return report_error(&error),
    };
    let options = match command.options(task) {
        Ok(options) => options,
        Err(error) => return report_error(&error),
    };
    let mut content = vec![MessageInput::Text { text: options.task }];
    match load_cli_images(options.images).await {
        Ok(images) => content.extend(images),
        Err(error) => return report_error(&error),
    }
    let handle = match resolve_application_handle(&application, options.session).await {
        Ok(handle) => handle,
        Err(error) => return report_error(&error),
    };
    let message_id = match options.message_id.map_or_else(generated_cli_message_id, Ok) {
        Ok(id) => id,
        Err(error) => return report_error(&error),
    };
    let cancellation = CancellationToken::new();
    let signal = match arm_signal(cancellation.clone()).await {
        Ok(signal) => signal,
        Err(error) => return report_error(&error),
    };
    let rendering_stopped = CancellationToken::new();
    let (renderer, render_receiver) = tokio::sync::mpsc::channel(CLI_RENDER_CHANNEL_CAPACITY);
    let render_task = spawn_cli_renderer(options.output, render_receiver);
    let (completion, mut finished) = tokio::sync::mpsc::channel(TURN_COMPLETION_CHANNEL_CAPACITY);
    tokio::spawn(drive_application_turn(
        handle,
        SubmitInput {
            message_id,
            content,
            model: options.model,
            sandbox: options.sandbox,
        },
        cancellation,
        rendering_stopped,
        renderer,
        completion,
    ));
    let completed = finished.recv().await;
    let cancelled = completed
        .as_ref()
        .is_some_and(|finished| finished.cancellation_requested);
    let render_result = if cancelled {
        tokio::time::timeout(Duration::from_secs(1), join_cli_renderer(render_task))
            .await
            .unwrap_or(Ok(()))
    } else {
        join_cli_renderer(render_task).await
    };
    let exit = if let Err(error) = render_result {
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
    exit
}

pub(super) fn generated_cli_message_id() -> rsi::Result<MessageId> {
    let mut entropy = [0_u8; 16];
    getrandom::fill(&mut entropy)
        .map_err(|error| RsiError::Boot(format!("OS entropy failed: {error}")))?;
    MessageId::new(format!("message-{:032x}", u128::from_le_bytes(entropy)))
        .map_err(|error| RsiError::Boot(error.to_string()))
}

pub(super) async fn load_cli_images(paths: Vec<PathBuf>) -> rsi::Result<Vec<MessageInput>> {
    if paths.len().saturating_add(1) > MAXIMUM_AGENT_MESSAGE_CONTENT_BLOCKS {
        return Err(usage(format!(
            "one message may contain at most {} images alongside its task",
            MAXIMUM_AGENT_MESSAGE_CONTENT_BLOCKS.saturating_sub(1)
        )));
    }
    tokio::task::spawn_blocking(move || read_cli_images(paths))
        .await
        .map_err(|error| RsiError::Boot(format!("image input worker failed: {error}")))?
}

pub(super) fn read_cli_images(paths: Vec<PathBuf>) -> rsi::Result<Vec<MessageInput>> {
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
        let remaining = MAXIMUM_SESSION_INPUT_IMAGE_BYTES.saturating_sub(total);
        if metadata.len() > u64::try_from(remaining).unwrap_or(u64::MAX) {
            return Err(usage(format!(
                "input images exceed {MAXIMUM_SESSION_INPUT_IMAGE_BYTES} aggregate bytes"
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
                "input images exceed {MAXIMUM_SESSION_INPUT_IMAGE_BYTES} aggregate bytes"
            )));
        }
        total = total
            .checked_add(bytes.len())
            .ok_or_else(|| usage("input image byte total overflowed"))?;
        images.push(MessageInput::Image {
            bytes: Arc::from(bytes),
        });
    }
    Ok(images)
}

pub(super) fn open_cli_image(path: &std::path::Path) -> std::io::Result<std::fs::File> {
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
pub(super) enum SessionInput {
    Line(String),
    TooLarge,
    InvalidUtf8,
    Error(String),
    Eof,
}

pub(super) const SESSION_INPUT_CHANNEL_CAPACITY: usize = 1;
pub(super) const MAXIMUM_QUEUED_SESSION_TURNS: usize = 16;

pub(super) fn spawn_session_input() -> tokio::sync::mpsc::Receiver<SessionInput> {
    let (sender, receiver) = tokio::sync::mpsc::channel(SESSION_INPUT_CHANNEL_CAPACITY);
    std::thread::spawn(move || {
        let stdin = std::io::stdin();
        let mut stdin = stdin.lock();
        forward_session_input(&mut stdin, &sender);
    });
    receiver
}

#[cfg(test)]
pub(super) fn spawn_session_input_reader(
    mut reader: impl std::io::BufRead + Send + 'static,
) -> tokio::sync::mpsc::Receiver<SessionInput> {
    let (sender, receiver) = tokio::sync::mpsc::channel(SESSION_INPUT_CHANNEL_CAPACITY);
    std::thread::spawn(move || forward_session_input(&mut reader, &sender));
    receiver
}

pub(super) fn forward_session_input(
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

pub(super) fn read_bounded_stdin_line(reader: &mut impl std::io::BufRead) -> SessionInput {
    let mut bytes = Vec::new();
    let mut oversized = false;
    loop {
        let available = match reader.fill_buf() {
            Ok(bytes) => bytes,
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

#[allow(clippy::too_many_lines)] // One REPL loop owns FIFO admission, signals, rendering, and detach.
pub(super) async fn run_session_application(
    application: Arc<dyn SessionApplication>,
    mode: rsi::SessionHostConnectionMode,
    command: SessionCommand,
) -> u8 {
    let selection = match command.resume {
        Some(session_id) => SessionSelection::Resume {
            session_id,
            cwd: command.cwd,
        },
        None => SessionSelection::Fresh {
            cwd: match command.cwd {
                Some(cwd) => cwd,
                None => match std::env::current_dir() {
                    Ok(cwd) => cwd,
                    Err(error) => return report_error(&RsiError::Boot(error.to_string())),
                },
            },
            session_id: command.session_id,
            agent_preset_id: command.agent_preset,
            workspace_trust: if command.trust_workspace {
                WorkspaceTrust::Trusted
            } else {
                WorkspaceTrust::Untrusted
            },
        },
    };
    let handle = match resolve_application_handle(&application, selection).await {
        Ok(handle) => handle,
        Err(error) => return report_error(&error),
    };
    let header = match handle.header().await {
        Ok(header) => header,
        Err(error) => return report_error(&RsiError::Run(error.to_string())),
    };
    eprintln!("session: {}", header.session_id());
    let mut input = spawn_session_input();
    let rendering_stopped = CancellationToken::new();
    let (renderer, render_receiver) = tokio::sync::mpsc::channel(CLI_RENDER_CHANNEL_CAPACITY);
    let render_task = spawn_cli_renderer(command.output, render_receiver);
    let (completion, mut completed_turns) =
        tokio::sync::mpsc::channel(TURN_COMPLETION_CHANNEL_CAPACITY);
    let mut queue = VecDeque::new();
    let mut active: Option<(MessageId, CancellationToken)> = None;
    let mut detaching = false;
    let mut turn_failed = false;
    loop {
        if active.is_none() {
            if let Some(text) = queue.pop_front() {
                let message_id = match generated_cli_message_id() {
                    Ok(id) => id,
                    Err(error) => return report_error(&error),
                };
                let cancellation = CancellationToken::new();
                tokio::spawn(drive_application_turn(
                    Arc::clone(&handle),
                    SubmitInput {
                        message_id: message_id.clone(),
                        content: vec![MessageInput::Text { text }],
                        model: None,
                        sandbox: None,
                    },
                    cancellation.clone(),
                    rendering_stopped.clone(),
                    renderer.clone(),
                    completion.clone(),
                ));
                active = Some((message_id, cancellation));
            } else if detaching {
                break;
            }
        }

        tokio::select! {
            biased;
            signal = tokio::signal::ctrl_c(), if active.is_some() => {
                if signal.is_ok()
                    && let Some((_, cancellation)) = &active {
                    cancellation.cancel();
                }
            }
            message = completed_turns.recv(), if active.is_some() => {
                match message {
                    Some(MessageTaskFinished { message_id, result, cancellation_requested: _ }) => {
                        if active.as_ref().is_some_and(|(active, _)| active == &message_id) {
                            active = None;
                            match result {
                                Ok(outcome) => {
                                    turn_failed |= !matches!(
                                        &outcome,
                                        TurnOutcome::Completed | TurnOutcome::Cancelled
                                    );
                                    if !detaching {
                                        report_terminal_diagnostic(&outcome);
                                    }
                                }
                                Err(error) => {
                                    turn_failed = true;
                                    if !detaching {
                                        eprintln!("error: {error}");
                                    }
                                }
                            }
                        }
                    }
                    None => return report_error(&RsiError::Run("Session turn worker exited".into())),
                }
            }
            incoming = input.recv(), if !detaching && queue.len() < MAXIMUM_QUEUED_SESSION_TURNS => {
                match incoming.unwrap_or(SessionInput::Eof) {
                    SessionInput::Line(line) => {
                        if let Some(text) = line.strip_prefix("::") {
                            queue.push_back(format!(":{text}"));
                        } else if let Some(command_line) = line.strip_prefix(':') {
                            match handle_session_command(command_line, &handle, active.as_ref(), queue.len()).await {
                                SessionCommandAction::Continue => {}
                                SessionCommandAction::Exit => {
                                    queue.clear();
                                    if active.is_none() || mode == rsi::SessionHostConnectionMode::Remote {
                                        rendering_stopped.cancel();
                                        break;
                                    }
                                    rendering_stopped.cancel();
                                    detaching = true;
                                }
                            }
                        } else if !line.is_empty() {
                            queue.push_back(line);
                        }
                    }
                    SessionInput::TooLarge => eprintln!("error: input line exceeds {MAXIMUM_TURN_TEXT_BYTES} bytes"),
                    SessionInput::InvalidUtf8 => eprintln!("error: input line is not UTF-8"),
                    SessionInput::Error(error) => {
                        eprintln!("error: stdin read failed: {error}");
                        queue.clear();
                        rendering_stopped.cancel();
                        detaching = true;
                    }
                    SessionInput::Eof => {
                        queue.clear();
                        if active.is_none() || mode == rsi::SessionHostConnectionMode::Remote {
                            rendering_stopped.cancel();
                            break;
                        }
                        rendering_stopped.cancel();
                        detaching = true;
                    }
                }
            }
        }
    }
    drop(renderer);
    drop(completion);
    if active.is_none()
        && let Err(error) = join_cli_renderer(render_task).await
    {
        return report_error(&error);
    }
    u8::from(turn_failed)
}

pub(super) enum SessionCommandAction {
    Continue,
    Exit,
}

pub(super) async fn handle_session_command(
    command: &str,
    handle: &Arc<dyn SessionHandle>,
    active: Option<&(MessageId, CancellationToken)>,
    queued: usize,
) -> SessionCommandAction {
    let mut parts = command.split_whitespace();
    match parts.next().unwrap_or("") {
        "queue" => eprintln!(
            "active: {}\tqueued: {queued}",
            active.map_or("none", |(message, _)| message.as_str())
        ),
        "cancel" => {
            if let Some((_, cancellation)) = active {
                cancellation.cancel();
            } else {
                eprintln!("no active message");
            }
        }
        "approvals" => match handle.pending_approvals().await {
            Ok(requests) if requests.is_empty() => eprintln!("no pending approvals"),
            Ok(requests) => {
                for request in requests {
                    eprintln!("{}\t{}\t{}", request.id, request.action, request.reason);
                }
            }
            Err(error) => eprintln!("error: {error}"),
        },
        decision @ ("allow" | "deny") => {
            let Some(id) = parts.next() else {
                eprintln!("usage: :{decision} APPROVAL_ID");
                return SessionCommandAction::Continue;
            };
            if parts.next().is_some() {
                eprintln!("usage: :{decision} APPROVAL_ID");
                return SessionCommandAction::Continue;
            }
            let choice = if decision == "allow" {
                rsi_approval_protocol::ApprovalDecision::AllowOnce
            } else {
                rsi_approval_protocol::ApprovalDecision::Deny
            };
            match handle.answer_approval(id, choice).await {
                Ok(true) => eprintln!("answered {id}"),
                Ok(false) => eprintln!("approval is not pending: {id}"),
                Err(error) => eprintln!("error: {error}"),
            }
        }
        "exit" => return SessionCommandAction::Exit,
        "help" | "" => {
            eprintln!(":queue  :cancel  :approvals  :allow ID  :deny ID  :exit  :help  ::TEXT");
        }
        other => eprintln!("unknown Session command: :{other}"),
    }
    SessionCommandAction::Continue
}

#[cfg(target_os = "linux")]
pub(super) fn standard_coding_tools() -> rsi::Result<Option<StandardCodingTools>> {
    let helper = std::env::current_exe()
        .and_then(std::fs::canonicalize)
        .map_err(|error| {
            RsiError::Boot(format!("failed to resolve current executable: {error}"))
        })?;
    let bash = std::fs::canonicalize("/bin/bash")
        .map_err(|error| RsiError::Boot(format!("/bin/bash is unavailable: {error}")))?;
    let environment = scrub_child_environment(std::env::vars_os());
    StandardCodingTools::new(bash, helper, environment).map(Some)
}

#[cfg(not(target_os = "linux"))]
pub(super) fn standard_coding_tools() -> rsi::Result<Option<StandardCodingTools>> {
    Ok(None)
}

pub(super) async fn arm_signal(cancellation: CancellationToken) -> rsi::Result<JoinHandle<()>> {
    let (armed_tx, armed_rx) = tokio::sync::oneshot::channel();
    let task = tokio::spawn(async move {
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
                if signal.await.is_ok() {
                    cancellation.cancel();
                }
            }
        }
    });
    armed_rx
        .await
        .map_err(|_| RsiError::Boot("SIGINT listener exited before registration".into()))?
        .map_err(|error| RsiError::Boot(format!("failed to register SIGINT listener: {error}")))?;
    Ok(task)
}

pub(super) fn report_terminal_diagnostic(outcome: &TurnOutcome) {
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

pub(super) fn write_live_event(
    stdout: &mut impl Write,
    mode: OutputMode,
    event: &CliEvent,
    wrote_text: &mut bool,
    text_ends_newline: &mut bool,
) -> rsi::Result<()> {
    match mode {
        OutputMode::Jsonl => write_jsonl_event(stdout, event),
        OutputMode::Text => write_text_event(stdout, event, wrote_text, text_ends_newline),
    }
}

pub(super) fn write_jsonl_event(stdout: &mut impl Write, event: &CliEvent) -> rsi::Result<()> {
    let line = event
        .json_line()
        .map_err(|error| RsiError::Run(error.to_string()))?;
    stdout
        .write_all(line.as_bytes())
        .and_then(|()| stdout.write_all(b"\n"))
        .and_then(|()| stdout.flush())
        .map_err(|error| stdout_write_error(&error))
}

pub(super) fn write_text_event(
    stdout: &mut impl Write,
    event: &CliEvent,
    wrote_text: &mut bool,
    text_ends_newline: &mut bool,
) -> rsi::Result<()> {
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

pub(super) fn write_text_delta(
    stdout: &mut impl Write,
    text: &str,
    wrote_text: &mut bool,
    text_ends_newline: &mut bool,
) -> rsi::Result<()> {
    stdout
        .write_all(text.as_bytes())
        .and_then(|()| stdout.flush())
        .map_err(|error| stdout_write_error(&error))?;
    *wrote_text = true;
    *text_ends_newline = text.ends_with('\n');
    Ok(())
}

pub(super) fn write_media_reference(
    stdout: &mut impl Write,
    media_id: &str,
    wrote_text: &mut bool,
    text_ends_newline: &mut bool,
) -> rsi::Result<()> {
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

pub(super) fn stdout_write_error(error: &std::io::Error) -> RsiError {
    RsiError::Run(format!("stdout write failed: {error}"))
}

pub(super) fn report_error(error: &RsiError) -> u8 {
    eprintln!("error: {error}");
    error.exit_code()
}
