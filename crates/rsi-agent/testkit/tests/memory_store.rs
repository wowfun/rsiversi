use rsi_agent_session_protocol::{
    EffectId, FrozenAgentProfile, SessionFact, SessionFactBody, SessionHeader, SessionId, TurnId,
    TurnOutcome,
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
        FrozenAgentProfile::new(
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
                facts: vec![fact(1)],
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
            facts: vec![fact(1)],
        })
        .await
        .unwrap();
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
            facts: vec![
                SessionFact::new(
                    1,
                    1,
                    SessionFactBody::TurnTerminal {
                        turn_id: unknown,
                        outcome: TurnOutcome::Completed,
                    },
                )
                .unwrap(),
            ],
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
            facts: vec![fact(1)],
        })
        .await
        .unwrap();
    for seq in 2..=3 {
        store
            .append(AppendBatch {
                session_id: session.clone(),
                expected_seq: seq - 1,
                header: None,
                facts: vec![model_delta(
                    seq,
                    &turn,
                    "x".repeat(MAX_LANGUAGE_OUTPUT_BYTES),
                )],
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
