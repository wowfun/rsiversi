use super::*;
use rusqlite::{
    StatementStatus,
    trace::{TraceEvent, TraceEventCodes},
};

static MEASUREMENT: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
static VM: AtomicU64 = AtomicU64::new(0);
static COMPLETED_QUERIES: AtomicU64 = AtomicU64::new(0);

fn count(event: TraceEvent<'_>) {
    if let TraceEvent::Profile(statement, _) = event {
        VM.fetch_add(
            u64::try_from(statement.get_status(StatementStatus::VmStep)).unwrap(),
            Ordering::Relaxed,
        );
        if statement.sql().contains("terminal_seq IS NOT NULL") {
            COMPLETED_QUERIES.fetch_add(1, Ordering::Relaxed);
        }
    }
}

async fn completed_history(store: &SqliteStore, count: u64) -> (SessionId, TurnId) {
    let header = test_header("history");
    let id = header.session_id().clone();
    for index in 0..count {
        let accepted = test_fact(index * 2 + 1);
        let terminal = SessionFact::new(
            index * 2 + 2,
            1,
            SessionFactBody::TurnTerminal {
                turn_id: accepted.body().turn_id().clone(),
                outcome: rsi_agent_session_protocol::TurnOutcome::Completed,
            },
        )
        .unwrap();
        rsi_agent_testkit::append_history_fixture(
            store,
            AppendBatch {
                session_id: id.clone(),
                expected_seq: index * 2,
                header: (index == 0).then(|| header.clone()),
                facts: vec![accepted.into(), terminal.into()],
            },
        )
        .await
        .unwrap();
    }
    let invoking = test_fact(count * 2 + 1);
    let turn = invoking.body().turn_id().clone();
    store
        .append(AppendBatch {
            session_id: id.clone(),
            expected_seq: count * 2,
            header: None,
            facts: vec![invoking.into()],
        })
        .await
        .unwrap();
    store.validate_session(&id).await.unwrap();
    (id, turn)
}

#[tokio::test]
async fn warm_fork_selection_work_does_not_grow_with_unselected_history() {
    let _measurement = MEASUREMENT.lock().await;
    let mut work = Vec::new();
    for history_size in [32, 512] {
        let root = tempfile::tempdir().unwrap();
        let store = SqliteStore::open(root.path()).unwrap();
        let (id, turn) = completed_history(&store, history_size).await;
        let validated = store.inner.validation_runs.load(Ordering::Relaxed);
        store
            .inner
            .connections
            .reader
            .lock()
            .unwrap()
            .trace_v2(TraceEventCodes::SQLITE_TRACE_PROFILE, Some(count));
        let mut samples = Vec::new();
        for selection in [
            ForkTurnSelection::None,
            ForkTurnSelection::Last(1),
            ForkTurnSelection::Last(3),
        ] {
            VM.store(0, Ordering::Relaxed);
            COMPLETED_QUERIES.store(0, Ordering::Relaxed);
            let boundary = store
                .resolve_fork_boundary(&id, &turn, selection.clone())
                .await
                .unwrap();
            let vm = VM.load(Ordering::Relaxed);
            eprintln!("fork completed={history_size} selection={selection:?} vm={vm}");
            if selection == ForkTurnSelection::None {
                assert_eq!(boundary.effective_turns, 0);
                assert_eq!(COMPLETED_QUERIES.load(Ordering::Relaxed), 0);
            }
            samples.push(vm);
        }
        store
            .inner
            .connections
            .reader
            .lock()
            .unwrap()
            .trace_v2(TraceEventCodes::empty(), None);
        assert_eq!(
            store.inner.validation_runs.load(Ordering::Relaxed),
            validated
        );
        assert_eq!(
            store
                .resolve_fork_boundary(&id, &turn, ForkTurnSelection::Last(u64::MAX))
                .await
                .unwrap()
                .effective_turns,
            history_size
        );
        assert_eq!(
            store
                .resolve_fork_boundary(&id, &turn, ForkTurnSelection::All)
                .await
                .unwrap()
                .effective_turns,
            history_size
        );
        work.push(samples);
    }
    for (small, large) in work[0].iter().zip(&work[1]) {
        assert!(
            *large <= small + 64,
            "fixed selection grew: {small} -> {large}"
        );
    }
}

async fn activation(store: &SqliteStore, name: &str, waiting: bool) -> SessionId {
    let id = seed_session(store, name).await;
    let mut append = settlement::append(&id, 0);
    append.controls.pop();
    if waiting {
        append.controls.push(
            AgentControlRecord::new(
                2,
                1,
                AgentControlRecordBody::ActivationWaitingForDescendants {
                    activation_id: ActivationId::new("activation-0").unwrap(),
                },
            )
            .unwrap(),
        );
    }
    store
        .commit_agent(settlement::commit(vec![append]))
        .await
        .unwrap();
    id
}

#[tokio::test]
async fn waiting_selection_work_and_pagination_ignore_unrelated_activations() {
    let _measurement = MEASUREMENT.lock().await;
    let mut work = Vec::new();
    for unrelated in [16, 256] {
        let root = tempfile::tempdir().unwrap();
        let store = SqliteStore::open(root.path()).unwrap();
        for index in 0..unrelated {
            activation(&store, &format!("a-{index:04}"), false).await;
        }
        store
            .inner
            .connections
            .reader
            .lock()
            .unwrap()
            .trace_v2(TraceEventCodes::SQLITE_TRACE_PROFILE, Some(count));

        let mut samples = Vec::new();
        let after = SessionId::new("a-0007").unwrap();
        for cursor in [None, Some(&after)] {
            VM.store(0, Ordering::Relaxed);
            let page = store.list_waiting_activations(cursor, 4).await.unwrap();
            samples.push(VM.load(Ordering::Relaxed));
            assert!(page.sessions.is_empty());
            assert!(!page.has_more);
        }
        let mut waiting = Vec::new();
        for index in 0..9 {
            waiting.push(activation(&store, &format!("z-{index:04}"), true).await);
        }
        for (after, expected, more) in [
            (None, &waiting[..4], true),
            (Some(&waiting[3]), &waiting[4..8], true),
            (Some(&waiting[7]), &waiting[8..], false),
            (Some(&waiting[8]), &waiting[9..], false),
        ] {
            VM.store(0, Ordering::Relaxed);
            let page = store.list_waiting_activations(after, 4).await.unwrap();
            samples.push(VM.load(Ordering::Relaxed));
            assert_eq!(page.sessions, expected);
            assert_eq!(page.has_more, more);
        }
        store
            .inner
            .connections
            .reader
            .lock()
            .unwrap()
            .trace_v2(TraceEventCodes::empty(), None);
        eprintln!("waiting unrelated={unrelated} vm={samples:?}");
        work.push(samples);
        let connection = store.inner.connections.reader.lock().unwrap();
        for sql in [
            session_store::LIST_WAITING_ACTIVATIONS_FIRST_SQL,
            LIST_WAITING_ACTIVATIONS_AFTER_SQL,
        ] {
            let mut plan = connection
                .prepare(&format!("EXPLAIN QUERY PLAN {sql}"))
                .unwrap();
            let detail = plan
                .query_map(params!["a", 5], |row| row.get::<_, String>(3))
                .unwrap()
                .collect::<rusqlite::Result<Vec<_>>>()
                .unwrap()
                .join("\n");
            assert!(detail.contains("active_activations_waiting"), "{detail}");
            assert!(!detail.contains("TEMP B-TREE"), "{detail}");
        }
    }
    for (small, large) in work[0].iter().zip(&work[1]) {
        assert!(
            *large <= small + 64,
            "waiting selection grew: {small} -> {large}"
        );
    }
}
