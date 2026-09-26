use super::*;
use rsi_agent_session_protocol::{ExecutionOwner, ProgramOutcome, ProgramRunEvent};
use rsi_agent_turn_protocol::{PrepareProgram, ProgramAgentRequest, ProgramRun};
struct Fixture {
    store: Arc<MemoryStore>,
    faults: Arc<FactReadRaceStore>,
    policy: rsi_agent_composition_protocol::DomainHandle<bool>,
    kernel: AgentKernel,
    composition: Arc<ProgramComposition>,
    workers: rsi_agent_kernel::KernelWorkers,
    root: TurnClaim,
    run: Arc<dyn ProgramRun>,
    _executor: rsi_agent_turn_protocol::ExecutorLease,
}
#[allow(clippy::too_many_lines)] // The fixture issues real model-origin authority and durably accepts its run.
async fn fixture() -> Fixture {
    fixture_with_accept_failure(false).await
}
#[allow(clippy::too_many_lines)] // The fixture issues real model authority and can lose acceptance acknowledgement.
async fn fixture_with_accept_failure(lose_ack: bool) -> Fixture {
    let store = Arc::new(MemoryStore::new());
    let faults = Arc::new(FactReadRaceStore::new(store.clone()));
    let definition = rsi_agent_composition_protocol::DomainDefinition::new(
        rsi_agent_session_protocol::DomainIdentity::new("fixture.program-policy", 1).unwrap(),
        &false,
        |_| Ok(()),
    )
    .unwrap();
    let catalog =
        rsi_agent_composition_protocol::DomainCatalog::new([definition.registration()]).unwrap();
    let policy = catalog.bind(&definition).unwrap();
    let composition = Arc::new(ProgramComposition(
        AgentCompositionPin::new(
            AgentPresetId::new("test-agent").unwrap(),
            "a".repeat(64),
            Arc::new(ProgramTools),
            Arc::new(rsi_agent_context::DefaultContextBuilder::default()),
            catalog,
            rsi_agent_composition_protocol::ContributionCatalog::default(),
            Arc::new(()),
        )
        .unwrap(),
    ));
    let kernel =
        AgentKernel::recover_with_clock(faults.clone(), composition.clone(), Arc::new(FixedClock))
            .await
            .unwrap();
    let workers = kernel.start_workers();
    kernel
        .submit_message(SubmitMessage {
            session: SubmitSession::Fresh(
                PreparedFreshSession::new(header("workflow-root"), composition.0.clone()).unwrap(),
            ),
            message: mailbox_message("workflow-input"),
            delivery: rsi_agent_session_protocol::MessageDelivery::NextTurn,
        })
        .await
        .unwrap();
    let executor = kernel.register("workflow-worker".into()).unwrap();
    let root = kernel
        .claim("workflow-worker", CancellationToken::new())
        .await
        .unwrap()
        .unwrap();
    kernel.composition(&root).unwrap();
    publish_model_source(&kernel, &root, "workflow", "fixture_workflow", &snapshot()).await;
    flush_bodies(
        &kernel,
        &root,
        vec![intent(
            root.turn_id(),
            "workflow",
            ToolOrigin::Model {
                effect_id: EffectId::new("source-model").unwrap(),
            },
            "fixture_workflow",
            ToolProgramRole::Workflow,
        )],
    )
    .await;
    flush_bodies(&kernel, &root, vec![started(root.turn_id(), "workflow")]).await;
    let caller = kernel
        .tool_caller(&root, &EffectId::new("workflow").unwrap())
        .unwrap();
    let domain = kernel
        .domain_states(root.session_id())
        .await
        .unwrap()
        .remove(0);
    let guard = rsi_agent_session_protocol::ProgramDomainGuard {
        domain: domain.snapshot.identity().clone(),
        revision: domain.revision,
        snapshot_sha256: domain.snapshot.sha256().unwrap(),
    };
    let before_cas = faults.cas_writes.load(Ordering::SeqCst);
    let run = kernel
        .prepare_program(PrepareProgram {
            caller: caller.clone(),
            cancellation: CancellationToken::new(),
            script: "return 42".into(),
            fork_turns: ForkTurnSelection::All,
            guard: Some(guard),
            continuation_domains: vec![],
        })
        .await
        .unwrap();
    assert!(
        store
            .read_program_records(root.session_id(), &run.descriptor().run_id)
            .await
            .unwrap()
            .is_none()
    );
    assert_eq!(
        faults.cas_writes.load(Ordering::SeqCst),
        before_cas,
        "preparation must not publish CAS"
    );
    assert!(run.start().await.is_err());
    faults
        .fail_program_after_apply
        .store(lose_ack, Ordering::Release);
    run.accept(&caller).await.unwrap();
    run.start().await.unwrap();
    let admitted_cas = faults.cas_writes.load(Ordering::SeqCst);
    assert!(run.accept(&caller).await.is_err());
    assert_eq!(
        faults.cas_writes.load(Ordering::SeqCst),
        admitted_cas,
        "duplicate admission must not write CAS"
    );
    Fixture {
        store,
        faults,
        policy,
        kernel,
        composition,
        workers,
        root,
        run,
        _executor: executor,
    }
}
async fn end_creator(f: &Fixture) {
    flush_bodies(
        &f.kernel,
        &f.root,
        vec![result(f.root.turn_id(), "workflow", "detached")],
    )
    .await;
    f.kernel
        .finish_turn(&f.root, &TurnOutcome::Completed)
        .await
        .unwrap();
    assert!(
        f.store
            .active_activation(f.root.session_id())
            .await
            .unwrap()
            .is_none()
    );
}
async fn child_claim(f: &Fixture) -> TurnClaim {
    tokio::time::timeout(
        std::time::Duration::from_secs(5),
        f.kernel.claim("workflow-worker", CancellationToken::new()),
    )
    .await
    .expect("child admission must progress")
    .unwrap()
    .unwrap()
}
#[tokio::test]
#[allow(clippy::too_many_lines)] // One lifecycle exhausts the real child budget, then validates the exclusive sink and successor denial.
async fn workflow_detaches_and_routes_128_initial_completions_without_parent_mailbox_growth() {
    let f = fixture().await;
    f.run.detach().await.unwrap();
    assert!(!f.run.cancel_from_creator().await.unwrap());
    for ordinal in 1..=128 {
        let owner = f.run.clone();
        let wait = tokio::spawn(async move {
            owner
                .agent(ProgramAgentRequest {
                    message: "finish".into(),
                    output_contract: None,
                    role: None,
                })
                .await
        });
        let claim = child_claim(&f).await;
        if ordinal == 1 {
            end_creator(&f).await;
        }
        assert_eq!(
            claim.session_id(),
            &f.run.descriptor().child_session_id(ordinal).unwrap()
        );
        assert!(
            matches!(claim.header().execution_owner(),Some(ExecutionOwner::ProgramRun{ordinal:actual,..}) if *actual==ordinal)
        );
        assert_eq!(
            claim.header().fork_origin().unwrap().invoking_turn_id,
            *f.root.turn_id()
        );
        assert_eq!(
            f.kernel.composition(&claim).unwrap().source_digest(),
            f.composition.0.source_digest()
        );
        let active = f
            .store
            .active_activation(claim.session_id())
            .await
            .unwrap()
            .unwrap();
        assert!(active.completion_to_program);
        assert!(active.completion_reserved_bytes.is_none());
        f.kernel
            .finish_turn(&claim, &TurnOutcome::Completed)
            .await
            .unwrap();
        let completed = tokio::time::timeout(std::time::Duration::from_secs(5), wait)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert_eq!(completed.receipt.ordinal, ordinal);
        assert_eq!(completed.receipt.outcome, ProgramOutcome::Completed);
        assert_eq!(
            f.store
                .read_agent_mailbox_summary(f.root.session_id())
                .await
                .unwrap()
                .pending_count,
            0
        );
        assert_eq!(
            f.store
                .completion_reservation_count(f.root.session_id())
                .await
                .unwrap(),
            1,
            "one run notice reservation, independent of the 128 child completions"
        );
    }
    assert!(
        f.run
            .agent(ProgramAgentRequest {
                message: "over the admission allowance".into(),
                output_contract: None,
                role: None
            })
            .await
            .is_err()
    );
    f.run
        .finish(
            ProgramOutcome::Completed,
            Some(serde_json::json!({"children":128})),
        )
        .await
        .unwrap();
    let records = f
        .store
        .read_program_records(f.root.session_id(), &f.run.descriptor().run_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        records
            .records
            .iter()
            .filter(|record| matches!(
                record.body(),
                AgentControlRecordBody::ProgramRun {
                    event: ProgramRunEvent::ChildSettled { .. },
                    ..
                }
            ))
            .count(),
        128
    );
    assert!(records.head.terminal);
    assert_eq!(
        f.store
            .read_agent_mailbox_summary(f.root.session_id())
            .await
            .unwrap()
            .pending_count,
        1
    );
    assert_eq!(
        f.store
            .list_program_notices(None, 256)
            .await
            .unwrap()
            .notices
            .len(),
        1
    );
    let notice = child_claim(&f).await;
    f.kernel.composition(&notice).unwrap();
    assert_eq!(notice.session_id(), f.root.session_id());
    let entered = f
        .kernel
        .read_facts(&notice, notice.accepted_seq() - 1, 3)
        .await
        .unwrap();
    assert!(entered.facts.iter().any(|fact| matches!(
        fact.body(),
        SessionFactBody::InputMessageEntered {
            source: rsi_agent_session_protocol::InputMessageSource::Program { .. },
            ..
        }
    )));
    publish_model_source(
        &f.kernel,
        &notice,
        "successor",
        "fixture_workflow",
        &snapshot(),
    )
    .await;
    flush_bodies(
        &f.kernel,
        &notice,
        vec![intent(
            notice.turn_id(),
            "successor",
            ToolOrigin::Model {
                effect_id: EffectId::new("source-model").unwrap(),
            },
            "fixture_workflow",
            ToolProgramRole::Workflow,
        )],
    )
    .await;
    flush_bodies(
        &f.kernel,
        &notice,
        vec![started(notice.turn_id(), "successor")],
    )
    .await;
    let caller = f
        .kernel
        .tool_caller(&notice, &EffectId::new("successor").unwrap())
        .unwrap();
    assert!(
        f.kernel
            .prepare_program(PrepareProgram {
                caller: caller.clone(),
                cancellation: CancellationToken::new(),
                script: "return 1".into(),
                fork_turns: ForkTurnSelection::None,
                guard: None,
                continuation_domains: vec![]
            })
            .await
            .is_err()
    );
    let observed = f
        .kernel
        .read_program(&caller, &f.run.descriptor().run_id)
        .await
        .unwrap();
    assert_eq!(observed.children, 128);
    assert_eq!(observed.result, Some(serde_json::json!({"children":128})));
    flush_bodies(
        &f.kernel,
        &notice,
        vec![result(notice.turn_id(), "successor", "denied")],
    )
    .await;
    f.kernel
        .finish_turn(&notice, &TurnOutcome::Completed)
        .await
        .unwrap();
    f.kernel.shutdown(f.workers).await.unwrap();
}
#[tokio::test]
async fn workflow_cancellation_wins_before_detach_and_recovery_discards_unclaimed_children() {
    let f = fixture().await;
    let owner = f.run.clone();
    let waiting = tokio::spawn(async move {
        owner
            .agent(ProgramAgentRequest {
                message: "never execute after restart".into(),
                output_contract: None,
                role: None,
            })
            .await
    });
    // Observe the actual durable admission, without claiming a provider Turn.
    let mut events = f
        .kernel
        .observe_session(
            f.root.session_id(),
            ObservationCursor {
                fact_seq: 0,
                control_seq: 0,
            },
        )
        .await
        .unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            let page = f
                .store
                .read_program_records(f.root.session_id(), &f.run.descriptor().run_id)
                .await
                .unwrap()
                .unwrap();
            if page.records.iter().any(|record| {
                matches!(
                    record.body(),
                    AgentControlRecordBody::ProgramRun {
                        event: ProgramRunEvent::ChildAdmitted { .. },
                        ..
                    }
                )
            }) {
                break;
            }
            events.next().await.unwrap().unwrap();
        }
    })
    .await
    .unwrap();
    waiting.abort();
    let _ = waiting.await;
    // Closing the Kernel does not replay accepted but unclaimed workflow work.
    f.kernel.shutdown(f.workers).await.unwrap();
    drop(events);
    let cold = AgentKernel::recover_with_clock(
        f.store.clone(),
        f.composition.clone(),
        Arc::new(FixedClock),
    )
    .await
    .unwrap();
    let records = f
        .store
        .read_program_records(f.root.session_id(), &f.run.descriptor().run_id)
        .await
        .unwrap()
        .unwrap();
    assert!(matches!(
        records.records.last().unwrap().body(),
        AgentControlRecordBody::ProgramRun {
            event: ProgramRunEvent::Terminal {
                outcome: ProgramOutcome::Interrupted,
                ..
            },
            ..
        }
    ));
    let child = f.run.descriptor().child_session_id(1).unwrap();
    assert_eq!(
        f.store
            .read_watermarks(&child)
            .await
            .unwrap()
            .durable_fact_seq,
        0
    );
    assert!(
        !f.store
            .read_agent_subtree_snapshot(f.root.session_id())
            .await
            .unwrap()
            .descendants[0]
            .status
            .has_waking_message
    );
    let workers = cold.start_workers();
    cold.shutdown(workers).await.unwrap();
    let g = fixture().await;
    assert!(g.run.cancel_from_creator().await.unwrap());
    assert!(g.run.detach().await.is_err());
    g.run.finish(ProgramOutcome::Cancelled, None).await.unwrap();
    g.kernel.shutdown(g.workers).await.unwrap();
}

#[tokio::test]
async fn workflow_recovery_discards_ordinary_unclaimed_grandchild() {
    let f = fixture().await;
    f.run.detach().await.unwrap();
    let owner = f.run.clone();
    let wait = tokio::spawn(async move {
        owner
            .agent(ProgramAgentRequest {
                message: "spawn work".into(),
                output_contract: None,
                role: None,
            })
            .await
    });
    let child = child_claim(&f).await;
    f.kernel.composition(&child).unwrap();
    let caller = control_tool_caller(&f.kernel, &child).await;
    let grandchild = SessionId::new("ordinary-grandchild").unwrap();
    f.kernel
        .spawn_agent(SpawnAgentRequest {
            caller,
            child_session_id: grandchild.clone(),
            task_name: "grandchild".into(),
            message_id: MessageId::new("grandchild-input").unwrap(),
            message: "must not replay".into(),
            fork_turns: ForkTurnSelection::None,
            output_contract: None,
            role: None,
            model: None,
            reasoning_effort: None,
            cancellation: CancellationToken::new(),
        })
        .await
        .unwrap();
    assert_eq!(
        f.store
            .read_watermarks(&grandchild)
            .await
            .unwrap()
            .durable_fact_seq,
        0
    );
    wait.abort();
    let _ = wait.await;
    f.kernel.shutdown(f.workers).await.unwrap();
    let cold = AgentKernel::recover_with_clock(
        f.store.clone(),
        f.composition.clone(),
        Arc::new(FixedClock),
    )
    .await
    .unwrap();
    let records = f
        .store
        .read_program_records(f.root.session_id(), &f.run.descriptor().run_id)
        .await
        .unwrap()
        .unwrap();
    assert!(records.head.terminal);
    assert_eq!(
        f.store
            .read_watermarks(&grandchild)
            .await
            .unwrap()
            .durable_fact_seq,
        0
    );
    assert_eq!(
        f.store
            .read_agent_mailbox_summary(&grandchild)
            .await
            .unwrap()
            .pending_count,
        0
    );
    let workers = cold.start_workers();
    cold.shutdown(workers).await.unwrap();
}

#[tokio::test]
async fn workflow_restart_discards_both_next_step_and_next_turn_notices() {
    for end_first in [false, true] {
        let f = fixture().await;
        f.run.detach().await.unwrap();
        if end_first {
            end_creator(&f).await;
        }
        f.run
            .finish(
                ProgramOutcome::Completed,
                Some(serde_json::json!({"done":true})),
            )
            .await
            .unwrap();
        let mailbox = f
            .store
            .read_agent_mailbox(f.root.session_id(), None)
            .await
            .unwrap();
        assert_eq!(mailbox.pending_count, 1);
        assert_eq!(
            mailbox.pending[0].target,
            if end_first {
                MessageTarget::NextTurn
            } else {
                MessageTarget::NextStep
            }
        );
        f.kernel.shutdown(f.workers).await.unwrap();
        let cold = AgentKernel::recover_with_clock(
            f.store.clone(),
            f.composition.clone(),
            Arc::new(FixedClock),
        )
        .await
        .unwrap();
        assert_eq!(
            f.store
                .read_agent_mailbox_summary(f.root.session_id())
                .await
                .unwrap()
                .pending_count,
            0
        );
        assert!(
            f.store
                .list_program_notices(None, 256)
                .await
                .unwrap()
                .notices
                .is_empty()
        );
        let workers = cold.start_workers();
        cold.shutdown(workers).await.unwrap();
    }
}

#[tokio::test]
async fn workflow_policy_change_revokes_even_after_lost_ack_and_off_on_cycle() {
    for lost_ack in [false, true] {
        let f = fixture().await;
        f.run.detach().await.unwrap();
        f.faults
            .fail_domain_after_apply
            .store(lost_ack, Ordering::Release);
        let mutation = |id: &str, revision, value| rsi_agent_turn_protocol::DomainMutation {
            guards: vec![],
            require_uncancelled_turn: false,
            request_id: rsi_agent_session_protocol::DomainRequestId::new(id).unwrap(),
            proposals: vec![
                f.policy
                    .propose(
                        rsi_agent_session_protocol::DomainRevision::new(revision),
                        &value,
                    )
                    .unwrap(),
            ],
            facts: vec![],
        };
        f.kernel
            .commit_domains(&f.root, mutation("on", 1, true))
            .await
            .unwrap();
        assert!(f.run.cancellation().is_cancelled());
        f.kernel
            .commit_domains(&f.root, mutation("off", 2, false))
            .await
            .unwrap();
        assert!(
            f.run
                .agent(ProgramAgentRequest {
                    message: "cannot regain authority".into(),
                    output_contract: None,
                    role: None
                })
                .await
                .is_err()
        );
        f.run.finish(ProgramOutcome::Cancelled, None).await.unwrap();
        f.kernel.shutdown(f.workers).await.unwrap();
    }
}

#[tokio::test]
async fn workflow_detach_commit_wins_against_later_creator_cancellation() {
    let f = fixture().await;
    f.faults.pause_next_agent_commit_before_apply();
    let run = f.run.clone();
    let detach = tokio::spawn(async move { run.detach().await });
    f.faults.wait_until_agent_commit_is_before_apply().await;
    let cancel = f.run.cancel_from_creator();
    tokio::pin!(cancel);
    assert!(futures_util::poll!(&mut cancel).is_pending());
    f.faults.release_agent_commit_before_apply();
    detach.await.unwrap().unwrap();
    assert!(!cancel.await.unwrap());
    assert!(!f.run.cancellation().is_cancelled());
    f.run.finish(ProgramOutcome::Completed, None).await.unwrap();
    f.kernel.shutdown(f.workers).await.unwrap();
}

#[tokio::test]
async fn workflow_finish_returns_the_durable_outcome_after_cancellation() {
    let f = fixture().await;
    f.run.cancel().await.unwrap();
    for _ in 0..2 {
        assert_eq!(
            f.run
                .finish(ProgramOutcome::Completed, Some(serde_json::json!(42)))
                .await
                .unwrap(),
            ProgramOutcome::Cancelled,
        );
    }
    let records = f
        .store
        .read_program_records(f.root.session_id(), &f.run.descriptor().run_id)
        .await
        .unwrap()
        .unwrap();
    assert!(matches!(
        records.records.last().unwrap().body(),
        AgentControlRecordBody::ProgramRun {
            event: ProgramRunEvent::Terminal {
                outcome: ProgramOutcome::Cancelled,
                result: None
            },
            ..
        }
    ));
    f.kernel.shutdown(f.workers).await.unwrap();
}

#[tokio::test]
async fn workflow_reconciles_lost_acceptance_progress_and_terminal_acknowledgements() {
    let f = fixture_with_accept_failure(true).await;
    f.faults
        .fail_program_after_apply
        .store(true, Ordering::Release);
    f.run.progress(None, "committed once".into()).await.unwrap();
    f.faults
        .fail_program_after_apply
        .store(true, Ordering::Release);
    assert_eq!(
        f.run.finish(ProgramOutcome::Completed, None).await.unwrap(),
        ProgramOutcome::Completed
    );
    let records = f
        .store
        .read_program_records(f.root.session_id(), &f.run.descriptor().run_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(records.records.len(), 4);
    assert!(records.head.terminal);
    f.kernel.shutdown(f.workers).await.unwrap();
}

#[tokio::test]
async fn workflow_cancellation_sweeps_a_grandchild_admitted_during_cancellation() {
    let f = fixture().await;
    let owner = f.run.clone();
    let waiting = tokio::spawn(async move {
        owner
            .agent(ProgramAgentRequest {
                message: "spawn".into(),
                output_contract: None,
                role: None,
            })
            .await
    });
    let child = child_claim(&f).await;
    f.kernel.composition(&child).unwrap();
    let caller = control_tool_caller(&f.kernel, &child).await;
    waiting.abort();
    let _ = waiting.await;
    let grandchild = SessionId::new("racing-grandchild").unwrap();
    f.faults.pause_next_agent_commit_before_apply();
    let kernel = f.kernel.clone();
    let id = grandchild.clone();
    let spawning = tokio::spawn(async move {
        kernel
            .spawn_agent(SpawnAgentRequest {
                caller,
                child_session_id: id,
                task_name: "grandchild".into(),
                message_id: MessageId::new("racing-input").unwrap(),
                message: "old work".into(),
                fork_turns: ForkTurnSelection::None,
                output_contract: None,
                role: None,
                model: None,
                reasoning_effort: None,
                cancellation: CancellationToken::new(),
            })
            .await
    });
    f.faults.wait_until_agent_commit_is_before_apply().await;
    f.faults.block_header_reads_for(child.session_id().clone());
    let reads = f.faults.header_read_attempts.load(Ordering::Acquire);
    let owner = f.run.clone();
    let cancelling = tokio::spawn(async move { owner.cancel().await });
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        while f.faults.header_read_attempts.load(Ordering::Acquire) == reads {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert!(!cancelling.is_finished());
    f.faults.release_blocked_header_reads();
    f.faults.release_agent_commit_before_apply();
    spawning.await.unwrap().unwrap();
    cancelling.await.unwrap().unwrap();
    assert_eq!(
        f.store
            .read_agent_mailbox_summary(&grandchild)
            .await
            .unwrap()
            .pending_count,
        0
    );
    assert_eq!(
        f.store
            .read_watermarks(&grandchild)
            .await
            .unwrap()
            .durable_fact_seq,
        0
    );
    let mut settled = result(child.turn_id(), "fixture-control-tool", "cancelled");
    if let SessionFactBody::ToolResult { identity, .. } = &mut settled {
        *identity = ToolResultIdentity::new(
            "fixture",
            "fixture-control-tool",
            "fixture-call",
            "a".repeat(64),
        )
        .unwrap();
    }
    flush_bodies(&f.kernel, &child, vec![settled]).await;
    f.kernel
        .finish_turn(&child, &TurnOutcome::Cancelled)
        .await
        .unwrap();
    f.run.finish(ProgramOutcome::Cancelled, None).await.unwrap();
    f.kernel.shutdown(f.workers).await.unwrap();
}

#[tokio::test]
async fn workflow_terminal_commit_keeps_admission_after_observer_is_dropped() {
    let f = fixture().await;
    f.run.detach().await.unwrap();
    f.faults.pause_next_agent_commit_before_apply();
    let run = f.run.clone();
    let finish = tokio::spawn(async move { run.finish(ProgramOutcome::Completed, None).await });
    f.faults.wait_until_agent_commit_is_before_apply().await;
    finish.abort();
    assert!(finish.await.unwrap_err().is_cancelled());
    let mutation = rsi_agent_turn_protocol::DomainMutation {
        guards: vec![],
        require_uncancelled_turn: false,
        request_id: rsi_agent_session_protocol::DomainRequestId::new("after-program").unwrap(),
        proposals: vec![
            f.policy
                .propose(rsi_agent_session_protocol::DomainRevision::new(1), &true)
                .unwrap(),
        ],
        facts: vec![],
    };
    let changed = f.kernel.commit_domains(&f.root, mutation);
    tokio::pin!(changed);
    assert!(
        futures_util::poll!(&mut changed).is_pending(),
        "the admitted terminal still owns the parent gate"
    );
    f.faults.release_agent_commit_before_apply();
    changed.await.unwrap();
    assert!(
        f.store
            .read_program_records(f.root.session_id(), &f.run.descriptor().run_id)
            .await
            .unwrap()
            .unwrap()
            .head
            .terminal
    );
    f.kernel.shutdown(f.workers).await.unwrap();
}

#[tokio::test]
#[allow(clippy::too_many_lines)] // Recovery preserves a distinct human activation across the old run's exclusive completion sink.
async fn workflow_recovery_preserves_independent_human_grandchild_input() {
    let f = fixture().await;
    f.run.detach().await.unwrap();
    let owner = f.run.clone();
    let wait = tokio::spawn(async move {
        owner
            .agent(ProgramAgentRequest {
                message: "spawn work".into(),
                output_contract: None,
                role: None,
            })
            .await
    });
    let child = child_claim(&f).await;
    f.kernel.composition(&child).unwrap();
    let caller = control_tool_caller(&f.kernel, &child).await;
    let grandchild = SessionId::new("ordinary-grandchild").unwrap();
    f.kernel
        .spawn_agent(SpawnAgentRequest {
            caller,
            child_session_id: grandchild.clone(),
            task_name: "grandchild".into(),
            message_id: MessageId::new("grandchild-input").unwrap(),
            message: "must not replay".into(),
            fork_turns: ForkTurnSelection::None,
            output_contract: None,
            role: None,
            model: None,
            reasoning_effort: None,
            cancellation: CancellationToken::new(),
        })
        .await
        .unwrap();
    assert_eq!(
        f.store
            .read_watermarks(&grandchild)
            .await
            .unwrap()
            .durable_fact_seq,
        0
    );
    for session in [&grandchild, child.session_id()] {
        f.kernel
            .submit_message(SubmitMessage {
                session: SubmitSession::Resume(f.kernel.prepare_resume(session).await.unwrap()),
                message: mailbox_message("independent-human"),
                delivery: rsi_agent_session_protocol::MessageDelivery::NextTurn,
            })
            .await
            .unwrap();
    }
    wait.abort();
    let _ = wait.await;
    f.kernel.shutdown(f.workers).await.unwrap();
    let cold = AgentKernel::recover_with_clock(
        f.store.clone(),
        f.composition.clone(),
        Arc::new(FixedClock),
    )
    .await
    .unwrap();
    let records = f
        .store
        .read_program_records(f.root.session_id(), &f.run.descriptor().run_id)
        .await
        .unwrap()
        .unwrap();
    assert!(matches!(
        records.records.last().unwrap().body(),
        AgentControlRecordBody::ProgramRun {
            event: ProgramRunEvent::Terminal {
                outcome: ProgramOutcome::Interrupted,
                ..
            },
            ..
        }
    ));
    assert_eq!(
        f.store
            .read_watermarks(&grandchild)
            .await
            .unwrap()
            .durable_fact_seq,
        0
    );
    assert_eq!(
        f.store
            .read_agent_mailbox_summary(&grandchild)
            .await
            .unwrap()
            .pending_count,
        1
    );
    let workers = cold.start_workers();
    let _executor = cold.register("recovered-human".into()).unwrap();
    let human = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        cold.claim("recovered-human", CancellationToken::new()),
    )
    .await
    .unwrap()
    .unwrap()
    .unwrap();
    assert_eq!(human.session_id(), &grandchild);
    cold.composition(&human).unwrap();
    cold.finish_turn(&human, &TurnOutcome::Completed)
        .await
        .unwrap();
    wait_for_settlement(f.store.as_ref(), child.session_id()).await;
    let unchanged = f
        .store
        .read_program_records(f.root.session_id(), &f.run.descriptor().run_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        records, unchanged,
        "interrupted run history stays immutable"
    );
    assert_eq!(
        f.store
            .read_agent_mailbox_summary(f.root.session_id())
            .await
            .unwrap()
            .pending_count,
        0,
        "the retired initial activation cannot fall through to the parent mailbox"
    );
    // Explicit human input in the Program child also keeps ordinary completion routing.
    let followup = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        cold.claim("recovered-human", CancellationToken::new()),
    )
    .await
    .unwrap()
    .unwrap()
    .unwrap();
    assert_eq!(followup.session_id(), child.session_id());
    assert_ne!(followup.turn_id(), child.turn_id());
    cold.composition(&followup).unwrap();
    cold.finish_turn(&followup, &TurnOutcome::Completed)
        .await
        .unwrap();
    assert_eq!(
        f.store
            .read_agent_mailbox_summary(f.root.session_id())
            .await
            .unwrap()
            .pending_count,
        1,
        "a later activation retains ordinary completion routing"
    );
    cold.shutdown(workers).await.unwrap();
}

#[tokio::test]
async fn workflow_progress_materializes_only_new_controls_for_repeated_readers() {
    let f = fixture().await;
    let caller = f
        .kernel
        .tool_caller(&f.root, &EffectId::new("workflow").unwrap())
        .unwrap();
    f.faults
        .program_records_materialized
        .store(0, Ordering::SeqCst);
    for index in 0..128 {
        f.run
            .progress(None, format!("progress {index}"))
            .await
            .unwrap();
        for _ in 0..16 {
            let view = f
                .kernel
                .read_program(&caller, &f.run.descriptor().run_id)
                .await
                .unwrap();
            assert_eq!(view.progress, Some(format!("progress {index}")));
        }
    }
    assert_eq!(
        f.faults.program_records_materialized.load(Ordering::SeqCst),
        129,
        "one Started and 128 new progress controls, independent of repeated readers"
    );
    f.run.finish(ProgramOutcome::Completed, None).await.unwrap();
    end_creator(&f).await;
    f.kernel.shutdown(f.workers).await.unwrap();
}

#[tokio::test]
async fn cancelled_child_wait_returns_without_claiming_child_terminal() {
    let f = fixture().await;
    let owner = f.run.clone();
    let waiting = tokio::spawn(async move {
        owner
            .agent(ProgramAgentRequest {
                message: "remain active".into(),
                output_contract: None,
                role: None,
            })
            .await
    });
    let child = child_claim(&f).await;
    f.run.cancel().await.unwrap();
    assert_eq!(
        tokio::time::timeout(std::time::Duration::from_secs(1), waiting)
            .await
            .expect("cancellation must release the waiter before a child terminal")
            .unwrap()
            .unwrap_err(),
        TurnError::Cancelled
    );
    assert!(
        f.kernel
            .outcome(child.session_id(), child.turn_id())
            .await
            .unwrap()
            .is_none()
    );
    f.kernel
        .finish_turn(&child, &TurnOutcome::Cancelled)
        .await
        .unwrap();
    assert_eq!(
        f.run.finish(ProgramOutcome::Cancelled, None).await.unwrap(),
        ProgramOutcome::Cancelled
    );
    f.kernel.shutdown(f.workers).await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn workflow_finalization_retains_ownership_beyond_interactive_cancellation_deadline() {
    let f = fixture().await;
    let owner = f.run.clone();
    let waiting = tokio::spawn(async move {
        owner
            .agent(ProgramAgentRequest {
                message: "spawn".into(),
                output_contract: None,
                role: None,
            })
            .await
    });
    let child = child_claim(&f).await;
    f.kernel.composition(&child).unwrap();
    let caller = control_tool_caller(&f.kernel, &child).await;
    waiting.abort();
    let _ = waiting.await;
    let grandchild = SessionId::new("racing-grandchild").unwrap();
    f.faults.pause_next_agent_commit_before_apply();
    let kernel = f.kernel.clone();
    let id = grandchild.clone();
    let spawning = tokio::spawn(async move {
        kernel
            .spawn_agent(SpawnAgentRequest {
                caller,
                child_session_id: id,
                task_name: "grandchild".into(),
                message_id: MessageId::new("racing-input").unwrap(),
                message: "old work".into(),
                fork_turns: ForkTurnSelection::None,
                output_contract: None,
                role: None,
                model: None,
                reasoning_effort: None,
                cancellation: CancellationToken::new(),
            })
            .await
    });
    f.faults.wait_until_agent_commit_is_before_apply().await;
    f.faults.block_header_reads_for(child.session_id().clone());
    let reads = f.faults.header_read_attempts.load(Ordering::Acquire);
    let owner = f.run.clone();
    let cancelling =
        tokio::spawn(async move { owner.finish(ProgramOutcome::Cancelled, None).await });
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        while f.faults.header_read_attempts.load(Ordering::Acquire) == reads {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    tokio::time::advance(std::time::Duration::from_secs(61)).await;
    tokio::task::yield_now().await;
    assert!(
        !cancelling.is_finished(),
        "owned finalization must outlive interactive cancellation deadline"
    );
    f.faults.release_blocked_header_reads();
    f.faults.release_agent_commit_before_apply();
    spawning.await.unwrap().unwrap();
    assert_eq!(
        f.store
            .read_agent_mailbox_summary(&grandchild)
            .await
            .unwrap()
            .pending_count,
        0
    );
    assert_eq!(
        f.store
            .read_watermarks(&grandchild)
            .await
            .unwrap()
            .durable_fact_seq,
        0
    );
    let mut settled = result(child.turn_id(), "fixture-control-tool", "cancelled");
    if let SessionFactBody::ToolResult { identity, .. } = &mut settled {
        *identity = ToolResultIdentity::new(
            "fixture",
            "fixture-control-tool",
            "fixture-call",
            "a".repeat(64),
        )
        .unwrap();
    }
    flush_bodies(&f.kernel, &child, vec![settled]).await;
    f.kernel
        .finish_turn(&child, &TurnOutcome::Cancelled)
        .await
        .unwrap();
    assert_eq!(
        cancelling.await.unwrap().unwrap(),
        ProgramOutcome::Cancelled
    );
    f.kernel.shutdown(f.workers).await.unwrap();
}

#[tokio::test]
async fn cold_recovery_interrupts_historical_roles_without_revalidating_against_a_new_catalog() {
    let f = fixture().await;
    f.kernel.shutdown(f.workers).await.unwrap();
    let old = &f.composition.0;
    let replacement = Arc::new(ProgramComposition(
        AgentCompositionPin::new(
            old.preset_id().clone(),
            "b".repeat(64),
            Arc::new(SourceOnlyTools),
            old.context_builder().clone(),
            old.domains().clone(),
            old.contributions().clone(),
            Arc::new(()),
        )
        .unwrap(),
    ));
    assert_ne!(
        old.tools().program_role("fixture_workflow"),
        replacement.0.tools().program_role("fixture_workflow")
    );
    let cold = AgentKernel::recover_with_clock(f.store.clone(), replacement, Arc::new(FixedClock))
        .await
        .unwrap();
    let records = f
        .store
        .read_program_records(f.root.session_id(), &f.run.descriptor().run_id)
        .await
        .unwrap()
        .unwrap();
    assert!(records.records.iter().any(|record| matches!(
        record.body(),
        AgentControlRecordBody::ProgramRun {
            event: ProgramRunEvent::Terminal {
                outcome: ProgramOutcome::Interrupted,
                ..
            },
            ..
        }
    )));
    let workers = cold.start_workers();
    cold.shutdown(workers).await.unwrap();
}

#[tokio::test]
async fn cancellation_arriving_during_finish_drains_children_before_terminal() {
    let f = fixture().await;
    let owner = f.run.clone();
    let waiter = tokio::spawn(async move {
        owner
            .agent(ProgramAgentRequest {
                message: "stay active".into(),
                output_contract: None,
                role: None,
            })
            .await
    });
    let child = child_claim(&f).await;
    waiter.abort();
    assert!(waiter.await.unwrap_err().is_cancelled());
    let mut finishing = Box::pin(f.run.finish(ProgramOutcome::Completed, None));
    assert!(futures_util::poll!(&mut finishing).is_pending());
    f.run.cancellation().cancel();
    let child_cancel = f.kernel.cancellation(&child).unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(1), async {
        tokio::select! {
            result = &mut finishing => panic!("finish must await the child receipt: {result:?}"),
            () = child_cancel.cancelled() => {},
        }
    })
    .await
    .expect("finish must drain children after late cancellation");
    let (finished, terminal) = tokio::time::timeout(std::time::Duration::from_secs(5), async {
        tokio::join!(
            finishing,
            f.kernel.finish_turn(&child, &TurnOutcome::Cancelled)
        )
    })
    .await
    .expect("owned drain and child settlement must both progress");
    terminal.unwrap();
    assert_eq!(finished.unwrap(), ProgramOutcome::Cancelled);
    f.kernel.shutdown(f.workers).await.unwrap();
}

#[tokio::test]
async fn lost_live_owner_revokes_process_token_and_requires_restart_recovery() {
    let f = fixture().await;
    let caller = f
        .kernel
        .tool_caller(&f.root, &EffectId::new("workflow").unwrap())
        .unwrap();
    let id = f.run.descriptor().run_id.clone();
    let cancelled = f.run.cancellation();
    drop(f.run);
    assert!(cancelled.is_cancelled());
    assert_eq!(
        f.kernel.cancel_program(&caller, &id).await.unwrap_err(),
        TurnError::StaleClaim
    );
    assert!(
        !f.store
            .read_program_records(f.root.session_id(), &id)
            .await
            .unwrap()
            .unwrap()
            .head
            .terminal
    );
    f.kernel.shutdown(f.workers).await.unwrap();
    let cold =
        AgentKernel::recover_with_clock(f.store.clone(), f.composition, Arc::new(FixedClock))
            .await
            .unwrap();
    let records = f
        .store
        .read_program_records(f.root.session_id(), &id)
        .await
        .unwrap()
        .unwrap();
    assert!(records.records.iter().any(|record| matches!(
        record.body(),
        AgentControlRecordBody::ProgramRun {
            event: ProgramRunEvent::Terminal {
                outcome: ProgramOutcome::Interrupted,
                ..
            },
            ..
        }
    )));
    let workers = cold.start_workers();
    cold.shutdown(workers).await.unwrap();
}
