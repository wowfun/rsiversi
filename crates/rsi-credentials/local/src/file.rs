use crate::SecretStore;
use rsi_credentials_protocol::{
    CredentialRef, CredentialStoreFailure as Failure, CredentialsError, Result, SecretValue,
};
use std::path::{Path, PathBuf};

/// Private, bounded JSON credential store. Construction performs no filesystem I/O.
#[derive(Debug)]
pub struct FileSecretStore {
    path: PathBuf,
    #[cfg(unix)]
    trusted_root_alias: bool,
}

impl FileSecretStore {
    /// Selects an explicit Host-owned absolute location, validated on every operation.
    #[must_use]
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self {
            path: path.into(),
            #[cfg(unix)]
            trusted_root_alias: false,
        }
    }

    /// Selects a location whose first component beneath `/` is caller-trusted.
    /// Resolution occurs on each operation; the remaining components reject links.
    #[must_use]
    pub fn with_trusted_root_alias(path: impl Into<PathBuf>) -> Self {
        Self {
            path: path.into(),
            #[cfg(unix)]
            trusted_root_alias: true,
        }
    }

    fn operation_path(&self) -> Result<std::borrow::Cow<'_, Path>> {
        #[cfg(unix)]
        if self.trusted_root_alias {
            return platform::resolve_root_alias(&self.path).map(std::borrow::Cow::Owned);
        }
        Ok(std::borrow::Cow::Borrowed(&self.path))
    }
}

impl SecretStore for FileSecretStore {
    fn get(&self, reference: &CredentialRef) -> Result<Option<SecretValue>> {
        reference.validate()?;
        platform::get(&self.operation_path()?, reference)
    }
    fn set(&self, reference: &CredentialRef, secret: &SecretValue) -> Result<()> {
        reference.validate()?;
        platform::modify(&self.operation_path()?, reference, Some(secret)).map(|_| ())
    }
    fn unset(&self, reference: &CredentialRef) -> Result<bool> {
        reference.validate()?;
        platform::modify(&self.operation_path()?, reference, None)
    }
    fn location(&self) -> Option<String> {
        self.path
            .to_str()
            .filter(|path| {
                !path.is_empty() && path.len() <= 2048 && !path.chars().any(char::is_control)
            })
            .map(str::to_owned)
    }
}

#[cfg(not(unix))]
mod platform {
    use super::{CredentialRef, CredentialsError, Failure, Path, Result, SecretValue};
    pub(super) fn get(_: &Path, _: &CredentialRef) -> Result<Option<SecretValue>> {
        Err(CredentialsError::Store(Failure::Unsupported))
    }
    pub(super) fn modify(_: &Path, _: &CredentialRef, _: Option<&SecretValue>) -> Result<bool> {
        Err(CredentialsError::Store(Failure::Unsupported))
    }
}

#[cfg(unix)]
mod platform {
    use super::{CredentialRef, CredentialsError, Failure, Path, Result, SecretValue};
    use cap_std::fs::Dir;
    use rsi_files_native_fs::{
        create_absolute_directory_no_follow, open_absolute_directory_no_follow,
        open_relative_file_no_follow,
    };
    use rustix::fs::{AtFlags, Mode, OFlags, openat, renameat, unlinkat};
    use serde::{Deserialize, Serialize};
    use std::{
        collections::BTreeMap,
        fs::File,
        io::{Read, Write},
        os::unix::fs::MetadataExt,
        sync::atomic::{AtomicU64, Ordering},
        time::{Duration, Instant},
    };
    use zeroize::Zeroizing;

    #[cfg(test)]
    use super::{FileSecretStore, SecretStore};

    const MAX_BYTES: usize = 4 * 1024 * 1024;
    const MAX_ENTRIES: usize = 4096;
    static SEQUENCE: AtomicU64 = AtomicU64::new(0);
    type Entries = BTreeMap<CredentialRef, SecretValue>;

    fn failure(kind: Failure) -> CredentialsError {
        CredentialsError::Store(kind)
    }
    #[cfg_attr(test, track_caller)]
    fn io(error: &std::io::Error) -> CredentialsError {
        #[cfg(test)]
        eprintln!(
            "credential fixture I/O at {}: kind={:?}, errno={:?}",
            std::panic::Location::caller(),
            error.kind(),
            error.raw_os_error()
        );
        failure(if error.kind() == std::io::ErrorKind::PermissionDenied {
            Failure::Permissions
        } else if rsi_files_native_fs::is_link_rejection(error) {
            Failure::UnsafePath
        } else {
            Failure::Io
        })
    }
    fn validate_file(file: &File) -> Result<()> {
        let metadata = file.metadata().map_err(|error| io(&error))?;
        if !metadata.is_file() || metadata.nlink() != 1 {
            return Err(failure(Failure::UnsafePath));
        }
        if metadata.uid() != rustix::process::geteuid().as_raw()
            || metadata.mode() & 0o7777 != 0o600
        {
            return Err(failure(Failure::Permissions));
        }
        Ok(())
    }
    fn validate_path(path: &Path) -> Result<()> {
        if !path.is_absolute()
            || path
                .to_str()
                .is_none_or(|p| p.len() > 2048 || p.chars().any(char::is_control))
        {
            return Err(failure(Failure::UnsafePath));
        }
        Ok(())
    }
    pub(super) fn resolve_root_alias(path: &Path) -> Result<std::path::PathBuf> {
        validate_path(path)?;
        let resolved = rsi_files_native_fs::resolve_absolute_root_alias(path, true)
            .map_err(|error| io(&error))?;
        validate_path(&resolved)?;
        Ok(resolved)
    }
    fn directory(path: &Path, create: bool) -> Result<Option<Dir>> {
        validate_path(path)?;
        let parent = path.parent().ok_or_else(|| failure(Failure::UnsafePath))?;
        let dir = if create {
            create_absolute_directory_no_follow(parent)
        } else {
            open_absolute_directory_no_follow(parent)
        };
        let dir = match dir {
            Ok(dir) => dir,
            Err(error) if !create && error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(None);
            }
            Err(error) => return Err(io(&error)),
        };
        let metadata = dir
            .try_clone()
            .map_err(|error| io(&error))?
            .into_std_file()
            .metadata()
            .map_err(|error| io(&error))?;
        if metadata.uid() != rustix::process::geteuid().as_raw()
            || metadata.mode() & 0o7777 != 0o700
        {
            return Err(failure(Failure::Permissions));
        }
        Ok(Some(dir))
    }
    fn name(path: &Path) -> Result<&std::ffi::OsStr> {
        path.file_name().ok_or_else(|| failure(Failure::UnsafePath))
    }
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Document {
        version: u32,
        entries: Vec<Entry>,
    }
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Entry {
        reference: CredentialRef,
        #[serde(deserialize_with = "secret")]
        secret: SecretValue,
    }
    fn secret<'de, D: serde::Deserializer<'de>>(
        decoder: D,
    ) -> std::result::Result<SecretValue, D::Error> {
        SecretValue::new(String::deserialize(decoder)?).map_err(serde::de::Error::custom)
    }
    fn read(dir: &Dir, path: &Path) -> Result<Entries> {
        let file = match open_relative_file_no_follow(dir, Path::new(name(path)?)) {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Entries::new()),
            Err(error) => return Err(io(&error)),
        };
        validate_file(&file)?;
        let length = file.metadata().map_err(|error| io(&error))?.len();
        if length > MAX_BYTES as u64 {
            return Err(failure(Failure::TooLarge));
        }
        let mut bytes = Zeroizing::new(vec![
            0;
            usize::try_from(length)
                .map_err(|_| failure(Failure::TooLarge))?
        ]);
        let mut file = file;
        file.read_exact(&mut bytes).map_err(|error| io(&error))?;
        let mut extra = Zeroizing::new([0_u8; 1]);
        if file.read(&mut *extra).map_err(|error| io(&error))? != 0 {
            return Err(failure(Failure::Corrupt));
        }
        let document: Document =
            serde_json::from_slice(&bytes).map_err(|_| failure(Failure::Corrupt))?;
        if document.version != 1 {
            return Err(failure(Failure::Corrupt));
        }
        if document.entries.len() > MAX_ENTRIES {
            return Err(failure(Failure::TooLarge));
        }
        let mut entries = Entries::new();
        for entry in document.entries {
            if entries.insert(entry.reference, entry.secret).is_some() {
                return Err(failure(Failure::Corrupt));
            }
        }
        Ok(entries)
    }
    pub(super) fn get(path: &Path, reference: &CredentialRef) -> Result<Option<SecretValue>> {
        let Some(dir) = directory(path, false)? else {
            return Ok(None);
        };
        Ok(read(&dir, path)?.remove(reference))
    }
    fn lock(dir: &Dir, path: &Path) -> Result<File> {
        let lock_name = format!(
            ".{}.lock",
            name(path)?
                .to_str()
                .ok_or_else(|| failure(Failure::UnsafePath))?
        );
        let flags = OFlags::RDWR | OFlags::CLOEXEC | OFlags::NOFOLLOW | OFlags::NONBLOCK;
        let fd = match openat(
            dir,
            lock_name.as_str(),
            flags | OFlags::CREATE | OFlags::EXCL,
            Mode::RUSR | Mode::WUSR,
        ) {
            Ok(fd) => fd,
            Err(rustix::io::Errno::EXIST) => {
                openat(dir, lock_name.as_str(), flags, Mode::empty()).map_err(|e| io(&e.into()))?
            }
            Err(error) => return Err(io(&error.into())),
        };
        let file = File::from(fd);
        validate_file(&file)?;
        let deadline = Instant::now() + Duration::from_secs(1);
        loop {
            match file.try_lock() {
                Ok(()) => return Ok(file),
                Err(std::fs::TryLockError::WouldBlock) if Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(10));
                }
                Err(std::fs::TryLockError::WouldBlock) => {
                    return Err(failure(Failure::LockTimeout));
                }
                Err(std::fs::TryLockError::Error(error)) => return Err(io(&error)),
            }
        }
    }
    #[derive(Serialize)]
    struct EncodedDocument<'a> {
        version: u32,
        entries: Vec<EncodedEntry<'a>>,
    }
    #[derive(Serialize)]
    struct EncodedEntry<'a> {
        reference: &'a CredentialRef,
        secret: &'a str,
    }
    fn encode(entries: &Entries) -> Result<Zeroizing<Vec<u8>>> {
        if entries.len() > MAX_ENTRIES {
            return Err(failure(Failure::TooLarge));
        }
        let document = EncodedDocument {
            version: 1,
            entries: entries
                .iter()
                .map(|(reference, secret)| EncodedEntry {
                    reference,
                    secret: secret.expose_secret(),
                })
                .collect(),
        };
        let mut counter = ByteCount(0);
        serde_json::to_writer(&mut counter, &document).map_err(|_| failure(Failure::TooLarge))?;
        let mut bytes = Zeroizing::new(Vec::with_capacity(counter.0));
        let mut writer = BoundedWriter(&mut bytes);
        serde_json::to_writer(&mut writer, &document).map_err(|_| failure(Failure::TooLarge))?;
        Ok(bytes)
    }
    struct BoundedWriter<'a>(&'a mut Vec<u8>);
    struct ByteCount(usize);
    impl Write for ByteCount {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            if bytes.len() > MAX_BYTES.saturating_sub(self.0) {
                return Err(std::io::Error::other("credential document exceeds limit"));
            }
            self.0 += bytes.len();
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    impl Write for BoundedWriter<'_> {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            if bytes.len() > MAX_BYTES.saturating_sub(self.0.len()) {
                return Err(std::io::Error::other("credential document exceeds limit"));
            }
            self.0.extend_from_slice(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    pub(super) fn modify(
        path: &Path,
        reference: &CredentialRef,
        secret: Option<&SecretValue>,
    ) -> Result<bool> {
        let dir = directory(path, true)?.ok_or_else(|| failure(Failure::Io))?;
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
        publish(&dir, path, &bytes)?;
        Ok(existed)
    }
    fn publish(dir: &Dir, path: &Path, bytes: &[u8]) -> Result<()> {
        let (temporary, mut file) = temporary(dir)?;
        let result = (|| {
            #[cfg(test)]
            fail_at(1)?;
            file.write_all(bytes)
                .and_then(|()| file.sync_all())
                .map_err(|error| io(&error))?;
            #[cfg(test)]
            fail_at(2)?;
            renameat(dir, temporary.as_str(), dir, name(path)?).map_err(|e| io(&e.into()))?;
            #[cfg(test)]
            fail_at(3)?;
            dir.try_clone()
                .and_then(|d| d.into_std_file().sync_all())
                .map_err(|_| CredentialsError::OutcomeUnknown)
        })();
        // The uniquely created name is never reused within this process. After
        // successful rename it is absent; cleanup never touches the destination.
        let _ = unlinkat(dir, temporary.as_str(), AtFlags::empty());
        result
    }
    #[cfg(test)]
    std::thread_local! { static FAILURE_STAGE: std::cell::Cell<u8> = const { std::cell::Cell::new(0) }; }
    #[cfg(test)]
    fn fail_at(stage: u8) -> Result<()> {
        if FAILURE_STAGE.get() != stage {
            return Ok(());
        }
        Err(if stage == 3 {
            CredentialsError::OutcomeUnknown
        } else {
            failure(Failure::Io)
        })
    }
    fn temporary(dir: &Dir) -> Result<(String, File)> {
        for _ in 0..64 {
            let sequence = SEQUENCE
                .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |n| n.checked_add(1))
                .map_err(|_| failure(Failure::Io))?;
            let name = format!(".credentials.{}.{sequence}.tmp", std::process::id());
            match openat(
                dir,
                name.as_str(),
                OFlags::CREATE | OFlags::EXCL | OFlags::WRONLY | OFlags::CLOEXEC | OFlags::NOFOLLOW,
                Mode::RUSR | Mode::WUSR,
            ) {
                Ok(fd) => {
                    let file = File::from(fd);
                    if let Err(error) = validate_temporary(&file) {
                        let _ = unlinkat(dir, name.as_str(), AtFlags::empty());
                        return Err(error);
                    }
                    return Ok((name, file));
                }
                Err(rustix::io::Errno::EXIST) => {}
                Err(error) => return Err(io(&error.into())),
            }
        }
        Err(failure(Failure::Io))
    }
    fn validate_temporary(file: &File) -> Result<()> {
        #[cfg(test)]
        fail_at(4)?;
        validate_file(file)
    }
    #[cfg(test)]
    mod tests {
        use super::*;
        use std::{fs, process::Command};

        fn temp_root() -> tempfile::TempDir {
            tempfile::tempdir_in(fs::canonicalize(std::env::temp_dir()).unwrap()).unwrap()
        }
        fn reference(slot: &str) -> CredentialRef {
            CredentialRef::new("fixture.provider", slot).unwrap()
        }
        fn key(value: &str) -> SecretValue {
            SecretValue::new(value).unwrap()
        }
        #[test]
        fn acquiring_an_existing_writer_lock_preserves_its_identity_and_contents() {
            let root = temp_root();
            let path = root.path().join("credentials/credentials.json");
            let directory = directory(&path, true).unwrap().unwrap();
            let mut first = lock(&directory, &path).unwrap();
            first.write_all(b"retained lock").unwrap();
            let before = first.metadata().unwrap();
            drop(first);
            let mut next = lock(&directory, &path).unwrap();
            let after = next.metadata().unwrap();
            assert_eq!((before.dev(), before.ino()), (after.dev(), after.ino()));
            let mut contents = String::new();
            next.read_to_string(&mut contents).unwrap();
            assert_eq!(contents, "retained lock");
        }
        #[test]
        fn concurrent_processes_preserve_each_others_records() {
            let root = temp_root();
            let path = root.path().join("credentials/credentials.json");
            let mut children = Vec::new();
            for n in 0..8 {
                children.push(
                    Command::new(std::env::current_exe().unwrap())
                        .args([
                            "--exact",
                            "file::platform::tests::file_writer_child",
                            "--nocapture",
                        ])
                        .env("RSI_CREDENTIAL_TEST_FILE", &path)
                        .env("RSI_CREDENTIAL_TEST_WRITER", n.to_string())
                        .stdout(std::process::Stdio::null())
                        .spawn()
                        .unwrap(),
                );
            }
            // Reap every writer before asserting, so one failure cannot remove the
            // temporary root while other children are still reporting their result.
            let statuses: Vec<_> = children
                .iter_mut()
                .map(|child| child.wait().unwrap())
                .collect();
            assert!(
                statuses.iter().all(std::process::ExitStatus::success),
                "{statuses:?}"
            );
            let store = FileSecretStore::new(&path);
            for writer in 0..8 {
                for n in 0..8 {
                    assert_eq!(
                        store
                            .get(&reference(&format!("writer-{writer}-{n}")))
                            .unwrap()
                            .unwrap()
                            .expose_secret(),
                        "fixture"
                    );
                }
            }
        }

        #[test]
        fn file_writer_child() {
            let Ok(path) = std::env::var("RSI_CREDENTIAL_TEST_FILE") else {
                return;
            };
            let writer = std::env::var("RSI_CREDENTIAL_TEST_WRITER").unwrap();
            let store = FileSecretStore::new(path);
            for n in 0..8 {
                store
                    .set(&reference(&format!("writer-{writer}-{n}")), &key("fixture"))
                    .unwrap();
            }
        }

        #[test]
        fn publication_failures_preserve_old_data_or_report_unknown_after_replacement() {
            let root = temp_root();
            let path = root.path().join("credentials/credentials.json");
            let store = FileSecretStore::new(&path);
            let reference = CredentialRef::new("fixture", "primary").unwrap();
            store
                .set(&reference, &SecretValue::new("original").unwrap())
                .unwrap();
            for stage in [4, 1, 2, 3] {
                FAILURE_STAGE.set(stage);
                let result = store.set(&reference, &SecretValue::new("replacement").unwrap());
                FAILURE_STAGE.set(0);
                if stage == 3 {
                    assert_eq!(result, Err(CredentialsError::OutcomeUnknown));
                } else {
                    assert_eq!(result, Err(failure(Failure::Io)));
                }
                assert_eq!(
                    store.get(&reference).unwrap().unwrap().expose_secret(),
                    if stage == 3 {
                        "replacement"
                    } else {
                        "original"
                    }
                );
                assert_eq!(
                    std::fs::read_dir(path.parent().unwrap()).unwrap().count(),
                    2,
                    "temporary files are removed"
                );
            }
        }
    }
}
