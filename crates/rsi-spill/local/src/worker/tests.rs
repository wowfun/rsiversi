use super::*;
use std::fs::FileTimes;
use tokio::sync::Semaphore;

fn cleanup(worker: &mut Worker, scope: SpillScope, remove: bool) -> rsi_spill::Result<bool> {
    let (done, receiver) = tokio::sync::oneshot::channel();
    worker.command(Command::Cleanup {
        scope,
        remove,
        done,
    });
    while worker.cleanup.is_some() {
        worker.cleanup_slice();
    }
    receiver.blocking_recv().unwrap()
}

fn sweep(worker: &mut Worker, now: SystemTime, startup: bool) {
    worker.start_scan(now, startup);
    while worker.scan.is_some() {
        worker.scan_slice();
    }
}

fn worker(root: &Path) -> Worker {
    Worker::open(
        SpillLocalConfig::new(root.join("spill")),
        Arc::new(State {
            closed: false.into(),
            fenced: Mutex::default(),
            bytes: Arc::new(Semaphore::new(QUEUED_BYTES)),
        }),
    )
    .unwrap()
}

#[cfg(unix)]
#[test]
fn shutdown_unlocks_even_while_a_duplicate_descriptor_is_retained() {
    let root = tempfile::tempdir().unwrap();
    let running = worker(root.path());
    let inherited = running.lease.try_clone().unwrap();
    drop(running);
    let replacement = worker(root.path());
    drop((replacement, inherited));
}

#[test]
fn empty_capture_does_not_materialize_a_scope_or_publish_an_object() {
    let root = tempfile::tempdir().unwrap();
    let mut worker = worker(root.path());
    let scope = SpillScope::new("session", "empty");
    let locator = Arc::new(SpillLocator::new(&scope, [3; 16]));
    let status = Arc::new(Mutex::new(CaptureStatus::Pending));
    worker.begin(locator.clone(), status.clone()).unwrap();
    assert!(!worker.config.directory.join(scope.as_str()).exists());
    worker.finish(&locator);
    assert!(!matches!(
        *status.lock().unwrap(),
        CaptureStatus::Complete(_)
    ));
    assert!(!worker.config.directory.join(scope.as_str()).exists());
}

#[test]
fn interrupted_cleanup_marker_is_not_clean() {
    let root = tempfile::tempdir().unwrap();
    let mut worker = worker(root.path());
    let scope = SpillScope::new("session", "interrupted");
    let directory = native::scope_directory(&worker.directory, scope.as_str(), true).unwrap();
    drop(native::create_file(&directory, Path::new(".purged")).unwrap());
    drop(directory);
    assert!(!cleanup(&mut worker, scope.clone(), false).unwrap());
    assert!(cleanup(&mut worker, scope.clone(), true).unwrap());
    assert!(cleanup(&mut worker, scope, false).unwrap());
}
fn object(
    worker: &Worker,
    scope: &SpillScope,
    suffix: &str,
    time: SystemTime,
) -> std::path::PathBuf {
    let directory = native::scope_directory(&worker.directory, scope.as_str(), true).unwrap();
    let name = format!("{}{suffix}", "ab".repeat(16));
    let mut file = native::create_file(&directory, Path::new(&name)).unwrap();
    file.write_all(b"fixture").unwrap();
    file.set_times(FileTimes::new().set_modified(time)).unwrap();
    worker.config.directory.join(scope.as_str()).join(name)
}

#[test]
fn scoped_cleanup_yields_to_queued_capture_and_shutdown() {
    let root = tempfile::tempdir().unwrap();
    let mut worker = worker(root.path());
    worker.config.minimum_free_bytes = 0;
    let scope = SpillScope::new("session", "large-purge");
    let old = object(&worker, &scope, ".log", SystemTime::now());
    let (sender, receiver) = mpsc::sync_channel(8);
    let (done, mut completion) = tokio::sync::oneshot::channel();
    sender
        .send(Command::Cleanup {
            scope,
            remove: true,
            done,
        })
        .unwrap();
    let locator = Arc::new(SpillLocator::new(
        &SpillScope::new("session", "other"),
        [7; 16],
    ));
    let status = Arc::new(Mutex::new(CaptureStatus::Pending));
    sender
        .send(Command::Begin {
            locator: locator.clone(),
            status: status.clone(),
        })
        .unwrap();
    sender
        .send(Command::Write {
            locator: locator.clone(),
            bytes: b"ok".to_vec(),
            _permit: worker
                .state
                .bytes
                .clone()
                .try_acquire_many_owned(2)
                .unwrap(),
        })
        .unwrap();
    let (done, _) = tokio::sync::oneshot::channel();
    sender.send(Command::Finish { locator, done }).unwrap();
    sender.send(Command::Stop).unwrap();
    worker.run(&receiver);
    assert!(matches!(
        *status.lock().unwrap(),
        CaptureStatus::Complete(_)
    ));
    assert!(
        old.exists(),
        "scoped cleanup must yield before walking the namespace"
    );
    assert!(matches!(
        completion.try_recv(),
        Err(tokio::sync::oneshot::error::TryRecvError::Closed)
    ));
    assert!(old.parent().unwrap().join(".purged").exists());
}

#[test]
fn scoped_cleanup_is_sliced_and_status_short_circuits() {
    let root = tempfile::tempdir().unwrap();
    let mut worker = worker(root.path());
    let scope = SpillScope::new("session", "many-files");
    let directory = native::scope_directory(&worker.directory, scope.as_str(), true).unwrap();
    for index in 0..SCAN_ENTRIES * 3 {
        native::create_file(&directory, Path::new(&format!("{index:032x}.log"))).unwrap();
    }
    native::create_file(&directory, Path::new("foreign")).unwrap();
    assert!(!cleanup(&mut worker, scope.clone(), false).unwrap());
    let (done, mut completion) = tokio::sync::oneshot::channel();
    worker.command(Command::Cleanup {
        scope: scope.clone(),
        remove: true,
        done,
    });
    worker.cleanup_slice();
    assert!(matches!(
        completion.try_recv(),
        Err(tokio::sync::oneshot::error::TryRecvError::Empty)
    ));
    assert!(directory.entries().unwrap().count() > SCAN_ENTRIES);
    let (done, status) = tokio::sync::oneshot::channel();
    worker.command(Command::Cleanup {
        scope: scope.clone(),
        remove: false,
        done,
    });
    assert!(!status.blocking_recv().unwrap().unwrap());
    while worker.cleanup.is_some() {
        worker.cleanup_slice();
    }
    assert!(completion.blocking_recv().unwrap().unwrap());
    assert_eq!(directory.entries().unwrap().count(), 1);
    assert!(cleanup(&mut worker, scope, false).unwrap());
}

#[test]
fn cancelled_status_releases_cursor_but_cancelled_removal_finishes() {
    let root = tempfile::tempdir().unwrap();
    let mut worker = worker(root.path());
    let scope = SpillScope::new("session", "cancelled-cleanup");
    let directory = native::scope_directory(&worker.directory, scope.as_str(), true).unwrap();
    for index in 0..SCAN_ENTRIES * 3 {
        native::create_file(&directory, Path::new(&format!("foreign-{index}"))).unwrap();
    }
    let path = object(&worker, &scope, ".log", SystemTime::now());
    for remove in [false, true] {
        let (done, completion) = tokio::sync::oneshot::channel();
        worker.command(Command::Cleanup {
            scope: scope.clone(),
            remove,
            done,
        });
        drop(completion);
        worker.cleanup_slice();
        if remove {
            assert!(worker.cleanup.is_some());
            while worker.cleanup.is_some() {
                worker.cleanup_slice();
            }
            assert!(!path.exists());
        } else {
            assert!(worker.cleanup.is_none());
            assert!(path.exists());
        }
    }
}

#[test]
fn age_cutoff_is_strict_and_never_reaps_active_or_unknown_files() {
    let root = tempfile::tempdir().unwrap();
    let mut worker = worker(root.path());
    let now = SystemTime::now();
    let cutoff = now - Duration::from_hours(30 * 24);
    let old = object(
        &worker,
        &SpillScope::new("session", "old"),
        ".log",
        cutoff - Duration::from_secs(1),
    );
    let boundary = object(
        &worker,
        &SpillScope::new("session", "boundary"),
        ".log",
        cutoff,
    );
    let partial = object(
        &worker,
        &SpillScope::new("session", "active"),
        ".part",
        cutoff - Duration::from_secs(1),
    );
    let unknown = worker.config.directory.join("unrelated.log");
    std::fs::write(&unknown, b"keep").unwrap();
    sweep(&mut worker, now, false);
    assert!(!old.exists());
    assert!(boundary.exists());
    assert!(partial.exists());
    assert!(unknown.exists());
    worker.config.cleanup_period_days = 0;
    sweep(&mut worker, now + Duration::from_hours(24), false);
    assert!(boundary.exists());
    sweep(&mut worker, now, true);
    assert!(!partial.exists());
    assert!(unknown.exists());
}

#[test]
fn pending_chunks_participate_in_space_admission() {
    let root = tempfile::tempdir().unwrap();
    let mut worker = worker(root.path());
    worker.config.minimum_free_bytes = 100;
    worker.remaining_space = 109;
    assert!(worker.admit_space().is_ok());
    let held = worker
        .state
        .bytes
        .clone()
        .try_acquire_many_owned(10)
        .unwrap();
    assert!(worker.admit_space().is_err());
    worker.remaining_space = 110;
    assert!(worker.admit_space().is_ok());
    drop(held);
}

#[test]
fn queued_capture_and_shutdown_precede_the_startup_sweep() {
    let root = tempfile::tempdir().unwrap();
    let mut original = worker(root.path());
    original.config.minimum_free_bytes = 0;
    let old = object(
        &original,
        &SpillScope::new("session", "old"),
        ".log",
        SystemTime::now() - Duration::from_hours(31 * 24),
    );
    let config = original.config.clone();
    let state = original.state.clone();
    drop(original);
    let worker = Worker::open(config, state.clone()).unwrap();
    let (sender, receiver) = mpsc::sync_channel(8);
    let locator = Arc::new(SpillLocator::new(
        &SpillScope::new("session", "new"),
        [1; 16],
    ));
    let status = Arc::new(Mutex::new(CaptureStatus::Pending));
    sender
        .send(Command::Begin {
            locator: locator.clone(),
            status: status.clone(),
        })
        .unwrap();
    sender
        .send(Command::Write {
            locator: locator.clone(),
            bytes: b"complete".to_vec(),
            _permit: state.bytes.clone().try_acquire_many_owned(8).unwrap(),
        })
        .unwrap();
    let (done, _) = tokio::sync::oneshot::channel();
    sender.send(Command::Finish { locator, done }).unwrap();
    sender.send(Command::Stop).unwrap();
    worker.run(&receiver);
    assert!(matches!(
        *status.lock().unwrap(),
        CaptureStatus::Complete(_)
    ));
    assert!(
        old.exists(),
        "startup must yield to already queued capture and shutdown"
    );
}

#[test]
fn incremental_sweep_reaches_every_scope_and_preserves_new_active_partials() {
    let root = tempfile::tempdir().unwrap();
    let mut worker = worker(root.path());
    worker.config.minimum_free_bytes = 0;
    let now = SystemTime::now();
    let old: Vec<_> = (0..SCAN_ENTRIES * 2)
        .map(|index| {
            object(
                &worker,
                &SpillScope::new("session", &format!("old-{index}")),
                ".log",
                now - Duration::from_hours(31 * 24),
            )
        })
        .collect();
    worker.start_scan(now, true);
    let locator = Arc::new(SpillLocator::new(
        &SpillScope::new("session", "active-new"),
        [2; 16],
    ));
    let status = Arc::new(Mutex::new(CaptureStatus::Pending));
    worker.begin(locator.clone(), status.clone()).unwrap();
    worker.write(&locator, b"keep");
    worker.scan_slice();
    assert!(
        old.iter().any(|path| path.exists()),
        "one slice must not visit the entire namespace"
    );
    while worker.scan.is_some() {
        worker.scan_slice();
    }
    assert!(old.iter().all(|path| !path.exists()));
    worker.finish(&locator);
    assert!(matches!(
        *status.lock().unwrap(),
        CaptureStatus::Complete(_)
    ));
    assert_eq!(
        &worker.read((*locator).clone(), 0, 64).unwrap().bytes[..],
        b"keep"
    );
}

#[test]
fn publication_collision_preserves_the_preexisting_complete_object() {
    let root = tempfile::tempdir().unwrap();
    let mut worker = worker(root.path());
    worker.config.minimum_free_bytes = 0;
    let scope = SpillScope::new("session", "collision");
    let locator = Arc::new(SpillLocator::new(&scope, [0xab; 16]));
    let status = Arc::new(Mutex::new(CaptureStatus::Pending));
    worker.begin(locator.clone(), status.clone()).unwrap();
    worker.write(&locator, b"new");
    let existing = object(&worker, &scope, ".log", SystemTime::now());
    worker.finish(&locator);
    assert!(matches!(
        *status.lock().unwrap(),
        CaptureStatus::Unavailable
    ));
    assert_eq!(std::fs::read(existing).unwrap(), b"fixture");
}

#[test]
fn interrupted_cleanup_retries_on_restart_and_preserves_foreign_children() {
    let root = tempfile::tempdir().unwrap();
    let worker = worker(root.path());
    let scope = SpillScope::new("session", "purged");
    let output = object(&worker, &scope, ".log", SystemTime::now());
    let directory = native::scope_directory(&worker.directory, scope.as_str(), false).unwrap();
    native::create_file(&directory, Path::new(".purged")).unwrap();
    native::create_file(&directory, Path::new("foreign")).unwrap();
    let config = worker.config.clone();
    let state = worker.state.clone();
    drop(directory);
    drop(worker);
    let mut reopened = Worker::open(config, state).unwrap();
    assert!(
        reopened
            .read(SpillLocator::new(&scope, [0xab; 16]), 0, 64)
            .is_err()
    );
    sweep(&mut reopened, SystemTime::now(), true);
    assert!(!output.exists());
    assert!(output.parent().unwrap().join("foreign").exists());
    assert!(cleanup(&mut reopened, scope, false).unwrap());
}

#[cfg(unix)]
#[test]
fn scans_and_reads_do_not_follow_scope_or_object_symlinks() {
    let root = tempfile::tempdir().unwrap();
    let mut worker = worker(root.path());
    let foreign = root.path().join("foreign");
    std::fs::create_dir(&foreign).unwrap();
    let file = foreign.join(format!("{}.log", "ab".repeat(16)));
    std::fs::write(&file, b"keep").unwrap();
    let scope = SpillScope::new("session", "link");
    std::os::unix::fs::symlink(&foreign, worker.config.directory.join(scope.as_str())).unwrap();
    let locator = SpillLocator::new(&scope, [0xab; 16]);
    sweep(
        &mut worker,
        SystemTime::now() + Duration::from_hours(31 * 24),
        true,
    );
    assert!(worker.read(locator, 0, 1024).is_err());
    assert!(cleanup(&mut worker, scope, true).is_err());
    assert_eq!(std::fs::read(file).unwrap(), b"keep");
}
