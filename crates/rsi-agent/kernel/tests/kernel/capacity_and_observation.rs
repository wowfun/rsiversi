use super::*;

#[tokio::test(start_paused = true)]
async fn durable_cancellation_fires_even_after_the_requesting_future_detaches() {
    let store = Arc::new(MemoryStore::new());
    let kernel = kernel(store.clone()).await;
    let worker = kernel.start_write_behind();
    let submitted = submit(&kernel, "session-cancel-detached", "hello").await;
    let _lease = kernel.register("executor".into()).unwrap();
    let claim = kernel
        .claim("executor", CancellationToken::new())
        .await
        .unwrap()
        .unwrap();
    let cancellation = kernel.cancellation(&claim).unwrap();
    let mut observation = kernel
        .observe(&submitted.session_id, submitted.accepted_seq)
        .await
        .unwrap();
    store.fail_next_appends(1);
    let cancelling = tokio::spawn({
        let kernel = kernel.clone();
        let session_id = submitted.session_id.clone();
        let turn_id = submitted.turn_id.clone();
        async move { kernel.cancel(&session_id, &turn_id, None).await }
    });
    let update = observation.next().await.unwrap().unwrap();
    assert!(matches!(
        update,
        TurnUpdate::Fact { fact, .. }
            if matches!(fact.body(), SessionFactBody::CancelRequested { .. })
    ));
    cancelling.abort();
    let _ = cancelling.await;
    assert!(!cancellation.is_cancelled());

    tokio::time::advance(std::time::Duration::from_millis(400)).await;
    tokio::task::yield_now().await;
    assert!(
        cancellation.is_cancelled(),
        "durable commit, not request-future ownership, must fire the token"
    );
    kernel.shutdown(worker).await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn persistent_store_failure_eventually_latches_a_flush_error() {
    let store = Arc::new(MemoryStore::new());
    let kernel = kernel(store.clone()).await;
    let worker = kernel.start_write_behind();
    let submitted = submit(&kernel, "session-cancel-persistent-io", "hello").await;
    let mut observation = kernel
        .observe(&submitted.session_id, submitted.accepted_seq)
        .await
        .unwrap();
    store.fail_next_appends(usize::MAX);
    let cancelling = tokio::spawn({
        let kernel = kernel.clone();
        let session_id = submitted.session_id.clone();
        let turn_id = submitted.turn_id.clone();
        async move { kernel.cancel(&session_id, &turn_id, None).await }
    });

    for _ in 0..16 {
        tokio::time::advance(std::time::Duration::from_secs(5)).await;
        tokio::task::yield_now().await;
        if cancelling.is_finished() {
            break;
        }
    }
    assert!(
        cancelling.is_finished(),
        "persistent I/O failure must not leave a public cancel future pending forever"
    );
    assert!(matches!(
        cancelling.await.unwrap(),
        Err(TurnError::Flush(_))
    ));
    assert!(matches!(
        observation.next().await,
        Some(Ok(TurnUpdate::Fact { fact, .. }))
            if matches!(fact.body(), SessionFactBody::CancelRequested { .. })
    ));
    let terminal = tokio::time::timeout(std::time::Duration::from_millis(1), observation.next())
        .await
        .expect("a latched flush error must terminate the attached observation");
    assert!(matches!(terminal, Some(Err(TurnError::Flush(_)))));
    assert!(matches!(
        kernel
            .submit(SubmitTurn {
                turn_id: client_turn_id(),
                session: resume(&kernel, submitted.session_id.clone()).await,
                text: "must not wedge behind the permanent failure".into(),
                model: None,
                sandbox: None,
            })
            .await,
        Err(TurnError::Flush(_))
    ));
    assert!(kernel.shutdown(worker).await.is_err());
}

#[tokio::test(start_paused = true)]
async fn failed_cancellation_admission_can_be_retried_after_capacity_recovers() {
    let store = Arc::new(MemoryStore::new());
    let kernel = kernel(store.clone()).await;
    let worker = kernel.start_write_behind();
    let first = submit(&kernel, "session-cancel-full", "hello").await;
    let _lease = kernel.register("executor".into()).unwrap();
    let claim = kernel
        .claim("executor", CancellationToken::new())
        .await
        .unwrap()
        .unwrap();
    let effect = EffectId::new("effect-fill").unwrap();
    let intent = kernel
        .publish(
            &claim,
            vec![SessionFactBody::ModelIntent {
                turn_id: first.turn_id.clone(),
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
    kernel
        .publish(
            &claim,
            vec![SessionFactBody::ModelStarted {
                turn_id: first.turn_id.clone(),
                effect_id: effect.clone(),
            }],
        )
        .await
        .unwrap()
        .published();
    let mut chunk = MAX_LANGUAGE_OUTPUT_BYTES;
    while chunk > 0 {
        let result = kernel
            .publish(
                &claim,
                vec![SessionFactBody::ModelEvent {
                    turn_id: first.turn_id.clone(),
                    effect_id: effect.clone(),
                    event: LanguageEvent::ContentDelta {
                        index: 0,
                        delta: ContentDelta::Text("x".repeat(chunk)),
                    },
                }],
            )
            .await;
        match result {
            Ok(_) => {}
            Err(TurnError::Flush(_) | TurnError::BudgetExceeded { .. }) => chunk /= 2,
            Err(error) => panic!("unexpected fill failure: {error}"),
        }
    }

    assert!(
        kernel
            .cancel(
                &first.session_id,
                &first.turn_id,
                Some("x".repeat(MAXIMUM_AGENT_DIAGNOSTIC_BYTES)),
            )
            .await
            .is_err(),
        "the full speculative suffix must reject the cancellation Fact"
    );

    tokio::time::advance(std::time::Duration::from_millis(200)).await;
    tokio::task::yield_now().await;
    let retry = kernel
        .cancel(&first.session_id, &first.turn_id, None)
        .await
        .unwrap();
    assert!(
        retry.accepted,
        "failed admission must not consume cancellation"
    );
    kernel.shutdown(worker).await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn shutdown_timeout_stops_the_worker_and_releases_its_store_owner() {
    let store = Arc::new(MemoryStore::new());
    let kernel = kernel(store.clone()).await;
    let worker = kernel.start_write_behind();
    let submitted = submit(&kernel, "session-shutdown-failure", "hello").await;
    let _lease = kernel.register("executor".into()).unwrap();
    let claim = kernel
        .claim("executor", CancellationToken::new())
        .await
        .unwrap()
        .unwrap();
    store.fail_next_appends(usize::MAX);
    kernel
        .publish(
            &claim,
            vec![SessionFactBody::ModelIntent {
                turn_id: submitted.turn_id,
                effect_id: EffectId::new("shutdown-pending").unwrap(),
                snapshot: snapshot(),
            }],
        )
        .await
        .unwrap()
        .published();

    let shutdown = tokio::spawn({
        let kernel = kernel.clone();
        async move { kernel.shutdown(worker).await }
    });
    tokio::task::yield_now().await;
    tokio::time::advance(std::time::Duration::from_secs(5)).await;
    tokio::task::yield_now().await;
    assert!(shutdown.await.unwrap().is_err());
    drop(kernel);
    assert_eq!(
        Arc::strong_count(&store),
        1,
        "failed shutdown must not leave the Store owned by a detached worker"
    );
}

#[tokio::test(start_paused = true)]
async fn shutdown_snapshots_flush_waiters_before_terminal_sessions_can_be_evicted() {
    let store = Arc::new(MemoryStore::new());
    let kernel = kernel(store.clone()).await;
    let worker = kernel.start_write_behind();
    let first = submit(&kernel, "session-shutdown-a", "first").await;
    let second = submit(&kernel, "session-shutdown-b", "second").await;
    let _lease = kernel.register("executor".into()).unwrap();

    for submitted in [&first, &second] {
        let claim = kernel
            .claim("executor", CancellationToken::new())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(claim.turn_id(), &submitted.turn_id);
        kernel
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
    }

    kernel
        .shutdown(worker)
        .await
        .expect("terminal eviction must not invalidate a later shutdown waiter");
    for submitted in [first, second] {
        let page = store.read_facts(&submitted.session_id, 0, 8).await.unwrap();
        assert_eq!(page.durable_seq, 2);
        assert!(matches!(
            page.facts.last().map(SessionFact::body),
            Some(SessionFactBody::TurnTerminal { .. })
        ));
    }
}

#[tokio::test(start_paused = true)]
async fn shutdown_fences_publish_before_its_final_flush_snapshot_can_be_extended() {
    let memory = Arc::new(MemoryStore::new());
    let store = Arc::new(FactReadRaceStore::new(memory.clone()));
    let store_contract: Arc<dyn SessionStore> = store.clone();
    let kernel =
        SessionKernel::recover_with_clock(store_contract, composition(), Arc::new(FixedClock))
            .await
            .unwrap();
    let worker = kernel.start_write_behind();
    let submitted = submit(&kernel, "session-shutdown-publish", "hello").await;
    let _lease = kernel.register("executor".into()).unwrap();
    let claim = kernel
        .claim("executor", CancellationToken::new())
        .await
        .unwrap()
        .unwrap();
    let effect_id = EffectId::new("shutdown-publish").unwrap();
    store.pause_next_append();
    kernel
        .publish(
            &claim,
            vec![SessionFactBody::ModelIntent {
                turn_id: submitted.turn_id.clone(),
                effect_id: effect_id.clone(),
                snapshot: snapshot(),
            }],
        )
        .await
        .unwrap()
        .published();

    let shutdown = tokio::spawn({
        let kernel = kernel.clone();
        async move { kernel.shutdown(worker).await }
    });
    store.wait_until_append_is_blocked().await;
    assert!(matches!(
        kernel
            .publish(
                &claim,
                vec![SessionFactBody::ModelStarted {
                    turn_id: submitted.turn_id.clone(),
                    effect_id,
                }],
            )
            .await,
        Err(TurnError::ShuttingDown)
    ));

    store.release_blocked_append();
    shutdown.await.unwrap().unwrap();
    let page = memory
        .read_facts(&submitted.session_id, 0, 8)
        .await
        .unwrap();
    assert_eq!(page.durable_seq, 2);
}

#[tokio::test]
async fn shutdown_settles_joined_cold_hydration_without_installing_a_resident_pin() {
    let memory = Arc::new(MemoryStore::new());
    append_terminal_history(&memory, "session-shutdown-hydration", 1).await;
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
    let worker = kernel.start_write_behind();
    store.pause_next_open_turn_read();

    let leader = tokio::spawn({
        let kernel = kernel.clone();
        async move {
            let session_id = SessionId::new("session-shutdown-hydration").unwrap();
            let prepared = kernel.prepare_resume(&session_id).await?;
            kernel
                .submit(SubmitTurn {
                    turn_id: client_turn_id(),
                    session: SubmitSession::Resume(prepared),
                    text: "leader".into(),
                    model: None,
                    sandbox: None,
                })
                .await
        }
    });
    store.wait_until_open_turn_read_is_captured().await;
    let follower = tokio::spawn({
        let kernel = kernel.clone();
        async move {
            let session_id = SessionId::new("session-shutdown-hydration").unwrap();
            let prepared = kernel.prepare_resume(&session_id).await?;
            kernel
                .submit(SubmitTurn {
                    turn_id: client_turn_id(),
                    session: SubmitSession::Resume(prepared),
                    text: "follower".into(),
                    model: None,
                    sandbox: None,
                })
                .await
        }
    });
    while composition.calls.load(Ordering::Acquire) != 1 {
        tokio::task::yield_now().await;
    }

    kernel.shutdown(worker).await.unwrap();

    assert_eq!(
        tokio::time::timeout(std::time::Duration::from_millis(100), follower)
            .await
            .expect("shutdown must settle hydration followers before Store I/O returns")
            .unwrap(),
        Err(TurnError::ShuttingDown)
    );
    store.release_captured_open_turn_read();
    assert_eq!(leader.await.unwrap(), Err(TurnError::ShuttingDown));
    assert_eq!(
        drops.load(Ordering::Acquire),
        1,
        "the shared hydration pin may not become resident after shutdown"
    );
}

#[tokio::test]
async fn next_turn_is_not_claimable_until_the_previous_terminal_is_durable() {
    let store = Arc::new(MemoryStore::new());
    let kernel = kernel(store).await;
    let worker = kernel.start_write_behind();
    let first = submit(&kernel, "session-queue", "first").await;
    let second = kernel
        .submit(SubmitTurn {
            turn_id: client_turn_id(),
            session: resume(&kernel, first.session_id.clone()).await,
            text: "second".into(),
            model: None,
            sandbox: None,
        })
        .await
        .unwrap();
    let _one = kernel.register("one".into()).unwrap();
    let _two = kernel.register("two".into()).unwrap();
    let first_claim = kernel
        .claim("one", CancellationToken::new())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(first_claim.turn_id(), &first.turn_id);
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
    let waiting = tokio::spawn({
        let kernel = kernel.clone();
        async move { kernel.claim("two", CancellationToken::new()).await }
    });
    tokio::task::yield_now().await;
    assert!(!waiting.is_finished());
    kernel
        .flush(&first_claim, terminal.last().unwrap().seq())
        .await
        .unwrap();
    assert_eq!(
        waiting.await.unwrap().unwrap().unwrap().turn_id(),
        &second.turn_id
    );
    kernel.shutdown(worker).await.unwrap();
}

#[tokio::test]
async fn one_executor_registration_can_hold_claims_for_two_distinct_sessions() {
    let store = Arc::new(MemoryStore::new());
    let kernel = kernel(store).await;
    let worker = kernel.start_write_behind();
    let first = submit(&kernel, "session-shared-executor-a", "first").await;
    let second = submit(&kernel, "session-shared-executor-b", "second").await;
    let _lease = kernel.register("shared-executor".into()).unwrap();

    let first_claim = kernel
        .claim("shared-executor", CancellationToken::new())
        .await
        .unwrap()
        .unwrap();
    let second_claim = kernel
        .claim("shared-executor", CancellationToken::new())
        .await
        .unwrap()
        .unwrap();

    assert_eq!(first_claim.session_id(), &first.session_id);
    assert_eq!(second_claim.session_id(), &second.session_id);
    assert_ne!(first_claim.claim_id(), second_claim.claim_id());
    kernel.shutdown(worker).await.unwrap();
}

#[tokio::test]
async fn conflicting_retry_does_not_replace_the_original_turn_control_state() {
    let store = Arc::new(MemoryStore::new());
    let kernel = kernel(store).await;
    let worker = kernel.start_write_behind();
    let turn_id = TurnId::new("caller-stable-turn").unwrap();
    let first = kernel
        .submit(SubmitTurn {
            turn_id: turn_id.clone(),
            session: fresh(header("session-conflicting-retry")),
            text: "original".into(),
            model: None,
            sandbox: None,
        })
        .await
        .unwrap();
    assert!(matches!(
        kernel
            .submit(SubmitTurn {
                turn_id: turn_id.clone(),
                session: fresh(header("session-conflicting-retry")),
                text: "changed".into(),
                model: None,
                sandbox: None,
            })
            .await,
        Err(TurnError::SubmissionConflict { .. })
    ));
    let _lease = kernel.register("executor-conflict".into()).unwrap();
    let claim = kernel
        .claim("executor-conflict", CancellationToken::new())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(claim.session_id(), &first.session_id);
    assert_eq!(claim.turn_id(), &turn_id);
    kernel.shutdown(worker).await.unwrap();
}

#[tokio::test]
#[allow(clippy::too_many_lines)] // Keep the exact admission, flush, and retry timeline visible.
async fn process_capacity_flush_required_preserves_bodies_and_turn_control_state() {
    let turn_id = TurnId::new("turn-1").unwrap();
    let effect_id = EffectId::new("effect-capacity").unwrap();
    let accepted = SessionFact::new(
        1,
        42,
        SessionFactBody::TurnAccepted {
            turn_id: turn_id.clone(),
            text: "hello".into(),
            model: None,
            sandbox: SandboxMode::WorkspaceWrite,
            require_approval: false,
        },
    )
    .unwrap();
    let intent = SessionFactBody::ModelIntent {
        turn_id: turn_id.clone(),
        effect_id: effect_id.clone(),
        snapshot: snapshot(),
    };
    let started = SessionFactBody::ModelStarted {
        turn_id: turn_id.clone(),
        effect_id: effect_id.clone(),
    };
    let body = SessionFactBody::ModelEvent {
        turn_id: turn_id.clone(),
        effect_id: effect_id.clone(),
        event: LanguageEvent::ContentDelta {
            index: 0,
            delta: ContentDelta::Text("x".repeat(1024)),
        },
    };
    let second_body = SessionFactBody::ModelEvent {
        turn_id: turn_id.clone(),
        effect_id,
        event: LanguageEvent::ContentDelta {
            index: 0,
            delta: ContentDelta::Text("y".repeat(1024)),
        },
    };
    let body_bytes = [
        intent.clone(),
        started.clone(),
        body.clone(),
        second_body.clone(),
    ]
    .into_iter()
    .enumerate()
    .map(|(index, body)| {
        SessionFact::new(u64::try_from(index + 2).unwrap(), 42, body)
            .unwrap()
            .encoded_len()
    })
    .max()
    .unwrap();
    let limits = KernelLimits {
        maximum_process_pending_fact_bytes: accepted.encoded_len().max(body_bytes),
        ..KernelLimits::default()
    };
    let memory = Arc::new(MemoryStore::new());
    let store: Arc<dyn SessionStore> = memory.clone();
    let kernel = SessionKernel::recover_with_clock_and_limits(
        store,
        composition(),
        Arc::new(FixedClock),
        limits,
    )
    .await
    .unwrap();
    let worker = kernel.start_write_behind();
    let _submitted = kernel
        .submit(SubmitTurn {
            turn_id: turn_id.clone(),
            session: fresh(header("session-process-publish")),
            text: "hello".into(),
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
    let intent = kernel
        .publish(&claim, vec![intent])
        .await
        .unwrap()
        .published();
    kernel
        .flush(&claim, intent.last().unwrap().seq())
        .await
        .unwrap();
    let started = kernel
        .publish(&claim, vec![started])
        .await
        .unwrap()
        .published();
    kernel
        .flush(&claim, started.last().unwrap().seq())
        .await
        .unwrap();
    memory.fail_next_appends(usize::MAX);
    let published = kernel
        .publish(&claim, vec![body])
        .await
        .unwrap()
        .published();
    let first_rejection = kernel.publish(&claim, vec![second_body.clone()]).await;
    assert!(
        matches!(
            &first_rejection,
            Ok(PublishAttempt::FlushRequired { unpublished }) if unpublished == &vec![second_body.clone()]
        ),
        "unexpected capacity result: {first_rejection:?}"
    );
    assert!(matches!(
        kernel.publish(&claim, vec![second_body.clone()]).await,
        Ok(PublishAttempt::FlushRequired { unpublished }) if unpublished == vec![second_body.clone()]
    ));
    memory.fail_next_appends(0);
    kernel
        .flush(&claim, published.last().unwrap().seq())
        .await
        .unwrap();
    assert_eq!(
        kernel
            .publish(&claim, vec![second_body])
            .await
            .unwrap()
            .published()
            .len(),
        1
    );
    kernel.shutdown(worker).await.unwrap();
}

#[tokio::test]
async fn publication_larger_than_an_empty_process_budget_is_invalid() {
    let turn_id = TurnId::new("turn-oversized-publication").unwrap();
    let intent = SessionFactBody::ModelIntent {
        turn_id: turn_id.clone(),
        effect_id: EffectId::new("effect-oversized-publication").unwrap(),
        snapshot: snapshot(),
    };
    let accepted_bytes = SessionFact::new(
        1,
        42,
        SessionFactBody::TurnAccepted {
            turn_id: turn_id.clone(),
            text: "hello".into(),
            model: None,
            sandbox: SandboxMode::WorkspaceWrite,
            require_approval: false,
        },
    )
    .unwrap()
    .encoded_len();
    assert!(
        SessionFact::new(2, 42, intent.clone())
            .unwrap()
            .encoded_len()
            > accepted_bytes
    );
    let store: Arc<dyn SessionStore> = Arc::new(MemoryStore::new());
    let kernel = SessionKernel::recover_with_clock_and_limits(
        store,
        composition(),
        Arc::new(FixedClock),
        KernelLimits {
            maximum_process_pending_fact_bytes: accepted_bytes,
            ..KernelLimits::default()
        },
    )
    .await
    .unwrap();
    let worker = kernel.start_write_behind();
    kernel
        .submit(SubmitTurn {
            turn_id: turn_id.clone(),
            session: fresh(header("session-oversized-publication")),
            text: "hello".into(),
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

    assert!(matches!(
        kernel.publish(&claim, vec![intent]).await,
        Err(TurnError::Invalid(_))
    ));
    kernel.shutdown(worker).await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn cancel_reports_pending_capacity_separately_from_durable_flush_failure() {
    let memory = Arc::new(MemoryStore::new());
    let kernel = kernel(memory.clone()).await;
    let worker = kernel.start_write_behind();
    let submitted = submit(&kernel, "session-cancel-pending-capacity", "hello").await;
    let _lease = kernel
        .register("executor-capacity-taxonomy".into())
        .unwrap();
    let claim = kernel
        .claim("executor-capacity-taxonomy", CancellationToken::new())
        .await
        .unwrap()
        .unwrap();
    let effect_id = EffectId::new("effect-capacity-taxonomy").unwrap();
    let intent = kernel
        .publish(
            &claim,
            vec![SessionFactBody::ModelIntent {
                turn_id: submitted.turn_id.clone(),
                effect_id: effect_id.clone(),
                snapshot: snapshot(),
            }],
        )
        .await
        .unwrap()
        .published();
    let intent_bytes = intent[0].encoded_len();
    kernel.flush(&claim, intent[0].seq()).await.unwrap();

    let started = SessionFactBody::ModelStarted {
        turn_id: submitted.turn_id.clone(),
        effect_id: effect_id.clone(),
    };
    let first_delta = SessionFactBody::ModelEvent {
        turn_id: submitted.turn_id.clone(),
        effect_id: effect_id.clone(),
        event: LanguageEvent::ContentDelta {
            index: 0,
            delta: ContentDelta::Text("x".repeat(MAX_LANGUAGE_OUTPUT_BYTES)),
        },
    };
    let second_delta_base = SessionFactBody::ModelEvent {
        turn_id: submitted.turn_id.clone(),
        effect_id: effect_id.clone(),
        event: LanguageEvent::ContentDelta {
            index: 0,
            delta: ContentDelta::Text("y".into()),
        },
    };
    let started_bytes = SessionFact::new(3, 42, started.clone())
        .unwrap()
        .encoded_len();
    let first_delta_bytes = SessionFact::new(4, 42, first_delta.clone())
        .unwrap()
        .encoded_len();
    let second_base_bytes = SessionFact::new(5, 42, second_delta_base)
        .unwrap()
        .encoded_len();
    let pending_budget = usize::try_from(MAXIMUM_TURN_GENERATED_FACT_BYTES).unwrap() - intent_bytes;
    let second_text_bytes = pending_budget
        .checked_sub(started_bytes + first_delta_bytes + second_base_bytes - 1)
        .expect("maximum deltas leave room for the second event");
    assert!(second_text_bytes <= MAX_LANGUAGE_OUTPUT_BYTES);
    let second_delta = SessionFactBody::ModelEvent {
        turn_id: submitted.turn_id.clone(),
        effect_id,
        event: LanguageEvent::ContentDelta {
            index: 0,
            delta: ContentDelta::Text("y".repeat(second_text_bytes)),
        },
    };

    memory.fail_next_appends(usize::MAX);
    assert!(matches!(
        kernel
            .publish(&claim, vec![started, first_delta, second_delta])
            .await
            .unwrap(),
        PublishAttempt::Published(_)
    ));
    assert_eq!(MAXIMUM_PENDING_FACT_BYTES, 64 * 1024 * 1024);
    assert_eq!(
        kernel
            .cancel(
                &submitted.session_id,
                &submitted.turn_id,
                Some("c".repeat(MAXIMUM_AGENT_DIAGNOSTIC_BYTES)),
            )
            .await,
        Err(TurnError::Capacity)
    );

    memory.fail_next_appends(0);
    kernel.shutdown(worker).await.unwrap();
}

#[tokio::test(start_paused = true)]
#[allow(clippy::too_many_lines)] // One scenario keeps both Sessions and the blocked durable commit ordered.
async fn cross_session_process_pressure_waits_for_global_durable_progress() {
    let first_turn = TurnId::new("turn-process-pressure-a").unwrap();
    let second_turn = TurnId::new("turn-process-pressure-b").unwrap();
    let first_body = SessionFactBody::ModelIntent {
        turn_id: first_turn.clone(),
        effect_id: EffectId::new("effect-process-pressure-a").unwrap(),
        snapshot: snapshot(),
    };
    let second_body = SessionFactBody::ModelIntent {
        turn_id: second_turn.clone(),
        effect_id: EffectId::new("effect-process-pressure-b").unwrap(),
        snapshot: snapshot(),
    };
    let fact_bytes = [
        SessionFact::new(
            1,
            42,
            SessionFactBody::TurnAccepted {
                turn_id: first_turn.clone(),
                text: "first".into(),
                model: None,
                sandbox: SandboxMode::WorkspaceWrite,
                require_approval: false,
            },
        )
        .unwrap()
        .encoded_len(),
        SessionFact::new(2, 42, first_body.clone())
            .unwrap()
            .encoded_len(),
        SessionFact::new(
            1,
            42,
            SessionFactBody::TurnAccepted {
                turn_id: second_turn.clone(),
                text: "second".into(),
                model: None,
                sandbox: SandboxMode::WorkspaceWrite,
                require_approval: false,
            },
        )
        .unwrap()
        .encoded_len(),
        SessionFact::new(2, 42, second_body.clone())
            .unwrap()
            .encoded_len(),
    ]
    .into_iter()
    .max()
    .unwrap();
    let memory = Arc::new(MemoryStore::new());
    let store = Arc::new(FactReadRaceStore::new(memory));
    let kernel = SessionKernel::recover_with_clock_and_limits(
        store.clone() as Arc<dyn SessionStore>,
        composition(),
        Arc::new(FixedClock),
        KernelLimits {
            maximum_process_pending_fact_bytes: fact_bytes,
            ..KernelLimits::default()
        },
    )
    .await
    .unwrap();
    let worker = kernel.start_write_behind();
    let first = kernel
        .submit(SubmitTurn {
            turn_id: first_turn,
            session: fresh(header("session-process-pressure-a")),
            text: "first".into(),
            model: None,
            sandbox: None,
        })
        .await
        .unwrap();
    let second = kernel
        .submit(SubmitTurn {
            turn_id: second_turn,
            session: fresh(header("session-process-pressure-b")),
            text: "second".into(),
            model: None,
            sandbox: None,
        })
        .await
        .unwrap();
    let _first_lease = kernel.register("executor-pressure-a".into()).unwrap();
    let _second_lease = kernel.register("executor-pressure-b".into()).unwrap();
    let first_claim = kernel
        .claim("executor-pressure-a", CancellationToken::new())
        .await
        .unwrap()
        .unwrap();
    let second_claim = kernel
        .claim("executor-pressure-b", CancellationToken::new())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(first_claim.turn_id(), &first.turn_id);
    assert_eq!(second_claim.turn_id(), &second.turn_id);
    let first_fact = kernel
        .publish(&first_claim, vec![first_body])
        .await
        .unwrap()
        .published()
        .pop()
        .unwrap();
    store.pause_next_append();
    let first_flush = tokio::spawn({
        let kernel = kernel.clone();
        let first_claim = first_claim.clone();
        async move { kernel.flush(&first_claim, first_fact.seq()).await }
    });
    store.wait_until_append_is_blocked().await;
    let second_publish = tokio::spawn({
        let kernel = kernel.clone();
        let second_claim = second_claim.clone();
        async move { kernel.publish(&second_claim, vec![second_body]).await }
    });
    tokio::task::yield_now().await;
    assert!(
        !second_publish.is_finished(),
        "cross-Session pressure must wait for global durable progress"
    );

    store.release_blocked_append();
    first_flush.await.unwrap().unwrap();
    assert!(matches!(
        second_publish.await.unwrap().unwrap(),
        PublishAttempt::Published(_)
    ));
    kernel.shutdown(worker).await.unwrap();
}

#[tokio::test(start_paused = true)]
#[allow(clippy::too_many_lines)] // One scenario keeps both Sessions, pressure, and failure ordered.
async fn cross_session_process_pressure_observes_own_permanent_flush_failure() {
    let target_session = SessionId::new("session-process-failure-a").unwrap();
    let blocker_session = SessionId::new("session-process-failure-b").unwrap();
    let target_turn = TurnId::new("turn-process-failure-a").unwrap();
    let blocker_turn = TurnId::new("turn-process-failure-b").unwrap();
    let target_effect = EffectId::new("effect-process-failure-a").unwrap();
    let blocker_effect = EffectId::new("effect-process-failure-b").unwrap();
    let target_first = SessionFactBody::ModelEvent {
        turn_id: target_turn.clone(),
        effect_id: target_effect.clone(),
        event: LanguageEvent::ContentDelta {
            index: 0,
            delta: ContentDelta::Text("x".repeat(4096)),
        },
    };
    let target_second = SessionFactBody::ModelEvent {
        turn_id: target_turn.clone(),
        effect_id: target_effect.clone(),
        event: LanguageEvent::ContentDelta {
            index: 0,
            delta: ContentDelta::Text("y".repeat(4096)),
        },
    };
    let blocker_first = SessionFactBody::ModelEvent {
        turn_id: blocker_turn.clone(),
        effect_id: blocker_effect.clone(),
        event: LanguageEvent::ContentDelta {
            index: 0,
            delta: ContentDelta::Text("z".repeat(4096)),
        },
    };
    let process_bytes = SessionFact::new(4, 42, target_first.clone())
        .unwrap()
        .encoded_len()
        + SessionFact::new(4, 42, blocker_first.clone())
            .unwrap()
            .encoded_len();
    let memory = Arc::new(MemoryStore::new());
    let store = Arc::new(FactReadRaceStore::new(memory));
    let kernel = SessionKernel::recover_with_clock_and_limits(
        store.clone() as Arc<dyn SessionStore>,
        composition(),
        Arc::new(FixedClock),
        KernelLimits {
            maximum_process_pending_fact_bytes: process_bytes,
            ..KernelLimits::default()
        },
    )
    .await
    .unwrap();
    let worker = kernel.start_write_behind();
    kernel
        .submit(SubmitTurn {
            turn_id: target_turn.clone(),
            session: fresh(header(target_session.as_str())),
            text: "target".into(),
            model: None,
            sandbox: None,
        })
        .await
        .unwrap();
    kernel
        .submit(SubmitTurn {
            turn_id: blocker_turn.clone(),
            session: fresh(header(blocker_session.as_str())),
            text: "blocker".into(),
            model: None,
            sandbox: None,
        })
        .await
        .unwrap();
    let _target_lease = kernel.register("executor-failure-a".into()).unwrap();
    let _blocker_lease = kernel.register("executor-failure-b".into()).unwrap();
    let target_claim = kernel
        .claim("executor-failure-a", CancellationToken::new())
        .await
        .unwrap()
        .unwrap();
    let blocker_claim = kernel
        .claim("executor-failure-b", CancellationToken::new())
        .await
        .unwrap()
        .unwrap();

    for (claim, turn_id, effect_id) in [
        (&target_claim, &target_turn, &target_effect),
        (&blocker_claim, &blocker_turn, &blocker_effect),
    ] {
        let intent = kernel
            .publish(
                claim,
                vec![SessionFactBody::ModelIntent {
                    turn_id: turn_id.clone(),
                    effect_id: effect_id.clone(),
                    snapshot: snapshot(),
                }],
            )
            .await
            .unwrap()
            .published();
        kernel.flush(claim, intent[0].seq()).await.unwrap();
        let started = kernel
            .publish(
                claim,
                vec![SessionFactBody::ModelStarted {
                    turn_id: turn_id.clone(),
                    effect_id: effect_id.clone(),
                }],
            )
            .await
            .unwrap()
            .published();
        kernel.flush(claim, started[0].seq()).await.unwrap();
    }

    kernel
        .publish(&target_claim, vec![target_first])
        .await
        .unwrap();
    kernel
        .publish(&blocker_claim, vec![blocker_first])
        .await
        .unwrap();
    store.fail_appends_for(target_session);
    store.pause_second_following_append();
    let waiting = tokio::spawn({
        let kernel = kernel.clone();
        let target_claim = target_claim.clone();
        async move { kernel.publish(&target_claim, vec![target_second]).await }
    });
    tokio::time::timeout(
        std::time::Duration::from_secs(1),
        store.wait_until_append_is_blocked(),
    )
    .await
    .expect("the unrelated Session flush must remain blocked");

    let result = tokio::time::timeout(std::time::Duration::from_millis(100), waiting)
        .await
        .expect("the Session's permanent flush failure must wake its pressured publication")
        .unwrap();
    assert!(matches!(result, Err(TurnError::Flush(_))));

    store.release_blocked_append();
    assert!(kernel.shutdown(worker).await.is_err());
}

#[tokio::test(start_paused = true)]
async fn live_session_working_set_has_an_exact_global_bound() {
    let store = Arc::new(MemoryStore::new());
    let kernel = kernel(store).await;
    let worker = kernel.start_write_behind();
    for index in 0..MAXIMUM_ACTIVE_SESSIONS {
        submit(&kernel, &format!("session-bound-{index}"), "queued").await;
    }
    assert_eq!(
        kernel
            .submit(SubmitTurn {
                turn_id: client_turn_id(),
                session: fresh(header("session-bound-overflow")),
                text: "overflow".into(),
                model: None,
                sandbox: None,
            })
            .await,
        Err(TurnError::Capacity)
    );
    kernel.shutdown(worker).await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn active_observer_capacity_is_exact_and_released_on_drop() {
    let store = Arc::new(MemoryStore::new());
    let store_contract: Arc<dyn SessionStore> = store;
    let kernel = SessionKernel::recover_with_clock_and_limits(
        store_contract,
        composition(),
        Arc::new(FixedClock),
        KernelLimits::default(),
    )
    .await
    .unwrap();
    let worker = kernel.start_write_behind();
    let submitted = submit(&kernel, "session-observer-bound", "queued").await;
    let mut observers = Vec::with_capacity(DEFAULT_MAXIMUM_ACTIVE_OBSERVERS);
    for _ in 0..DEFAULT_MAXIMUM_ACTIVE_OBSERVERS {
        observers.push(kernel.observe(&submitted.session_id, 0).await.unwrap());
    }
    assert!(matches!(
        kernel.observe(&submitted.session_id, 0).await,
        Err(TurnError::ObserverCapacity)
    ));
    drop(observers.pop());
    observers.push(kernel.observe(&submitted.session_id, 0).await.unwrap());
    drop(observers);
    kernel.shutdown(worker).await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn observation_reports_durability_that_advanced_while_unpolled() {
    let store = Arc::new(MemoryStore::new());
    let kernel = kernel(store).await;
    let worker = kernel.start_write_behind();
    let submitted = submit(&kernel, "session-durable-update", "hello").await;
    let _lease = kernel.register("executor".into()).unwrap();
    let claim = kernel
        .claim("executor", CancellationToken::new())
        .await
        .unwrap()
        .unwrap();
    kernel.flush(&claim, submitted.accepted_seq).await.unwrap();
    let mut observation = kernel
        .observe(&submitted.session_id, submitted.accepted_seq)
        .await
        .unwrap();
    let published = kernel
        .publish(
            &claim,
            vec![SessionFactBody::ModelIntent {
                turn_id: submitted.turn_id,
                effect_id: EffectId::new("effect-observed").unwrap(),
                snapshot: snapshot(),
            }],
        )
        .await
        .unwrap()
        .published();
    assert!(matches!(
        observation.next().await,
        Some(Ok(TurnUpdate::Fact { fact, durable_seq }))
            if fact.seq() == published[0].seq() && durable_seq < fact.seq()
    ));
    kernel.flush(&claim, published[0].seq()).await.unwrap();

    let update = tokio::time::timeout(std::time::Duration::from_millis(100), observation.next())
        .await
        .expect("an unseen durability advance must wake the stream")
        .expect("observation remains open")
        .unwrap();
    assert_eq!(
        update,
        TurnUpdate::Durable {
            durable_seq: published[0].seq()
        }
    );
    kernel.shutdown(worker).await.unwrap();
}

#[tokio::test]
async fn cancelling_evicted_terminal_turns_does_not_consume_live_session_capacity() {
    let store = Arc::new(MemoryStore::new());
    let mut terminal_turns = Vec::new();
    for index in 0..MAXIMUM_ACTIVE_SESSIONS {
        let session_id = SessionId::new(format!("terminal-session-{index}")).unwrap();
        let turn_id = TurnId::new(format!("terminal-turn-{index}")).unwrap();
        store
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
                            text: "done".into(),
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
        terminal_turns.push((session_id, turn_id));
    }
    let kernel = kernel(store).await;
    let worker = kernel.start_write_behind();
    for (session_id, turn_id) in terminal_turns {
        assert!(
            kernel
                .cancel(&session_id, &turn_id, Some("late".into()))
                .await
                .unwrap()
                .already_terminal
        );
    }

    kernel
        .submit(SubmitTurn {
            turn_id: client_turn_id(),
            session: fresh(header("capacity-remains-free")),
            text: "new".into(),
            model: None,
            sandbox: None,
        })
        .await
        .unwrap();
    kernel.shutdown(worker).await.unwrap();
}

#[tokio::test]
async fn invalid_resumes_of_idle_durable_sessions_do_not_consume_live_capacity() {
    let store = Arc::new(MemoryStore::new());
    let mut terminal_sessions = Vec::new();
    for index in 0..MAXIMUM_ACTIVE_SESSIONS {
        let session_id = SessionId::new(format!("invalid-resume-session-{index}")).unwrap();
        let turn_id = TurnId::new(format!("invalid-resume-turn-{index}")).unwrap();
        store
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
                            text: "done".into(),
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
                            turn_id,
                            outcome: TurnOutcome::Completed,
                        },
                    )
                    .unwrap(),
                ],
            })
            .await
            .unwrap();
        terminal_sessions.push(session_id);
    }
    let kernel = kernel(store).await;
    let oversized = "x".repeat(MAXIMUM_TURN_TEXT_BYTES + 1);
    for session_id in terminal_sessions {
        assert!(matches!(
            kernel
                .submit(SubmitTurn {
                    turn_id: client_turn_id(),
                    session: resume(&kernel, session_id).await,
                    text: oversized.clone(),
                    model: None,
                    sandbox: None,
                })
                .await,
            Err(TurnError::Invalid(_))
        ));
    }

    let worker = kernel.start_write_behind();
    kernel
        .submit(SubmitTurn {
            turn_id: client_turn_id(),
            session: fresh(header("capacity-after-invalid-resumes")),
            text: "new".into(),
            model: None,
            sandbox: None,
        })
        .await
        .expect("invalid resume input must not retain idle durable sessions");
    kernel.shutdown(worker).await.unwrap();
}

#[tokio::test]
async fn failed_admission_after_hydration_releases_idle_resident_capacity() {
    let memory = Arc::new(MemoryStore::new());
    for index in 0..MAXIMUM_ACTIVE_SESSIONS {
        append_terminal_history(&memory, &format!("failed-admission-session-{index}"), 1).await;
    }
    let store = Arc::new(FactReadRaceStore::new(memory));
    let store_contract: Arc<dyn SessionStore> = store.clone();
    let kernel = SessionKernel::recover_with_clock_and_limits(
        store_contract,
        composition(),
        Arc::new(FixedClock),
        KernelLimits {
            maximum_process_pending_fact_bytes: 1,
            ..KernelLimits::default()
        },
    )
    .await
    .unwrap();
    for index in 0..MAXIMUM_ACTIVE_SESSIONS {
        assert!(matches!(
            kernel
                .submit(SubmitTurn {
                    turn_id: client_turn_id(),
                    session: resume(
                        &kernel,
                        SessionId::new(format!("failed-admission-session-{index}")).unwrap(),
                    )
                    .await,
                    text: "cannot fit".into(),
                    model: None,
                    sandbox: None,
                })
                .await,
            Err(TurnError::Capacity)
        ));
    }

    store.reset_header_read_attempts();
    assert!(matches!(
        kernel
            .submit(SubmitTurn {
                turn_id: client_turn_id(),
                session: fresh(header("capacity-after-failed-admissions")),
                text: "cannot fit either".into(),
                model: None,
                sandbox: None,
            })
            .await,
        Err(TurnError::Capacity)
    ));
    assert_eq!(
        store.header_read_attempts(),
        1,
        "failed admission must release each newly hydrated idle session before fresh capacity is checked"
    );
}

#[tokio::test]
async fn historical_outcome_lookup_does_not_page_the_complete_session_log() {
    let memory = Arc::new(MemoryStore::new());
    append_terminal_history(&memory, "session-indexed-outcome", 300).await;
    let store = Arc::new(FactReadRaceStore::new(memory));
    let store_contract: Arc<dyn SessionStore> = store.clone();
    let kernel =
        SessionKernel::recover_with_clock(store_contract, composition(), Arc::new(FixedClock))
            .await
            .unwrap();
    store.reset_read_attempts();

    assert_eq!(
        kernel
            .outcome(
                &SessionId::new("session-indexed-outcome").unwrap(),
                &TurnId::new("turn-history-299").unwrap(),
            )
            .await
            .unwrap(),
        Some(TurnOutcome::Completed)
    );
    assert_eq!(
        store.read_attempts(),
        0,
        "an outcome lookup must use the Store's turn index, not full-log pages"
    );
}

#[tokio::test]
async fn recovery_skips_fact_pages_for_sessions_without_open_turns() {
    let memory = Arc::new(MemoryStore::new());
    append_terminal_history(&memory, "session-recovery-index", 300).await;
    let store = Arc::new(FactReadRaceStore::new(memory));
    let store_contract: Arc<dyn SessionStore> = store.clone();

    SessionKernel::recover_with_clock(store_contract, composition(), Arc::new(FixedClock))
        .await
        .unwrap();

    assert_eq!(
        store.read_attempts(),
        0,
        "recovery must query the bounded open-turn index before decoding Fact bodies"
    );
    assert_eq!(
        store.open_turn_read_attempts(),
        0,
        "closed sessions must be excluded by Store enumeration, not probed one by one"
    );
}

#[tokio::test]
async fn durable_observation_pages_store_reads_instead_of_reading_one_fact_at_a_time() {
    let memory = Arc::new(MemoryStore::new());
    append_terminal_history(&memory, "session-observation-pages", 300).await;
    let store = Arc::new(FactReadRaceStore::new(memory));
    let store_contract: Arc<dyn SessionStore> = store.clone();
    let kernel =
        SessionKernel::recover_with_clock(store_contract, composition(), Arc::new(FixedClock))
            .await
            .unwrap();
    store.reset_read_attempts();

    let session = SessionId::new("session-observation-pages").unwrap();
    let mut observation = kernel.observe(&session, 0).await.unwrap();
    let mut facts = 0;
    while let Some(update) = observation.next().await {
        if matches!(update.unwrap(), TurnUpdate::Fact { .. }) {
            facts += 1;
        }
    }

    assert_eq!(facts, 600);
    assert_eq!(
        store.read_attempts(),
        2,
        "600 durable Facts fit in two protocol-bounded Store pages"
    );
}

#[tokio::test]
async fn concurrent_resumes_join_one_control_state_load() {
    let memory = Arc::new(MemoryStore::new());
    append_terminal_history(&memory, "session-joined-load", 1).await;
    let store = Arc::new(FactReadRaceStore::new(memory));
    let store_contract: Arc<dyn SessionStore> = store.clone();
    let kernel =
        SessionKernel::recover_with_clock(store_contract, composition(), Arc::new(FixedClock))
            .await
            .unwrap();
    let worker = kernel.start_write_behind();
    store.reset_read_attempts();
    store.reset_open_turn_read_attempts();
    store.pause_next_open_turn_read();

    let first = tokio::spawn({
        let kernel = kernel.clone();
        async move {
            let session_id = SessionId::new("session-joined-load").unwrap();
            let prepared = kernel.prepare_resume(&session_id).await?;
            kernel
                .submit(SubmitTurn {
                    turn_id: client_turn_id(),
                    session: SubmitSession::Resume(prepared),
                    text: "first".into(),
                    model: None,
                    sandbox: None,
                })
                .await
        }
    });
    store.wait_until_open_turn_read_is_captured().await;
    let second = tokio::spawn({
        let kernel = kernel.clone();
        async move {
            let session_id = SessionId::new("session-joined-load").unwrap();
            let prepared = kernel.prepare_resume(&session_id).await?;
            kernel
                .submit(SubmitTurn {
                    turn_id: client_turn_id(),
                    session: SubmitSession::Resume(prepared),
                    text: "second".into(),
                    model: None,
                    sandbox: None,
                })
                .await
        }
    });
    tokio::task::yield_now().await;
    store.release_captured_open_turn_read();
    first.await.unwrap().unwrap();
    second.await.unwrap().unwrap();

    assert_eq!(
        store.open_turn_read_attempts(),
        1,
        "concurrent resumes of one idle session must join one Store load"
    );
    kernel.shutdown(worker).await.unwrap();
}

#[tokio::test]
async fn concurrent_resume_joins_the_resident_load_when_source_becomes_unavailable() {
    let memory = Arc::new(MemoryStore::new());
    append_terminal_history(&memory, "session-source-race", 1).await;
    let store = Arc::new(FactReadRaceStore::new(memory));
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
    let worker = kernel.start_write_behind();
    store.pause_next_open_turn_read();

    let first = tokio::spawn({
        let kernel = kernel.clone();
        async move {
            let session_id = SessionId::new("session-source-race").unwrap();
            let prepared = kernel.prepare_resume(&session_id).await?;
            kernel
                .submit(SubmitTurn {
                    turn_id: client_turn_id(),
                    session: SubmitSession::Resume(prepared),
                    text: "first".into(),
                    model: None,
                    sandbox: None,
                })
                .await
        }
    });
    store.wait_until_open_turn_read_is_captured().await;
    composition.set_unavailable();
    let second = tokio::spawn({
        let kernel = kernel.clone();
        async move {
            let session_id = SessionId::new("session-source-race").unwrap();
            let prepared = kernel.prepare_resume(&session_id).await?;
            kernel
                .submit(SubmitTurn {
                    turn_id: client_turn_id(),
                    session: SubmitSession::Resume(prepared),
                    text: "second".into(),
                    model: None,
                    sandbox: None,
                })
                .await
        }
    });
    tokio::task::yield_now().await;
    assert!(!second.is_finished());
    store.release_captured_open_turn_read();

    first.await.unwrap().unwrap();
    second.await.unwrap().unwrap();
    assert_eq!(store.open_turn_read_attempts(), 1);
    assert_eq!(composition.calls.load(Ordering::Acquire), 1);
    kernel.shutdown(worker).await.unwrap();
}

#[tokio::test]
async fn cancelled_fresh_header_lookup_releases_its_exact_reservation() {
    let memory = Arc::new(MemoryStore::new());
    let store = Arc::new(FactReadRaceStore::new(memory));
    let store_contract: Arc<dyn SessionStore> = store.clone();
    let kernel =
        SessionKernel::recover_with_clock(store_contract, composition(), Arc::new(FixedClock))
            .await
            .unwrap();
    store.block_header_reads();
    let drops = Arc::new(AtomicUsize::new(0));
    let session_header = header("session-cancelled-fresh");
    let pin = AgentCompositionPin::new(
        session_header.agent_preset_id().clone(),
        "a".repeat(64),
        Arc::new(EmptyTools),
        Arc::new(DropOwner(Arc::clone(&drops))),
    )
    .unwrap();
    let first = tokio::spawn({
        let kernel = kernel.clone();
        async move {
            kernel
                .submit(SubmitTurn {
                    turn_id: client_turn_id(),
                    session: SubmitSession::Fresh(
                        PreparedFreshSession::new(session_header, pin).unwrap(),
                    ),
                    text: "first".into(),
                    model: None,
                    sandbox: None,
                })
                .await
        }
    });
    tokio::time::timeout(std::time::Duration::from_secs(1), async {
        while store.header_read_attempts() != 1 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap_or_else(|_| {
        panic!(
            "fresh header lookup did not block exactly once; observed {} attempts",
            store.header_read_attempts()
        )
    });
    first.abort();
    let _ = first.await;
    assert_eq!(drops.load(Ordering::Acquire), 1);
    store.release_blocked_header_reads();
    let worker = kernel.start_write_behind();

    kernel
        .submit(SubmitTurn {
            turn_id: client_turn_id(),
            session: fresh(header("session-cancelled-fresh")),
            text: "retry".into(),
            model: None,
            sandbox: None,
        })
        .await
        .expect("dropping the first lookup must release its reservation");
    kernel.shutdown(worker).await.unwrap();
}

#[tokio::test]
async fn failed_fresh_submission_releases_its_prepared_generation_pin() {
    let store = Arc::new(MemoryStore::new());
    let store_contract: Arc<dyn SessionStore> = store.clone();
    let kernel = SessionKernel::recover_with_clock_and_limits(
        store_contract,
        composition(),
        Arc::new(FixedClock),
        KernelLimits {
            maximum_process_pending_fact_bytes: 1,
            ..KernelLimits::default()
        },
    )
    .await
    .unwrap();
    let drops = Arc::new(AtomicUsize::new(0));
    let session_header = header("session-failed-fresh-pin");
    let session_id = session_header.session_id().clone();
    let pin = AgentCompositionPin::new(
        session_header.agent_preset_id().clone(),
        "a".repeat(64),
        Arc::new(EmptyTools),
        Arc::new(DropOwner(Arc::clone(&drops))),
    )
    .unwrap();

    assert_eq!(
        kernel
            .submit(SubmitTurn {
                turn_id: client_turn_id(),
                session: SubmitSession::Fresh(
                    PreparedFreshSession::new(session_header, pin).unwrap(),
                ),
                text: "cannot fit".into(),
                model: None,
                sandbox: None,
            })
            .await,
        Err(TurnError::Capacity)
    );
    assert_eq!(drops.load(Ordering::Acquire), 1);
    assert!(store.header(&session_id).await.is_err());
}

#[tokio::test]
async fn cancelled_hydration_leader_settles_followers_and_releases_capacity() {
    let memory = Arc::new(MemoryStore::new());
    append_terminal_history(&memory, "session-cancelled-hydration", 1).await;
    let store = Arc::new(FactReadRaceStore::new(memory));
    let store_contract: Arc<dyn SessionStore> = store.clone();
    let kernel =
        SessionKernel::recover_with_clock(store_contract, composition(), Arc::new(FixedClock))
            .await
            .unwrap();
    store.pause_next_open_turn_read();
    let leader = tokio::spawn({
        let kernel = kernel.clone();
        async move {
            let session_id = SessionId::new("session-cancelled-hydration").unwrap();
            let prepared = kernel.prepare_resume(&session_id).await?;
            kernel
                .submit(SubmitTurn {
                    turn_id: client_turn_id(),
                    session: SubmitSession::Resume(prepared),
                    text: "leader".into(),
                    model: None,
                    sandbox: None,
                })
                .await
        }
    });
    store.wait_until_open_turn_read_is_captured().await;
    let follower = tokio::spawn({
        let kernel = kernel.clone();
        async move {
            let session_id = SessionId::new("session-cancelled-hydration").unwrap();
            let prepared = kernel.prepare_resume(&session_id).await?;
            kernel
                .submit(SubmitTurn {
                    turn_id: client_turn_id(),
                    session: SubmitSession::Resume(prepared),
                    text: "follower".into(),
                    model: None,
                    sandbox: None,
                })
                .await
        }
    });
    tokio::task::yield_now().await;
    leader.abort();
    let _ = leader.await;
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(100), follower)
            .await
            .expect("a cancelled leader must settle its followers")
            .unwrap()
            .is_err()
    );

    let worker = kernel.start_write_behind();
    kernel
        .submit(SubmitTurn {
            turn_id: client_turn_id(),
            session: resume(
                &kernel,
                SessionId::new("session-cancelled-hydration").unwrap(),
            )
            .await,
            text: "retry".into(),
            model: None,
            sandbox: None,
        })
        .await
        .expect("a later hydration attempt must be admitted");
    kernel.shutdown(worker).await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn cold_resume_resolves_its_header_before_resident_capacity_rejection() {
    let memory = Arc::new(MemoryStore::new());
    append_terminal_history(&memory, "session-capacity-cold-resume", 1).await;
    let store = Arc::new(FactReadRaceStore::new(memory));
    let store_contract: Arc<dyn SessionStore> = store.clone();
    let kernel =
        SessionKernel::recover_with_clock(store_contract, composition(), Arc::new(FixedClock))
            .await
            .unwrap();
    let worker = kernel.start_write_behind();
    for index in 0..MAXIMUM_ACTIVE_SESSIONS {
        submit(&kernel, &format!("session-resident-{index}"), "queued").await;
    }
    store.reset_header_read_attempts();

    assert_eq!(
        kernel
            .submit(SubmitTurn {
                turn_id: client_turn_id(),
                session: resume(
                    &kernel,
                    SessionId::new("session-capacity-cold-resume").unwrap(),
                )
                .await,
                text: "must resolve the durable preset first".into(),
                model: None,
                sandbox: None,
            })
            .await,
        Err(TurnError::Capacity)
    );
    assert_eq!(
        store.header_read_attempts(),
        1,
        "cold resume must read its durable preset before resident admission"
    );
    kernel.shutdown(worker).await.unwrap();
}
