use super::*;

#[test]
fn reset_archives_current_and_old_schema_bytes_without_opening_them() {
    for version in [AGENT_STORE_SCHEMA_VERSION, AGENT_STORE_SCHEMA_VERSION - 1] {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().join("agent");
        drop(SqliteStore::open(&root).unwrap());
        let db = Connection::open(root.join("sessions.sqlite3")).unwrap();
        db.pragma_update(None, "user_version", version).unwrap();
        drop(db);
        let database = fs::read(root.join("sessions.sqlite3")).unwrap();
        fs::write(root.join("sessions.sqlite3-wal"), b"opaque old WAL").unwrap();
        fs::write(root.join("cas/object"), b"old CAS").unwrap();
        fs::write(root.join("cas/staging/retained"), b"old staging").unwrap();
        fs::write(temporary.path().join("settings"), b"keep").unwrap();
        let lock = same_file::Handle::from_path(root.join(".writer.lock")).unwrap();
        let (store, receipt) = SqliteStore::reset_and_open(&root).unwrap();
        let backup = receipt.backup.unwrap();
        assert_eq!(fs::read(backup.join("sessions.sqlite3")).unwrap(), database);
        assert_eq!(
            fs::read(backup.join("sessions.sqlite3-wal")).unwrap(),
            b"opaque old WAL"
        );
        assert_eq!(fs::read(backup.join("cas/object")).unwrap(), b"old CAS");
        assert_eq!(
            fs::read(backup.join("cas/staging/retained")).unwrap(),
            b"old staging"
        );
        assert_eq!(
            lock,
            same_file::Handle::from_path(backup.join(".writer.lock")).unwrap()
        );
        assert_eq!(
            fs::read(temporary.path().join("settings")).unwrap(),
            b"keep"
        );
        assert!(!root.join("cas/object").exists());
        let db = Connection::open(root.join("sessions.sqlite3")).unwrap();
        assert_eq!(
            db.pragma_query_value(None, "user_version", |row| row.get::<_, u32>(0))
                .unwrap(),
            AGENT_STORE_SCHEMA_VERSION
        );
        assert_eq!(
            db.query_row("SELECT count(*) FROM sessions", [], |row| row
                .get::<_, i64>(0))
                .unwrap(),
            0
        );
        assert!(matches!(
            SqliteStore::open(&root),
            Err(StoreError::WriterLocked)
        ));
        drop(store);
    }
}

#[test]
fn missing_store_and_repeated_request_do_not_create_backups() {
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path().join("agent");
    let request = SqliteStoreResetRequest::new();
    drop(request.open(&root).unwrap());
    assert!(request.take_receipt().unwrap().backup.is_none());
    fs::write(root.join("retained"), b"after first boot").unwrap();
    drop(request.clone().open(&root).unwrap());
    assert!(request.take_receipt().is_none());
    assert_eq!(
        fs::read(root.join("retained")).unwrap(),
        b"after first boot"
    );
    assert_eq!(fs::read_dir(temporary.path()).unwrap().count(), 1);
}

#[test]
fn active_writer_prevents_reset_and_later_backups_never_overwrite() {
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path().join("agent");
    let store = SqliteStore::open(&root).unwrap();
    let error = SqliteStore::reset_and_open(&root).unwrap_err();
    assert!(matches!(error.source, StoreError::WriterLocked));
    assert!(error.receipt.backup.is_none());
    assert_eq!(fs::read_dir(temporary.path()).unwrap().count(), 1);
    drop(store);
    let (store, first) = SqliteStore::reset_and_open(&root).unwrap();
    let first = first.backup.unwrap();
    let before = fs::read(first.join("sessions.sqlite3")).unwrap();
    drop(store);
    let (_, second) = SqliteStore::reset_and_open(&root).unwrap();
    assert_ne!(Some(&first), second.backup.as_ref());
    assert_eq!(fs::read(first.join("sessions.sqlite3")).unwrap(), before);
}

#[test]
fn failure_after_backup_preserves_and_reports_the_old_root() {
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path().join("agent");
    drop(SqliteStore::open(&root).unwrap());
    let before = fs::read(root.join("sessions.sqlite3")).unwrap();
    let error = reset_and_open_with(&root, |_| {
        Err(StoreError::Io("injected open failure".into()))
    })
    .unwrap_err();
    assert!(error.to_string().contains("injected open failure"));
    let backup = error.receipt.backup.unwrap();
    assert_eq!(fs::read(backup.join("sessions.sqlite3")).unwrap(), before);
    assert!(!root.exists());
}

#[test]
fn a_moved_lock_handle_cannot_authorize_the_replacement_store() {
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path().join("agent");
    drop(SqliteStore::open(&root).unwrap());
    let old = OpenOptions::new()
        .read(true)
        .write(true)
        .open(root.join(".writer.lock"))
        .unwrap();
    let (store, _) = SqliteStore::reset_and_open(&root).unwrap();
    old.try_lock().unwrap();
    assert!(
        filesystem::validate_open_file(&root.join(".writer.lock"), &old, "writer lock").is_err()
    );
    assert!(matches!(
        SqliteStore::open(&root),
        Err(StoreError::WriterLocked)
    ));
    old.unlock().unwrap();
    drop(store);
}

#[cfg(unix)]
#[test]
fn root_symlink_suffixes_cannot_bypass_open_verify_or_reset_checks() {
    use std::os::unix::fs::symlink;

    for suffix in ["", "/", "/.", "///./."] {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().join("agent");
        drop(SqliteStore::open(&root).unwrap());
        let before = fs::read(root.join("sessions.sqlite3")).unwrap();
        let alias = temporary.path().join("alias");
        symlink(&root, &alias).unwrap();
        let mut aliased_path = alias.into_os_string();
        aliased_path.push(suffix);
        let aliased_path = PathBuf::from(aliased_path);
        let error = SqliteStore::reset_and_open(&aliased_path).unwrap_err();
        assert!(
            matches!(error.source, StoreError::Invalid(_)),
            "{error}; suffix {suffix:?}"
        );
        assert!(error.receipt.backup.is_none());
        assert!(
            matches!(
                SqliteStore::open(&aliased_path),
                Err(StoreError::Invalid(_))
            ),
            "ordinary open followed the linked root with suffix {suffix:?}"
        );
        assert!(matches!(
            SqliteStore::verify(&aliased_path),
            Err(StoreError::Invalid(_))
        ));
        assert_eq!(fs::read(root.join("sessions.sqlite3")).unwrap(), before);
        assert_eq!(fs::read_dir(temporary.path()).unwrap().count(), 2);
        let mut real_path = root.into_os_string();
        real_path.push(suffix);
        drop(SqliteStore::open(PathBuf::from(real_path)).unwrap());
    }
}

#[cfg(unix)]
#[test]
fn reset_rejects_root_symlinks_and_inaccessible_backup_parent() {
    use std::os::unix::fs::{PermissionsExt as _, symlink};
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path().join("agent");
    drop(SqliteStore::open(&root).unwrap());
    let alias = temporary.path().join("alias");
    symlink(&root, &alias).unwrap();
    assert!(matches!(
        SqliteStore::reset_and_open(&alias).unwrap_err().source,
        StoreError::Invalid(_)
    ));
    fs::set_permissions(temporary.path(), fs::Permissions::from_mode(0o500)).unwrap();
    let result = SqliteStore::reset_and_open(&root);
    fs::set_permissions(temporary.path(), fs::Permissions::from_mode(0o700)).unwrap();
    // Root can bypass directory permissions; ordinary test runners cannot.
    if let Err(error) = result {
        assert!(error.receipt.backup.is_none());
        assert!(root.join("sessions.sqlite3").is_file());
    }
}

#[test]
fn unrelated_nonempty_directory_is_not_a_store_reset_target() {
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path().join("home");
    fs::create_dir(&root).unwrap();
    fs::write(root.join("credentials"), b"preserve").unwrap();
    let error = SqliteStore::reset_and_open(&root).unwrap_err();
    assert!(matches!(error.source, StoreError::Invalid(_)));
    assert!(error.receipt.backup.is_none());
    assert_eq!(fs::read(root.join("credentials")).unwrap(), b"preserve");
    assert_eq!(fs::read_dir(&root).unwrap().count(), 1);
    assert_eq!(fs::read_dir(temporary.path()).unwrap().count(), 1);
}

#[test]
fn empty_store_initializes_without_a_backup() {
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path().join("agent");
    fs::create_dir(&root).unwrap();
    let (_, receipt) = SqliteStore::reset_and_open(&root).unwrap();
    assert!(receipt.backup.is_none());
    assert_eq!(fs::read_dir(temporary.path()).unwrap().count(), 1);
}

#[test]
fn unwound_request_cannot_fall_back_to_an_ordinary_open() {
    for after_backup in [false, true] {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().join("agent");
        drop(SqliteStore::open(&root).unwrap());
        let before = fs::read(root.join("sessions.sqlite3")).unwrap();
        let request = SqliteStoreResetRequest::new();
        let panic = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            request.open_with(&root, |root| {
                assert!(after_backup, "injected panic before backup");
                reset_and_open_progress(
                    root,
                    |_| panic!("injected panic after backup"),
                    |receipt| request.1.publish(receipt.clone()),
                )
            })
        }));
        assert!(panic.is_err());
        let error = request.clone().open(&root).unwrap_err();
        assert!(
            error.to_string().contains("reset did not complete"),
            "{error}"
        );
        if after_backup {
            let backup = request.take_receipt().unwrap().backup.unwrap();
            assert_eq!(fs::read(backup.join("sessions.sqlite3")).unwrap(), before);
            assert!(!root.exists());
        } else {
            assert!(request.take_receipt().is_none());
            assert_eq!(fs::read(root.join("sessions.sqlite3")).unwrap(), before);
        }
    }
}

#[test]
fn failed_request_does_not_fall_back_to_an_ordinary_open() {
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path().join("agent");
    let store = SqliteStore::open(&root).unwrap();
    let request = SqliteStoreResetRequest::new();
    assert!(matches!(request.open(&root), Err(StoreError::WriterLocked)));
    drop(store);
    assert!(matches!(
        request.clone().open(&root),
        Err(StoreError::WriterLocked)
    ));
    assert_eq!(fs::read_dir(temporary.path()).unwrap().count(), 1);
}

#[test]
fn request_failure_after_backup_cannot_create_an_empty_store_on_retry() {
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path().join("agent");
    drop(SqliteStore::open(&root).unwrap());
    let before = fs::read(root.join("sessions.sqlite3")).unwrap();
    let request = SqliteStoreResetRequest::new();
    let first = request
        .open_with(&root, |root| {
            reset_and_open_with(root, |_| {
                Err(StoreError::Io("injected new Store failure".into()))
            })
        })
        .unwrap_err();
    let backup = request.take_receipt().unwrap().backup.unwrap();
    assert_eq!(fs::read(backup.join("sessions.sqlite3")).unwrap(), before);
    assert!(!root.exists());
    assert_eq!(
        request.open(&root).unwrap_err().to_string(),
        first.to_string()
    );
    assert!(!root.exists());
    assert!(request.take_receipt().is_none());
    assert_eq!(fs::read_dir(temporary.path()).unwrap().count(), 1);
}

#[tokio::test]
async fn reset_receipt_is_available_while_replacement_initialization_is_blocked() {
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path().join("agent");
    drop(SqliteStore::open(&root).unwrap());
    let request = SqliteStoreResetRequest::new();
    let opening = request.clone();
    let path = root.clone();
    let (release, wait) = std::sync::mpsc::channel();
    let work = tokio::task::spawn_blocking(move || {
        opening.open_with(&path, |root| {
            reset_and_open_progress(
                root,
                |_| {
                    wait.recv().unwrap();
                    Err(StoreError::Io("cold startup failed after backup".into()))
                },
                |receipt| opening.1.publish(receipt.clone()),
            )
        })
    });
    tokio::time::timeout(Duration::from_secs(5), request.receipt_ready())
        .await
        .unwrap();
    let receipt = request.take_receipt().unwrap();
    assert_eq!(receipt.root, root);
    assert!(receipt.backup.unwrap().join("sessions.sqlite3").exists());
    assert!(
        !work.is_finished(),
        "receipt must precede completion of reset/open"
    );
    release.send(()).unwrap();
    assert!(work.await.unwrap().is_err());
    assert!(request.take_receipt().is_none(), "one receipt per reset");
}
