use super::*;

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
