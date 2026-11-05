use super::*;
use rsi_agent_session_protocol::{InputClaimRef, PreparedInput, StepInputDecision};

#[tokio::test]
async fn preparation_resolves_only_unconsumed_claims_before_a_conversation() {
    let kernel = kernel(Arc::new(MemoryStore::new())).await;
    let workers = kernel.start_workers();
    submit(&kernel, "prepared-input", "original").await;
    let lease = kernel.register("worker".into()).unwrap();
    let claim = kernel
        .claim("worker", CancellationToken::new())
        .await
        .unwrap()
        .unwrap();
    let turn = claim.turn_id().clone();
    let step = StepId::new("fixture-step").unwrap();
    tool_origin::flush_bodies(&kernel, &claim, vec![start_step(&turn)]).await;
    let model = || SessionFactBody::ModelIntent {
        turn_id: turn.clone(),
        effect_id: EffectId::new("model").unwrap(),
        purpose: rsi_agent_session_protocol::ModelPurpose::Conversation,
        snapshot: snapshot(),
        price_quote: None,
        evidence: rsi_agent_session_protocol::RequestEvidence::Unavailable {
            reason: rsi_agent_session_protocol::EvidenceUnavailable::NotCaptured,
        },
    };
    assert!(kernel.publish(&claim, vec![model()]).await.is_err());
    let prepare = |claims: Vec<InputClaimRef>| SessionFactBody::StepInputsPrepared {
        turn_id: turn.clone(),
        step_id: step.clone(),
        decision: StepInputDecision::enter(
            claims
                .into_iter()
                .map(|claim| PreparedInput::Claimed { claim })
                .collect(),
        )
        .unwrap(),
    };
    let input = InputClaimRef::new(claim.accepted_seq(), 0).unwrap();
    for invalid in [
        InputClaimRef::new(claim.accepted_seq(), 1).unwrap(),
        InputClaimRef::new(claim.accepted_seq() + 1, 0).unwrap(),
    ] {
        // Failed staged publication must leave even earlier valid references unconsumed.
        assert!(
            kernel
                .publish(&claim, vec![prepare(vec![input, invalid])])
                .await
                .is_err()
        );
    }
    tool_origin::flush_bodies(&kernel, &claim, vec![prepare(vec![input, input])]).await;
    assert!(
        kernel
            .publish(&claim, vec![prepare(vec![input])])
            .await
            .is_err()
    );
    tool_origin::flush_bodies(&kernel, &claim, vec![model()]).await;
    drop(lease);
    kernel.shutdown(workers).await.unwrap();
}

#[tokio::test]
async fn rejection_cannot_be_reopened_by_an_empty_preparation() {
    let kernel = kernel(Arc::new(MemoryStore::new())).await;
    let workers = kernel.start_workers();
    submit(&kernel, "rejected-input", "original").await;
    let lease = kernel.register("worker".into()).unwrap();
    let claim = kernel
        .claim("worker", CancellationToken::new())
        .await
        .unwrap()
        .unwrap();
    let turn = claim.turn_id().clone();
    let prepare = |decision| SessionFactBody::StepInputsPrepared {
        turn_id: turn.clone(),
        step_id: StepId::new("fixture-step").unwrap(),
        decision,
    };
    tool_origin::flush_bodies(
        &kernel,
        &claim,
        vec![start_step(&turn), prepare(StepInputDecision::reject())],
    )
    .await;
    assert!(
        kernel
            .publish(
                &claim,
                vec![prepare(StepInputDecision::enter(vec![]).unwrap())]
            )
            .await
            .is_err()
    );
    assert!(
        kernel
            .publish(
                &claim,
                vec![SessionFactBody::ModelIntent {
                    turn_id: turn,
                    effect_id: EffectId::new("model").unwrap(),
                    purpose: rsi_agent_session_protocol::ModelPurpose::Conversation,
                    snapshot: snapshot(),
                    price_quote: None,
                    evidence: rsi_agent_session_protocol::RequestEvidence::Unavailable {
                        reason: rsi_agent_session_protocol::EvidenceUnavailable::NotCaptured,
                    },
                }]
            )
            .await
            .is_err()
    );
    drop(lease);
    kernel.shutdown(workers).await.unwrap();
}
