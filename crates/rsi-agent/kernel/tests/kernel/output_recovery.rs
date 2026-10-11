use super::*;

pub(super) fn source_bodies(turn: &TurnId) -> Vec<SessionFactBody> {
    let effect = EffectId::new("cut").unwrap();
    vec![
        SessionFactBody::ModelIntent {
            turn_id: turn.clone(),
            effect_id: effect.clone(),
            snapshot: snapshot(),
            purpose: rsi_agent_session_protocol::ModelPurpose::Conversation,
            price_quote: None,
            evidence: rsi_agent_session_protocol::RequestEvidence::Unavailable {
                reason: rsi_agent_session_protocol::EvidenceUnavailable::NotCaptured,
            },
        },
        SessionFactBody::ModelStarted {
            turn_id: turn.clone(),
            effect_id: effect.clone(),
        },
        SessionFactBody::ModelEvent {
            turn_id: turn.clone(),
            effect_id: effect,
            purpose: rsi_agent_session_protocol::ModelEventPurpose::Conversation,
            event: LanguageEvent::Finished {
                reason: rsi_ai_protocol::FinishReason::MaxTokens,
                replay: None,
            },
        },
    ]
}

fn marker(turn: &TurnId, source: &str, ordinal: u64) -> SessionFactBody {
    SessionFactBody::ModelOutputRecovery {
        turn_id: turn.clone(),
        source_model_effect_id: EffectId::new(source).unwrap(),
        ordinal,
    }
}

#[tokio::test]
async fn output_recovery_retains_generated_fact_byte_charges() {
    let turn = TurnId::new("byte-turn").unwrap();
    let mut bodies = source_bodies(&turn);
    bodies.push(marker(&turn, "cut", 1));
    let limit: u64 = bodies
        .iter()
        .enumerate()
        .map(|(index, body)| {
            SessionFact::new(index as u64 + 2, 42, body.clone())
                .unwrap()
                .encoded_len() as u64
        })
        .sum();
    let mut wire = serde_json::to_value(header("output-bytes")).unwrap();
    wire["settings"]["turn_budget"]["maximum_generated_record_bytes"] = serde_json::json!(limit);
    let store = Arc::new(MemoryStore::new());
    let kernel = kernel(store.clone()).await;
    let workers = kernel.start_workers();
    kernel
        .submit(SubmitTurn {
            turn_id: turn,
            session: fresh(serde_json::from_value(wire).unwrap()),
            text: "work".into(),
            model: None,
            reasoning_effort: None,
            sandbox: None,
        })
        .await
        .unwrap();
    let lease = kernel.register("byte-worker".into()).unwrap();
    let claim = kernel
        .claim("byte-worker", CancellationToken::new())
        .await
        .unwrap()
        .unwrap();
    for body in bodies {
        tool_origin::flush_bodies(&kernel, &claim, vec![body]).await;
    }
    let mut next = source_bodies(claim.turn_id()).remove(0);
    if let SessionFactBody::ModelIntent { effect_id, .. } = &mut next {
        *effect_id = EffectId::new("next").unwrap();
    }
    let next_bytes = SessionFact::new(6, 42, next.clone()).unwrap().encoded_len() as u64;
    assert!(matches!(
        kernel.publish(&claim, vec![next]).await,
        Err(TurnError::BudgetExceeded {
            dimension: BudgetDimension::GeneratedRecordBytes,
            consumed,
            limit: actual_limit,
        }) if consumed == limit + next_bytes && actual_limit == limit
    ));
    assert_eq!(
        store
            .read_facts(claim.session_id(), 0, 16)
            .await
            .unwrap()
            .facts
            .len(),
        5,
        "over-budget successor must not become durable"
    );
    drop(lease);
    kernel.shutdown(workers).await.unwrap();
}

#[tokio::test]
async fn multi_marker_publication_is_rejected_without_consuming_allowance() {
    let store = Arc::new(MemoryStore::new());
    let kernel = kernel(store.clone()).await;
    let workers = kernel.start_workers();
    let mut wire = serde_json::to_value(header("multi-recovery")).unwrap();
    wire["settings"]["turn_budget"]["maximum_output_recoveries"] = serde_json::json!(1);
    kernel
        .submit(SubmitTurn {
            turn_id: TurnId::new("turn").unwrap(),
            session: fresh(serde_json::from_value(wire).unwrap()),
            text: "work".into(),
            model: None,
            reasoning_effort: None,
            sandbox: None,
        })
        .await
        .unwrap();
    let lease = kernel.register("multi-worker".into()).unwrap();
    let claim = kernel
        .claim("multi-worker", CancellationToken::new())
        .await
        .unwrap()
        .unwrap();
    for body in source_bodies(claim.turn_id()) {
        tool_origin::flush_bodies(&kernel, &claim, vec![body]).await;
    }
    let before = store
        .read_facts(claim.session_id(), 0, 256)
        .await
        .unwrap()
        .facts
        .len();
    let mut batch = vec![marker(claim.turn_id(), "cut", 1)];
    let mut next = source_bodies(claim.turn_id());
    for body in &mut next {
        match body {
            SessionFactBody::ModelIntent { effect_id, .. }
            | SessionFactBody::ModelStarted { effect_id, .. }
            | SessionFactBody::ModelEvent { effect_id, .. } => {
                *effect_id = EffectId::new("next").unwrap()
            }
            _ => unreachable!(),
        }
    }
    batch.extend(next);
    batch.push(marker(claim.turn_id(), "next", 2));
    let result = kernel.publish(&claim, batch).await;
    assert!(
        matches!(result, Err(TurnError::Invalid(ref message))
        if message == "executor publication admits at most one output recovery marker"),
        "{result:?}"
    );
    assert_eq!(
        store
            .read_facts(claim.session_id(), 0, 256)
            .await
            .unwrap()
            .facts
            .len(),
        before
    );
    tool_origin::flush_bodies(&kernel, &claim, vec![marker(claim.turn_id(), "cut", 1)]).await;
    drop(lease);
    kernel.shutdown(workers).await.unwrap();
}

#[tokio::test]
async fn recovery_provenance_is_atomic_and_cannot_be_reused_or_cross_turn() {
    let store = Arc::new(MemoryStore::new());
    let kernel = kernel(store.clone()).await;
    let workers = kernel.start_workers();
    submit(&kernel, "output-origin", "work").await;
    let lease = kernel.register("output-worker".into()).unwrap();
    let claim = kernel
        .claim("output-worker", CancellationToken::new())
        .await
        .unwrap()
        .unwrap();
    for body in source_bodies(claim.turn_id()) {
        tool_origin::flush_bodies(&kernel, &claim, vec![body]).await;
    }
    let before = store
        .read_facts(claim.session_id(), 0, 256)
        .await
        .unwrap()
        .facts
        .len();
    for body in [
        marker(claim.turn_id(), "other", 1),
        marker(claim.turn_id(), "cut", 2),
        marker(&TurnId::new("foreign").unwrap(), "cut", 1),
    ] {
        assert!(kernel.publish(&claim, vec![body]).await.is_err());
    }
    assert_eq!(
        store
            .read_facts(claim.session_id(), 0, 256)
            .await
            .unwrap()
            .facts
            .len(),
        before
    );
    tool_origin::flush_bodies(&kernel, &claim, vec![marker(claim.turn_id(), "cut", 1)]).await;
    assert!(
        kernel
            .publish(&claim, vec![marker(claim.turn_id(), "cut", 2)])
            .await
            .is_err()
    );
    drop(lease);
    kernel.shutdown(workers).await.unwrap();
}

#[tokio::test]
async fn cold_recovery_at_every_truncation_boundary_interrupts_without_requeue() {
    for boundary in 0..4 {
        let store = Arc::new(MemoryStore::new());
        let session = SessionId::new(format!("output-cold-{boundary}")).unwrap();
        let turn = TurnId::new("turn").unwrap();
        let mut bodies = source_bodies(&turn);
        if boundary > 0 {
            bodies.push(marker(&turn, "cut", 1));
        }
        if boundary > 1 {
            let mut next = source_bodies(&turn);
            for body in &mut next {
                match body {
                    SessionFactBody::ModelIntent { effect_id, .. }
                    | SessionFactBody::ModelStarted { effect_id, .. }
                    | SessionFactBody::ModelEvent { effect_id, .. } => {
                        *effect_id = EffectId::new("next").unwrap()
                    }
                    _ => unreachable!(),
                }
            }
            bodies.extend(next.into_iter().take((boundary - 1) as usize));
        }
        let mut facts = vec![accepted_fact(1, &turn)];
        for body in bodies {
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
        let lease = kernel.register("no-replay".into()).unwrap();
        let cancellation = CancellationToken::new();
        cancellation.cancel();
        assert!(
            kernel
                .claim("no-replay", cancellation)
                .await
                .unwrap()
                .is_none()
        );
        drop(lease);
    }
}
