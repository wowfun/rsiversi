use super::*;

#[tokio::test(start_paused = true)]
async fn fresh_submission_returns_only_after_its_acceptance_is_durable() {
    let store = Arc::new(MemoryStore::new());
    let kernel = kernel(store.clone()).await;
    let worker = kernel.start_write_behind();
    let submitted = submit(&kernel, "session-lazy", "hello").await;
    let session = submitted.session_id.clone();
    assert_eq!(
        store.header(&session).await.unwrap(),
        header("session-lazy")
    );

    let mut observation = kernel.observe(&session, 0).await.unwrap();
    assert!(matches!(
        observation.next().await.unwrap().unwrap(),
        TurnUpdate::Fact { durable_seq: 1, .. }
    ));
    assert_eq!(
        store.read_facts(&session, 0, 8).await.unwrap().durable_seq,
        1
    );
    kernel.shutdown(worker).await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn submission_without_a_running_write_behind_worker_fails_within_a_bound() {
    let store = Arc::new(MemoryStore::new());
    let kernel = kernel(store).await;
    let result = tokio::time::timeout(
        std::time::Duration::from_secs(61),
        kernel.submit(SubmitTurn {
            turn_id: TurnId::new("turn-no-worker").unwrap(),
            session: fresh(header("session-no-worker")),
            text: "must not wait forever".into(),
            model: None,
            sandbox: None,
        }),
    )
    .await
    .expect("the Kernel must bound a durability wait without its worker");
    assert!(
        matches!(result, Err(TurnError::Flush(ref message)) if message.contains("timed out")),
        "unexpected result: {result:?}"
    );

    let worker = kernel.start_write_behind();
    kernel.shutdown(worker).await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn permanent_flush_failure_rejects_later_mailbox_submission() {
    let memory = Arc::new(MemoryStore::new());
    let store = Arc::new(FactReadRaceStore::new(memory.clone()));
    let kernel = SessionKernel::recover_with_clock(
        store.clone() as Arc<dyn SessionStore>,
        composition(),
        Arc::new(FixedClock),
    )
    .await
    .unwrap();
    let worker = kernel.start_write_behind();
    let session_id = SessionId::new("session-mailbox-flush-latch").unwrap();
    let turn_id = TurnId::new("turn-mailbox-flush-latch").unwrap();
    kernel
        .submit(SubmitTurn {
            turn_id: turn_id.clone(),
            session: fresh(header(session_id.as_str())),
            text: "create a resident session".into(),
            model: None,
            sandbox: None,
        })
        .await
        .unwrap();
    let _lease = kernel
        .register("executor-mailbox-flush-latch".into())
        .unwrap();
    let claim = kernel
        .claim("executor-mailbox-flush-latch", CancellationToken::new())
        .await
        .unwrap()
        .unwrap();
    let published = kernel
        .publish(
            &claim,
            vec![SessionFactBody::ModelIntent {
                turn_id,
                effect_id: EffectId::new("effect-mailbox-flush-latch").unwrap(),
                snapshot: snapshot(),
            }],
        )
        .await
        .unwrap()
        .published();
    store.fail_appends_for(session_id.clone());
    let flush_error = kernel.flush(&claim, published[0].seq()).await.unwrap_err();
    let TurnError::Flush(expected) = flush_error else {
        panic!("expected the injected append failure to latch: {flush_error:?}");
    };

    let result = kernel
        .submit_message(SubmitMessage {
            session: resume(&kernel, session_id.clone()).await,
            message: mailbox_message("message-after-flush-latch"),
            target: MessageTarget::NextStep,
            wake_required: false,
        })
        .await;

    assert_eq!(result, Err(TurnError::Flush(expected)));
    assert!(
        memory
            .read_agent_mailbox(&session_id, None)
            .await
            .unwrap()
            .pending
            .is_empty(),
        "a mailbox message must not become durable behind a permanently failed Fact suffix"
    );
    assert!(kernel.shutdown(worker).await.is_err());
}

#[tokio::test]
async fn blocked_retry_does_not_serialize_an_independent_session_submission() {
    let memory = Arc::new(MemoryStore::new());
    let session_id = SessionId::new("session-blocked-retry").unwrap();
    let turn_id = TurnId::new("turn-blocked-retry").unwrap();
    memory
        .append(AppendBatch {
            session_id: session_id.clone(),
            expected_seq: 0,
            header: Some(header(session_id.as_str())),
            facts: vec![
                SessionFact::new(
                    1,
                    1,
                    SessionFactBody::TurnAccepted {
                        turn_id: turn_id.clone(),
                        text: "retry body".into(),
                        model: None,
                        sandbox: SandboxMode::WorkspaceWrite,
                        require_approval: false,
                    },
                )
                .unwrap(),
                SessionFact::new(
                    2,
                    2,
                    SessionFactBody::TurnTerminal {
                        turn_id: turn_id.clone(),
                        outcome: TurnOutcome::Completed,
                    },
                )
                .unwrap(),
            ],
        })
        .await
        .unwrap();
    let store = Arc::new(FactReadRaceStore::new(memory));
    let kernel = SessionKernel::recover_with_clock(
        store.clone() as Arc<dyn SessionStore>,
        composition(),
        Arc::new(FixedClock),
    )
    .await
    .unwrap();
    let worker = kernel.start_write_behind();
    store.block_header_reads_for(session_id.clone());
    let blocked = tokio::spawn({
        let kernel = kernel.clone();
        async move {
            kernel
                .submit(SubmitTurn {
                    turn_id,
                    session: fresh(header(session_id.as_str())),
                    text: "retry body".into(),
                    model: None,
                    sandbox: None,
                })
                .await
        }
    });
    tokio::time::timeout(std::time::Duration::from_secs(1), async {
        while store.header_read_attempts() == 0 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("retry must reach the blocked Store read");

    let independent = tokio::time::timeout(
        std::time::Duration::from_secs(1),
        submit(&kernel, "session-independent", "independent"),
    )
    .await;
    store.release_blocked_header_reads();
    assert!(blocked.await.unwrap().is_ok());
    assert!(
        independent.is_ok(),
        "an unrelated Session was serialized behind blocked Store I/O"
    );
    kernel.shutdown(worker).await.unwrap();
}

#[tokio::test]
async fn caller_turn_id_is_idempotent_live_and_after_restart_but_body_changes_conflict() {
    let store = Arc::new(MemoryStore::new());
    let initial = kernel(Arc::clone(&store)).await;
    let worker = initial.start_write_behind();
    let turn_id = TurnId::new("caller-retry-turn").unwrap();
    let request = || SubmitTurn {
        turn_id: turn_id.clone(),
        session: fresh(header("session-idempotent-submit")),
        text: "same canonical body".into(),
        model: None,
        sandbox: None,
    };

    let first = initial.submit(request()).await.unwrap();
    assert_eq!(initial.submit(request()).await.unwrap(), first);
    assert!(matches!(
        initial
            .submit(SubmitTurn {
                text: "different body".into(),
                ..request()
            })
            .await,
        Err(TurnError::SubmissionConflict { session, turn })
            if session == "session-idempotent-submit" && turn == "caller-retry-turn"
    ));
    initial.shutdown(worker).await.unwrap();

    let restarted = kernel(Arc::clone(&store)).await;
    let restarted_worker = restarted.start_write_behind();
    assert_eq!(restarted.submit(request()).await.unwrap(), first);
    let stored = store
        .read_turn_boundary(&first.session_id, &turn_id)
        .await
        .unwrap();
    assert_eq!(stored.accepted_seq(), first.accepted_seq);
    restarted.shutdown(restarted_worker).await.unwrap();
}

#[tokio::test]
async fn indexed_turn_boundary_reads_share_the_process_store_read_admission() {
    let memory = Arc::new(MemoryStore::new());
    append_terminal_history(&memory, "session-boundary-admission-a", 1).await;
    append_terminal_history(&memory, "session-boundary-admission-b", 1).await;
    let store = Arc::new(FactReadRaceStore::new(memory));
    let kernel = SessionKernel::recover_with_clock_and_limits(
        store.clone() as Arc<dyn SessionStore>,
        composition(),
        Arc::new(FixedClock),
        KernelLimits {
            maximum_store_read_bytes: MAXIMUM_SESSION_FACT_BYTES,
            ..KernelLimits::default()
        },
    )
    .await
    .unwrap();
    store.block_turn_boundary_reads();
    let turn = TurnId::new("turn-history-0").unwrap();
    let first = tokio::spawn({
        let kernel = kernel.clone();
        let turn = turn.clone();
        async move {
            kernel
                .outcome(
                    &SessionId::new("session-boundary-admission-a").unwrap(),
                    &turn,
                )
                .await
        }
    });
    store.wait_for_turn_boundary_attempts(1).await;
    let second = tokio::spawn({
        let kernel = kernel.clone();
        async move {
            kernel
                .outcome(
                    &SessionId::new("session-boundary-admission-b").unwrap(),
                    &TurnId::new("turn-history-0").unwrap(),
                )
                .await
        }
    });
    tokio::task::yield_now().await;
    assert_eq!(
        store.turn_boundary_read_attempts(),
        1,
        "the second maximum-weight boundary read bypassed Store-read admission"
    );
    store.release_one_turn_boundary_read();
    store.wait_for_turn_boundary_attempts(2).await;
    store.release_one_turn_boundary_read();
    assert_eq!(first.await.unwrap().unwrap(), Some(TurnOutcome::Completed));
    assert_eq!(second.await.unwrap().unwrap(), Some(TurnOutcome::Completed));
}

#[tokio::test]
async fn caller_turn_id_retry_after_terminal_pruning_does_not_reexecute() {
    let store = Arc::new(MemoryStore::new());
    let kernel = kernel(Arc::clone(&store)).await;
    let worker = kernel.start_write_behind();
    let turn_id = TurnId::new("pruned-retry-turn").unwrap();
    let request = || SubmitTurn {
        turn_id: turn_id.clone(),
        session: fresh(header("session-pruned-retry")),
        text: "retried body".into(),
        model: None,
        sandbox: None,
    };

    let first = kernel.submit(request()).await.unwrap();
    // A second live turn keeps the session resident while the first turn's
    // durable terminal entry is pruned from the in-memory turn index.
    let keeper = kernel
        .submit(SubmitTurn {
            turn_id: TurnId::new("resident-keeper-turn").unwrap(),
            session: resume(&kernel, first.session_id.clone()).await,
            text: "keeper".into(),
            model: None,
            sandbox: None,
        })
        .await
        .unwrap();
    let _lease = kernel.register("executor-pruned".into()).unwrap();
    let claim = kernel
        .claim("executor-pruned", CancellationToken::new())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(claim.turn_id(), &first.turn_id);
    let terminal = kernel
        .publish(
            &claim,
            vec![SessionFactBody::TurnTerminal {
                turn_id: first.turn_id.clone(),
                outcome: TurnOutcome::Completed,
            }],
        )
        .await
        .unwrap()
        .published();
    kernel
        .flush(&claim, terminal.last().unwrap().seq())
        .await
        .unwrap();

    // The terminal turn is pruned while the session stays resident; a retry
    // must resolve against the Store instead of re-accepting the turn.
    assert_eq!(kernel.submit(request()).await.unwrap(), first);

    // The only claimable turn is the keeper: the retry did not re-enqueue.
    let next = kernel
        .claim("executor-pruned", CancellationToken::new())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(next.turn_id(), &keeper.turn_id);
    kernel.shutdown(worker).await.unwrap();
}

#[tokio::test]
async fn resident_session_keeps_its_pin_while_a_new_session_uses_the_new_generation() {
    let store = Arc::new(MemoryStore::new());
    let composition = Arc::new(MutableComposition::new('a'));
    let store_contract: Arc<dyn SessionStore> = store;
    let composition_contract: Arc<dyn AgentComposition> = composition.clone();
    let kernel = SessionKernel::recover_with_clock(
        store_contract,
        composition_contract,
        Arc::new(FixedClock),
    )
    .await
    .unwrap();
    let worker = kernel.start_write_behind();

    let first_header = header("session-generation-a");
    let first_pin = composition
        .pin(first_header.agent_preset_id())
        .await
        .unwrap();
    let first_tools = first_pin.tools();
    kernel
        .submit(SubmitTurn {
            turn_id: client_turn_id(),
            session: SubmitSession::Fresh(
                PreparedFreshSession::new(first_header, first_pin).unwrap(),
            ),
            text: "first A turn".into(),
            model: None,
            sandbox: None,
        })
        .await
        .unwrap();
    composition.select_digest('b');
    kernel
        .submit(SubmitTurn {
            turn_id: client_turn_id(),
            session: resume(&kernel, SessionId::new("session-generation-a").unwrap()).await,
            text: "resident still A".into(),
            model: None,
            sandbox: None,
        })
        .await
        .unwrap();
    let second_header = header("session-generation-b");
    let second_pin = composition
        .pin(second_header.agent_preset_id())
        .await
        .unwrap();
    let second_tools = second_pin.tools();
    kernel
        .submit(SubmitTurn {
            turn_id: client_turn_id(),
            session: SubmitSession::Fresh(
                PreparedFreshSession::new(second_header, second_pin).unwrap(),
            ),
            text: "new session B".into(),
            model: None,
            sandbox: None,
        })
        .await
        .unwrap();

    let _first_lease = kernel.register("executor-a".into()).unwrap();
    let _second_lease = kernel.register("executor-b".into()).unwrap();
    let first_claim = kernel
        .claim("executor-a", CancellationToken::new())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(first_claim.session_id().as_str(), "session-generation-a");
    let first_claim_pin = kernel.composition(&first_claim).unwrap();
    assert_eq!(first_claim_pin.source_digest(), "a".repeat(64));
    assert!(Arc::ptr_eq(&first_claim_pin.tools(), &first_tools));
    let second_claim = kernel
        .claim("executor-b", CancellationToken::new())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(second_claim.session_id().as_str(), "session-generation-b");
    let second_claim_pin = kernel.composition(&second_claim).unwrap();
    assert_eq!(second_claim_pin.source_digest(), "b".repeat(64));
    assert!(Arc::ptr_eq(&second_claim_pin.tools(), &second_tools));
    assert!(!Arc::ptr_eq(
        &first_claim_pin.tools(),
        &second_claim_pin.tools()
    ));
    kernel.shutdown(worker).await.unwrap();
}

#[tokio::test]
async fn shutdown_releases_resident_generation_pins_while_service_handles_escape() {
    let store = Arc::new(MemoryStore::new());
    let kernel = kernel(store).await;
    let escaped_kernel = kernel.clone();
    let worker = kernel.start_write_behind();
    let session_header = header("session-shutdown-pin");
    let drops = Arc::new(AtomicUsize::new(0));
    let pin = AgentCompositionPin::new(
        session_header.agent_preset_id().clone(),
        "a".repeat(64),
        Arc::new(EmptyTools),
        Arc::new(DropOwner(Arc::clone(&drops))),
    )
    .unwrap();

    kernel
        .submit(SubmitTurn {
            turn_id: client_turn_id(),
            session: SubmitSession::Fresh(PreparedFreshSession::new(session_header, pin).unwrap()),
            text: "keep the resident generation pinned".into(),
            model: None,
            sandbox: None,
        })
        .await
        .unwrap();
    assert_eq!(drops.load(Ordering::Acquire), 0);

    kernel.shutdown(worker).await.unwrap();

    assert_eq!(
        drops.load(Ordering::Acquire),
        1,
        "shutdown must quiesce resident generation ownership even when a service handle escapes"
    );
    assert!(matches!(
        escaped_kernel
            .prepare_resume(&SessionId::new("session-shutdown-pin").unwrap())
            .await,
        Err(TurnError::ShuttingDown)
    ));
}

#[tokio::test]
async fn unavailable_cold_preset_fails_before_fact_log_materialization() {
    let memory = Arc::new(MemoryStore::new());
    append_terminal_history(&memory, "session-unavailable-preset", 1).await;
    let store = Arc::new(FactReadRaceStore::new(memory.clone()));
    let composition = Arc::new(MutableComposition::new('a'));
    let store_contract: Arc<dyn SessionStore> = store.clone();
    let composition_contract: Arc<dyn AgentComposition> = composition.clone();
    let kernel = SessionKernel::recover_with_clock(
        store_contract,
        composition_contract,
        Arc::new(FixedClock),
    )
    .await
    .unwrap();
    composition.set_unavailable();
    store.reset_header_read_attempts();
    store.reset_read_attempts();
    store.reset_open_turn_read_attempts();

    assert!(matches!(
        kernel
            .prepare_resume(&SessionId::new("session-unavailable-preset").unwrap())
            .await,
        Err(TurnError::Composition(_))
    ));
    assert_eq!(composition.calls.load(Ordering::Acquire), 1);
    assert_eq!(store.header_read_attempts(), 1);
    assert_eq!(store.read_attempts(), 0);
    assert_eq!(store.open_turn_read_attempts(), 0);
    assert_eq!(
        memory
            .read_facts(&SessionId::new("session-unavailable-preset").unwrap(), 0, 8,)
            .await
            .unwrap()
            .facts
            .len(),
        2
    );
}

#[tokio::test]
async fn dropping_an_unsubmitted_cold_resume_releases_its_pin_without_hydration() {
    let memory = Arc::new(MemoryStore::new());
    append_terminal_history(&memory, "session-dropped-resume-token", 1).await;
    let store = Arc::new(FactReadRaceStore::new(memory));
    let drops = Arc::new(AtomicUsize::new(0));
    let composition = Arc::new(DropTrackingComposition {
        calls: AtomicUsize::new(0),
        drops: Arc::clone(&drops),
    });
    let store_contract: Arc<dyn SessionStore> = store.clone();
    let composition_contract: Arc<dyn AgentComposition> = composition.clone();
    let kernel = SessionKernel::recover_with_clock(
        store_contract,
        composition_contract,
        Arc::new(FixedClock),
    )
    .await
    .unwrap();
    store.reset_read_attempts();
    store.reset_open_turn_read_attempts();

    let prepared = kernel
        .prepare_resume(&SessionId::new("session-dropped-resume-token").unwrap())
        .await
        .unwrap();
    assert_eq!(composition.calls.load(Ordering::Acquire), 1);
    assert_eq!(drops.load(Ordering::Acquire), 0);
    assert_eq!(store.read_attempts(), 0);
    assert_eq!(store.open_turn_read_attempts(), 0);

    drop(prepared);
    assert_eq!(drops.load(Ordering::Acquire), 1);
}

#[tokio::test]
async fn dropping_an_unsubmitted_fresh_draft_releases_its_pin_without_store_state() {
    let store = MemoryStore::new();
    let drops = Arc::new(AtomicUsize::new(0));
    let composition: Arc<dyn AgentComposition> = Arc::new(DropTrackingComposition {
        calls: AtomicUsize::new(0),
        drops: Arc::clone(&drops),
    });
    let session_header = header("session-dropped-fresh-draft");
    let session_id = session_header.session_id().clone();

    let draft = AgentSessionDraft::new(session_header, composition)
        .await
        .unwrap();
    assert_eq!(drops.load(Ordering::Acquire), 0);

    drop(draft);

    assert_eq!(drops.load(Ordering::Acquire), 1);
    assert!(matches!(
        store.header(&session_id).await,
        Err(StoreError::NotFound(missing)) if missing == session_id.to_string()
    ));
}

#[tokio::test]
async fn resume_token_from_another_kernel_is_rejected_and_releases_its_pin() {
    let source_store = Arc::new(MemoryStore::new());
    append_terminal_history(&source_store, "session-foreign-resume-token", 1).await;
    let drops = Arc::new(AtomicUsize::new(0));
    let composition = Arc::new(DropTrackingComposition {
        calls: AtomicUsize::new(0),
        drops: Arc::clone(&drops),
    });
    let source_store_contract: Arc<dyn SessionStore> = source_store;
    let composition_contract: Arc<dyn AgentComposition> = composition;
    let source = SessionKernel::recover_with_clock(
        source_store_contract,
        composition_contract,
        Arc::new(FixedClock),
    )
    .await
    .unwrap();
    let prepared = source
        .prepare_resume(&SessionId::new("session-foreign-resume-token").unwrap())
        .await
        .unwrap();
    let target = kernel(Arc::new(MemoryStore::new())).await;

    let error = target
        .submit(SubmitTurn {
            turn_id: client_turn_id(),
            session: SubmitSession::Resume(prepared),
            text: "must not cross Kernel authority".into(),
            model: None,
            sandbox: None,
        })
        .await
        .unwrap_err();
    assert!(matches!(
        error,
        TurnError::Invalid(message) if message.contains("different Turn service")
    ));
    assert_eq!(drops.load(Ordering::Acquire), 1);
}

#[tokio::test]
async fn resume_preparation_uses_the_resident_pin_when_the_source_is_unavailable() {
    let store = Arc::new(MemoryStore::new());
    let composition = Arc::new(MutableComposition::new('a'));
    let store_contract: Arc<dyn SessionStore> = store;
    let composition_contract: Arc<dyn AgentComposition> = composition.clone();
    let kernel = SessionKernel::recover_with_clock(
        store_contract,
        composition_contract,
        Arc::new(FixedClock),
    )
    .await
    .unwrap();
    let worker = kernel.start_write_behind();
    let session_header = header("session-resident-damaged-source");
    let pin = composition
        .pin(session_header.agent_preset_id())
        .await
        .unwrap();
    kernel
        .submit(SubmitTurn {
            turn_id: client_turn_id(),
            session: SubmitSession::Fresh(PreparedFreshSession::new(session_header, pin).unwrap()),
            text: "resident A".into(),
            model: None,
            sandbox: None,
        })
        .await
        .unwrap();
    composition.set_unavailable();

    let prepared = kernel
        .prepare_resume(&SessionId::new("session-resident-damaged-source").unwrap())
        .await
        .unwrap();
    kernel
        .submit(SubmitTurn {
            turn_id: client_turn_id(),
            session: SubmitSession::Resume(prepared),
            text: "resident A remains available".into(),
            model: None,
            sandbox: None,
        })
        .await
        .unwrap();
    assert_eq!(composition.calls.load(Ordering::Acquire), 1);
    kernel.shutdown(worker).await.unwrap();
}

#[tokio::test]
async fn cold_resume_after_process_restart_pins_the_current_generation() {
    let store = Arc::new(MemoryStore::new());
    append_terminal_history(&store, "session-cold-generation-b", 1).await;
    let composition = Arc::new(MutableComposition::new('a'));
    composition.select_digest('b');
    let store_contract: Arc<dyn SessionStore> = store;
    let composition_contract: Arc<dyn AgentComposition> = composition;
    let kernel = SessionKernel::recover_with_clock(
        store_contract,
        composition_contract,
        Arc::new(FixedClock),
    )
    .await
    .unwrap();
    let worker = kernel.start_write_behind();

    kernel
        .submit(SubmitTurn {
            turn_id: client_turn_id(),
            session: resume(
                &kernel,
                SessionId::new("session-cold-generation-b").unwrap(),
            )
            .await,
            text: "cold session uses current B".into(),
            model: None,
            sandbox: None,
        })
        .await
        .unwrap();
    let _lease = kernel.register("executor-cold-b".into()).unwrap();
    let claim = kernel
        .claim("executor-cold-b", CancellationToken::new())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        kernel.composition(&claim).unwrap().source_digest(),
        "b".repeat(64)
    );
    kernel.shutdown(worker).await.unwrap();
}

#[tokio::test]
async fn resume_after_idle_eviction_pins_the_current_generation() {
    let store = Arc::new(MemoryStore::new());
    let composition = Arc::new(MutableComposition::new('a'));
    let store_contract: Arc<dyn SessionStore> = store;
    let composition_contract: Arc<dyn AgentComposition> = composition.clone();
    let kernel = SessionKernel::recover_with_clock(
        store_contract,
        composition_contract,
        Arc::new(FixedClock),
    )
    .await
    .unwrap();
    let worker = kernel.start_write_behind();
    let session_header = header("session-evicted-generation-b");
    let pin = composition
        .pin(session_header.agent_preset_id())
        .await
        .unwrap();
    let first = kernel
        .submit(SubmitTurn {
            turn_id: client_turn_id(),
            session: SubmitSession::Fresh(PreparedFreshSession::new(session_header, pin).unwrap()),
            text: "generation A".into(),
            model: None,
            sandbox: None,
        })
        .await
        .unwrap();
    let _lease = kernel.register("executor-eviction".into()).unwrap();
    let first_claim = kernel
        .claim("executor-eviction", CancellationToken::new())
        .await
        .unwrap()
        .unwrap();
    let terminal = kernel
        .publish(
            &first_claim,
            vec![SessionFactBody::TurnTerminal {
                turn_id: first.turn_id,
                outcome: TurnOutcome::Completed,
            }],
        )
        .await
        .unwrap()
        .published();
    kernel
        .flush(&first_claim, terminal.last().unwrap().seq())
        .await
        .unwrap();

    composition.select_digest('b');
    let prepared = kernel.prepare_resume(&first.session_id).await.unwrap();
    kernel
        .submit(SubmitTurn {
            turn_id: client_turn_id(),
            session: SubmitSession::Resume(prepared),
            text: "generation B".into(),
            model: None,
            sandbox: None,
        })
        .await
        .unwrap();
    let second_claim = kernel
        .claim("executor-eviction", CancellationToken::new())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        kernel.composition(&second_claim).unwrap().source_digest(),
        "b".repeat(64)
    );
    assert_eq!(composition.calls.load(Ordering::Acquire), 2);
    kernel.shutdown(worker).await.unwrap();
}

#[tokio::test]
async fn cold_composition_failure_has_a_utf8_safe_bounded_diagnostic() {
    let memory = Arc::new(MemoryStore::new());
    append_terminal_history(&memory, "session-unbounded-composition", 1).await;
    let store: Arc<dyn SessionStore> = memory;
    let composition: Arc<dyn AgentComposition> = Arc::new(UnboundedDiagnosticComposition);
    let kernel = SessionKernel::recover_with_clock(store, composition, Arc::new(FixedClock))
        .await
        .unwrap();

    let Err(TurnError::Composition(message)) = kernel
        .prepare_resume(&SessionId::new("session-unbounded-composition").unwrap())
        .await
    else {
        panic!("cold composition failure must preserve its typed error class");
    };
    assert!(message.len() <= MAXIMUM_AGENT_DIAGNOSTIC_BYTES);
    assert!(std::str::from_utf8(message.as_bytes()).is_ok());
}

#[tokio::test]
async fn store_read_failure_has_a_utf8_safe_bounded_turn_diagnostic() {
    let memory = Arc::new(MemoryStore::new());
    let store = Arc::new(FactReadRaceStore::new(memory));
    let store_contract: Arc<dyn SessionStore> = store.clone();
    let kernel =
        SessionKernel::recover_with_clock(store_contract, composition(), Arc::new(FixedClock))
            .await
            .unwrap();
    store.fail_next_read(format!(
        "{}\0tail",
        "界".repeat(MAXIMUM_AGENT_DIAGNOSTIC_BYTES)
    ));

    let Err(TurnError::Store(message)) = kernel
        .observe(&SessionId::new("session-store-diagnostic").unwrap(), 0)
        .await
    else {
        panic!("ordinary Store read failure must not be classified as a durability flush");
    };
    assert!(message.len() <= MAXIMUM_AGENT_DIAGNOSTIC_BYTES);
    assert!(std::str::from_utf8(message.as_bytes()).is_ok());
    assert!(!message.contains('\0'));
}

#[tokio::test(start_paused = true)]
async fn explicit_effect_flush_waits_through_transient_failure_without_reordering() {
    let store = Arc::new(MemoryStore::new());
    let kernel = kernel(store.clone()).await;
    let worker = kernel.start_write_behind();
    let submitted = submit(&kernel, "session-retry", "hello").await;
    let _lease = kernel.register("executor".into()).unwrap();
    let claim = kernel
        .claim("executor", CancellationToken::new())
        .await
        .unwrap()
        .unwrap();
    let effect = EffectId::new("model-1").unwrap();
    let facts = kernel
        .publish(
            &claim,
            vec![SessionFactBody::ModelIntent {
                turn_id: submitted.turn_id.clone(),
                effect_id: effect,
                snapshot: snapshot(),
            }],
        )
        .await
        .unwrap()
        .published();
    store.fail_next_appends(1);
    let through = facts.last().unwrap().seq();
    let flush = tokio::spawn({
        let kernel = kernel.clone();
        let claim = claim.clone();
        async move { kernel.flush(&claim, through).await }
    });
    tokio::task::yield_now().await;
    assert!(!flush.is_finished());
    assert_eq!(
        store
            .read_facts(&submitted.session_id, 0, 8)
            .await
            .unwrap()
            .durable_seq,
        submitted.accepted_seq
    );
    tokio::time::advance(std::time::Duration::from_millis(199)).await;
    tokio::task::yield_now().await;
    assert!(!flush.is_finished());
    tokio::time::advance(std::time::Duration::from_millis(1)).await;
    tokio::task::yield_now().await;
    assert_eq!(flush.await.unwrap().unwrap(), through);
    let stored = store.read_facts(&submitted.session_id, 0, 8).await.unwrap();
    assert_eq!(stored.facts.len(), 2);
    assert!(matches!(
        stored.facts[0].body(),
        SessionFactBody::TurnAccepted { .. }
    ));
    assert!(matches!(
        stored.facts[1].body(),
        SessionFactBody::ModelIntent { .. }
    ));
    kernel.shutdown(worker).await.unwrap();
}

#[tokio::test]
async fn effect_start_requires_its_intent_to_be_durable() {
    let store = Arc::new(MemoryStore::new());
    let kernel = kernel(store).await;
    let worker = kernel.start_write_behind();
    let submitted = submit(&kernel, "session-effect-fence", "hello").await;
    let _lease = kernel.register("executor".into()).unwrap();
    let claim = kernel
        .claim("executor", CancellationToken::new())
        .await
        .unwrap()
        .unwrap();
    let effect = EffectId::new("model-effect-fence").unwrap();

    let error = kernel
        .publish(
            &claim,
            vec![
                SessionFactBody::ModelIntent {
                    turn_id: submitted.turn_id.clone(),
                    effect_id: effect.clone(),
                    snapshot: snapshot(),
                },
                SessionFactBody::ModelStarted {
                    turn_id: submitted.turn_id.clone(),
                    effect_id: effect.clone(),
                },
            ],
        )
        .await
        .expect_err("an effect start cannot share the undurable intent publication");
    assert!(matches!(error, TurnError::Invalid(_)));

    let intent = kernel
        .publish(
            &claim,
            vec![SessionFactBody::ModelIntent {
                turn_id: submitted.turn_id.clone(),
                effect_id: effect.clone(),
                snapshot: snapshot(),
            }],
        )
        .await
        .unwrap()
        .published();
    let through = intent.last().unwrap().seq();
    kernel.flush(&claim, through).await.unwrap();
    kernel
        .publish(
            &claim,
            vec![SessionFactBody::ModelStarted {
                turn_id: submitted.turn_id,
                effect_id: effect,
            }],
        )
        .await
        .unwrap()
        .published();

    kernel.shutdown(worker).await.unwrap();
}

#[tokio::test]
async fn cancellation_single_assigns_cancelled_even_if_executor_reports_completed() {
    let store = Arc::new(MemoryStore::new());
    let kernel = kernel(store.clone()).await;
    let worker = kernel.start_write_behind();
    let submitted = submit(&kernel, "session-cancel", "hello").await;
    let _lease = kernel.register("executor".into()).unwrap();
    let claim = kernel
        .claim("executor", CancellationToken::new())
        .await
        .unwrap()
        .unwrap();
    let cancellation = kernel.cancellation(&claim).unwrap();
    assert!(
        kernel
            .cancel(
                &submitted.session_id,
                &submitted.turn_id,
                Some("stop".into())
            )
            .await
            .unwrap()
            .accepted
    );
    assert!(cancellation.is_cancelled());
    let terminal = kernel
        .publish(
            &claim,
            vec![SessionFactBody::TurnTerminal {
                turn_id: submitted.turn_id.clone(),
                outcome: TurnOutcome::Completed,
            }],
        )
        .await
        .unwrap()
        .published();
    kernel
        .flush(&claim, terminal.last().unwrap().seq())
        .await
        .unwrap();
    assert_eq!(
        kernel
            .outcome(&submitted.session_id, &submitted.turn_id)
            .await
            .unwrap(),
        Some(TurnOutcome::Cancelled)
    );
    let stored = store.read_facts(&submitted.session_id, 0, 8).await.unwrap();
    assert!(matches!(
        stored.facts.last().unwrap().body(),
        SessionFactBody::TurnTerminal {
            outcome: TurnOutcome::Cancelled,
            ..
        }
    ));
    kernel.shutdown(worker).await.unwrap();
}

#[tokio::test]
async fn claim_horizon_hides_later_accepted_turns_but_admits_claimed_turn_facts() {
    let store = Arc::new(MemoryStore::new());
    let kernel = kernel(store).await;
    let worker = kernel.start_write_behind();
    let first = submit(&kernel, "session-horizon", "FIRST_PRIVATE_PROMPT").await;
    let later = kernel
        .submit(SubmitTurn {
            turn_id: client_turn_id(),
            session: resume(&kernel, first.session_id.clone()).await,
            text: "LATER_PRIVATE_PROMPT".into(),
            model: None,
            sandbox: None,
        })
        .await
        .unwrap();
    let _lease = kernel.register("executor".into()).unwrap();
    let claim = kernel
        .claim("executor", CancellationToken::new())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(claim.turn_id(), &first.turn_id);

    let initial = kernel.read_facts(&claim, 0, 8).await.unwrap();
    assert_eq!(initial.through_seq, later.accepted_seq);
    assert_eq!(initial.facts.len(), 1);
    assert!(matches!(
        initial.facts[0].body(),
        SessionFactBody::TurnAccepted { text, .. } if text == "FIRST_PRIVATE_PROMPT"
    ));

    let effect_id = EffectId::new("effect-horizon").unwrap();
    let published = kernel
        .publish(
            &claim,
            vec![SessionFactBody::ModelIntent {
                turn_id: first.turn_id.clone(),
                effect_id: effect_id.clone(),
                snapshot: snapshot(),
            }],
        )
        .await
        .unwrap()
        .published();
    let incremental = kernel
        .read_facts(&claim, initial.through_seq, 8)
        .await
        .unwrap();
    assert_eq!(incremental.facts, published);
    assert!(matches!(
        incremental.facts[0].body(),
        SessionFactBody::ModelIntent { effect_id: current, .. } if current == &effect_id
    ));
    kernel.shutdown(worker).await.unwrap();
}

#[tokio::test]
async fn checkpoint_maintenance_reads_the_exact_prefix_including_queued_turns() {
    let store = Arc::new(MemoryStore::new());
    let kernel = kernel(store).await;
    let worker = kernel.start_write_behind();
    let first = submit(&kernel, "session-checkpoint-queue", "first").await;
    let queued = kernel
        .submit(SubmitTurn {
            turn_id: client_turn_id(),
            session: resume(&kernel, first.session_id.clone()).await,
            text: "queued".into(),
            model: None,
            sandbox: None,
        })
        .await
        .unwrap();
    let _lease = kernel.register("executor".into()).unwrap();
    let claim = kernel
        .claim("executor", CancellationToken::new())
        .await
        .unwrap()
        .unwrap();
    let terminal = kernel
        .publish(
            &claim,
            vec![SessionFactBody::TurnTerminal {
                turn_id: first.turn_id,
                outcome: TurnOutcome::Completed,
            }],
        )
        .await
        .unwrap()
        .published();
    kernel
        .flush(&claim, terminal.last().unwrap().seq())
        .await
        .unwrap();

    let page = kernel
        .read_checkpoint_facts(&claim, 0, 8)
        .await
        .unwrap()
        .expect("a terminal claim with no speculative suffix is checkpointable");

    assert_eq!(page.through_seq, terminal.last().unwrap().seq());
    assert!(page.facts.iter().any(|fact| {
        matches!(
            fact.body(),
            SessionFactBody::TurnAccepted { turn_id, text, .. }
                if turn_id == &queued.turn_id && text == "queued"
        )
    }));
    kernel.shutdown(worker).await.unwrap();
}

#[tokio::test]
async fn checkpoint_maintenance_rejects_a_foreign_terminal_claim() {
    let store = Arc::new(MemoryStore::new());
    let kernel = kernel(store).await;
    let worker = kernel.start_write_behind();
    let submitted = submit(&kernel, "session-checkpoint-claim-binding", "hello").await;
    let _lease = kernel.register("executor".into()).unwrap();
    let claim = kernel
        .claim("executor", CancellationToken::new())
        .await
        .unwrap()
        .unwrap();
    let terminal = kernel
        .publish(
            &claim,
            vec![SessionFactBody::TurnTerminal {
                turn_id: submitted.turn_id,
                outcome: TurnOutcome::Completed,
            }],
        )
        .await
        .unwrap()
        .published();
    kernel
        .flush(&claim, terminal.last().unwrap().seq())
        .await
        .unwrap();

    let foreign = TurnClaimIssuer::new().issue(
        claim.executor_id().to_owned(),
        claim.claim_id(),
        claim.session_id().clone(),
        claim.turn_id().clone(),
        Arc::new(claim.header().clone()),
        claim.accepted_at_ms(),
        claim.accepted_seq(),
        claim.live_seq(),
    );
    assert!(matches!(
        kernel
            .read_checkpoint_facts(&foreign, 0, MAXIMUM_FACTS_PER_READ)
            .await,
        Err(TurnError::StaleClaim)
    ));

    kernel.shutdown(worker).await.unwrap();
}

#[tokio::test]
async fn checkpoint_store_failure_remains_typed_at_the_execution_seam() {
    let memory = Arc::new(MemoryStore::new());
    let store = Arc::new(FactReadRaceStore::new(memory));
    let store_contract: Arc<dyn SessionStore> = store.clone();
    let kernel =
        SessionKernel::recover_with_clock(store_contract, composition(), Arc::new(FixedClock))
            .await
            .unwrap();
    let worker = kernel.start_write_behind();
    let submitted = submit(&kernel, "session-checkpoint-write-failure", "hello").await;
    let _lease = kernel.register("executor".into()).unwrap();
    let claim = kernel
        .claim("executor", CancellationToken::new())
        .await
        .unwrap()
        .unwrap();
    let terminal = kernel
        .publish(
            &claim,
            vec![SessionFactBody::TurnTerminal {
                turn_id: submitted.turn_id,
                outcome: TurnOutcome::Completed,
            }],
        )
        .await
        .unwrap()
        .published();
    let through_seq = terminal.last().unwrap().seq();
    kernel.flush(&claim, through_seq).await.unwrap();
    store.fail_next_checkpoint_write();

    assert!(matches!(
        kernel
            .write_context_checkpoint(
                &claim,
                ContextCheckpoint {
                    header_fingerprint: claim.header().fingerprint().unwrap(),
                    through_seq,
                    fact_prefix_sha256: "0".repeat(64),
                    bytes: Arc::from(b"checkpoint".as_slice()),
                },
            )
            .await,
        Err(TurnError::Store(message)) if message.contains("injected checkpoint write failure")
    ));
    kernel.shutdown(worker).await.unwrap();
}

#[tokio::test]
async fn tightened_store_read_budget_disables_checkpoint_maintenance_end_to_end() {
    let store = Arc::new(MemoryStore::new());
    let store_contract: Arc<dyn SessionStore> = store.clone();
    let kernel = SessionKernel::recover_with_clock_and_limits(
        store_contract,
        composition(),
        Arc::new(FixedClock),
        KernelLimits {
            maximum_store_read_bytes: MAXIMUM_SESSION_FACT_BYTES,
            ..KernelLimits::default()
        },
    )
    .await
    .unwrap();
    let worker = kernel.start_write_behind();
    let submitted = submit(&kernel, "session-checkpoint-disabled", "hello").await;
    let _lease = kernel.register("executor".into()).unwrap();
    let claim = kernel
        .claim("executor", CancellationToken::new())
        .await
        .unwrap()
        .unwrap();
    let terminal = kernel
        .publish(
            &claim,
            vec![SessionFactBody::TurnTerminal {
                turn_id: submitted.turn_id,
                outcome: TurnOutcome::Completed,
            }],
        )
        .await
        .unwrap()
        .published();
    let through_seq = terminal.last().unwrap().seq();
    kernel.flush(&claim, through_seq).await.unwrap();

    assert!(
        kernel
            .read_checkpoint_facts(&claim, 0, MAXIMUM_FACTS_PER_READ)
            .await
            .unwrap()
            .is_none()
    );
    let durable = store
        .read_facts(claim.session_id(), 0, MAXIMUM_FACTS_PER_READ)
        .await
        .unwrap();
    assert!(
        !kernel
            .write_context_checkpoint(
                &claim,
                ContextCheckpoint {
                    header_fingerprint: claim.header().fingerprint().unwrap(),
                    through_seq,
                    fact_prefix_sha256: fact_prefix_sha256(&durable.facts).unwrap(),
                    bytes: Arc::from(b"disabled-checkpoint".as_slice()),
                },
            )
            .await
            .unwrap()
    );
    assert!(
        store
            .read_context_checkpoint(claim.session_id())
            .await
            .unwrap()
            .is_none()
    );
    kernel.shutdown(worker).await.unwrap();
}

#[tokio::test]
#[allow(clippy::too_many_lines)] // The deterministic interleaving keeps every race barrier visible.
async fn claim_fact_read_never_skips_a_prefix_committed_during_store_io() {
    let memory = Arc::new(MemoryStore::new());
    let store = Arc::new(FactReadRaceStore::new(memory));
    let kernel =
        SessionKernel::recover_with_clock(store.clone(), composition(), Arc::new(FixedClock))
            .await
            .unwrap();
    let worker = kernel.start_write_behind();
    let submitted = submit(&kernel, "session-read-race", "hello").await;
    let _lease = kernel.register("executor".into()).unwrap();
    let claim = kernel
        .claim("executor", CancellationToken::new())
        .await
        .unwrap()
        .unwrap();
    let effect = EffectId::new("model-read-race").unwrap();
    let intent = kernel
        .publish(
            &claim,
            vec![SessionFactBody::ModelIntent {
                turn_id: submitted.turn_id.clone(),
                effect_id: effect.clone(),
                snapshot: snapshot(),
            }],
        )
        .await
        .unwrap()
        .published();
    kernel
        .flush(&claim, intent.last().unwrap().seq())
        .await
        .unwrap();
    let started = kernel
        .publish(
            &claim,
            vec![SessionFactBody::ModelStarted {
                turn_id: submitted.turn_id.clone(),
                effect_id: effect.clone(),
            }],
        )
        .await
        .unwrap()
        .published();
    kernel
        .flush(&claim, started.last().unwrap().seq())
        .await
        .unwrap();
    worker.abort();
    let _ = worker.await;

    let mut first_batch = Vec::with_capacity(MAXIMUM_FACTS_PER_READ);
    first_batch.push(SessionFactBody::ModelEvent {
        turn_id: submitted.turn_id.clone(),
        effect_id: effect.clone(),
        event: LanguageEvent::ContentStarted {
            index: 0,
            content: ContentStart::Text,
        },
    });
    first_batch.extend(
        (1..MAXIMUM_FACTS_PER_READ).map(|_| SessionFactBody::ModelEvent {
            turn_id: submitted.turn_id.clone(),
            effect_id: effect.clone(),
            event: LanguageEvent::ContentDelta {
                index: 0,
                delta: ContentDelta::Text("x".into()),
            },
        }),
    );
    kernel
        .publish(&claim, first_batch)
        .await
        .unwrap()
        .published();
    kernel
        .publish(
            &claim,
            vec![SessionFactBody::ModelEvent {
                turn_id: submitted.turn_id,
                effect_id: effect,
                event: LanguageEvent::ContentDelta {
                    index: 0,
                    delta: ContentDelta::Text("tail".into()),
                },
            }],
        )
        .await
        .unwrap()
        .published();

    store.pause_next_read();
    let read = tokio::spawn({
        let kernel = kernel.clone();
        let claim = claim.clone();
        async move { kernel.read_facts(&claim, 0, MAXIMUM_FACTS_PER_READ).await }
    });
    store.wait_until_read_is_captured().await;
    store.pause_second_following_append();
    let worker = kernel.start_write_behind();
    store.wait_until_append_is_blocked().await;
    store.release_captured_read();

    let page = read.await.unwrap().unwrap();
    assert_eq!(page.through_seq, 3);
    assert!(
        page.facts
            .windows(2)
            .all(|pair| pair[1].seq() == pair[0].seq() + 1),
        "a Store prefix committed during the read must be returned on a later page, not skipped: {:?}",
        page.facts.iter().map(|fact| fact.seq()).collect::<Vec<_>>()
    );
    let committed = kernel
        .read_facts(&claim, page.through_seq, MAXIMUM_FACTS_PER_READ)
        .await
        .unwrap();
    assert_eq!(committed.facts.first().map(|fact| fact.seq()), Some(4));
    assert_eq!(committed.facts.last().map(|fact| fact.seq()), Some(515));
    assert_eq!(committed.through_seq, 515);

    store.release_blocked_append();
    kernel.shutdown(worker).await.unwrap();
}

#[tokio::test]
async fn claim_fact_read_does_not_cross_the_live_horizon_captured_before_store_io() {
    let memory = Arc::new(MemoryStore::new());
    let store = Arc::new(FactReadRaceStore::new(memory));
    let kernel =
        SessionKernel::recover_with_clock(store.clone(), composition(), Arc::new(FixedClock))
            .await
            .unwrap();
    let worker = kernel.start_write_behind();
    let submitted = submit(&kernel, "session-captured-live-horizon", "first").await;
    let _lease = kernel.register("executor".into()).unwrap();
    let claim = kernel
        .claim("executor", CancellationToken::new())
        .await
        .unwrap()
        .unwrap();
    kernel.flush(&claim, submitted.accepted_seq).await.unwrap();
    worker.abort();
    let _ = worker.await;

    let intent = kernel
        .publish(
            &claim,
            vec![SessionFactBody::ModelIntent {
                turn_id: submitted.turn_id.clone(),
                effect_id: EffectId::new("captured-live-horizon").unwrap(),
                snapshot: snapshot(),
            }],
        )
        .await
        .unwrap()
        .published();
    let captured_live_seq = intent.last().unwrap().seq();

    store.pause_next_read();
    let read = tokio::spawn({
        let kernel = kernel.clone();
        let claim = claim.clone();
        async move { kernel.read_facts(&claim, 0, MAXIMUM_FACTS_PER_READ).await }
    });
    store.wait_until_read_is_captured().await;
    let worker = kernel.start_write_behind();
    let later = tokio::spawn({
        let kernel = kernel.clone();
        async move {
            kernel
                .submit(SubmitTurn {
                    turn_id: client_turn_id(),
                    session: resume(&kernel, submitted.session_id).await,
                    text: "LATER_PRIVATE_PROMPT".into(),
                    model: None,
                    sandbox: None,
                })
                .await
        }
    });
    tokio::task::yield_now().await;
    assert!(
        !later.is_finished(),
        "a maximum-weight boundary read must wait behind the captured Store page"
    );
    store.release_captured_read();

    let page = read.await.unwrap().unwrap();
    let later = later.await.unwrap().unwrap();
    assert!(page.through_seq <= captured_live_seq);
    assert!(
        page.facts
            .iter()
            .all(|fact| fact.seq() <= captured_live_seq)
    );
    assert!(page.facts.iter().all(|fact| {
        !matches!(
            fact.body(),
            SessionFactBody::TurnAccepted { turn_id, .. } if turn_id == &later.turn_id
        )
    }));

    kernel.shutdown(worker).await.unwrap();
}

#[tokio::test]
async fn executor_cannot_classify_cancellation_without_a_durable_request() {
    let store = Arc::new(MemoryStore::new());
    let kernel = kernel(store.clone()).await;
    let worker = kernel.start_write_behind();
    let submitted = submit(&kernel, "session-unrequested-cancel", "hello").await;
    let _lease = kernel.register("executor".into()).unwrap();
    let claim = kernel
        .claim("executor", CancellationToken::new())
        .await
        .unwrap()
        .unwrap();
    let terminal = kernel
        .publish(
            &claim,
            vec![SessionFactBody::TurnTerminal {
                turn_id: submitted.turn_id.clone(),
                outcome: TurnOutcome::Cancelled,
            }],
        )
        .await
        .unwrap()
        .published();
    kernel
        .flush(&claim, terminal.last().unwrap().seq())
        .await
        .unwrap();

    assert!(matches!(
        kernel
            .outcome(&submitted.session_id, &submitted.turn_id)
            .await
            .unwrap(),
        Some(TurnOutcome::Failed { code, .. }) if code == "executor.unrequested_cancellation"
    ));
    let stored = store.read_facts(&submitted.session_id, 0, 8).await.unwrap();
    assert!(matches!(
        stored.facts.last().unwrap().body(),
        SessionFactBody::TurnTerminal {
            outcome: TurnOutcome::Failed { code, .. },
            ..
        } if code == "executor.unrequested_cancellation"
    ));
    kernel.shutdown(worker).await.unwrap();
}

#[tokio::test]
async fn terminal_outcome_and_fact_are_hidden_until_their_prefix_is_durable() {
    let store = Arc::new(MemoryStore::new());
    let kernel = kernel(store).await;
    let initial_worker = kernel.start_write_behind();
    let submitted = submit(&kernel, "session-terminal-fence", "hello").await;
    initial_worker.abort();
    let _ = initial_worker.await;
    let _lease = kernel.register("executor".into()).unwrap();
    let claim = kernel
        .claim("executor", CancellationToken::new())
        .await
        .unwrap()
        .unwrap();
    let terminal = kernel
        .publish(
            &claim,
            vec![SessionFactBody::TurnTerminal {
                turn_id: submitted.turn_id.clone(),
                outcome: TurnOutcome::Completed,
            }],
        )
        .await
        .unwrap()
        .published();
    assert_eq!(
        kernel
            .outcome(&submitted.session_id, &submitted.turn_id)
            .await
            .unwrap(),
        None
    );
    let mut observation = kernel
        .observe(&submitted.session_id, submitted.accepted_seq)
        .await
        .unwrap();
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(10), observation.next())
            .await
            .is_err(),
        "a speculative terminal Fact must not enter observation"
    );

    let worker = kernel.start_write_behind();
    kernel
        .flush(&claim, terminal.last().unwrap().seq())
        .await
        .unwrap();
    assert_eq!(
        kernel
            .outcome(&submitted.session_id, &submitted.turn_id)
            .await
            .unwrap(),
        Some(TurnOutcome::Completed)
    );
    let mut observed_terminal = false;
    while let Some(update) = observation.next().await {
        if matches!(
            update.unwrap(),
            TurnUpdate::Fact { fact, durable_seq }
                if fact.seq() == terminal[0].seq() && durable_seq >= fact.seq()
        ) {
            observed_terminal = true;
            break;
        }
    }
    assert!(observed_terminal);
    kernel.shutdown(worker).await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn cancellation_does_not_fire_before_its_fact_is_durable() {
    let store = Arc::new(MemoryStore::new());
    let kernel = kernel(store.clone()).await;
    let worker = kernel.start_write_behind();
    let submitted = submit(&kernel, "session-cancel-durable", "hello").await;
    let _lease = kernel.register("executor".into()).unwrap();
    let claim = kernel
        .claim("executor", CancellationToken::new())
        .await
        .unwrap()
        .unwrap();
    let cancellation = kernel.cancellation(&claim).unwrap();
    store.fail_next_appends(1);
    let cancelling = tokio::spawn({
        let kernel = kernel.clone();
        let session_id = submitted.session_id.clone();
        let turn_id = submitted.turn_id.clone();
        async move {
            kernel
                .cancel(&session_id, &turn_id, Some("stop".into()))
                .await
        }
    });
    tokio::task::yield_now().await;
    assert!(!cancellation.is_cancelled());
    assert!(!cancelling.is_finished());

    tokio::time::advance(std::time::Duration::from_millis(200)).await;
    tokio::task::yield_now().await;
    assert!(cancelling.await.unwrap().unwrap().accepted);
    assert!(cancellation.is_cancelled());
    let stored = store.read_facts(&submitted.session_id, 0, 8).await.unwrap();
    assert!(matches!(
        stored.facts.last().unwrap().body(),
        SessionFactBody::CancelRequested { .. }
    ));
    kernel.shutdown(worker).await.unwrap();
}
