use super::*;

fn fixture() -> (tempfile::TempDir, SpillLocalConfig) {
    let root = tempfile::tempdir().unwrap();
    let mut config = SpillLocalConfig::new(root.path().join("spill"));
    config.minimum_free_bytes = 0;
    (root, config)
}
async fn block(store: &LocalSpillStore) -> mpsc::Sender<()> {
    let (entered, started) = oneshot::channel();
    let (release, resume) = mpsc::channel();
    store
        .shared
        .send(Command::Block {
            entered,
            release: resume,
        })
        .unwrap();
    started.await.unwrap();
    release
}

#[tokio::test]
async fn stalled_read_and_status_have_a_bounded_wait() {
    let (_root, config) = fixture();
    let store = LocalSpillStore::open(config).await.unwrap();
    let release = block(&store).await;
    let scope = store.generation_scope();
    let locator = SpillLocator::new(&scope, [1; 16]);
    let result = tokio::time::timeout(Duration::from_secs(7), async {
        tokio::join!(store.read(&locator, 0, 1), store.is_clean(&scope))
    })
    .await;
    drop(release);
    store.shutdown().await;
    let (read, status) = result.expect("direct consumers must not wait indefinitely");
    assert!(matches!(read, Err(SpillError::Unavailable(_))));
    assert!(matches!(status, Err(SpillError::Unavailable(_))));
}

#[tokio::test]
async fn complete_raw_output_reconstructs_across_windows_and_restart() {
    let (_root, config) = fixture();
    let store = LocalSpillStore::open(config.clone()).await.unwrap();
    let scope = SpillScope::new("session", "parent");
    let bytes: Vec<u8> = (0_u8..251).cycle().take(5 * 1024 * 1024).collect();
    let capture = store.scope(scope).capture().unwrap();
    for chunk in bytes.chunks(MAXIMUM_SPILL_READ_BYTES) {
        assert!(capture.write(chunk).await);
    }
    let reference = capture.finish().await.unwrap();
    assert_eq!(reference.bytes, bytes.len() as u64);
    store.shutdown().await;
    let reopened = LocalSpillStore::open(config).await.unwrap();
    let mut copied = Vec::new();
    let mut offset = 0;
    while offset < reference.bytes {
        let page = reopened
            .read(&reference.locator, offset, MAXIMUM_SPILL_READ_BYTES)
            .await
            .unwrap();
        assert_eq!(page.offset, offset);
        assert!(page.bytes.len() <= MAXIMUM_SPILL_READ_BYTES);
        copied.extend_from_slice(&page.bytes);
        offset = page.next_offset;
    }
    assert_eq!(copied, bytes);
    assert!(
        reopened
            .read(&reference.locator, 0, MAXIMUM_SPILL_READ_BYTES + 1)
            .await
            .is_err()
    );
    assert!(
        reopened
            .read(&reference.locator, u64::MAX, 10)
            .await
            .is_err()
    );
    reopened.shutdown().await;
}

#[tokio::test]
async fn overflow_floor_and_queue_pressure_abandon_only_full_capture() {
    let (_root, mut config) = fixture();
    config.maximum_object_bytes = 1024;
    let store = LocalSpillStore::open(config.clone()).await.unwrap();
    let sink = store.scope(SpillScope::new("session", "limits"));
    let small = sink.capture().unwrap();
    assert!(small.write(&vec![b'a'; 1024]).await);
    assert_eq!(small.finish().await.unwrap().bytes, 1024);
    let overflow = sink.capture().unwrap();
    assert!(overflow.write(&vec![b'b'; 1025]).await);
    assert!(overflow.finish().await.is_none());
    let queued = sink.capture().unwrap();
    let release = block(&store).await;
    let chunk = vec![b'c'; MAXIMUM_SPILL_READ_BYTES];
    for _ in 0..QUEUED_BYTES / chunk.len() {
        assert!(queued.push(&chunk));
    }
    assert!(!queued.push(&chunk));
    release.send(()).unwrap();
    assert!(queued.finish().await.is_none());
    store.shutdown().await;
    config.minimum_free_bytes = u64::MAX;
    let store = LocalSpillStore::open(config).await.unwrap();
    assert!(
        store
            .save_text(store.generation_scope(), "disk floor")
            .await
            .is_err()
    );
    store.shutdown().await;
}

#[tokio::test]
async fn timeout_and_shutdown_retain_the_worker_lease_until_io_settles() {
    let (_root, config) = fixture();
    let store = LocalSpillStore::open(config.clone()).await.unwrap();
    let capture = store.scope(store.generation_scope()).capture().unwrap();
    assert!(capture.write(b"late").await);
    let release = block(&store).await;
    let (done, mut pending_cleanup) = oneshot::channel();
    store
        .shared
        .send(Command::Cleanup {
            scope: store.generation_scope(),
            remove: false,
            done,
        })
        .unwrap();
    assert!(capture.finish().await.is_none());
    assert!(LocalSpillStore::open(config.clone()).await.is_err());
    store.stop();
    assert!(LocalSpillStore::open(config.clone()).await.is_err());
    release.send(()).unwrap();
    store.shutdown().await;
    assert!(matches!(
        pending_cleanup.try_recv(),
        Err(oneshot::error::TryRecvError::Closed)
    ));
    assert!(capture.reference().is_none());
    let reopened = LocalSpillStore::open(config).await.unwrap();
    assert!(reopened.is_clean(&store.generation_scope()).await.unwrap());
    reopened.shutdown().await;
}

#[tokio::test]
async fn purge_fences_preexisting_sinks_and_keeps_unselected_scopes() {
    let (_root, config) = fixture();
    let store = LocalSpillStore::open(config.clone()).await.unwrap();
    let parent = SpillScope::new("session", "parent");
    let child = SpillScope::new("session", "child");
    let inherited = store
        .save_text(parent.clone(), "shared by fork")
        .await
        .unwrap();
    let retained = store.save_text(child.clone(), "child-owned").await.unwrap();
    let sink = store.scope(parent.clone());
    let active = sink.capture().unwrap();
    assert!(active.write(b"not published").await);
    store.fence(&parent);
    assert!(sink.capture().is_none());
    assert!(active.finish().await.is_none());
    store.cleanup(&parent).await.unwrap();
    assert!(store.is_clean(&parent).await.unwrap());
    assert!(store.read(&inherited.locator, 0, 100).await.is_err());
    assert_eq!(
        store
            .read(&retained.locator, 0, 100)
            .await
            .unwrap()
            .bytes
            .as_ref(),
        b"child-owned"
    );
    store.cleanup(&parent).await.unwrap();
    store.shutdown().await;
    let reopened = LocalSpillStore::open(config).await.unwrap();
    assert!(reopened.is_clean(&parent).await.unwrap());
    assert!(reopened.read(&inherited.locator, 0, 100).await.is_err());
    assert_eq!(
        reopened
            .read(&retained.locator, 0, 100)
            .await
            .unwrap()
            .bytes
            .as_ref(),
        b"child-owned"
    );
    reopened.shutdown().await;
}

#[tokio::test]
async fn text_is_verbatim_and_age_disabled_does_not_disable_purge() {
    let (_root, mut config) = fixture();
    config.cleanup_period_days = 0;
    let store = LocalSpillStore::open(config).await.unwrap();
    let scope = store.generation_scope();
    let text = "中文🦀\r\n\0quoted\" slash\\";
    let reference = store.save_text(scope.clone(), text).await.unwrap();
    assert_eq!(
        store.read(&reference.locator, 0, 1024).await.unwrap().bytes,
        text.as_bytes()
    );
    store.cleanup(&scope).await.unwrap();
    assert!(store.is_clean(&scope).await.unwrap());
    store.shutdown().await;
}

#[tokio::test]
async fn actual_sixty_four_mib_stream_is_complete_and_the_next_byte_abandons_it() {
    let (_root, config) = fixture();
    let store = LocalSpillStore::open(config).await.unwrap();
    let sink = store.scope(store.generation_scope());
    let chunk = vec![b'x'; MAXIMUM_SPILL_READ_BYTES];
    let exact = sink.capture().unwrap();
    for _ in 0..MAXIMUM_SPILL_BYTES / chunk.len() as u64 {
        assert!(exact.write(&chunk).await);
    }
    let reference = exact.finish().await.unwrap();
    assert_eq!(reference.bytes, MAXIMUM_SPILL_BYTES);
    let tail = store
        .read(&reference.locator, MAXIMUM_SPILL_BYTES - 3, 64)
        .await
        .unwrap();
    assert_eq!(tail.bytes.as_ref(), b"xxx");
    let over = sink.capture().unwrap();
    for _ in 0..MAXIMUM_SPILL_BYTES / chunk.len() as u64 {
        assert!(over.write(&chunk).await);
    }
    over.write(b"x").await;
    assert!(over.finish().await.is_none());
    store.shutdown().await;
}
