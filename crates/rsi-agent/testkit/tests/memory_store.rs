use rsi_agent_session_protocol::{
    AgentPresetId, EffectId, FrozenAgentSettings, SessionFact, SessionFactBody, SessionHeader,
    SessionId, TurnId, TurnOutcome,
};
use rsi_agent_store_protocol::{
    AppendBatch, MAXIMUM_STORE_FACT_PAGE_BYTES, SessionStore, StoreError,
};
use rsi_agent_testkit::{MemoryStore, assert_mechanical_store_contract};
use rsi_ai_protocol::{ContentDelta, LanguageEvent, MAX_LANGUAGE_OUTPUT_BYTES, ModelRef};
use rsi_sandbox::SandboxMode;

fn header() -> SessionHeader {
    SessionHeader::new(
        SessionId::new("memory-session").unwrap(),
        1,
        "/workspace",
        AgentPresetId::new("test-agent").unwrap(),
        FrozenAgentSettings::new(
            "default",
            "system",
            ModelRef::new("deployment", "model").unwrap(),
            SandboxMode::WorkspaceWrite,
            false,
        )
        .unwrap(),
    )
    .unwrap()
}

fn fact(seq: u64) -> SessionFact {
    SessionFact::new(
        seq,
        seq,
        SessionFactBody::TurnAccepted {
            turn_id: TurnId::new(format!("turn-{seq}")).unwrap(),
            text: "text".into(),
            model: None,
            sandbox: SandboxMode::WorkspaceWrite,
            require_approval: false,
        },
    )
    .unwrap()
}

fn model_delta(seq: u64, turn_id: &TurnId, text: String) -> SessionFact {
    SessionFact::new(
        seq,
        seq,
        SessionFactBody::ModelEvent {
            turn_id: turn_id.clone(),
            effect_id: EffectId::new("effect").unwrap(),
            event: LanguageEvent::ContentDelta {
                index: 0,
                delta: ContentDelta::Text(text),
            },
        },
    )
    .unwrap()
}

#[tokio::test]
async fn memory_store_is_compare_and_append_and_failure_injection_is_precommit() {
    let store = MemoryStore::new();
    let session = SessionId::new("memory-session").unwrap();
    store.fail_next_appends(1);
    assert!(matches!(
        store
            .append(AppendBatch {
                session_id: session.clone(),
                expected_seq: 0,
                header: Some(header()),
                facts: (vec![fact(1)]).into_iter().map(Into::into).collect(),
            })
            .await,
        Err(StoreError::Io(_))
    ));
    assert!(matches!(
        store.header(&session).await,
        Err(StoreError::NotFound(_))
    ));
    store
        .append(AppendBatch {
            session_id: session.clone(),
            expected_seq: 0,
            header: Some(header()),
            facts: (vec![fact(1)]).into_iter().map(Into::into).collect(),
        })
        .await
        .unwrap();
    assert_eq!(
        store
            .header(&session)
            .await
            .unwrap()
            .agent_preset_id()
            .as_str(),
        "test-agent"
    );
    assert_eq!(
        store.read_facts(&session, 0, 8).await.unwrap().facts,
        vec![fact(1)]
    );
}

#[tokio::test]
async fn memory_store_rejects_a_terminal_fact_for_an_unknown_turn_at_append() {
    let store = MemoryStore::new();
    let session = SessionId::new("memory-session").unwrap();
    let unknown = TurnId::new("unknown-turn").unwrap();

    let result = store
        .append(AppendBatch {
            session_id: session,
            expected_seq: 0,
            header: Some(header()),
            facts: (vec![
                SessionFact::new(
                    1,
                    1,
                    SessionFactBody::TurnTerminal {
                        turn_id: unknown,
                        outcome: TurnOutcome::Completed,
                    },
                )
                .unwrap(),
            ])
            .into_iter()
            .map(Into::into)
            .collect(),
        })
        .await;

    assert!(matches!(result, Err(StoreError::Corrupt(_))));
}

#[tokio::test]
async fn memory_store_fact_pages_stop_before_the_aggregate_byte_bound() {
    let store = MemoryStore::new();
    let session = SessionId::new("memory-session").unwrap();
    let turn = TurnId::new("turn-1").unwrap();
    store
        .append(AppendBatch {
            session_id: session.clone(),
            expected_seq: 0,
            header: Some(header()),
            facts: (vec![fact(1)]).into_iter().map(Into::into).collect(),
        })
        .await
        .unwrap();
    for seq in 2..=3 {
        store
            .append(AppendBatch {
                session_id: session.clone(),
                expected_seq: seq - 1,
                header: None,
                facts: (vec![model_delta(
                    seq,
                    &turn,
                    "x".repeat(MAX_LANGUAGE_OUTPUT_BYTES),
                )])
                .into_iter()
                .map(Into::into)
                .collect(),
            })
            .await
            .unwrap();
    }

    let page = store.read_facts(&session, 0, 8).await.unwrap();
    assert_eq!(page.facts.len(), 2);
    assert!(!page.caught_up());
    assert!(
        page.facts
            .iter()
            .map(SessionFact::encoded_len)
            .sum::<usize>()
            <= MAXIMUM_STORE_FACT_PAGE_BYTES
    );
}

#[tokio::test]
async fn memory_store_passes_the_shared_mechanical_contract() {
    let turn = TurnId::new("turn-1").unwrap();
    assert_mechanical_store_contract(
        &MemoryStore::new(),
        header(),
        fact(1),
        SessionFact::new(
            2,
            2,
            SessionFactBody::CancelRequested {
                turn_id: turn.clone(),
                reason: Some("stop".into()),
            },
        )
        .unwrap(),
        SessionFact::new(
            3,
            3,
            SessionFactBody::TurnTerminal {
                turn_id: turn,
                outcome: TurnOutcome::Completed,
            },
        )
        .unwrap(),
    )
    .await;
}

#[tokio::test]
async fn append_retry_and_atomic_staging_retain_the_same_immutable_fact_allocations() {
    use rsi_agent_store_protocol::{AtomicAgentCommit, AtomicSessionAppend};
    use std::sync::Arc;

    let store = MemoryStore::new();
    let accepted = Arc::new(fact(1));
    let payload = Arc::new(model_delta(
        2,
        accepted.body().turn_id(),
        "x".repeat(8 * 1024 * 1024),
    ));
    let payload_identity = Arc::downgrade(&payload);
    let batch = AppendBatch {
        session_id: header().session_id().clone(),
        expected_seq: 0,
        header: Some(header()),
        facts: vec![accepted, payload],
    };
    store.fail_next_appends(1);
    assert!(store.append(batch.clone()).await.is_err());
    assert_eq!(payload_identity.strong_count(), 1);
    store.append(batch).await.unwrap();
    assert_eq!(
        payload_identity.strong_count(),
        1,
        "the Store retains the submitted allocation"
    );

    let terminal = Arc::new(
        SessionFact::new(
            3,
            3,
            SessionFactBody::TurnTerminal {
                turn_id: TurnId::new("turn-1").unwrap(),
                outcome: TurnOutcome::Completed,
            },
        )
        .unwrap(),
    );
    store
        .commit_agent(AtomicAgentCommit {
            sessions: vec![AtomicSessionAppend {
                session_id: header().session_id().clone(),
                expected_fact_seq: 2,
                expected_control_seq: 0,
                header: None,
                facts: vec![terminal],
                controls: vec![],
            }],
            required_active_activations: vec![],
            quiescent_descendants_of: None,
        })
        .await
        .unwrap();
    assert_eq!(
        payload_identity.strong_count(),
        1,
        "transaction staging shares retained history"
    );
    let page = store.read_facts(header().session_id(), 1, 1).await.unwrap();
    assert_eq!(
        page.facts[0].encoded_len(),
        payload_identity.upgrade().unwrap().encoded_len()
    );
    drop(store);
    assert!(payload_identity.upgrade().is_none());
}
