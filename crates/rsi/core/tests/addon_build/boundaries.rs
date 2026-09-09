use super::*;
use rsi::{NativeAddonBuildError, NativeAddonError};

#[test]
fn fingerprint_captures_missing_and_nested_inputs_and_rejects_changed_authority() {
    use std::os::unix::{fs::symlink, net::UnixListener};
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    let path = manifest(&root.join("source"), "exit 0\n");
    let text = fs::read_to_string(&path)
        .unwrap()
        .replace("'input']", "'input', 'nested', 'missing']");
    fs::write(&path, &text).unwrap();
    fs::create_dir(root.join("source/nested")).unwrap();
    fs::write(root.join("source/nested/part"), b"A").unwrap();
    let build = NativeAddonBuild::open(&path).unwrap();
    let first = build.fingerprint().unwrap();
    fs::write(root.join("source/nested/part"), b"B").unwrap();
    assert_ne!(first, build.fingerprint().unwrap());
    fs::write(root.join("source/nested/part"), b"A").unwrap();
    assert_eq!(first, build.fingerprint().unwrap());
    fs::write(root.join("source/missing"), b"created").unwrap();
    assert_ne!(first, build.fingerprint().unwrap());
    fs::remove_file(root.join("source/missing")).unwrap();
    symlink("input", root.join("source/missing")).unwrap();
    assert!(build.fingerprint().is_err());
    fs::remove_file(root.join("source/missing")).unwrap();
    let socket = UnixListener::bind(root.join("source/missing")).unwrap();
    assert!(build.fingerprint().is_err());
    drop(socket);
    fs::remove_file(root.join("source/missing")).unwrap();
    fs::write(&path, format!("{text}\n# revised\n")).unwrap();
    assert!(build.fingerprint().is_err());
    fs::write(&path, text).unwrap();
    fs::rename(root.join("source"), root.join("replaced")).unwrap();
    fs::create_dir(root.join("source")).unwrap();
    assert!(build.fingerprint().is_err());
}
#[test]
fn fingerprint_bounds_tree_and_file_bytes_before_build_and_rejects_output_watch() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    let path = manifest(&root.join("source"), "exit 0\n");
    let text = fs::read_to_string(&path).unwrap();
    fs::write(&path, text.replace("'input']", "'artifact.bin']")).unwrap();
    assert!(NativeAddonBuild::open(&path).is_err());
    fs::write(&path, &text).unwrap();
    let build = NativeAddonBuild::open(&path).unwrap();
    fs::File::create(root.join("source/input"))
        .unwrap()
        .set_len(16 * 1024 * 1024 + 1)
        .unwrap();
    assert!(matches!(
        build.fingerprint(),
        Err(NativeAddonError::Capacity(_))
    ));
    fs::write(root.join("source/input"), b"small").unwrap();
    fs::write(&path, text.replace("'input']", "'input', 'nested']")).unwrap();
    fs::create_dir(root.join("source/nested")).unwrap();
    for index in 0..1024 {
        fs::write(root.join(format!("source/nested/{index}")), []).unwrap();
    }
    assert!(matches!(
        NativeAddonBuild::open(&path).unwrap().fingerprint(),
        Err(NativeAddonError::Capacity(_))
    ));
}
#[tokio::test]
async fn changed_declared_input_prevents_publication_even_after_exit_zero() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    let path = manifest(
        &root.join("source"),
        "printf changed > input\ncp input artifact.bin\n",
    );
    let build = Arc::new(NativeAddonBuild::open(&path).unwrap());
    let store = Arc::new(NativeAddonStore::open(root.join("store")).unwrap());
    let manager = manager(&root).await;
    let result = manager
        .service()
        .run(
            build,
            store.clone(),
            environment(),
            CancellationToken::new(),
        )
        .await;
    assert!(matches!(
        result,
        Err(NativeAddonBuildError::Source(NativeAddonError::Conflict))
    ));
    assert_eq!(store.snapshot().unwrap().revision, 0);
    assert!(store.snapshot().unwrap().installed.is_empty());
    assert!(manager.shutdown().await.is_clean());
}
async fn until_exists(path: &Path) {
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        while !path.exists() {
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn explicit_cancellation_returns_settlement_and_reopens_service_admission() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    let path = manifest(
        &root.join("source"),
        "trap '' TERM\nsleep 30 &\necho $! > child.pid\nwait\n",
    );
    let text = fs::read_to_string(&path)
        .unwrap()
        .replace("timeout_seconds = 1", "timeout_seconds = 120");
    fs::write(&path, text).unwrap();
    let build = Arc::new(NativeAddonBuild::open(&path).unwrap());
    let store = Arc::new(NativeAddonStore::open(root.join("store")).unwrap());
    let manager = manager(&root).await;
    let service = manager.service().clone();
    let candidate = build.clone();
    let target = store.clone();
    let cancel = CancellationToken::new();
    let request_cancel = cancel.clone();
    let waiter = tokio::spawn(async move {
        service
            .run(candidate, target, environment(), request_cancel)
            .await
    });
    until_exists(&root.join("source/child.pid")).await;
    cancel.cancel();
    let report = waiter.await.unwrap().unwrap();
    assert_eq!(report.status, NativeAddonBuildStatus::Cancelled);
    assert!(report.installed.is_none());
    assert_not_running(&root.join("source/child.pid"));
    fs::write(root.join("source/build.sh"), "cp input artifact.bin\n").unwrap();
    let report = manager
        .service()
        .run(build, store, environment(), CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(report.status, NativeAddonBuildStatus::Succeeded);
    assert!(manager.shutdown().await.is_clean());
}
#[tokio::test]
async fn timeout_settles_real_process_group_and_preserves_bounded_raw_tails() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    let path = manifest(
        &root.join("source"),
        "trap '' TERM\nhead -c 70000 /dev/zero\nprintf diagnostic >&2\nsleep 30 &\necho $! > child.pid\nwait\n",
    );
    let build = Arc::new(NativeAddonBuild::open(&path).unwrap());
    let store = Arc::new(NativeAddonStore::open(root.join("store")).unwrap());
    let manager = manager(&root).await;
    let report = manager
        .service()
        .run(
            build,
            store.clone(),
            environment(),
            CancellationToken::new(),
        )
        .await
        .unwrap();
    assert_eq!(report.status, NativeAddonBuildStatus::TimedOut);
    assert_eq!(report.stdout.bytes, vec![0; 65536]);
    assert!(report.stdout.lossy);
    assert_eq!(report.stdout.oldest_offset, 70000 - 65536);
    assert_eq!(report.stdout.next_offset, 70000);
    assert_eq!(report.stderr.bytes, b"diagnostic");
    assert!(report.installed.is_none());
    assert_eq!(store.snapshot().unwrap().revision, 0);
    assert!(manager.shutdown().await.is_clean());
    assert_not_running(&root.join("source/child.pid"));
}
#[tokio::test]
async fn cancelled_waiter_keeps_work_owned_and_retirement_joins_before_releasing_source_lock() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    let path = manifest(
        &root.join("source"),
        "trap '' TERM\nsleep 30 &\necho $! > child.pid\nwait\n",
    );
    let build = Arc::new(NativeAddonBuild::open(&path).unwrap());
    let store = Arc::new(NativeAddonStore::open(root.join("store")).unwrap());
    let manager = manager(&root).await;
    let service = manager.service().clone();
    let retained = service.clone();
    let candidate = build.clone();
    let target = store.clone();
    let waiter = tokio::spawn(async move {
        service
            .run(candidate, target, environment(), CancellationToken::new())
            .await
    });
    until_exists(&root.join("source/child.pid")).await;
    assert!(matches!(
        retained
            .run(
                build.clone(),
                store.clone(),
                environment(),
                CancellationToken::new()
            )
            .await,
        Err(NativeAddonBuildError::Busy)
    ));
    let directory = fs::File::open(root.join("source")).unwrap();
    assert!(matches!(
        directory.try_lock(),
        Err(std::fs::TryLockError::WouldBlock)
    ));
    waiter.abort();
    assert!(waiter.await.unwrap_err().is_cancelled());
    assert!(manager.shutdown().await.is_clean());
    directory
        .try_lock()
        .expect("source lock released after owned cleanup");
    directory.unlock().unwrap();
    assert_not_running(&root.join("source/child.pid"));
    assert_eq!(store.snapshot().unwrap().revision, 0);
    assert!(matches!(
        retained
            .run(build, store, environment(), CancellationToken::new())
            .await,
        Err(NativeAddonBuildError::Closed)
    ));
}
fn assert_not_running(pid_path: &Path) {
    let pid = fs::read_to_string(pid_path).unwrap();
    let pid: u32 = pid.trim().parse().unwrap();
    #[cfg(target_os = "linux")]
    if let Ok(stat) = fs::read_to_string(format!("/proc/{pid}/stat")) {
        let state = stat.rsplit_once(") ").unwrap().1.as_bytes()[0];
        assert!(
            matches!(state, b'Z' | b'X'),
            "build descendant still running: {stat}"
        );
    }
    #[cfg(not(target_os = "linux"))]
    let _ = pid; // Native platform Process tests own their liveness oracle.
}

#[test]
fn build_executable_cannot_turn_into_an_env_assignment() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    let path = manifest(&root.join("source"), "exit 0\n");
    let text = fs::read_to_string(&path)
        .unwrap()
        .replace("'/bin/sh', 'build.sh'", "'PATH=/unreviewed'");
    fs::write(&path, text).unwrap();
    assert!(NativeAddonBuild::open(&path).is_err());
}
