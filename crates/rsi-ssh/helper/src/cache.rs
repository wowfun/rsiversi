use cap_std::fs::Dir;
use rustix::fs::{FlockOperation, Mode, OFlags};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeSet,
    fs::File,
    io::{Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
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
    directory: Dir,
    path: PathBuf,
}
/// Shared kernel lease retaining one verified immutable helper inode.
#[derive(Debug)]
pub struct ArtifactLease {
    _file: LockedFile,
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
    /// Returns the immutable pathname protected by this still-live lease.
    pub fn path(&self) -> &Path {
        &self.path
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
            directory,
            path: runtime.join("rsi-ssh").join(service),
        })
    }
    /// Publishes complete digest-verified bytes and returns a shared launcher lease.
    /// A failed publication may leave a bounded staging/orphan file for the next writer.
    pub fn publish(&self, expected: [u8; 32], input: &mut impl Read) -> Result<ArtifactLease> {
        let _writer = self.writer()?;
        self.clean_staging()?;
        let mut state = self.load()?;
        self.collect_locked(&mut state)?;
        let name = hex::encode(expected);
        let names = self.names()?;
        if !names.contains(&name) {
            if names.len() >= MAXIMUM_ARTIFACTS {
                return Err(CacheError::Capacity);
            }
            self.stage(expected, input)?;
            rustix::fs::renameat_with(
                &self.directory,
                STAGE,
                &self.directory,
                &name,
                rustix::fs::RenameFlags::NOREPLACE,
            )
            .map_err(|_| CacheError::Io)?;
            rustix::fs::fsync(&self.directory).map_err(|_| CacheError::Io)?;
        }
        let lease = self.lease_locked(expected)?;
        if !state.newest.contains(&name) {
            state.newest.push(name);
        }
        self.save(&state)?;
        self.collect_locked(&mut state)?;
        Ok(lease)
    }
    /// Acquires a separate helper lease while the launcher still holds its lease.
    pub fn acquire(&self, digest: [u8; 32]) -> Result<ArtifactLease> {
        let _writer = self.writer()?;
        self.lease_locked(digest)
    }
    /// Collects inactive old versions while retaining live leases and the newest two.
    pub fn collect(&self) -> Result<()> {
        let _writer = self.writer()?;
        self.clean_staging()?;
        self.collect_locked(&mut self.load()?)
    }
    fn writer(&self) -> Result<LockedFile> {
        self.check_path()?;
        let file = self.open_file(WRITER, OFlags::RDWR | OFlags::CREATE, 0o600)?;
        validate_file(&file, 0o600, 0)?;
        lock(&file, FlockOperation::NonBlockingLockExclusive)?;
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
    fn lease_locked(&self, digest: [u8; 32]) -> Result<ArtifactLease> {
        let name = hex::encode(digest);
        let file = self.open_file(&name, OFlags::RDONLY, 0)?;
        validate_file(&file, 0o500, MAXIMUM_ARTIFACT)?;
        lock(&file, FlockOperation::NonBlockingLockShared)?;
        let mut file = LockedFile(file);
        let mut hash = Sha256::new();
        let read = std::io::copy(&mut (&mut file.0).take(MAXIMUM_ARTIFACT + 1), &mut hash)
            .map_err(|_| CacheError::Io)?;
        if read == 0 || read > MAXIMUM_ARTIFACT || <[u8; 32]>::from(hash.finalize()) != digest {
            return Err(CacheError::Digest);
        }
        file.0
            .seek(SeekFrom::Start(0))
            .map_err(|_| CacheError::Io)?;
        Ok(ArtifactLease {
            _file: file,
            path: self.path.join(name),
            digest,
        })
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
    fn collect_locked(&self, state: &mut Publications) -> Result<()> {
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
                Ok(()) => rustix::fs::unlinkat(&self.directory, name, rustix::fs::AtFlags::empty())
                    .map_err(|_| CacheError::Io)?,
                Err(CacheError::Busy) => retained.push(name.clone()),
                Err(error) => return Err(error),
            }
        }
        state.newest = retained;
        self.save(state)
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
    #[test]
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
