use super::*;

#[test]
fn a_new_shard_receipt_syncs_both_namespace_entries_after_the_object_is_readable() {
    let original = tempfile::tempdir().unwrap();
    let (_, reference) = fixture(original.path(), 4096);
    let reads = ByteBudget::default();
    let media = read_object(
        &object_path(original.path(), &reference.id),
        &reference.id,
        &reads,
    )
    .unwrap();
    let fresh = tempfile::tempdir().unwrap();
    fs::create_dir(fresh.path().join("objects")).unwrap();
    let path = object_path(fresh.path(), &reference.id);
    let mut synced = Vec::new();
    write_object_with_sync(&path, &media, &reads, |directory| {
        assert_eq!(
            read_object(&path, &reference.id, &reads).unwrap().bytes,
            media.bytes
        );
        synced.push(directory.to_path_buf());
        Ok(())
    })
    .unwrap();
    assert_eq!(
        synced,
        [
            path.parent().unwrap(),
            fresh.path().join("objects").as_path()
        ]
    );
}

#[test]
fn a_failed_shard_parent_sync_keeps_the_object_unknown_until_an_identical_retry() {
    let temporary = tempfile::tempdir().unwrap();
    let (_, reference) = fixture(temporary.path(), 4096);
    let path = object_path(temporary.path(), &reference.id);
    let reads = ByteBudget::default();
    let media = read_object(&path, &reference.id, &reads).unwrap();
    fs::remove_file(&path).unwrap();
    let objects = temporary.path().join("objects");
    assert!(matches!(
        write_object_with_sync(&path, &media, &reads, |directory| {
            if directory == objects {
                Err(std::io::Error::other("injected shard parent sync failure"))
            } else {
                Ok(())
            }
        }),
        Err(MediaError::Api(rsi_api_protocol::ApiError::OutcomeUnknown))
    ));
    assert_eq!(
        read_object(&path, &reference.id, &reads).unwrap().bytes,
        media.bytes
    );
    let mut synced = Vec::new();
    write_object_with_sync(&path, &media, &reads, |directory| {
        synced.push(directory.to_path_buf());
        Ok(())
    })
    .unwrap();
    assert_eq!(synced, [path.parent().unwrap(), objects.as_path()]);
    assert_eq!(fs::read_dir(path.parent().unwrap()).unwrap().count(), 1);
}

#[test]
fn failed_publication_sync_is_unknown_and_an_identical_retry_obtains_a_new_receipt() {
    let temporary = tempfile::tempdir().unwrap();
    let (_, reference) = fixture(temporary.path(), 4096);
    let path = object_path(temporary.path(), &reference.id);
    let reads = ByteBudget::default();
    let media = read_object(&path, &reference.id, &reads).unwrap();
    fs::remove_file(&path).unwrap();
    assert!(matches!(
        write_object_with_sync(&path, &media, &reads, |_| {
            Err(std::io::Error::other("injected directory sync failure"))
        }),
        Err(MediaError::Api(rsi_api_protocol::ApiError::OutcomeUnknown))
    ));
    let published = read_object(&path, &reference.id, &reads).unwrap();
    assert_eq!(published.reference, media.reference);
    assert_eq!(published.bytes, media.bytes);
    assert_eq!(fs::read_dir(path.parent().unwrap()).unwrap().count(), 1);
    let mut synced = false;
    write_object_with_sync(&path, &media, &reads, |_| {
        synced = true;
        Ok(())
    })
    .unwrap();
    assert!(synced);
}

#[tokio::test]
async fn put_admits_before_hashing_and_rejects_a_bad_digest_before_publication() {
    let temporary = tempfile::tempdir().unwrap();
    let (backend, mut reference) = fixture(temporary.path(), 4096);
    reference.id = MediaId::new("0".repeat(64)).unwrap();
    let media = StoredMedia {
        reference: reference.clone(),
        bytes: bytes::Bytes::from_static(b"backend verifies immutable bytes, not raster decoding"),
    };
    let held = backend.io.clone().try_acquire_owned().unwrap();
    assert!(matches!(
        backend.put(media.clone()).await,
        Err(MediaError::AdmissionFull(_))
    ));
    drop(held);
    assert!(matches!(
        backend.put(media).await,
        Err(MediaError::Corrupt(_))
    ));
    assert!(!object_path(temporary.path(), &reference.id).exists());
}

fn fixture(root: &Path, budget: usize) -> (Backend, MediaRef) {
    prepare_objects(root).unwrap();
    let bytes = bytes::Bytes::from_static(b"backend verifies immutable bytes, not raster decoding");
    let reference = MediaRef {
        id: MediaId::new(hex::encode(Sha256::digest(&bytes))).unwrap(),
        mime: "image/png".into(),
        bytes: bytes.len() as u64,
        width: 1,
        height: 1,
    };
    let media = StoredMedia {
        reference: reference.clone(),
        bytes,
    };
    write_object(
        &object_path(root, &reference.id),
        &media,
        &ByteBudget::default(),
    )
    .unwrap();
    (
        Backend {
            root: root.into(),
            reads: ByteBudget::new(budget).unwrap(),
            io: Arc::new(tokio::sync::Semaphore::new(1)),
        },
        reference,
    )
}

#[tokio::test]
async fn canonical_views_keep_the_complete_file_allocation_until_the_last_slice_drops() {
    let temporary = tempfile::tempdir().unwrap();
    let (mut backend, reference) = fixture(temporary.path(), 4096);
    let file_bytes = usize::try_from(
        std::fs::metadata(object_path(temporary.path(), &reference.id))
            .unwrap()
            .len(),
    )
    .unwrap();
    backend.reads = ByteBudget::new(file_bytes).unwrap();
    let stored = backend.get(&reference.id).await.unwrap();
    assert_eq!(backend.reads.used(), file_bytes);
    assert!(file_bytes > stored.bytes.len());
    let slice = stored.bytes.slice(1..2);
    drop(stored);
    assert!(matches!(
        backend.get(&reference.id).await,
        Err(MediaError::AdmissionFull(_))
    ));
    assert_eq!(backend.reads.used(), file_bytes);
    drop(slice);
    assert_eq!(backend.reads.used(), 0);
    assert!(backend.get(&reference.id).await.is_ok());
    assert_eq!(backend.reads.used(), 0);
}

#[test]
fn cancelled_waiter_cannot_release_a_queued_blocking_io_slot() {
    cancelled_blocking_waiter(false);
}

#[test]
fn cancelled_put_waiter_cannot_release_a_queued_blocking_io_slot() {
    cancelled_blocking_waiter(true);
}

fn cancelled_blocking_waiter(put: bool) {
    let temporary = tempfile::tempdir().unwrap();
    let (backend, reference) = fixture(temporary.path(), 4096);
    let runtime = tokio::runtime::Builder::new_current_thread()
        .max_blocking_threads(1)
        .enable_all()
        .build()
        .unwrap();
    let (entered, entering) = std::sync::mpsc::channel();
    let (release, gate) = std::sync::mpsc::channel();
    let blocker = runtime.spawn_blocking(move || {
        entered.send(()).unwrap();
        gate.recv().unwrap();
    });
    entering
        .recv_timeout(std::time::Duration::from_secs(2))
        .unwrap();
    runtime.block_on(async {
        let mut operation = Box::pin(async {
            if put {
                backend
                    .put(StoredMedia {
                        reference: reference.clone(),
                        bytes: bytes::Bytes::from_static(
                            b"backend verifies immutable bytes, not raster decoding",
                        ),
                    })
                    .await
            } else {
                backend.get(&reference.id).await.map(|_| ())
            }
        });
        assert!(
            std::future::poll_fn(|cx| std::task::Poll::Ready(
                operation.as_mut().poll(cx).is_pending()
            ))
            .await
        );
        assert_eq!(backend.io.available_permits(), 0);
        drop(operation);
        assert_eq!(backend.io.available_permits(), 0);
        assert!(matches!(
            backend.get(&reference.id).await,
            Err(MediaError::AdmissionFull(_))
        ));
        release.send(()).unwrap();
        blocker.await.unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(2), async {
            while backend.io.available_permits() == 0 || backend.reads.used() != 0 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        assert_eq!(backend.reads.used(), 0);
        assert!(backend.get(&reference.id).await.is_ok());
    });
}
