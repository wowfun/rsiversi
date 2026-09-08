use super::*;
use rsi_agent_composition_protocol::{DomainCatalog, DomainDefinition, DomainHandle};
use rsi_agent_session_protocol::{DomainIdentity, DomainRevision, MessageDelivery};

#[derive(Debug)]
struct DomainComposition {
    pin: AgentCompositionPin,
}

#[async_trait]
impl AgentComposition for DomainComposition {
    async fn default_preset_id(&self) -> rsi_agent_composition_protocol::Result<AgentPresetId> {
        Ok(self.pin.preset_id().clone())
    }
    async fn pin(
        &self,
        preset: &AgentPresetId,
    ) -> rsi_agent_composition_protocol::Result<AgentCompositionPin> {
        assert_eq!(preset, self.pin.preset_id());
        Ok(self.pin.clone())
    }
}

fn domain_composition() -> (Arc<DomainComposition>, DomainHandle<bool>) {
    let definition = DomainDefinition::new(
        DomainIdentity::new("fixture.plan", 1).unwrap(),
        &false,
        |_| Ok(()),
    )
    .unwrap();
    let catalog = DomainCatalog::new([definition.registration()]).unwrap();
    let handle = catalog.bind(&definition).unwrap();
    let pin = AgentCompositionPin::new(
        AgentPresetId::new("test-agent").unwrap(),
        "a".repeat(64),
        Arc::new(EmptyTools),
        Arc::new(rsi_agent_context::DefaultContextBuilder::default()),
        catalog,
        Arc::new(()),
    )
    .unwrap();
    (Arc::new(DomainComposition { pin }), handle)
}

struct DomainRun {
    kernel: AgentKernel,
    workers: rsi_agent_kernel::KernelWorkers,
    lease: rsi_agent_turn_protocol::ExecutorLease,
    claim: rsi_agent_turn_protocol::TurnClaim,
    handle: DomainHandle<bool>,
}

impl DomainRun {
    async fn start(store: Arc<dyn SessionStore>, budget: TurnBudget) -> Self {
        Self::start_with_clock(store, budget, Arc::new(FixedClock)).await
    }
    async fn start_with_clock(
        store: Arc<dyn SessionStore>,
        budget: TurnBudget,
        clock: Arc<dyn Clock>,
    ) -> Self {
        let (composition, handle) = domain_composition();
        let header = SessionHeader::new(
            SessionId::new("domain-exec").unwrap(),
            42,
            "/workspace",
            AgentPresetId::new("test-agent").unwrap(),
            FrozenAgentSettings::new_with_budget(
                "default",
                "system",
                ModelRef::new("deployment", "model").unwrap(),
                SandboxMode::WorkspaceWrite,
                false,
                budget,
            )
            .unwrap(),
        )
        .unwrap();
        let initial = PreparedFreshSession::new(header, composition.pin.clone()).unwrap();
        let kernel = AgentKernel::recover_with_clock(store, composition, clock)
            .await
            .unwrap();
        let workers = kernel.start_workers();
        kernel
            .submit(SubmitTurn {
                session: SubmitSession::Fresh(initial),
                turn_id: TurnId::new("domain-turn").unwrap(),
                text: "work".into(),
                model: None,
                sandbox: None,
            })
            .await
            .unwrap();
        let lease = kernel.register("domain-worker".into()).unwrap();
        let claim = kernel
            .claim("domain-worker", CancellationToken::new())
            .await
            .unwrap()
            .unwrap();
        Self {
            kernel,
            workers,
            lease,
            claim,
            handle,
        }
    }
    fn mutation(
        &self,
        request: &str,
        revision: u64,
        state: bool,
        facts: Vec<SessionFactBody>,
    ) -> rsi_agent_turn_protocol::DomainMutation {
        rsi_agent_turn_protocol::DomainMutation {
            request_id: rsi_agent_session_protocol::DomainRequestId::new(request).unwrap(),
            proposals: vec![
                self.handle
                    .propose(DomainRevision::new(revision), &state)
                    .unwrap(),
            ],
            facts,
        }
    }
    fn step(&self, id: &str) -> SessionFactBody {
        SessionFactBody::StepStarted {
            turn_id: self.claim.turn_id().clone(),
            step_id: StepId::new(id).unwrap(),
        }
    }
    async fn stop(self) {
        drop(self.lease);
        self.kernel.shutdown(self.workers).await.unwrap();
    }
}

#[tokio::test]
async fn mixed_domain_mutations_bind_facts_revisions_generation_and_precommit_failure() {
    let store = Arc::new(MemoryStore::new());
    let run = DomainRun::start(store.clone(), TurnBudget::default()).await;
    let first = run
        .kernel
        .commit_domains(
            &run.claim,
            run.mutation("first", 1, true, vec![run.step("initial")]),
        )
        .await
        .unwrap();
    assert_eq!(first.control_seq(), 2);
    assert_eq!(first.commit().fact_span().unwrap().first_seq(), 2);
    assert_eq!(
        first.commit().source(),
        &rsi_agent_session_protocol::DomainMutationSource::Turn {
            turn_id: run.claim.turn_id().clone()
        }
    );
    assert_eq!(
        run.kernel
            .commit_domains(
                &run.claim,
                run.mutation("first", 1, true, vec![run.step("initial")])
            )
            .await
            .unwrap(),
        first
    );
    assert!(matches!(
        run.kernel
            .commit_domains(
                &run.claim,
                run.mutation("first", 1, true, vec![run.step("changed")])
            )
            .await,
        Err(TurnError::DomainRequestConflict { .. })
    ));
    assert!(matches!(
        run.kernel
            .commit_domains(&run.claim, run.mutation("stale", 1, false, vec![]))
            .await,
        Err(TurnError::DomainRevisionConflict { .. })
    ));
    let (_, foreign) = domain_composition();
    let mut wrong_generation = run.mutation("foreign", 2, false, vec![]);
    wrong_generation.proposals = vec![foreign.propose(DomainRevision::new(2), &false).unwrap()];
    assert!(matches!(
        run.kernel
            .commit_domains(&run.claim, wrong_generation)
            .await,
        Err(TurnError::Composition(_))
    ));
    store.fail_next_appends(1);
    assert!(matches!(
        run.kernel
            .commit_domains(&run.claim, run.mutation("second", 2, false, vec![]))
            .await,
        Err(TurnError::Store(_))
    ));
    let state = run
        .kernel
        .domain_states(run.claim.session_id())
        .await
        .unwrap();
    assert_eq!(state[0].revision, DomainRevision::new(2));
    assert!(run.handle.decode(&state[0].snapshot).unwrap());
    assert!(
        run.kernel
            .domain_request(
                run.claim.session_id(),
                &rsi_agent_session_protocol::DomainRequestId::new("second").unwrap()
            )
            .await
            .unwrap()
            .is_none()
    );
    let second = run
        .kernel
        .commit_domains(&run.claim, run.mutation("second", 2, false, vec![]))
        .await
        .unwrap();
    assert_eq!(second.control_seq(), 3);
    let usage = store
        .read_turn_domain_usage(run.claim.session_id(), run.claim.turn_id())
        .await
        .unwrap();
    assert_eq!(
        (
            usage.records,
            usage.durable_fact_seq,
            usage.durable_control_seq
        ),
        (2, 2, 3)
    );
    run.stop().await;
}

#[tokio::test]
async fn lost_domain_acknowledgements_reconcile_exact_receipts_and_charge_once() {
    let memory = Arc::new(MemoryStore::new());
    let store = Arc::new(FactReadRaceStore::new(memory.clone()));
    let run = DomainRun::start(
        store.clone(),
        TurnBudget::new(1_800_000, 64, 256, 1, 67_108_864).unwrap(),
    )
    .await;
    store.fail_domain_after_apply.store(true, Ordering::Release);
    let first = run
        .kernel
        .commit_domains(&run.claim, run.mutation("lost", 1, true, vec![]))
        .await
        .unwrap();
    assert_eq!(
        run.kernel
            .commit_domains(&run.claim, run.mutation("lost", 1, true, vec![]))
            .await
            .unwrap(),
        first
    );
    assert!(matches!(
        run.kernel
            .commit_domains(&run.claim, run.mutation("next", 2, false, vec![]))
            .await,
        Err(TurnError::BudgetExceeded {
            consumed: 2,
            limit: 1,
            ..
        })
    ));
    assert_eq!(
        memory
            .read_turn_domain_usage(run.claim.session_id(), run.claim.turn_id())
            .await
            .unwrap()
            .records,
        1
    );
    run.stop().await;
}

#[tokio::test]
async fn unresolvable_domain_commit_closes_execution_but_preserves_queryable_history() {
    let memory = Arc::new(MemoryStore::new());
    let store = Arc::new(FactReadRaceStore::new(memory.clone()));
    let run = DomainRun::start(store.clone(), TurnBudget::default()).await;
    let caller = run.kernel.agent_caller(&run.claim).unwrap();
    store.fail_domain_after_apply.store(true, Ordering::Release);
    store
        .fail_domain_lookup_after_apply
        .store(true, Ordering::Release);
    assert!(matches!(
        run.kernel
            .commit_domains(&run.claim, run.mutation("unknown", 1, true, vec![]))
            .await,
        Err(TurnError::DomainOutcomeUnknown { .. })
    ));
    store.domain_lookup_fails.store(false, Ordering::Release);
    let stored = run
        .kernel
        .domain_request(
            run.claim.session_id(),
            &rsi_agent_session_protocol::DomainRequestId::new("unknown").unwrap(),
        )
        .await
        .unwrap()
        .unwrap();
    assert_eq!(stored.control_seq(), 2);
    let rejected_child = SessionId::new("failed-domain-child").unwrap();
    assert!(matches!(
        run.kernel
            .spawn_agent(SpawnAgentRequest {
                caller,
                cancellation: CancellationToken::new(),
                child_session_id: rejected_child.clone(),
                task_name: "must-stay-closed".into(),
                message_id: MessageId::new("failed-domain-child").unwrap(),
                message: "must not be admitted".into(),
                fork_turns: ForkTurnSelection::None,
            })
            .await,
        Err(TurnError::Flush(_))
    ));
    assert!(matches!(
        memory.header(&rejected_child).await,
        Err(StoreError::NotFound(_))
    ));
    assert!(matches!(
        run.kernel
            .commit_domains(&run.claim, run.mutation("next", 2, false, vec![]))
            .await,
        Err(TurnError::Flush(_))
    ));
    drop(run.lease);
    assert!(run.kernel.shutdown(run.workers).await.is_err());
    let (composition, _) = domain_composition();
    let reopened =
        AgentKernel::recover_with_clock(memory.clone(), composition, Arc::new(FixedClock))
            .await
            .unwrap();
    assert_eq!(
        reopened
            .domain_states(run.claim.session_id())
            .await
            .unwrap()[0]
            .revision,
        DomainRevision::new(2)
    );
    assert_eq!(
        memory
            .read_turn_domain_usage(run.claim.session_id(), run.claim.turn_id())
            .await
            .unwrap()
            .records,
        1
    );
}

#[tokio::test]
async fn dropped_domain_waiter_retains_source_and_installs_budget_before_claim_handoff() {
    let memory = Arc::new(MemoryStore::new());
    let store = Arc::new(FactReadRaceStore::new(memory));
    let run = DomainRun::start(
        store.clone(),
        TurnBudget::new(1_800_000, 64, 256, 1, 67_108_864).unwrap(),
    )
    .await;
    store
        .pause_agent_commit_after_apply
        .store(true, Ordering::Release);
    let kernel = run.kernel.clone();
    let claim = run.claim.clone();
    let request = run.mutation("retained", 1, true, vec![]);
    let waiting = tokio::spawn(async move { kernel.commit_domains(&claim, request).await });
    tokio::time::timeout(
        std::time::Duration::from_secs(5),
        store.agent_commit_applied.notified(),
    )
    .await
    .unwrap();
    waiting.abort();
    assert!(waiting.await.unwrap_err().is_cancelled());
    run.kernel.release(&run.claim).unwrap();
    store.release_agent_commit.notify_one();
    let next = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        run.kernel.claim("domain-worker", CancellationToken::new()),
    )
    .await
    .unwrap()
    .unwrap()
    .unwrap();
    assert_ne!(next.claim_id(), run.claim.claim_id());
    assert!(matches!(
        run.kernel
            .commit_domains(&next, run.mutation("next", 2, false, vec![]))
            .await,
        Err(TurnError::BudgetExceeded {
            consumed: 2,
            limit: 1,
            ..
        })
    ));
    run.stop().await;
}

#[tokio::test]
async fn control_only_domain_work_exhausts_record_budget_and_rejects_the_whole_mixed_batch() {
    let store = Arc::new(MemoryStore::new());
    let run = DomainRun::start(
        store.clone(),
        TurnBudget::new(1_800_000, 64, 256, 1, 67_108_864).unwrap(),
    )
    .await;
    run.kernel
        .commit_domains(&run.claim, run.mutation("first", 1, true, vec![]))
        .await
        .unwrap();
    let before = store
        .read_domain_states(run.claim.session_id(), None)
        .await
        .unwrap();
    assert!(matches!(
        run.kernel
            .commit_domains(
                &run.claim,
                run.mutation("over", 2, false, vec![run.step("rejected")])
            )
            .await,
        Err(TurnError::BudgetExceeded {
            dimension: BudgetDimension::GeneratedRecords,
            consumed: 3,
            limit: 1
        })
    ));
    assert_eq!(
        store
            .read_domain_states(run.claim.session_id(), None)
            .await
            .unwrap(),
        before
    );
    let outcome = TurnOutcome::BudgetExceeded {
        dimension: BudgetDimension::GeneratedRecords,
        consumed: 3,
        limit: 1,
    };
    let terminal = run
        .kernel
        .publish(
            &run.claim,
            vec![
                SessionFactBody::BudgetExhausted {
                    turn_id: run.claim.turn_id().clone(),
                    dimension: BudgetDimension::GeneratedRecords,
                    consumed: 3,
                    limit: 1,
                },
                SessionFactBody::TurnTerminal {
                    turn_id: run.claim.turn_id().clone(),
                    outcome,
                },
            ],
        )
        .await
        .unwrap()
        .published();
    run.kernel
        .flush(&run.claim, terminal.last().unwrap().seq())
        .await
        .unwrap();
    let controls = store
        .read_controls(run.claim.session_id(), 0, 8)
        .await
        .unwrap();
    assert!(matches!(
        controls.records.last().unwrap().body(),
        AgentControlRecordBody::TurnBoundaryRecorded { .. }
    ));
    run.stop().await;
}

async fn prepared_session(
    id: &str,
    composition: Arc<DomainComposition>,
    handle: &DomainHandle<bool>,
) -> PreparedFreshSession {
    let mut draft = AgentSessionDraft::new(header(id), composition)
        .await
        .unwrap();
    draft
        .apply_domain_initial(&handle.propose(DomainRevision::new(0), &true).unwrap())
        .unwrap();
    draft.into_fresh()
}

#[tokio::test]
async fn an_open_step_can_end_after_its_generated_record_budget_is_full() {
    let run = DomainRun::start(
        Arc::new(MemoryStore::new()),
        TurnBudget::new(1_800_000, 64, 256, 2, 67_108_864).unwrap(),
    )
    .await;
    run.kernel
        .commit_domains(
            &run.claim,
            run.mutation("full", 1, true, vec![run.step("open")]),
        )
        .await
        .unwrap();
    run.kernel
        .finish_turn(
            &run.claim,
            &TurnOutcome::BudgetExceeded {
                dimension: BudgetDimension::GeneratedRecords,
                consumed: 3,
                limit: 2,
            },
        )
        .await
        .unwrap();
    run.stop().await;
}

#[derive(Debug)]
struct DomainClock(std::sync::atomic::AtomicU64);
impl Clock for DomainClock {
    fn now_ms(&self) -> u64 {
        self.0.load(Ordering::Acquire)
    }
}

#[tokio::test]
async fn elapsed_domain_work_is_rejected_while_atomic_ending_closes_the_open_step() {
    let clock = Arc::new(DomainClock(std::sync::atomic::AtomicU64::new(42)));
    let store = Arc::new(MemoryStore::new());
    let run = DomainRun::start_with_clock(
        store.clone(),
        TurnBudget::new(10, 64, 256, 65_536, 67_108_864).unwrap(),
        clock.clone(),
    )
    .await;
    run.kernel
        .commit_domains(
            &run.claim,
            run.mutation("before", 1, true, vec![run.step("open")]),
        )
        .await
        .unwrap();
    clock.0.store(52, Ordering::Release);
    assert!(matches!(
        run.kernel
            .commit_domains(&run.claim, run.mutation("late", 2, false, vec![]))
            .await,
        Err(TurnError::BudgetExceeded {
            dimension: BudgetDimension::Elapsed,
            consumed: 10,
            limit: 10
        })
    ));
    let outcome = TurnOutcome::BudgetExceeded {
        dimension: BudgetDimension::Elapsed,
        consumed: 10,
        limit: 10,
    };
    let terminal = run.kernel.finish_turn(&run.claim, &outcome).await.unwrap();
    let ending = store
        .read_facts(run.claim.session_id(), 2, 8)
        .await
        .unwrap();
    assert_eq!(ending.facts.len(), 3);
    assert_eq!(ending.durable_seq, terminal.seq());
    assert!(matches!(
        ending.facts[0].body(),
        SessionFactBody::StepEnded { .. }
    ));
    assert!(matches!(
        ending.facts[1].body(),
        SessionFactBody::BudgetExhausted {
            dimension: BudgetDimension::Elapsed,
            ..
        }
    ));
    assert_eq!(
        store
            .read_turn_domain_usage(run.claim.session_id(), run.claim.turn_id())
            .await
            .unwrap()
            .records,
        1
    );
    run.stop().await;
}

#[tokio::test]
async fn failed_direct_ending_keeps_business_closed_and_allows_retry_without_partial_closure() {
    let store = Arc::new(MemoryStore::new());
    let run = DomainRun::start(store.clone(), TurnBudget::default()).await;
    run.kernel
        .commit_domains(
            &run.claim,
            run.mutation("before", 1, true, vec![run.step("open")]),
        )
        .await
        .unwrap();
    store.fail_next_appends(1);
    assert!(matches!(
        run.kernel
            .finish_turn(&run.claim, &TurnOutcome::Completed)
            .await,
        Err(TurnError::Store(_))
    ));
    assert_eq!(
        store
            .read_facts(run.claim.session_id(), 0, 8)
            .await
            .unwrap()
            .facts
            .len(),
        2
    );
    assert!(matches!(
        run.kernel
            .commit_domains(&run.claim, run.mutation("after", 2, false, vec![]))
            .await,
        Err(TurnError::StaleClaim)
    ));
    let terminal = run
        .kernel
        .finish_turn(&run.claim, &TurnOutcome::Completed)
        .await
        .unwrap();
    assert_eq!(terminal.seq(), 4);
    run.stop().await;
}

#[tokio::test]
async fn lost_terminal_acknowledgements_resolve_direct_and_mailbox_endings() {
    for mailbox in [false, true] {
        let memory = Arc::new(MemoryStore::new());
        let store = Arc::new(FactReadRaceStore::new(memory.clone()));
        let mut run = DomainRun::start(store.clone(), TurnBudget::default()).await;
        if mailbox {
            run.kernel
                .finish_turn(&run.claim, &TurnOutcome::Completed)
                .await
                .unwrap();
            run.kernel
                .submit_message(SubmitMessage {
                    session: resume(&run.kernel, run.claim.session_id().clone()).await,
                    message: mailbox_message("terminal-mailbox"),
                    delivery: MessageDelivery::NextTurn,
                })
                .await
                .unwrap();
            run.claim = run
                .kernel
                .claim("domain-worker", CancellationToken::new())
                .await
                .unwrap()
                .unwrap();
        }
        store
            .fail_terminal_after_apply
            .store(true, Ordering::Release);
        let terminal = run
            .kernel
            .finish_turn(&run.claim, &TurnOutcome::Completed)
            .await
            .unwrap();
        assert_eq!(
            memory
                .read_turn_boundary(run.claim.session_id(), run.claim.turn_id())
                .await
                .unwrap()
                .terminal(),
            Some(terminal.as_ref())
        );
        assert_eq!(
            run.kernel
                .outcome(run.claim.session_id(), run.claim.turn_id())
                .await
                .unwrap(),
            Some(TurnOutcome::Completed)
        );
        assert!(
            memory
                .active_activation(run.claim.session_id())
                .await
                .unwrap()
                .is_none()
        );
        run.stop().await;
    }
}

#[tokio::test]
async fn unknown_terminal_result_keeps_release_and_canonical_outcome_query_available() {
    let memory = Arc::new(MemoryStore::new());
    let store = Arc::new(FactReadRaceStore::new(memory));
    let run = DomainRun::start(store.clone(), TurnBudget::default()).await;
    store
        .fail_terminal_after_apply
        .store(true, Ordering::Release);
    store
        .fail_terminal_lookup_after_apply
        .store(true, Ordering::Release);
    assert!(matches!(
        run.kernel
            .finish_turn(&run.claim, &TurnOutcome::Completed)
            .await,
        Err(TurnError::Store(_))
    ));
    assert!(matches!(
        run.kernel
            .commit_domains(&run.claim, run.mutation("forbidden", 1, true, vec![]))
            .await,
        Err(TurnError::Flush(_))
    ));
    store.terminal_lookup_fails.store(false, Ordering::Release);
    assert_eq!(
        run.kernel
            .outcome(run.claim.session_id(), run.claim.turn_id())
            .await
            .unwrap(),
        Some(TurnOutcome::Completed)
    );
    run.kernel.release(&run.claim).unwrap();
    drop(run.lease);
    assert!(run.kernel.shutdown(run.workers).await.is_err());
}

#[tokio::test]
async fn sqlite_domain_execution_reopens_with_a_new_codec_generation_and_exact_receipts() {
    let root = tempfile::tempdir().unwrap();
    let store = Arc::new(rsi_agent_store_sqlite::SqliteStore::open(root.path()).unwrap());
    let run = DomainRun::start(store.clone(), TurnBudget::default()).await;
    let receipt = run
        .kernel
        .commit_domains(
            &run.claim,
            run.mutation("sqlite-mixed", 1, true, vec![run.step("sql-step")]),
        )
        .await
        .unwrap();
    run.kernel
        .finish_turn(&run.claim, &TurnOutcome::Completed)
        .await
        .unwrap();
    let session = run.claim.session_id().clone();
    run.stop().await;
    drop(store);
    rsi_agent_store_sqlite::SqliteStore::verify(root.path()).unwrap();

    let reopened_store = Arc::new(rsi_agent_store_sqlite::SqliteStore::open(root.path()).unwrap());
    let (composition, handle) = domain_composition();
    let kernel = AgentKernel::recover_with_clock(reopened_store, composition, Arc::new(FixedClock))
        .await
        .unwrap();
    let states = kernel.domain_states(&session).await.unwrap();
    assert_eq!(states[0].revision, DomainRevision::new(2));
    assert!(handle.decode(&states[0].snapshot).unwrap());
    assert_eq!(
        kernel
            .domain_request(&session, receipt.commit().request_id().unwrap())
            .await
            .unwrap(),
        Some(receipt)
    );
    let workers = kernel.start_workers();
    kernel
        .submit(SubmitTurn {
            session: resume(&kernel, session.clone()).await,
            turn_id: TurnId::new("sql-cold-turn").unwrap(),
            text: "continue".into(),
            model: None,
            sandbox: None,
        })
        .await
        .unwrap();
    let lease = kernel.register("sql-cold-worker".into()).unwrap();
    let claim = kernel
        .claim("sql-cold-worker", CancellationToken::new())
        .await
        .unwrap()
        .unwrap();
    kernel
        .commit_domains(
            &claim,
            rsi_agent_turn_protocol::DomainMutation {
                request_id: rsi_agent_session_protocol::DomainRequestId::new("sql-cold-update")
                    .unwrap(),
                proposals: vec![handle.propose(DomainRevision::new(2), &false).unwrap()],
                facts: vec![],
            },
        )
        .await
        .unwrap();
    kernel
        .finish_turn(&claim, &TurnOutcome::Completed)
        .await
        .unwrap();
    assert_eq!(
        kernel.domain_states(&session).await.unwrap()[0].revision,
        DomainRevision::new(3)
    );
    drop(lease);
    kernel.shutdown(workers).await.unwrap();
    drop(kernel);
    rsi_agent_store_sqlite::SqliteStore::verify(root.path()).unwrap();
}

#[tokio::test]
async fn domain_byte_budget_charges_complete_canonical_envelopes() {
    use rsi_agent_session_protocol::{
        AgentControlRecord, DomainMutationSource, DomainRequestId, DomainStateCommit,
        DomainStateUpdate,
    };
    let (_, handle) = domain_composition();
    let proposal = handle.propose(DomainRevision::new(1), &true).unwrap();
    let commit = DomainStateCommit::new(
        Some(DomainRequestId::new("a").unwrap()),
        DomainMutationSource::Turn {
            turn_id: TurnId::new("domain-turn").unwrap(),
        },
        vec![
            DomainStateUpdate::new(proposal.expected_revision(), proposal.snapshot().clone())
                .unwrap(),
        ],
    )
    .unwrap();
    let expected_bytes = AgentControlRecord::new(
        2,
        42,
        AgentControlRecordBody::DomainStateCommitted { commit },
    )
    .unwrap()
    .encoded_len() as u64;
    let store = Arc::new(MemoryStore::new());
    let run = DomainRun::start(
        store.clone(),
        TurnBudget::new(1_800_000, 64, 256, 65_536, expected_bytes).unwrap(),
    )
    .await;
    run.kernel
        .commit_domains(&run.claim, run.mutation("a", 1, true, vec![]))
        .await
        .unwrap();
    assert_eq!(
        store
            .read_turn_domain_usage(run.claim.session_id(), run.claim.turn_id())
            .await
            .unwrap()
            .bytes,
        expected_bytes
    );
    assert!(
        matches!(run.kernel.commit_domains(&run.claim, run.mutation("b", 2, false, vec![])).await,
        Err(TurnError::BudgetExceeded { dimension: BudgetDimension::GeneratedRecordBytes, consumed, limit }) if consumed > expected_bytes && limit == expected_bytes)
    );
    run.stop().await;
}

#[tokio::test]
async fn cold_recovery_restores_turn_control_usage_and_excludes_command_controls() {
    use rsi_agent_session_protocol::{
        AgentControlRecord, DomainMutationSource, DomainRequestId, DomainStateCommit,
        DomainStateUpdate,
    };
    for turn_source in [false, true] {
        let store = Arc::new(MemoryStore::new());
        let run = DomainRun::start(
            store.clone(),
            TurnBudget::new(1_800_000, 64, 256, 1, 67_108_864).unwrap(),
        )
        .await;
        run.kernel
            .commit_domains(&run.claim, run.mutation("first", 1, true, vec![]))
            .await
            .unwrap();
        let claim = run.claim.clone();
        let proposal = run.handle.propose(DomainRevision::new(2), &false).unwrap();
        run.stop().await;
        let source = if turn_source {
            DomainMutationSource::Turn {
                turn_id: claim.turn_id().clone(),
            }
        } else {
            DomainMutationSource::Command {
                command: "fixture.toggle".into(),
            }
        };
        let commit = DomainStateCommit::new(
            Some(DomainRequestId::new("raw-second").unwrap()),
            source,
            vec![
                DomainStateUpdate::new(proposal.expected_revision(), proposal.snapshot().clone())
                    .unwrap(),
            ],
        )
        .unwrap();
        store
            .commit_agent(rsi_agent_store_protocol::AtomicAgentCommit {
                sessions: vec![rsi_agent_store_protocol::AtomicSessionAppend {
                    session_id: claim.session_id().clone(),
                    expected_fact_seq: 1,
                    expected_control_seq: 2,
                    header: None,
                    facts: vec![],
                    controls: vec![
                        AgentControlRecord::new(
                            3,
                            42,
                            AgentControlRecordBody::DomainStateCommitted { commit },
                        )
                        .unwrap(),
                    ],
                }],
                required_active_activations: vec![],
                quiescent_descendants_of: None,
            })
            .await
            .unwrap();
        let (composition, _) = domain_composition();
        let recovered =
            AgentKernel::recover_with_clock(store, composition, Arc::new(FixedClock)).await;
        if turn_source {
            assert!(matches!(
                recovered,
                Err(rsi_agent_kernel::KernelError::Invariant(_))
            ));
        } else {
            recovered.unwrap();
        }
    }
}

#[tokio::test]
async fn first_turn_and_message_atomically_commit_the_actual_draft_baseline() {
    for mailbox in [false, true] {
        let store = Arc::new(MemoryStore::new());
        let (composition, handle) = domain_composition();
        let initial = prepared_session("domain-fresh", composition.clone(), &handle).await;
        let expected_digest = initial.baseline().digest().to_owned();
        let kernel =
            AgentKernel::recover_with_clock(store.clone(), composition, Arc::new(FixedClock))
                .await
                .unwrap();
        let worker = kernel.start_workers();
        if mailbox {
            let receipt = kernel
                .submit_message(SubmitMessage {
                    session: SubmitSession::Fresh(initial),
                    message: mailbox_message("initial"),
                    delivery: MessageDelivery::NextTurn,
                })
                .await
                .unwrap();
            assert_eq!(receipt.accepted_control_seq, 2);
        } else {
            kernel
                .submit(SubmitTurn {
                    turn_id: client_turn_id(),
                    session: SubmitSession::Fresh(initial),
                    text: "hello".into(),
                    model: None,
                    sandbox: None,
                })
                .await
                .unwrap();
        }
        let session = SessionId::new("domain-fresh").unwrap();
        let states = store.read_domain_states(&session, None).await.unwrap();
        assert_eq!(states.states.len(), 1);
        assert_eq!(states.states[0].head.revision, DomainRevision::new(1));
        assert_eq!(
            states.states[0].snapshot.state().value(),
            &serde_json::Value::Bool(true)
        );
        let controls = store.read_controls(&session, 0, 8).await.unwrap();
        let AgentControlRecordBody::DomainStateCommitted { commit } = controls.records[0].body()
        else {
            panic!("first control lost its actual domain baseline");
        };
        assert_eq!(commit.request_sha256(), expected_digest);
        assert_eq!(
            store.header(&session).await.unwrap(),
            header("domain-fresh")
        );
        kernel.shutdown(worker).await.unwrap();
    }
}

#[tokio::test]
async fn cold_execution_rejects_missing_domain_codecs_while_history_remains_readable() {
    let store = Arc::new(MemoryStore::new());
    let (domains, handle) = domain_composition();
    let initial = prepared_session("domain-cold", domains.clone(), &handle).await;
    let kernel = AgentKernel::recover_with_clock(store.clone(), domains, Arc::new(FixedClock))
        .await
        .unwrap();
    let worker = kernel.start_workers();
    kernel
        .submit_message(SubmitMessage {
            session: SubmitSession::Fresh(initial),
            message: mailbox_message("initial"),
            delivery: MessageDelivery::NextTurn,
        })
        .await
        .unwrap();
    kernel.shutdown(worker).await.unwrap();
    let reopened =
        AgentKernel::recover_with_clock(store.clone(), composition(), Arc::new(FixedClock))
            .await
            .unwrap();
    let session = SessionId::new("domain-cold").unwrap();
    assert!(matches!(
        reopened.prepare_resume(&session).await,
        Err(TurnError::Composition(_))
    ));
    assert_eq!(
        store
            .read_domain_states(&session, None)
            .await
            .unwrap()
            .states
            .len(),
        1
    );
    assert_eq!(
        reopened.session_header(&session).await.unwrap(),
        header("domain-cold")
    );
}

#[tokio::test]
async fn first_submission_retries_compare_actual_baselines_on_resident_and_cold_paths() {
    for mailbox in [false, true] {
        let turn_id = client_turn_id();
        let store = Arc::new(MemoryStore::new());
        let (composition, handle) = domain_composition();
        let kernel = AgentKernel::recover_with_clock(
            store.clone(),
            composition.clone(),
            Arc::new(FixedClock),
        )
        .await
        .unwrap();
        let worker = kernel.start_workers();
        let submit = |kernel: AgentKernel, initial: PreparedFreshSession| {
            let turn_id = turn_id.clone();
            async move {
                if mailbox {
                    kernel
                        .submit_message(SubmitMessage {
                            session: SubmitSession::Fresh(initial),
                            message: mailbox_message("same-id"),
                            delivery: MessageDelivery::NextTurn,
                        })
                        .await
                        .map(|_| ())
                } else {
                    kernel
                        .submit(SubmitTurn {
                            turn_id,
                            session: SubmitSession::Fresh(initial),
                            text: "same request".into(),
                            model: None,
                            sandbox: None,
                        })
                        .await
                        .map(|_| ())
                }
            }
        };
        submit(
            kernel.clone(),
            prepared_session("domain-retry", composition.clone(), &handle).await,
        )
        .await
        .unwrap();
        let mut current = kernel;
        let mut worker = worker;
        for cold in [false, true] {
            if cold {
                current.shutdown(worker).await.unwrap();
                current = AgentKernel::recover_with_clock(
                    store.clone(),
                    composition.clone(),
                    Arc::new(FixedClock),
                )
                .await
                .unwrap();
                worker = current.start_workers();
            }
            let wrong =
                PreparedFreshSession::new(header("domain-retry"), composition.pin.clone()).unwrap();
            assert!(
                submit(current.clone(), wrong).await.is_err(),
                "same Header/request identity cannot hide a changed initial state"
            );
            submit(
                current.clone(),
                prepared_session("domain-retry", composition.clone(), &handle).await,
            )
            .await
            .unwrap();
        }
        current.shutdown(worker).await.unwrap();
    }
}

#[tokio::test]
#[allow(clippy::too_many_lines)] // One durable fork scenario retains the exact parent terminal and later idle mutation.
async fn fork_domain_baseline_uses_terminal_state_and_none_uses_target_defaults() {
    let store = Arc::new(MemoryStore::new());
    let (composition, handle) = domain_composition();
    let parent = SessionId::new("domain-parent").unwrap();
    let kernel =
        AgentKernel::recover_with_clock(store.clone(), composition.clone(), Arc::new(FixedClock))
            .await
            .unwrap();
    let worker = kernel.start_workers();
    kernel
        .submit_message(SubmitMessage {
            session: SubmitSession::Fresh(
                prepared_session(parent.as_str(), composition.clone(), &handle).await,
            ),
            message: mailbox_message("first"),
            delivery: MessageDelivery::NextTurn,
        })
        .await
        .unwrap();
    let lease = kernel.register("domain-executor".into()).unwrap();
    let first = kernel
        .claim("domain-executor", CancellationToken::new())
        .await
        .unwrap()
        .unwrap();
    kernel
        .finish_turn(&first, &TurnOutcome::Completed)
        .await
        .unwrap();
    drop(lease);
    kernel.shutdown(worker).await.unwrap();

    // An idle command changes the current state after the inherited terminal horizon.
    let page = store.read_domain_states(&parent, None).await.unwrap();
    let update = rsi_agent_session_protocol::DomainStateUpdate::new(
        page.states[0].head.revision,
        rsi_agent_session_protocol::DomainSnapshot::new(
            page.states[0].snapshot.identity().clone(),
            rsi_agent_session_protocol::DomainStateValue::new(false.into()).unwrap(),
        ),
    )
    .unwrap();
    let commit = rsi_agent_session_protocol::DomainStateCommit::new(
        Some(rsi_agent_session_protocol::DomainRequestId::new("idle-toggle").unwrap()),
        rsi_agent_session_protocol::DomainMutationSource::Command {
            command: "fixture.toggle".into(),
        },
        vec![update],
    )
    .unwrap();
    store
        .commit_agent(rsi_agent_store_protocol::AtomicAgentCommit {
            sessions: vec![rsi_agent_store_protocol::AtomicSessionAppend {
                session_id: parent.clone(),
                expected_fact_seq: page.durable_fact_seq,
                expected_control_seq: page.durable_control_seq,
                header: None,
                facts: vec![],
                controls: vec![
                    rsi_agent_session_protocol::AgentControlRecord::new(
                        page.durable_control_seq + 1,
                        42,
                        AgentControlRecordBody::DomainStateCommitted { commit },
                    )
                    .unwrap(),
                ],
            }],
            required_active_activations: vec![],
            quiescent_descendants_of: None,
        })
        .await
        .unwrap();
    let reopened =
        AgentKernel::recover_with_clock(store.clone(), composition, Arc::new(FixedClock))
            .await
            .unwrap();
    let worker = reopened.start_workers();
    reopened
        .submit_message(SubmitMessage {
            session: resume(&reopened, parent).await,
            message: mailbox_message("invoking"),
            delivery: MessageDelivery::NextTurn,
        })
        .await
        .unwrap();
    let lease = reopened.register("domain-forker".into()).unwrap();
    let invoking = reopened
        .claim("domain-forker", CancellationToken::new())
        .await
        .unwrap()
        .unwrap();
    for (name, selection, expected) in [
        ("all", ForkTurnSelection::All, true),
        ("none", ForkTurnSelection::None, false),
    ] {
        let child = SessionId::new(format!("domain-child-{name}")).unwrap();
        let request = || SpawnAgentRequest {
            cancellation: CancellationToken::new(),
            caller: reopened.agent_caller(&invoking).unwrap(),
            child_session_id: child.clone(),
            task_name: name.into(),
            message_id: MessageId::new(format!("child-{name}")).unwrap(),
            message: "inherit once".into(),
            fork_turns: selection.clone(),
        };
        let first = reopened.spawn_agent(request()).await.unwrap();
        let retry = reopened.spawn_agent(request()).await.unwrap();
        assert_eq!(
            first, retry,
            "retry must reuse the immutable child baseline"
        );
        let inherited = store.read_domain_states(&child, None).await.unwrap();
        assert_eq!(inherited.states.len(), 1);
        assert_eq!(inherited.states[0].head.revision, DomainRevision::new(1));
        assert_eq!(
            inherited.states[0].snapshot.state().value(),
            &serde_json::Value::Bool(expected),
            "{name} selected the wrong domain horizon"
        );
    }
    reopened
        .finish_turn(&invoking, &TurnOutcome::Completed)
        .await
        .unwrap();
    drop(lease);
    reopened.shutdown(worker).await.unwrap();
}
