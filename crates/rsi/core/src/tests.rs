use super::application::{
    SESSION_INPUT_CHANNEL_CAPACITY, SessionCommand, SessionInput, drive_application_turn,
    load_cli_images, read_bounded_stdin_line, read_cli_images, spawn_session_input_reader,
    submit_with_reconciliation,
};
#[cfg(target_os = "linux")]
use super::host_cli::{
    DaemonControlEvent, format_session_host_diagnostics, host_stop_timeout,
    next_daemon_control_event, stop_reload_task,
};
use super::*;

#[test]
fn terminal_rendering_neutralizes_bidi_controls_without_removing_joiners() {
    assert_eq!(
        application::terminal_text("ab\u{202e}cd\u{202c}\u{2066}x\u{2069}\u{200f}\u{061c}\r"),
        "ab�cd��x����"
    );
    assert_eq!(
        application::terminal_text("中文\n\t👩\u{200d}💻"),
        "中文\n\t👩\u{200d}💻"
    );
}

#[test]
fn text_session_queries_write_their_results_to_stdout() {
    let mut output = Vec::new();
    let mut wrote = false;
    let mut newline = false;
    application::write_text_event(&mut output, &application::CliEvent::Notice {
        kind: "sessions", value: serde_json::json!({"sessions": [{"session_id": "readable-session"}], "has_more": false}),
    }, &mut wrote, &mut newline).unwrap();
    assert!(
        String::from_utf8(output)
            .unwrap()
            .contains("readable-session"),
        "text query results were omitted from stdout"
    );
    assert!(wrote && newline);
}

#[tokio::test]
async fn cancelled_terminal_keeps_its_finish_line_under_renderer_backpressure() {
    use super::application::{CliRenderMessage, send_finish_line};
    let (sender, mut receiver) = tokio::sync::mpsc::channel(1);
    sender.send(CliRenderMessage::FinishLine).await.unwrap();
    let stopped = CancellationToken::new();
    let mut finishing = Box::pin(send_finish_line(&sender, &stopped));
    assert!(
        futures_util::poll!(&mut finishing).is_pending(),
        "cancelled terminal discarded its trailing newline under pressure"
    );
    receiver.recv().await.unwrap();
    finishing.await.unwrap();
    assert!(matches!(
        receiver.recv().await,
        Some(CliRenderMessage::FinishLine)
    ));
}

#[derive(Debug, Default)]
pub(crate) struct UnknownThenAcceptedHandle {
    pub(crate) submissions: std::sync::atomic::AtomicUsize,
    pub(crate) cancellation_race: bool,
    pub(crate) observation_gate: Option<Arc<tokio::sync::Semaphore>>,
    pub(crate) query_finds_message: bool,
    pub(crate) queries: std::sync::atomic::AtomicUsize,
    pub(crate) cancellations: std::sync::Mutex<Vec<CancelTarget>>,
    pub(crate) fail_observation: bool,
    pub(crate) observations: std::sync::atomic::AtomicUsize,
    pub(crate) first_status_error: Option<SessionApplicationError>,
    pub(crate) interaction_polls: std::sync::atomic::AtomicUsize,
    pub(crate) interaction_failures: std::sync::atomic::AtomicUsize,
    pub(crate) pending_question:
        std::sync::Mutex<Option<rsi_user_questions_protocol::QuestionRequest>>,
    pub(crate) history_error: Option<SessionApplicationError>,
}

#[async_trait::async_trait]
impl SessionHandle for UnknownThenAcceptedHandle {
    async fn inspect(
        &self,
    ) -> rsi_session::Result<rsi_agent_store_protocol::StoreSessionInspection> {
        unimplemented!("fixture does not inspect")
    }
    async fn pending_questions(
        &self,
    ) -> rsi_session::Result<Vec<rsi_user_questions_protocol::QuestionRequest>> {
        Ok(self
            .pending_question
            .lock()
            .unwrap()
            .iter()
            .cloned()
            .collect())
    }
    async fn answer_question(
        &self,
        _: &str,
        _: rsi_user_questions_protocol::QuestionAnswer,
    ) -> rsi_session::Result<bool> {
        Ok(false)
    }
    async fn read_output(
        &self,
        _: &str,
        _: u64,
        _: usize,
    ) -> rsi_session::Result<rsi_process::OutputPage> {
        unimplemented!("fixture does not read output")
    }
    async fn header(&self) -> rsi_session::Result<rsi_agent_session_protocol::SessionHeader> {
        use rsi_agent_session_protocol::{AgentPresetId, FrozenAgentSettings, SessionHeader};
        Ok(SessionHeader::new(
            SessionId::new("session-reconcile").unwrap(),
            1,
            std::env::temp_dir().to_str().unwrap(),
            AgentPresetId::new("test").unwrap(),
            FrozenAgentSettings::new(
                "test",
                "system",
                rsi_ai_protocol::ModelRef::new("test", "model").unwrap(),
                rsi_sandbox::SandboxMode::WorkspaceWrite,
                false,
            )
            .unwrap(),
        )
        .unwrap())
    }

    async fn submit(
        &self,
        request: SubmitInput,
    ) -> rsi_session::Result<rsi_agent_turn_protocol::MessageReceipt> {
        if !self.cancellation_race
            && self
                .submissions
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst)
                == 0
        {
            return Err(SessionApplicationError::MessageOutcomeUnknown {
                session: "session-reconcile".into(),
                message: request.message_id.to_string(),
            });
        }
        Ok(rsi_agent_turn_protocol::MessageReceipt {
            session_id: SessionId::new("session-reconcile").unwrap(),
            message_id: request.message_id,
            accepted_control_seq: 1,
            observed_fact_seq: 0,
            state: MessageState::Pending,
        })
    }

    async fn message_status(
        &self,
        message_id: &MessageId,
    ) -> rsi_session::Result<rsi_agent_turn_protocol::MessageReceipt> {
        self.queries
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        if message_id.as_str() == "a-first"
            && let Some(error) = &self.first_status_error
        {
            return Err(error.clone());
        }
        if !self.query_finds_message {
            return Err(SessionApplicationError::NotFound(message_id.to_string()));
        }
        Ok(rsi_agent_turn_protocol::MessageReceipt {
            session_id: SessionId::new("session-reconcile").unwrap(),
            message_id: message_id.clone(),
            accepted_control_seq: 1,
            observed_fact_seq: 0,
            state: MessageState::Pending,
        })
    }

    async fn generate_image(
        &self,
        _request: rsi_session::SubmitDirectImage,
    ) -> rsi_session::Result<rsi_session::TurnReceipt> {
        unreachable!("not used")
    }

    async fn cancel(
        &self,
        target: CancelTarget,
        _reason: Option<String>,
    ) -> rsi_session::Result<rsi_agent_turn_protocol::CancelResult> {
        let accepted = matches!(target, CancelTarget::Turn(_));
        self.cancellations.lock().unwrap().push(target);
        Ok(rsi_agent_turn_protocol::CancelResult {
            accepted,
            already_terminal: false,
        })
    }

    async fn history_before(
        &self,
        _exclusive_before_seq: Option<u64>,
        _limit: usize,
    ) -> rsi_session::Result<rsi_session::SessionHistoryPage> {
        if let Some(error) = &self.history_error {
            return Err(error.clone());
        }
        unreachable!("not used")
    }

    async fn observe(
        &self,
        cursor: ObservationCursor,
    ) -> rsi_session::Result<rsi_agent_turn_protocol::SessionObservationStream> {
        use rsi_agent_session_protocol::{ActivationId, AgentControlRecord, StepId};
        self.observations
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        if self.fail_observation {
            return Err(SessionApplicationError::Backend(
                "persistent failure".into(),
            ));
        }
        let turn_id = TurnId::new("turn-reconcile").unwrap();
        let update = if cursor.fact_seq == 0 {
            SessionObservation::Control {
                record: Arc::new(
                    AgentControlRecord::new(
                        2,
                        1,
                        AgentControlRecordBody::MessageClaimed {
                            message_id: MessageId::new("message-reconcile").unwrap(),
                            activation_id: ActivationId::new("activation-reconcile").unwrap(),
                            turn_id,
                            step_id: StepId::new("step-reconcile").unwrap(),
                            entered_fact_seq: 1,
                        },
                    )
                    .unwrap(),
                ),
                durable_control_seq: 2,
            }
        } else {
            SessionObservation::Fact {
                fact: Arc::new(
                    SessionFact::new(
                        2,
                        1,
                        SessionFactBody::TurnTerminal {
                            turn_id,
                            outcome: TurnOutcome::Completed,
                        },
                    )
                    .unwrap(),
                ),
                durable_fact_seq: 2,
            }
        };
        if cursor.fact_seq > 0
            && let Some(gate) = &self.observation_gate
        {
            let gate = gate.clone();
            return Ok(Box::pin(futures_util::stream::once(async move {
                let permit = gate.acquire().await.unwrap();
                tokio::time::sleep(Duration::from_millis(350)).await;
                drop(permit);
                Ok(update)
            })));
        }
        Ok(Box::pin(futures_util::stream::iter([Ok(update)])))
    }

    async fn pending_approvals(
        &self,
    ) -> rsi_session::Result<Vec<rsi_approval_protocol::ApprovalRequest>> {
        use std::sync::atomic::Ordering;
        self.interaction_polls.fetch_add(1, Ordering::SeqCst);
        if self
            .interaction_failures
            .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |left| {
                left.checked_sub(1)
            })
            .is_ok()
        {
            return Err(SessionApplicationError::Backend(
                "injected interaction failure".into(),
            ));
        }
        if let Some(gate) = &self.observation_gate {
            let _permit = gate.acquire().await.unwrap();
        }
        Ok(Vec::new())
    }

    async fn answer_approval(
        &self,
        _approval_id: &str,
        _decision: rsi_approval_protocol::ApprovalDecision,
    ) -> rsi_session::Result<bool> {
        unreachable!("not used")
    }
}

#[tokio::test]
async fn unknown_message_outcome_retries_the_same_identity_once() {
    let concrete = Arc::new(UnknownThenAcceptedHandle::default());
    let handle: Arc<dyn SessionHandle> = concrete.clone();
    let message_id = MessageId::new("message-reconcile").unwrap();
    let receipt = submit_with_reconciliation(
        &handle,
        SubmitInput {
            delivery: rsi_agent_session_protocol::MessageDelivery::NextTurn,
            message_id: message_id.clone(),
            content: vec![MessageInput::Text {
                text: "reconcile".into(),
            }],
            model: None,
            sandbox: None,
        },
    )
    .await
    .unwrap();

    assert_eq!(receipt.message_id, message_id);
    assert_eq!(
        concrete
            .submissions
            .load(std::sync::atomic::Ordering::SeqCst),
        2
    );
}

#[cfg(target_os = "linux")]
#[test]
fn session_host_diagnostics_format_has_stable_explicit_fields() {
    let formatted = format_session_host_diagnostics(
        SessionHostDiagnosticsSnapshot {
            accepted_connections: 2,
            request_failures: 1,
            ..SessionHostDiagnosticsSnapshot::default()
        },
        true,
    );

    assert_eq!(
        formatted,
        "Session Host diagnostics final=true accepted_connections=2 accept_errors=0 peer_credential_errors=0 foreign_uid_rejections=0 capacity_rejections=0 handshake_rejections=0 handshake_failures=0 request_failures=1 response_failures=0 connection_task_panics=0 drain_aborted_connections=0"
    );
}

fn parse(arguments: &[&str]) -> rsi::Result<Parse> {
    Command::parse_cli(arguments.iter().map(OsString::from))
}

fn parse_headless(arguments: &[&str]) -> rsi::Result<Command> {
    let mut values = vec![OsString::from("run")];
    values.extend(arguments.iter().map(OsString::from));
    match Command::parse(values)? {
        Parse::Run(command) => Ok(command),
        _ => Err(usage("headless parser returned a non-headless command")),
    }
}

#[test]
fn enforces_input_model_and_session_exclusivity() {
    assert!(parse_headless(&[]).is_err());
    assert!(parse_headless(&["task", "--stdin"]).is_err());
    assert!(parse_headless(&["task", "--deployment", "one"]).is_err());
    assert!(
        parse_headless(&[
            "task",
            "--resume",
            "session-one",
            "--session-id",
            "session-two"
        ])
        .is_err()
    );
    assert!(
        parse_headless(&["task", "--deployment", "contains space", "--model", "model"]).is_err()
    );
    assert!(parse_headless(&["task", "--output", "text", "--output", "jsonl"]).is_err());
    assert!(parse(&["run", "task"]).is_err());
}

#[test]
fn parses_one_valid_agent_preset_only_for_a_fresh_session() {
    let command = parse_headless(&["task", "--agent-preset", "coding-agent"]).unwrap();
    assert_eq!(
        command.agent_preset.as_ref().map(AgentPresetId::as_str),
        Some("coding-agent")
    );
    let options = command.options("task".into()).unwrap();
    assert!(matches!(
        options.session,
        SessionSelection::Fresh {
            agent_preset_id: Some(ref id),
            ..
        } if id.as_str() == "coding-agent"
    ));
    assert!(
        parse_headless(&[
            "task",
            "--agent-preset",
            "coding-agent",
            "--agent-preset",
            "review-agent"
        ])
        .is_err()
    );
    assert!(parse_headless(&["task", "--agent-preset", "Upper"]).is_err());
    assert!(
        parse_headless(&[
            "task",
            "--resume",
            "session-one",
            "--agent-preset",
            "coding-agent"
        ])
        .is_err()
    );
}

#[test]
fn parses_repeat_images_message_identity_and_explicit_fresh_workspace_trust() {
    let command = parse_headless(&[
        "task",
        "--message-id",
        "message-explicit",
        "-i",
        "first.png",
        "--image",
        "second.png",
        "--trust-workspace",
    ])
    .unwrap();
    assert_eq!(
        command.message_id.as_ref().map(MessageId::as_str),
        Some("message-explicit")
    );
    assert_eq!(
        command.images,
        [PathBuf::from("first.png"), PathBuf::from("second.png")]
    );
    assert!(matches!(
        command.options("task".into()).unwrap().session,
        SessionSelection::Fresh {
            workspace_trust: WorkspaceTrust::Trusted,
            ..
        }
    ));
    assert!(parse_headless(&["task", "--resume", "session-one", "--trust-workspace"]).is_err());
    assert!(parse_headless(&["task", "--trust-workspace", "--trust-workspace"]).is_err());
    assert!(
        parse_headless(&[
            "task",
            "--message-id",
            "message-one",
            "--message-id",
            "message-two"
        ])
        .is_err()
    );
}

#[test]
fn session_resume_cannot_override_immutable_workspace_trust() {
    let resume = SessionId::new("session-resume-trust").unwrap();
    let parsed = SessionCommand::parse(vec![
        OsString::from("--resume"),
        OsString::from(resume.as_str()),
        OsString::from("--trust-workspace"),
    ]);
    assert!(matches!(
        parsed,
        Err(RsiError::Boot(message))
            if message.contains("immutable authority")
    ));
}

#[tokio::test]
async fn image_count_is_rejected_before_any_path_is_opened() {
    let paths =
        vec![PathBuf::from("intentionally-missing.image"); MAXIMUM_AGENT_MESSAGE_CONTENT_BLOCKS];
    assert!(matches!(
        load_cli_images(paths).await,
        Err(RsiError::Boot(message))
            if message.contains("at most 63 images")
    ));
}

#[test]
fn image_files_are_read_in_order_and_rejected_by_metadata_before_oversized_allocation() {
    let temporary = tempfile::tempdir().unwrap();
    let first = temporary.path().join("first.image");
    let second = temporary.path().join("second.image");
    std::fs::write(&first, [1_u8, 2, 3]).unwrap();
    std::fs::write(&second, [4_u8, 5]).unwrap();
    let images = read_cli_images(vec![first, second]).unwrap();
    assert!(matches!(
        images.as_slice(),
        [
            MessageInput::Image { bytes: first },
            MessageInput::Image { bytes: second },
        ] if first.as_ref() == [1, 2, 3] && second.as_ref() == [4, 5]
    ));

    let empty = temporary.path().join("empty.image");
    std::fs::write(&empty, []).unwrap();
    assert!(read_cli_images(vec![empty]).is_err());

    let oversized = temporary.path().join("oversized.image");
    let file = std::fs::File::create(&oversized).unwrap();
    file.set_len(u64::try_from(MAXIMUM_SESSION_INPUT_IMAGE_BYTES).unwrap() + 1)
        .unwrap();
    assert!(read_cli_images(vec![oversized]).is_err());
}

#[cfg(unix)]
#[test]
fn image_fifo_is_rejected_without_waiting_for_a_writer() {
    let temporary = tempfile::tempdir().unwrap();
    let fifo = temporary.path().join("image.fifo");
    rustix::fs::mkfifoat(
        rustix::fs::CWD,
        &fifo,
        rustix::fs::Mode::RUSR | rustix::fs::Mode::WUSR,
    )
    .unwrap();

    let started = std::time::Instant::now();
    assert!(matches!(
        read_cli_images(vec![fifo]),
        Err(RsiError::Boot(message)) if message.contains("not a regular file")
    ));
    assert!(started.elapsed() < Duration::from_secs(1));
}

#[test]
fn leading_slash_is_plain_task_and_dash_task_uses_separator() {
    let command = parse_headless(&["/status"]).unwrap();
    assert_eq!(command.positional.as_deref(), Some("/status"));
    let command = parse_headless(&["--", "--literal"]).unwrap();
    assert_eq!(command.positional.as_deref(), Some("--literal"));
}

#[test]
fn agent_store_verify_has_a_strict_absolute_root_contract() {
    let Parse::AgentStore(command) = parse(&[
        "agent-store",
        "verify",
        "--root",
        "/tmp/rsi-agent-store",
        "--output",
        "json",
    ])
    .unwrap() else {
        panic!("agent-store")
    };
    assert_eq!(command.root, Some(PathBuf::from("/tmp/rsi-agent-store")));
    assert_eq!(command.output, ManagementOutput::Json);
    assert!(parse(&["agent-store", "verify", "--root", "relative"]).is_err());
    assert!(parse(&["agent-store", "verify", "--root", "/a", "--root", "/b"]).is_err());
    assert!(parse(&["agent-store", "unknown"]).is_err());
}

#[test]
fn parses_named_applications_and_strict_profile_management() {
    let Parse::Application(application) = parse(&[
        "--profile",
        "headless",
        "task",
        "--session-id",
        "session-one",
    ])
    .unwrap() else {
        panic!("application")
    };
    assert_eq!(application.profile.as_str(), "headless");
    assert_eq!(application.arguments.len(), 3);

    let Parse::Profile(profile) = parse(&[
        "profile", "host", "copy", "standard", "custom", "--output", "json",
    ])
    .unwrap() else {
        panic!("profile")
    };
    assert_eq!(profile.kind, ProfileKind::Host);
    assert_eq!(profile.operation, ProfileOperationKind::Copy);
    assert_eq!(profile.ids, ["standard", "custom"]);
    assert_eq!(profile.output, ManagementOutput::Json);
    assert!(parse(&["profile", "application", "preview", "session"]).is_err());
    assert!(parse(&["profile", "host", "delete"]).is_err());
}

#[test]
fn parses_explicit_host_lifecycle_without_ambiguous_targets() {
    let Parse::Host(command) =
        parse(&["host", "restart", "--profile", "custom", "--force"]).unwrap()
    else {
        panic!("host")
    };
    assert_eq!(command.operation, HostOperation::Restart);
    assert_eq!(command.profile.as_str(), "custom");
    assert!(command.force);
    assert!(parse(&["host", "status", "--profile", "custom"]).is_err());
    assert!(parse(&["host", "reload", "--force"]).is_err());

    let Parse::Host(detached) = parse(&["host", "serve", "--detached-child"]).unwrap() else {
        panic!("detached serve")
    };
    assert!(detached.detached_child);
    assert!(parse(&["host", "start", "--detached-child"]).is_err());
}

#[cfg(target_os = "linux")]
#[test]
fn graceful_host_stop_wait_includes_drain_and_shutdown_margin() {
    assert_eq!(
        host_stop_timeout(false),
        SESSION_HOST_DRAIN_TIMEOUT + HOST_SHUTDOWN_MARGIN
    );
    assert!(host_stop_timeout(false) > SESSION_HOST_DRAIN_TIMEOUT);
    assert_eq!(host_stop_timeout(true), FORCE_HOST_STOP_TIMEOUT);
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn daemon_stop_selection_remains_live_during_reload() {
    let mut daemon_task = tokio::spawn(std::future::pending::<rsi::Result<()>>());
    let mut reload_task = tokio::spawn(std::future::pending::<()>());

    let event = next_daemon_control_event(
        &mut daemon_task,
        std::future::ready(Some(())),
        std::future::pending::<Option<()>>(),
        std::future::pending::<Option<()>>(),
        false,
        Some(&mut reload_task),
    )
    .await;

    assert!(matches!(event, DaemonControlEvent::Stop));
    assert!(!reload_task.is_finished());
    let mut reload_task = Some(reload_task);
    stop_reload_task(&mut reload_task).await;
    assert!(reload_task.is_none());
    // Once TERM starts shutdown, a ready SIGHUP must not start another reload.
    daemon_task.abort();
    let event = next_daemon_control_event(
        &mut daemon_task,
        std::future::pending::<Option<()>>(),
        std::future::pending::<Option<()>>(),
        std::future::ready(Some(())),
        false,
        None,
    )
    .await;
    assert!(matches!(event, DaemonControlEvent::Daemon(_)));
}

#[test]
fn session_input_is_bounded_before_a_complete_line_is_allocated() {
    for capacity in [7, 8 * 1024] {
        let mut reader = std::io::BufReader::with_capacity(
            capacity,
            std::io::Cursor::new(format!("{}\nok\n", "x".repeat(MAXIMUM_TURN_TEXT_BYTES + 1))),
        );
        assert!(matches!(
            read_bounded_stdin_line(&mut reader),
            SessionInput::TooLarge
        ));
        assert!(matches!(
            read_bounded_stdin_line(&mut reader),
            SessionInput::Line(line) if line == "ok"
        ));
    }
}

#[tokio::test]
async fn session_input_reader_backpressures_after_one_complete_line() {
    let mut input = spawn_session_input_reader(std::io::Cursor::new("one\ntwo\nthree\n"));
    tokio::time::timeout(std::time::Duration::from_secs(1), async {
        while input.len() != SESSION_INPUT_CHANNEL_CAPACITY {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("reader did not fill its bounded handoff");
    assert_eq!(input.len(), 1);
    assert!(matches!(input.recv().await, Some(SessionInput::Line(line)) if line == "one"));
    tokio::time::timeout(std::time::Duration::from_secs(1), async {
        while input.len() != SESSION_INPUT_CHANNEL_CAPACITY {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("reader did not resume after consumer progress");
    assert_eq!(input.len(), 1);
}

#[tokio::test]
async fn interrupt_that_loses_message_claim_race_still_cancels_the_claimed_turn() {
    let concrete = Arc::new(UnknownThenAcceptedHandle {
        cancellation_race: true,
        ..Default::default()
    });
    let cancellation = CancellationToken::new();
    cancellation.cancel();
    let (render, _rendered) = tokio::sync::mpsc::channel(32);
    let (completion, mut completed) = tokio::sync::mpsc::channel(1);
    drive_application_turn(
        concrete.clone(),
        SubmitInput {
            delivery: rsi_agent_session_protocol::MessageDelivery::NextTurn,
            message_id: MessageId::new("message-reconcile").unwrap(),
            content: vec![MessageInput::Text {
                text: "cancel".into(),
            }],
            model: None,
            sandbox: None,
        },
        cancellation,
        CancellationToken::new(),
        render,
        completion,
    )
    .await;
    assert!(completed.recv().await.is_some());
    assert_eq!(
        *concrete.cancellations.lock().unwrap(),
        [
            CancelTarget::Message(MessageId::new("message-reconcile").unwrap()),
            CancelTarget::Turn(TurnId::new("turn-reconcile").unwrap()),
        ]
    );
}

#[tokio::test]
async fn unknown_message_outcome_queries_before_resending_input() {
    let concrete = Arc::new(UnknownThenAcceptedHandle {
        query_finds_message: true,
        ..Default::default()
    });
    let handle: Arc<dyn SessionHandle> = concrete.clone();
    let receipt = submit_with_reconciliation(
        &handle,
        SubmitInput {
            delivery: rsi_agent_session_protocol::MessageDelivery::NextTurn,
            message_id: MessageId::new("message-reconcile").unwrap(),
            content: vec![MessageInput::Text {
                text: "query accepted input".into(),
            }],
            model: None,
            sandbox: None,
        },
    )
    .await
    .unwrap();
    assert_eq!(receipt.message_id.as_str(), "message-reconcile");
    assert_eq!(
        concrete
            .submissions
            .load(std::sync::atomic::Ordering::SeqCst),
        1
    );
    assert_eq!(
        concrete.queries.load(std::sync::atomic::Ordering::SeqCst),
        1
    );
}

#[tokio::test]
async fn interaction_refresh_cannot_deadlock_a_suspended_observation_read() {
    let concrete = Arc::new(UnknownThenAcceptedHandle {
        query_finds_message: false,
        observation_gate: Some(Arc::new(tokio::sync::Semaphore::new(1))),
        ..UnknownThenAcceptedHandle::default()
    });
    let (render, mut rendered) = tokio::sync::mpsc::channel(32);
    let rendering = tokio::spawn(async move { while rendered.recv().await.is_some() {} });
    let (completion, mut completed) = tokio::sync::mpsc::channel(1);
    let task = tokio::spawn(drive_application_turn(
        concrete,
        SubmitInput {
            delivery: rsi_agent_session_protocol::MessageDelivery::NextTurn,
            message_id: MessageId::new("message-reconcile").unwrap(),
            content: vec![MessageInput::Text {
                text: "observe".into(),
            }],
            model: None,
            sandbox: None,
        },
        CancellationToken::new(),
        CancellationToken::new(),
        render,
        completion,
    ));
    let result = tokio::time::timeout(Duration::from_secs(2), completed.recv())
        .await
        .expect("stream must keep polling while interaction refresh awaits its read permit")
        .unwrap();
    assert_eq!(result.result.unwrap(), TurnOutcome::Completed);
    task.await.unwrap();
    rendering.await.unwrap();
}

#[tokio::test]
async fn cancelled_turn_still_delivers_terminal_envelopes_through_backpressure() {
    use super::application::{CliEvent, CliRenderMessage, send_cli_event};
    let session_id = SessionId::new("terminal-render").unwrap();
    let turn_id = TurnId::new("terminal-turn").unwrap();
    let fact = Arc::new(
        rsi_agent_session_protocol::SessionFact::new(
            1,
            1,
            rsi_agent_session_protocol::SessionFactBody::TurnTerminal {
                turn_id: turn_id.clone(),
                outcome: TurnOutcome::Cancelled,
            },
        )
        .unwrap(),
    );
    for event in [
        CliEvent::Fact {
            session_id: session_id.clone(),
            fact,
            durable_seq: 1,
        },
        CliEvent::Outcome {
            session_id,
            turn_id,
            outcome: TurnOutcome::Cancelled,
            durable_seq: 1,
        },
    ] {
        let (sender, mut receiver) = tokio::sync::mpsc::channel(1);
        sender.send(CliRenderMessage::FinishLine).await.unwrap();
        let cancelled = CancellationToken::new();
        cancelled.cancel();
        let task = tokio::spawn(async move {
            send_cli_event(&sender, &CancellationToken::new(), &cancelled, event).await
        });
        tokio::task::yield_now().await;
        assert!(
            !task.is_finished(),
            "terminal event was dropped on cancellation"
        );
        receiver.recv().await.unwrap();
        assert!(matches!(
            receiver.recv().await,
            Some(CliRenderMessage::Event(_))
        ));
        task.await.unwrap().unwrap();
    }
}
