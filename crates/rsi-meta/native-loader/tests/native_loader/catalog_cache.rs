use super::*;

#[test]
fn catalog_rejects_an_unbounded_deadline_before_claiming_the_cache() {
    let parent = tempfile::tempdir().unwrap();
    let cache = parent.path().join("cache");
    let mut options = CatalogOptions::new(&cache);
    options.callback_timeout = Duration::MAX;

    assert!(matches!(
        NativeCatalog::new(options),
        Err(LoaderError::InvalidInput(_))
    ));
    assert!(
        !cache.exists(),
        "invalid options performed cache I/O before validation"
    );
}

#[test]
fn catalog_rejects_invalid_resource_limits_before_claiming_the_cache() {
    let parent = tempfile::tempdir().unwrap();
    let invalid_limits = [
        NativeCatalogLimits {
            maximum_cache_bytes: 0,
            ..NativeCatalogLimits::default()
        },
        NativeCatalogLimits {
            maximum_cache_artifacts: 0,
            ..NativeCatalogLimits::default()
        },
        NativeCatalogLimits {
            maximum_cache_artifacts: 65_537,
            ..NativeCatalogLimits::default()
        },
        NativeCatalogLimits {
            maximum_staging_bytes: 0,
            ..NativeCatalogLimits::default()
        },
        NativeCatalogLimits {
            maximum_concurrent_callbacks: 0,
            ..NativeCatalogLimits::default()
        },
        NativeCatalogLimits {
            maximum_concurrent_callbacks: 257,
            ..NativeCatalogLimits::default()
        },
        NativeCatalogLimits {
            maximum_live_instances: 0,
            ..NativeCatalogLimits::default()
        },
        NativeCatalogLimits {
            maximum_live_instances: 65_537,
            ..NativeCatalogLimits::default()
        },
        NativeCatalogLimits {
            maximum_concurrent_destructions: 0,
            ..NativeCatalogLimits::default()
        },
        NativeCatalogLimits {
            maximum_concurrent_destructions: 65,
            ..NativeCatalogLimits::default()
        },
    ];

    for (index, limits) in invalid_limits.into_iter().enumerate() {
        let cache = parent.path().join(format!("cache-{index}"));
        let mut options = CatalogOptions::new(&cache);
        options.limits = limits;
        assert!(matches!(
            NativeCatalog::new(options),
            Err(LoaderError::InvalidInput(_))
        ));
        assert!(!cache.exists(), "invalid limits performed cache I/O");
    }

    let cache = parent.path().join("zero-timeout");
    let mut options = CatalogOptions::new(&cache);
    options.callback_timeout = Duration::ZERO;
    assert!(matches!(
        NativeCatalog::new(options),
        Err(LoaderError::InvalidInput(_))
    ));
    assert!(!cache.exists(), "a zero timeout performed cache I/O");
}

#[test]
fn catalog_exclusively_owns_its_cache_directory() {
    let cache = tempfile::tempdir().unwrap();
    let first = NativeCatalog::new(CatalogOptions::new(cache.path())).unwrap();

    assert!(matches!(
        NativeCatalog::new(CatalogOptions::new(cache.path())),
        Err(LoaderError::CacheLocked(_))
    ));
    drop(first);
    NativeCatalog::new(CatalogOptions::new(cache.path()))
        .expect("dropping the catalog releases its directory lock");
}

#[cfg(unix)]
#[test]
fn unlinking_the_lock_marker_cannot_bypass_cache_ownership() {
    let cache = tempfile::tempdir().unwrap();
    let first = NativeCatalog::new(CatalogOptions::new(cache.path())).unwrap();
    std::fs::remove_file(cache.path().join(".rsi-meta.lock")).unwrap();

    assert!(matches!(
        NativeCatalog::new(CatalogOptions::new(cache.path())),
        Err(LoaderError::CacheLocked(_))
    ));
    let loaded = first
        .load(native_fixture())
        .expect("the pinned owner remains usable after marker unlink");

    drop(loaded);
    drop(first);
    wait_for_catalog_ownership_release(cache.path());
}

#[cfg(unix)]
#[test]
fn catalog_poisons_itself_if_the_claimed_cache_path_is_replaced() {
    let parent = tempfile::tempdir().unwrap();
    let cache = parent.path().join("cache");
    std::fs::create_dir(&cache).unwrap();
    let first = NativeCatalog::new(CatalogOptions::new(&cache)).unwrap();
    let moved = parent.path().join("moved");
    std::fs::rename(&cache, &moved).unwrap();
    std::fs::create_dir(&cache).unwrap();
    let replacement_owner = NativeCatalog::new(CatalogOptions::new(&cache)).unwrap();

    assert!(matches!(
        first.load(native_fixture()),
        Err(LoaderError::CachePoisoned)
    ));
    replacement_owner
        .load(native_fixture())
        .expect("the replacement directory owner should remain usable");
}

#[cfg(windows)]
#[test]
fn windows_catalog_lock_path_cannot_be_replaced_while_owned() {
    let cache = tempfile::tempdir().unwrap();
    let catalog = NativeCatalog::new(CatalogOptions::new(cache.path())).unwrap();
    let lock = cache.path().join(".rsi-meta.lock");
    let moved = cache.path().with_extension("moved");

    assert!(std::fs::remove_file(&lock).is_err());
    assert!(std::fs::rename(cache.path(), moved).is_err());
    drop(catalog);
    std::fs::remove_file(lock).expect("dropping the catalog releases the lock path");
}

#[test]
fn live_factory_retains_catalog_ownership_until_native_destruction_finishes() {
    let cache = tempfile::tempdir().unwrap();
    let catalog = NativeCatalog::new(CatalogOptions::new(cache.path())).unwrap();
    let factory = catalog.load(native_fixture()).unwrap();

    drop(catalog);
    assert!(matches!(
        NativeCatalog::new(CatalogOptions::new(cache.path())),
        Err(LoaderError::CacheLocked(_))
    ));

    drop(factory);
    let reopened = wait_for_catalog_ownership_release(cache.path());
    assert_eq!(reopened.snapshot().cache_artifacts, 1);
}

#[test]
fn concurrent_same_digest_loads_share_one_cache_commit_and_charge() {
    let artifact = native_fixture().clone();
    let artifact_bytes = std::fs::metadata(&artifact).unwrap().len();
    let cache = tempfile::tempdir().unwrap();
    let mut options = CatalogOptions::new(cache.path());
    options.limits.maximum_concurrent_callbacks = 2;
    let catalog = NativeCatalog::new(options).unwrap();
    let start = Arc::new(std::sync::Barrier::new(3));
    let mut loaders = Vec::new();
    for _ in 0..2 {
        let catalog = catalog.clone();
        let artifact = artifact.clone();
        let start = Arc::clone(&start);
        loaders.push(std::thread::spawn(move || {
            start.wait();
            catalog.load(artifact).unwrap()
        }));
    }
    start.wait();
    let factories = loaders
        .into_iter()
        .map(|loader| loader.join().unwrap())
        .collect::<Vec<_>>();

    assert_eq!(factories[0].identity(), factories[1].identity());
    let snapshot = catalog.snapshot();
    assert_eq!(snapshot.cache_artifacts, 1);
    assert_eq!(snapshot.cache_bytes, artifact_bytes);
    drop(factories);
    wait_for_staging_release(&catalog);
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn concurrent_cold_same_digest_loads_share_one_staging_artifact() {
    let markers = tempfile::tempdir().unwrap();
    let identity_entered = markers.path().join("identity-entered");
    let identity_release = markers.path().join("identity-release");
    let (_fixture, artifact) = blocking_identity_fixture(&identity_entered, &identity_release);
    let artifact_bytes = std::fs::metadata(&artifact).unwrap().len();
    let cache = tempfile::tempdir().unwrap();
    let mut options = CatalogOptions::new(cache.path());
    options.callback_timeout = Duration::from_secs(2);
    options.limits.maximum_concurrent_callbacks = 2;
    options.limits.maximum_staging_bytes = artifact_bytes;
    let catalog = NativeCatalog::new(options).unwrap();

    let first = tokio::task::spawn_blocking({
        let catalog = catalog.clone();
        let artifact = artifact.clone();
        move || catalog.load(artifact)
    });
    wait_for_file(&identity_entered).await;
    let second = tokio::task::spawn_blocking({
        let catalog = catalog.clone();
        let artifact = artifact.clone();
        move || catalog.load(artifact)
    });
    let admission_deadline = std::time::Instant::now() + Duration::from_secs(1);
    while catalog.snapshot().active_loads < 2
        && !second.is_finished()
        && std::time::Instant::now() < admission_deadline
    {
        tokio::task::yield_now().await;
    }
    let active_loads = catalog.snapshot().active_loads;
    let second_finished = second.is_finished();
    std::fs::write(&identity_release, b"release").unwrap();
    assert_eq!(
        active_loads, 2,
        "the second cold load left admission before it could join the digest fence; finished={second_finished}"
    );

    let first = first.await.unwrap().unwrap();
    let second = second
        .await
        .unwrap()
        .expect("same-digest work duplicated its complete staging reservation");
    assert_eq!(first.identity(), second.identity());
    assert_eq!(catalog.snapshot().rejected_staging_admissions, 0);
    drop((first, second));
    wait_for_staging_release(&catalog);
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn a_source_changed_behind_a_digest_waiter_rekeys_its_stable_copy() {
    let markers = tempfile::tempdir().unwrap();
    let identity_entered = markers.path().join("identity-entered");
    let identity_release = markers.path().join("identity-release");
    let (_fixture, first_artifact) =
        blocking_identity_fixture(&identity_entered, &identity_release);
    let source_directory = tempfile::tempdir().unwrap();
    let source = source_directory.path().join("mutable-native.so");
    std::fs::copy(&first_artifact, &source).unwrap();
    let first_digest = hex::encode(Sha256::digest(std::fs::read(&source).unwrap()));
    let second_digest = hex::encode(Sha256::digest(std::fs::read(native_fixture()).unwrap()));
    assert_ne!(first_digest, second_digest);

    let cache = tempfile::tempdir().unwrap();
    let mut options = CatalogOptions::new(cache.path());
    options.callback_timeout = Duration::from_secs(2);
    options.limits.maximum_concurrent_callbacks = 2;
    let catalog = NativeCatalog::new(options).unwrap();
    let first = tokio::task::spawn_blocking({
        let catalog = catalog.clone();
        let source = source.clone();
        move || catalog.load(source)
    });
    wait_for_file(&identity_entered).await;
    let second = tokio::task::spawn_blocking({
        let catalog = catalog.clone();
        let source = source.clone();
        move || catalog.load(source)
    });
    let second_admitted = std::time::Instant::now() + Duration::from_secs(1);
    while catalog.snapshot().active_loads < 2 && std::time::Instant::now() < second_admitted {
        tokio::task::yield_now().await;
    }
    assert_eq!(catalog.snapshot().active_loads, 2);

    let replacement = source_directory.path().join("replacement.so");
    std::fs::copy(native_fixture(), &replacement).unwrap();
    std::fs::rename(replacement, &source).unwrap();
    std::fs::write(&identity_release, b"release").unwrap();

    let first = first.await.unwrap().unwrap();
    let second = second.await.unwrap().unwrap();
    let digest = |factory: &rsi_meta::ResolvedFactory| match factory.identity() {
        FactoryIdentity::Native { sha256, .. } => sha256.clone(),
        identity @ FactoryIdentity::Linked { .. } => {
            panic!("native factory retained a non-native identity: {identity}")
        }
    };
    assert_eq!(digest(&first), first_digest);
    assert_eq!(digest(&second), second_digest);
}

#[test]
fn live_module_reuse_does_not_claim_a_second_staging_artifact() {
    let artifact_bytes = std::fs::metadata(native_fixture()).unwrap().len();
    let cache = tempfile::tempdir().unwrap();
    let mut options = CatalogOptions::new(cache.path());
    options.limits.maximum_staging_bytes = artifact_bytes;
    let catalog = NativeCatalog::new(options).unwrap();

    let first = catalog.load(native_fixture()).unwrap();
    assert_eq!(catalog.snapshot().staging_bytes, artifact_bytes);
    let second = catalog
        .load(native_fixture())
        .expect("a live module identity should not need another stable staging copy");

    assert_eq!(first.identity(), second.identity());
    assert_eq!(catalog.snapshot().staging_bytes, artifact_bytes);
    drop((first, second));
    wait_for_staging_release(&catalog);
}

#[cfg(unix)]
#[test]
fn catalog_rejects_a_symlink_at_its_lock_path() {
    let cache = tempfile::tempdir().unwrap();
    let target = cache.path().join("outside-lock");
    std::fs::write(&target, b"must remain ordinary data").unwrap();
    std::os::unix::fs::symlink(&target, cache.path().join(".rsi-meta.lock")).unwrap();

    assert!(matches!(
        NativeCatalog::new(CatalogOptions::new(cache.path())),
        Err(LoaderError::InvalidInput(_))
    ));
    assert_eq!(std::fs::read(target).unwrap(), b"must remain ordinary data");
}

#[cfg(unix)]
#[test]
fn catalog_rejects_a_symlink_cache_directory_before_claiming_it() {
    let parent = tempfile::tempdir().unwrap();
    let target = parent.path().join("target");
    std::fs::create_dir(&target).unwrap();
    let cache = parent.path().join("cache");
    std::os::unix::fs::symlink(&target, &cache).unwrap();

    assert!(matches!(
        NativeCatalog::new(CatalogOptions::new(&cache)),
        Err(LoaderError::InvalidInput(_))
    ));
    assert!(!target.join(".rsi-meta.lock").exists());
}

#[test]
fn catalog_rejects_existing_cache_contents_over_the_configured_budget() {
    let cache = tempfile::tempdir().unwrap();
    std::fs::write(
        cache.path().join(format!("{}.native", "0".repeat(64))),
        b"two bytes",
    )
    .unwrap();
    let mut options = CatalogOptions::new(cache.path());
    options.limits.maximum_cache_bytes = 1;

    assert!(matches!(
        NativeCatalog::new(options),
        Err(LoaderError::CapacityExhausted {
            resource: "cache bytes",
            limit: 1,
        })
    ));
}

#[test]
fn catalog_rejects_existing_cache_artifacts_over_the_configured_budget() {
    let cache = tempfile::tempdir().unwrap();
    std::fs::write(cache.path().join(format!("{}.native", "0".repeat(64))), b"").unwrap();
    std::fs::write(cache.path().join(format!("{}.native", "1".repeat(64))), b"").unwrap();
    let mut options = CatalogOptions::new(cache.path());
    options.limits.maximum_cache_artifacts = 1;

    assert!(matches!(
        NativeCatalog::new(options),
        Err(LoaderError::CapacityExhausted {
            resource: "cache artifacts",
            limit: 1,
        })
    ));
}

#[test]
fn catalog_rejects_unmanaged_cache_history() {
    let cache = tempfile::tempdir().unwrap();
    std::fs::write(cache.path().join("stale-staging-file"), b"stale").unwrap();

    assert!(matches!(
        NativeCatalog::new(CatalogOptions::new(cache.path())),
        Err(LoaderError::InvalidInput(_))
    ));
}

#[test]
fn failed_native_validation_does_not_commit_or_retain_staging_capacity() {
    let cache = tempfile::tempdir().unwrap();
    let source = tempfile::NamedTempFile::new().unwrap();
    std::fs::write(source.path(), b"not a dynamic library").unwrap();
    let catalog = NativeCatalog::new(CatalogOptions::new(cache.path())).unwrap();

    assert!(catalog.load(source.path()).is_err());

    wait_for_callback_quiescence(&catalog);
    let snapshot = catalog.snapshot();
    assert_eq!(snapshot.cache_artifacts, 0);
    assert_eq!(snapshot.cache_bytes, 0);
    assert_eq!(snapshot.staging_bytes, 0);
    assert_eq!(snapshot.active_callbacks, 0);
    assert!(
        std::fs::read_dir(cache.path())
            .unwrap()
            .filter_map(std::result::Result::ok)
            .all(|entry| !entry.file_name().to_string_lossy().ends_with(".native")),
        "failed validation left a durable digest"
    );
}

#[test]
fn successful_validation_commits_cache_and_releases_live_staging_on_drop() {
    let cache = tempfile::tempdir().unwrap();
    let artifact_bytes = std::fs::metadata(native_fixture()).unwrap().len();
    let catalog = NativeCatalog::new(CatalogOptions::new(cache.path())).unwrap();

    let factory = catalog.load(native_fixture()).unwrap();
    let loaded = catalog.snapshot();
    assert_eq!(loaded.cache_artifacts, 1);
    assert_eq!(loaded.cache_bytes, artifact_bytes);
    assert_eq!(loaded.peak_cache_artifacts, 1);
    assert_eq!(loaded.peak_cache_bytes, artifact_bytes);
    assert_eq!(loaded.staging_bytes, artifact_bytes);

    drop(factory);
    wait_for_staging_release(&catalog);

    drop(catalog);
    let reopened = NativeCatalog::new(CatalogOptions::new(cache.path())).unwrap();
    assert_eq!(reopened.snapshot().cache_artifacts, 1);
    assert_eq!(reopened.snapshot().cache_bytes, artifact_bytes);
}

#[test]
fn cache_commit_capacity_failure_rolls_back_the_reservation() {
    let cache = tempfile::tempdir().unwrap();
    let mut options = CatalogOptions::new(cache.path());
    options.limits.maximum_cache_bytes = 1;
    let catalog = NativeCatalog::new(options).unwrap();

    assert!(matches!(
        catalog.load(native_fixture()),
        Err(LoaderError::CapacityExhausted {
            resource: "cache bytes",
            limit: 1,
        })
    ));
    wait_for_staging_release(&catalog);
    let snapshot = catalog.snapshot();
    assert_eq!(snapshot.cache_artifacts, 0);
    assert_eq!(snapshot.cache_bytes, 0);
    assert_eq!(snapshot.staging_bytes, 0);
    assert_eq!(snapshot.rejected_cache_admissions, 1);
}

#[test]
fn staging_capacity_is_reserved_and_released_at_the_catalog_seam() {
    let cache = tempfile::tempdir().unwrap();
    let source = tempfile::NamedTempFile::new().unwrap();
    std::fs::write(source.path(), b"two bytes").unwrap();
    let mut options = CatalogOptions::new(cache.path());
    options.limits = NativeCatalogLimits {
        maximum_staging_bytes: 1,
        ..NativeCatalogLimits::default()
    };
    let catalog = NativeCatalog::new(options).unwrap();

    assert!(matches!(
        catalog.load(source.path()),
        Err(LoaderError::CapacityExhausted {
            resource: "staging bytes",
            limit: 1,
        })
    ));
    let snapshot = catalog.snapshot();
    assert_eq!(snapshot.staging_bytes, 0);
    assert_eq!(snapshot.rejected_staging_admissions, 1);
    assert_eq!(snapshot.peak_callbacks, 0);
}

#[test]
fn catalog_rejects_an_oversized_artifact_before_mapping() {
    let (_cache, catalog) = catalog();
    let file = tempfile::NamedTempFile::new().unwrap();
    file.as_file()
        .set_len(rsi_meta_native_loader::MAX_ARTIFACT_BYTES + 1)
        .unwrap();
    assert!(matches!(
        catalog.load(file.path()),
        Err(rsi_meta_native_loader::LoaderError::ArtifactTooLarge)
    ));
}

#[test]
fn catalog_rejects_existing_cache_bytes_that_do_not_match_the_source() {
    let (cache, catalog) = catalog();
    let bytes = std::fs::read(native_fixture()).unwrap();
    let digest = hex::encode(Sha256::digest(&bytes));
    std::fs::write(cache.path().join(format!("{digest}.native")), b"collision").unwrap();

    assert!(matches!(
        catalog.load(native_fixture()),
        Err(rsi_meta_native_loader::LoaderError::CacheCollision(_))
    ));
}

#[test]
fn catalog_accounts_for_an_identical_digest_created_after_startup() {
    let (cache, catalog) = catalog();
    let bytes = std::fs::read(native_fixture()).unwrap();
    let artifact_bytes = u64::try_from(bytes.len()).unwrap();
    let digest = hex::encode(Sha256::digest(&bytes));
    std::fs::write(cache.path().join(format!("{digest}.native")), bytes).unwrap();

    let factory = catalog.load(native_fixture()).unwrap();
    let snapshot = catalog.snapshot();
    assert_eq!(snapshot.cache_artifacts, 1);
    assert_eq!(snapshot.cache_bytes, artifact_bytes);
    assert_eq!(snapshot.peak_cache_artifacts, 1);
    assert_eq!(snapshot.peak_cache_bytes, artifact_bytes);

    drop(factory);
    wait_for_staging_release(&catalog);
}

#[cfg(unix)]
#[test]
fn live_module_reuse_does_not_consult_the_durable_cache() {
    let (cache, catalog) = catalog();
    let first = catalog.load(native_fixture()).unwrap();
    let bytes = std::fs::read(native_fixture()).unwrap();
    let digest = hex::encode(Sha256::digest(&bytes));
    std::fs::write(
        cache.path().join(format!("{digest}.native")),
        b"durable cache collision",
    )
    .unwrap();

    let second = catalog
        .load(native_fixture())
        .expect("a live private mapping does not depend on the durable cache");
    assert_eq!(first.identity(), second.identity());

    drop(first);
    drop(second);
    assert!(matches!(
        catalog.load(native_fixture()),
        Err(rsi_meta_native_loader::LoaderError::CacheCollision(_))
    ));
}

#[cfg(unix)]
#[test]
fn catalog_rejects_a_symlink_at_the_content_addressed_cache_path() {
    let (cache, catalog) = catalog();
    let bytes = std::fs::read(native_fixture()).unwrap();
    let digest = hex::encode(Sha256::digest(&bytes));
    std::os::unix::fs::symlink(
        native_fixture(),
        cache.path().join(format!("{digest}.native")),
    )
    .unwrap();

    assert!(matches!(
        catalog.load(native_fixture()),
        Err(rsi_meta_native_loader::LoaderError::InvalidInput(_))
    ));
}

#[cfg(unix)]
#[test]
fn catalog_rejects_a_fifo_without_waiting_for_a_writer() {
    let (_cache, catalog) = catalog();
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("artifact.fifo");
    assert!(
        std::process::Command::new("mkfifo")
            .arg(&path)
            .status()
            .expect("run mkfifo")
            .success()
    );
    let (result_tx, result_rx) = std::sync::mpsc::channel();
    let load = std::thread::spawn(move || {
        let result = matches!(
            catalog.load(&path),
            Err(rsi_meta_native_loader::LoaderError::InvalidInput(_))
        );
        let _ = result_tx.send(result);
    });
    assert!(
        result_rx
            .recv_timeout(Duration::from_secs(5))
            .expect("FIFO open blocked instead of using the nonblocking file boundary")
    );
    load.join().unwrap();
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn cache_commit_rejects_staging_mutation_after_native_validation_started() {
    use std::io::{Seek as _, SeekFrom, Write as _};

    const ORIGINAL_TRAILER: &[u8] = b"original-trailer";
    const MODIFIED_TRAILER: &[u8] = b"modified-trailer";

    let markers = tempfile::tempdir().unwrap();
    let identity_entered = markers.path().join("identity-entered");
    let identity_release = markers.path().join("identity-release");
    let (_fixture, artifact) = blocking_identity_fixture(&identity_entered, &identity_release);
    let mut source = std::fs::OpenOptions::new()
        .append(true)
        .open(&artifact)
        .unwrap();
    source.write_all(ORIGINAL_TRAILER).unwrap();
    drop(source);

    let cache = tempfile::tempdir().unwrap();
    let catalog = NativeCatalog::new(CatalogOptions::new(cache.path())).unwrap();
    let load = tokio::task::spawn_blocking({
        let catalog = catalog.clone();
        move || catalog.load(artifact)
    });
    wait_for_file(&identity_entered).await;

    let staged = std::fs::read_dir(cache.path())
        .unwrap()
        .map(std::result::Result::unwrap)
        .find(|entry| {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            name != ".rsi-meta.lock" && !name.ends_with(".native")
        })
        .expect("native validation did not retain its private staging file")
        .path();
    let mut staged = std::fs::OpenOptions::new()
        .write(true)
        .open(staged)
        .unwrap();
    staged
        .seek(SeekFrom::End(
            -i64::try_from(ORIGINAL_TRAILER.len()).unwrap(),
        ))
        .unwrap();
    staged.write_all(MODIFIED_TRAILER).unwrap();
    drop(staged);
    std::fs::write(&identity_release, b"release").unwrap();

    assert!(matches!(
        load.await.unwrap(),
        Err(LoaderError::StagedArtifactChanged)
    ));
    wait_for_staging_release_async(&catalog).await;
    let snapshot = catalog.snapshot();
    assert_eq!(snapshot.cache_artifacts, 0);
    assert_eq!(snapshot.cache_bytes, 0);
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn timed_out_artifact_entry_fences_reentry_until_the_worker_returns() {
    let markers = tempfile::tempdir().unwrap();
    let entry_log = markers.path().join("entry-log");
    let entry_release = markers.path().join("entry-release");
    let (_fixture, artifact) = blocking_entry_fixture(&entry_log, &entry_release);
    let (cache, catalog) = catalog_with_timeout(Duration::from_millis(30));

    let first = tokio::task::spawn_blocking({
        let catalog = catalog.clone();
        let artifact = artifact.clone();
        move || catalog.load(artifact)
    });
    wait_for_file(&entry_log).await;
    assert!(matches!(
        first.await.unwrap(),
        Err(LoaderError::Timeout("native module initialization"))
    ));

    assert!(matches!(
        catalog.load(&artifact),
        Err(LoaderError::Callback {
            operation: "load",
            ..
        })
    ));
    assert_eq!(
        std::fs::read(&entry_log).unwrap(),
        b"x",
        "a second worker re-entered a still-running native entry callback"
    );
    drop(catalog);
    assert!(matches!(
        NativeCatalog::new(CatalogOptions::new(cache.path())),
        Err(LoaderError::CacheLocked(_))
    ));

    let attempts = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let contender = std::thread::spawn({
        let attempts = Arc::clone(&attempts);
        let cache = cache.path().to_path_buf();
        move || loop {
            match NativeCatalog::new(CatalogOptions::new(&cache)) {
                Err(LoaderError::CacheLocked(_)) => {
                    attempts.fetch_add(1, std::sync::atomic::Ordering::Release);
                    std::hint::spin_loop();
                }
                result => return result,
            }
        }
    });
    while attempts.load(std::sync::atomic::Ordering::Acquire) < 100 {
        tokio::task::yield_now().await;
    }
    std::fs::write(&entry_release, b"release").unwrap();
    let reopened = contender
        .join()
        .unwrap()
        .expect("cache ownership was released before failed staging cleanup");
    assert_eq!(reopened.snapshot().staging_bytes, 0);
}
