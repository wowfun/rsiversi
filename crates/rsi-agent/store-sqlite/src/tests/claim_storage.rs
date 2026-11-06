use super::claim_admission::{accepted_append, claim_append};
use super::*;
use rsi_agent_session_protocol::{
    ActivationOutcome, AgentMessageContent, ContributionId, ForkPoint, ForkTurnSelection,
    HistoryOrigin, InputMessageSource, MessageAttribution, MessageOptions, StepOutcome,
    TurnOutcome,
};
use rsi_agent_store_protocol::{
    MAXIMUM_STORE_FACT_PAGE_BYTES, MaintenanceOperationId, PurgeRequest, PurgeTarget,
    StoreMaintenance,
};

fn commit(append: AtomicSessionAppend) -> AtomicAgentCommit {
    AtomicAgentCommit {
        sessions: vec![append],
        required_active_activations: vec![],
        quiescent_descendants_of: None,
    }
}

pub(super) async fn seed(store: &SqliteStore, together: bool) -> (SessionHeader, Vec<SessionFact>) {
    let header = test_header("claim-storage");
    let message = AgentMessage {
        message_id: MessageId::new("input").unwrap(),
        source: AgentMessageSource::Human {
            attribution: Some(MessageAttribution {
                id: "source-ownership".into(),
                source: serde_json::json!({"kind":"plugin:test", "nested":["保留",true]}),
            }),
        },
        content: vec![AgentMessageContent::Text {
            text: format!("ONE_COPY_BODY:{}", "字\n\"\\".repeat(4096)),
        }],
        options: MessageOptions::default(),
    };
    let mut acceptance = accepted_append(header.clone(), &message);
    let mut claim = claim_append(header.session_id(), &message, 3, true);
    claim.facts.push(Arc::new(
        SessionFact::new(
            4,
            2,
            SessionFactBody::InputMessageClaimed {
                turn_id: TurnId::new("turn").unwrap(),
                step_id: StepId::new("step").unwrap(),
                source: InputMessageSource::PluginContext {
                    contribution_id: ContributionId::new("fixture-context").unwrap(),
                    tool: None,
                },
                content: vec![AgentMessageContent::Text {
                    text: "inline context without mailbox acceptance".into(),
                }],
            },
        )
        .unwrap(),
    ));
    let expected = claim
        .facts
        .iter()
        .map(|fact| fact.as_ref().clone())
        .collect();
    if together {
        acceptance.facts = claim.facts;
        acceptance.controls.extend(claim.controls);
        store.commit_agent(commit(acceptance)).await.unwrap();
    } else {
        store.commit_agent(commit(acceptance)).await.unwrap();
        store.commit_agent(commit(claim)).await.unwrap();
    }
    (header, expected)
}

async fn check_reads(store: &SqliteStore, id: &SessionId, expected: &[SessionFact]) {
    let maximum = MAXIMUM_STORE_FACT_PAGE_BYTES;
    assert_eq!(
        store.read_facts(id, 0, 64, maximum).await.unwrap().facts,
        expected
    );
    assert_eq!(
        store
            .read_facts_before(id, 0, 64, maximum)
            .await
            .unwrap()
            .facts,
        expected
    );
    assert_eq!(
        store
            .read_turn_facts(id, &TurnId::new("turn").unwrap(), 0, 64, maximum)
            .await
            .unwrap()
            .facts,
        expected
    );
    assert_eq!(
        store
            .read_fact_window(id, 0, 64, maximum)
            .await
            .unwrap()
            .facts,
        expected
    );
    assert_eq!(
        store.read_fact_suffix(id, 64, maximum).await.unwrap().facts,
        expected
    );
    let bound = expected[2].encoded_len();
    assert_eq!(
        store.read_facts(id, 2, 64, bound).await.unwrap().facts,
        expected[2..3]
    );
    assert!(matches!(
        store.read_facts(id, 2, 64, bound - 1).await,
        Err(StoreError::ReadRequiresCapacity { minimum_bytes }) if minimum_bytes == bound
    ));
    store
        .inner
        .fact_materializations
        .store(0, Ordering::Relaxed);
    let window = store.read_fact_window(id, 2, 64, bound - 1).await.unwrap();
    assert_eq!(window.facts, expected[3..]);
    assert_eq!(window.omitted.len(), 1);
    assert_eq!(
        (window.omitted[0].seq, window.omitted[0].encoded_bytes),
        (3, bound)
    );
    assert_eq!(store.inner.fact_materializations.load(Ordering::Relaxed), 1);
    let suffix = store.read_fact_suffix(id, 64, bound - 1).await.unwrap();
    assert!(suffix.byte_limited);
    assert_eq!(suffix.facts, expected[3..]);
}

#[tokio::test]
async fn queued_claims_store_one_body_and_preserve_every_logical_reader() {
    for together in [false, true] {
        let root = private_tempdir();
        let store = SqliteStore::open(root.path()).unwrap();
        let (header, expected) = seed(&store, together).await;
        let db = Connection::open(root.path().join("sessions.sqlite3")).unwrap();
        let (raw, acceptance, logical): (String, i64, i64) = db
            .query_row(
                "SELECT fact_json, acceptance_control_seq, logical_bytes FROM facts WHERE seq=3",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        assert!(raw.len() < 512);
        assert!(!raw.contains("content") && !raw.contains("source"));
        assert_eq!(acceptance, 1);
        assert_eq!(usize::try_from(logical).unwrap(), expected[2].encoded_len());
        let copies: i64 = db.query_row("SELECT sum(instr(body,'ONE_COPY_BODY:')>0) FROM (SELECT fact_json AS body FROM facts UNION ALL SELECT control_json FROM agent_controls)", [], |row|row.get(0)).unwrap();
        assert_eq!(copies, 1);
        assert!(db.query_row("SELECT acceptance_control_seq IS NULL AND instr(fact_json,'inline context')>0 FROM facts WHERE seq=4", [], |row|row.get::<_,bool>(0)).unwrap());
        drop(db);
        check_reads(&store, header.session_id(), &expected).await;
        let id = header.session_id().clone();
        let point = store
            .with_reader(move |db| read_indexed_fact(db, &id, 3))
            .await
            .unwrap();
        assert_eq!(point, expected[2]);
        drop(store);
        SqliteStore::verify(root.path()).unwrap();
        SqliteStore::rebuild_reference_index(
            root.path(),
            &rsi_agent_store_protocol::ReferenceCodecs::default(),
        )
        .unwrap();
        let reopened = SqliteStore::open(root.path()).unwrap();
        check_reads(&reopened, header.session_id(), &expected).await;
    }
}

async fn finish(store: &SqliteStore, header: &SessionHeader) -> PurgeRequest {
    let turn = TurnId::new("turn").unwrap();
    let step = SessionFact::new(
        5,
        3,
        SessionFactBody::StepEnded {
            turn_id: turn.clone(),
            step_id: StepId::new("step").unwrap(),
            outcome: StepOutcome::Completed,
        },
    )
    .unwrap();
    let terminal = SessionFact::new(
        6,
        3,
        SessionFactBody::TurnTerminal {
            turn_id: turn.clone(),
            outcome: TurnOutcome::Completed,
            result: None,
        },
    )
    .unwrap();
    store
        .commit_agent(commit(AtomicSessionAppend {
            session_id: header.session_id().clone(),
            header: None,
            expected_fact_seq: 4,
            expected_control_seq: 3,
            facts: vec![Arc::new(step), Arc::new(terminal)],
            controls: vec![
                AgentControlRecord::new(
                    4,
                    3,
                    AgentControlRecordBody::ActivationSettled {
                        activation_id: ActivationId::new("activation").unwrap(),
                        outcome: ActivationOutcome::Completed { result: None },
                    },
                )
                .unwrap(),
                AgentControlRecord::new(
                    5,
                    3,
                    AgentControlRecordBody::TurnBoundaryRecorded {
                        turn_id: turn,
                        terminal_fact_seq: 6,
                    },
                )
                .unwrap(),
            ],
        }))
        .await
        .unwrap();
    PurgeRequest {
        operation_id: MaintenanceOperationId::new("purge-claims").unwrap(),
        targets: vec![PurgeTarget {
            session_id: header.session_id().clone(),
            header_sha256: header.fingerprint().unwrap(),
            fact_seq: 6,
            control_seq: 5,
        }],
    }
}

#[tokio::test]
async fn normalized_claim_coverage_survives_purge_and_reopen() {
    let root = private_tempdir();
    let store = SqliteStore::open(root.path()).unwrap();
    let (header, _) = seed(&store, true).await;
    let request = finish(&store, &header).await;
    store.begin_purge(request.clone()).await.unwrap().unwrap();
    drop(store);
    let store = SqliteStore::open(root.path()).unwrap();
    assert!(
        store
            .purge_batch(request.operation_id.as_str())
            .await
            .unwrap()
            .complete
    );
    drop(store);
    SqliteStore::verify(root.path()).unwrap();
}

#[tokio::test]
async fn normalized_claim_preserves_selected_fork_history_after_reopen() {
    let root = private_tempdir();
    let store = SqliteStore::open(root.path()).unwrap();
    let (header, expected) = seed(&store, true).await;
    finish(&store, &header).await;
    let point = ForkPoint::through(&TurnId::new("turn").unwrap());
    let boundary = store
        .resolve_fork_boundary(header.session_id(), &point, ForkTurnSelection::Last(1))
        .await
        .unwrap();
    let facts = store
        .read_facts(header.session_id(), 0, 64, MAXIMUM_STORE_FACT_PAGE_BYTES)
        .await
        .unwrap()
        .facts;
    assert_eq!(
        boundary.terminal_prefix_sha256,
        rsi_agent_session_protocol::fact_prefix_sha256(&facts).unwrap()
    );
    let child = header
        .forked_independent(
            SessionId::new("selected-fork").unwrap(),
            4,
            HistoryOrigin {
                parent_session_id: header.session_id().clone(),
                parent_header_fingerprint: header.fingerprint().unwrap(),
                point,
                resolved_after_seq: boundary.resolved_after_seq,
                resolved_terminal_seq: boundary.resolved_terminal_seq,
                terminal_prefix_sha256: boundary.terminal_prefix_sha256,
                resolved_terminal_control_seq: boundary.resolved_terminal_control_seq,
                terminal_control_prefix_sha256: boundary.terminal_control_prefix_sha256,
                requested_turns: ForkTurnSelection::Last(1),
                effective_turns: boundary.effective_turns,
            },
        )
        .unwrap();
    store
        .commit_agent(commit(AtomicSessionAppend {
            session_id: child.session_id().clone(),
            header: Some(child.clone()),
            expected_fact_seq: 0,
            expected_control_seq: 0,
            facts: vec![Arc::new(test_fact(1))],
            controls: vec![],
        }))
        .await
        .unwrap();
    drop(store);
    let store = SqliteStore::open(root.path()).unwrap();
    let child = store.header(child.session_id()).await.unwrap();
    let inherited = rsi_agent_store_protocol::capture_inherited_intervals(&store, &child)
        .await
        .unwrap();
    assert_eq!(inherited.len(), 1);
    let interval = &inherited[0];
    let origin = interval.source.history_origin().unwrap();
    let page = store
        .read_facts(
            interval.parent.session_id(),
            origin.resolved_after_seq,
            64,
            MAXIMUM_STORE_FACT_PAGE_BYTES,
        )
        .await
        .unwrap();
    assert_eq!(page.facts, facts);
    assert_eq!(page.facts[2], expected[2]);
    assert_eq!(
        page.facts.last().unwrap().seq(),
        origin.resolved_terminal_seq
    );
}

#[tokio::test]
async fn normalized_claim_rejects_broken_references_and_false_byte_projections() {
    for sql in [
        "UPDATE facts SET acceptance_control_seq=2 WHERE seq=3",
        "UPDATE facts SET logical_bytes=logical_bytes-1 WHERE seq=3",
        "UPDATE facts SET fact_json=json_set(fact_json,'$.accepted_control_seq',2) WHERE seq=3",
        "DELETE FROM agent_controls WHERE seq=1",
        "UPDATE agent_controls SET control_json=json_set(control_json,'$.wake_required',json('false')) WHERE seq=1",
    ] {
        let root = private_tempdir();
        let store = SqliteStore::open(root.path()).unwrap();
        let (header, _) = seed(&store, true).await;
        drop(store);
        let db = Connection::open(root.path().join("sessions.sqlite3")).unwrap();
        db.pragma_update(None, "foreign_keys", false).unwrap();
        db.execute(sql, []).unwrap();
        drop(db);
        assert!(
            matches!(
                SqliteStore::verify(root.path()),
                Err(StoreError::Corrupt(_))
            ),
            "{sql}"
        );
        let store = SqliteStore::open(root.path()).unwrap();
        assert!(
            matches!(
                store
                    .read_facts(header.session_id(), 0, 64, MAXIMUM_STORE_FACT_PAGE_BYTES)
                    .await,
                Err(StoreError::Corrupt(_))
            ),
            "{sql}"
        );
    }
}
