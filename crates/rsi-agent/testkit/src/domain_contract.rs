use rsi_agent_session_protocol::{
    AgentControlRecord, AgentControlRecordBody, DomainIdentity, DomainMutationSource,
    DomainRequestId, DomainRevision, DomainSnapshot, DomainStateCommit, DomainStateUpdate,
    DomainStateValue, SessionFact, SessionHeader,
};
use rsi_agent_store_protocol::{AtomicAgentCommit, AtomicSessionAppend, SessionStore, StoreError};
use std::sync::Arc;

/// Checks stable commit order across control-only writes and wall-clock rollback.
/// # Panics
/// Panics if the Store loses atomic Fact anchors or misorders a control page.
pub async fn assert_control_fact_order(store: &dyn SessionStore, header: SessionHeader) {
    use rsi_agent_session_protocol::{SessionFactBody, TurnId};
    let turn = TurnId::new("anchor-turn").unwrap();
    let accepted = SessionFact::new(
        1,
        1000,
        SessionFactBody::TurnAccepted {
            turn_id: turn.clone(),
            text: "begin".into(),
            model: None,
            reasoning_effort: None,
            sandbox: rsi_sandbox::SandboxMode::WorkspaceWrite,
            require_approval: false,
        },
    )
    .unwrap();
    let baseline = control(
        1,
        None,
        DomainMutationSource::Baseline,
        vec![update("a", 0, false)],
    );
    let baseline = AgentControlRecord::new(1, 1000, baseline.body().clone()).unwrap();
    store
        .commit_agent(atomic(&header, 0, 0, true, vec![accepted], vec![baseline]))
        .await
        .unwrap();
    let choice = control(
        2,
        Some("choose"),
        DomainMutationSource::Turn {
            turn_id: turn.clone(),
        },
        vec![update("a", 1, true)],
    );
    store
        .commit_agent(atomic(&header, 1, 1, false, vec![], vec![choice]))
        .await
        .unwrap();
    let event = SessionFact::new(
        2,
        1,
        SessionFactBody::CancelRequested {
            turn_id: turn.clone(),
            reason: None,
        },
    )
    .unwrap();
    let change = bind_fact(
        &control(
            3,
            Some("mixed"),
            DomainMutationSource::Turn { turn_id: turn },
            vec![update("a", 2, false)],
        ),
        &event,
    );
    store
        .commit_agent(atomic(&header, 1, 2, false, vec![event], vec![change]))
        .await
        .unwrap();
    let page = store
        .read_controls(
            header.session_id(),
            0,
            3,
            rsi_agent_store_protocol::MAXIMUM_STORE_CONTROL_PAGE_BYTES,
        )
        .await
        .unwrap();
    assert_eq!(page.fact_cursors, [1, 1, 2]);
    assert_eq!((page.durable_seq, page.durable_fact_seq), (3, 2));
    assert!(page.records[0].timestamp_ms() > page.records[1].timestamp_ms());
    let tail = store
        .read_controls(
            header.session_id(),
            1,
            1,
            rsi_agent_store_protocol::MAXIMUM_STORE_CONTROL_PAGE_BYTES,
        )
        .await
        .unwrap();
    assert_eq!(tail.fact_cursors, [1]);
    assert_eq!(tail.records[0].seq(), 2);
    assert!(!tail.caught_up());
}

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

/// Exercises atomic executable selection, active-set replacement and historical reads.
///
/// # Panics
/// Panics when a Store loses the recipe/state commit boundary or historical membership.
#[allow(clippy::too_many_lines)] // One shared transaction scenario checks both Store implementations.
pub async fn assert_generation_store_contract(
    store: &dyn SessionStore,
    header: SessionHeader,
    accepted: SessionFact,
) {
    use rsi_agent_session_protocol::GenerationSelection;
    let first = GenerationSelection::new("a".repeat(64)).unwrap();
    let second = GenerationSelection::new("b".repeat(64)).unwrap();
    let initial = control(
        1,
        None,
        DomainMutationSource::Generation {
            selection: first.clone(),
            turn_id: None,
        },
        vec![update("a", 0, false), update("b", 0, true)],
    );
    let turn_id = accepted.body().turn_id().clone();
    store
        .commit_agent(atomic(&header, 0, 0, true, vec![accepted], vec![initial]))
        .await
        .unwrap();
    let next = DomainMutationSource::Generation {
        selection: second.clone(),
        turn_id: Some(turn_id.clone()),
    };
    let invalid = control(
        2,
        None,
        next.clone(),
        vec![update("a", 9, true), update("c", 0, false)],
    );
    assert!(matches!(
        store
            .commit_agent(atomic(&header, 1, 1, false, vec![], vec![invalid]))
            .await,
        Err(StoreError::DomainRevisionConflict { .. })
    ));
    let introduced = |id, seq, enabled| {
        DomainStateUpdate::introduced(seq, update(id, 0, enabled).snapshot().clone()).unwrap()
    };
    for updates in [vec![update("c", 0, false)]] {
        let invalid = control(2, None, next.clone(), updates);
        assert!(matches!(
            store
                .commit_agent(atomic(&header, 1, 1, false, vec![], vec![invalid]))
                .await,
            Err(StoreError::Invalid(_))
        ));
    }
    let unchanged = store
        .read_domain_states(header.session_id(), None, None)
        .await
        .unwrap();
    assert_eq!(
        unchanged.generation.as_ref().map(|entry| entry.control_seq),
        Some(1)
    );
    assert_eq!(unchanged.durable_control_seq, 1);
    assert_eq!(unchanged.states.len(), 2);
    let replacement = control(2, None, next.clone(), vec![introduced("c", 2, false)]);
    let replacement = with_removed(&replacement, &["b"]);
    let references = rsi_agent_store_protocol::native_control_references(&replacement).unwrap();
    assert!(
        references.iter().next().is_none(),
        "diagnostic selections are not CAS roots"
    );
    store
        .commit_agent(atomic(&header, 1, 1, false, vec![], vec![replacement]))
        .await
        .unwrap();
    let reintroduce = with_removed(
        &control(3, None, next.clone(), vec![introduced("b", 3, true)]),
        &["a", "c"],
    );
    store
        .commit_agent(atomic(&header, 1, 2, false, vec![], vec![reintroduce]))
        .await
        .unwrap();
    let empty = with_removed(&control(4, None, next.clone(), vec![]), &["b"]);
    store
        .commit_agent(atomic(&header, 1, 3, false, vec![], vec![empty]))
        .await
        .unwrap();
    for (horizon, expected) in [
        (1, vec!["a", "b"]),
        (2, vec!["a", "c"]),
        (3, vec!["b"]),
        (4, vec![]),
    ] {
        let page = store
            .read_domain_states(header.session_id(), Some(horizon), None)
            .await
            .unwrap();
        assert_eq!(
            page.generation.as_ref().map(|entry| entry.control_seq),
            Some(horizon)
        );
        assert_eq!(
            page.generation.as_ref().unwrap().selection,
            if horizon == 1 {
                first.clone()
            } else {
                second.clone()
            }
        );
        assert_eq!(
            page.states
                .iter()
                .map(|state| state.head.identity.id())
                .collect::<Vec<_>>(),
            expected
        );
        for id in ["a", "b", "c", "absent"] {
            let named = store
                .read_domain_states(header.session_id(), Some(horizon), Some(id))
                .await
                .unwrap();
            assert!(
                named.generation.is_none(),
                "named reads omit unrelated generation metadata"
            );
            assert_eq!(
                named.states,
                page.states
                    .iter()
                    .filter(|state| state.head.identity.id() == id)
                    .cloned()
                    .collect::<Vec<_>>()
            );
        }
        if horizon == 2 {
            assert_eq!(page.states[0].head.revision.get(), 1);
            assert_eq!(
                page.states[0].head.control_seq, 1,
                "unchanged snapshots keep their source position"
            );
            assert_eq!(page.states[1].head.revision.get(), 2);
        }
        if horizon == 3 {
            assert_eq!(page.states[0].head.revision.get(), 3);
        }
    }
    let current = store
        .read_domain_states(header.session_id(), None, None)
        .await
        .unwrap();
    assert!(current.states.is_empty());
    assert_eq!(
        current.generation.as_ref().map(|entry| entry.control_seq),
        Some(4)
    );
    use rsi_agent_session_protocol::{SessionFactBody, TurnOutcome};
    let cancel = SessionFact::new(
        2,
        2,
        SessionFactBody::CancelRequested {
            turn_id: turn_id.clone(),
            reason: None,
        },
    )
    .unwrap();
    let terminal = SessionFact::new(
        3,
        3,
        SessionFactBody::TurnTerminal {
            turn_id,
            outcome: TurnOutcome::Cancelled,
            result: None,
        },
    )
    .unwrap();
    let boundary = AgentControlRecord::new(
        5,
        terminal.timestamp_ms(),
        AgentControlRecordBody::TurnBoundaryRecorded {
            turn_id: terminal.body().turn_id().clone(),
            terminal_fact_seq: terminal.seq(),
        },
    )
    .unwrap();
    store
        .commit_agent(atomic(
            &header,
            1,
            4,
            false,
            vec![cancel, terminal],
            vec![boundary],
        ))
        .await
        .unwrap();
    let rejected = control(6, None, next, vec![introduced("late", 6, true)]);
    assert!(
        matches!(store.commit_agent(atomic(&header,3,5,false,vec![],vec![rejected])).await,
        Err(StoreError::Invalid(reason)) if reason.contains("open originating Turn"))
    );
    let after = store
        .read_domain_states(header.session_id(), None, None)
        .await
        .unwrap();
    assert_eq!(after.durable_control_seq, 5);
    assert!(after.states.is_empty());
    store.validate_session(header.session_id()).await.unwrap();
}

fn with_removed(record: &AgentControlRecord, ids: &[&str]) -> AgentControlRecord {
    let AgentControlRecordBody::DomainStateCommitted { commit } = record.body() else {
        unreachable!()
    };
    AgentControlRecord::new(
        record.seq(),
        record.timestamp_ms(),
        AgentControlRecordBody::DomainStateCommitted {
            commit: commit
                .clone()
                .with_removed(
                    ids.iter()
                        .map(|id| rsi_agent_session_protocol::DomainIdentity::new(*id, 1).unwrap())
                        .collect(),
                )
                .unwrap(),
        },
    )
    .unwrap()
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
    let first = store.read_domain_states(session, None, None).await.unwrap();
    let baseline_usage = store
        .read_turn_domain_usage(session, accepted.body().turn_id())
        .await
        .unwrap();
    assert_eq!((baseline_usage.records, baseline_usage.bytes), (0, 0));
    first.validate().unwrap();
    assert_eq!((first.durable_fact_seq, first.durable_control_seq), (1, 1));
    assert_eq!(first.states.len(), 2);
    assert_eq!(first.states[0].head.revision, DomainRevision::new(1));
    let stale_command = control(
        2,
        Some("stale-command"),
        DomainMutationSource::Command {
            invocation: rsi_agent_session_protocol::SessionCommandInvocation {
                command: rsi_agent_session_protocol::ContributionId::new("fixture.command")
                    .unwrap(),
                request_id: rsi_agent_session_protocol::DomainRequestId::new("stale-command")
                    .unwrap(),
                expected_revision: rsi_agent_session_protocol::CommandRevision::Durable {
                    control_seq: 0,
                },
                arguments: rsi_agent_session_protocol::CommandArguments::new(true.into()).unwrap(),
            },
        },
        vec![update("a", 1, true)],
    );
    assert!(
        store
            .commit_agent(atomic(&header, 1, 1, false, vec![], vec![stale_command]))
            .await
            .is_err()
    );
    assert_eq!(
        store.read_domain_states(session, None, None).await.unwrap(),
        first
    );
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
        store.read_domain_states(session, None, None).await.unwrap(),
        first
    );
    assert_eq!(
        store
            .read_facts(
                session,
                0,
                8,
                rsi_agent_store_protocol::MAXIMUM_STORE_FACT_PAGE_BYTES
            )
            .await
            .unwrap()
            .facts
            .len(),
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
        store.read_domain_states(session, None, None).await.unwrap(),
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
    let current = store.read_domain_states(session, None, None).await.unwrap();
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
    let historical = store
        .read_domain_states(session, Some(1), None)
        .await
        .unwrap();
    for (cut, all) in [(None, &current), (Some(1), &historical)] {
        let selected = store
            .read_domain_states(session, cut, Some("a"))
            .await
            .unwrap();
        assert_eq!(selected.states.as_slice(), &all.states[..1]);
        assert_eq!(
            (
                selected.durable_fact_seq,
                selected.durable_control_seq,
                selected.selected_control_seq
            ),
            (
                all.durable_fact_seq,
                all.durable_control_seq,
                all.selected_control_seq
            )
        );
        assert!(
            store
                .read_domain_states(session, cut, Some("absent"))
                .await
                .unwrap()
                .states
                .is_empty()
        );
    }
    assert_eq!(historical.states, first.states);
    assert!(
        store
            .read_domain_states(session, Some(0), None)
            .await
            .unwrap()
            .states
            .is_empty()
    );
    assert!(
        store
            .read_domain_states(session, Some(3), None)
            .await
            .is_err()
    );
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
        store.read_domain_states(session, None, None).await.unwrap(),
        current
    );
    store.validate_session(session).await.unwrap();
}
