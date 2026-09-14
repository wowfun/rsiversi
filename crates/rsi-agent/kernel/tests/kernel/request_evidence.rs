use super::*;
use rsi_agent_session_protocol::{EvidencePart, EvidenceSection, RequestEvidence};
use rsi_agent_turn_protocol::TurnClaim;

fn package(text: String) -> RequestEvidence {
    RequestEvidence::Available {
        configuration: EvidencePart::inline("{}".into()),
        system: EvidencePart::inline(text),
        tools: EvidencePart::inline("[]".into()),
        manifest: vec![],
    }
}
fn body(claim: &TurnClaim, id: &str, evidence: RequestEvidence) -> SessionFactBody {
    SessionFactBody::ModelIntent {
        turn_id: claim.turn_id().clone(),
        effect_id: EffectId::new(id).unwrap(),
        snapshot: snapshot(),
        purpose: rsi_agent_session_protocol::ModelPurpose::Conversation,
        price_quote: None,
        evidence,
    }
}
async fn publish_close(
    kernel: &AgentKernel,
    claim: &TurnClaim,
    id: &str,
    evidence: RequestEvidence,
) -> u64 {
    let facts = kernel
        .publish(claim, vec![body(claim, id, evidence)])
        .await
        .unwrap()
        .published();
    let seq = facts[0].seq();
    kernel.flush(claim, seq).await.unwrap();
    let facts = kernel
        .publish(
            claim,
            vec![SessionFactBody::ModelStarted {
                turn_id: claim.turn_id().clone(),
                effect_id: EffectId::new(id).unwrap(),
            }],
        )
        .await
        .unwrap()
        .published();
    kernel.flush(claim, facts[0].seq()).await.unwrap();
    let facts = kernel
        .publish(
            claim,
            vec![SessionFactBody::ModelEvent {
                turn_id: claim.turn_id().clone(),
                effect_id: EffectId::new(id).unwrap(),
                purpose: rsi_agent_session_protocol::ModelEventPurpose::Conversation,
                event: LanguageEvent::Finished {
                    reason: rsi_ai_protocol::FinishReason::Stop,
                    replay: None,
                },
            }],
        )
        .await
        .unwrap()
        .published();
    kernel.flush(claim, facts[0].seq()).await.unwrap();
    seq
}
fn reference(seq: u64, original: &RequestEvidence) -> RequestEvidence {
    let mut evidence = package("unused".into());
    let source = original.part(EvidenceSection::System).unwrap();
    *evidence.part_mut(EvidenceSection::System).unwrap() = EvidencePart::Reference {
        seq,
        section: EvidenceSection::System,
        sha256: source.sha256().into(),
        bytes: u32::try_from(source.bytes()).unwrap(),
    };
    evidence
}

fn all_references(seq: u64, original: &RequestEvidence) -> RequestEvidence {
    let mut all = original.clone();
    for section in [
        EvidenceSection::Configuration,
        EvidenceSection::System,
        EvidenceSection::Tools,
    ] {
        let part = original.part(section).unwrap();
        *all.part_mut(section).unwrap() = EvidencePart::Reference {
            seq,
            section,
            sha256: part.sha256().into(),
            bytes: u32::try_from(part.bytes()).unwrap(),
        };
    }
    all
}

#[tokio::test]
async fn evidence_references_require_original_inline_bytes_in_the_same_session() {
    let store = Arc::new(MemoryStore::new());
    let kernel = kernel(store.clone()).await;
    let worker = kernel.start_workers();
    submit(&kernel, "evidence-source", "inspect").await;
    let lease = kernel.register("evidence-worker".into()).unwrap();
    let claim = kernel
        .claim("evidence-worker", CancellationToken::new())
        .await
        .unwrap()
        .unwrap();
    let original = package("unique system source".into());
    let seq = publish_close(&kernel, &claim, "original", original.clone()).await;
    let all = all_references(seq, &original);
    store.take_fact_read_cursors();
    publish_close(&kernel, &claim, "all-sections", all).await;
    assert_eq!(
        store
            .take_fact_read_cursors()
            .iter()
            .filter(|cursor| **cursor == seq - 1)
            .count(),
        1,
        "all sections referencing the same intent must share one durable read"
    );
    store.take_fact_read_cursors();
    let second = publish_close(&kernel, &claim, "reference", reference(seq, &original)).await;
    assert!(
        store.take_fact_read_cursors().is_empty(),
        "a previously validated immutable original must not be read again"
    );
    assert!(
        matches!(kernel.publish(&claim,vec![body(&claim,"chain",reference(second,&original))]).await,Err(TurnError::Invalid(message)) if message.contains("original inline"))
    );
    let mut zero = reference(0, &original);
    store.take_fact_read_cursors();
    assert!(
        kernel
            .publish(&claim, vec![body(&claim, "zero", zero.clone())])
            .await
            .is_err()
    );
    assert!(store.take_fact_read_cursors().is_empty());
    if let EvidencePart::Reference { seq: target, .. } =
        zero.part_mut(EvidenceSection::System).unwrap()
    {
        *target = u64::MAX;
    }
    assert!(
        kernel
            .publish(&claim, vec![body(&claim, "future", zero)])
            .await
            .is_err()
    );
    let mut mismatch = reference(seq, &original);
    if let EvidencePart::Reference { sha256, .. } =
        mismatch.part_mut(EvidenceSection::System).unwrap()
    {
        *sha256 = "0".repeat(64);
    }
    assert!(
        kernel
            .publish(&claim, vec![body(&claim, "mismatch", mismatch)])
            .await
            .is_err()
    );
    let facts = kernel
        .publish(
            &claim,
            vec![SessionFactBody::TurnTerminal {
                turn_id: claim.turn_id().clone(),
                outcome: TurnOutcome::Completed,
            }],
        )
        .await
        .unwrap()
        .published();
    kernel.flush(&claim, facts[0].seq()).await.unwrap();
    assert!(matches!(kernel.release(&claim), Err(TurnError::StaleClaim)));
    submit(&kernel, "other-evidence-session", "inspect").await;
    let other = kernel
        .claim("evidence-worker", CancellationToken::new())
        .await
        .unwrap()
        .unwrap();
    assert!(
        kernel
            .publish(
                &other,
                vec![body(&other, "foreign", reference(seq, &original))]
            )
            .await
            .is_err()
    );
    kernel.release(&other).unwrap();
    drop(lease);
    kernel.shutdown(worker).await.unwrap();
}

#[tokio::test]
async fn inline_budget_rejects_before_publication_and_recovery_preserves_evidence() {
    let store = Arc::new(MemoryStore::new());
    let initial = kernel(store.clone()).await;
    let worker = initial.start_workers();
    submit(&initial, "evidence-budget", "inspect").await;
    let lease = initial.register("evidence-budget-worker".into()).unwrap();
    let claim = initial
        .claim("evidence-budget-worker", CancellationToken::new())
        .await
        .unwrap()
        .unwrap();
    let original = package("x".repeat(8 * 1024 * 1024));
    let seq = publish_close(&initial, &claim, "large-original", original.clone()).await;
    assert!(matches!(
        initial
            .publish(&claim, vec![body(&claim, "too-large", original.clone())])
            .await,
        Err(TurnError::EvidenceBudget)
    ));
    publish_close(
        &initial,
        &claim,
        "small-reference",
        reference(seq, &original),
    )
    .await;
    publish_close(
        &initial,
        &claim,
        "another-reference",
        reference(seq, &original),
    )
    .await;
    let session = claim.session_id().clone();
    let turn = claim.turn_id().clone();
    initial.release(&claim).unwrap();
    drop(lease);
    initial.shutdown(worker).await.unwrap();
    store.take_fact_read_cursors();
    let restored = kernel(store.clone()).await;
    assert_eq!(
        store
            .take_fact_read_cursors()
            .iter()
            .filter(|cursor| **cursor == seq - 1)
            .count(),
        1,
        "recovery reads an original once across repeated references in unfinished work"
    );
    let worker = restored.start_workers();
    assert!(matches!(
        restored.outcome(&session, &turn).await.unwrap(),
        Some(TurnOutcome::Interrupted { .. })
    ));
    let lease = restored
        .register("recovered-evidence-worker".into())
        .unwrap();
    restored
        .submit(SubmitTurn {
            reasoning_effort: None,
            turn_id: client_turn_id(),
            session: SubmitSession::Resume(restored.prepare_resume(&session).await.unwrap()),
            text: "new turn".into(),
            model: None,
            sandbox: None,
        })
        .await
        .unwrap();
    let claim = tokio::time::timeout(
        std::time::Duration::from_secs(2),
        restored.claim("recovered-evidence-worker", CancellationToken::new()),
    )
    .await
    .unwrap()
    .unwrap()
    .unwrap();
    // A new Turn has its own budget, and can still inspect/reuse the old original.
    publish_close(&restored, &claim, "new-turn-inline", original.clone()).await;
    publish_close(
        &restored,
        &claim,
        "old-original-reference",
        reference(seq, &original),
    )
    .await;
    let facts = store.read_facts(claim.session_id(), 0, 128).await.unwrap();
    assert_eq!(
        facts
            .facts
            .iter()
            .filter(|fact| matches!(fact.body(), SessionFactBody::ModelIntent { .. }))
            .count(),
        5
    );
    restored.release(&claim).unwrap();
    drop(lease);
    restored.shutdown(worker).await.unwrap();
}
