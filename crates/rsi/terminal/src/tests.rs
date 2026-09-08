use super::*;

#[test]
fn interrupted_line_reads_preserve_the_partial_input() {
    struct Source(usize);
    impl std::io::Read for Source {
        fn read(&mut self, output: &mut [u8]) -> std::io::Result<usize> {
            self.0 += 1;
            match self.0 {
                1 => {
                    output[..3].copy_from_slice(b"one");
                    Ok(3)
                }
                2 | 3 => Err(std::io::ErrorKind::Interrupted.into()),
                4 => {
                    output[..4].copy_from_slice(b"two\n");
                    Ok(4)
                }
                _ => Ok(0),
            }
        }
    }
    let mut reader = std::io::BufReader::new(Source(0));
    assert!(
        matches!(read_bounded_stdin_line(&mut reader), SessionInput::Line(text) if text == "onetwo")
    );
    assert!(matches!(
        read_bounded_stdin_line(&mut reader),
        SessionInput::Eof
    ));
}

#[test]
fn terminal_rendering_neutralizes_bidi_controls_without_removing_joiners() {
    assert_eq!(
        crate::terminal_text("ab\u{202e}cd\u{202c}\u{2066}x\u{2069}\u{200f}\u{061c}\r"),
        "ab�cd��x����"
    );
    assert_eq!(
        crate::terminal_text("中文\n\t👩\u{200d}💻"),
        "中文\n\t👩\u{200d}💻"
    );
}

#[test]
fn text_session_queries_write_their_results_to_stdout() {
    let mut output = Vec::new();
    let mut wrote = false;
    let mut newline = false;
    crate::write_text_event(&mut output, &crate::CliEvent::Notice {
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
    use crate::{CliRenderMessage, send_finish_line};
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
    pub(crate) commands:
        std::sync::Mutex<Vec<rsi_agent_session_protocol::SessionCommandInvocation>>,
    pub(crate) command_receipt:
        std::sync::Mutex<Option<rsi_agent_session_protocol::SessionCommandReceipt>>,
    pub(crate) submitted_requests: std::sync::Mutex<Vec<SubmitInput>>,
    pub(crate) inspection:
        std::sync::Mutex<Option<rsi_agent_store_protocol::StoreSessionInspection>>,
    pub(crate) inspection_capacity: std::sync::atomic::AtomicUsize,
    pub(crate) submissions: std::sync::atomic::AtomicUsize,
    pub(crate) cancellation_race: bool,
    pub(crate) observation_gate: Option<Arc<tokio::sync::Semaphore>>,
    pub(crate) query_finds_message: bool,
    pub(crate) status_error: std::sync::Mutex<Option<SessionError>>,
    pub(crate) queries: std::sync::atomic::AtomicUsize,
    pub(crate) cancellations: std::sync::Mutex<Vec<CancelTarget>>,
    pub(crate) fail_observation: bool,
    pub(crate) observations: std::sync::atomic::AtomicUsize,
    pub(crate) first_status_error: Option<SessionError>,
    pub(crate) interaction_polls: std::sync::atomic::AtomicUsize,
    pub(crate) interaction_failures: std::sync::atomic::AtomicUsize,
    pub(crate) interaction_capacity: std::sync::atomic::AtomicUsize,
    pub(crate) capacity_error: Option<SessionError>,
    pub(crate) observation_capacity: std::sync::atomic::AtomicUsize,
    pub(crate) pending_question:
        std::sync::Mutex<Option<rsi_user_questions_protocol::QuestionRequest>>,
    pub(crate) history_error: Option<SessionError>,
    pub(crate) history_gate: Option<Arc<tokio::sync::Semaphore>>,
    pub(crate) history_active: std::sync::atomic::AtomicUsize,
}

#[async_trait::async_trait]
impl SessionHandle for UnknownThenAcceptedHandle {
    async fn draft_snapshot(
        &self,
    ) -> rsi_session_protocol::Result<rsi_session_protocol::SessionDraftView> {
        panic!("unexpected draft snapshot")
    }
    async fn select_preset(
        &self,
        _: rsi_session_protocol::SelectDraftPreset,
    ) -> rsi_session_protocol::Result<rsi_session_protocol::SessionDraftView> {
        panic!("unexpected preset selection")
    }

    async fn commands(
        &self,
    ) -> rsi_session_protocol::Result<rsi_agent_session_protocol::SessionCommandsView> {
        use rsi_agent_session_protocol::*;
        Ok(SessionCommandsView::new(
            CommandRevision::Draft { revision: 0 },
            vec![
                SessionCommandDescriptor::new(
                    ContributionId::new("fixture.plan").unwrap(),
                    "plan",
                    "Plan on or off",
                    true,
                )
                .unwrap(),
            ],
        )
        .unwrap())
    }
    async fn execute_command(
        &self,
        invocation: rsi_agent_session_protocol::SessionCommandInvocation,
    ) -> rsi_session_protocol::Result<rsi_agent_session_protocol::SessionCommandReceipt> {
        self.commands.lock().unwrap().push(invocation.clone());
        Err(SessionError::CommandOutcomeUnknown {
            request_id: invocation.request_id,
        })
    }
    async fn command_status(
        &self,
        _: &rsi_agent_session_protocol::DomainRequestId,
    ) -> rsi_session_protocol::Result<Option<rsi_agent_session_protocol::SessionCommandReceipt>>
    {
        Ok(self.command_receipt.lock().unwrap().clone())
    }

    async fn read_message(
        &self,
        _message_id: &MessageId,
        _accepted_control_seq: u64,
    ) -> rsi_session_protocol::Result<rsi_agent_session_protocol::AgentMessage> {
        Err(SessionError::NotFound("fixture message body".into()))
    }

    async fn observe_projections(
        &self,
    ) -> rsi_session_protocol::Result<rsi_session_protocol::ProjectionStream> {
        Ok(Box::pin(futures_util::stream::pending()))
    }
    async fn observe_interactions(
        &self,
    ) -> rsi_session_protocol::Result<rsi_session_protocol::InteractionStream> {
        let snapshot = rsi_session_protocol::InteractionRetention::default().retain(
            self.pending_approvals().await?,
            self.pending_questions().await?,
        )?;
        Ok(Box::pin(futures_util::StreamExt::chain(
            futures_util::stream::once(async move { Ok(snapshot) }),
            futures_util::stream::pending(),
        )))
    }
    async fn inspect(
        &self,
    ) -> rsi_session_protocol::Result<rsi_agent_store_protocol::StoreSessionInspection> {
        if self
            .inspection_capacity
            .fetch_update(
                std::sync::atomic::Ordering::SeqCst,
                std::sync::atomic::Ordering::SeqCst,
                |remaining| remaining.checked_sub(1),
            )
            .is_ok()
        {
            return Err(SessionError::Api(rsi_api_protocol::ApiError::Capacity));
        }
        self.inspection
            .lock()
            .unwrap()
            .clone()
            .ok_or_else(|| SessionError::NotFound("fixture inspection".into()))
    }
    async fn pending_questions(
        &self,
    ) -> rsi_session_protocol::Result<Vec<rsi_user_questions_protocol::QuestionRequest>> {
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
    ) -> rsi_session_protocol::Result<bool> {
        Ok(false)
    }

    async fn header(
        &self,
    ) -> rsi_session_protocol::Result<rsi_agent_session_protocol::SessionHeader> {
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
    ) -> rsi_session_protocol::Result<rsi_agent_turn_protocol::MessageReceipt> {
        self.submitted_requests
            .lock()
            .unwrap()
            .push(request.clone());
        if !self.cancellation_race
            && self
                .submissions
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst)
                == 0
        {
            return Err(SessionError::MessageOutcomeUnknown {
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
    ) -> rsi_session_protocol::Result<rsi_agent_turn_protocol::MessageReceipt> {
        self.queries
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        if let Some(error) = &*self.status_error.lock().unwrap() {
            return Err(error.clone());
        }
        if message_id.as_str() == "a-first"
            && let Some(error) = &self.first_status_error
        {
            return Err(error.clone());
        }
        if !self.query_finds_message {
            return Err(SessionError::NotFound(message_id.to_string()));
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
        _request: rsi_session_protocol::SubmitDirectImage,
    ) -> rsi_session_protocol::Result<rsi_session_protocol::TurnReceipt> {
        unreachable!("not used")
    }

    async fn cancel(
        &self,
        target: CancelTarget,
        _reason: Option<String>,
    ) -> rsi_session_protocol::Result<rsi_agent_turn_protocol::CancelResult> {
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
    ) -> rsi_session_protocol::Result<rsi_session_protocol::SessionHistoryPage> {
        if let Some(gate) = &self.history_gate {
            struct Active<'a>(&'a std::sync::atomic::AtomicUsize);
            impl Drop for Active<'_> {
                fn drop(&mut self) {
                    self.0.fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
                }
            }
            self.history_active
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            let _active = Active(&self.history_active);
            gate.acquire().await.unwrap().forget();
        }
        if let Some(error) = &self.history_error {
            return Err(error.clone());
        }
        unreachable!("not used")
    }

    async fn observe(
        &self,
        cursor: ObservationCursor,
    ) -> rsi_session_protocol::Result<rsi_agent_turn_protocol::SessionObservationStream> {
        use rsi_agent_session_protocol::{ActivationId, AgentControlRecord, StepId};
        self.observations
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        if self
            .observation_capacity
            .fetch_update(
                std::sync::atomic::Ordering::SeqCst,
                std::sync::atomic::Ordering::SeqCst,
                |left| left.checked_sub(1),
            )
            .is_ok()
        {
            return Err(self
                .capacity_error
                .clone()
                .unwrap_or(SessionError::Capacity));
        }
        if self.fail_observation {
            return Err(SessionError::Backend("persistent failure".into()));
        }
        let turn_id = TurnId::new("turn-reconcile").unwrap();
        let update = if cursor.fact_seq == 0 {
            SessionObservation::Control {
                record: rsi_agent_turn_protocol::ObservationRetention::default()
                    .retain_controls(vec![Arc::new(
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
                    )])
                    .unwrap()
                    .pop()
                    .unwrap(),
                durable_control_seq: 2,
            }
        } else {
            SessionObservation::Fact {
                fact: rsi_agent_turn_protocol::ObservationRetention::default()
                    .retain_fact(Arc::new(
                        SessionFact::new(
                            2,
                            1,
                            SessionFactBody::TurnTerminal {
                                turn_id,
                                outcome: TurnOutcome::Completed,
                            },
                        )
                        .unwrap(),
                    ))
                    .unwrap(),
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
    ) -> rsi_session_protocol::Result<Vec<rsi_approval_protocol::ApprovalRequest>> {
        use std::sync::atomic::Ordering;
        self.interaction_polls.fetch_add(1, Ordering::SeqCst);
        if self
            .interaction_capacity
            .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |left| {
                left.checked_sub(1)
            })
            .is_ok()
        {
            return Err(self
                .capacity_error
                .clone()
                .unwrap_or(SessionError::Capacity));
        }
        if self
            .interaction_failures
            .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |left| {
                left.checked_sub(1)
            })
            .is_ok()
        {
            return Err(SessionError::Backend("injected interaction failure".into()));
        }
        if let Some(gate) = &self.observation_gate {
            let _permit = gate.acquire().await.unwrap();
        }
        Ok(Vec::new())
    }

    async fn answer_approval(
        &self,
        _owner: &SessionId,
        _approval_id: &str,
        _decision: rsi_approval_protocol::ApprovalDecision,
    ) -> rsi_session_protocol::Result<bool> {
        unreachable!("not used")
    }
}

#[tokio::test]
async fn unknown_message_outcome_retries_the_same_identity_once() {
    let concrete = Arc::new(UnknownThenAcceptedHandle::default());
    let handle: Arc<dyn SessionHandle> = concrete.clone();
    let message_id = MessageId::new("message-reconcile").unwrap();
    let receipt = submit_with_reconciliation(
        handle.as_ref(),
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

fn parse_headless(arguments: &[&str]) -> Result<Command> {
    Command::parse(arguments.iter().map(OsString::from))
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
        load_cli_images(paths, &ApplicationWork::default()).await,
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
        [first, second] if first.as_ref() == [1, 2, 3] && second.as_ref() == [4, 5]
    ));

    let empty = temporary.path().join("empty.image");
    std::fs::write(&empty, []).unwrap();
    assert!(read_cli_images(vec![empty]).is_err());

    let oversized = temporary.path().join("oversized.image");
    let file = std::fs::File::create(&oversized).unwrap();
    file.set_len(u64::try_from(crate::MAXIMUM_CLI_IMAGE_BYTES).unwrap() + 1)
        .unwrap();
    assert!(read_cli_images(vec![oversized]).is_err());
}

#[cfg(unix)]
#[test]
fn image_fifo_is_rejected_without_waiting_for_a_writer() {
    let temporary = tempfile::tempdir().unwrap();
    let fifo = temporary.path().join("image.fifo");
    assert!(
        std::process::Command::new("/usr/bin/mkfifo")
            .env_clear()
            .args(["-m", "600"])
            .arg(&fifo)
            .status()
            .unwrap()
            .success()
    );

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
        ApplicationWork::default(),
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
        handle.as_ref(),
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
        ApplicationWork::default(),
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
    use crate::{CliEvent, CliRenderMessage, send_cli_event};
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
            fact: rsi_agent_turn_protocol::ObservationRetention::default()
                .retain_fact(fact)
                .unwrap(),
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

#[async_trait::async_trait]
impl rsi_process::ProcessOutputCache for UnknownThenAcceptedHandle {
    async fn read(
        &self,
        _: &str,
        _: u64,
        _: usize,
    ) -> rsi_process::Result<rsi_process::OutputPage> {
        unreachable!("submission fixture does not read Process output")
    }
}

#[async_trait::async_trait]
impl rsi_ai_protocol::LanguageModels for UnknownThenAcceptedHandle {
    async fn list_models(
        &self,
        _: Option<&ModelRef>,
        _: usize,
    ) -> Result<rsi_ai_protocol::LanguageModelPage, rsi_ai_protocol::ModelsError> {
        unreachable!("submission fixture does not read the model catalog")
    }
}

#[derive(Debug)]
pub(crate) struct UnusedWorkspace;
#[async_trait::async_trait]
impl rsi_workspace_protocol::WorkspaceRegistry for UnusedWorkspace {
    async fn get(
        &self,
        _: &rsi_workspace_protocol::WorkspaceId,
    ) -> rsi_workspace_protocol::Result<rsi_workspace_protocol::WorkspaceRecord> {
        unreachable!("not used")
    }
    async fn list(
        &self,
        _: Option<rsi_workspace_protocol::WorkspaceCursor>,
        _: usize,
    ) -> rsi_workspace_protocol::Result<rsi_workspace_protocol::WorkspacePage> {
        unreachable!("not used")
    }
    async fn get_or_create(
        &self,
        _: &std::path::Path,
    ) -> rsi_workspace_protocol::Result<rsi_workspace_protocol::WorkspaceRecord> {
        unreachable!("not used")
    }
    async fn status(
        &self,
        _: &rsi_workspace_protocol::WorkspaceId,
    ) -> rsi_workspace_protocol::Result<rsi_workspace_protocol::WorkspaceStatus> {
        unreachable!("not used")
    }
    async fn delete_registration(
        &self,
        _: &rsi_workspace_protocol::WorkspaceId,
    ) -> rsi_workspace_protocol::Result<bool> {
        unreachable!("not used")
    }
}

#[cfg(unix)]
#[derive(Debug)]
struct HeadlessDomain(Arc<UnknownThenAcceptedHandle>);
#[cfg(unix)]
#[async_trait::async_trait]
impl SessionService for HeadlessDomain {
    async fn create(
        &self,
        _: CreateSession,
    ) -> rsi_session_protocol::Result<Arc<dyn SessionHandle>> {
        Ok(self.0.clone())
    }
    async fn attach(&self, _: &SessionId) -> rsi_session_protocol::Result<Arc<dyn SessionHandle>> {
        Ok(self.0.clone())
    }
    async fn list_recent(
        &self,
        _: Option<&rsi_session_protocol::RecentSessionCursor>,
        _: usize,
    ) -> rsi_session_protocol::Result<rsi_session_protocol::RecentSessionPage> {
        unreachable!()
    }
}
#[cfg(unix)]
#[async_trait::async_trait]
impl rsi_media_protocol::Media for HeadlessDomain {
    async fn import_image(
        &self,
        _: bytes::Bytes,
    ) -> rsi_media_protocol::Result<rsi_media_protocol::MediaRef> {
        unreachable!()
    }
    async fn read(
        &self,
        _: &rsi_media_protocol::MediaRef,
    ) -> rsi_media_protocol::Result<rsi_media_protocol::StoredMedia> {
        unreachable!()
    }
}

#[cfg(unix)]
#[tokio::test]
async fn headless_sigint_bounds_stalled_terminal_observation() {
    const CHILD: &str = "RSI_TEST_HEADLESS_STALLED_OBSERVATION";
    if std::env::var_os(CHILD).is_none() {
        let status = tokio::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "tests::headless_sigint_bounds_stalled_terminal_observation",
                "--nocapture",
            ])
            .env(CHILD, "1")
            .status()
            .await
            .unwrap();
        assert!(status.success());
        return;
    }
    let concrete = Arc::new(UnknownThenAcceptedHandle {
        query_finds_message: true,
        observation_gate: Some(Arc::new(tokio::sync::Semaphore::new(0))),
        ..Default::default()
    });
    let domain = Arc::new(HeadlessDomain(concrete.clone()));
    let work = ApplicationWork::default();
    let running = tokio::spawn(run_headless_application(
        domain.clone(),
        Arc::new(UnusedWorkspace),
        domain,
        parse_headless(&[
            "task",
            "--resume",
            "session-reconcile",
            "--message-id",
            "message-reconcile",
        ])
        .unwrap(),
        work.clone(),
    ));
    tokio::time::timeout(Duration::from_secs(2), async {
        while concrete
            .observations
            .load(std::sync::atomic::Ordering::SeqCst)
            < 2
        {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    // The signal listener is armed before the observed message driver starts.
    rustix::process::kill_process(rustix::process::getpid(), rustix::process::Signal::INT).unwrap();
    let exit = tokio::time::timeout(Duration::from_secs(2), running)
        .await
        .expect("SIGINT grace must bound local terminal observation")
        .unwrap();
    assert_eq!(exit, 130);
    assert!(
        concrete
            .cancellations
            .lock()
            .unwrap()
            .iter()
            .any(|t| matches!(t, CancelTarget::Turn(_)))
    );
    work.tasks.close();
    tokio::time::timeout(Duration::from_secs(1), work.tasks.wait())
        .await
        .unwrap();
}

#[cfg(unix)]
#[tokio::test]
async fn line_sigint_interrupts_initial_service_queries_and_drains_presentation() {
    #[derive(Debug)]
    struct Stalled(CancellationToken);
    #[async_trait::async_trait]
    impl SessionService for Stalled {
        async fn create(
            &self,
            _: CreateSession,
        ) -> rsi_session_protocol::Result<Arc<dyn SessionHandle>> {
            unreachable!()
        }
        async fn attach(
            &self,
            _: &SessionId,
        ) -> rsi_session_protocol::Result<Arc<dyn SessionHandle>> {
            self.0.cancel();
            std::future::pending().await
        }
        async fn list_recent(
            &self,
            _: Option<&rsi_session_protocol::RecentSessionCursor>,
            _: usize,
        ) -> rsi_session_protocol::Result<rsi_session_protocol::RecentSessionPage> {
            self.0.cancel();
            std::future::pending().await
        }
    }
    const CHILD: &str = "RSI_TEST_LINE_STALLED_QUERY";
    if std::env::var_os(CHILD).is_none() {
        let mut child = tokio::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "tests::line_sigint_interrupts_initial_service_queries_and_drains_presentation",
                "--nocapture",
            ])
            .env(CHILD, "1")
            .kill_on_drop(true)
            .spawn()
            .unwrap();
        assert!(
            tokio::time::timeout(Duration::from_secs(10), child.wait())
                .await
                .unwrap()
                .unwrap()
                .success()
        );
        return;
    }
    for arguments in [vec!["--list"], vec!["--history", "session-reconcile"]] {
        let handle = Arc::new(UnknownThenAcceptedHandle::default());
        let (renderer, _events) = tokio::sync::mpsc::channel(32);
        let (runtime, surfaces) = crate::surfaces::fixture(handle.clone(), &renderer).await;
        surfaces.close().await.unwrap();
        let work = ApplicationWork::default();
        let entered = CancellationToken::new();
        let running = tokio::spawn(crate::session_cli::run_session_application(
            Arc::new(Stalled(entered.clone())),
            handle,
            Arc::new(UnusedWorkspace),
            SessionCommand::parse(arguments.into_iter().map(OsString::from).collect()).unwrap(),
            work.clone(),
            runtime.root(),
        ));
        tokio::time::timeout(Duration::from_secs(2), entered.cancelled())
            .await
            .unwrap();
        rustix::process::kill_process(rustix::process::getpid(), rustix::process::Signal::INT)
            .unwrap();
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(3), running)
                .await
                .unwrap()
                .unwrap(),
            130
        );
        work.tasks.close();
        tokio::time::timeout(Duration::from_secs(1), work.tasks.wait())
            .await
            .unwrap();
        assert!(runtime.shutdown().await.is_clean());
    }
}
