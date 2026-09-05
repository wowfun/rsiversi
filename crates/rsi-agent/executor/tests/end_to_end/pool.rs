use super::*;

#[tokio::test(start_paused = true)]
async fn bounded_pool_enforces_its_peak_and_progresses_independent_sessions() {
    let stack = BaseStack::activate().await;
    let first_entered = Arc::new(Notify::new());
    let first_release = Arc::new(Notify::new());
    let second_entered = Arc::new(Notify::new());
    let second_release = Arc::new(Notify::new());
    let third_entered = Arc::new(Notify::new());
    let third_release = Arc::new(Notify::new());
    let starts = Arc::new(AtomicUsize::new(0));
    let fixture = Arc::new(LanguageFixture {
        outcomes: Mutex::new(VecDeque::from([
            gated_answer(&first_entered, &first_release),
            gated_answer(&second_entered, &second_release),
            gated_answer(&third_entered, &third_release),
        ])),
        requests: Mutex::new(vec![]),
        starts: Arc::clone(&starts),
        store: stack.store.clone(),
        retry_policy: RetryPolicy::default(),
    });
    let language_fiber = stack
        .activate_language("test.language.concurrent-sessions", fixture)
        .await;
    let executor_fiber = stack
        .activate_executor_with_config(json!({
            "executor_id": "executor-concurrent-sessions",
            "maximum_active_turns": 2,
        }))
        .await;
    let turns = stack
        .runtime
        .root()
        .lookup_local::<TurnServiceContract>()
        .unwrap();
    let first = stack
        .submit_fresh(&turns, "session-concurrent-a", "block session A")
        .await;
    tokio::time::timeout(std::time::Duration::from_secs(2), first_entered.notified())
        .await
        .expect("session A did not enter its provider stream");
    let second = stack
        .submit_fresh(&turns, "session-concurrent-b", "complete session B")
        .await;
    tokio::time::timeout(std::time::Duration::from_secs(2), second_entered.notified())
        .await
        .expect("session B did not enter its provider stream");
    let third = stack
        .submit_fresh(&turns, "session-concurrent-c", "wait for pool capacity")
        .await;
    assert!(
        tokio::time::timeout(
            std::time::Duration::from_millis(1),
            third_entered.notified(),
        )
        .await
        .is_err(),
        "a third provider started above the configured two-lane peak"
    );
    assert_eq!(starts.load(Ordering::Acquire), 2);
    assert_eq!(
        turns
            .outcome(&third.session_id, &third.turn_id)
            .await
            .unwrap(),
        None,
        "the third Session must wait while both configured lanes are active"
    );

    second_release.notify_one();
    assert_eq!(
        wait_for_outcome(&turns, &second).await,
        TurnOutcome::Completed
    );
    tokio::time::timeout(std::time::Duration::from_secs(2), third_entered.notified())
        .await
        .expect("session C did not start after one lane settled");
    assert_eq!(starts.load(Ordering::Acquire), 3);
    assert_eq!(
        turns
            .outcome(&first.session_id, &first.turn_id)
            .await
            .unwrap(),
        None,
        "session A must still be blocked while session B completes"
    );
    third_release.notify_one();
    assert_eq!(
        wait_for_outcome(&turns, &third).await,
        TurnOutcome::Completed
    );
    first_release.notify_one();
    assert_eq!(
        wait_for_outcome(&turns, &first).await,
        TurnOutcome::Completed
    );

    drop(turns);
    stack.dispose(language_fiber, executor_fiber).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[allow(clippy::too_many_lines)] // The public seam needs its complete dependency stack visible.
async fn lane_failure_waits_for_siblings_before_executor_cleanup() {
    let runtime = Runtime::default();
    let lease_dropped = Arc::new(AtomicBool::new(false));
    let claim_header = Arc::new(header_for_session(
        "session-failing-claim",
        TurnBudget::default(),
    ));
    let claim = TurnClaimIssuer::new().issue(
        "executor-failing-claim".into(),
        1,
        claim_header.session_id().clone(),
        TurnId::new("turn-failing-claim").unwrap(),
        claim_header,
        u64::MAX,
        1,
        1,
    );
    let sibling_release = Arc::new((Mutex::new(false), Condvar::new()));
    let turns = Arc::new(FailingClaimFixture {
        claims: AtomicUsize::new(0),
        claim: Mutex::new(Some(claim)),
        sibling_started: Arc::new(Notify::new()),
        sibling_cancelled: Arc::new(Notify::new()),
        sibling_release: Arc::clone(&sibling_release),
        lease_dropped: Arc::clone(&lease_dropped),
    });
    let turns_fiber = activate_fixture(
        &runtime,
        "test.turns.failing-claim",
        "turns",
        Arc::new(FailingClaimFixtureFactory {
            fixture: Arc::clone(&turns),
        }),
    )
    .await;
    let jobs_fiber = activate_fixture(
        &runtime,
        "rsi.jobs.local",
        "jobs",
        Arc::new(JobsLocalFactory),
    )
    .await;
    let security_fiber = activate_fixture(
        &runtime,
        "test.security.failing-claim",
        "security",
        Arc::new(SecurityFixtureFactory),
    )
    .await;
    let store = Arc::new(MemoryStore::new());
    let image_media_fiber = activate_fixture(
        &runtime,
        "test.image-media.failing-claim",
        "image-media",
        Arc::new(ImageMediaFixtureFactory {
            image: Arc::new(ImageFixture {
                events: Mutex::new(VecDeque::new()),
                store: Arc::clone(&store),
            }),
            media: Arc::new(MediaFixture {
                imports: AtomicUsize::new(0),
                store,
            }),
        }),
    )
    .await;
    let language_fiber = activate_fixture(
        &runtime,
        "test.language.failing-claim",
        "language",
        Arc::new(PendingLanguageFactory {
            fixture: Arc::new(PendingLanguage {
                entered: Arc::new(Notify::new()),
            }),
        }),
    )
    .await;
    let executor_fiber = activate_configured_fixture(
        &runtime,
        "rsi.agent.executor",
        "executor",
        Arc::new(ExecutorFactory),
        json!({
            "executor_id": "executor-failing-claim",
            "maximum_active_turns": 2,
        }),
    )
    .await;

    tokio::time::timeout(
        std::time::Duration::from_secs(2),
        turns.sibling_cancelled.notified(),
    )
    .await
    .expect("the sibling lane did not observe pool cancellation");
    let disposal = executor_fiber.dispose();
    tokio::pin!(disposal);
    tokio::select! {
        biased;
        report = &mut disposal => panic!("executor cleanup raced its live sibling: {report:?}"),
        () = tokio::task::yield_now() => {}
    }
    assert!(
        !lease_dropped.load(Ordering::Acquire),
        "the shared executor lease must remain owned while a sibling lane is settling"
    );

    let (released, changed) = &*sibling_release;
    *released.lock().unwrap() = true;
    changed.notify_one();
    let report = disposal.await;
    assert_eq!(report.total_failures(), 1);
    assert!(report.failures().iter().any(|failure| {
        failure
            .error
            .contains("Agent executor claim lane failed: Agent executor claim is stale")
    }));
    assert!(lease_dropped.load(Ordering::Acquire));

    for fiber in [
        language_fiber,
        image_media_fiber,
        security_fiber,
        jobs_fiber,
        turns_fiber,
    ] {
        assert!(fiber.dispose().await.is_clean());
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn parked_tool_releases_a_single_lane_and_reacquires_it_before_returning() {
    let stack = BaseStack::activate().await;
    let parked = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    let tools = Arc::clone(&stack.tool_registrar);
    let parking_lease = tools
        .register(ToolRegistration {
            definition: ToolDefinition::new("park", "park", json!({"type":"object"}))
                .unwrap()
                .with_scheduling(ToolScheduling::ExclusiveFinal),
            timeout_ms: 5_000,
            executor: Arc::new(ParkingTool {
                parked: Arc::clone(&parked),
                release: Arc::clone(&release),
            }),
        })
        .unwrap();
    let fixture = Arc::new(LanguageFixture {
        outcomes: Mutex::new(VecDeque::from([
            StartOutcome::Stream(tool_calls_script(&[("park-call", "park", r"{}")])),
            StartOutcome::Stream(answer_script()),
            StartOutcome::Stream(answer_script()),
        ])),
        requests: Mutex::new(vec![]),
        starts: Arc::new(AtomicUsize::new(0)),
        store: stack.store.clone(),
        retry_policy: RetryPolicy::default(),
    });
    let language_fiber = stack
        .activate_language("test.language.parked-lane", fixture)
        .await;
    let executor_fiber = stack
        .activate_executor_with_config(json!({
            "executor_id": "executor-parked-lane",
            "maximum_active_turns": 1,
        }))
        .await;
    let turns = stack
        .runtime
        .root()
        .lookup_local::<TurnServiceContract>()
        .unwrap();
    let first = stack
        .submit_fresh(&turns, "session-parked-lane-a", "park")
        .await;
    tokio::time::timeout(std::time::Duration::from_secs(2), parked.notified())
        .await
        .expect("the first Tool did not release its executor lane");

    let second = stack
        .submit_fresh(&turns, "session-parked-lane-b", "run while parked")
        .await;
    assert_eq!(
        wait_for_outcome(&turns, &second).await,
        TurnOutcome::Completed
    );
    assert!(
        turns
            .outcome(&first.session_id, &first.turn_id)
            .await
            .unwrap()
            .is_none()
    );

    release.notify_one();
    assert_eq!(
        wait_for_outcome(&turns, &first).await,
        TurnOutcome::Completed
    );

    drop((parking_lease, tools, turns));
    stack.dispose(language_fiber, executor_fiber).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn single_lane_configuration_keeps_independent_sessions_serial() {
    let stack = BaseStack::activate().await;
    let first_entered = Arc::new(Notify::new());
    let first_release = Arc::new(Notify::new());
    let second_entered = Arc::new(Notify::new());
    let second_release = Arc::new(Notify::new());
    let starts = Arc::new(AtomicUsize::new(0));
    let fixture = Arc::new(LanguageFixture {
        outcomes: Mutex::new(VecDeque::from([
            StartOutcome::GatedStream {
                events: answer_script(),
                waiting_after_first: Arc::clone(&first_entered),
                release: Arc::clone(&first_release),
            },
            StartOutcome::GatedStream {
                events: answer_script(),
                waiting_after_first: Arc::clone(&second_entered),
                release: Arc::clone(&second_release),
            },
        ])),
        requests: Mutex::new(vec![]),
        starts: Arc::clone(&starts),
        store: stack.store.clone(),
        retry_policy: RetryPolicy::default(),
    });
    let language_fiber = stack
        .activate_language("test.language.single-lane", fixture)
        .await;
    let executor_fiber = stack
        .activate_executor_with_config(json!({
            "executor_id": "executor-single-lane",
            "maximum_active_turns": 1,
        }))
        .await;
    let turns = stack
        .runtime
        .root()
        .lookup_local::<TurnServiceContract>()
        .unwrap();
    let first = turns
        .submit(SubmitTurn {
            turn_id: client_turn_id(),
            session: stack
                .fresh(header_for_session(
                    "session-single-a",
                    TurnBudget::default(),
                ))
                .await,
            text: "first".into(),
            model: None,
            sandbox: None,
        })
        .await
        .unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(2), first_entered.notified())
        .await
        .expect("first session did not enter its provider stream");
    let second = turns
        .submit(SubmitTurn {
            turn_id: client_turn_id(),
            session: stack
                .fresh(header_for_session(
                    "session-single-b",
                    TurnBudget::default(),
                ))
                .await,
            text: "second".into(),
            model: None,
            sandbox: None,
        })
        .await
        .unwrap();
    tokio::task::yield_now().await;
    assert_eq!(starts.load(Ordering::Acquire), 1);

    first_release.notify_one();
    assert_eq!(
        wait_for_outcome(&turns, &first).await,
        TurnOutcome::Completed
    );
    tokio::time::timeout(std::time::Duration::from_secs(2), second_entered.notified())
        .await
        .expect("second session did not start after the first settled");
    assert_eq!(starts.load(Ordering::Acquire), 2);
    second_release.notify_one();
    assert_eq!(
        wait_for_outcome(&turns, &second).await,
        TurnOutcome::Completed
    );

    drop(turns);
    stack.dispose(language_fiber, executor_fiber).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn interleaved_same_session_submission_does_not_fail_the_streaming_turn() {
    let stack = BaseStack::activate().await;
    let waiting_after_first = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    let waiting_in_second = Arc::new(Notify::new());
    let release_second = Arc::new(Notify::new());
    let fixture = Arc::new(LanguageFixture {
        outcomes: Mutex::new(VecDeque::from([
            StartOutcome::GatedStream {
                events: answer_script(),
                waiting_after_first: Arc::clone(&waiting_after_first),
                release: Arc::clone(&release),
            },
            StartOutcome::GatedStream {
                events: answer_script(),
                waiting_after_first: Arc::clone(&waiting_in_second),
                release: Arc::clone(&release_second),
            },
        ])),
        requests: Mutex::new(vec![]),
        starts: Arc::new(AtomicUsize::new(0)),
        store: stack.store.clone(),
        retry_policy: RetryPolicy::default(),
    });
    let language_fiber = stack
        .activate_language("test.language.interleaved", fixture)
        .await;
    let executor_fiber = stack.activate_executor("executor-interleaved").await;
    let turns = stack
        .runtime
        .root()
        .lookup_local::<TurnServiceContract>()
        .unwrap();
    let first = turns
        .submit(SubmitTurn {
            turn_id: client_turn_id(),
            session: stack.fresh(header()).await,
            text: "first".into(),
            model: None,
            sandbox: None,
        })
        .await
        .unwrap();
    tokio::time::timeout(
        std::time::Duration::from_secs(2),
        waiting_after_first.notified(),
    )
    .await
    .expect("executor did not publish the first streamed event");
    let second = turns
        .submit(SubmitTurn {
            turn_id: client_turn_id(),
            session: SubmitSession::Resume(turns.prepare_resume(&first.session_id).await.unwrap()),
            text: "second".into(),
            model: None,
            sandbox: None,
        })
        .await
        .unwrap();
    release.notify_one();

    tokio::time::timeout(
        std::time::Duration::from_secs(2),
        waiting_in_second.notified(),
    )
    .await
    .expect("executor did not start the queued second turn");
    if let Some(checkpoint) = stack
        .store
        .read_context_checkpoint(&first.session_id)
        .await
        .unwrap()
    {
        assert!(
            checkpoint.through_seq >= second.accepted_seq,
            "a checkpoint racing a queued turn must include that accepted state"
        );
    }
    release_second.notify_one();

    for submitted in [&first, &second] {
        let outcome = tokio::time::timeout(std::time::Duration::from_secs(2), async {
            loop {
                if let Some(outcome) = turns
                    .outcome(&submitted.session_id, &submitted.turn_id)
                    .await
                    .unwrap()
                {
                    break outcome;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("interleaved turn did not terminate");
        assert_eq!(outcome, TurnOutcome::Completed);
    }

    drop(turns);
    stack.dispose(language_fiber, executor_fiber).await;
}

async fn run_retry_case(
    dispatch_status: DispatchStatus,
) -> (TurnOutcome, Vec<SessionFactBody>, usize) {
    let stack = BaseStack::activate().await;
    let starts = Arc::new(AtomicUsize::new(0));
    let fixture = Arc::new(LanguageFixture {
        outcomes: Mutex::new(VecDeque::from([
            StartOutcome::Error(
                AiError::new(
                    ErrorKind::RateLimited,
                    ErrorPhase::Connect,
                    dispatch_status,
                    "temporary refusal",
                )
                .unwrap(),
            ),
            StartOutcome::Stream(answer_script()),
        ])),
        requests: Mutex::new(vec![]),
        starts: starts.clone(),
        store: stack.store.clone(),
        retry_policy: RetryPolicy::new(1, vec![ErrorKind::RateLimited], 1, 1, 0).unwrap(),
    });
    let language_fiber = stack
        .activate_language("test.language.retry", fixture)
        .await;
    let executor_fiber = stack.activate_executor("executor-retry").await;
    let (submitted, outcome) = stack.submit_and_wait("retry safely").await;
    let facts = stack
        .store
        .read_facts(&submitted.session_id, 0, 64)
        .await
        .unwrap()
        .facts
        .into_iter()
        .map(|fact| fact.body().clone())
        .collect();
    let start_count = starts.load(Ordering::Acquire);

    stack.dispose(language_fiber, executor_fiber).await;
    (outcome, facts, start_count)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn retries_only_a_policy_admitted_proven_undispatched_model_attempt() {
    let (outcome, facts, starts) = run_retry_case(DispatchStatus::NotDispatched).await;
    assert_eq!(outcome, TurnOutcome::Completed);
    assert_eq!(starts, 2);
    assert_eq!(
        facts
            .iter()
            .filter(|body| matches!(body, SessionFactBody::ModelIntent { .. }))
            .count(),
        2
    );
    assert!(facts.iter().any(|body| matches!(
        body,
        SessionFactBody::ModelEvent {
            event: LanguageEvent::Failed { error, .. },
            ..
        } if error.dispatch_status() == DispatchStatus::NotDispatched
    )));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn never_retries_a_dispatch_uncertain_model_attempt() {
    let (outcome, facts, starts) = run_retry_case(DispatchStatus::Unknown).await;
    assert!(matches!(
        outcome,
        TurnOutcome::Interrupted {
            effect: Some(rsi_agent_session_protocol::EffectKind::Model),
            ..
        }
    ));
    assert_eq!(starts, 1);
    assert_eq!(
        facts
            .iter()
            .filter(|body| matches!(body, SessionFactBody::ModelIntent { .. }))
            .count(),
        1
    );
}

#[derive(Debug)]
pub(super) struct PanicCommitTools {
    pub(super) inner: Arc<dyn ToolRuntime>,
    pub(super) panicked: Arc<Notify>,
}

#[async_trait]
impl ToolRuntime for PanicCommitTools {
    fn definitions(&self) -> Vec<ToolDefinition> {
        self.inner.definitions()
    }
    fn prepare(
        &self,
        invocation_id: &str,
        call: rsi_tools_protocol::ToolCall,
    ) -> ToolResultType<Box<dyn rsi_tools_protocol::PreparedToolCall>> {
        self.inner.prepare(invocation_id, call)
    }
    fn query(
        &self,
        identity: &rsi_tools_protocol::ToolResultIdentity,
    ) -> ToolResultType<RetainedToolResult> {
        self.inner.query(identity)
    }
    async fn wait(
        &self,
        identity: &rsi_tools_protocol::ToolResultIdentity,
        cancellation: CancellationToken,
    ) -> ToolResultType<RetainedToolResult> {
        self.inner.wait(identity, cancellation).await
    }
    fn commit(&self, identity: &rsi_tools_protocol::ToolResultIdentity) -> ToolResultType<()> {
        self.inner.commit(identity)?;
        self.panicked.notify_one();
        panic!("injected Tool runtime commit panic");
    }
}

#[tokio::test]
async fn lane_panic_releases_tracking_pins_after_all_lanes_stop() {
    let stack = BaseStack::activate().await;
    let panicked = Arc::new(Notify::new());
    *stack.composition.panic_on_commit.lock().unwrap() = Some(panicked.clone());
    let _tool = stack
        .tool_registrar
        .register(ToolRegistration {
            definition: ToolDefinition::new("echo", "echo", json!({"type":"object"})).unwrap(),
            timeout_ms: 1000,
            executor: Arc::new(EchoTool {
                store: stack.store.clone(),
                calls: Arc::new(AtomicUsize::new(0)),
            }),
        })
        .unwrap();
    let language = stack
        .activate_language(
            "test.language.panic",
            Arc::new(LanguageFixture {
                outcomes: Mutex::new(VecDeque::from([StartOutcome::Stream(tool_script())])),
                requests: Mutex::new(Vec::new()),
                starts: Arc::new(AtomicUsize::new(0)),
                store: stack.store.clone(),
                retry_policy: RetryPolicy::default(),
            }),
        )
        .await;
    let executor = stack
        .activate_executor_with_config(json!({"executor_id":"executor-panic"}))
        .await;
    let turns = stack
        .runtime
        .root()
        .lookup_local::<TurnServiceContract>()
        .unwrap();
    stack
        .submit_fresh(&turns, "session-panic", "panic after a durable result")
        .await;
    tokio::time::timeout(std::time::Duration::from_secs(2), panicked.notified())
        .await
        .unwrap();
    let execution = stack
        .runtime
        .root()
        .lookup_local::<TurnExecutionContract>()
        .unwrap();
    let lease = execution.register("executor-after-panic".into()).unwrap();
    let claim = tokio::time::timeout(
        std::time::Duration::from_secs(2),
        execution.claim("executor-after-panic", CancellationToken::new()),
    )
    .await
    .unwrap()
    .unwrap()
    .unwrap();
    let PublishAttempt::Published(facts) = execution
        .publish(
            &claim,
            vec![SessionFactBody::TurnTerminal {
                turn_id: claim.turn_id().clone(),
                outcome: TurnOutcome::Completed,
            }],
        )
        .await
        .unwrap()
    else {
        panic!("terminal publication must fit");
    };
    execution.flush(&claim, facts[0].seq()).await.unwrap();
    drop(stack.composition.pin.lock().unwrap().take());
    tokio::time::timeout(std::time::Duration::from_secs(1), async {
        while stack.composition.owner_drops.load(Ordering::Acquire) != 1 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("a stopped pool must release tracking pins before Fiber disposal");
    assert!(!executor.dispose().await.is_clean());
    drop(lease);
    drop((turns, execution));
    assert!(language.dispose().await.is_clean());
    stack.dispose_services().await;
}
