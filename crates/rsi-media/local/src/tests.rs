use super::*;

fn fixture(root: &Path, budget: usize) -> (Backend, MediaRef) {
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
        let mut read = Box::pin(backend.get(&reference.id));
        assert!(
            std::future::poll_fn(|cx| std::task::Poll::Ready(read.as_mut().poll(cx).is_pending()))
                .await
        );
        assert_eq!(backend.io.available_permits(), 0);
        drop(read);
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
