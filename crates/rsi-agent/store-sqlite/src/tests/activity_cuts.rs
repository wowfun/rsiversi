use super::*;

#[cfg(feature = "test-support")]
#[tokio::test]
async fn cold_session_summary_returns_header_and_cuts_in_one_metadata_read() {
    let root = private_tempdir();
    let store = SqliteStore::open(root.path()).unwrap();
    let id = seed_session(&store, "single-summary").await;
    let header = store.header(&id).await.unwrap();
    drop(store);
    let store = SqliteStore::open(root.path()).unwrap();
    store.begin_store_measurements();
    let row = store.session_summary(&id).await.unwrap();
    assert_eq!(row.header, header);
    assert_eq!(row.durable_fact_seq, 1);
    assert_eq!(row.durable_control_seq, 0);
    assert!(row.last_activity_ms >= header.created_at_ms());
    let measured = store.take_store_measurements();
    assert!(measured.validation.is_empty());
    assert_eq!(measured.reader.len(), 1);
    assert_eq!(measured.reader[0].operation, StoreOperation::Header);
    assert!(matches!(
        store
            .session_summary(&SessionId::new("absent-summary").unwrap())
            .await,
        Err(StoreError::NotFound(_))
    ));
}

#[tokio::test]
async fn cuts_keep_per_item_corruption_separate_from_valid_sessions() {
    let root = private_tempdir();
    let store = SqliteStore::open(root.path()).unwrap();
    let good = seed_session(&store, "cut-good").await;
    let bad = seed_session(&store, "cut-corrupt").await;
    {
        let mut slot = store.inner.connections.writer.lock().unwrap();
        slot.as_mut()
            .unwrap()
            .execute("DELETE FROM turns WHERE session_id=?1", [bad.as_str()])
            .unwrap();
    }
    *store.inner.validated_sessions.lock().unwrap() = ValidatedSessionCache::default();
    let missing = SessionId::new("cut-missing").unwrap();
    let rows = store
        .session_activity_cuts(&[bad, good.clone(), missing.clone()])
        .await
        .unwrap();
    assert!(matches!(&rows[0], Err(StoreError::Corrupt(_))));
    assert_eq!(
        rows[1],
        Ok(rsi_agent_store_protocol::StoreActivityCut {
            session_id: good,
            durable_fact_seq: 1,
            has_open_turn: true
        })
    );
    assert_eq!(rows[2], Err(StoreError::NotFound(missing.to_string())));
}

#[tokio::test]
async fn admitted_cut_keeps_proofs_after_cache_eviction_without_owning_pin_slots() {
    let root = private_tempdir();
    let store = SqliteStore::open(root.path()).unwrap();
    let mut ids = Vec::new();
    for index in 0..64 {
        ids.push(seed_session(&store, &format!("cut-{index}")).await);
    }
    let (entered, started) = tokio::sync::oneshot::channel();
    let (release, blocked) = std::sync::mpsc::channel();
    *store.inner.activity_cut_pause.lock().unwrap() = Some((entered, blocked));
    let worker = store.clone();
    let selected = ids.clone();
    let capture = tokio::spawn(async move { worker.session_activity_cuts(&selected).await });
    started.await.unwrap();
    let proofs = {
        let mut cache = store.inner.validated_sessions.lock().unwrap();
        let proofs = ids
            .iter()
            .map(|id| Arc::downgrade(cache.proofs.get(id).unwrap()))
            .collect::<Vec<_>>();
        *cache = ValidatedSessionCache::default();
        proofs
    };
    assert_eq!(
        store.inner.pin_admission.available_permits(),
        VALIDATED_SESSION_CACHE_CAPACITY
    );
    for index in 0..300 {
        seed_session(&store, &format!("churn-{index}")).await;
    }
    assert!(proofs.iter().all(|proof| proof.strong_count() > 0));
    let validations = store.inner.validation_runs.load(Ordering::Relaxed);
    release.send(()).unwrap();
    let cuts = capture.await.unwrap().unwrap();
    assert_eq!(cuts.len(), 64);
    assert!(cuts.into_iter().all(|cut| cut.unwrap().has_open_turn));
    assert_eq!(
        store.inner.validation_runs.load(Ordering::Relaxed),
        validations
    );
    assert!(proofs.iter().all(|proof| proof.upgrade().is_none()));
}

#[cfg(feature = "test-support")]
#[tokio::test]
async fn warm_access_and_cut_batches_have_three_measured_reader_operations() {
    let root = private_tempdir();
    let store = SqliteStore::open(root.path()).unwrap();
    let mut ids = Vec::new();
    for index in 0..128 {
        ids.push(seed_session(&store, &format!("batch-{index}")).await);
    }
    store.begin_store_measurements();
    assert_eq!(store.session_access(&ids).await.unwrap().len(), 128);
    for batch in ids.chunks(64) {
        assert_eq!(store.session_activity_cuts(batch).await.unwrap().len(), 64);
    }
    let measurements = store.take_store_measurements();
    assert!(measurements.validation.is_empty());
    assert_eq!(
        measurements
            .reader
            .iter()
            .map(|sample| sample.operation)
            .collect::<Vec<_>>(),
        vec![
            StoreOperation::AccessBatch,
            StoreOperation::ActivityCuts,
            StoreOperation::ActivityCuts
        ]
    );
}

#[cfg(feature = "test-support")]
#[tokio::test]
async fn cold_catalog_summaries_need_one_reader_without_canonical_history_proofs() {
    let root = private_tempdir();
    let store = SqliteStore::open(root.path()).unwrap();
    let mut ids = Vec::new();
    for index in 0..64 {
        ids.push(seed_session(&store, &format!("cold-catalog-{index}")).await);
    }
    drop(store);
    let store = SqliteStore::open(root.path()).unwrap();
    store.begin_store_measurements();
    let rows = store.session_activity_summaries(&ids).await.unwrap();
    assert_eq!(rows.len(), ids.len());
    assert!(
        rows.iter()
            .all(|row| row.as_ref().is_some_and(|row| row.durable_fact_seq == 1))
    );
    let summaries = store.take_store_measurements();
    assert_eq!(summaries.reader.len(), 1);
    assert!(summaries.validation.is_empty());

    // Canonical Turn checks have a separate cost and remain available to recovery.
    store.begin_store_measurements();
    let cuts = store.session_activity_cuts(&ids).await.unwrap();
    assert!(
        cuts.iter()
            .all(|cut| cut.as_ref().is_ok_and(|cut| cut.has_open_turn))
    );
    let cuts = store.take_store_measurements();
    eprintln!(
        "64 cold Sessions, metadata reader jobs {}, validations {}; additional activity-cut reader jobs {}, validations {}",
        summaries.reader.len(),
        summaries.validation.len(),
        cuts.reader.len(),
        cuts.validation.len()
    );
}
