#![cfg(unix)]
use rsi_files::LocalFiles;
use rsi_files_protocol::*;
use std::{fs, os::unix::fs::symlink, path::Path};
use tokio_util::sync::CancellationToken;

static SERIAL: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
fn binding(root: &Path) -> FilesBinding {
    FilesBinding::new(FilesCaller::default(), "session", "header", root.to_owned()).unwrap()
}
fn path(value: &str) -> RelativePath {
    RelativePath::new(value.as_bytes()).unwrap()
}
fn cancel() -> CancellationToken {
    CancellationToken::new()
}

#[tokio::test]
async fn exact_pages_preserve_non_utf8_controls_offsets_and_reject_wrong_kinds() {
    let _serial = SERIAL.lock().await;
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path().canonicalize().unwrap();
    let bytes: Vec<u8> = (0..=255)
        .cycle()
        .take(MAXIMUM_FILE_PAGE_BYTES + 33)
        .collect();
    fs::write(root.join("bytes"), &bytes).unwrap();
    let service = LocalFiles::new().unwrap();
    let binding = binding(&root);
    let opened = service
        .open(binding.clone(), path("bytes"), FileKind::File, cancel())
        .await
        .unwrap();
    assert_eq!(opened.length, bytes.len() as u64);
    let first = service
        .read(
            binding.clone(),
            opened.token.clone(),
            0,
            MAXIMUM_FILE_PAGE_BYTES,
            cancel(),
        )
        .await
        .unwrap();
    let second = service
        .read(
            binding.clone(),
            opened.token.clone(),
            MAXIMUM_FILE_PAGE_BYTES as u64,
            100,
            cancel(),
        )
        .await
        .unwrap();
    assert_eq!(
        [
            hex::decode(first.bytes_hex).unwrap(),
            hex::decode(second.bytes_hex).unwrap()
        ]
        .concat(),
        bytes
    );
    let end = service
        .read(
            binding.clone(),
            opened.token.clone(),
            opened.length,
            1,
            cancel(),
        )
        .await
        .unwrap();
    assert!(end.bytes_hex.is_empty());
    for (offset, maximum) in [(0, 0), (0, MAXIMUM_FILE_PAGE_BYTES + 1), (u64::MAX, 1)] {
        assert_eq!(
            service
                .read(
                    binding.clone(),
                    opened.token.clone(),
                    offset,
                    maximum,
                    cancel()
                )
                .await,
            Err(FilesError::Invalid)
        );
    }
    assert_eq!(
        service
            .list(binding.clone(), opened.token.clone(), 0, 1, cancel())
            .await,
        Err(FilesError::Invalid)
    );
    service.release(&binding, &opened.token).unwrap();
    assert_eq!(
        service.read(binding, opened.token, 0, 1, cancel()).await,
        Err(FilesError::Unavailable)
    );
    service.close().await;
}

#[tokio::test]
async fn retained_root_does_not_follow_renamed_root_or_relative_symlink_replacement() {
    let _serial = SERIAL.lock().await;
    let temporary = tempfile::tempdir().unwrap();
    let base = temporary.path().canonicalize().unwrap();
    fs::create_dir_all(base.join("root/sub")).unwrap();
    fs::create_dir(base.join("outside")).unwrap();
    fs::write(base.join("root/sub/value"), b"inside").unwrap();
    fs::write(base.join("outside/value"), b"outside").unwrap();
    let service = LocalFiles::new().unwrap();
    let binding = binding(&base.join("root"));
    let opened = service
        .open(binding.clone(), path("sub/value"), FileKind::File, cancel())
        .await
        .unwrap();
    fs::rename(base.join("root"), base.join("old")).unwrap();
    symlink(base.join("outside"), base.join("root")).unwrap();
    assert_eq!(
        service
            .read(binding.clone(), opened.token.clone(), 0, 100, cancel())
            .await
            .unwrap()
            .bytes_hex,
        hex::encode(b"inside")
    );
    assert!(
        service
            .open(binding.clone(), path("value"), FileKind::File, cancel())
            .await
            .is_err()
    );
    fs::rename(base.join("old/sub"), base.join("old/moved")).unwrap();
    symlink(base.join("outside"), base.join("old/sub")).unwrap();
    assert_eq!(
        service.read(binding, opened.token, 0, 100, cancel()).await,
        Err(FilesError::Changed)
    );
    service.close().await;
}

#[tokio::test]
async fn writes_replacements_directory_mutation_and_refresh_have_explicit_versions() {
    let _serial = SERIAL.lock().await;
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path().canonicalize().unwrap();
    fs::write(root.join("value"), "first").unwrap();
    let service = LocalFiles::new().unwrap();
    let binding = binding(&root);
    let file = service
        .open(binding.clone(), path("value"), FileKind::File, cancel())
        .await
        .unwrap();
    fs::write(root.join("value"), "second-longer").unwrap();
    assert_eq!(
        service
            .read(binding.clone(), file.token, 0, 99, cancel())
            .await,
        Err(FilesError::Changed)
    );
    let file = service
        .open(binding.clone(), path("value"), FileKind::File, cancel())
        .await
        .unwrap();
    fs::rename(root.join("value"), root.join("old")).unwrap();
    fs::write(root.join("value"), "replacement").unwrap();
    assert_eq!(
        service
            .read(binding.clone(), file.token, 0, 99, cancel())
            .await,
        Err(FilesError::Changed)
    );
    let directory = service
        .open(
            binding.clone(),
            RelativePath::default(),
            FileKind::Directory,
            cancel(),
        )
        .await
        .unwrap();
    fs::write(root.join("new-entry"), "new").unwrap();
    assert_eq!(
        service
            .list(binding.clone(), directory.token, 0, 10, cancel())
            .await,
        Err(FilesError::Changed)
    );
    let refreshed = service
        .open(
            binding.clone(),
            RelativePath::default(),
            FileKind::Directory,
            cancel(),
        )
        .await
        .unwrap();
    assert_eq!(
        service
            .list(binding, refreshed.token, 0, 10, cancel())
            .await
            .unwrap()
            .total,
        3
    );
    service.close().await;
}

#[tokio::test]
async fn directory_pages_preserve_raw_names_sort_without_following_links_and_bound_encoded_size() {
    let _serial = SERIAL.lock().await;
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path().canonicalize().unwrap();
    for i in 0..300 {
        fs::write(root.join(format!("{i:03}-\"\n\\")), "").unwrap();
    }
    fs::write(root.join("终"), "raw-name").unwrap();
    symlink("missing", root.join("link")).unwrap();
    let service = LocalFiles::new().unwrap();
    let binding = binding(&root);
    let opened = service
        .open(
            binding.clone(),
            RelativePath::default(),
            FileKind::Directory,
            cancel(),
        )
        .await
        .unwrap();
    assert_eq!(opened.length, 302);
    let first = service
        .list(
            binding.clone(),
            opened.token.clone(),
            0,
            MAXIMUM_DIRECTORY_PAGE_ENTRIES,
            cancel(),
        )
        .await
        .unwrap();
    let second = service
        .list(
            binding.clone(),
            opened.token.clone(),
            first.entries.len(),
            MAXIMUM_DIRECTORY_PAGE_ENTRIES,
            cancel(),
        )
        .await
        .unwrap();
    assert!(serde_json::to_vec(&first).unwrap().len() <= MAXIMUM_DIRECTORY_PAGE_BYTES);
    assert!(first.entries.len() <= MAXIMUM_DIRECTORY_PAGE_ENTRIES);
    let entries = [first.entries, second.entries].concat();
    assert_eq!(entries.len(), 302);
    assert!(entries.windows(2).all(|pair| pair[0].path < pair[1].path));
    assert_eq!(
        entries
            .iter()
            .find(|entry| entry.name == "link")
            .unwrap()
            .kind,
        None
    );
    let raw = entries.last().unwrap();
    assert_eq!(raw.path.as_bytes(), "终".as_bytes());
    let raw = service
        .open(binding.clone(), raw.path.clone(), FileKind::File, cancel())
        .await
        .unwrap();
    assert_eq!(
        service
            .read(binding, raw.token, 0, 20, cancel())
            .await
            .unwrap()
            .bytes_hex,
        hex::encode("raw-name")
    );
    service.close().await;
}

#[tokio::test]
async fn token_checks_caller_session_revision_root_and_provider_generation() {
    let _serial = SERIAL.lock().await;
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path().canonicalize().unwrap();
    fs::write(root.join("value"), "first").unwrap();
    let service = LocalFiles::new().unwrap();
    let other = LocalFiles::new().unwrap();
    let caller = FilesCaller::default();
    let binding = FilesBinding::new(caller.clone(), "s", "r", root.clone()).unwrap();
    let opened = service
        .open(binding.clone(), path("value"), FileKind::File, cancel())
        .await
        .unwrap();
    for wrong in [
        FilesBinding::new(FilesCaller::default(), "s", "r", root.clone()).unwrap(),
        FilesBinding::new(caller.clone(), "other", "r", root.clone()).unwrap(),
        FilesBinding::new(caller.clone(), "s", "other", root.clone()).unwrap(),
        FilesBinding::new(caller, "s", "r", root.join("other")).unwrap(),
    ] {
        assert_eq!(
            service
                .read(wrong.clone(), opened.token.clone(), 0, 1, cancel())
                .await,
            Err(FilesError::Binding)
        );
        assert_eq!(
            service.release(&wrong, &opened.token),
            Err(FilesError::Binding)
        );
    }
    assert_eq!(
        other
            .read(binding.clone(), opened.token.clone(), 0, 1, cancel())
            .await,
        Err(FilesError::Unavailable)
    );
    assert_eq!(
        service
            .read(binding, opened.token, 0, 1, cancel())
            .await
            .unwrap()
            .bytes_hex,
        "66"
    );
    service.close().await;
    other.close().await;
}

#[tokio::test]
async fn special_files_links_cancellation_and_retirement_reject_without_body_reads() {
    let _serial = SERIAL.lock().await;
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path().canonicalize().unwrap();
    fs::write(root.join("value"), "first").unwrap();
    symlink("value", root.join("link")).unwrap();
    assert!(
        std::process::Command::new("mkfifo")
            .arg(root.join("fifo"))
            .status()
            .unwrap()
            .success()
    );
    let service = LocalFiles::new().unwrap();
    let binding = binding(&root);
    assert_eq!(
        service
            .open(binding.clone(), path("fifo"), FileKind::File, cancel())
            .await,
        Err(FilesError::Invalid)
    );
    assert!(
        service
            .open(binding.clone(), path("link"), FileKind::File, cancel())
            .await
            .is_err()
    );
    assert!(
        service
            .open(
                binding.clone(),
                RelativePath::default(),
                FileKind::File,
                cancel()
            )
            .await
            .is_err()
    );
    let stopped = cancel();
    stopped.cancel();
    assert_eq!(
        service
            .open(binding.clone(), path("value"), FileKind::File, stopped)
            .await,
        Err(FilesError::Cancelled)
    );
    service.close().await;
    assert_eq!(
        service
            .open(binding, path("value"), FileKind::File, cancel())
            .await,
        Err(FilesError::Cancelled)
    );
}

#[tokio::test]
async fn snapshot_and_process_token_capacity_are_hard_bounds_with_early_release() {
    let _serial = SERIAL.lock().await;
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path().canonicalize().unwrap();
    for i in 0..=MAXIMUM_DIRECTORY_ENTRIES {
        fs::write(root.join(i.to_string()), "").unwrap();
    }
    let service = LocalFiles::new().unwrap();
    let other = LocalFiles::new().unwrap();
    let binding = binding(&root);
    assert_eq!(
        service
            .open(
                binding.clone(),
                RelativePath::default(),
                FileKind::Directory,
                cancel()
            )
            .await,
        Err(FilesError::Capacity)
    );
    let mut tokens = Vec::new();
    for _ in 0..MAXIMUM_FILE_TOKENS {
        tokens.push(
            service
                .open(binding.clone(), path("0"), FileKind::File, cancel())
                .await
                .unwrap()
                .token,
        );
    }
    assert_eq!(
        other
            .open(binding.clone(), path("0"), FileKind::File, cancel())
            .await,
        Err(FilesError::Capacity)
    );
    service.release(&binding, &tokens.pop().unwrap()).unwrap();
    other
        .open(binding.clone(), path("0"), FileKind::File, cancel())
        .await
        .unwrap();
    service.close().await;
    for _ in 0..MAXIMUM_FILE_TOKENS - 1 {
        other
            .open(binding.clone(), path("0"), FileKind::File, cancel())
            .await
            .unwrap();
    }
    other.close().await;
}

#[tokio::test]
async fn ordinary_factory_withdrawal_closes_retained_services_and_changes_tokens() {
    let _serial = SERIAL.lock().await;
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path().canonicalize().unwrap();
    fs::write(root.join("value"), "first").unwrap();
    let binding = binding(&root);
    let runtime = rsi_meta::Runtime::default();
    let mut previous = None;
    for _ in 0..2 {
        let fiber = runtime
            .root()
            .apply(
                rsi_meta::ResolvedFactory::linked(
                    "files",
                    "test",
                    rsi_meta::UpdateMode::Replayable,
                    std::sync::Arc::new(rsi_files::FilesFactory),
                ),
                serde_json::Value::Null,
            )
            .await
            .unwrap();
        let service = runtime.root().lookup_local::<FilesContract>().unwrap();
        let opened = service
            .open(binding.clone(), path("value"), FileKind::File, cancel())
            .await
            .unwrap();
        if let Some(previous) = previous {
            assert_ne!(opened.token, previous);
            assert_eq!(
                service
                    .read(binding.clone(), previous, 0, 1, cancel())
                    .await,
                Err(FilesError::Unavailable)
            );
        }
        previous = Some(opened.token.clone());
        assert!(fiber.dispose().await.is_clean());
        assert!(runtime.root().lookup_local::<FilesContract>().is_none());
        assert_eq!(
            service
                .read(binding.clone(), opened.token, 0, 1, cancel())
                .await,
            Err(FilesError::Cancelled)
        );
    }
    assert!(runtime.shutdown().await.is_clean());
}

#[tokio::test]
async fn long_directory_paths_reduce_each_page_before_encoded_byte_budget() {
    let _serial = SERIAL.lock().await;
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path().canonicalize().unwrap();
    let relative = (0..16)
        .map(|_| "a".repeat(200))
        .collect::<Vec<_>>()
        .join("/");
    let mut directory = rsi_files_native_fs::open_absolute_directory_no_follow(&root).unwrap();
    for component in relative.split('/') {
        directory.create_dir(component).unwrap();
        directory = directory.open_dir(component).unwrap();
    }
    for index in 0..30 {
        directory
            .write(format!("{index:03}{}", "\"".repeat(197)), "")
            .unwrap();
    }
    let service = LocalFiles::new().unwrap();
    let binding = binding(&root);
    let opened = service
        .open(
            binding.clone(),
            path(&relative),
            FileKind::Directory,
            cancel(),
        )
        .await
        .unwrap();
    let mut offset = 0;
    while offset < 30 {
        let page = service
            .list(
                binding.clone(),
                opened.token.clone(),
                offset,
                MAXIMUM_DIRECTORY_PAGE_ENTRIES,
                cancel(),
            )
            .await
            .unwrap();
        assert!(!page.entries.is_empty());
        assert!(page.entries.len() < 30);
        assert!(serde_json::to_vec(&page).unwrap().len() <= MAXIMUM_DIRECTORY_PAGE_BYTES);
        offset += page.entries.len();
    }
    assert_eq!(offset, 30);
    service.close().await;
}

// APFS rejects invalid UTF-8 names at file creation. Linux exercises their native
// round trip; the platform-independent protocol still verifies exact byte paths.
#[cfg(target_os = "linux")]
#[tokio::test]
async fn native_non_utf8_filename_round_trips_without_lossy_path_resolution() {
    use std::os::unix::ffi::OsStrExt as _;
    let _serial = SERIAL.lock().await;
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path().canonicalize().unwrap();
    fs::write(root.join(std::ffi::OsStr::from_bytes(b"\xff")), "raw-name").unwrap();
    let service = LocalFiles::new().unwrap();
    let binding = binding(&root);
    let directory = service
        .open(
            binding.clone(),
            RelativePath::default(),
            FileKind::Directory,
            cancel(),
        )
        .await
        .unwrap();
    let page = service
        .list(binding.clone(), directory.token, 0, 1, cancel())
        .await
        .unwrap();
    assert_eq!(page.entries[0].path.as_bytes(), b"\xff");
    let file = service
        .open(
            binding.clone(),
            page.entries[0].path.clone(),
            FileKind::File,
            cancel(),
        )
        .await
        .unwrap();
    assert_eq!(service.describe(&binding, &file.token).unwrap(), file);
    assert_eq!(
        service
            .read(binding, file.token, 0, 16, cancel())
            .await
            .unwrap()
            .bytes_hex,
        hex::encode("raw-name")
    );
    service.close().await;
}

#[tokio::test]
async fn retiring_one_caller_releases_only_its_tokens_and_preserves_peer_reads() {
    let _serial = SERIAL.lock().await;
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path().canonicalize().unwrap();
    fs::write(root.join("file"), "body").unwrap();
    let service = LocalFiles::new().unwrap();
    let one = binding(&root);
    let two = binding(&root);
    let first = service
        .open(one.clone(), path("file"), FileKind::File, cancel())
        .await
        .unwrap();
    let second = service
        .open(two.clone(), path("file"), FileKind::File, cancel())
        .await
        .unwrap();
    service.release_caller(one.caller());
    assert_eq!(
        service.describe(&one, &first.token),
        Err(FilesError::Unavailable)
    );
    assert_eq!(service.describe(&two, &second.token).unwrap(), second);
    assert_eq!(
        service
            .read(two, second.token, 0, 4, cancel())
            .await
            .unwrap()
            .bytes_hex,
        hex::encode("body")
    );
    service.close().await;
}
