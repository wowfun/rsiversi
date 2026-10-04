use cap_std::fs::Dir;
use rustix::fs::{FlockOperation, Mode, OFlags};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeSet,
    fs::File,
    io::{Read, Seek, SeekFrom, Write},
    os::fd::AsRawFd,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

const MAXIMUM_ARTIFACT: u64 = 128 * 1024 * 1024;
const MAXIMUM_ARTIFACTS: usize = 32;
const MAXIMUM_STATE: u64 = 4096;
const WRITER: &str = "writer.lock";
const STATE: &str = "state.json";
const STAGE: &str = ".artifact.new";
const STATE_STAGE: &str = ".state.new";

/// Cache failures distinguish contention and capacity from malformed durable data.
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum CacheError {
    /// The root or entry is not private, native, executable and exclusively owned.
    #[error("SSH artifact cache root or entry is unsafe")]
    Unsafe,
    /// A fixed input or durable metadata bound was violated.
    #[error("SSH artifact cache input is invalid")]
    Invalid,
    /// Another publisher or collector owns the writer lock.
    #[error("SSH artifact cache writer is busy")]
    Busy,
    /// Writer admission did not complete before its absolute deadline.
    #[error("SSH artifact cache contention deadline exceeded")]
    ContentionTimeout,
    /// The artifact bytes or retained-version count exceed their fixed bound.
    #[error("SSH artifact cache capacity exceeded")]
    Capacity,
    /// The complete bytes differ from the expected immutable digest.
    #[error("SSH helper artifact digest differs")]
    Digest,
    /// A bounded native file operation failed.
    #[error("SSH artifact cache I/O failed")]
    Io,
}
type Result<T> = std::result::Result<T, CacheError>;

/// Admission policy for the exclusive writer only, never for replaying publication.
#[derive(Clone, Copy, Debug)]
pub enum WriterLockPolicy {
    /// Refuse contention immediately.
    Immediate,
    /// Wait only until this fixed monotonic deadline.
    WaitUntil(Instant),
}

#[derive(Debug, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Publications {
    newest: Vec<String>,
}
impl Publications {
    fn decode(bytes: &[u8]) -> Result<Self> {
        if bytes.len() as u64 > MAXIMUM_STATE {
            return Err(CacheError::Capacity);
        }
        let value: Self = serde_json::from_slice(bytes).map_err(|_| CacheError::Invalid)?;
        let unique = value.newest.iter().collect::<BTreeSet<_>>();
        if value.newest.len() > MAXIMUM_ARTIFACTS
            || unique.len() != value.newest.len()
            || value.newest.iter().any(|name| !is_digest(name))
        {
            return Err(CacheError::Invalid);
        }
        Ok(value)
    }
}

/// Private target cache handle. A cache handle by itself does not retain an artifact.
#[derive(Debug)]
pub struct ArtifactCache {
    #[cfg(test)]
    verification_pause: Option<std::sync::Arc<VerificationPause>>,
    directory: Dir,
    path: PathBuf,
}
#[cfg(test)]
#[derive(Debug)]
struct VerificationPause {
    entered: std::sync::mpsc::Sender<()>,
    resume: std::sync::Mutex<std::sync::mpsc::Receiver<()>>,
}
/// Shared kernel lease retaining one verified immutable helper inode.
#[derive(Debug)]
pub struct ArtifactLease {
    file: LockedFile,
    path: PathBuf,
    digest: [u8; 32],
}
#[derive(Debug)]
struct LockedFile(File);
impl Drop for LockedFile {
    fn drop(&mut self) {
        // An unrelated concurrent fork can temporarily inherit a CLOEXEC file
        // description. Release this owner's lock explicitly at its actual end.
        let _ = rustix::fs::flock(&self.0, FlockOperation::Unlock);
    }
}
impl ArtifactLease {
    /// Returns the cache location for diagnostics, not executable authority.
    pub fn path(&self) -> &Path {
        &self.path
    }
    /// Returns an executable path bound to the verified inode while this lease lives.
    pub fn executable(&self) -> PathBuf {
        PathBuf::from(format!(
            "/proc/{}/fd/{}",
            std::process::id(),
            self.file.0.as_raw_fd()
        ))
    }
    /// Returns the exact bytes verified when acquiring this lease.
    pub fn digest(&self) -> &[u8; 32] {
        &self.digest
    }
}

impl ArtifactCache {
    /// Opens one Service namespace under an existing private executable runtime root.
    pub fn open(runtime: &Path, service: &str) -> Result<Self> {
        if !super::valid_service_namespace(service) {
            return Err(CacheError::Invalid);
        }
        let runtime_handle = rsi_files_native_fs::open_absolute_directory_no_follow(runtime)
            .map_err(|_| CacheError::Unsafe)?;
        validate_directory(&runtime_handle)?;
        let family = private_child(&runtime_handle, "rsi-ssh")?;
        let directory = private_child(&family, service)?;
        Ok(Self {
            #[cfg(test)]
            verification_pause: None,
            directory,
            path: runtime.join("rsi-ssh").join(service),
        })
    }
    /// Returns a verified cached inode or stages incoming bytes for install/repair.
    /// Healthy cache hits leave input untouched; successful reuse refreshes publication order.
    /// A failed publication may leave a bounded staging/orphan file for the next writer.
    pub fn publish(
        &self,
        expected: [u8; 32],
        input: &mut impl Read,
        policy: WriterLockPolicy,
    ) -> Result<ArtifactLease> {
        let mut writer = self.writer(policy)?;
        self.clean_staging()?;
        let mut state = self.load()?;
        let name = hex::encode(expected);
        let names = self.names()?;
        let existing = names.contains(&name);
        if !existing {
            self.stage(expected, input)?;
            self.collect_locked(&mut state)?;
            if self.names()?.len() >= MAXIMUM_ARTIFACTS {
                return Err(CacheError::Capacity);
            }
            self.install_staged(expected, rustix::fs::RenameFlags::NOREPLACE)?;
        }
        let lease = self.open_lease(expected)?;
        let lease = if existing {
            drop(writer);
            let lease = match self.verify_lease(lease) {
                Err(CacheError::Digest) => return self.repair(expected, input, policy),
                result => result?,
            };
            writer = self.writer(policy)?;
            self.clean_staging()?;
            state = self.load()?;
            lease
        } else {
            lease
        };
        let previous = state.newest.clone();
        state.newest.retain(|entry| entry != &name);
        state.newest.push(name);
        self.collect_locked(&mut state)?;
        if state.newest != previous {
            self.save(&state)?;
        }
        drop(writer);
        if existing {
            Ok(lease)
        } else {
            self.verify_lease(lease)
        }
    }

    fn repair(
        &self,
        expected: [u8; 32],
        input: &mut impl Read,
        policy: WriterLockPolicy,
    ) -> Result<ArtifactLease> {
        let writer = self.writer(policy)?;
        self.clean_staging()?;
        let mut state = self.load()?;
        let name = hex::encode(expected);
        let names = self.names()?;
        let inactive = if names.contains(&name) {
            let file = self.open_file(&name, OFlags::RDONLY, 0)?;
            validate_file(&file, 0o500, MAXIMUM_ARTIFACT)?;
            lock(&file, FlockOperation::NonBlockingLockExclusive)?;
            Some(LockedFile(file))
        } else {
            None
        };
        self.stage(expected, input)?;
        self.collect_locked(&mut state)?;
        if inactive.is_none() && self.names()?.len() >= MAXIMUM_ARTIFACTS {
            return Err(CacheError::Capacity);
        }
        self.install_staged(
            expected,
            if inactive.is_some() {
                rustix::fs::RenameFlags::empty()
            } else {
                rustix::fs::RenameFlags::NOREPLACE
            },
        )?;
        let lease = self.open_lease(expected)?;
        state.newest.retain(|entry| entry != &name);
        state.newest.push(name);
        self.collect_locked(&mut state)?;
        self.save(&state)?;
        drop(inactive);
        drop(writer);
        self.verify_lease(lease)
    }

    fn install_staged(&self, expected: [u8; 32], flags: rustix::fs::RenameFlags) -> Result<()> {
        rustix::fs::renameat_with(
            &self.directory,
            STAGE,
            &self.directory,
            hex::encode(expected),
            flags,
        )
        .map_err(|_| CacheError::Io)?;
        rustix::fs::fsync(&self.directory).map_err(|_| CacheError::Io)
    }
    /// Acquires a separate helper lease while the launcher still holds its lease.
    pub fn acquire(&self, digest: [u8; 32], policy: WriterLockPolicy) -> Result<ArtifactLease> {
        let writer = self.writer(policy)?;
        let lease = self.open_lease(digest)?;
        drop(writer);
        self.verify_lease(lease)
    }
    /// Collects inactive old versions while retaining live leases and the newest two.
    pub fn collect(&self, policy: WriterLockPolicy) -> Result<()> {
        let _writer = self.writer(policy)?;
        self.clean_staging()?;
        let mut state = self.load()?;
        if self.collect_locked(&mut state)? {
            self.save(&state)?;
        }
        Ok(())
    }
    fn writer(&self, policy: WriterLockPolicy) -> Result<LockedFile> {
        self.check_path()?;
        let file = self.open_file(WRITER, OFlags::RDWR | OFlags::CREATE, 0o600)?;
        validate_file(&file, 0o600, 0)?;
        let mut delay = Duration::from_millis(1);
        loop {
            if matches!(policy, WriterLockPolicy::WaitUntil(deadline) if Instant::now() >= deadline)
            {
                return Err(CacheError::ContentionTimeout);
            }
            match lock(&file, FlockOperation::NonBlockingLockExclusive) {
                Ok(()) => break,
                Err(CacheError::Busy) => match policy {
                    WriterLockPolicy::Immediate => return Err(CacheError::Busy),
                    WriterLockPolicy::WaitUntil(deadline) => {
                        let remaining = deadline.saturating_duration_since(Instant::now());
                        if remaining.is_zero() {
                            return Err(CacheError::ContentionTimeout);
                        }
                        std::thread::sleep(delay.min(remaining));
                        delay = (delay * 2).min(Duration::from_millis(50));
                    }
                },
                Err(error) => return Err(error),
            }
        }
        Ok(LockedFile(file))
    }
    fn check_path(&self) -> Result<()> {
        let current = rsi_files_native_fs::open_absolute_directory_no_follow(&self.path)
            .map_err(|_| CacheError::Unsafe)?;
        validate_directory(&current)?;
        let current = rustix::fs::fstat(&current).map_err(|_| CacheError::Io)?;
        let pinned = rustix::fs::fstat(&self.directory).map_err(|_| CacheError::Io)?;
        if (current.st_dev, current.st_ino) != (pinned.st_dev, pinned.st_ino) {
            return Err(CacheError::Unsafe);
        }
        Ok(())
    }
    fn open_file(&self, name: &str, flags: OFlags, mode: u32) -> Result<File> {
        rustix::fs::openat(
            &self.directory,
            name,
            flags | OFlags::CLOEXEC | OFlags::NOFOLLOW | OFlags::NONBLOCK,
            Mode::from_raw_mode(mode),
        )
        .map(File::from)
        .map_err(|_| CacheError::Io)
    }
    fn clean_staging(&self) -> Result<()> {
        for name in [STAGE, STATE_STAGE] {
            match rustix::fs::unlinkat(&self.directory, name, rustix::fs::AtFlags::empty()) {
                Ok(()) | Err(rustix::io::Errno::NOENT) => {}
                Err(_) => return Err(CacheError::Io),
            }
        }
        Ok(())
    }
    fn stage(&self, expected: [u8; 32], input: &mut impl Read) -> Result<()> {
        let mut file =
            self.open_file(STAGE, OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL, 0o600)?;
        let mut hash = Sha256::new();
        let mut total = 0_u64;
        let mut buffer = vec![0; 64 * 1024].into_boxed_slice();
        loop {
            let bytes = input.read(&mut buffer).map_err(|_| CacheError::Io)?;
            if bytes == 0 {
                break;
            }
            total += bytes as u64;
            if total > MAXIMUM_ARTIFACT {
                return Err(CacheError::Capacity);
            }
            hash.update(&buffer[..bytes]);
            file.write_all(&buffer[..bytes])
                .map_err(|_| CacheError::Io)?;
        }
        if total == 0 || <[u8; 32]>::from(hash.finalize()) != expected {
            return Err(CacheError::Digest);
        }
        rustix::fs::fchmod(&file, Mode::RUSR | Mode::XUSR).map_err(|_| CacheError::Io)?;
        file.sync_all().map_err(|_| CacheError::Io)
    }
    fn open_lease(&self, digest: [u8; 32]) -> Result<ArtifactLease> {
        let name = hex::encode(digest);
        let file = self.open_file(&name, OFlags::RDONLY, 0)?;
        validate_file(&file, 0o500, MAXIMUM_ARTIFACT)?;
        lock(&file, FlockOperation::NonBlockingLockShared)?;
        Ok(ArtifactLease {
            file: LockedFile(file),
            path: self.path.join(name),
            digest,
        })
    }
    fn verify_lease(&self, mut lease: ArtifactLease) -> Result<ArtifactLease> {
        #[cfg(test)]
        if let Some(pause) = &self.verification_pause {
            pause.entered.send(()).unwrap();
            pause
                .resume
                .lock()
                .unwrap()
                .recv_timeout(Duration::from_secs(10))
                .expect("verification gate was not released");
        }
        let mut hash = Sha256::new();
        let read = std::io::copy(
            &mut (&mut lease.file.0).take(MAXIMUM_ARTIFACT + 1),
            &mut hash,
        )
        .map_err(|_| CacheError::Io)?;
        if read == 0 || read > MAXIMUM_ARTIFACT || <[u8; 32]>::from(hash.finalize()) != lease.digest
        {
            return Err(CacheError::Digest);
        }
        lease
            .file
            .0
            .seek(SeekFrom::Start(0))
            .map_err(|_| CacheError::Io)?;
        self.check_path()?;
        Ok(lease)
    }
    fn names(&self) -> Result<BTreeSet<String>> {
        let mut names = BTreeSet::new();
        let mut count = 0;
        for entry in self.directory.entries().map_err(|_| CacheError::Io)? {
            count += 1;
            if count > MAXIMUM_ARTIFACTS + 4 {
                return Err(CacheError::Capacity);
            }
            let name = entry
                .map_err(|_| CacheError::Io)?
                .file_name()
                .into_string()
                .map_err(|_| CacheError::Invalid)?;
            if is_digest(&name) {
                names.insert(name);
            } else if ![WRITER, STATE, STAGE, STATE_STAGE].contains(&name.as_str()) {
                return Err(CacheError::Invalid);
            }
        }
        if names.len() > MAXIMUM_ARTIFACTS {
            return Err(CacheError::Capacity);
        }
        Ok(names)
    }
    fn load(&self) -> Result<Publications> {
        let file = match rustix::fs::openat(
            &self.directory,
            STATE,
            OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC | OFlags::NONBLOCK,
            Mode::empty(),
        ) {
            Ok(file) => File::from(file),
            Err(rustix::io::Errno::NOENT) => return Ok(Publications::default()),
            Err(_) => return Err(CacheError::Io),
        };
        validate_file(&file, 0o600, MAXIMUM_STATE)?;
        let mut bytes = Vec::new();
        file.take(MAXIMUM_STATE + 1)
            .read_to_end(&mut bytes)
            .map_err(|_| CacheError::Io)?;
        Publications::decode(&bytes)
    }
    fn save(&self, state: &Publications) -> Result<()> {
        let bytes = serde_json::to_vec(state).map_err(|_| CacheError::Invalid)?;
        if bytes.len() as u64 > MAXIMUM_STATE {
            return Err(CacheError::Capacity);
        }
        let mut file = self.open_file(
            STATE_STAGE,
            OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL,
            0o600,
        )?;
        file.write_all(&bytes).map_err(|_| CacheError::Io)?;
        file.sync_all().map_err(|_| CacheError::Io)?;
        drop(file);
        rustix::fs::renameat(&self.directory, STATE_STAGE, &self.directory, STATE)
            .map_err(|_| CacheError::Io)?;
        rustix::fs::fsync(&self.directory).map_err(|_| CacheError::Io)
    }
    fn collect_locked(&self, state: &mut Publications) -> Result<bool> {
        let previous = state.newest.clone();
        let mut directory_changed = false;
        let names = self.names()?;
        state.newest.retain(|name| names.contains(name));
        let mut order = names
            .iter()
            .filter(|name| !state.newest.contains(name))
            .cloned()
            .collect::<Vec<_>>();
        order.append(&mut state.newest);
        state.newest = order;
        let keep = state
            .newest
            .iter()
            .rev()
            .take(2)
            .cloned()
            .collect::<BTreeSet<_>>();
        let mut retained = Vec::new();
        for name in &state.newest {
            let file = self.open_file(name, OFlags::RDONLY, 0)?;
            validate_file(&file, 0o500, MAXIMUM_ARTIFACT)?;
            if keep.contains(name) {
                retained.push(name.clone());
                continue;
            }
            match lock(&file, FlockOperation::NonBlockingLockExclusive) {
                Ok(()) => {
                    rustix::fs::unlinkat(&self.directory, name, rustix::fs::AtFlags::empty())
                        .map_err(|_| CacheError::Io)?;
                    directory_changed = true;
                }
                Err(CacheError::Busy) => retained.push(name.clone()),
                Err(error) => return Err(error),
            }
        }
        state.newest = retained;
        if directory_changed {
            rustix::fs::fsync(&self.directory).map_err(|_| CacheError::Io)?;
        }
        Ok(state.newest != previous)
    }
}

fn is_digest(name: &str) -> bool {
    name.len() == 64
        && name
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}
fn lock(file: &File, operation: FlockOperation) -> Result<()> {
    rustix::fs::flock(file, operation).map_err(|error| {
        if error == rustix::io::Errno::WOULDBLOCK {
            CacheError::Busy
        } else {
            CacheError::Io
        }
    })
}
fn validate_directory(directory: &Dir) -> Result<()> {
    let stat = rustix::fs::fstat(directory).map_err(|_| CacheError::Io)?;
    if stat.st_uid != rustix::process::getuid().as_raw() || stat.st_mode & 0o7777 != 0o700 {
        return Err(CacheError::Unsafe);
    }
    let filesystem = rustix::fs::fstatfs(directory).map_err(|_| CacheError::Io)?;
    if !matches!(
        i128::from(filesystem.f_type) & 0xffff_ffff,
        0x0102_1994 | 0xef53 | 0x5846_5342 | 0x9123_683e
    ) {
        return Err(CacheError::Unsafe);
    }
    let flags = rustix::fs::fstatvfs(directory)
        .map_err(|_| CacheError::Io)?
        .f_flag;
    if flags
        .intersects(rustix::fs::StatVfsMountFlags::NOEXEC | rustix::fs::StatVfsMountFlags::RDONLY)
    {
        return Err(CacheError::Unsafe);
    }
    Ok(())
}
fn private_child(parent: &Dir, name: &str) -> Result<Dir> {
    match rustix::fs::mkdirat(parent, name, Mode::RUSR | Mode::WUSR | Mode::XUSR) {
        Ok(()) | Err(rustix::io::Errno::EXIST) => {}
        Err(_) => return Err(CacheError::Io),
    }
    let directory = rsi_files_native_fs::open_relative_directory_no_follow(parent, Path::new(name))
        .map_err(|_| CacheError::Unsafe)?;
    validate_directory(&directory)?;
    Ok(directory)
}
fn validate_file(file: &File, mode: u32, maximum: u64) -> Result<()> {
    let stat = rustix::fs::fstat(file).map_err(|_| CacheError::Io)?;
    if rustix::fs::FileType::from_raw_mode(stat.st_mode) != rustix::fs::FileType::RegularFile
        || stat.st_uid != rustix::process::getuid().as_raw()
        || stat.st_nlink != 1
        || stat.st_mode & 0o7777 != mode
        || !u64::try_from(stat.st_size).is_ok_and(|size| size <= maximum)
    {
        return Err(CacheError::Unsafe);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn verification_pause() -> (
        std::sync::Arc<VerificationPause>,
        std::sync::mpsc::Receiver<()>,
        std::sync::mpsc::Sender<()>,
    ) {
        let (entered, observing) = std::sync::mpsc::channel();
        let (release, resume) = std::sync::mpsc::channel();
        (
            std::sync::Arc::new(VerificationPause {
                entered,
                resume: std::sync::Mutex::new(resume),
            }),
            observing,
            release,
        )
    }
    #[test]
    #[ignore = "requires native Linux flock"]
    fn finished_lock_ownership_is_not_extended_by_an_inherited_description() {
        for operation in [
            FlockOperation::NonBlockingLockExclusive,
            FlockOperation::NonBlockingLockShared,
        ] {
            let temporary = tempfile::tempfile().unwrap();
            let path = format!(
                "/proc/self/fd/{}",
                std::os::fd::AsRawFd::as_raw_fd(&temporary)
            );
            let open = || {
                std::fs::OpenOptions::new()
                    .read(true)
                    .write(true)
                    .open(&path)
                    .unwrap()
            };
            let owner = LockedFile(open());
            lock(&owner.0, operation).unwrap();
            let inherited = owner.0.try_clone().unwrap();
            assert_eq!(
                lock(&open(), FlockOperation::NonBlockingLockExclusive),
                Err(CacheError::Busy)
            );
            drop(owner);
            let next = LockedFile(open());
            lock(&next.0, FlockOperation::NonBlockingLockExclusive).unwrap();
            drop(inherited);
            assert_eq!(
                lock(&open(), FlockOperation::NonBlockingLockExclusive),
                Err(CacheError::Busy)
            );
        }
    }
    #[test]
    #[ignore = "requires native Linux flock and a local executable filesystem"]
    fn acquiring_hash_holds_only_artifact_lease_while_collection_progresses() {
        use std::os::unix::fs::PermissionsExt;
        let root = tempfile::Builder::new()
            .permissions(std::fs::Permissions::from_mode(0o700))
            .tempdir()
            .unwrap();
        let service = "9".repeat(32);
        let cache = ArtifactCache::open(root.path(), &service).unwrap();
        let bytes = b"#!/bin/sh\nexit 0\n";
        let digest = Sha256::digest(bytes).into();
        let lease = cache
            .publish(digest, &mut bytes.as_slice(), WriterLockPolicy::Immediate)
            .unwrap();
        let old_path = lease.path().to_owned();
        drop(lease);
        let (pause, entered, release) = verification_pause();
        let mut acquiring = ArtifactCache::open(root.path(), &service).unwrap();
        acquiring.verification_pause = Some(pause);
        let worker = std::thread::spawn(move || {
            acquiring
                .acquire(digest, WriterLockPolicy::Immediate)
                .unwrap()
        });
        entered
            .recv_timeout(Duration::from_secs(10))
            .expect("verification worker did not enter");
        for bytes in [b"#!/bin/sh\nexit 1\n", b"#!/bin/sh\nexit 2\n"] {
            let digest = Sha256::digest(bytes).into();
            drop(
                cache
                    .publish(digest, &mut bytes.as_slice(), WriterLockPolicy::Immediate)
                    .unwrap(),
            );
        }
        cache.collect(WriterLockPolicy::Immediate).unwrap();
        assert!(old_path.exists());
        release.send(()).unwrap();
        let lease = worker.join().unwrap();
        assert_eq!(*lease.digest(), digest);
        drop(lease);
        cache.collect(WriterLockPolicy::Immediate).unwrap();
        assert!(!old_path.exists());
    }
    #[test]
    #[ignore = "requires native Linux flock and a local executable filesystem"]
    fn publishing_hash_releases_writer_and_preserves_concurrent_publications() {
        use std::os::unix::fs::PermissionsExt;
        let root = tempfile::Builder::new()
            .permissions(std::fs::Permissions::from_mode(0o700))
            .tempdir()
            .unwrap();
        let service = "8".repeat(32);
        let cache = ArtifactCache::open(root.path(), &service).unwrap();
        let bytes = b"#!/bin/sh\nexit 0\n";
        let digest = Sha256::digest(bytes).into();
        let (pause, entered, release) = verification_pause();
        let mut publishing = ArtifactCache::open(root.path(), &service).unwrap();
        publishing.verification_pause = Some(pause);
        let worker = std::thread::spawn(move || {
            publishing.publish(digest, &mut bytes.as_slice(), WriterLockPolicy::Immediate)
        });
        entered
            .recv_timeout(Duration::from_secs(10))
            .expect("verification worker did not enter");
        let progress = (|| {
            for bytes in [b"#!/bin/sh\nexit 1\n", b"#!/bin/sh\nexit 2\n"] {
                let digest = Sha256::digest(bytes).into();
                drop(cache.publish(digest, &mut bytes.as_slice(), WriterLockPolicy::Immediate)?);
            }
            cache.collect(WriterLockPolicy::Immediate)
        })();
        let old_path = cache.path.join(hex::encode(digest));
        let retained = old_path.exists();
        release.send(()).unwrap();
        let lease = worker.join().unwrap().unwrap();
        assert_eq!(progress, Ok(()));
        assert!(retained);
        assert_eq!(*lease.digest(), digest);
        drop(lease);
        cache.collect(WriterLockPolicy::Immediate).unwrap();
        assert!(!old_path.exists());
        assert_eq!(cache.load().unwrap().newest.len(), 2);
    }
    #[test]
    #[ignore = "requires native Linux flock and a local executable filesystem"]
    fn concurrent_hashes_preserve_first_publication_order_through_collection() {
        use std::os::unix::fs::PermissionsExt;
        let root = tempfile::Builder::new()
            .permissions(std::fs::Permissions::from_mode(0o700))
            .tempdir()
            .unwrap();
        let service = "6".repeat(32);
        let cache = ArtifactCache::open(root.path(), &service).unwrap();
        let mut workers = Vec::new();
        let mut pauses = Vec::new();
        let mut expected = Vec::new();
        for index in 0..2 {
            let bytes = format!("#!/bin/sh\nexit {index}\n").into_bytes();
            let digest = Sha256::digest(&bytes).into();
            expected.push(hex::encode(digest));
            let (pause, entered, release) = verification_pause();
            let mut publishing = ArtifactCache::open(root.path(), &service).unwrap();
            publishing.verification_pause = Some(pause);
            workers.push(std::thread::spawn(move || {
                publishing.publish(digest, &mut bytes.as_slice(), WriterLockPolicy::Immediate)
            }));
            entered
                .recv_timeout(Duration::from_secs(10))
                .expect("verification worker did not enter");
            pauses.push(release);
        }
        let bytes = b"#!/bin/sh\nexit 2\n";
        let digest = Sha256::digest(bytes).into();
        expected.push(hex::encode(digest));
        let progress = cache.publish(digest, &mut bytes.as_slice(), WriterLockPolicy::Immediate);
        for (worker, release) in workers.into_iter().zip(pauses) {
            release.send(()).unwrap();
            drop(worker.join().unwrap().unwrap());
        }
        drop(progress.unwrap());
        assert_eq!(cache.load().unwrap().newest, expected);
        cache.collect(WriterLockPolicy::Immediate).unwrap();
        assert_eq!(cache.load().unwrap().newest, expected[1..]);
    }

    #[test]
    #[ignore = "requires native Linux flock and a local executable filesystem"]
    fn final_verification_rejects_corruption_after_publication_without_issuing_a_lease() {
        use std::os::unix::fs::PermissionsExt;
        let root = tempfile::Builder::new()
            .permissions(std::fs::Permissions::from_mode(0o700))
            .tempdir()
            .unwrap();
        let service = "7".repeat(32);
        let cache = ArtifactCache::open(root.path(), &service).unwrap();
        let bytes = b"#!/bin/sh\nexit 0\n";
        let digest = Sha256::digest(bytes).into();
        let (pause, entered, release) = verification_pause();
        let mut publishing = ArtifactCache::open(root.path(), &service).unwrap();
        publishing.verification_pause = Some(pause);
        let worker = std::thread::spawn(move || {
            publishing
                .publish(digest, &mut bytes.as_slice(), WriterLockPolicy::Immediate)
                .map(|_| ())
        });
        entered
            .recv_timeout(Duration::from_secs(10))
            .expect("verification worker did not enter");
        let path = cache.path.join(hex::encode(digest));
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        std::fs::write(&path, b"corrupt installed bytes").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o500)).unwrap();
        release.send(()).unwrap();
        assert_eq!(worker.join().unwrap(), Err(CacheError::Digest));
        assert_eq!(cache.load().unwrap().newest, [hex::encode(digest)]);
        assert!(matches!(
            cache.acquire(digest, WriterLockPolicy::Immediate),
            Err(CacheError::Digest)
        ));
        let repaired = cache
            .publish(digest, &mut bytes.as_slice(), WriterLockPolicy::Immediate)
            .unwrap();
        assert_eq!(std::fs::read(repaired.path()).unwrap(), bytes);
        assert_eq!(cache.load().unwrap().newest, [hex::encode(digest)]);
    }
    #[test]
    fn durable_publication_order_rejects_ambiguous_or_unbounded_input() {
        let name = "1".repeat(64);
        let valid = format!("{{\"newest\":[\"{name}\"]}}");
        assert_eq!(
            Publications::decode(valid.as_bytes()).unwrap().newest,
            std::slice::from_ref(&name)
        );
        for bytes in [
            format!("{{\"newest\":[\"{name}\",\"{name}\"]}}"),
            "{\"newest\":[\"../outside\"]}".into(),
            "{\"newest\":[],\"extra\":true}".into(),
            "{\"newest\":[],\"newest\":[]}".into(),
        ] {
            assert!(Publications::decode(bytes.as_bytes()).is_err());
        }
        assert!(
            Publications::decode(&vec![b' '; usize::try_from(MAXIMUM_STATE).unwrap() + 1]).is_err()
        );
        let names = (0_u8..33)
            .map(|index| hex::encode([index; 32]))
            .collect::<Vec<_>>();
        assert!(
            Publications::decode(&serde_json::to_vec(&Publications { newest: names }).unwrap())
                .is_err()
        );
    }
}
