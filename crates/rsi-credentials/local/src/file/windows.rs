use super::codec::{Entries, MAX_BYTES, decode, encode};
use super::{CredentialRef, CredentialsError, Failure, Path, Result, SecretValue};
use rsi_files_native_fs as native;
use std::{
    fs::File,
    io::{Read, Write},
    path::PathBuf,
    sync::atomic::{AtomicU64, Ordering},
    time::{Duration, Instant},
};
use zeroize::Zeroizing;

static SEQUENCE: AtomicU64 = AtomicU64::new(0);
fn failure(kind: Failure) -> CredentialsError {
    CredentialsError::Store(kind)
}
#[expect(
    clippy::needless_pass_by_value,
    reason = "Result::map_err consumes the native error without exposing its text."
)]
fn io(error: std::io::Error) -> CredentialsError {
    failure(
        if native::is_link_rejection(&error) || error.kind() == std::io::ErrorKind::InvalidInput {
            Failure::UnsafePath
        } else if error.kind() == std::io::ErrorKind::PermissionDenied {
            Failure::Permissions
        } else {
            Failure::Io
        },
    )
}
fn name(path: &Path) -> Result<&std::ffi::OsStr> {
    path.file_name().ok_or_else(|| failure(Failure::UnsafePath))
}
fn validate_file(file: &File) -> Result<()> {
    if native::file_version(file).map_err(io)?.link_count != 1 {
        return Err(failure(Failure::UnsafePath));
    }
    Ok(())
}
fn directory(path: &Path, create: bool) -> Result<Option<native::Directory>> {
    if path
        .to_str()
        .is_none_or(|p| p.len() > 2048 || p.chars().any(char::is_control))
    {
        return Err(failure(Failure::UnsafePath));
    }
    native::validate_absolute_path(path).map_err(io)?;
    let parent = path.parent().ok_or_else(|| failure(Failure::UnsafePath))?;
    let directory = if create {
        native::create_absolute_directory_and_sync_no_follow(parent)
    } else {
        native::open_absolute_directory_no_follow(parent)
    };
    let directory = match directory {
        Ok(directory) => directory,
        Err(error) if !create && error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(io(error)),
    };
    directory.verify_private().map_err(io)?;
    Ok(Some(directory))
}
fn read(dir: &native::Directory, path: &Path) -> Result<Entries> {
    let mut file = match native::open_relative_private_file_no_follow(dir, Path::new(name(path)?)) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Entries::new()),
        Err(error) => return Err(io(error)),
    };
    validate_file(&file)?;
    let size = file.metadata().map_err(io)?.len();
    if size > MAX_BYTES as u64 {
        return Err(failure(Failure::TooLarge));
    }
    let mut bytes = Zeroizing::new(vec![
        0;
        usize::try_from(size)
            .map_err(|_| failure(Failure::TooLarge))?
    ]);
    file.read_exact(&mut bytes).map_err(io)?;
    let mut extra = Zeroizing::new([0u8; 1]);
    if file.read(&mut *extra).map_err(io)? != 0 {
        return Err(failure(Failure::Corrupt));
    }
    decode(&bytes)
}
pub(super) fn get(path: &Path, reference: &CredentialRef) -> Result<Option<SecretValue>> {
    let Some(dir) = directory(path, false)? else {
        return Ok(None);
    };
    Ok(read(&dir, path)?.remove(reference))
}
fn lock(dir: &native::Directory, path: &Path) -> Result<File> {
    let lock_name = PathBuf::from(format!(
        ".{}.lock",
        name(path)?
            .to_str()
            .ok_or_else(|| failure(Failure::UnsafePath))?
    ));
    let file = match native::create_relative_private_file_no_follow(dir, &lock_name) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            native::open_relative_private_file_no_follow(dir, &lock_name).map_err(io)?
        }
        Err(error) => return Err(io(error)),
    };
    validate_file(&file)?;
    let deadline = Instant::now() + Duration::from_secs(1);
    loop {
        match file.try_lock() {
            Ok(()) => return Ok(file),
            Err(std::fs::TryLockError::WouldBlock) if Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(10));
            }
            Err(std::fs::TryLockError::WouldBlock) => return Err(failure(Failure::LockTimeout)),
            Err(std::fs::TryLockError::Error(error)) => return Err(io(error)),
        }
    }
}
pub(super) fn modify(
    path: &Path,
    reference: &CredentialRef,
    secret: Option<&SecretValue>,
) -> Result<bool> {
    let dir = directory(path, true)?.ok_or_else(|| failure(Failure::Io))?;
    let namespace = dir.pin_namespace().map_err(io)?;
    let _lock = lock(&dir, path)?;
    let mut entries = read(&dir, path)?;
    let existed = if let Some(secret) = secret {
        entries.insert(reference.clone(), secret.clone()).is_some()
    } else {
        entries.remove(reference).is_some()
    };
    if secret.is_none() && !existed {
        return Ok(false);
    }
    let bytes = encode(&entries)?;
    let (temporary, mut file) = temporary(&dir)?;
    let result = validate_file(&file).and_then(|()| {
        file.write_all(&bytes)
            .and_then(|()| file.sync_all())
            .map_err(io)
    });
    drop(file);
    let result = result
        .and_then(|()| {
            namespace
                .publish_child(
                    &temporary,
                    Path::new(name(path)?),
                    native::EntryKind::File,
                    true,
                )
                .map_err(io)
        })
        .and_then(|()| {
            namespace
                .sync_all()
                .map_err(|_| CredentialsError::OutcomeUnknown)
        });
    if result.is_err() {
        let _ = std::fs::remove_file(path.parent().unwrap().join(&temporary));
    }
    result.map(|()| existed)
}
fn temporary(dir: &native::Directory) -> Result<(PathBuf, File)> {
    for _ in 0..64 {
        let sequence = SEQUENCE
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |v| v.checked_add(1))
            .map_err(|_| failure(Failure::Io))?;
        let name = PathBuf::from(format!(
            ".credentials.{}.{sequence}.tmp",
            std::process::id()
        ));
        match native::create_relative_private_file_no_follow(dir, &name) {
            Ok(file) => return Ok((name, file)),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(io(error)),
        }
    }
    Err(failure(Failure::Io))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{FileSecretStore, SecretStore};
    fn reference(slot: &str) -> CredentialRef {
        CredentialRef::new("fixture.provider", slot).unwrap()
    }
    fn key(value: &str) -> SecretValue {
        SecretValue::new(value).unwrap()
    }
    #[test]
    fn reads_are_side_effect_free_and_native_saved_entries_survive_reopen() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("private/credentials.json");
        let store = FileSecretStore::new(&path);
        assert!(store.get(&reference("one")).unwrap().is_none());
        assert!(!path.parent().unwrap().exists());
        store.set(&reference("one"), &key("first")).unwrap();
        store.set(&reference("two"), &key("second")).unwrap();
        store.set(&reference("one"), &key("replacement")).unwrap();
        let reopened = FileSecretStore::new(&path);
        assert_eq!(
            reopened
                .get(&reference("one"))
                .unwrap()
                .unwrap()
                .expose_secret(),
            "replacement"
        );
        assert!(reopened.unset(&reference("one")).unwrap());
        assert!(!reopened.unset(&reference("one")).unwrap());
        assert_eq!(
            store
                .get(&reference("two"))
                .unwrap()
                .unwrap()
                .expose_secret(),
            "second"
        );
    }
    #[test]
    fn native_corrupt_document_is_preserved_and_diagnostics_are_redacted() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("private/credentials.json");
        let dir =
            native::create_absolute_directory_and_sync_no_follow(path.parent().unwrap()).unwrap();
        let mut file =
            native::create_relative_private_file_no_follow(&dir, Path::new("credentials.json"))
                .unwrap();
        let bytes = b"{\"version\":1,\"version\":1,\"secret-marker\":true}";
        file.write_all(bytes).unwrap();
        drop(file);
        let store = FileSecretStore::new(&path);
        let error = store.get(&reference("one")).unwrap_err();
        assert_eq!(error, failure(Failure::Corrupt));
        assert!(!format!("{error:?} {error}").contains("secret-marker"));
        assert_eq!(
            store.set(&reference("one"), &key("replacement")),
            Err(failure(Failure::Corrupt))
        );
        assert_eq!(std::fs::read(&path).unwrap(), bytes);
    }
    #[test]
    fn native_credential_files_and_writer_locks_reject_multiple_links() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("private/credentials.json");
        let store = FileSecretStore::new(&path);
        store.set(&reference("one"), &key("fixture")).unwrap();
        let alias = root.path().join("alias");
        std::fs::hard_link(&path, &alias).unwrap();
        assert!(matches!(
            store.get(&reference("one")),
            Err(CredentialsError::Store(Failure::UnsafePath))
        ));
        std::fs::remove_file(alias).unwrap();
        let lock_path = path.parent().unwrap().join(".credentials.json.lock");
        std::fs::hard_link(lock_path, root.path().join("lock-alias")).unwrap();
        assert_eq!(
            store.set(&reference("two"), &key("fixture")),
            Err(failure(Failure::UnsafePath))
        );
        assert_eq!(
            store
                .get(&reference("one"))
                .unwrap()
                .unwrap()
                .expose_secret(),
            "fixture"
        );
    }
    #[test]
    fn unprotected_parent_and_aliases_refuse_before_creating_writer_state() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("credentials.json");
        let store = FileSecretStore::new(path);
        assert!(matches!(
            store.get(&reference("one")),
            Err(CredentialsError::Store(Failure::Permissions))
        ));
        assert_eq!(
            store.set(&reference("one"), &key("first")),
            Err(failure(Failure::Permissions))
        );
        assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 0);
        for path in [
            r"D:relative\credentials.json",
            r"\\?\D:\missing\credentials.json",
            r"D:\missing\credentials.json:ads",
        ] {
            assert!(matches!(
                FileSecretStore::new(path).get(&reference("one")),
                Err(CredentialsError::Store(Failure::UnsafePath))
            ));
        }
    }
    #[test]
    fn native_independent_writers_preserve_records_and_lock_timeout_is_bounded() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("private/credentials.json");
        FileSecretStore::new(&path)
            .set(&reference("seed"), &key("seed"))
            .unwrap();
        let threads: Vec<_> = (0..8)
            .map(|i| {
                let path = path.clone();
                std::thread::spawn(move || {
                    FileSecretStore::new(path)
                        .set(&reference(&i.to_string()), &key("fixture"))
                        .unwrap();
                })
            })
            .collect();
        for thread in threads {
            thread.join().unwrap();
        }
        let store = FileSecretStore::new(&path);
        for i in 0..8 {
            assert_eq!(
                store
                    .get(&reference(&i.to_string()))
                    .unwrap()
                    .unwrap()
                    .expose_secret(),
                "fixture"
            );
        }
        let dir = directory(&path, false).unwrap().unwrap();
        let guard = lock(&dir, &path).unwrap();
        let start = Instant::now();
        assert_eq!(
            store.set(&reference("blocked"), &key("fixture")),
            Err(failure(Failure::LockTimeout))
        );
        assert!(start.elapsed() < Duration::from_secs(3));
        drop(guard);
    }
}
