#![cfg(unix)]

use super::*;
use std::os::unix::fs::MetadataExt as _;

#[test]
fn objects_symlink_is_rejected_without_changing_external_permissions() {
    use std::os::unix::fs::{PermissionsExt as _, symlink};
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path().join("cas");
    let outside = temporary.path().join("outside");
    fs::create_dir(&root).unwrap();
    fs::create_dir(&outside).unwrap();
    fs::set_permissions(&outside, fs::Permissions::from_mode(0o755)).unwrap();
    symlink(&outside, root.join("objects")).unwrap();
    assert!(matches!(
        unix::Store::open(&root),
        Err(MediaError::Corrupt(_))
    ));
    assert_eq!(
        fs::metadata(&outside).unwrap().permissions().mode() & 0o777,
        0o755
    );
    assert_eq!(fs::read_dir(&outside).unwrap().count(), 0);
}

#[tokio::test]
async fn shard_symlink_is_rejected_for_reads_and_publication() {
    use std::os::unix::fs::symlink;
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path().join("cas");
    let (backend, reference) = fixture(&root, 4096);
    let media = backend.get(&reference.id).await.unwrap();
    let shard = object_path(&root, &reference.id)
        .parent()
        .unwrap()
        .to_owned();
    let outside = temporary.path().join("outside");
    fs::rename(&shard, &outside).unwrap();
    symlink(&outside, &shard).unwrap();
    assert!(matches!(
        backend.get(&reference.id).await,
        Err(MediaError::Corrupt(_))
    ));
    assert!(matches!(
        backend.put(media).await,
        Err(MediaError::Corrupt(_))
    ));
    assert_eq!(fs::read_dir(&outside).unwrap().count(), 1);
}

#[tokio::test]
async fn retained_cas_authority_survives_replacement_of_its_absolute_name() {
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path().join("cas");
    let retained = temporary.path().join("retained");
    let (backend, reference) = fixture(&root, 4096);
    fs::rename(&root, &retained).unwrap();
    fs::create_dir(&root).unwrap();
    let mut media = backend.get(&reference.id).await.unwrap();
    media.bytes = bytes::Bytes::from_static(b"new immutable object");
    media.reference.id = MediaId::new(hex::encode(Sha256::digest(&media.bytes))).unwrap();
    media.reference.bytes = media.bytes.len() as u64;
    backend.put(media.clone()).await.unwrap();
    assert!(object_path(&retained, &media.reference.id).is_file());
    assert_eq!(
        backend.get(&media.reference.id).await.unwrap().bytes,
        media.bytes
    );
    assert_eq!(fs::read_dir(&root).unwrap().count(), 0);
}

#[test]
fn a_new_shard_receipt_syncs_both_namespace_entries_after_the_object_is_readable() {
    let original = tempfile::tempdir().unwrap();
    let (backend, reference) = fixture(original.path(), 4096);
    let reads = ByteBudget::default();
    let media = backend.store.get(&reference.id, &reads).unwrap();
    let fresh = tempfile::tempdir().unwrap();
    let store = unix::Store::open(fresh.path()).unwrap();
    let path = object_path(fresh.path(), &reference.id);
    let mut synced = Vec::new();
    store
        .put_with_sync(&media, &reads, |directory| {
            assert_eq!(store.get(&reference.id, &reads).unwrap().bytes, media.bytes);
            synced.push(directory.dir_metadata()?.ino());
            Ok(())
        })
        .unwrap();
    assert_eq!(
        synced,
        [
            fs::metadata(path.parent().unwrap()).unwrap().ino(),
            fs::metadata(fresh.path().join("objects")).unwrap().ino()
        ]
    );
}

#[test]
fn failed_publication_sync_keeps_the_object_unknown_until_an_identical_retry() {
    for fail_parent_only in [false, true] {
        let temporary = tempfile::tempdir().unwrap();
        let (backend, reference) = fixture(temporary.path(), 4096);
        let store = &backend.store;
        let path = object_path(temporary.path(), &reference.id);
        let reads = ByteBudget::default();
        let media = store.get(&reference.id, &reads).unwrap();
        fs::remove_file(&path).unwrap();
        let parent = fs::metadata(temporary.path().join("objects"))
            .unwrap()
            .ino();
        assert!(matches!(
            store.put_with_sync(&media, &reads, |directory| {
                if !fail_parent_only || directory.dir_metadata()?.ino() == parent {
                    Err(std::io::Error::other("injected directory sync failure"))
                } else {
                    Ok(())
                }
            }),
            Err(MediaError::Api(rsi_api_protocol::ApiError::OutcomeUnknown))
        ));
        assert_eq!(store.get(&reference.id, &reads).unwrap().bytes, media.bytes);
        let mut synced = Vec::new();
        store
            .put_with_sync(&media, &reads, |directory| {
                synced.push(directory.dir_metadata()?.ino());
                Ok(())
            })
            .unwrap();
        assert_eq!(
            synced,
            [fs::metadata(path.parent().unwrap()).unwrap().ino(), parent]
        );
        assert_eq!(fs::read_dir(path.parent().unwrap()).unwrap().count(), 1);
    }
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
    let store = Arc::new(unix::Store::open(root).unwrap());
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
    store.put(&media, &ByteBudget::default()).unwrap();
    (
        Backend {
            store,
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

#[tokio::test]
async fn concurrent_reads_charge_each_complete_file_until_its_last_slice_drops() {
    let temporary = tempfile::tempdir().unwrap();
    let (mut backend, reference) = fixture(temporary.path(), 4096);
    let file_bytes = usize::try_from(
        std::fs::metadata(object_path(temporary.path(), &reference.id))
            .unwrap()
            .len(),
    )
    .unwrap();
    backend.reads = ByteBudget::new(file_bytes * 2).unwrap();
    backend.io = Arc::new(tokio::sync::Semaphore::new(2));
    let (first, second) = tokio::join!(backend.get(&reference.id), backend.get(&reference.id));
    let (first, second) = (first.unwrap(), second.unwrap());
    assert_eq!(backend.reads.used(), file_bytes * 2);
    let slice = first.bytes.slice(1..2);
    drop(first);
    assert!(matches!(
        backend.get(&reference.id).await,
        Err(MediaError::AdmissionFull(_))
    ));
    drop(slice);
    assert_eq!(backend.reads.used(), file_bytes);
    assert!(backend.get(&reference.id).await.is_ok());
    drop(second);
    assert_eq!(backend.reads.used(), 0);
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
