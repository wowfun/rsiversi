use super::*;
use rsi_agent_session_protocol::{ModelAttemptBuilder, ModelAttemptLog, ModelEventPurpose};
use rsi_agent_store_protocol::MAXIMUM_STORE_FACT_PAGE_BYTES as FACT_PAGE_BYTES;
use rsi_agent_turn_protocol::ModelLiveStatus;

#[test]
fn live_model_reservations_allow_host_growth_but_reject_fractional_attempts() {
    for bytes in [64 * 1024 * 1024, 256 * 1024 * 1024, 512 * 1024 * 1024] {
        assert!(
            KernelLimits {
                maximum_live_model_bytes: bytes,
                ..KernelLimits::default()
            }
            .validate()
            .is_ok()
        );
    }
    for bytes in [0, 1, 65 * 1024 * 1024] {
        assert!(
            KernelLimits {
                maximum_live_model_bytes: bytes,
                ..KernelLimits::default()
            }
            .validate()
            .is_err()
        );
    }
}

pub(super) fn source_bodies(turn: &TurnId) -> Vec<SessionFactBody> {
    let effect = EffectId::new("cut").unwrap();
    vec![
        model_intent_fact(1, turn, &effect).body().clone(),
        model_started_fact(2, turn, &effect).body().clone(),
        SessionFactBody::ModelAttemptSettled {
            turn_id: turn.clone(),
            effect_id: effect,
            purpose: ModelEventPurpose::Conversation,
            attempt: ModelAttemptLog::from_events(
                [(
                    0,
                    LanguageEvent::Finished {
                        reason: rsi_ai_protocol::FinishReason::MaxTokens,
                        replay: None,
                    },
                )],
                true,
            )
            .unwrap(),
        },
    ]
}

#[tokio::test]
async fn model_identity_is_per_turn_and_rejected_before_live_publication() {
    for sqlite in [false, true] {
        let directory = tempfile::tempdir().unwrap();
        let store: Arc<dyn SessionStore> = if sqlite {
            Arc::new(rsi_agent_store_sqlite::SqliteStore::open(directory.path()).unwrap())
        } else {
            Arc::new(MemoryStore::new())
        };
        let kernel =
            AgentKernel::recover_with_clock(store.clone(), composition(), Arc::new(FixedClock))
                .await
                .unwrap();
        let workers = kernel.start_workers();
        let worker = kernel.register("worker".into()).unwrap();
        for turn_index in 0..2 {
            let session = if turn_index == 0 {
                fresh(header("identity"))
            } else {
                resume(&kernel, SessionId::new("identity").unwrap()).await
            };
            kernel
                .submit(SubmitTurn {
                    reasoning_effort: None,
                    turn_id: client_turn_id(),
                    session,
                    text: "work".into(),
                    model: None,
                    sandbox: None,
                })
                .await
                .unwrap();
            let claim = kernel
                .claim("worker", CancellationToken::new())
                .await
                .unwrap()
                .unwrap();
            for body in source_bodies(claim.turn_id()) {
                tool_origin::flush_bodies(&kernel, &claim, vec![body]).await;
            }
            let before = store.read_watermarks(claim.session_id()).await.unwrap();
            assert!(matches!(
                kernel
                    .publish(&claim, vec![source_bodies(claim.turn_id()).remove(0)])
                    .await,
                Err(TurnError::Invalid(_))
            ));
            assert_eq!(
                store.read_watermarks(claim.session_id()).await.unwrap(),
                before
            );
            kernel
                .finish_turn(&claim, &TurnOutcome::MaxTokens)
                .await
                .unwrap();
        }
        drop(worker);
        kernel.shutdown(workers).await.unwrap();
    }
}

#[tokio::test]
async fn host_can_admit_more_than_four_simultaneous_model_attempts() {
    let kernel = AgentKernel::recover_with_clock_and_limits(
        Arc::new(MemoryStore::new()),
        composition(),
        Arc::new(FixedClock),
        KernelLimits {
            maximum_live_model_bytes: 512 * 1024 * 1024,
            ..KernelLimits::default()
        },
    )
    .await
    .unwrap();
    let workers = kernel.start_workers();
    let worker = kernel.register("worker".into()).unwrap();
    let mut active = Vec::new();
    for index in 0..6 {
        submit(&kernel, &format!("model-{index}"), "work").await;
        let claim = kernel
            .claim("worker", CancellationToken::new())
            .await
            .unwrap()
            .unwrap();
        let lease = tokio::time::timeout(
            std::time::Duration::from_secs(2),
            kernel.reserve_model_attempt(
                &claim,
                EffectId::new("attempt").unwrap(),
                ModelEventPurpose::Conversation,
                CancellationToken::new(),
            ),
        )
        .await
        .expect("configured reservations admit all six without waiting")
        .unwrap();
        active.push((claim, lease));
    }
    for (claim, lease) in active {
        drop(lease);
        kernel
            .cancel(claim.session_id(), claim.turn_id(), None)
            .await
            .unwrap();
        kernel
            .finish_turn(&claim, &TurnOutcome::Cancelled)
            .await
            .unwrap();
    }
    drop(worker);
    kernel.shutdown(workers).await.unwrap();
}

#[tokio::test]
async fn memory_and_sqlite_enforce_model_coordinates_atomically_across_append_paths() {
    use rsi_agent_store_protocol::{AtomicAgentCommit, AtomicSessionAppend};
    for sqlite in [false, true] {
        let directory = tempfile::tempdir().unwrap();
        let store: Arc<dyn SessionStore> = if sqlite {
            Arc::new(rsi_agent_store_sqlite::SqliteStore::open(directory.path()).unwrap())
        } else {
            Arc::new(MemoryStore::new())
        };
        let session = SessionId::new("uniqueness").unwrap();
        let turn = TurnId::new("turn").unwrap();
        let bodies = source_bodies(&turn);
        let mut facts = vec![Arc::new(accepted_fact(1, &turn))];
        facts.extend(bodies.iter().enumerate().map(|(index, body)| {
            Arc::new(SessionFact::new(index as u64 + 2, 1, body.clone()).unwrap())
        }));
        store
            .append(AppendBatch {
                session_id: session.clone(),
                expected_seq: 0,
                header: Some(header(session.as_str())),
                facts,
            })
            .await
            .unwrap();
        let before = store.read_watermarks(&session).await.unwrap();
        for duplicate in &bodies {
            let fact = Arc::new(SessionFact::new(5, 1, duplicate.clone()).unwrap());
            assert!(
                store
                    .append(AppendBatch {
                        session_id: session.clone(),
                        expected_seq: 4,
                        header: None,
                        facts: vec![fact.clone()]
                    })
                    .await
                    .is_err()
            );
            assert_eq!(store.read_watermarks(&session).await.unwrap(), before);
            assert!(
                store
                    .commit_agent(AtomicAgentCommit {
                        sessions: vec![AtomicSessionAppend {
                            session_id: session.clone(),
                            expected_fact_seq: 4,
                            expected_control_seq: 0,
                            header: None,
                            facts: vec![fact],
                            controls: vec![],
                        }],
                        required_active_activations: vec![],
                        quiescent_descendants_of: None
                    })
                    .await
                    .is_err()
            );
            assert_eq!(store.read_watermarks(&session).await.unwrap(), before);
        }
        let fresh = SessionFactBody::ModelStarted {
            turn_id: turn,
            effect_id: EffectId::new("new").unwrap(),
        };
        assert!(
            store
                .append(AppendBatch {
                    session_id: session.clone(),
                    expected_seq: 4,
                    header: None,
                    facts: vec![
                        Arc::new(SessionFact::new(5, 1, fresh.clone()).unwrap()),
                        Arc::new(SessionFact::new(6, 1, fresh.clone()).unwrap())
                    ]
                })
                .await
                .is_err()
        );
        assert_eq!(store.read_watermarks(&session).await.unwrap(), before);
        // Rejected candidates must not poison the uniqueness index.
        store
            .append(AppendBatch {
                session_id: session.clone(),
                expected_seq: 4,
                header: None,
                facts: vec![Arc::new(SessionFact::new(5, 1, fresh).unwrap())],
            })
            .await
            .unwrap();
        assert_eq!(
            store
                .read_facts(&session, 0, 10, FACT_PAGE_BYTES)
                .await
                .unwrap()
                .facts
                .len(),
            5
        );
    }
}

#[tokio::test]
#[allow(clippy::too_many_lines)] // One lifecycle rejects invalid writes before proving exact durable commitment.
async fn settlement_and_live_commit_require_exact_started_identity_and_durable_sequence() {
    let store = Arc::new(MemoryStore::new());
    let kernel = kernel(store.clone()).await;
    let workers = kernel.start_workers();
    submit(&kernel, "attempt", "work").await;
    let worker = kernel.register("worker".into()).unwrap();
    let claim = kernel
        .claim("worker", CancellationToken::new())
        .await
        .unwrap()
        .unwrap();
    let effect = EffectId::new("cut").unwrap();
    let mut stream = kernel.observe_models(claim.session_id()).await.unwrap();
    let mut live = kernel
        .reserve_model_attempt(
            &claim,
            effect.clone(),
            ModelEventPurpose::Conversation,
            CancellationToken::new(),
        )
        .await
        .unwrap();
    assert!(live.started().is_err());
    assert!(
        live.publish(rsi_agent_session_protocol::ModelAttemptPrefix::default())
            .is_err()
    );
    assert!(
        kernel
            .reserve_model_attempt(
                &claim,
                effect.clone(),
                ModelEventPurpose::Conversation,
                CancellationToken::new()
            )
            .await
            .is_err()
    );
    let mut bodies = source_bodies(claim.turn_id());
    let mut settlement = bodies.pop().unwrap();
    assert!(
        kernel
            .publish(&claim, vec![settlement.clone()])
            .await
            .is_err()
    );
    tool_origin::flush_bodies(&kernel, &claim, vec![bodies.remove(0)]).await;
    for invalid in [
        SessionFactBody::ModelStarted {
            turn_id: claim.turn_id().clone(),
            effect_id: EffectId::new("missing-effect").unwrap(),
        },
        SessionFactBody::ImageStarted {
            turn_id: claim.turn_id().clone(),
            effect_id: effect.clone(),
        },
    ] {
        assert!(kernel.publish(&claim, vec![invalid]).await.is_err());
    }
    assert!(
        kernel
            .publish(&claim, vec![settlement.clone()])
            .await
            .is_err()
    );
    tool_origin::flush_bodies(&kernel, &claim, bodies.clone()).await;
    assert!(kernel.publish(&claim, bodies).await.is_err());
    live.started().unwrap();
    assert!(live.started().is_err());
    let mut builder = ModelAttemptBuilder::new();
    builder
        .push(
            0,
            &LanguageEvent::ContentStarted {
                index: 0,
                content: ContentStart::Text,
            },
        )
        .unwrap();
    builder
        .push(
            1,
            &LanguageEvent::ContentDelta {
                index: 0,
                delta: ContentDelta::Text("界".into()),
            },
        )
        .unwrap();
    live.publish(builder.prefix()).unwrap();
    let large_delta = LanguageEvent::ContentDelta {
        index: 0,
        delta: ContentDelta::Text("x".repeat(128 * 1024)),
    };
    while builder.push(1, &large_delta).is_ok() {}
    builder
        .push(
            2,
            &LanguageEvent::Finished {
                reason: rsi_ai_protocol::FinishReason::MaxTokens,
                replay: None,
            },
        )
        .unwrap();
    if let SessionFactBody::ModelAttemptSettled { attempt, .. } = &mut settlement {
        *attempt = builder.finish(true).unwrap();
    }
    let preview = stream.next().await.unwrap().unwrap();
    assert_eq!(preview.as_ref().prefix.event_count(), 2);
    assert_eq!(preview.as_ref().status, ModelLiveStatus::Streaming);
    for (turn, effect, purpose) in [
        (
            TurnId::new("foreign").unwrap(),
            EffectId::new("cut").unwrap(),
            ModelEventPurpose::Conversation,
        ),
        (
            claim.turn_id().clone(),
            EffectId::new("foreign").unwrap(),
            ModelEventPurpose::Conversation,
        ),
        (
            claim.turn_id().clone(),
            EffectId::new("cut").unwrap(),
            ModelEventPurpose::ContextCompaction,
        ),
    ] {
        let mut invalid = settlement.clone();
        if let SessionFactBody::ModelAttemptSettled {
            turn_id,
            effect_id,
            purpose: p,
            ..
        } = &mut invalid
        {
            *turn_id = turn;
            *effect_id = effect;
            *p = purpose;
        }
        assert!(kernel.publish(&claim, vec![invalid]).await.is_err());
    }
    assert!(
        kernel
            .publish(&claim, vec![settlement.clone(), settlement.clone()])
            .await
            .is_err()
    );
    let PublishAttempt::Published(receipt) = kernel
        .publish(&claim, vec![settlement.clone()])
        .await
        .unwrap()
    else {
        panic!("settlement fits");
    };
    assert!(
        live.committed(&receipt).is_err(),
        "acceptance is not durability"
    );
    let seq = receipt.facts().last().unwrap().seq();
    kernel.flush(&claim, seq).await.unwrap();
    let foreign = rsi_agent_turn_protocol::TurnClaimIssuer::new();
    assert!(foreign.published(&claim, receipt.facts().to_vec()).is_err());
    let foreign_claim = foreign.issue(
        claim.executor_id().into(),
        claim.claim_id(),
        claim.session_id().clone(),
        claim.turn_id().clone(),
        Arc::new(claim.header().clone()),
        claim.accepted_at_ms(),
        claim.accepted_seq(),
        claim.live_seq(),
    );
    let forged = foreign
        .published(&foreign_claim, receipt.facts().to_vec())
        .unwrap();
    assert!(matches!(
        live.committed(&forged),
        Err(TurnError::StaleClaim)
    ));
    store.take_fact_read_cursors();
    live.committed(&receipt).unwrap();
    assert!(
        store.take_fact_read_cursors().is_empty(),
        "acknowledgement must not reread result bodies"
    );
    assert!(live.committed(&receipt).is_err());
    let committed = stream.next().await.unwrap().unwrap();
    assert_eq!(
        committed.as_ref().status,
        ModelLiveStatus::Committed { fact_seq: seq }
    );
    assert_eq!(committed.as_ref().prefix.event_count(), 0);
    assert!(
        kernel
            .publish(&claim, vec![settlement.clone()])
            .await
            .is_err()
    );
    kernel
        .finish_turn(&claim, &TurnOutcome::MaxTokens)
        .await
        .unwrap();
    assert!(kernel.publish(&claim, vec![settlement]).await.is_err());
    drop((live, preview, committed, stream, worker));
    kernel.shutdown(workers).await.unwrap();
}

#[tokio::test]
async fn receipts_reject_other_claims_effects_purposes_and_retired_executors() {
    for (effect, purpose) in [
        ("other", ModelEventPurpose::Conversation),
        ("cut", ModelEventPurpose::ContextCompaction),
        ("cut", ModelEventPurpose::Conversation),
    ] {
        let kernel = kernel(Arc::new(MemoryStore::new())).await;
        let workers = kernel.start_workers();
        let worker = kernel.register("worker".into()).unwrap();
        submit(&kernel, "first", "work").await;
        let claim = kernel
            .claim("worker", CancellationToken::new())
            .await
            .unwrap()
            .unwrap();
        let mut live = kernel
            .reserve_model_attempt(
                &claim,
                EffectId::new(effect).unwrap(),
                purpose,
                CancellationToken::new(),
            )
            .await
            .unwrap();
        let mut bodies = source_bodies(claim.turn_id());
        let settlement = bodies.pop().unwrap();
        for body in bodies {
            tool_origin::flush_bodies(&kernel, &claim, vec![body]).await;
        }
        let PublishAttempt::Published(receipt) =
            kernel.publish(&claim, vec![settlement]).await.unwrap()
        else {
            panic!("fits");
        };
        kernel
            .flush(&claim, receipt.facts().last().unwrap().seq())
            .await
            .unwrap();
        if effect != "cut" || purpose != ModelEventPurpose::Conversation {
            assert!(live.committed(&receipt).is_err());
        }
        submit(&kernel, "second", "work").await;
        let other = kernel
            .claim("worker", CancellationToken::new())
            .await
            .unwrap()
            .unwrap();
        let PublishAttempt::Published(other_receipt) = kernel
            .publish(&other, vec![source_bodies(other.turn_id()).remove(0)])
            .await
            .unwrap()
        else {
            panic!("fits");
        };
        kernel
            .flush(&other, other_receipt.facts().last().unwrap().seq())
            .await
            .unwrap();
        assert!(matches!(
            live.committed(&other_receipt),
            Err(TurnError::StaleClaim)
        ));
        drop(worker);
        assert!(matches!(
            live.committed(&receipt),
            Err(TurnError::StaleClaim)
        ));
        drop(live);
        kernel.shutdown(workers).await.unwrap();
    }
}

#[tokio::test(start_paused = true)]
async fn live_pool_reserves_before_dispatch_and_has_cancellable_bounded_admission() {
    let store = Arc::new(MemoryStore::new());
    let kernel = AgentKernel::recover_with_clock_and_limits(
        store.clone(),
        composition(),
        Arc::new(FixedClock),
        KernelLimits {
            maximum_live_model_bytes: 64 * 1024 * 1024,
            ..KernelLimits::default()
        },
    )
    .await
    .unwrap();
    let workers = kernel.start_workers();
    let worker = kernel.register("worker".into()).unwrap();
    submit(&kernel, "first", "work").await;
    let first = kernel
        .claim("worker", CancellationToken::new())
        .await
        .unwrap()
        .unwrap();
    submit(&kernel, "second", "work").await;
    let second = kernel
        .claim("worker", CancellationToken::new())
        .await
        .unwrap()
        .unwrap();
    let effect = EffectId::new("attempt").unwrap();
    let live = kernel
        .reserve_model_attempt(
            &first,
            effect.clone(),
            ModelEventPurpose::Conversation,
            CancellationToken::new(),
        )
        .await
        .unwrap();
    let cancellation = CancellationToken::new();
    let mut waiting = Box::pin(kernel.reserve_model_attempt(
        &second,
        effect.clone(),
        ModelEventPurpose::Conversation,
        cancellation.clone(),
    ));
    assert!(futures_util::poll!(waiting.as_mut()).is_pending());
    cancellation.cancel();
    assert!(matches!(waiting.await, Err(TurnError::Cancelled)));
    let before = tokio::time::Instant::now();
    assert!(matches!(
        kernel
            .reserve_model_attempt(
                &second,
                effect.clone(),
                ModelEventPurpose::Conversation,
                CancellationToken::new()
            )
            .await,
        Err(TurnError::Capacity)
    ));
    assert_eq!(
        tokio::time::Instant::now() - before,
        std::time::Duration::from_mins(1)
    );
    assert_eq!(
        store
            .read_facts(second.session_id(), 0, 10, FACT_PAGE_BYTES)
            .await
            .unwrap()
            .facts
            .len(),
        1
    );
    drop(live);
    let replacement = kernel
        .reserve_model_attempt(
            &second,
            effect.clone(),
            ModelEventPurpose::Conversation,
            CancellationToken::new(),
        )
        .await
        .unwrap();
    drop(replacement);
    kernel
        .cancel(first.session_id(), first.turn_id(), None)
        .await
        .unwrap();
    kernel
        .cancel(second.session_id(), second.turn_id(), None)
        .await
        .unwrap();
    kernel
        .finish_turn(&first, &TurnOutcome::Cancelled)
        .await
        .unwrap();
    kernel
        .finish_turn(&second, &TurnOutcome::Cancelled)
        .await
        .unwrap();
    drop(worker);
    kernel.shutdown(workers).await.unwrap();
}

#[tokio::test]
async fn cold_recovery_at_attempt_boundaries_interrupts_without_replaying() {
    for boundary in 1..=3 {
        let store = Arc::new(MemoryStore::new());
        let session = SessionId::new(format!("cold-{boundary}")).unwrap();
        let turn = TurnId::new("turn").unwrap();
        let mut facts = vec![accepted_fact(1, &turn)];
        for body in source_bodies(&turn).into_iter().take(boundary) {
            let seq = facts.len() as u64 + 1;
            facts.push(SessionFact::new(seq, seq, body).unwrap());
        }
        store
            .append(AppendBatch {
                session_id: session.clone(),
                expected_seq: 0,
                header: Some(header(session.as_str())),
                facts: facts.into_iter().map(Into::into).collect(),
            })
            .await
            .unwrap();
        let kernel = AgentKernel::recover_with_clock(store, composition(), Arc::new(FixedClock))
            .await
            .unwrap();
        assert!(matches!(
            kernel.outcome(&session, &turn).await.unwrap(),
            Some(TurnOutcome::Interrupted { .. })
        ));
        let worker = kernel.register("no-replay".into()).unwrap();
        let cancellation = CancellationToken::new();
        cancellation.cancel();
        assert!(
            kernel
                .claim("no-replay", cancellation)
                .await
                .unwrap()
                .is_none()
        );
        drop(worker);
    }
}
