use super::*;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn independent_readers_progress_and_cancelled_workers_retain_their_slots() {
    let root = private_tempdir();
    let store = SqliteStore::open(root.path()).unwrap();
    let (entered, started) = tokio::sync::oneshot::channel();
    let (release, blocked) = std::sync::mpsc::channel();
    let owner = store.clone();
    let first = tokio::spawn(async move {
        owner
            .with_reader(move |_| {
                entered.send(()).unwrap();
                blocked.recv().unwrap();
                Ok(())
            })
            .await
    });
    started.await.unwrap();
    assert_eq!(store.inner.reader_admission.available_permits(), 1);
    tokio::time::timeout(
        Duration::from_secs(5),
        store.with_reader(|connection| {
            assert!(connection.is_autocommit());
            Ok(())
        }),
    )
    .await
    .unwrap()
    .unwrap();
    first.abort();
    assert!(first.await.unwrap_err().is_cancelled());
    assert_eq!(store.inner.reader_admission.available_permits(), 1);
    assert!(matches!(
        SqliteStore::open(root.path()),
        Err(StoreError::WriterLocked)
    ));
    release.send(()).unwrap();
    tokio::time::timeout(Duration::from_secs(5), async {
        while store.inner.reader_admission.available_permits() != 2 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn unhealthy_connections_close_admission_instead_of_being_recycled() {
    for panic in [false, true] {
        for writer in [false, true] {
            let root = private_tempdir();
            let store = SqliteStore::open(root.path()).unwrap();
            store.validate_references().await.unwrap();
            let operation = move |connection: &mut Connection| -> Result<()> {
                assert!(!panic, "injected SQLite worker panic");
                connection.execute_batch("BEGIN").map_err(sql_error)
            };
            let result = if writer {
                store.with_writer(operation).await
            } else {
                store.with_reader(operation).await
            };
            assert_eq!(
                result,
                Err(StoreError::Io(
                    "SQLite worker left an unhealthy connection; lane closed".into()
                ))
            );
            if writer {
                assert!(
                    store.inner.reference_proof.lock().unwrap().is_some(),
                    "condemned writers must skip proof maintenance"
                );
            }
            let next = if writer {
                store.with_writer(|_| Ok(())).await
            } else {
                store.with_reader(|_| Ok(())).await
            };
            assert!(matches!(next, Err(StoreError::Io(_))));
        }
    }
}

#[tokio::test]
async fn first_record_capacity_refusal_materializes_no_fact_body() {
    let root = private_tempdir();
    let store = SqliteStore::open(root.path()).unwrap();
    let header = test_header("bounded-page");
    let id = header.session_id().clone();
    let fact = test_fact(1);
    store
        .append(AppendBatch {
            session_id: id.clone(),
            expected_seq: 0,
            header: Some(header),
            facts: vec![Arc::new(fact.clone())],
        })
        .await
        .unwrap();
    store.prepare_session(&id).await.unwrap();
    store
        .inner
        .fact_materializations
        .store(0, Ordering::Relaxed);
    let expected = StoreError::ReadRequiresCapacity {
        minimum_bytes: fact.encoded_len(),
    };
    assert_eq!(store.read_facts(&id, 0, 1, 1).await.unwrap_err(), expected);
    assert_eq!(
        store.read_facts_before(&id, 0, 1, 1).await.unwrap_err(),
        expected
    );
    assert_eq!(
        store
            .read_turn_facts(&id, fact.body().turn_id(), 0, 1, 1)
            .await
            .unwrap_err(),
        expected
    );
    assert_eq!(store.inner.fact_materializations.load(Ordering::Relaxed), 0);
    assert_eq!(
        store
            .read_facts(&id, 0, 1, fact.encoded_len())
            .await
            .unwrap()
            .facts,
        vec![fact]
    );
    assert_eq!(store.inner.fact_materializations.load(Ordering::Relaxed), 1);
}

#[tokio::test]
async fn unhealthy_writer_preserves_lane_failure_when_proof_reads_are_unavailable() {
    use rusqlite::hooks::{AuthAction, AuthContext, Authorization};
    let root = private_tempdir();
    let store = SqliteStore::open(root.path()).unwrap();
    store.validate_references().await.unwrap();
    let result = store
        .with_writer(|connection| {
            // A post-operation proof read would now fail, masking the lane diagnosis.
            connection
                .authorizer(Some(|context: AuthContext<'_>| {
                    if matches!(context.action, AuthAction::Pragma { .. }) {
                        Authorization::Deny
                    } else {
                        Authorization::Allow
                    }
                }))
                .unwrap();
            connection.execute_batch("BEGIN").map_err(sql_error)
        })
        .await;
    assert_eq!(
        result,
        Err(StoreError::Io(
            "SQLite worker left an unhealthy connection; lane closed".into()
        ))
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn validation_panic_wakes_queued_reads_without_dispatching_them() {
    use std::{future::Future as _, task::Poll};
    let root = private_tempdir();
    let store = SqliteStore::open(root.path()).unwrap();
    let owner = store.clone();
    let (entered, started) = tokio::sync::oneshot::channel();
    let (release, blocked) = std::sync::mpsc::channel();
    let first = tokio::spawn(async move {
        owner
            .with_validation(move |_| -> Result<()> {
                entered.send(()).unwrap();
                blocked.recv().unwrap();
                panic!("injected validation connection panic");
            })
            .await
    });
    started.await.unwrap();
    let ran = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let observed = ran.clone();
    let mut queued = Box::pin(store.with_validation(move |_| {
        observed.store(true, Ordering::SeqCst);
        Ok(())
    }));
    std::future::poll_fn(|cx| {
        assert!(queued.as_mut().poll(cx).is_pending());
        Poll::Ready(())
    })
    .await;
    release.send(()).unwrap();
    assert!(matches!(first.await.unwrap(), Err(StoreError::Io(_))));
    assert!(matches!(
        tokio::time::timeout(Duration::from_secs(5), queued)
            .await
            .unwrap(),
        Err(StoreError::Io(_))
    ));
    assert!(!ran.load(Ordering::SeqCst));
    assert!(
        store
            .inner
            .connections
            .validation_reader
            .lock()
            .unwrap()
            .is_none()
    );
    store.with_reader(|_| Ok(())).await.unwrap();
    store.with_writer(|_| Ok(())).await.unwrap();
}

#[tokio::test]
async fn native_store_mechanical_byte_and_access_contract() {
    let root = private_tempdir();
    let store = SqliteStore::open(root.path()).unwrap();
    let turn = TurnId::new("turn-1").unwrap();
    Box::pin(rsi_agent_testkit::assert_mechanical_store_contract(
        &store,
        test_header("shared-contract"),
        test_fact(1),
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
                outcome: rsi_agent_session_protocol::TurnOutcome::Completed,
                result: None,
            },
        )
        .unwrap(),
    ))
    .await;
}

#[tokio::test]
async fn condemned_writer_releases_its_write_transaction_before_store_drop() {
    let root = private_tempdir();
    let store = SqliteStore::open(root.path()).unwrap();
    assert!(
        store
            .with_writer(|connection| {
                connection
                    .execute_batch("BEGIN IMMEDIATE")
                    .map_err(sql_error)
            })
            .await
            .is_err()
    );
    let probe = Connection::open(root.path().join("sessions.sqlite3")).unwrap();
    probe.busy_timeout(Duration::ZERO).unwrap();
    probe.execute_batch("BEGIN IMMEDIATE; ROLLBACK").unwrap();
    drop(store);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn healthy_concurrent_reader_is_retained_for_ordered_shutdown() {
    let root = private_tempdir();
    let store = SqliteStore::open(root.path()).unwrap();
    let (entered, started) = tokio::sync::oneshot::channel();
    let (release, blocked) = std::sync::mpsc::channel();
    let owner = store.clone();
    let healthy = tokio::spawn(async move {
        owner
            .with_reader(move |_| {
                entered.send(()).unwrap();
                blocked.recv().unwrap();
                Ok(())
            })
            .await
    });
    started.await.unwrap();
    assert!(
        store
            .with_reader(|connection| connection.execute_batch("BEGIN").map_err(sql_error))
            .await
            .is_err()
    );
    release.send(()).unwrap();
    healthy.await.unwrap().unwrap();
    assert_eq!(store.inner.connections.readers.lock().unwrap().len(), 1);
}
