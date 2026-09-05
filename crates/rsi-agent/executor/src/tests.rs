use super::*;
use rsi_agent_session_protocol::{
    AgentPath, AgentPresetId, ForkOrigin, ForkTurnSelection, FrozenAgentSettings, SessionHeader,
    SessionId, TurnId,
};
use rsi_agent_turn_protocol::{ContextCheckpoint, ExecutorLease, ForkFactPage};
use rsi_media_protocol::MediaId;
use rsi_sandbox::SandboxMode;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

#[tokio::test]
async fn parked_tool_lane_releases_admission_and_reacquires_before_resume() {
    let admission = Arc::new(Semaphore::new(1));
    let permit = Arc::clone(&admission).acquire_owned().await.unwrap();
    let parking = Arc::new(ExecutorLaneParking {
        admission: Arc::clone(&admission),
        permit: Mutex::new(Some(permit)),
        stop: CancellationToken::new(),
        closed: CancellationToken::new(),
    });

    parking.park().await.unwrap();
    let replacement = Arc::clone(&admission).acquire_owned().await.unwrap();
    let resuming = tokio::spawn({
        let parking = Arc::clone(&parking);
        async move { parking.resume(CancellationToken::new()).await }
    });
    tokio::task::yield_now().await;
    assert!(!resuming.is_finished());
    drop(replacement);
    resuming.await.unwrap().unwrap();
    assert_eq!(admission.available_permits(), 0);
    parking.park().await.unwrap();
    assert_eq!(admission.available_permits(), 1);
}

#[tokio::test]
async fn parked_tool_lane_cancellation_bounds_reacquisition() {
    let admission = Arc::new(Semaphore::new(1));
    let permit = Arc::clone(&admission).acquire_owned().await.unwrap();
    let parking = Arc::new(ExecutorLaneParking {
        admission: Arc::clone(&admission),
        permit: Mutex::new(Some(permit)),
        stop: CancellationToken::new(),
        closed: CancellationToken::new(),
    });
    parking.park().await.unwrap();
    let replacement = Arc::clone(&admission).acquire_owned().await.unwrap();
    let cancellation = CancellationToken::new();
    cancellation.cancel();

    assert_eq!(
        parking.resume(cancellation).await.unwrap_err(),
        ToolError::Cancelled
    );
    drop(replacement);
}

#[tokio::test]
async fn settled_lane_reclaims_admission_from_a_retained_tool_extension() {
    let admission = Arc::new(Semaphore::new(1));
    let permit = Arc::clone(&admission).acquire_owned().await.unwrap();
    let parking = Arc::new(ExecutorLaneParking {
        admission: Arc::clone(&admission),
        permit: Mutex::new(Some(permit)),
        stop: CancellationToken::new(),
        closed: CancellationToken::new(),
    });
    let retained_extension = ToolLaneParkingAuthority::new(parking.clone());

    parking.close();

    assert_eq!(admission.available_permits(), 1);
    assert_eq!(
        retained_extension.park().await.unwrap_err(),
        ToolError::ShuttingDown
    );
}

#[test]
fn prepared_executor_charge_includes_inline_and_dynamic_config_state() {
    let config = ExecutorConfig {
        executor_id: "executor".into(),
        maximum_active_turns: 1,
        max_context_messages: default_context_messages(),
        max_context_bytes: default_context_bytes(),
        durability_wait_ms: default_durability_wait_ms(),
        finalization_wait_ms: default_finalization_wait_ms(),
        retained_tool_wait_ms: default_retained_tool_wait_ms(),
    };

    assert_eq!(
        executor_config_retained_bytes(&config).unwrap(),
        std::mem::size_of::<ExecutorConfig>() + config.executor_id.len()
    );
}

#[test]
fn executor_concurrency_is_validated_at_its_config_owner() {
    for maximum_active_turns in [1, MAXIMUM_EXECUTOR_ACTIVE_TURNS] {
        let config: ExecutorConfig = serde_json::from_value(serde_json::json!({
            "executor_id": "executor",
            "maximum_active_turns": maximum_active_turns,
        }))
        .unwrap();
        config.validate().unwrap();
    }
    for maximum_active_turns in [0, MAXIMUM_EXECUTOR_ACTIVE_TURNS + 1] {
        let config: ExecutorConfig = serde_json::from_value(serde_json::json!({
            "executor_id": "executor",
            "maximum_active_turns": maximum_active_turns,
        }))
        .unwrap();
        assert!(matches!(config.validate(), Err(ExecutorError::Invalid(_))));
    }
}

#[test]
fn context_failure_diagnostic_is_utf8_safe_and_protocol_bounded() {
    let outcome = failure_outcome(
        "context.invalid",
        "界".repeat(MAXIMUM_AGENT_DIAGNOSTIC_BYTES),
    );
    outcome.validate().unwrap();
    let TurnOutcome::Failed { message, .. } = outcome else {
        panic!("context failure must preserve its typed terminal class");
    };
    assert!(message.len() <= MAXIMUM_AGENT_DIAGNOSTIC_BYTES);
    assert!(std::str::from_utf8(message.as_bytes()).is_ok());
}

#[test]
fn tool_admission_failures_keep_their_stable_terminal_codes() {
    for (error, expected_code, expected_message) in [
        (
            ToolError::Capacity,
            "tool.capacity",
            "Tool capacity is exhausted",
        ),
        (
            ToolError::ShuttingDown,
            "tool.shutting_down",
            "Tool provider is shutting down",
        ),
    ] {
        let DriveFailure::Turn(TurnOutcome::Failed { code, message }) = tool_failure(&error) else {
            panic!("tool admission failure changed terminal class")
        };
        assert_eq!(code, expected_code);
        assert_eq!(message, expected_message);
    }
}

#[test]
fn finalization_priority_matrix_preserves_partial_media_and_ignores_blockers_for_failures() {
    let media = MediaRef {
        id: MediaId::new("a".repeat(64)).unwrap(),
        mime: "image/png".into(),
        bytes: 1,
        width: 1,
        height: 1,
    };
    assert_eq!(
        apply_finalization_failure(
            TurnOutcome::PartialFailed {
                media: vec![media.clone()],
                code: "image.failure".into(),
                message: "image failed".into(),
            },
            "jobs.cleanup",
            "cleanup failed",
            true,
        ),
        TurnOutcome::PartialFailed {
            media: vec![media],
            code: "jobs.cleanup".into(),
            message: "cleanup failed".into(),
        }
    );
    assert_eq!(
        apply_finalization_failure(
            TurnOutcome::Cancelled,
            "jobs.unreported",
            "output was not collected",
            false,
        ),
        TurnOutcome::Cancelled
    );
    assert!(matches!(
        apply_finalization_failure(
            TurnOutcome::BudgetExceeded {
                dimension: BudgetDimension::Elapsed,
                consumed: 10,
                limit: 10,
            },
            "jobs.cleanup",
            "cleanup failed",
            true,
        ),
        TurnOutcome::Failed { code, .. } if code == "jobs.cleanup"
    ));
}

#[tokio::test]
async fn completed_drive_wins_when_the_elapsed_deadline_is_also_ready() {
    let stop = CancellationToken::new();
    stop.cancel();

    let drive = select_drive_or_stop(
        &stop,
        std::future::ready(Err(DriveFailure::Turn(TurnOutcome::Completed))),
    )
    .await;

    assert!(matches!(
        drive,
        Err(DriveFailure::Turn(TurnOutcome::Completed))
    ));
    assert!(!elapsed_deadline_wins(true, &drive));
}

#[derive(Debug)]
struct FullBeforePublish {
    facts: Vec<Arc<SessionFact>>,
    required_flush_seq: u64,
    durable_seq: AtomicU64,
    publish_calls: AtomicUsize,
    flushes: Mutex<Vec<u64>>,
    shutdown_on_publish: bool,
}

#[derive(Debug)]
struct CheckpointFixture {
    facts: Vec<Arc<SessionFact>>,
    fork_facts: Vec<Arc<SessionFact>>,
    writes: Mutex<Vec<ContextCheckpoint>>,
}

#[async_trait]
impl TurnExecution for CheckpointFixture {
    fn register(&self, _executor_id: String) -> rsi_agent_turn_protocol::Result<ExecutorLease> {
        unreachable!("checkpoint writer test does not register")
    }

    async fn claim(
        &self,
        _executor_id: &str,
        _cancellation: CancellationToken,
    ) -> rsi_agent_turn_protocol::Result<Option<TurnClaim>> {
        unreachable!("checkpoint writer test does not claim")
    }

    fn composition(
        &self,
        _claim: &TurnClaim,
    ) -> rsi_agent_turn_protocol::Result<AgentCompositionPin> {
        unreachable!("checkpoint writer test does not resolve composition")
    }

    async fn read_fork_facts(
        &self,
        _claim: &TurnClaim,
        _after_parent_seq: u64,
        _limit: usize,
    ) -> rsi_agent_turn_protocol::Result<Option<ForkFactPage>> {
        unreachable!("checkpoint writer uses only maintenance fork reads")
    }

    async fn enter_pending_step_messages(
        &self,
        _claim: &TurnClaim,
    ) -> rsi_agent_turn_protocol::Result<usize> {
        unreachable!("checkpoint writer does not enter messages")
    }

    async fn refresh_workspace_context(
        &self,
        _claim: &TurnClaim,
    ) -> rsi_agent_turn_protocol::Result<usize> {
        unreachable!("checkpoint writer does not refresh workspace context")
    }

    async fn close_current_step(
        &self,
        _claim: &TurnClaim,
        _outcome: &TurnOutcome,
    ) -> rsi_agent_turn_protocol::Result<()> {
        unreachable!("checkpoint writer does not close Steps")
    }

    async fn finish_activation_turn(
        &self,
        _claim: &TurnClaim,
        _outcome: &TurnOutcome,
    ) -> rsi_agent_turn_protocol::Result<Option<Arc<SessionFact>>> {
        unreachable!("checkpoint writer does not settle activations")
    }

    async fn read_facts(
        &self,
        _claim: &TurnClaim,
        _after_seq: u64,
        _limit: usize,
    ) -> rsi_agent_turn_protocol::Result<rsi_agent_turn_protocol::ClaimFactPage> {
        unreachable!("checkpoint writer uses only maintenance reads")
    }

    async fn read_checkpoint_facts(
        &self,
        _claim: &TurnClaim,
        after_seq: u64,
        limit: usize,
    ) -> rsi_agent_turn_protocol::Result<Option<rsi_agent_turn_protocol::ClaimFactPage>> {
        let facts = self
            .facts
            .iter()
            .filter(|fact| fact.seq() > after_seq)
            .take(limit)
            .cloned()
            .collect::<Vec<_>>();
        Ok(Some(rsi_agent_turn_protocol::ClaimFactPage {
            through_seq: facts.last().map_or(after_seq, |fact| fact.seq()),
            facts,
        }))
    }

    async fn read_checkpoint_fork_facts(
        &self,
        _claim: &TurnClaim,
        after_parent_seq: u64,
        limit: usize,
    ) -> rsi_agent_turn_protocol::Result<Option<ForkFactPage>> {
        let facts = self
            .fork_facts
            .iter()
            .filter(|fact| fact.seq() > after_parent_seq)
            .take(limit)
            .cloned()
            .collect::<Vec<_>>();
        let terminal_parent_seq = self.fork_facts.last().map_or(0, |fact| fact.seq());
        Ok(Some(ForkFactPage {
            through_parent_seq: facts.last().map_or(after_parent_seq, |fact| fact.seq()),
            terminal_parent_seq,
            facts,
        }))
    }

    async fn write_context_checkpoint(
        &self,
        _claim: &TurnClaim,
        checkpoint: ContextCheckpoint,
    ) -> rsi_agent_turn_protocol::Result<bool> {
        self.writes.lock().unwrap().push(checkpoint);
        Ok(true)
    }

    async fn publish(
        &self,
        _claim: &TurnClaim,
        _bodies: Vec<SessionFactBody>,
    ) -> rsi_agent_turn_protocol::Result<PublishAttempt> {
        unreachable!("checkpoint writer test does not publish")
    }

    async fn flush(
        &self,
        _claim: &TurnClaim,
        _through_seq: u64,
    ) -> rsi_agent_turn_protocol::Result<u64> {
        unreachable!("checkpoint writer test does not flush")
    }

    fn cancellation(
        &self,
        _claim: &TurnClaim,
    ) -> rsi_agent_turn_protocol::Result<CancellationToken> {
        unreachable!("checkpoint writer test does not cancel")
    }

    fn release(&self, _claim: &TurnClaim) -> rsi_agent_turn_protocol::Result<()> {
        unreachable!("checkpoint writer test does not release")
    }
}

#[async_trait]
impl TurnExecution for FullBeforePublish {
    fn register(&self, _executor_id: String) -> rsi_agent_turn_protocol::Result<ExecutorLease> {
        unreachable!("terminal publication test does not register")
    }

    async fn claim(
        &self,
        _executor_id: &str,
        _cancellation: CancellationToken,
    ) -> rsi_agent_turn_protocol::Result<Option<TurnClaim>> {
        unreachable!("terminal publication test does not claim")
    }

    fn composition(
        &self,
        _claim: &TurnClaim,
    ) -> rsi_agent_turn_protocol::Result<AgentCompositionPin> {
        unreachable!("terminal publication test does not resolve composition")
    }

    async fn read_fork_facts(
        &self,
        _claim: &TurnClaim,
        _after_parent_seq: u64,
        _limit: usize,
    ) -> rsi_agent_turn_protocol::Result<Option<ForkFactPage>> {
        unreachable!("terminal publication test does not read fork Facts")
    }

    async fn enter_pending_step_messages(
        &self,
        _claim: &TurnClaim,
    ) -> rsi_agent_turn_protocol::Result<usize> {
        unreachable!("terminal publication test does not enter messages")
    }

    async fn refresh_workspace_context(
        &self,
        _claim: &TurnClaim,
    ) -> rsi_agent_turn_protocol::Result<usize> {
        unreachable!("terminal publication test does not refresh workspace context")
    }

    async fn close_current_step(
        &self,
        _claim: &TurnClaim,
        _outcome: &TurnOutcome,
    ) -> rsi_agent_turn_protocol::Result<()> {
        Ok(())
    }

    async fn finish_activation_turn(
        &self,
        _claim: &TurnClaim,
        _outcome: &TurnOutcome,
    ) -> rsi_agent_turn_protocol::Result<Option<Arc<SessionFact>>> {
        Ok(None)
    }

    async fn read_facts(
        &self,
        _claim: &TurnClaim,
        after_seq: u64,
        limit: usize,
    ) -> rsi_agent_turn_protocol::Result<rsi_agent_turn_protocol::ClaimFactPage> {
        let facts = self
            .facts
            .iter()
            .filter(|fact| fact.seq() > after_seq)
            .take(limit)
            .cloned()
            .collect::<Vec<_>>();
        Ok(rsi_agent_turn_protocol::ClaimFactPage {
            through_seq: facts.last().map_or(after_seq, |fact| fact.seq()),
            facts,
        })
    }

    async fn publish(
        &self,
        _claim: &TurnClaim,
        bodies: Vec<SessionFactBody>,
    ) -> rsi_agent_turn_protocol::Result<PublishAttempt> {
        if self.shutdown_on_publish {
            return Err(TurnError::ShuttingDown);
        }
        if self.publish_calls.fetch_add(1, Ordering::SeqCst) == 0 {
            return Ok(PublishAttempt::FlushRequired {
                unpublished: bodies,
            });
        }
        if self.durable_seq.load(Ordering::SeqCst) < self.required_flush_seq {
            return Ok(PublishAttempt::FlushRequired {
                unpublished: bodies,
            });
        }
        let next_seq = self
            .facts
            .last()
            .map_or(1, |fact| fact.seq().saturating_add(1));
        Ok(PublishAttempt::Published(vec![Arc::new(
            SessionFact::new(next_seq, next_seq, bodies.into_iter().next().unwrap()).unwrap(),
        )]))
    }

    async fn flush(
        &self,
        _claim: &TurnClaim,
        through_seq: u64,
    ) -> rsi_agent_turn_protocol::Result<u64> {
        self.flushes.lock().unwrap().push(through_seq);
        self.durable_seq.store(through_seq, Ordering::SeqCst);
        Ok(through_seq)
    }

    fn cancellation(
        &self,
        _claim: &TurnClaim,
    ) -> rsi_agent_turn_protocol::Result<CancellationToken> {
        unreachable!("terminal publication test does not cancel")
    }

    fn release(&self, _claim: &TurnClaim) -> rsi_agent_turn_protocol::Result<()> {
        unreachable!("terminal publication test does not release")
    }
}

fn claim() -> (TurnClaim, SessionFact) {
    let session_id = SessionId::new("session-terminal-retry").unwrap();
    let turn_id = TurnId::new("turn-terminal-retry").unwrap();
    let header = SessionHeader::new(
        session_id.clone(),
        1,
        "/tmp",
        AgentPresetId::new("test-agent").unwrap(),
        FrozenAgentSettings::new(
            "test",
            "system",
            ModelRef::new("test", "model").unwrap(),
            SandboxMode::WorkspaceWrite,
            false,
        )
        .unwrap(),
    )
    .unwrap();
    let accepted = SessionFact::new(
        1,
        1,
        SessionFactBody::TurnAccepted {
            turn_id: turn_id.clone(),
            text: "task".into(),
            model: None,
            sandbox: SandboxMode::WorkspaceWrite,
            require_approval: false,
        },
    )
    .unwrap();
    (
        rsi_agent_turn_protocol::TurnClaimIssuer::new().issue(
            "executor".into(),
            1,
            session_id,
            turn_id,
            Arc::new(header),
            1,
            1,
            1,
        ),
        accepted,
    )
}

fn completed_turn_facts(turn_id: TurnId, text: &str) -> Vec<Arc<SessionFact>> {
    vec![
        Arc::new(
            SessionFact::new(
                1,
                1,
                SessionFactBody::TurnAccepted {
                    turn_id: turn_id.clone(),
                    text: text.into(),
                    model: None,
                    sandbox: SandboxMode::WorkspaceWrite,
                    require_approval: false,
                },
            )
            .unwrap(),
        ),
        Arc::new(
            SessionFact::new(
                2,
                2,
                SessionFactBody::TurnTerminal {
                    turn_id,
                    outcome: TurnOutcome::Completed,
                },
            )
            .unwrap(),
        ),
    ]
}

fn fork_checkpoint_claim(parent_session_id: SessionId) -> TurnClaim {
    let session_id = SessionId::new("session-checkpoint-child").unwrap();
    let turn_id = TurnId::new("turn-checkpoint-child").unwrap();
    let header = SessionHeader::new(
        session_id.clone(),
        1,
        "/tmp",
        AgentPresetId::new("test-agent").unwrap(),
        FrozenAgentSettings::new(
            "test",
            "system",
            ModelRef::new("test", "model").unwrap(),
            SandboxMode::WorkspaceWrite,
            false,
        )
        .unwrap(),
    )
    .unwrap()
    .with_fork_origin(ForkOrigin {
        parent_session_id: parent_session_id.clone(),
        root_session_id: parent_session_id,
        path: AgentPath::new(vec![1]).unwrap(),
        task_name: "checkpoint-child".into(),
        parent_header_fingerprint: "a".repeat(64),
        invoking_turn_id: TurnId::new("turn-checkpoint-spawn").unwrap(),
        resolved_after_seq: 0,
        resolved_terminal_seq: 2,
        terminal_prefix_sha256: "b".repeat(64),
        requested_turns: ForkTurnSelection::All,
        effective_turns: 1,
    })
    .unwrap();
    rsi_agent_turn_protocol::TurnClaimIssuer::new().issue(
        "executor".into(),
        1,
        session_id,
        turn_id,
        Arc::new(header),
        1,
        1,
        2,
    )
}

#[tokio::test]
async fn terminal_publication_flushes_and_retries_a_full_speculative_suffix() {
    let (claim, accepted) = claim();
    let turns = FullBeforePublish {
        facts: vec![Arc::new(accepted)],
        required_flush_seq: 1,
        durable_seq: AtomicU64::new(0),
        publish_calls: AtomicUsize::new(0),
        flushes: Mutex::new(Vec::new()),
        shutdown_on_publish: false,
    };
    let config = ExecutorConfig {
        executor_id: "executor".into(),
        maximum_active_turns: 1,
        max_context_messages: default_context_messages(),
        max_context_bytes: default_context_bytes(),
        durability_wait_ms: 1_000,
        finalization_wait_ms: 1_000,
        retained_tool_wait_ms: 1_000,
    };

    publish_terminal(&turns, &config, &claim, TurnOutcome::Completed)
        .await
        .unwrap();
    assert_eq!(turns.publish_calls.load(Ordering::SeqCst), 2);
    assert_eq!(turns.flushes.lock().unwrap().as_slice(), [1, 2]);
}

#[tokio::test]
async fn terminal_publication_treats_kernel_shutdown_as_driver_stop() {
    let (claim, accepted) = claim();
    let turns = FullBeforePublish {
        facts: vec![Arc::new(accepted)],
        required_flush_seq: 1,
        durable_seq: AtomicU64::new(0),
        publish_calls: AtomicUsize::new(0),
        flushes: Mutex::new(Vec::new()),
        shutdown_on_publish: true,
    };
    let config = ExecutorConfig {
        executor_id: "executor".into(),
        maximum_active_turns: 1,
        max_context_messages: default_context_messages(),
        max_context_bytes: default_context_bytes(),
        durability_wait_ms: 1_000,
        finalization_wait_ms: 1_000,
        retained_tool_wait_ms: 1_000,
    };

    assert!(matches!(
        publish_terminal(&turns, &config, &claim, TurnOutcome::Completed).await,
        Err(DriveFailure::Stopped)
    ));
}

#[tokio::test]
async fn nonterminal_publication_flushes_the_live_tail_when_the_fold_lags() {
    let (claim, accepted) = claim();
    let later = SessionFact::new(
        2,
        2,
        SessionFactBody::CancelRequested {
            turn_id: claim.turn_id().clone(),
            reason: Some("published outside the fold".into()),
        },
    )
    .unwrap();
    let turns = FullBeforePublish {
        facts: vec![Arc::new(accepted), Arc::new(later)],
        required_flush_seq: 2,
        durable_seq: AtomicU64::new(0),
        publish_calls: AtomicUsize::new(0),
        flushes: Mutex::new(Vec::new()),
        shutdown_on_publish: false,
    };
    let config = ExecutorConfig {
        executor_id: "executor".into(),
        maximum_active_turns: 1,
        max_context_messages: default_context_messages(),
        max_context_bytes: default_context_bytes(),
        durability_wait_ms: 1_000,
        finalization_wait_ms: 1_000,
        retained_tool_wait_ms: 1_000,
    };

    let facts = publish_nonterminal_with_capacity_retry(
        &turns,
        &config,
        &claim,
        vec![SessionFactBody::CancelRequested {
            turn_id: claim.turn_id().clone(),
            reason: Some("retry".into()),
        }],
    )
    .await
    .unwrap();

    assert_eq!(facts.last().unwrap().seq(), 3);
    assert_eq!(turns.flushes.lock().unwrap().as_slice(), [2]);
}

#[tokio::test]
async fn checkpoint_writer_drains_a_coalesced_request_after_close() {
    let (claim, accepted) = claim();
    let queued_turn = TurnId::new("turn-queued").unwrap();
    let fixture = Arc::new(CheckpointFixture {
        facts: vec![
            Arc::new(accepted),
            Arc::new(
                SessionFact::new(
                    2,
                    2,
                    SessionFactBody::TurnTerminal {
                        turn_id: claim.turn_id().clone(),
                        outcome: TurnOutcome::Completed,
                    },
                )
                .unwrap(),
            ),
            Arc::new(
                SessionFact::new(
                    3,
                    3,
                    SessionFactBody::TurnAccepted {
                        turn_id: queued_turn,
                        text: "queued task".into(),
                        model: None,
                        sandbox: SandboxMode::WorkspaceWrite,
                        require_approval: false,
                    },
                )
                .unwrap(),
            ),
        ],
        fork_facts: vec![],
        writes: Mutex::new(Vec::new()),
    });
    let turns: Arc<dyn TurnExecution> = fixture.clone();
    let scheduler = Arc::new(CheckpointScheduler::new());
    let request = CheckpointRequest::new(claim.clone(), ContextLimits::default());
    assert_eq!(
        scheduler.schedule(request.clone()),
        checkpoint::ScheduleOutcome::Scheduled
    );
    assert_eq!(
        scheduler.schedule(request),
        checkpoint::ScheduleOutcome::Coalesced
    );
    let checkpoint_task = tokio::spawn(run_checkpoint_writer(turns, Arc::clone(&scheduler)));
    scheduler.close();
    tokio::time::timeout(Duration::from_secs(1), checkpoint_task)
        .await
        .expect("checkpoint writer did not drain the admitted request")
        .unwrap();

    let writes = fixture.writes.lock().unwrap();
    assert_eq!(writes.len(), 1);
    assert_eq!(writes[0].through_seq, 3);
    let restored = ContextFold::from_checkpoint(
        claim.header().clone(),
        ContextLimits::default(),
        &writes[0].bytes,
    )
    .unwrap();
    assert_eq!(restored.through_seq(), 3);
    assert!(
        serde_json::to_string(&restored.project(ContextLimits::default()).unwrap().messages)
            .unwrap()
            .contains("queued task")
    );
}

#[tokio::test]
async fn first_fork_checkpoint_includes_the_terminal_parent_prefix() {
    let parent_session_id = SessionId::new("session-checkpoint-parent").unwrap();
    let parent_facts = completed_turn_facts(
        TurnId::new("turn-checkpoint-parent").unwrap(),
        "inherited task",
    );
    let claim = fork_checkpoint_claim(parent_session_id);
    let child_facts =
        completed_turn_facts(TurnId::new("turn-checkpoint-child").unwrap(), "child task");
    let fixture = Arc::new(CheckpointFixture {
        facts: child_facts,
        fork_facts: parent_facts,
        writes: Mutex::new(Vec::new()),
    });
    let turns: Arc<dyn TurnExecution> = fixture.clone();
    let scheduler = Arc::new(CheckpointScheduler::new());
    assert_eq!(
        scheduler.schedule(CheckpointRequest::new(
            claim.clone(),
            ContextLimits::default(),
        )),
        checkpoint::ScheduleOutcome::Scheduled
    );
    let checkpoint_task = tokio::spawn(run_checkpoint_writer(turns, Arc::clone(&scheduler)));
    scheduler.close();
    tokio::time::timeout(Duration::from_secs(1), checkpoint_task)
        .await
        .expect("fork checkpoint writer did not settle")
        .unwrap();

    let writes = fixture.writes.lock().unwrap();
    assert_eq!(writes.len(), 1);
    let checkpoint = &writes[0];
    let restored = ContextFold::from_checkpoint(
        claim.header().clone(),
        ContextLimits::default(),
        &checkpoint.bytes,
    )
    .unwrap();
    let messages =
        serde_json::to_string(&restored.project(ContextLimits::default()).unwrap().messages)
            .unwrap();
    assert!(messages.contains("inherited task"));
    assert!(messages.contains("child task"));
    assert_eq!(checkpoint.through_seq, 2);
}

#[test]
fn completed_model_without_a_successor_is_not_classified_as_fresh_work() {
    let (claim, accepted) = claim();
    let effect_id = EffectId::new("effect-model-complete").unwrap();
    let facts = vec![
        accepted,
        SessionFact::new(
            2,
            2,
            SessionFactBody::ModelIntent {
                turn_id: claim.turn_id().clone(),
                effect_id: effect_id.clone(),
                snapshot: PreparedCallSnapshot {
                    call_id: "call-1".into(),
                    deployment_id: "test".into(),
                    provider_family: "test".into(),
                    capability: rsi_ai_protocol::AiCapability::Language,
                    model: "model".into(),
                    protocol: "test".into(),
                    transport: "memory".into(),
                    endpoint_fingerprint: "endpoint".into(),
                    config_generation: 1,
                    credential_source: None,
                    retry_policy: rsi_ai_protocol::RetryPolicy::default(),
                    request_sha256: "a".repeat(64),
                },
            },
        )
        .unwrap(),
        SessionFact::new(
            3,
            3,
            SessionFactBody::ModelStarted {
                turn_id: claim.turn_id().clone(),
                effect_id: effect_id.clone(),
            },
        )
        .unwrap(),
        SessionFact::new(
            4,
            4,
            SessionFactBody::ModelEvent {
                turn_id: claim.turn_id().clone(),
                effect_id,
                event: LanguageEvent::Finished {
                    reason: FinishReason::Stop,
                    replay: None,
                },
            },
        )
        .unwrap(),
    ];
    let mut state = ScannedTurn::default();
    let facts = facts.into_iter().map(Arc::new).collect::<Vec<_>>();
    scan_turn(&claim, &mut state, &facts).unwrap();
    assert!(state.completed_model_without_successor);
    assert!(state.effects.is_empty());
}

#[test]
fn finalization_deadline_is_bounded_during_factory_preparation() {
    let factory = ExecutorFactory;
    for wait in [0, MAXIMUM_FINALIZATION_WAIT_MS + 1] {
        let error = factory
            .prepare(&serde_json::json!({
                "executor_id": "executor",
                "finalization_wait_ms": wait
            }))
            .expect_err("unbounded finalization deadline");
        assert!(error.to_string().contains("finalization_wait_ms"));
    }
}

#[test]
fn retained_tool_deadline_is_bounded_during_factory_preparation() {
    let factory = ExecutorFactory;
    for wait in [0, MAXIMUM_RETAINED_TOOL_WAIT_MS + 1] {
        let error = factory
            .prepare(&serde_json::json!({
                "executor_id": "executor",
                "retained_tool_wait_ms": wait
            }))
            .expect_err("unbounded retained Tool deadline");
        assert!(error.to_string().contains("retained_tool_wait_ms"));
    }
}
