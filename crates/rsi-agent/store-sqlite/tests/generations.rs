#![cfg(any(unix, windows))]

use rsi_agent_session_protocol::*;
use rsi_agent_store_protocol::{SessionStore, StoreError, StoreMaintenance};
use rsi_agent_store_sqlite::SqliteStore;
use rsi_ai_protocol::ModelRef;
use rsi_sandbox::SandboxMode;

fn header() -> SessionHeader {
    SessionHeader::new_local(
        SessionId::new("generations").unwrap(),
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

fn accepted() -> SessionFact {
    SessionFact::new(
        1,
        1,
        SessionFactBody::TurnAccepted {
            turn_id: TurnId::new("one").unwrap(),
            text: "start".into(),
            model: None,
            reasoning_effort: None,
            sandbox: SandboxMode::WorkspaceWrite,
            require_approval: false,
        },
    )
    .unwrap()
}

#[tokio::test]
async fn generation_selections_survive_reopen_with_complete_historical_membership() {
    let parent = tempfile::tempdir().unwrap();
    let root = parent.path().join("store");
    let store = SqliteStore::open(&root).unwrap();
    rsi_agent_testkit::assert_generation_store_contract(&store, header(), accepted()).await;
    drop(store);
    SqliteStore::verify(&root).unwrap();
    let store = SqliteStore::open(&root).unwrap();
    let id = header().session_id().clone();
    let current = store.read_domain_states(&id, None, None).await.unwrap();
    assert!(current.states.is_empty());
    assert_eq!(
        current.generation.as_ref().map(|entry| entry.control_seq),
        Some(4)
    );
    let historic = store
        .read_domain_states(&id, Some(1), Some("b"))
        .await
        .unwrap();
    assert!(historic.generation.is_none());
    assert_eq!(historic.states.len(), 1);
    let retained = store
        .read_domain_states(&id, Some(2), Some("a"))
        .await
        .unwrap();
    assert_eq!(retained.states[0].head.revision, DomainRevision::new(1));
    assert_eq!(
        retained.states[0].snapshot.state().value(),
        &serde_json::json!(false)
    );
    drop(store);
    SqliteStore::verify(&root).unwrap();
}

#[tokio::test]
async fn missing_generation_index_is_rejected_by_cold_reads_and_offline_verification() {
    let parent = tempfile::tempdir().unwrap();
    let root = parent.path().join("store");
    let store = SqliteStore::open(&root).unwrap();
    rsi_agent_testkit::assert_generation_store_contract(&store, header(), accepted()).await;
    drop(store);
    let connection = rusqlite::Connection::open(root.join("sessions.sqlite3")).unwrap();
    connection
        .execute(
            "DELETE FROM generation_selections WHERE control_seq = 2",
            [],
        )
        .unwrap();
    drop(connection);
    let store = SqliteStore::open(&root).unwrap();
    assert!(matches!(
        store
            .read_domain_states(header().session_id(), None, None)
            .await,
        Err(StoreError::Corrupt(_))
    ));
    drop(store);
    assert!(matches!(
        SqliteStore::verify(&root),
        Err(StoreError::Corrupt(_))
    ));
}

#[tokio::test]
async fn invalid_membership_index_is_rejected_by_cold_reads_and_offline_verification() {
    for tamper in [
        "UPDATE domain_membership_cuts SET names_json='[\"fabricated\"]' WHERE control_seq=2",
        "DELETE FROM domain_membership_cuts WHERE control_seq=2",
        "INSERT INTO domain_membership_cuts(session_id, control_seq, names_json) VALUES('generations', 5, '[]')",
    ] {
        let parent = tempfile::tempdir().unwrap();
        let root = parent.path().join("store");
        let store = SqliteStore::open(&root).unwrap();
        rsi_agent_testkit::assert_generation_store_contract(&store, header(), accepted()).await;
        drop(store);
        let connection = rusqlite::Connection::open(root.join("sessions.sqlite3")).unwrap();
        connection.execute(tamper, []).unwrap();
        drop(connection);
        let store = SqliteStore::open(&root).unwrap();
        assert!(matches!(
            store
                .read_domain_states(header().session_id(), None, None)
                .await,
            Err(StoreError::Corrupt(_))
        ));
        drop(store);
        assert!(matches!(
            SqliteStore::verify(&root),
            Err(StoreError::Corrupt(_))
        ));
    }
}

#[tokio::test]
#[expect(
    clippy::too_many_lines,
    reason = "One durable history verifies retention, canonical CAS roots and complete purge together"
)]
async fn purging_history_releases_explicit_roots_without_loading_removed_plugins() {
    use rsi_agent_store_protocol::{
        AtomicAgentCommit, AtomicSessionAppend, MaintenanceOperationId, PurgeRequest, PurgeTarget,
    };
    use std::sync::Arc;
    let parent = tempfile::tempdir().unwrap();
    let root = parent.path().join("store");
    let store = SqliteStore::open(&root).unwrap();
    let first = store
        .put_cas(Arc::from(b"recipe one".as_slice()))
        .await
        .unwrap();
    let second = store
        .put_cas(Arc::from(b"recipe two".as_slice()))
        .await
        .unwrap();
    let snapshot = |changed| {
        let object = if changed { &second } else { &first };
        let mut references = ReferenceCollector::default();
        references.cas(&object.sha256, object.byte_len).unwrap();
        DomainSnapshot::new(
            DomainIdentity::new("unloaded.domain", 42).unwrap(),
            DomainStateValue::new(
                serde_json::json!({"segments":[],"sessions":[],"checkpoint":changed}),
            )
            .unwrap(),
        )
        .with_references(references)
    };
    for seq in 1..=3 {
        let source = if seq == 2 {
            DomainMutationSource::Turn {
                turn_id: TurnId::new("one").unwrap(),
            }
        } else {
            let recipe = if seq == 1 { &first } else { &second };
            DomainMutationSource::Generation {
                selection: GenerationSelection::new(recipe.sha256.clone()).unwrap(),
                turn_id: (seq != 1).then(|| TurnId::new("one").unwrap()),
            }
        };
        let updates = if seq == 3 {
            vec![]
        } else {
            vec![DomainStateUpdate::new(DomainRevision::new(seq - 1), snapshot(seq == 2)).unwrap()]
        };
        let commit = DomainStateCommit::new(
            (seq == 2).then(|| DomainRequestId::new("ordinary-change").unwrap()),
            source,
            updates,
        )
        .unwrap();
        store
            .commit_agent(AtomicAgentCommit {
                sessions: vec![AtomicSessionAppend {
                    session_id: header().session_id().clone(),
                    expected_fact_seq: u64::from(seq != 1),
                    expected_control_seq: seq - 1,
                    header: (seq == 1).then(header),
                    facts: if seq == 1 {
                        vec![Arc::new(accepted())]
                    } else {
                        vec![]
                    },
                    controls: vec![
                        AgentControlRecord::new(
                            seq,
                            seq,
                            AgentControlRecordBody::DomainStateCommitted { commit },
                        )
                        .unwrap(),
                    ],
                }],
                required_active_activations: vec![],
                quiescent_descendants_of: None,
            })
            .await
            .unwrap();
    }
    let retained = store
        .read_domain_states(header().session_id(), None, None)
        .await
        .unwrap();
    assert_eq!(retained.states[0].head.revision, DomainRevision::new(2));
    assert_eq!(retained.states[0].snapshot, snapshot(true));
    let turn_id = TurnId::new("one").unwrap();
    store
        .commit_agent(AtomicAgentCommit {
            sessions: vec![AtomicSessionAppend {
                session_id: header().session_id().clone(),
                expected_fact_seq: 1,
                expected_control_seq: 3,
                header: None,
                facts: vec![Arc::new(
                    SessionFact::new(
                        2,
                        4,
                        SessionFactBody::TurnTerminal {
                            turn_id: turn_id.clone(),
                            outcome: TurnOutcome::Completed,
                            result: None,
                        },
                    )
                    .unwrap(),
                )],
                controls: vec![
                    AgentControlRecord::new(
                        4,
                        4,
                        AgentControlRecordBody::TurnBoundaryRecorded {
                            turn_id,
                            terminal_fact_seq: 2,
                        },
                    )
                    .unwrap(),
                ],
            }],
            required_active_activations: vec![],
            quiescent_descendants_of: None,
        })
        .await
        .unwrap();
    drop((first, second));
    assert!(
        store.preview_cas_gc(8).await.unwrap().is_empty(),
        "both historical snapshots retain explicit CAS roots"
    );
    let request = PurgeRequest {
        operation_id: MaintenanceOperationId::new("purge-generation-history").unwrap(),
        targets: vec![PurgeTarget {
            session_id: header().session_id().clone(),
            header_sha256: header().fingerprint().unwrap(),
            fact_seq: 2,
            control_seq: 4,
        }],
    };
    let mut receipt = store.begin_purge(request.clone()).await.unwrap().unwrap();
    while !receipt.complete {
        receipt = store
            .purge_batch(request.operation_id.as_str())
            .await
            .unwrap();
    }
    assert!(matches!(
        store
            .read_domain_states(header().session_id(), None, None)
            .await,
        Err(StoreError::NotFound(_))
    ));
    assert_eq!(store.preview_cas_gc(8).await.unwrap().len(), 2);
    drop(store);
    SqliteStore::verify(&root).unwrap();
    let connection = rusqlite::Connection::open(root.join("sessions.sqlite3")).unwrap();
    for table in [
        "generation_selections",
        "domain_membership_cuts",
        "domain_heads",
        "domain_versions",
        "domain_requests",
        "domain_removals",
    ] {
        let count: i64 = connection
            .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(count, 0, "{table}");
    }
}

#[tokio::test]
async fn unchanged_large_state_is_stored_once_across_generation_history() {
    use rsi_agent_store_protocol::{AtomicAgentCommit, AtomicSessionAppend};
    let parent = tempfile::tempdir().unwrap();
    let root = parent.path().join("store");
    let store = SqliteStore::open(&root).unwrap();
    let state = DomainSnapshot::new(
        DomainIdentity::new("large", 1).unwrap(),
        DomainStateValue::new(serde_json::json!("x".repeat(200 * 1024))).unwrap(),
    );
    for seq in 1..=100 {
        let commit = DomainStateCommit::new(
            None,
            DomainMutationSource::Generation {
                selection: GenerationSelection::new(format!("{seq:064x}")).unwrap(),
                turn_id: (seq != 1).then(|| TurnId::new("one").unwrap()),
            },
            if seq == 1 {
                vec![DomainStateUpdate::introduced(1, state.clone()).unwrap()]
            } else {
                vec![]
            },
        )
        .unwrap();
        store
            .commit_agent(AtomicAgentCommit {
                sessions: vec![AtomicSessionAppend {
                    session_id: header().session_id().clone(),
                    expected_fact_seq: u64::from(seq != 1),
                    expected_control_seq: seq - 1,
                    header: (seq == 1).then(header),
                    facts: if seq == 1 {
                        vec![std::sync::Arc::new(accepted())]
                    } else {
                        vec![]
                    },
                    controls: vec![
                        AgentControlRecord::new(
                            seq,
                            seq,
                            AgentControlRecordBody::DomainStateCommitted { commit },
                        )
                        .unwrap(),
                    ],
                }],
                required_active_activations: vec![],
                quiescent_descendants_of: None,
            })
            .await
            .unwrap();
    }
    let current = store
        .read_domain_states(header().session_id(), None, None)
        .await
        .unwrap();
    assert_eq!(current.domain_control_seq, 1);
    assert_eq!(current.states[0].head.control_seq, 1);
    assert_eq!(current.states[0].head.revision, DomainRevision::new(1));
    assert_eq!(current.states[0].snapshot, state);
    drop(store);
    let connection = rusqlite::Connection::open(root.join("sessions.sqlite3")).unwrap();
    let (versions, bytes): (i64, i64) = connection.query_row("SELECT (SELECT COUNT(*) FROM domain_versions), (SELECT SUM(length(CAST(control_json AS BLOB))) FROM agent_controls)", [], |row| Ok((row.get(0)?, row.get(1)?))).unwrap();
    assert_eq!(versions, 1);
    let memberships: i64 = connection
        .query_row("SELECT COUNT(*) FROM domain_membership_cuts", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(
        memberships, 1,
        "unchanged names must not duplicate membership cuts"
    );
    assert!(
        bytes < 300 * 1024,
        "unchanged payload was duplicated: {bytes}"
    );
    drop(connection);
    SqliteStore::verify(&root).unwrap();
    let store = SqliteStore::open(&root).unwrap();
    assert_eq!(
        store
            .read_domain_states(header().session_id(), Some(50), Some("large"))
            .await
            .unwrap()
            .states[0]
            .snapshot,
        state
    );
}

#[tokio::test]
#[expect(
    clippy::too_many_lines,
    reason = "One large legal command follows atomic publication, state and receipt reads, then cold reopen"
)]
async fn large_domain_command_round_trips_states_and_receipt() {
    use rsi_agent_store_protocol::{AtomicAgentCommit, AtomicSessionAppend};
    let parent = tempfile::tempdir().unwrap();
    let root = parent.path().join("store");
    let store = SqliteStore::open(&root).unwrap();
    let states: Vec<_> = (0..4)
        .map(|n| {
            DomainSnapshot::new(
                DomainIdentity::new(format!("state-{n}"), 1).unwrap(),
                DomainStateValue::new(serde_json::json!("s".repeat(240 * 1024))).unwrap(),
            )
        })
        .collect();
    let invocation = SessionCommandInvocation {
        command: ContributionId::new("fixture.command").unwrap(),
        request_id: DomainRequestId::new("large-command").unwrap(),
        expected_revision: CommandRevision::Durable { control_seq: 1 },
        arguments: CommandArguments::new(serde_json::json!("a".repeat(60 * 1024))).unwrap(),
    };
    let inputs: Vec<_> = (0..4)
        .map(|ordinal| {
            let source = CommandMessageSource {
                command: invocation.command.clone(),
                request_id: invocation.request_id.clone(),
                invocation_sha256: invocation.digest().unwrap(),
                ordinal,
                attribution: None,
            };
            CommandMessage {
                delivery: MessageDelivery::NextStep,
                message: AgentMessage {
                    message_id: source.message_id().unwrap(),
                    source: AgentMessageSource::Command { source },
                    content: vec![AgentMessageContent::Text {
                        text: "i".repeat(50 * 1024),
                    }],
                    options: MessageOptions::default(),
                },
            }
        })
        .collect();
    for seq in 1..=2 {
        let commit = DomainStateCommit::new_with_command_inputs(
            (seq == 2).then(|| invocation.request_id.clone()),
            if seq == 1 {
                DomainMutationSource::Baseline
            } else {
                DomainMutationSource::Command {
                    invocation: invocation.clone(),
                }
            },
            states
                .iter()
                .map(|state| {
                    DomainStateUpdate::new(DomainRevision::new(seq - 1), state.clone()).unwrap()
                })
                .collect(),
            if seq == 1 { vec![] } else { inputs.clone() },
        )
        .unwrap();
        let mut controls = vec![
            AgentControlRecord::new(
                seq,
                seq,
                AgentControlRecordBody::DomainStateCommitted { commit },
            )
            .unwrap(),
        ];
        if seq == 2 {
            assert!(controls[0].encoded_len() > MAXIMUM_DOMAIN_BASELINE_BYTES + 48 * 1024);
            for (index, input) in inputs.iter().enumerate() {
                controls.push(
                    AgentControlRecord::new(
                        3 + index as u64,
                        2,
                        AgentControlRecordBody::MessageAccepted {
                            message: input.message.clone(),
                            delivery: input.delivery,
                            bound_turn_id: None,
                            root_session_id: header().session_id().clone(),
                            target: MessageTarget::NextStep,
                            wake_required: false,
                        },
                    )
                    .unwrap(),
                );
            }
        }
        store
            .commit_agent(AtomicAgentCommit {
                sessions: vec![AtomicSessionAppend {
                    session_id: header().session_id().clone(),
                    expected_fact_seq: u64::from(seq == 2),
                    expected_control_seq: seq - 1,
                    header: (seq == 1).then(header),
                    facts: if seq == 1 {
                        vec![accepted().into()]
                    } else {
                        vec![]
                    },
                    controls,
                }],
                required_active_activations: vec![],
                quiescent_descendants_of: None,
            })
            .await
            .unwrap();
    }
    let receipt = store
        .read_domain_request(header().session_id(), &invocation.request_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(receipt.seq(), 2);
    let page = store
        .read_domain_states(header().session_id(), None, None)
        .await
        .unwrap();
    assert_eq!(page.states.len(), 4);
    assert_eq!(page.states[0].snapshot, states[0]);
    drop(store);
    SqliteStore::verify(&root).unwrap();
    let reopened = SqliteStore::open(&root).unwrap();
    assert_eq!(
        reopened
            .read_domain_request(header().session_id(), &invocation.request_id)
            .await
            .unwrap(),
        Some(receipt)
    );
}
