use super::*;
use rsi_agent_session_protocol::{ExecutionOwner, ProgramOutcome, ProgramRunEvent};
use rsi_agent_turn_protocol::{PrepareProgram, ProgramAgentRequest, ProgramRead, ProgramRun};
struct Fixture {
    store: Arc<dyn SessionStore>,
    faults: Arc<FactReadRaceStore>,
    policy: rsi_agent_composition_protocol::DomainHandle<bool>,
    kernel: AgentKernel,
    composition: Arc<ProgramComposition>,
    workers: rsi_agent_kernel::KernelWorkers,
    root: TurnClaim,
    run: Arc<dyn ProgramRun>,
    creator_cancellation: CancellationToken,
    _executor: rsi_agent_turn_protocol::ExecutorLease,
}
#[allow(clippy::too_many_lines)] // The fixture issues real model-origin authority and durably accepts its run.
async fn fixture() -> Fixture {
    fixture_with_accept_failure(false).await
}
#[allow(clippy::too_many_lines)] // The fixture issues real model authority and can lose acceptance acknowledgement.
async fn fixture_with_accept_failure(lose_ack: bool) -> Fixture {
    fixture_with_execution(lose_ack, None).await
}
#[allow(clippy::too_many_lines)] // Builds the same real workflow fixture under an optional exact execution lease.
async fn fixture_with_execution(
    lose_ack: bool,
    execution: Option<rsi_execution::ExecutionLease>,
) -> Fixture {
    fixture_with_store(lose_ack, execution, Arc::new(MemoryStore::new()), true).await
}
#[allow(clippy::too_many_lines)] // Reuses real Kernel authority with either mechanical Store implementation.
async fn fixture_with_store(
    lose_ack: bool,
    execution: Option<rsi_execution::ExecutionLease>,
    store: Arc<dyn SessionStore>,
    accept: bool,
) -> Fixture {
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
    let mut root_header = header("workflow-root");
    if let Some(execution) = &execution {
        root_header = SessionHeader::new(
            root_header.session_id().clone(),
            root_header.created_at_ms(),
            rsi_execution::ExecutionCoordinates::new(
                execution.binding().location().clone(),
                root_header.canonical_cwd(),
            )
            .unwrap(),
            root_header.agent_preset_id().clone(),
            root_header.settings().clone(),
        )
        .unwrap();
    }
    let mut session = SubmitSession::Fresh(
        PreparedFreshSession::new(root_header, composition.0.clone()).unwrap(),
    );
    if let Some(execution) = execution {
        session = session.with_execution(execution).unwrap();
    }
    kernel
        .submit_message(SubmitMessage {
            session,
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
    let caller = start_workflow_tool(&kernel, &root).await;
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
    let creator_cancellation = CancellationToken::new();
    let run = kernel
        .prepare_program(PrepareProgram {
            caller: caller.clone(),
            cancellation: creator_cancellation.clone(),
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
    if accept {
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
    }
    Fixture {
        store,
        faults,
        policy,
        kernel,
        composition,
        workers,
        root,
        run,
        creator_cancellation,
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

async fn workflow_caller_for_root(
    f: &Fixture,
    name: &str,
) -> rsi_agent_turn_protocol::AgentCallerAuthority {
    f.kernel
        .submit_message(SubmitMessage {
            session: SubmitSession::Fresh(
                PreparedFreshSession::new(header(name), f.composition.0.clone()).unwrap(),
            ),
            message: mailbox_message(&format!("{name}-input")),
            delivery: rsi_agent_session_protocol::MessageDelivery::NextTurn,
        })
        .await
        .unwrap();
    let claim = f
        .kernel
        .claim("workflow-worker", CancellationToken::new())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(claim.session_id().as_str(), name);
    start_workflow_tool(&f.kernel, &claim).await
}

async fn start_workflow_tool(
    kernel: &AgentKernel,
    claim: &TurnClaim,
) -> rsi_agent_turn_protocol::AgentCallerAuthority {
    kernel.composition(claim).unwrap();
    publish_model_source(kernel, claim, "workflow", "fixture_workflow", &snapshot()).await;
    flush_bodies(
        kernel,
        claim,
        vec![intent(
            claim.turn_id(),
            "workflow",
            ToolOrigin::Model {
                effect_id: EffectId::new("source-model").unwrap(),
            },
            "fixture_workflow",
            ToolProgramRole::Workflow,
        )],
    )
    .await;
    flush_bodies(kernel, claim, vec![started(claim.turn_id(), "workflow")]).await;
    kernel
        .tool_caller(claim, &EffectId::new("workflow").unwrap())
        .unwrap()
}

fn capacity_request(caller: &rsi_agent_turn_protocol::AgentCallerAuthority) -> PrepareProgram {
    PrepareProgram {
        caller: caller.clone(),
        cancellation: CancellationToken::new(),
        script: "return 42".into(),
        fork_turns: ForkTurnSelection::None,
        guard: None,
        continuation_domains: vec![],
    }
}

async fn settle_capacity_creator(
    f: &Fixture,
    caller: &rsi_agent_turn_protocol::AgentCallerAuthority,
) {
    flush_bodies(
        &f.kernel,
        caller.claim(),
        vec![result(caller.turn_id(), "workflow", "detached")],
    )
    .await;
    f.kernel
        .finish_turn(caller.claim(), &TurnOutcome::Completed)
        .await
        .unwrap();
}

#[tokio::test]
async fn workflow_capacity_rejects_overlap_and_ninth_owner_then_reopens_after_terminal() {
    let f = fixture().await;
    let caller = f
        .kernel
        .tool_caller(&f.root, &EffectId::new("workflow").unwrap())
        .unwrap();
    assert!(matches!(
        f.kernel.prepare_program(capacity_request(&caller)).await,
        Err(TurnError::Capacity)
    ));
    assert!(!f.run.cancellation().is_cancelled());
    f.run.detach().await.unwrap();
    end_creator(&f).await;

    let mut owners = Vec::new();
    for index in 1..8 {
        let caller = workflow_caller_for_root(&f, &format!("capacity-root-{index}")).await;
        let run = f
            .kernel
            .prepare_program(capacity_request(&caller))
            .await
            .unwrap();
        run.accept(&caller).await.unwrap();
        run.start().await.unwrap();
        run.detach().await.unwrap();
        settle_capacity_creator(&f, &caller).await;
        owners.push(run);
    }
    assert_eq!(
        f.store
            .list_active_program_runs(None, 16)
            .await
            .unwrap()
            .runs
            .len(),
        8
    );
    let ninth = workflow_caller_for_root(&f, "capacity-root-ninth").await;
    let before = f.store.read_watermarks(ninth.session_id()).await.unwrap();
    let cas_writes = f.faults.cas_writes.load(Ordering::SeqCst);
    assert!(matches!(
        f.kernel.prepare_program(capacity_request(&ninth)).await,
        Err(TurnError::Capacity)
    ));
    assert_eq!(
        f.store.read_watermarks(ninth.session_id()).await.unwrap(),
        before
    );
    assert_eq!(f.faults.cas_writes.load(Ordering::SeqCst), cas_writes);
    assert!(
        f.kernel
            .list_session_programs(ninth.session_id(), before.durable_control_seq, None, 8)
            .await
            .unwrap()
            .runs
            .is_empty()
    );

    // Terminal cleanup releases capacity even while the old owner Arc is retained.
    assert_eq!(
        f.run.finish(ProgramOutcome::Completed, None).await.unwrap(),
        ProgramOutcome::Completed
    );
    let replacement = f
        .kernel
        .prepare_program(capacity_request(&ninth))
        .await
        .unwrap();
    replacement.accept(&ninth).await.unwrap();
    replacement.start().await.unwrap();
    replacement.detach().await.unwrap();
    settle_capacity_creator(&f, &ninth).await;
    assert_eq!(
        f.store
            .list_active_program_runs(None, 16)
            .await
            .unwrap()
            .runs
            .len(),
        8
    );
    replacement
        .finish(ProgramOutcome::Completed, None)
        .await
        .unwrap();
    for run in owners {
        run.finish(ProgramOutcome::Completed, None).await.unwrap();
    }
    assert!(
        f.store
            .list_active_program_runs(None, 16)
            .await
            .unwrap()
            .runs
            .is_empty()
    );
    f.kernel.shutdown(f.workers).await.unwrap();
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
    for (offset, first, last, next) in [(0, 1, 16, Some(16)), (112, 113, 128, None)] {
        let details = f
            .kernel
            .read_session_program(
                f.root.session_id(),
                &f.run.descriptor().run_id,
                ProgramRead {
                    children_offset: offset,
                    ..ProgramRead::default()
                },
            )
            .await
            .unwrap();
        assert_eq!(details.overview.children, 128);
        assert_eq!(details.children_offset, offset);
        assert_eq!(details.next_children_offset, next);
        assert_eq!(
            details
                .children
                .iter()
                .map(|child| child.ordinal)
                .collect::<Vec<_>>(),
            (first..=last).collect::<Vec<_>>()
        );
        assert_eq!(
            details.overview.retention.as_ref().unwrap().bytes(),
            serde_json::to_vec(&details).unwrap().len()
        );
    }
    for offset in [1, 144] {
        assert!(
            f.kernel
                .read_session_program(
                    f.root.session_id(),
                    &f.run.descriptor().run_id,
                    ProgramRead {
                        children_offset: offset,
                        ..ProgramRead::default()
                    },
                )
                .await
                .is_err()
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
async fn workflow_commit_with_lost_ack_and_unavailable_readback_is_unknown() {
    for operation in ["accept", "start", "progress", "finish"] {
        let f = fixture_with_store(
            false,
            None,
            Arc::new(MemoryStore::new()),
            !matches!(operation, "accept" | "start"),
        )
        .await;
        let caller = f
            .kernel
            .tool_caller(&f.root, &EffectId::new("workflow").unwrap())
            .unwrap();
        if operation == "start" {
            f.run.accept(&caller).await.unwrap();
        }
        f.faults
            .fail_program_after_apply
            .store(true, Ordering::Release);
        *f.faults.control_read_error.lock().unwrap() =
            Some(StoreError::Io("unavailable Program reconciliation".into()));
        let result = match operation {
            "accept" => f.run.accept(&caller).await,
            "start" => f.run.start().await,
            "progress" => f.run.progress(None, "committed progress".into()).await,
            "finish" => f
                .run
                .finish(ProgramOutcome::Completed, None)
                .await
                .map(|_| ()),
            _ => unreachable!(),
        };
        assert_eq!(
            result.unwrap_err(),
            TurnError::ExecutionOutcomeUnknown,
            "{operation}"
        );
        let durable = f
            .store
            .read_program_records(f.root.session_id(), &f.run.descriptor().run_id)
            .await
            .unwrap()
            .unwrap();
        assert!(
            durable
                .records
                .iter()
                .any(|record| matches!(record.body(), AgentControlRecordBody::ProgramRun { .. }))
        );
        f.run.finish(ProgramOutcome::Cancelled, None).await.unwrap();
        f.kernel.shutdown(f.workers).await.unwrap();
    }
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

#[tokio::test]
async fn detached_ssh_workflow_child_and_completion_keep_creator_execution() {
    use crate::execution_authority::tuple;
    let location = rsi_execution::ExecutionLocation::Ssh {
        target: rsi_execution::ExecutionTargetId::parse("b".repeat(32)).unwrap(),
    };
    let execution = tuple::lease(location, Arc::new(tuple::Gate::default()), 7);
    let f = fixture_with_execution(false, Some(execution.clone())).await;
    f.run.detach().await.unwrap();
    end_creator(&f).await;
    let run = f.run.clone();
    let wait = tokio::spawn(async move {
        run.agent(ProgramAgentRequest {
            message: "inspect target".into(),
            output_contract: None,
            role: None,
        })
        .await
    });
    let child = child_claim(&f).await;
    assert_eq!(child.execution(), Some(&execution));
    f.kernel
        .finish_turn(&child, &TurnOutcome::Completed)
        .await
        .unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(5), wait)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    f.run.finish(ProgramOutcome::Completed, None).await.unwrap();
    let notice = child_claim(&f).await;
    assert_eq!(notice.session_id(), f.root.session_id());
    assert_eq!(notice.execution(), Some(&execution));
    f.kernel
        .finish_turn(&notice, &TurnOutcome::Completed)
        .await
        .unwrap();
    f.kernel.shutdown(f.workers).await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn revoked_workflow_denies_new_children_but_settles_without_rearming_its_notice() {
    use crate::execution_authority::tuple;
    let location = rsi_execution::ExecutionLocation::Ssh {
        target: rsi_execution::ExecutionTargetId::parse("c".repeat(32)).unwrap(),
    };
    let gate = Arc::new(tuple::Gate::default());
    let execution = tuple::lease(location, gate.clone(), 8);
    let f = fixture_with_execution(false, Some(execution)).await;
    f.run.detach().await.unwrap();
    end_creator(&f).await;
    gate.revoked.store(true, Ordering::SeqCst);
    assert!(matches!(
        f.run
            .agent(ProgramAgentRequest {
                message: "forbidden".into(),
                output_contract: None,
                role: None
            })
            .await,
        Err(TurnError::ExecutionUnavailable)
    ));
    assert!(matches!(
        f.run.progress(None, "forbidden".into()).await,
        Err(TurnError::ExecutionUnavailable)
    ));
    assert_eq!(
        f.run
            .finish(ProgramOutcome::Interrupted, None)
            .await
            .unwrap(),
        ProgramOutcome::Interrupted
    );
    assert!(
        tokio::time::timeout(
            std::time::Duration::from_millis(100),
            f.kernel.claim("workflow-worker", CancellationToken::new())
        )
        .await
        .is_err()
    );
    let records = f
        .store
        .read_program_records(f.root.session_id(), &f.run.descriptor().run_id)
        .await
        .unwrap()
        .unwrap();
    assert!(!records.records.iter().any(|record| matches!(
        record.body(),
        AgentControlRecordBody::ProgramRun {
            event: ProgramRunEvent::ChildAdmitted { .. },
            ..
        }
    )));
    f.kernel.shutdown(f.workers).await.unwrap();
}

#[tokio::test]
async fn workbench_reads_canonical_history_and_cancellation_receipts_without_model_authority() {
    use rsi_agent_turn_protocol::ProgramCancelReceipt;
    let f = fixture().await;
    let id = f.run.descriptor().run_id.clone();
    f.run
        .progress(
            Some("\u{1}".repeat(rsi_agent_session_protocol::MAXIMUM_PROGRAM_PHASE_BYTES)),
            "\u{1}".repeat(rsi_agent_session_protocol::MAXIMUM_PROGRAM_PROGRESS_BYTES),
        )
        .await
        .unwrap();
    f.run.detach().await.unwrap();
    end_creator(&f).await;
    let seed = f
        .store
        .read_watermarks(f.root.session_id())
        .await
        .unwrap()
        .durable_control_seq;
    let history = f
        .kernel
        .list_session_programs(f.root.session_id(), seed, None, 8)
        .await
        .unwrap();
    assert_eq!(history.runs.len(), 1);
    assert_eq!(
        history.runs[0].retention.as_ref().unwrap().bytes(),
        serde_json::to_vec(&history.runs[0]).unwrap().len()
    );
    assert!(history.runs[0].detached && !history.runs[0].orphaned);
    assert!(
        f.kernel
            .list_session_programs(
                f.root.session_id(),
                seed,
                Some(history.runs[0].accepted_control_seq),
                8
            )
            .await
            .unwrap()
            .runs
            .is_empty()
    );
    assert!(
        f.kernel
            .read_session_program(
                &SessionId::new("foreign").unwrap(),
                &id,
                ProgramRead::default()
            )
            .await
            .is_err()
    );
    let receipt = f
        .kernel
        .cancel_session_program(f.root.session_id(), &id)
        .await
        .unwrap();
    assert_eq!(receipt.run_id(), &id);
    assert!(matches!(receipt, ProgramCancelReceipt::Accepted { .. }));
    assert!(f.run.cancellation().is_cancelled());
    let details = f
        .kernel
        .read_session_program(f.root.session_id(), &id, ProgramRead::default())
        .await
        .unwrap();
    assert_eq!(
        details.overview.retention.as_ref().unwrap().bytes(),
        serde_json::to_vec(&details).unwrap().len()
    );
    assert!(
        details.overview.cancelling && details.overview.outcome.is_none(),
        "acceptance does not assert cleanup"
    );
    f.run.finish(ProgramOutcome::Completed, None).await.unwrap();
    for _ in 0..2 {
        assert!(matches!(
            f.kernel
                .cancel_session_program(f.root.session_id(), &id)
                .await
                .unwrap(),
            ProgramCancelReceipt::AlreadyTerminal {
                outcome: ProgramOutcome::Cancelled,
                ..
            }
        ));
    }
    f.kernel.shutdown(f.workers).await.unwrap();
}

#[tokio::test]
async fn workbench_cancellation_reports_an_orphan_without_fabricating_terminal_cleanup() {
    use rsi_agent_turn_protocol::ProgramCancelReceipt;
    let f = fixture().await;
    let id = f.run.descriptor().run_id.clone();
    let session = f.root.session_id().clone();
    drop(f.run);
    assert!(matches!(
        f.kernel
            .cancel_session_program(&session, &id)
            .await
            .unwrap(),
        ProgramCancelReceipt::OrphanedRequiresRestart { .. }
    ));
    assert!(
        f.kernel
            .read_session_program(&session, &id, ProgramRead::default())
            .await
            .unwrap()
            .overview
            .orphaned
    );
    f.kernel.shutdown(f.workers).await.unwrap();
}

#[tokio::test]
async fn workbench_cancel_survives_dropped_acknowledgement_and_retains_admitted_commit() {
    let f = fixture().await;
    let id = f.run.descriptor().run_id.clone();
    f.faults.pause_next_agent_commit_before_apply();
    let kernel = f.kernel.clone();
    let session = f.root.session_id().clone();
    let run = id.clone();
    let waiter = tokio::spawn(async move { kernel.cancel_session_program(&session, &run).await });
    f.faults.wait_until_agent_commit_is_before_apply().await;
    waiter.abort();
    assert!(waiter.await.unwrap_err().is_cancelled());
    f.faults.release_agent_commit_before_apply();
    // A second request joins the same serialized durable transition.
    f.kernel
        .cancel_session_program(f.root.session_id(), &id)
        .await
        .unwrap();
    assert!(
        f.kernel
            .read_session_program(f.root.session_id(), &id, ProgramRead::default())
            .await
            .unwrap()
            .overview
            .cancelling
    );
    f.run.finish(ProgramOutcome::Cancelled, None).await.unwrap();
    f.kernel.shutdown(f.workers).await.unwrap();
}

#[tokio::test]
async fn workbench_cancel_preserves_prewrite_refusals_without_signalling_the_run() {
    for refusal in [
        StoreError::ReadCapacity,
        StoreError::ValidationBusy,
        StoreError::Invalid("injected prewrite refusal".into()),
    ] {
        let f = fixture().await;
        let id = f.run.descriptor().run_id.clone();
        let before = f.store.read_watermarks(f.root.session_id()).await.unwrap();
        // The first read selects the owner; the second is inside the owned task.
        *f.faults.program_read_fault.lock().unwrap() = Some((
            f.root.session_id().clone(),
            id.clone(),
            std::num::NonZeroUsize::new(2).unwrap(),
            refusal.clone(),
        ));
        let error = f
            .kernel
            .cancel_session_program(f.root.session_id(), &id)
            .await
            .unwrap_err();
        if matches!(refusal, StoreError::Invalid(_)) {
            assert!(matches!(error, TurnError::Invalid(_)), "{error:?}");
        } else {
            assert_eq!(error, TurnError::Capacity);
        }
        assert!(!f.run.cancellation().is_cancelled());
        assert_eq!(
            before,
            f.store.read_watermarks(f.root.session_id()).await.unwrap()
        );
        f.kernel
            .cancel_session_program(f.root.session_id(), &id)
            .await
            .unwrap();
        assert!(f.run.cancellation().is_cancelled());
        f.run.finish(ProgramOutcome::Cancelled, None).await.unwrap();
        f.kernel.shutdown(f.workers).await.unwrap();
    }
}

#[tokio::test]
async fn cancellation_preparation_failure_preserves_the_callers_authority_boundary() {
    for creator in [false, true] {
        let f = fixture().await;
        let id = f.run.descriptor().run_id.clone();
        let before = f.store.read_watermarks(f.root.session_id()).await.unwrap();
        *f.faults.watermark_read_fault.lock().unwrap() = Some((
            f.root.session_id().clone(),
            StoreError::Io("injected cancellation preparation failure".into()),
        ));
        let error = if creator {
            f.creator_cancellation.cancel();
            f.run.cancel_from_creator().await.unwrap_err()
        } else {
            f.kernel
                .cancel_session_program(f.root.session_id(), &id)
                .await
                .unwrap_err()
        };
        assert!(matches!(error, TurnError::Store(_)), "{error:?}");
        assert_eq!(f.run.cancellation().is_cancelled(), creator);
        assert_eq!(
            before,
            f.store.read_watermarks(f.root.session_id()).await.unwrap()
        );
        assert_eq!(
            f.run.finish(ProgramOutcome::Completed, None).await.unwrap(),
            if creator {
                ProgramOutcome::Cancelled
            } else {
                ProgramOutcome::Completed
            }
        );
        f.kernel.shutdown(f.workers).await.unwrap();
    }
}

#[tokio::test]
async fn cancellation_commit_refusals_preserve_the_callers_authority_boundary() {
    for creator in [false, true] {
        for refusal in [
            StoreError::ValidationBusy,
            StoreError::ReadCapacity,
            StoreError::Invalid("injected cancellation refusal".into()),
            StoreError::Conflict {
                expected: 0,
                actual: 1,
            },
        ] {
            let f = fixture().await;
            let id = f.run.descriptor().run_id.clone();
            let before = f.store.read_watermarks(f.root.session_id()).await.unwrap();
            *f.faults.commit_admission_refusal.lock().unwrap() = Some(refusal.clone());
            let error = if creator {
                f.creator_cancellation.cancel();
                f.run.cancel_from_creator().await.unwrap_err()
            } else {
                f.kernel
                    .cancel_session_program(f.root.session_id(), &id)
                    .await
                    .unwrap_err()
            };
            *f.faults.commit_admission_refusal.lock().unwrap() = None;
            assert!(
                !matches!(error, TurnError::ExecutionOutcomeUnknown),
                "{error:?}"
            );
            assert_eq!(f.run.cancellation().is_cancelled(), creator, "{refusal:?}");
            assert_eq!(
                before,
                f.store.read_watermarks(f.root.session_id()).await.unwrap()
            );
            assert_eq!(
                f.run.finish(ProgramOutcome::Completed, None).await.unwrap(),
                if creator {
                    ProgramOutcome::Cancelled
                } else {
                    ProgramOutcome::Completed
                }
            );
            f.kernel.shutdown(f.workers).await.unwrap();
        }
    }
}

#[tokio::test]
async fn cancellation_commit_panic_signals_the_owner_without_replaying() {
    for creator in [false, true] {
        let f = fixture().await;
        let id = f.run.descriptor().run_id.clone();
        let before = f.store.read_watermarks(f.root.session_id()).await.unwrap();
        f.faults
            .panic_program_after_apply
            .store(true, Ordering::Release);
        let error = if creator {
            f.run.cancel_from_creator().await.unwrap_err()
        } else {
            f.kernel
                .cancel_session_program(f.root.session_id(), &id)
                .await
                .unwrap_err()
        };
        assert_eq!(error, TurnError::ExecutionOutcomeUnknown);
        assert!(f.run.cancellation().is_cancelled());
        let after = f.store.read_watermarks(f.root.session_id()).await.unwrap();
        assert_eq!(after.durable_control_seq, before.durable_control_seq + 1);
        assert!(
            f.kernel
                .read_session_program(f.root.session_id(), &id, ProgramRead::default())
                .await
                .unwrap()
                .overview
                .cancelling
        );
        assert_eq!(
            f.run.finish(ProgramOutcome::Completed, None).await.unwrap(),
            ProgramOutcome::Cancelled
        );
        f.kernel.shutdown(f.workers).await.unwrap();
    }
}

#[tokio::test]
async fn cancellation_holds_session_admission_through_commit_against_finish() {
    let f = fixture().await;
    let id = f.run.descriptor().run_id.clone();
    f.faults.pause_next_agent_commit_before_apply();
    let kernel = f.kernel.clone();
    let session = f.root.session_id().clone();
    let cancel = tokio::spawn(async move { kernel.cancel_session_program(&session, &id).await });
    f.faults.wait_until_agent_commit_is_before_apply().await;
    let reads = f.faults.program_record_reads.load(Ordering::Acquire);
    let owner = f.run.clone();
    let finish = tokio::spawn(async move { owner.finish(ProgramOutcome::Completed, None).await });
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        while f.faults.program_record_reads.load(Ordering::Acquire) == reads {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert!(!finish.is_finished());
    assert!(!f.run.cancellation().is_cancelled());
    f.faults.release_agent_commit_before_apply();
    assert!(matches!(
        cancel.await.unwrap().unwrap(),
        rsi_agent_turn_protocol::ProgramCancelReceipt::Accepted { .. }
    ));
    assert_eq!(finish.await.unwrap().unwrap(), ProgramOutcome::Cancelled);
    f.kernel.shutdown(f.workers).await.unwrap();
}

#[tokio::test]
async fn workbench_cancel_reconciles_lost_ack_or_reports_unknown_when_readback_fails() {
    for readback_available in [true, false] {
        let f = fixture().await;
        let id = f.run.descriptor().run_id.clone();
        f.faults
            .fail_program_after_apply
            .store(true, Ordering::Release);
        if !readback_available {
            *f.faults.control_read_error.lock().unwrap() = Some(StoreError::Io(
                "injected cancellation readback failure".into(),
            ));
        }
        let receipt = f
            .kernel
            .cancel_session_program(f.root.session_id(), &id)
            .await;
        if readback_available {
            assert!(matches!(
                receipt,
                Ok(rsi_agent_turn_protocol::ProgramCancelReceipt::Accepted { .. })
            ));
        } else {
            assert_eq!(receipt.unwrap_err(), TurnError::ExecutionOutcomeUnknown);
        }
        assert!(f.run.cancellation().is_cancelled());
        assert!(
            f.kernel
                .read_session_program(f.root.session_id(), &id, ProgramRead::default())
                .await
                .unwrap()
                .overview
                .cancelling
        );
        f.kernel
            .cancel_session_program(f.root.session_id(), &id)
            .await
            .unwrap();
        f.run.finish(ProgramOutcome::Cancelled, None).await.unwrap();
        f.kernel.shutdown(f.workers).await.unwrap();
    }
}

#[tokio::test]
async fn workbench_historical_detail_and_blobs_share_one_canonical_read() {
    use sha2::Digest as _;
    let f = fixture().await;
    let run = f.run.descriptor().run_id.clone();
    let session = f.root.session_id().clone();
    f.run
        .finish(ProgramOutcome::Completed, Some(serde_json::json!(42)))
        .await
        .unwrap();
    end_creator(&f).await;
    drop(f.run);
    f.faults.program_record_reads.store(0, Ordering::SeqCst);
    let details = f
        .kernel
        .read_session_program(
            &session,
            &run,
            ProgramRead {
                script: true,
                result: true,
                ..ProgramRead::default()
            },
        )
        .await
        .unwrap();
    let script = details.script.as_ref().unwrap();
    let result = details.result.as_ref().unwrap();
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(result).unwrap(),
        serde_json::json!(42)
    );
    assert_eq!(script.len() as u64, details.overview.script_ref.bytes);
    assert_eq!(f.faults.program_record_reads.load(Ordering::SeqCst), 1);
    assert_eq!(
        format!("{:x}", sha2::Sha256::digest(script)),
        details.overview.script_ref.sha256
    );
    assert!(
        f.kernel
            .read_session_program(
                &session,
                &run,
                ProgramRead {
                    expected_control_seq: Some(details.overview.control_seq + 1),
                    script: true,
                    result: true,
                    ..ProgramRead::default()
                }
            )
            .await
            .is_err()
    );

    f.kernel.shutdown(f.workers).await.unwrap();
}

#[tokio::test]
async fn workbench_cancel_rechecks_terminal_after_owner_retires_during_read() {
    let f = fixture().await;
    let id = f.run.descriptor().run_id.clone();
    f.faults.pause_next_agent_commit_before_apply();
    let owner = f.run.clone();
    let finish = tokio::spawn(async move { owner.finish(ProgramOutcome::Completed, None).await });
    f.faults.wait_until_agent_commit_is_before_apply().await;
    let (entered_tx, entered) = tokio::sync::oneshot::channel();
    let (release, gate) = tokio::sync::oneshot::channel();
    *f.faults.program_read_barrier.lock().unwrap() =
        Some((f.root.session_id().clone(), id.clone(), entered_tx, gate));
    let kernel = f.kernel.clone();
    let session = f.root.session_id().clone();
    let cancel = tokio::spawn(async move { kernel.cancel_session_program(&session, &id).await });
    entered.await.unwrap();
    f.faults.release_agent_commit_before_apply();
    finish.await.unwrap().unwrap();
    release.send(()).unwrap();
    assert!(matches!(
        cancel.await.unwrap().unwrap(),
        rsi_agent_turn_protocol::ProgramCancelReceipt::AlreadyTerminal {
            outcome: ProgramOutcome::Completed,
            ..
        }
    ));
    f.kernel.shutdown(f.workers).await.unwrap();
}

#[tokio::test]
async fn workbench_rejects_malformed_history_before_canonical_reads() {
    use rsi_agent_store_protocol::StoreProgramHistoryPage;
    let f = fixture().await;
    let session = f.root.session_id();
    let seed = f
        .store
        .read_watermarks(session)
        .await
        .unwrap()
        .durable_control_seq;
    let head = f
        .store
        .list_program_history(session, seed, None, 1)
        .await
        .unwrap()
        .runs
        .remove(0);
    let mut beyond_seed = head.clone();
    beyond_seed.first_control_seq = seed + 1;
    for (page, limit) in [
        (
            StoreProgramHistoryPage {
                runs: vec![head.clone(), head.clone()],
                has_more: false,
            },
            1,
        ),
        (
            StoreProgramHistoryPage {
                runs: vec![head.clone(), head],
                has_more: false,
            },
            2,
        ),
        (
            StoreProgramHistoryPage {
                runs: vec![],
                has_more: true,
            },
            1,
        ),
        (
            StoreProgramHistoryPage {
                runs: vec![beyond_seed],
                has_more: false,
            },
            1,
        ),
    ] {
        *f.faults.program_history_override.lock().unwrap() = Some(page);
        let before = f.faults.program_record_reads.load(Ordering::SeqCst);
        assert!(
            f.kernel
                .list_session_programs(session, seed, None, limit)
                .await
                .is_err()
        );
        assert_eq!(f.faults.program_record_reads.load(Ordering::SeqCst), before);
    }
    f.run.finish(ProgramOutcome::Completed, None).await.unwrap();
    f.kernel.shutdown(f.workers).await.unwrap();
}

#[tokio::test]
async fn workbench_missing_blob_reports_store_failure_with_object_identity() {
    let f = fixture().await;
    let descriptor = f.run.descriptor();
    *f.faults.missing_cas.lock().unwrap() = Some(descriptor.script.sha256.clone());
    let error = f
        .kernel
        .read_session_program(
            f.root.session_id(),
            &descriptor.run_id,
            ProgramRead {
                script: true,
                ..ProgramRead::default()
            },
        )
        .await
        .unwrap_err();
    assert!(
        matches!(error, TurnError::Store(message) if message.contains(&descriptor.script.sha256)),
        "missing blob must not report a missing Session"
    );
    f.run
        .finish(ProgramOutcome::Completed, Some(serde_json::json!(42)))
        .await
        .unwrap();
    let result = f
        .kernel
        .read_session_program(
            f.root.session_id(),
            &descriptor.run_id,
            ProgramRead::default(),
        )
        .await
        .unwrap()
        .overview
        .result_ref
        .unwrap();
    *f.faults.missing_cas.lock().unwrap() = Some(result.sha256.clone());
    let caller = f
        .kernel
        .tool_caller(&f.root, &EffectId::new("workflow").unwrap())
        .unwrap();
    let error = f
        .kernel
        .read_program(&caller, &descriptor.run_id)
        .await
        .unwrap_err();
    assert!(
        matches!(error, TurnError::Store(message) if message.contains(&result.sha256)),
        "model observation must preserve the missing object identity too"
    );
    f.kernel.shutdown(f.workers).await.unwrap();
}

#[tokio::test]
async fn creator_cancellation_after_acceptance_refuses_start_and_detach_before_launch() {
    let f = fixture_with_store(false, None, Arc::new(MemoryStore::new()), false).await;
    let caller = f
        .kernel
        .tool_caller(&f.root, &EffectId::new("workflow").unwrap())
        .unwrap();
    f.run.accept(&caller).await.unwrap();
    f.creator_cancellation.cancel();
    assert_eq!(f.run.start().await, Err(TurnError::Cancelled));
    assert_eq!(f.run.detach().await, Err(TurnError::Cancelled));
    assert!(f.run.cancellation().is_cancelled());
    let records = f
        .store
        .read_program_records(f.root.session_id(), &f.run.descriptor().run_id)
        .await
        .unwrap()
        .unwrap();
    assert!(!records.records.iter().any(|record| matches!(
        record.body(),
        AgentControlRecordBody::ProgramRun {
            event: ProgramRunEvent::Started | ProgramRunEvent::Detached,
            ..
        }
    )));
    assert_eq!(
        f.run
            .finish(ProgramOutcome::Completed, Some(serde_json::json!(42)))
            .await
            .unwrap(),
        ProgramOutcome::Cancelled
    );
    end_creator(&f).await;
    f.kernel.shutdown(f.workers).await.unwrap();
}

#[tokio::test]
async fn sqlite_publication_faults_prevent_kernel_acceptance_and_result_reference_commits() {
    use rsi_agent_store_sqlite::{
        SqliteStore,
        test_support::{CasPublicationPhase as Phase, CasPublicationStep as Step},
    };
    for step in [
        Step::FileSync,
        Step::Link,
        Step::TemporaryRemoval,
        Step::StagingSync,
        Step::NamespaceSync,
    ] {
        for phase in [Phase::Before, Phase::After] {
            let root = tempfile::tempdir().unwrap();
            let store = Arc::new(SqliteStore::open(root.path()).unwrap());
            let f = fixture_with_store(false, None, store.clone(), false).await;
            let caller = f
                .kernel
                .tool_caller(&f.root, &EffectId::new("workflow").unwrap())
                .unwrap();
            let id = &f.run.descriptor().run_id;
            let before = store.read_watermarks(f.root.session_id()).await.unwrap();
            store.fail_next_cas_publication(step, phase);
            assert!(
                f.run.accept(&caller).await.is_err(),
                "accept {step:?} {phase:?}"
            );
            assert_eq!(
                store.read_watermarks(f.root.session_id()).await.unwrap(),
                before
            );
            assert!(
                store
                    .read_program_records(f.root.session_id(), id)
                    .await
                    .unwrap()
                    .is_none()
            );
            f.run.accept(&caller).await.unwrap();
            f.run.start().await.unwrap();
            store.fail_next_cas_publication(step, phase);
            assert!(
                f.run
                    .finish(
                        ProgramOutcome::Completed,
                        Some(serde_json::json!({"total":42}))
                    )
                    .await
                    .is_err(),
                "finish {step:?} {phase:?}"
            );
            let records = store
                .read_program_records(f.root.session_id(), id)
                .await
                .unwrap()
                .unwrap();
            assert!(
                !records.records.iter().any(|record| matches!(
                    record.body(),
                    AgentControlRecordBody::ProgramRun {
                        event: ProgramRunEvent::Terminal {
                            result: Some(_),
                            ..
                        },
                        ..
                    }
                )),
                "failed publication admitted a result reference"
            );
            // The adapter does not infer a committed terminal from readable bytes.
            // Shutdown retains the unfinished ledger for recovery.
            f.kernel.shutdown(f.workers).await.unwrap();
        }
    }
}
