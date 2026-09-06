use super::*;

async fn mutation_fixture(
    direct: bool,
) -> (
    SessionKernel,
    rsi_agent_kernel::KernelWorkers,
    Arc<FactReadRaceStore>,
    Arc<MemoryStore>,
    rsi_agent_turn_protocol::ExecutorLease,
    rsi_agent_turn_protocol::TurnClaim,
    SessionId,
) {
    let memory = Arc::new(MemoryStore::new());
    let observed = Arc::new(FactReadRaceStore::new(memory.clone()));
    let kernel =
        SessionKernel::recover_with_clock(observed.clone(), composition(), Arc::new(FixedClock))
            .await
            .unwrap();
    let workers = kernel.start_workers();
    let root = SessionId::new("mutation-root").unwrap();
    if direct {
        submit(&kernel, root.as_str(), "direct source without activation").await;
    } else {
        kernel
            .submit_message(SubmitMessage {
                session: fresh(header(root.as_str())),
                message: mailbox_message("mutation-root-message"),
                delivery: rsi_agent_session_protocol::MessageDelivery::NextTurn,
            })
            .await
            .unwrap();
    }
    let lease = kernel.register("mutation-executor".into()).unwrap();
    let claim = kernel
        .claim("mutation-executor", CancellationToken::new())
        .await
        .unwrap()
        .unwrap();
    let child = SessionId::new("mutation-child").unwrap();
    kernel
        .spawn_agent(SpawnAgentRequest {
            cancellation: CancellationToken::new(),
            caller: kernel.agent_caller(&claim).unwrap(),
            child_session_id: child.clone(),
            task_name: "child".into(),
            message_id: MessageId::new("mutation-child-message").unwrap(),
            message: "child".into(),
            fork_turns: ForkTurnSelection::None,
        })
        .await
        .unwrap();
    (kernel, workers, observed, memory, lease, claim, child)
}

#[tokio::test]
async fn source_release_and_same_id_registration_wait_for_cancelled_commit_waiter() {
    for (withdraw, direct) in [(false, false), (true, false), (false, true), (true, true)] {
        let (kernel, workers, observed, memory, lease, claim, child) =
            mutation_fixture(direct).await;
        assert_eq!(
            memory
                .active_activation(claim.session_id())
                .await
                .unwrap()
                .is_none(),
            direct
        );
        let mut lease = Some(lease);
        let _child_lease = kernel.register("mutation-child-executor".into()).unwrap();
        // A direct parent has no activation to reserve child-completion capacity.
        // Its child remains pending; the authority fence must still cover the send.
        let _child_claim = if direct {
            None
        } else {
            Some(
                tokio::time::timeout(
                    std::time::Duration::from_secs(2),
                    kernel.claim("mutation-child-executor", CancellationToken::new()),
                )
                .await
                .unwrap()
                .unwrap()
                .unwrap(),
            )
        };
        let caller = kernel.agent_caller(&claim).unwrap();
        let before_controls = memory
            .read_controls(&child, 0, 8)
            .await
            .unwrap()
            .records
            .len();
        observed.pause_next_agent_commit_before_apply();
        let send = tokio::spawn({
            let kernel = kernel.clone();
            let child = child.clone();
            async move {
                kernel
                    .send_agent_message(SendAgentMessage {
                        cancellation: CancellationToken::new(),
                        caller,
                        target_session_id: child,
                        message_id: MessageId::new("retained-message").unwrap(),
                        message: "keep this admitted mutation".into(),
                        start_new_turn: false,
                    })
                    .await
            }
        });
        observed.wait_until_agent_commit_is_before_apply().await;
        if withdraw {
            drop(lease.take());
            lease = Some(kernel.register("mutation-executor".into()).unwrap());
        } else {
            kernel.release(&claim).unwrap();
        }
        assert!(matches!(
            kernel.agent_caller(&claim),
            Err(TurnError::StaleClaim)
        ));
        send.abort();
        assert!(send.await.unwrap_err().is_cancelled());
        let next = kernel.claim("mutation-executor", CancellationToken::new());
        tokio::pin!(next);
        assert!(
            futures_util::poll!(&mut next).is_pending(),
            "source was reclaimed before admitted target commit returned"
        );
        assert_eq!(
            memory
                .read_controls(&child, 0, 8)
                .await
                .unwrap()
                .records
                .len(),
            before_controls
        );
        observed.release_agent_commit_before_apply();
        let replacement = tokio::time::timeout(std::time::Duration::from_secs(2), next)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert_eq!(replacement.session_id(), claim.session_id());
        assert_ne!(replacement.claim_id(), claim.claim_id());
        assert!(
            kernel
                .message_status(&child, &MessageId::new("retained-message").unwrap())
                .await
                .is_ok()
        );
        drop(lease);
        kernel.shutdown(workers).await.unwrap();
    }
}

#[tokio::test]
async fn tree_control_requires_validation_even_when_metadata_is_readable() {
    let memory = Arc::new(MemoryStore::new());
    let id = SessionId::new("cold-tree-validation").unwrap();
    append_terminal_history(&memory, id.as_str(), 1).await;
    let observed = Arc::new(FactReadRaceStore::new(memory));
    let kernel =
        SessionKernel::recover_with_clock(observed.clone(), composition(), Arc::new(FixedClock))
            .await
            .unwrap();
    *observed.failed_validation.lock().unwrap() = Some(id.clone());
    assert_eq!(kernel.session_header(&id).await.unwrap().session_id(), &id);
    assert!(matches!(
        kernel.tree_sessions(&id).await,
        Err(TurnError::Store(_))
    ));
    assert!(matches!(
        kernel.prepare_resume(&id).await,
        Err(TurnError::Store(_))
    ));
}

#[derive(Debug, Default)]
struct PublicationClock {
    pause: Mutex<
        Option<(
            tokio::sync::oneshot::Sender<()>,
            std::sync::mpsc::Receiver<()>,
        )>,
    >,
}

impl Clock for PublicationClock {
    fn now_ms(&self) -> u64 {
        let pause = self.pause.lock().unwrap().take();
        if let Some((entered, release)) = pause {
            entered.send(()).unwrap();
            release
                .recv_timeout(std::time::Duration::from_secs(2))
                .expect("payload staging blocked unrelated Kernel state access");
        }
        42
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn publication_staging_releases_global_state_but_keeps_session_admission() {
    let clock = Arc::new(PublicationClock::default());
    let kernel = SessionKernel::recover_with_clock(
        Arc::new(MemoryStore::new()),
        composition(),
        clock.clone(),
    )
    .await
    .unwrap();
    let workers = kernel.start_workers();
    submit(&kernel, "publication-a", "large producer").await;
    submit(&kernel, "publication-b", "independent claim").await;
    let _lease = kernel.register("publication-executor".into()).unwrap();
    let first = kernel
        .claim("publication-executor", CancellationToken::new())
        .await
        .unwrap()
        .unwrap();
    let other = kernel
        .claim("publication-executor", CancellationToken::new())
        .await
        .unwrap()
        .unwrap();
    let effect = EffectId::new("publication-effect").unwrap();
    for body in [
        SessionFactBody::ModelIntent {
            turn_id: first.turn_id().clone(),
            effect_id: effect.clone(),
            snapshot: snapshot(),
        },
        SessionFactBody::ModelStarted {
            turn_id: first.turn_id().clone(),
            effect_id: effect.clone(),
        },
    ] {
        let facts = kernel
            .publish(&first, vec![body])
            .await
            .unwrap()
            .published();
        kernel
            .flush(&first, facts.last().unwrap().seq())
            .await
            .unwrap();
    }
    let event = || SessionFactBody::ModelEvent {
        turn_id: first.turn_id().clone(),
        effect_id: effect.clone(),
        event: LanguageEvent::ContentDelta {
            index: 0,
            delta: ContentDelta::Text("x".repeat(1024 * 1024)),
        },
    };
    let body = event();
    let (entered, captured) = tokio::sync::oneshot::channel();
    let (release, barrier) = std::sync::mpsc::channel();
    *clock.pause.lock().unwrap() = Some((entered, barrier));
    let publication = tokio::spawn({
        let kernel = kernel.clone();
        let claim = first.clone();
        async move { kernel.publish(&claim, vec![body]).await }
    });
    captured.await.unwrap();
    let same_session = kernel.publish(&first, vec![event()]);
    tokio::pin!(same_session);
    assert!(futures_util::poll!(&mut same_session).is_pending());
    kernel.release(&other).unwrap();
    let reclaimed = kernel
        .claim("publication-executor", CancellationToken::new())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(reclaimed.session_id(), other.session_id());
    release.send(()).unwrap();
    let first_facts = publication.await.unwrap().unwrap().published();
    let next_facts = same_session.await.unwrap().published();
    assert_eq!(next_facts[0].seq(), first_facts[0].seq() + 1);
    kernel.shutdown(workers).await.unwrap();
}

#[tokio::test]
async fn activation_terminal_cannot_bypass_its_atomic_settlement() {
    let (kernel, workers, _observed, memory, _lease, claim, child) = mutation_fixture(false).await;
    kernel
        .close_current_step(&claim, &TurnOutcome::Completed)
        .await
        .unwrap();
    let attempted = kernel
        .publish(
            &claim,
            vec![SessionFactBody::TurnTerminal {
                turn_id: claim.turn_id().clone(),
                outcome: TurnOutcome::Completed,
            }],
        )
        .await;
    assert!(
        matches!(attempted, Err(TurnError::Invalid(_))),
        "an activation terminal must include its durable control transition"
    );
    kernel
        .finish_activation_turn(&claim, &TurnOutcome::Completed)
        .await
        .unwrap()
        .unwrap();
    let _child_lease = kernel
        .register("settle-after-rejected-terminal".into())
        .unwrap();
    let child_claim = kernel
        .claim("settle-after-rejected-terminal", CancellationToken::new())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(child_claim.session_id(), &child);
    kernel
        .finish_activation_turn(&child_claim, &TurnOutcome::Completed)
        .await
        .unwrap()
        .unwrap();
    wait_for_settlement(memory.as_ref(), claim.session_id()).await;
    assert!(memory.active_activation(&child).await.unwrap().is_none());
    kernel.shutdown(workers).await.unwrap();
}

#[tokio::test]
async fn rejected_direct_terminal_keeps_agent_mutations_available() {
    let (kernel, workers, _observed, _memory, _lease, claim, child) = mutation_fixture(true).await;
    let result = kernel
        .publish(
            &claim,
            vec![SessionFactBody::TurnTerminal {
                turn_id: TurnId::new("wrong-terminal-turn").unwrap(),
                outcome: TurnOutcome::Completed,
            }],
        )
        .await;
    assert!(matches!(result, Err(TurnError::Invalid(_))));
    kernel
        .send_agent_message(SendAgentMessage {
            cancellation: CancellationToken::new(),
            caller: kernel.agent_caller(&claim).unwrap(),
            target_session_id: child,
            message_id: MessageId::new("after-rejected-terminal").unwrap(),
            message: "the original claim remains live".into(),
            start_new_turn: false,
        })
        .await
        .expect("rejected terminal permanently closed the source mutation gate");
    kernel.shutdown(workers).await.unwrap();
}

#[tokio::test]
async fn admitted_terminal_failure_keeps_mutations_closed_but_allows_settlement_retry() {
    let (kernel, workers, _observed, memory, _lease, claim, child) = mutation_fixture(false).await;
    kernel
        .close_current_step(&claim, &TurnOutcome::Completed)
        .await
        .unwrap();
    let page = kernel.read_facts(&claim, 0, 256).await.unwrap();
    kernel.flush(&claim, page.through_seq).await.unwrap();
    memory.fail_next_appends(1);
    assert!(matches!(
        kernel
            .finish_activation_turn(&claim, &TurnOutcome::Completed)
            .await,
        Err(TurnError::Store(_))
    ));
    let result = kernel
        .send_agent_message(SendAgentMessage {
            cancellation: CancellationToken::new(),
            caller: kernel.agent_caller(&claim).unwrap(),
            target_session_id: child,
            message_id: MessageId::new("after-admitted-terminal").unwrap(),
            message: "must remain fenced".into(),
            start_new_turn: false,
        })
        .await;
    assert!(matches!(result, Err(TurnError::StaleClaim)));
    assert!(
        kernel
            .finish_activation_turn(&claim, &TurnOutcome::Completed)
            .await
            .unwrap()
            .is_some()
    );
    kernel.shutdown(workers).await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn cancelled_terminal_drain_reopens_only_after_the_last_mutation_finishes() {
    let (kernel, workers, observed, _memory, _lease, claim, child) = mutation_fixture(false).await;
    observed.pause_next_agent_commit_before_apply();
    let send = tokio::spawn({
        let kernel = kernel.clone();
        let caller = kernel.agent_caller(&claim).unwrap();
        let child = child.clone();
        async move {
            kernel
                .send_agent_message(SendAgentMessage {
                    cancellation: CancellationToken::new(),
                    caller,
                    target_session_id: child,
                    message_id: MessageId::new("draining-message").unwrap(),
                    message: "retained".into(),
                    start_new_turn: false,
                })
                .await
        }
    });
    observed.wait_until_agent_commit_is_before_apply().await;
    let mut terminal = Box::pin(kernel.finish_activation_turn(&claim, &TurnOutcome::Completed));
    assert!(futures_util::poll!(&mut terminal).is_pending());
    drop(terminal);
    observed.release_agent_commit_before_apply();
    send.await.unwrap().unwrap();
    kernel
        .send_agent_message(SendAgentMessage {
            cancellation: CancellationToken::new(),
            caller: kernel.agent_caller(&claim).unwrap(),
            target_session_id: child,
            message_id: MessageId::new("after-abandoned-terminal").unwrap(),
            message: "still live".into(),
            start_new_turn: false,
        })
        .await
        .expect("abandoned terminal drain did not restore its live claim");
    kernel
        .finish_activation_turn(&claim, &TurnOutcome::Completed)
        .await
        .unwrap();
    kernel.shutdown(workers).await.unwrap();
}

#[tokio::test]
async fn cancelled_spawn_commit_is_recoverable_by_exact_retry() {
    let (kernel, workers, observed, memory, _lease, claim, _) = mutation_fixture(false).await;
    let mut request = SpawnAgentRequest {
        cancellation: CancellationToken::new(),
        caller: kernel.agent_caller(&claim).unwrap(),
        child_session_id: SessionId::new("spawn-retry-child").unwrap(),
        task_name: "retry-child".into(),
        message_id: MessageId::new("spawn-retry-message").unwrap(),
        message: "exact initial message".into(),
        fork_turns: ForkTurnSelection::None,
    };
    observed.pause_next_agent_commit_after_apply();
    let spawn = tokio::spawn({
        let kernel = kernel.clone();
        let request = request.clone();
        async move { kernel.spawn_agent(request).await }
    });
    observed.wait_until_agent_commit_is_applied().await;
    request.cancellation.cancel();
    assert_eq!(spawn.await.unwrap(), Err(TurnError::Cancelled));
    request.cancellation = CancellationToken::new();
    let retry = kernel.spawn_agent(request.clone());
    tokio::pin!(retry);
    assert!(futures_util::poll!(&mut retry).is_pending());
    observed.release_applied_agent_commit();
    let recovered = retry.await.unwrap();
    assert_eq!(recovered.session_id, request.child_session_id);
    assert_eq!(recovered.message.message_id, request.message_id);
    assert_eq!(recovered.message.accepted_control_seq, 1);
    assert_eq!(
        kernel.spawn_agent(request.clone()).await.unwrap(),
        recovered
    );
    request.message.push_str(" changed");
    assert!(matches!(
        kernel.spawn_agent(request.clone()).await,
        Err(TurnError::MessageConflict { .. })
    ));
    request.message = "exact initial message".into();
    request.fork_turns = ForkTurnSelection::All;
    assert!(matches!(
        kernel.spawn_agent(request.clone()).await,
        Err(TurnError::Invalid(_))
    ));
    assert_eq!(
        memory
            .read_controls(&request.child_session_id, 0, 8)
            .await
            .unwrap()
            .records
            .len(),
        1
    );
    kernel.shutdown(workers).await.unwrap();
}

#[tokio::test]
async fn terminal_drains_admitted_source_mutation_after_store_commit() {
    let (kernel, workers, observed, memory, _lease, claim, child) = mutation_fixture(false).await;
    let caller = kernel.agent_caller(&claim).unwrap();
    observed.pause_next_agent_commit_after_apply();
    let send = tokio::spawn({
        let kernel = kernel.clone();
        async move {
            kernel
                .send_agent_message(SendAgentMessage {
                    cancellation: CancellationToken::new(),
                    caller,
                    target_session_id: child,
                    message_id: MessageId::new("terminal-fenced-message").unwrap(),
                    message: "finish receipt first".into(),
                    start_new_turn: false,
                })
                .await
        }
    });
    observed.wait_until_agent_commit_is_applied().await;
    let terminal = kernel.finish_activation_turn(&claim, &TurnOutcome::Completed);
    tokio::pin!(terminal);
    assert!(futures_util::poll!(&mut terminal).is_pending());
    let facts = memory.read_facts(claim.session_id(), 0, 32).await.unwrap();
    assert!(!facts.facts.iter().any(|fact| matches!(
        fact.body(),
        SessionFactBody::StepEnded { .. } | SessionFactBody::TurnTerminal { .. }
    )));
    send.abort();
    assert!(send.await.unwrap_err().is_cancelled());
    assert!(futures_util::poll!(&mut terminal).is_pending());
    observed.release_applied_agent_commit();
    terminal.await.unwrap().unwrap();
    kernel.shutdown(workers).await.unwrap();
}

#[tokio::test]
async fn cancelled_source_preparation_cannot_commit_later() {
    let (kernel, workers, observed, memory, _lease, claim, child) = mutation_fixture(false).await;
    observed.block_header_reads();
    let cancellation = CancellationToken::new();
    let send = tokio::spawn({
        let kernel = kernel.clone();
        let child = child.clone();
        let cancellation = cancellation.clone();
        let caller = kernel.agent_caller(&claim).unwrap();
        async move {
            kernel
                .send_agent_message(SendAgentMessage {
                    cancellation,
                    caller,
                    target_session_id: child,
                    message_id: MessageId::new("cancelled-before-admission").unwrap(),
                    message: "must not appear".into(),
                    start_new_turn: false,
                })
                .await
        }
    });
    tokio::task::yield_now().await;
    cancellation.cancel();
    assert_eq!(send.await.unwrap(), Err(TurnError::Cancelled));
    observed.release_blocked_header_reads();
    assert_eq!(
        memory
            .read_controls(&child, 0, 8)
            .await
            .unwrap()
            .records
            .len(),
        1
    );
    kernel.shutdown(workers).await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn shutdown_timeout_does_not_abort_an_admitted_target_commit() {
    let (kernel, workers, observed, memory, _lease, claim, child) = mutation_fixture(false).await;
    observed.pause_next_agent_commit_before_apply();
    let send = tokio::spawn({
        let kernel = kernel.clone();
        let child = child.clone();
        let caller = kernel.agent_caller(&claim).unwrap();
        async move {
            kernel
                .send_agent_message(SendAgentMessage {
                    cancellation: CancellationToken::new(),
                    caller,
                    target_session_id: child,
                    message_id: MessageId::new("shutdown-retained-message").unwrap(),
                    message: "commit through shutdown".into(),
                    start_new_turn: false,
                })
                .await
        }
    });
    observed.wait_until_agent_commit_is_before_apply().await;
    assert!(kernel.shutdown(workers).await.is_err());
    assert!(!send.is_finished());
    observed.release_agent_commit_before_apply();
    assert!(send.await.unwrap().is_ok());
    assert_eq!(
        memory
            .read_controls(&child, 0, 8)
            .await
            .unwrap()
            .records
            .len(),
        2
    );
}

#[derive(Debug)]
struct GatedPreparation {
    all: bool,
    entered: AtomicUsize,
    active: AtomicUsize,
    peak: AtomicUsize,
    release: tokio::sync::Semaphore,
}

#[async_trait]
impl WorkspaceContext for GatedPreparation {
    async fn snapshot(
        &self,
        header: &SessionHeader,
        _messages: &[&AgentMessage],
    ) -> std::result::Result<WorkspaceContextSnapshot, WorkspaceContextError> {
        let active = self.active.fetch_add(1, Ordering::AcqRel) + 1;
        self.peak.fetch_max(active, Ordering::AcqRel);
        self.entered.fetch_add(1, Ordering::AcqRel);
        if self.all || header.session_id().as_str() == "ready-000" {
            self.release.acquire().await.unwrap().forget();
        }
        self.active.fetch_sub(1, Ordering::AcqRel);
        Ok(WorkspaceContextSnapshot {
            complete: true,
            instructions_sha256: "a".repeat(64),
            instructions: None,
            skill_catalog_sha256: "b".repeat(64),
            skill_catalog: None,
            invocations: Vec::new(),
        })
    }
}

#[tokio::test]
async fn slow_ready_root_does_not_hold_other_roots_and_preparation_is_bounded() {
    for all in [false, true] {
        let memory = Arc::new(MemoryStore::new());
        let context = Arc::new(GatedPreparation {
            all,
            entered: AtomicUsize::new(0),
            active: AtomicUsize::new(0),
            peak: AtomicUsize::new(0),
            release: tokio::sync::Semaphore::new(0),
        });
        let kernel = SessionKernel::recover_with_context_clock_and_limits(
            memory.clone(),
            composition(),
            context.clone(),
            Arc::new(FixedClock),
            KernelLimits::default(),
        )
        .await
        .unwrap();
        let workers = kernel.start_workers();
        for index in 0..8 {
            kernel
                .submit_message(SubmitMessage {
                    session: fresh(header(&format!("ready-{index:03}"))),
                    message: mailbox_message(&format!("ready-message-{index:03}")),
                    delivery: rsi_agent_session_protocol::MessageDelivery::NextTurn,
                })
                .await
                .unwrap();
        }
        let _lease = kernel.register("ready-executor".into()).unwrap();
        let cancellation = CancellationToken::new();
        let claim = tokio::spawn({
            let kernel = kernel.clone();
            let cancellation = cancellation.clone();
            async move { kernel.claim("ready-executor", cancellation).await }
        });
        if all {
            tokio::time::timeout(std::time::Duration::from_secs(2), async {
                while context.active.load(Ordering::Acquire) < 4 {
                    tokio::task::yield_now().await;
                }
            })
            .await
            .unwrap();
            assert_eq!(context.entered.load(Ordering::Acquire), 4);
            assert_eq!(context.peak.load(Ordering::Acquire), 4);
            cancellation.cancel();
            assert!(claim.await.unwrap().unwrap().is_none());
        } else {
            let started = tokio::time::Instant::now();
            let claim = tokio::time::timeout(std::time::Duration::from_secs(2), claim)
                .await
                .unwrap()
                .unwrap()
                .unwrap()
                .unwrap();
            assert_ne!(claim.session_id().as_str(), "ready-000");
            println!(
                "slow_root_interference independent_claim_us={}",
                started.elapsed().as_micros()
            );
            assert!(context.peak.load(Ordering::Acquire) <= 4);
        }
        context.release.add_permits(8);
        kernel.shutdown(workers).await.unwrap();
    }
}

#[tokio::test(start_paused = true)]
async fn new_ready_root_is_discovered_while_the_previous_page_is_still_preparing() {
    let context = Arc::new(GatedPreparation {
        all: false,
        entered: AtomicUsize::new(0),
        active: AtomicUsize::new(0),
        peak: AtomicUsize::new(0),
        release: tokio::sync::Semaphore::new(0),
    });
    let kernel = SessionKernel::recover_with_context_clock_and_limits(
        Arc::new(MemoryStore::new()),
        composition(),
        context.clone(),
        Arc::new(FixedClock),
        KernelLimits::default(),
    )
    .await
    .unwrap();
    let workers = kernel.start_workers();
    kernel
        .submit_message(SubmitMessage {
            session: fresh(header("ready-000")),
            message: mailbox_message("first-ready"),
            delivery: rsi_agent_session_protocol::MessageDelivery::NextTurn,
        })
        .await
        .unwrap();
    let _lease = kernel.register("new-ready".into()).unwrap();
    let claim = tokio::spawn({
        let kernel = kernel.clone();
        async move { kernel.claim("new-ready", CancellationToken::new()).await }
    });
    while context.entered.load(Ordering::Acquire) == 0 {
        tokio::task::yield_now().await;
    }
    kernel
        .submit_message(SubmitMessage {
            session: fresh(header("ready-new")),
            message: mailbox_message("later-ready"),
            delivery: rsi_agent_session_protocol::MessageDelivery::NextTurn,
        })
        .await
        .unwrap();
    let result = tokio::time::timeout(std::time::Duration::from_secs(1), claim)
        .await
        .expect("new root stalled behind an in-flight preparation from the final ready page")
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(result.session_id().as_str(), "ready-new");
    context.release.add_permits(1);
    kernel.shutdown(workers).await.unwrap();
}

#[tokio::test]
async fn agent_interrupt_is_visible_only_after_its_owned_commit_returns() {
    let (kernel, workers, observed, memory, _lease, claim, child) = mutation_fixture(false).await;
    let _child_lease = kernel.register("interrupt-child-executor".into()).unwrap();
    let child_claim = kernel
        .claim("interrupt-child-executor", CancellationToken::new())
        .await
        .unwrap()
        .unwrap();
    let token = kernel.cancellation(&child_claim).unwrap();
    let before = memory.read_facts(&child, 0, 32).await.unwrap().durable_seq;
    let mut observation = kernel.observe(&child, before).await.unwrap();
    observed.pause_next_agent_commit_before_apply();
    let interrupt = tokio::spawn({
        let kernel = kernel.clone();
        let child = child.clone();
        let caller = kernel.agent_caller(&claim).unwrap();
        async move {
            kernel
                .interrupt_agent(&caller, &child, CancellationToken::new())
                .await
        }
    });
    observed.wait_until_agent_commit_is_before_apply().await;
    assert!(!token.is_cancelled());
    assert_eq!(
        memory
            .read_facts(&child, before, 1)
            .await
            .unwrap()
            .durable_seq,
        before
    );
    assert!(futures_util::poll!(observation.next()).is_pending());
    interrupt.abort();
    assert!(interrupt.await.unwrap_err().is_cancelled());
    observed.release_agent_commit_before_apply();
    tokio::time::timeout(std::time::Duration::from_secs(2), token.cancelled())
        .await
        .unwrap();
    let update = observation.next().await.unwrap().unwrap();
    assert!(
        matches!(update, TurnUpdate::Fact { fact, durable_seq } if durable_seq >= fact.seq() && matches!(fact.body(), SessionFactBody::CancelRequested { .. }))
    );
    drop(observation);
    kernel.shutdown(workers).await.unwrap();
}
