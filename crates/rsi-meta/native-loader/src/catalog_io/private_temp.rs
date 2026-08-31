use super::CacheDirectoryClaim;
#[cfg(windows)]
use super::{open_bounded_regular_file, verify_exact_digest};
use crate::LoaderError;
#[cfg(all(unix, test))]
use std::cell::Cell;
use std::fs::File;
#[cfg(windows)]
use std::fs::OpenOptions;
#[cfg(unix)]
use std::io;
#[cfg(windows)]
use std::io::Write;
use std::path::Path;
#[cfg(all(unix, test))]
use std::path::PathBuf;
#[cfg(unix)]
use std::sync::atomic::{AtomicU64, Ordering};
#[cfg(windows)]
use tempfile::{Builder as TempBuilder, TempPath};

pub(crate) enum PersistOutcome {
    Published,
    AlreadyExists,
}

pub(crate) struct PrivateTemp {
    #[cfg(unix)]
    file: Option<File>,
    #[cfg(unix)]
    directory: File,
    #[cfg(unix)]
    name: Option<String>,
    #[cfg(all(unix, test))]
    path: PathBuf,
    #[cfg(all(unix, test))]
    fail_next_remove: bool,
    #[cfg(all(unix, test))]
    fail_next_reopen: Cell<Option<rustix::io::Errno>>,
    #[cfg(windows)]
    file: Option<File>,
    #[cfg(windows)]
    path: Option<TempPath>,
}

impl PrivateTemp {
    pub(crate) fn new_in(claim: &CacheDirectoryClaim) -> Result<Self, LoaderError> {
        #[cfg(unix)]
        {
            use rustix::fs::{Mode, OFlags};

            static NEXT_TEMP: AtomicU64 = AtomicU64::new(0);
            let directory = claim.directory.try_clone()?;
            for _ in 0..128 {
                let sequence = NEXT_TEMP.fetch_add(1, Ordering::Relaxed);
                let name = format!(".rsi-meta-{}-{sequence:016x}.tmp", std::process::id());
                match rustix::fs::openat(
                    &claim.directory,
                    &name,
                    OFlags::CREATE
                        | OFlags::EXCL
                        | OFlags::RDWR
                        | OFlags::CLOEXEC
                        | OFlags::NOFOLLOW,
                    Mode::RUSR | Mode::WUSR,
                ) {
                    Ok(file) => {
                        return Ok(Self {
                            file: Some(File::from(file)),
                            directory,
                            #[cfg(test)]
                            path: claim.path.join(&name),
                            #[cfg(test)]
                            fail_next_remove: false,
                            #[cfg(test)]
                            fail_next_reopen: Cell::new(None),
                            name: Some(name),
                        });
                    }
                    Err(rustix::io::Errno::EXIST) => {}
                    Err(error) => return Err(LoaderError::Io(error.into())),
                }
            }
            Err(LoaderError::Io(io::Error::new(
                io::ErrorKind::AlreadyExists,
                "catalog could not allocate a private temporary name",
            )))
        }
        #[cfg(windows)]
        {
            use std::os::windows::fs::OpenOptionsExt as _;

            const FILE_ATTRIBUTE_TEMPORARY: u32 = 0x0000_0100;
            const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
            let named = TempBuilder::new()
                .prefix(".rsi-meta-")
                .suffix(".dll")
                .make_in(&claim.path, |path| {
                    OpenOptions::new()
                        .create_new(true)
                        .read(true)
                        .write(true)
                        .share_mode(0)
                        .custom_flags(FILE_ATTRIBUTE_TEMPORARY | FILE_FLAG_OPEN_REPARSE_POINT)
                        .open(path)
                })?;
            let (file, path) = named.into_parts();
            Ok(Self {
                file: Some(file),
                path: Some(path),
            })
        }
    }

    pub(crate) fn file(&self) -> &File {
        self.file.as_ref().expect("private temp file present")
    }

    pub(crate) fn file_mut(&mut self) -> &mut File {
        self.file.as_mut().expect("private temp file present")
    }

    #[cfg(any(test, windows))]
    pub(crate) fn path(&self) -> &Path {
        #[cfg(all(unix, test))]
        {
            &self.path
        }
        #[cfg(windows)]
        {
            self.path.as_deref().expect("private temp path present")
        }
    }

    pub(crate) fn reopen(&self) -> Result<File, LoaderError> {
        #[cfg(unix)]
        {
            let name = self.name.as_ref().ok_or(LoaderError::CachePoisoned)?;
            let reopened = match self.open_name(name) {
                Ok(reopened) => reopened,
                Err(error) => {
                    return match self.name_is_owned(name) {
                        Ok(true) => Err(LoaderError::Io(error.into())),
                        Ok(false) | Err(_) => Err(LoaderError::CachePoisoned),
                    };
                }
            };
            let open_stat =
                rustix::fs::fstat(self.file()).map_err(|_| LoaderError::CachePoisoned)?;
            let reopened_stat =
                rustix::fs::fstat(&reopened).map_err(|_| LoaderError::CachePoisoned)?;
            if open_stat.st_dev != reopened_stat.st_dev || open_stat.st_ino != reopened_stat.st_ino
            {
                return Err(LoaderError::CachePoisoned);
            }
            Ok(reopened)
        }
        #[cfg(windows)]
        {
            open_bounded_regular_file(self.path()).map(|(file, _)| file)
        }
    }

    #[cfg(unix)]
    fn open_name(&self, name: &str) -> Result<File, rustix::io::Errno> {
        use rustix::fs::{Mode, OFlags};

        #[cfg(test)]
        if let Some(error) = self.fail_next_reopen.take() {
            return Err(error);
        }
        rustix::fs::openat(
            &self.directory,
            name,
            OFlags::RDONLY | OFlags::CLOEXEC | OFlags::NONBLOCK | OFlags::NOFOLLOW,
            Mode::empty(),
        )
        .map(File::from)
    }

    #[cfg(all(unix, not(any(target_os = "espidf", target_os = "redox"))))]
    fn name_is_owned(&self, name: &str) -> Result<bool, rustix::io::Errno> {
        let open_stat = rustix::fs::fstat(self.file())?;
        let named_stat =
            rustix::fs::statat(&self.directory, name, rustix::fs::AtFlags::SYMLINK_NOFOLLOW)?;
        Ok(open_stat.st_dev == named_stat.st_dev && open_stat.st_ino == named_stat.st_ino)
    }

    #[cfg(all(unix, any(target_os = "espidf", target_os = "redox")))]
    fn name_is_owned(&self, name: &str) -> Result<bool, rustix::io::Errno> {
        use rustix::fs::{Mode, OFlags};

        let reopened = rustix::fs::openat(
            &self.directory,
            name,
            OFlags::RDONLY | OFlags::CLOEXEC | OFlags::NONBLOCK | OFlags::NOFOLLOW,
            Mode::empty(),
        )?;
        let open_stat = rustix::fs::fstat(self.file())?;
        let reopened_stat = rustix::fs::fstat(&reopened)?;
        Ok(open_stat.st_dev == reopened_stat.st_dev && open_stat.st_ino == reopened_stat.st_ino)
    }

    #[cfg(unix)]
    fn remove_name(&mut self) -> Result<(), LoaderError> {
        let Some(name) = self.name.clone() else {
            return Ok(());
        };
        match self.name_is_owned(&name) {
            Ok(true) => {}
            Ok(false) | Err(rustix::io::Errno::NOENT) => {
                self.name.take();
                return Err(LoaderError::CachePoisoned);
            }
            Err(_) => return Err(LoaderError::CachePoisoned),
        }
        #[cfg(test)]
        if std::mem::take(&mut self.fail_next_remove) {
            return Err(LoaderError::CachePoisoned);
        }
        match rustix::fs::unlinkat(&self.directory, &name, rustix::fs::AtFlags::empty()) {
            Ok(()) => {
                self.name.take();
                Ok(())
            }
            Err(rustix::io::Errno::NOENT) => {
                self.name.take();
                Err(LoaderError::CachePoisoned)
            }
            Err(_) => Err(LoaderError::CachePoisoned),
        }
    }

    #[cfg(all(unix, not(target_os = "redox")))]
    fn persist_via_link_noclobber(
        &mut self,
        name: &str,
        target_name: &std::ffi::OsStr,
    ) -> Result<PersistOutcome, LoaderError> {
        match rustix::fs::linkat(
            &self.directory,
            name,
            &self.directory,
            target_name,
            rustix::fs::AtFlags::empty(),
        ) {
            Ok(()) => {
                if self.remove_name().is_err() {
                    let _ = rustix::fs::unlinkat(
                        &self.directory,
                        target_name,
                        rustix::fs::AtFlags::empty(),
                    );
                    return Err(LoaderError::CachePoisoned);
                }
                Ok(PersistOutcome::Published)
            }
            Err(rustix::io::Errno::EXIST) => {
                self.remove_name()?;
                Ok(PersistOutcome::AlreadyExists)
            }
            Err(error) => {
                self.remove_name()?;
                Err(LoaderError::Io(error.into()))
            }
        }
    }

    #[cfg(unix)]
    pub(crate) fn seal_for_read(self, _expected_bytes: u64, _digest: &str) -> Self {
        self
    }

    #[cfg(windows)]
    pub(crate) fn seal_for_read(
        mut self,
        expected_bytes: u64,
        digest: &str,
    ) -> Result<Self, LoaderError> {
        self.file_mut().flush()?;
        drop(self.file.take().expect("private temp file present"));
        let (mut file, length) = open_bounded_regular_file(self.path())?;
        if length != expected_bytes {
            return Err(LoaderError::StagedArtifactChanged);
        }
        verify_exact_digest(&mut file, expected_bytes, digest)?;
        self.file = Some(file);
        Ok(self)
    }

    pub(crate) fn persist_noclobber(
        mut self,
        target: &Path,
    ) -> Result<PersistOutcome, LoaderError> {
        #[cfg(unix)]
        {
            if let Err(error) = self.reopen() {
                if matches!(error, LoaderError::CachePoisoned) {
                    self.name.take();
                    return Err(error);
                }
                self.remove_name()?;
                return Err(error);
            }
            let name = self
                .name
                .as_ref()
                .expect("private temp name present")
                .clone();
            let target_name = target.file_name().ok_or_else(|| {
                LoaderError::Io(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    format!("cache target has no file name: {}", target.display()),
                ))
            })?;
            #[cfg(any(
                target_os = "android",
                target_os = "linux",
                target_os = "macos",
                target_os = "ios",
                target_os = "tvos",
                target_os = "visionos",
                target_os = "watchos",
                target_os = "redox"
            ))]
            {
                use rustix::fs::{RenameFlags, renameat_with};

                match renameat_with(
                    &self.directory,
                    &name,
                    &self.directory,
                    target_name,
                    RenameFlags::NOREPLACE,
                ) {
                    Ok(()) => {
                        self.name.take();
                        return Ok(PersistOutcome::Published);
                    }
                    Err(rustix::io::Errno::EXIST) => {
                        self.remove_name()?;
                        return Ok(PersistOutcome::AlreadyExists);
                    }
                    Err(rustix::io::Errno::NOSYS | rustix::io::Errno::INVAL) => {}
                    Err(error) => {
                        self.remove_name()?;
                        return Err(LoaderError::Io(error.into()));
                    }
                }
            }
            #[cfg(not(target_os = "redox"))]
            {
                self.persist_via_link_noclobber(&name, target_name)
            }
            #[cfg(target_os = "redox")]
            {
                self.remove_name()?;
                Err(LoaderError::Io(io::Error::new(
                    io::ErrorKind::Unsupported,
                    "relative no-clobber publication is unsupported",
                )))
            }
        }
        #[cfg(windows)]
        {
            drop(self.file.take().expect("private temp file present"));
            let path = self.path.take().expect("private temp path present");
            match path.persist_noclobber(target) {
                Ok(()) => Ok(PersistOutcome::Published),
                Err(error) if error.error.kind() == std::io::ErrorKind::AlreadyExists => {
                    Ok(PersistOutcome::AlreadyExists)
                }
                Err(error) => Err(LoaderError::Io(error.error)),
            }
        }
    }
}

#[cfg(unix)]
impl Drop for PrivateTemp {
    fn drop(&mut self) {
        if let Some(name) = self.name.take()
            && self.name_is_owned(&name).unwrap_or(false)
        {
            let _ = rustix::fs::unlinkat(&self.directory, name, rustix::fs::AtFlags::empty());
        }
        drop(self.file.take());
    }
}

#[cfg(windows)]
impl Drop for PrivateTemp {
    fn drop(&mut self) {
        drop(self.file.take());
        drop(self.path.take());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(unix)]
    use crate::catalog_io::cache_path;
    use std::fs;
    #[cfg(windows)]
    use std::fs::OpenOptions;
    use std::io::{Read, Write};

    #[cfg(unix)]
    #[test]
    fn private_temp_uses_the_claimed_directory_after_path_replacement() {
        let parent = tempfile::tempdir().unwrap();
        let cache = parent.path().join("cache");
        fs::create_dir(&cache).unwrap();
        let claim = CacheDirectoryClaim::capture(&cache).unwrap();
        let claimed = parent.path().join("claimed");
        fs::rename(&cache, &claimed).unwrap();
        fs::create_dir(&cache).unwrap();

        let temporary = PrivateTemp::new_in(&claim).unwrap();
        let name = temporary.path().file_name().unwrap();
        assert!(claim.open_regular(name.to_str().unwrap()).is_ok());
        assert!(claimed.join(name).is_file());
        assert!(!cache.join(name).exists());
    }

    #[cfg(unix)]
    #[test]
    fn cache_publication_uses_the_claimed_directory_after_path_replacement() {
        let parent = tempfile::tempdir().unwrap();
        let cache = parent.path().join("cache");
        fs::create_dir(&cache).unwrap();
        let claim = CacheDirectoryClaim::capture(&cache).unwrap();
        let digest = "d".repeat(64);
        let mut temporary = PrivateTemp::new_in(&claim).unwrap();
        temporary.file_mut().write_all(b"claimed").unwrap();

        let claimed = parent.path().join("claimed");
        fs::rename(&cache, &claimed).unwrap();
        fs::create_dir(&cache).unwrap();
        let replacement_target = cache_path(&cache, &digest);
        fs::write(&replacement_target, b"replacement").unwrap();

        assert!(matches!(
            temporary.persist_noclobber(&replacement_target).unwrap(),
            PersistOutcome::Published
        ));
        let (mut published, _) = claim.open_cache(&digest).unwrap();
        let mut bytes = Vec::new();
        published.read_to_end(&mut bytes).unwrap();
        assert_eq!(bytes, b"claimed");
        assert_eq!(fs::read(replacement_target).unwrap(), b"replacement");
        assert_eq!(fs::read(cache_path(&claimed, &digest)).unwrap(), b"claimed");
    }

    #[cfg(unix)]
    #[test]
    fn private_temp_rejects_path_replacement_before_publication() {
        let directory = tempfile::tempdir().unwrap();
        let claim = CacheDirectoryClaim::capture(directory.path()).unwrap();
        let mut temporary = PrivateTemp::new_in(&claim).unwrap();
        temporary.file_mut().write_all(b"verified bytes").unwrap();
        let temporary_path = temporary.path().to_path_buf();
        fs::remove_file(&temporary_path).unwrap();
        fs::write(&temporary_path, b"replacement bytes").unwrap();
        let target = directory.path().join("published.native");

        assert!(matches!(
            temporary.persist_noclobber(&target),
            Err(LoaderError::CachePoisoned)
        ));
        assert!(!target.exists());
        assert_eq!(fs::read(temporary_path).unwrap(), b"replacement bytes");
    }

    #[cfg(unix)]
    #[test]
    fn ordinary_reopen_failure_removes_the_owned_temporary_name() {
        let directory = tempfile::tempdir().unwrap();
        let claim = CacheDirectoryClaim::capture(directory.path()).unwrap();
        let mut temporary = PrivateTemp::new_in(&claim).unwrap();
        temporary.file_mut().write_all(b"verified bytes").unwrap();
        let temporary_path = temporary.path().to_path_buf();
        let target = directory.path().join("published.native");
        temporary
            .fail_next_reopen
            .set(Some(rustix::io::Errno::MFILE));

        assert!(matches!(
            temporary.persist_noclobber(&target),
            Err(LoaderError::Io(error)) if error.raw_os_error() == Some(libc::EMFILE)
        ));
        assert!(!temporary_path.exists());
        assert!(!target.exists());
    }

    #[cfg(all(unix, not(target_os = "redox")))]
    #[test]
    fn link_publication_rolls_back_when_the_temporary_name_cannot_be_removed() {
        let directory = tempfile::tempdir().unwrap();
        let claim = CacheDirectoryClaim::capture(directory.path()).unwrap();
        let mut temporary = PrivateTemp::new_in(&claim).unwrap();
        temporary.file_mut().write_all(b"verified bytes").unwrap();
        let temporary_path = temporary.path().to_path_buf();
        let temporary_name = temporary_path
            .file_name()
            .unwrap()
            .to_str()
            .unwrap()
            .to_owned();
        let target = directory.path().join("published.native");
        temporary.fail_next_remove = true;

        assert!(matches!(
            temporary.persist_via_link_noclobber(&temporary_name, target.file_name().unwrap()),
            Err(LoaderError::CachePoisoned)
        ));
        assert!(!target.exists());
        assert!(temporary_path.exists());

        drop(temporary);
        assert!(!temporary_path.exists());
    }

    #[cfg(windows)]
    #[test]
    fn sealed_windows_staging_denies_write_and_delete_until_drop() {
        use sha2::{Digest as _, Sha256};

        let directory = tempfile::tempdir().unwrap();
        let claim = CacheDirectoryClaim::capture(directory.path()).unwrap();
        let mut temporary = PrivateTemp::new_in(&claim).unwrap();
        temporary.file_mut().write_all(b"sealed").unwrap();
        let path = temporary.path().to_path_buf();
        assert!(OpenOptions::new().write(true).open(&path).is_err());

        let digest = hex::encode(Sha256::digest(b"sealed"));
        let temporary = temporary.seal_for_read(6, &digest).unwrap();
        assert!(OpenOptions::new().write(true).open(&path).is_err());
        assert!(fs::remove_file(&path).is_err());
        let mut reopened = temporary.reopen().unwrap();
        let mut contents = Vec::new();
        reopened.read_to_end(&mut contents).unwrap();
        assert_eq!(contents, b"sealed");

        drop(reopened);
        drop(temporary);
        assert!(!path.exists());
    }
}
