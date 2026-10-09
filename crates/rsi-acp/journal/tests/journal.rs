use rsi_acp_journal::{Capabilities, ConversationId, Error, Journal, Limits, RecordKind, Status};
use serde_json::json;

fn id() -> ConversationId {
    ConversationId::new("external-fixture").unwrap()
}

#[tokio::test]
async fn corrupt_later_snapshot_cannot_partially_commit_startup_recovery() {
    let root = tempfile::tempdir().unwrap();
    let journal = Journal::open(root.path().to_owned(), Limits::default())
        .await
        .unwrap();
    let generation = create(&journal).await;
    let visible = journal
        .append(&id(), generation, RecordKind::Update, json!("visible"))
        .await
        .unwrap();
    let replay = journal.begin_replay(&id(), generation).await.unwrap();
    journal
        .append(&id(), generation, RecordKind::Update, json!("unpublished"))
        .await
        .unwrap();
    let corrupt = ConversationId::new("z-corrupt").unwrap();
    journal
        .create(
            corrupt.clone(),
            "configured-peer".into(),
            std::env::current_dir().unwrap().to_str().unwrap().into(),
        )
        .await
        .unwrap();
    journal.close().await.unwrap();
    let path = root.path().join("observed.sqlite3");
    let connection = rusqlite::Connection::open(&path).unwrap();
    let metadata: Vec<u8> = connection
        .query_row(
            "SELECT metadata FROM sessions WHERE id=?1",
            [corrupt.as_str()],
            |row| row.get(0),
        )
        .unwrap();
    connection
        .execute(
            "UPDATE sessions SET metadata=zeroblob(length(metadata)) WHERE id=?1",
            [corrupt.as_str()],
        )
        .unwrap();
    drop(connection);
    let before = std::fs::read(&path).unwrap();
    assert_eq!(
        Journal::open(root.path().to_owned(), Limits::default())
            .await
            .unwrap_err(),
        Error::Corrupt
    );
    assert!(
        std::fs::read(&path).unwrap() == before,
        "failed startup rewrote an earlier valid conversation"
    );
    let connection = rusqlite::Connection::open(&path).unwrap();
    connection
        .execute(
            "UPDATE sessions SET metadata=?1 WHERE id=?2",
            rusqlite::params![metadata, corrupt.as_str()],
        )
        .unwrap();
    drop(connection);
    let journal = Journal::open(root.path().to_owned(), Limits::default())
        .await
        .unwrap();
    assert_eq!(journal.get(&id()).await.unwrap().status, Status::Unknown);
    assert_eq!(journal.get(&corrupt).await.unwrap().status, Status::Unknown);
    assert_eq!(
        journal.position(&id(), replay.epoch).await.unwrap(),
        visible
    );
    assert_eq!(
        journal
            .page(&id(), replay.epoch, 0)
            .await
            .unwrap()
            .records
            .len(),
        1
    );
    journal.close().await.unwrap();
    let journal = Journal::open(root.path().to_owned(), Limits::default())
        .await
        .unwrap();
    assert_eq!(journal.get(&id()).await.unwrap().status, Status::Unknown);
    assert_eq!(
        journal
            .page(&id(), replay.epoch, 0)
            .await
            .unwrap()
            .records
            .len(),
        1
    );
    journal.close().await.unwrap();
}

#[tokio::test]
async fn opening_a_locked_database_fails_before_the_driver_busy_timeout() {
    let root = tempfile::tempdir().unwrap();
    let journal = Journal::open(root.path().to_owned(), Limits::default())
        .await
        .unwrap();
    journal.close().await.unwrap();
    let connection = rusqlite::Connection::open(root.path().join("observed.sqlite3")).unwrap();
    connection.execute_batch("BEGIN EXCLUSIVE").unwrap();
    let mut reopening = tokio::spawn(Journal::open(root.path().to_owned(), Limits::default()));
    let result = tokio::time::timeout(std::time::Duration::from_secs(1), &mut reopening).await;
    connection.execute_batch("ROLLBACK").unwrap();
    if let Ok(result) = result {
        assert_eq!(result.unwrap().unwrap_err(), Error::Io);
    } else {
        if let Ok(journal) = reopening.await.unwrap() {
            journal.close().await.unwrap();
        }
        panic!("startup probes retained the driver's five-second busy timeout");
    }
}

#[tokio::test]
async fn existing_page_geometry_and_freelist_are_rejected_without_startup_rewrites() {
    for large_pages in [false, true] {
        let root = tempfile::tempdir().unwrap();
        let journal = Journal::open(root.path().to_owned(), Limits::default())
            .await
            .unwrap();
        let generation = create(&journal).await;
        journal
            .append(
                &id(),
                generation,
                RecordKind::Update,
                json!("x".repeat(800_000)),
            )
            .await
            .unwrap();
        journal.close().await.unwrap();
        let path = root.path().join("observed.sqlite3");
        let connection = rusqlite::Connection::open(&path).unwrap();
        if large_pages {
            connection
                .execute_batch("PRAGMA page_size=65536; VACUUM;")
                .unwrap();
        } else {
            connection
                .execute_batch(
                    "PRAGMA foreign_keys=ON; DELETE FROM observations; DELETE FROM sessions;",
                )
                .unwrap();
            let free: i64 = connection
                .pragma_query_value(None, "freelist_count", |row| row.get(0))
                .unwrap();
            assert!(free > 85);
            let pages: i64 = connection
                .pragma_query_value(None, "page_count", |row| row.get(0))
                .unwrap();
            assert!(pages - free <= 85, "live pages alone must fit the cap");
            let rows: i64 = connection
                .query_row(
                    "SELECT (SELECT count(*) FROM observations) + (SELECT count(*) FROM sessions)",
                    [],
                    |row| row.get(0),
                )
                .unwrap();
            assert_eq!(rows, 0);
        }
        drop(connection);
        let before = std::fs::read(&path).unwrap();
        let result = Journal::open(
            root.path().to_owned(),
            Limits {
                session_bytes: 1024 * 1024,
                owner_bytes: 1024 * 1024,
            },
        )
        .await;
        assert_eq!(
            result.unwrap_err(),
            if large_pages {
                Error::Corrupt
            } else {
                Error::Quota
            }
        );
        assert_eq!(std::fs::read(&path).unwrap(), before);
    }
}
async fn create(journal: &Journal) -> u64 {
    journal
        .create(
            id(),
            "configured-peer".into(),
            std::env::current_dir().unwrap().to_str().unwrap().into(),
        )
        .await
        .unwrap();
    let snapshot = journal.connect(&id()).await.unwrap();
    journal
        .bind(
            &id(),
            snapshot.generation,
            "remote-exact".into(),
            Capabilities {
                load: true,
                resume: true,
                ..Capabilities::default()
            },
        )
        .await
        .unwrap();
    snapshot.generation
}

#[tokio::test]
async fn replay_replaces_only_after_success_and_reconnect_fences_old_peers() {
    let root = tempfile::tempdir().unwrap();
    let journal = Journal::open(root.path().to_owned(), Limits::default())
        .await
        .unwrap();
    let generation = create(&journal).await;
    for index in 0..1100 {
        journal
            .append(
                &id(),
                generation,
                RecordKind::Update,
                json!({"text":format!("old-{index}")}),
            )
            .await
            .unwrap();
    }
    let mut after = 0;
    let mut count = 0;
    loop {
        let page = journal.page(&id(), 1, after).await.unwrap();
        assert!(page.records.len() <= 64);
        assert_eq!(page.records[0].sequence, after + 1);
        count += page.records.len();
        after = page.records.last().unwrap().sequence;
        if !page.has_more {
            break;
        }
    }
    assert_eq!(count, 1100);
    assert_eq!(journal.position(&id(), 1).await.unwrap(), 1100);
    journal.begin_replay(&id(), generation).await.unwrap();
    journal
        .append(
            &id(),
            generation,
            RecordKind::Update,
            json!({"text":"failed replacement"}),
        )
        .await
        .unwrap();
    assert_eq!(journal.get(&id()).await.unwrap().epoch, 1);
    assert_eq!(
        journal.position(&id(), 1).await.unwrap(),
        1100,
        "Staged replay cannot advance a visible read position"
    );
    assert_eq!(
        journal
            .finish_replay(&id(), generation, false)
            .await
            .unwrap()
            .epoch,
        1
    );
    journal.begin_replay(&id(), generation).await.unwrap();
    journal
        .append(
            &id(),
            generation,
            RecordKind::Update,
            json!({"text":"complete replacement"}),
        )
        .await
        .unwrap();
    assert_eq!(
        journal
            .finish_replay(&id(), generation, true)
            .await
            .unwrap()
            .epoch,
        3
    );
    assert_eq!(journal.page(&id(), 3, 0).await.unwrap().records.len(), 1);
    assert_eq!(journal.position(&id(), 1).await.unwrap_err(), Error::Stale);
    assert_eq!(journal.position(&id(), 3).await.unwrap(), 1102);
    assert!(journal.window(&id(), 3, 1, 0).await.is_err());
    let next = journal.connect(&id()).await.unwrap().generation;
    assert!(next > generation);
    assert_eq!(
        journal
            .append(&id(), generation, RecordKind::User, json!("stale"))
            .await
            .unwrap_err(),
        Error::Stale
    );
    journal.settle(&id(), next, Status::Running).await.unwrap();
    drop(journal);
    let journal = Journal::open(root.path().to_owned(), Limits::default())
        .await
        .unwrap();
    let saved = journal.get(&id()).await.unwrap();
    assert_eq!(saved.status, Status::Unknown);
    assert_eq!(saved.remote.as_deref(), Some("remote-exact"));
    assert_eq!(saved.epoch, 3);
}

#[tokio::test]
async fn quota_preserves_settlement_and_large_records_use_bounded_exact_windows() {
    let root = tempfile::tempdir().unwrap();
    let journal = Journal::open(
        root.path().to_owned(),
        Limits {
            session_bytes: 2 * 1024 * 1024,
            owner_bytes: 16 * 1024 * 1024,
        },
    )
    .await
    .unwrap();
    let generation = create(&journal).await;
    let text = "界".repeat(150_000);
    let seq = journal
        .append(&id(), generation, RecordKind::Update, json!({"text":text}))
        .await
        .unwrap();
    let page = journal.page(&id(), 1, 0).await.unwrap();
    assert!(page.records[0].value.is_none());
    let mut bytes = Vec::new();
    loop {
        let window = journal.window(&id(), 1, seq, bytes.len()).await.unwrap();
        assert!(window.len() <= 65536);
        if window.is_empty() {
            break;
        }
        bytes.extend_from_slice(&window);
    }
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&bytes).unwrap()["text"],
        text
    );
    while journal
        .append(
            &id(),
            generation,
            RecordKind::Update,
            json!({"text":"x".repeat(400_000)}),
        )
        .await
        .is_ok()
    {}
    journal
        .settle(&id(), generation, Status::Unknown)
        .await
        .unwrap();
    assert_eq!(journal.get(&id()).await.unwrap().status, Status::Unknown);
    assert_eq!(
        journal
            .append(
                &id(),
                generation,
                RecordKind::Update,
                json!("x".repeat(1_048_577))
            )
            .await
            .unwrap_err(),
        Error::Quota
    );
}

#[tokio::test]
async fn exclusive_lease_unknown_schema_and_foreign_directory_fail_without_reset() {
    let root = tempfile::tempdir().unwrap();
    let journal = Journal::open(root.path().to_owned(), Limits::default())
        .await
        .unwrap();
    assert_eq!(
        Journal::open(root.path().to_owned(), Limits::default())
            .await
            .unwrap_err(),
        Error::Locked
    );
    create(&journal).await;
    drop(journal);
    let path = root.path().join("observed.sqlite3");
    let connection = rusqlite::Connection::open(&path).unwrap();
    connection.pragma_update(None, "user_version", 2).unwrap();
    drop(connection);
    let before = std::fs::read(&path).unwrap();
    assert_eq!(
        Journal::open(root.path().to_owned(), Limits::default())
            .await
            .unwrap_err(),
        Error::Corrupt
    );
    assert_eq!(std::fs::read(&path).unwrap(), before);
    let foreign = tempfile::tempdir().unwrap();
    std::fs::write(foreign.path().join("sessions.sqlite3"), b"native marker").unwrap();
    assert_eq!(
        Journal::open(foreign.path().to_owned(), Limits::default())
            .await
            .unwrap_err(),
        Error::Input
    );
    assert!(!foreign.path().join(".writer.lock").exists());
}

#[tokio::test]
async fn durable_accounting_tampering_cannot_bypass_observation_quotas() {
    let root = tempfile::tempdir().unwrap();
    let journal = Journal::open(root.path().to_owned(), Limits::default())
        .await
        .unwrap();
    let generation = create(&journal).await;
    journal
        .append(&id(), generation, RecordKind::User, json!("observed"))
        .await
        .unwrap();
    drop(journal);
    let connection = rusqlite::Connection::open(root.path().join("observed.sqlite3")).unwrap();
    connection
        .execute("UPDATE sessions SET bytes=16384", [])
        .unwrap();
    drop(connection);
    assert_eq!(
        Journal::open(root.path().to_owned(), Limits::default())
            .await
            .unwrap_err(),
        Error::Corrupt
    );
}

#[cfg(unix)]
#[tokio::test]
async fn symlink_and_hardlinked_journals_are_rejected() {
    let root = tempfile::tempdir().unwrap();
    let journal = Journal::open(root.path().to_owned(), Limits::default())
        .await
        .unwrap();
    drop(journal);
    let alias = tempfile::tempdir().unwrap();
    std::os::unix::fs::symlink(
        root.path().join("observed.sqlite3"),
        alias.path().join("observed.sqlite3"),
    )
    .unwrap();
    assert_eq!(
        Journal::open(alias.path().to_owned(), Limits::default())
            .await
            .unwrap_err(),
        Error::Input
    );
    std::fs::remove_file(alias.path().join("observed.sqlite3")).unwrap();
    std::fs::hard_link(
        root.path().join("observed.sqlite3"),
        alias.path().join("observed.sqlite3"),
    )
    .unwrap();
    assert_eq!(
        Journal::open(alias.path().to_owned(), Limits::default())
            .await
            .unwrap_err(),
        Error::Input
    );
}

#[tokio::test]
async fn failed_and_interrupted_replay_refunds_quota_without_reusing_coordinates() {
    let root = tempfile::tempdir().unwrap();
    let limits = Limits {
        session_bytes: 32 * 1024,
        owner_bytes: 1024 * 1024,
    };
    let journal = Journal::open(root.path().to_owned(), limits).await.unwrap();
    let generation = create(&journal).await;
    let original = journal
        .append(&id(), generation, RecordKind::Update, json!("visible"))
        .await
        .unwrap();
    let mut last = original;
    for _ in 0..8 {
        journal.begin_replay(&id(), generation).await.unwrap();
        let seq = journal
            .append(
                &id(),
                generation,
                RecordKind::Update,
                json!("x".repeat(8000)),
            )
            .await
            .unwrap();
        assert!(seq > last);
        last = seq;
        journal
            .finish_replay(&id(), generation, false)
            .await
            .unwrap();
    }
    assert_eq!(journal.position(&id(), 1).await.unwrap(), original);
    journal.begin_replay(&id(), generation).await.unwrap();
    journal
        .append(
            &id(),
            generation,
            RecordKind::Update,
            json!("x".repeat(8000)),
        )
        .await
        .unwrap();
    journal
        .settle(&id(), generation, Status::Unknown)
        .await
        .unwrap();

    journal.begin_replay(&id(), generation).await.unwrap();
    journal
        .append(
            &id(),
            generation,
            RecordKind::Update,
            json!("x".repeat(8000)),
        )
        .await
        .unwrap();
    journal.close().await.unwrap();
    let journal = Journal::open(root.path().to_owned(), limits).await.unwrap();
    let generation = journal.connect(&id()).await.unwrap().generation;
    let next = journal
        .append(
            &id(),
            generation,
            RecordKind::Update,
            json!("x".repeat(8000)),
        )
        .await
        .unwrap();
    assert!(next > last + 2);
    assert_eq!(journal.page(&id(), 1, 0).await.unwrap().records.len(), 2);
    journal.close().await.unwrap();
    Journal::open(root.path().to_owned(), limits)
        .await
        .unwrap()
        .close()
        .await
        .unwrap();
}

#[tokio::test]
async fn batch_is_atomic_at_quota_and_stale_generation_and_preserves_order() {
    let root = tempfile::tempdir().unwrap();
    let journal = Journal::open(
        root.path().to_owned(),
        Limits {
            session_bytes: 32 * 1024,
            owner_bytes: 1024 * 1024,
        },
    )
    .await
    .unwrap();
    let generation = create(&journal).await;
    let records = || {
        vec![
            (RecordKind::Update, json!("a".repeat(10000))),
            (RecordKind::Update, json!("b".repeat(10000))),
        ]
    };
    assert_eq!(
        journal
            .append_batch(&id(), generation, records())
            .await
            .unwrap_err(),
        Error::Quota
    );
    assert_eq!(journal.position(&id(), 1).await.unwrap(), 0);
    assert_eq!(
        journal
            .append_batch(&id(), generation + 1, records())
            .await
            .unwrap_err(),
        Error::Stale
    );
    let sequences = journal
        .append_batch(
            &id(),
            generation,
            (0..64)
                .map(|i| (RecordKind::Update, json!({"i":i})))
                .collect(),
        )
        .await
        .unwrap();
    assert_eq!(sequences, (1..=64).collect::<Vec<_>>());
    let page = journal.page(&id(), 1, 0).await.unwrap();
    for (i, record) in page.records.iter().enumerate() {
        assert_eq!(record.value, Some(json!({"i":i})));
    }
}
