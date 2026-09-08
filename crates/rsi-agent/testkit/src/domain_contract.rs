use rsi_agent_session_protocol::{
    AgentControlRecord, AgentControlRecordBody, DomainIdentity, DomainMutationSource,
    DomainRequestId, DomainRevision, DomainSnapshot, DomainStateCommit, DomainStateUpdate,
    DomainStateValue, SessionFact, SessionHeader,
};
use rsi_agent_store_protocol::{AtomicAgentCommit, AtomicSessionAppend, SessionStore, StoreError};
use std::sync::Arc;

fn update(id: &str, revision: u64, enabled: bool) -> DomainStateUpdate {
    DomainStateUpdate::new(
        DomainRevision::new(revision),
        DomainSnapshot::new(
            DomainIdentity::new(id, 1).unwrap(),
            DomainStateValue::new(enabled.into()).unwrap(),
        ),
    )
    .unwrap()
}

fn control(
    seq: u64,
    request: Option<&str>,
    source: DomainMutationSource,
    updates: Vec<DomainStateUpdate>,
) -> AgentControlRecord {
    AgentControlRecord::new(
        seq,
        seq,
        AgentControlRecordBody::DomainStateCommitted {
            commit: DomainStateCommit::new(
                request.map(|id| DomainRequestId::new(id).unwrap()),
                source,
                updates,
            )
            .unwrap(),
        },
    )
    .unwrap()
}

fn atomic(
    header: &SessionHeader,
    expected_fact_seq: u64,
    expected_control_seq: u64,
    fresh: bool,
    facts: Vec<SessionFact>,
    controls: Vec<AgentControlRecord>,
) -> AtomicAgentCommit {
    AtomicAgentCommit {
        sessions: vec![AtomicSessionAppend {
            session_id: header.session_id().clone(),
            expected_fact_seq,
            expected_control_seq,
            header: fresh.then(|| header.clone()),
            facts: facts.into_iter().map(Arc::new).collect(),
            controls,
        }],
        required_active_activations: vec![],
        quiescent_descendants_of: None,
    }
}

fn bind_fact(control: &AgentControlRecord, fact: &SessionFact) -> AgentControlRecord {
    let AgentControlRecordBody::DomainStateCommitted { commit } = control.body() else {
        unreachable!("domain fixture control")
    };
    AgentControlRecord::new(
        control.seq(),
        control.timestamp_ms(),
        AgentControlRecordBody::DomainStateCommitted {
            commit: commit.clone().with_facts([fact]).unwrap(),
        },
    )
    .unwrap()
}

/// Exercises canonical baseline, mixed atomic revision CAS, historical state and request lookup.
///
/// # Panics
/// Panics when the fresh fixture or Store violates the mechanical domain contract.
#[allow(clippy::too_many_lines)] // One shared scenario preserves the exact pre/post state across mixed commits and receipt retries.
pub async fn assert_domain_store_contract(
    store: &dyn SessionStore,
    header: SessionHeader,
    accepted: SessionFact,
    event: SessionFact,
) {
    let session = header.session_id();
    let baseline = control(
        1,
        None,
        DomainMutationSource::Baseline,
        vec![update("a", 0, false), update("b", 0, true)],
    );
    store
        .commit_agent(atomic(
            &header,
            0,
            0,
            true,
            vec![accepted.clone()],
            vec![baseline],
        ))
        .await
        .unwrap();
    let first = store.read_domain_states(session, None).await.unwrap();
    let baseline_usage = store
        .read_turn_domain_usage(session, accepted.body().turn_id())
        .await
        .unwrap();
    assert_eq!((baseline_usage.records, baseline_usage.bytes), (0, 0));
    first.validate().unwrap();
    assert_eq!((first.durable_fact_seq, first.durable_control_seq), (1, 1));
    assert_eq!(first.states.len(), 2);
    assert_eq!(first.states[0].head.revision, DomainRevision::new(1));
    assert_eq!(
        first.states[0].snapshot.state().value(),
        &serde_json::Value::Bool(false)
    );
    let origin = DomainMutationSource::Turn {
        turn_id: accepted.body().turn_id().clone(),
    };
    let wrong = bind_fact(
        &control(
            2,
            Some("wrong-revision"),
            origin.clone(),
            vec![update("a", 1, true), update("b", 9, false)],
        ),
        &event,
    );
    assert!(matches!(
        store
            .commit_agent(atomic(
                &header,
                1,
                1,
                false,
                vec![event.clone()],
                vec![wrong]
            ))
            .await,
        Err(StoreError::DomainRevisionConflict { .. })
    ));
    assert_eq!(
        store.read_domain_states(session, None).await.unwrap(),
        first
    );
    assert_eq!(
        store.read_facts(session, 0, 8).await.unwrap().facts.len(),
        1
    );
    assert!(
        store
            .read_domain_request(session, &DomainRequestId::new("wrong-revision").unwrap())
            .await
            .unwrap()
            .is_none()
    );

    let accepted_control = bind_fact(
        &control(
            2,
            Some("replace-both"),
            origin.clone(),
            vec![update("a", 1, true), update("b", 1, false)],
        ),
        &event,
    );
    let changed_fact = SessionFact::new(
        event.seq(),
        event.timestamp_ms(),
        rsi_agent_session_protocol::SessionFactBody::CancelRequested {
            turn_id: event.body().turn_id().clone(),
            reason: Some("changed request".into()),
        },
    )
    .unwrap();
    assert!(matches!(
        store
            .commit_agent(atomic(
                &header,
                1,
                1,
                false,
                vec![changed_fact],
                vec![accepted_control.clone()]
            ))
            .await,
        Err(StoreError::Invalid(_))
    ));
    assert_eq!(
        store.read_domain_states(session, None).await.unwrap(),
        first
    );
    store
        .commit_agent(atomic(
            &header,
            1,
            1,
            false,
            vec![event],
            vec![accepted_control.clone()],
        ))
        .await
        .unwrap();
    let current = store.read_domain_states(session, None).await.unwrap();
    let usage = store
        .read_turn_domain_usage(session, accepted.body().turn_id())
        .await
        .unwrap();
    assert_eq!((usage.durable_fact_seq, usage.durable_control_seq), (2, 2));
    assert_eq!(
        (usage.records, usage.bytes),
        (1, accepted_control.encoded_len() as u64)
    );
    current.validate().unwrap();
    assert_eq!(
        (current.durable_fact_seq, current.durable_control_seq),
        (2, 2)
    );
    assert_eq!(current.states[0].head.revision, DomainRevision::new(2));
    assert_eq!(
        current.states[1].snapshot.state().value(),
        &serde_json::Value::Bool(false)
    );
    let historical = store.read_domain_states(session, Some(1)).await.unwrap();
    assert_eq!(historical.states, first.states);
    assert!(
        store
            .read_domain_states(session, Some(0))
            .await
            .unwrap()
            .states
            .is_empty()
    );
    assert!(store.read_domain_states(session, Some(3)).await.is_err());
    assert_eq!(
        store
            .read_domain_request(session, &DomainRequestId::new("replace-both").unwrap())
            .await
            .unwrap(),
        Some(accepted_control)
    );
    let duplicate_request = control(3, Some("replace-both"), origin, vec![update("a", 2, false)]);
    assert!(matches!(
        store
            .commit_agent(atomic(
                &header,
                2,
                2,
                false,
                vec![],
                vec![duplicate_request]
            ))
            .await,
        Err(StoreError::DomainRequestConflict { .. })
    ));
    assert_eq!(
        store.read_domain_states(session, None).await.unwrap(),
        current
    );
    store.validate_session(session).await.unwrap();
}
