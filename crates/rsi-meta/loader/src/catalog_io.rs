use super::catalog_resources::NativeCatalogLimits;
use super::{LoaderError, MAX_ARTIFACT_BYTES};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};

mod private_temp;

pub(super) use private_temp::{PersistOutcome, PrivateTemp};

pub(super) const COPY_BUFFER_BYTES: usize = 64 * 1024;

#[cfg(unix)]
#[derive(Debug)]
pub(super) struct CacheDirectoryClaim {
    path: PathBuf,
    directory: File,
    identity: FileIdentity,
}

#[cfg(windows)]
#[derive(Debug)]
pub(super) struct CacheDirectoryClaim {
    path: PathBuf,
}

#[cfg(unix)]
#[derive(Clone, Debug, Eq, PartialEq)]
struct FileIdentity {
    device: u64,
    inode: u64,
}

#[cfg(unix)]
impl FileIdentity {
    fn from_metadata(metadata: &fs::Metadata) -> Self {
        use std::os::unix::fs::MetadataExt as _;
        Self {
            device: metadata.dev(),
            inode: metadata.ino(),
        }
    }
}

#[cfg(unix)]
impl CacheDirectoryClaim {
    pub(super) fn capture(path: &Path) -> Result<Self, LoaderError> {
        use rustix::fs::{Mode, OFlags};

        let directory = rustix::fs::open(
            path,
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::empty(),
        )
        .map(File::from)
        .map_err(|error| LoaderError::Io(error.into()))?;
        let metadata = directory.metadata()?;
        if !metadata.file_type().is_dir() {
            return Err(LoaderError::InvalidInput(format!(
                "catalog cache is not a directory: {}",
                path.display()
            )));
        }
        Ok(Self {
            path: path.to_path_buf(),
            directory,
            identity: FileIdentity::from_metadata(&metadata),
        })
    }

    pub(super) fn matches_path(&self) -> Result<bool, LoaderError> {
        let metadata = fs::symlink_metadata(&self.path)?;
        if !metadata.file_type().is_dir() {
            return Ok(false);
        }
        Ok(self.identity == FileIdentity::from_metadata(&metadata))
    }

    pub(super) fn try_lock(&self) -> io::Result<()> {
        self.directory.try_lock().map_err(Into::into)
    }

    pub(super) fn unlock(&self) {
        let _ = self.directory.unlock();
    }

    pub(super) fn open_lock(&self) -> Result<File, LoaderError> {
        use rustix::fs::{Mode, OFlags};

        let path = self.path.join(".rsi-meta.lock");
        let file = rustix::fs::openat(
            &self.directory,
            ".rsi-meta.lock",
            OFlags::CREATE | OFlags::RDWR | OFlags::CLOEXEC | OFlags::NOFOLLOW,
            Mode::RUSR | Mode::WUSR,
        )
        .map(File::from)
        .map_err(|error| {
            if matches!(error, rustix::io::Errno::LOOP | rustix::io::Errno::ISDIR) {
                LoaderError::InvalidInput(format!(
                    "catalog lock is not a regular file: {}",
                    path.display()
                ))
            } else {
                LoaderError::Io(error.into())
            }
        })?;
        validate_cache_lock_file(&path, &file)?;
        Ok(file)
    }

    pub(super) fn remove_file(&self, target: &Path) -> io::Result<()> {
        let name = target.file_name().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("cache target has no file name: {}", target.display()),
            )
        })?;
        rustix::fs::unlinkat(&self.directory, name, rustix::fs::AtFlags::empty())
            .map_err(Into::into)
    }

    fn open_regular(&self, name: &str) -> Result<(File, u64), LoaderError> {
        use rustix::fs::{Mode, OFlags};

        let path = self.path.join(name);
        let file = rustix::fs::openat(
            &self.directory,
            name,
            OFlags::RDONLY | OFlags::CLOEXEC | OFlags::NONBLOCK | OFlags::NOFOLLOW,
            Mode::empty(),
        )
        .map(File::from)
        .map_err(|error| {
            if error == rustix::io::Errno::LOOP {
                LoaderError::InvalidInput(format!(
                    "artifact is not a regular file: {}",
                    path.display()
                ))
            } else {
                LoaderError::Io(error.into())
            }
        })?;
        let metadata = file.metadata()?;
        if !metadata.is_file() {
            return Err(LoaderError::InvalidInput(format!(
                "artifact is not a regular file: {}",
                path.display()
            )));
        }
        if metadata.len() > MAX_ARTIFACT_BYTES {
            return Err(LoaderError::ArtifactTooLarge);
        }
        let length = metadata.len();
        Ok((file, length))
    }

    #[cfg(target_os = "linux")]
    pub(super) fn sync(&self) -> Result<(), LoaderError> {
        self.directory.sync_all()?;
        Ok(())
    }
}

impl CacheDirectoryClaim {
    pub(super) fn open_cache(&self, digest: &str) -> Result<(File, u64), LoaderError> {
        self.open_regular(&cache_name(digest))
    }
}

#[cfg(windows)]
impl CacheDirectoryClaim {
    pub(super) fn capture(path: &Path) -> Result<Self, LoaderError> {
        let metadata = fs::symlink_metadata(path)?;
        if !metadata.file_type().is_dir() {
            return Err(LoaderError::InvalidInput(format!(
                "catalog cache is not a directory: {}",
                path.display()
            )));
        }
        // The marker handle deliberately omits FILE_SHARE_DELETE. Windows then
        // pins the lock entry and its containing directory for this Catalog.
        Ok(Self {
            path: path.to_path_buf(),
        })
    }

    pub(super) fn matches_path(&self) -> Result<bool, LoaderError> {
        Ok(fs::symlink_metadata(&self.path)?.file_type().is_dir())
    }

    pub(super) fn open_lock(&self) -> Result<File, LoaderError> {
        use std::os::windows::fs::OpenOptionsExt as _;
        const FILE_SHARE_READ: u32 = 0x0000_0001;
        const FILE_SHARE_WRITE: u32 = 0x0000_0002;
        const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
        let path = self.path.join(".rsi-meta.lock");
        validate_cache_lock_path(&path)?;
        let mut options = OpenOptions::new();
        options.create(true).read(true).write(true).truncate(false);
        options
            .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE)
            .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT);
        let file = options.open(&path)?;
        validate_cache_lock_file(&path, &file)?;
        Ok(file)
    }

    pub(super) fn remove_file(&self, target: &Path) -> io::Result<()> {
        let name = target.file_name().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("cache target has no file name: {}", target.display()),
            )
        })?;
        fs::remove_file(self.path.join(name))
    }

    fn open_regular(&self, name: &str) -> Result<(File, u64), LoaderError> {
        open_bounded_regular_file(&self.path.join(name))
    }
}

pub(super) fn cache_path(directory: &Path, digest: &str) -> PathBuf {
    directory.join(cache_name(digest))
}

fn cache_name(digest: &str) -> String {
    format!("{digest}.native")
}

pub(super) fn ensure_cache_directory(path: &Path) -> Result<(), LoaderError> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_dir() => return Ok(()),
        Ok(_) => {
            return Err(LoaderError::InvalidInput(format!(
                "catalog cache is not a directory: {}",
                path.display()
            )));
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(LoaderError::Io(error)),
    }
    fs::create_dir_all(path)?;
    if !fs::symlink_metadata(path)?.file_type().is_dir() {
        return Err(LoaderError::InvalidInput(format!(
            "catalog cache is not a directory: {}",
            path.display()
        )));
    }
    Ok(())
}

#[cfg(windows)]
fn validate_cache_lock_path(path: &Path) -> Result<(), LoaderError> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if !metadata.file_type().is_file() => {
            return Err(LoaderError::InvalidInput(format!(
                "catalog lock is not a regular file: {}",
                path.display()
            )));
        }
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(LoaderError::Io(error)),
    }
    Ok(())
}

fn validate_cache_lock_file(path: &Path, file: &File) -> Result<(), LoaderError> {
    if !file.metadata()?.is_file() {
        return Err(LoaderError::InvalidInput(format!(
            "catalog lock is not a regular file: {}",
            path.display()
        )));
    }
    Ok(())
}

pub(super) fn scan_cache(
    claim: &CacheDirectoryClaim,
    limits: &NativeCatalogLimits,
) -> Result<BTreeMap<String, u64>, LoaderError> {
    let mut entries = BTreeMap::new();
    let mut bytes = 0_u64;

    #[cfg(unix)]
    {
        let directory = rustix::fs::Dir::read_from(&claim.directory)
            .map_err(|error| LoaderError::Io(error.into()))?;
        for entry in directory {
            let entry = entry.map_err(|error| LoaderError::Io(error.into()))?;
            let name = entry.file_name().to_str().map_err(|_| {
                LoaderError::InvalidInput(format!(
                    "catalog cache contains a non-UTF-8 entry: {}",
                    claim.path.display()
                ))
            })?;
            account_cache_entry(claim, name, limits, &mut entries, &mut bytes)?;
        }
    }
    #[cfg(windows)]
    for entry in fs::read_dir(&claim.path)? {
        let entry = entry?;
        let name = entry.file_name();
        let name = name.to_str().ok_or_else(|| {
            LoaderError::InvalidInput(format!(
                "catalog cache contains a non-UTF-8 entry: {}",
                entry.path().display()
            ))
        })?;
        account_cache_entry(claim, name, limits, &mut entries, &mut bytes)?;
    }
    Ok(entries)
}

fn account_cache_entry(
    claim: &CacheDirectoryClaim,
    name: &str,
    limits: &NativeCatalogLimits,
    entries: &mut BTreeMap<String, u64>,
    bytes: &mut u64,
) -> Result<(), LoaderError> {
    if name == "." || name == ".." || name == ".rsi-meta.lock" {
        return Ok(());
    }
    let path = claim.path.join(name);
    let digest = managed_digest(name).ok_or_else(|| {
        LoaderError::InvalidInput(format!(
            "catalog cache contains an unmanaged entry: {}",
            path.display()
        ))
    })?;
    let (_, length) = claim.open_regular(name).map_err(|error| match error {
        LoaderError::ArtifactTooLarge => LoaderError::InvalidInput(format!(
            "managed cache entry exceeds the artifact limit: {}",
            path.display()
        )),
        LoaderError::InvalidInput(_) => LoaderError::InvalidInput(format!(
            "managed cache entry is not a regular file: {}",
            path.display()
        )),
        error => error,
    })?;
    if entries.len() >= limits.maximum_cache_artifacts {
        return Err(LoaderError::CapacityExhausted {
            resource: "cache artifacts",
            limit: u64::try_from(limits.maximum_cache_artifacts).unwrap_or(u64::MAX),
        });
    }
    *bytes = bytes
        .checked_add(length)
        .ok_or(LoaderError::CapacityExhausted {
            resource: "cache bytes",
            limit: limits.maximum_cache_bytes,
        })?;
    if *bytes > limits.maximum_cache_bytes {
        return Err(LoaderError::CapacityExhausted {
            resource: "cache bytes",
            limit: limits.maximum_cache_bytes,
        });
    }
    entries.insert(digest.to_owned(), length);
    Ok(())
}

fn managed_digest(name: &str) -> Option<&str> {
    let digest = name.strip_suffix(".native")?;
    (digest.len() == 64
        && digest
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)))
    .then_some(digest)
}

pub(super) fn open_bounded_regular_file(path: &Path) -> Result<(File, u64), LoaderError> {
    let initial = fs::symlink_metadata(path)?;
    if !initial.file_type().is_file() {
        return Err(LoaderError::InvalidInput(format!(
            "artifact is not a regular file: {}",
            path.display()
        )));
    }
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.custom_flags(libc::O_CLOEXEC | libc::O_NONBLOCK | libc::O_NOFOLLOW);
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt as _;
        const FILE_SHARE_READ: u32 = 0x0000_0001;
        const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
        options
            .share_mode(FILE_SHARE_READ)
            .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT);
    }
    let file = options.open(path)?;
    let metadata = file.metadata()?;
    if !metadata.is_file() {
        return Err(LoaderError::InvalidInput(format!(
            "artifact is not a regular file: {}",
            path.display()
        )));
    }
    if metadata.len() > MAX_ARTIFACT_BYTES {
        return Err(LoaderError::ArtifactTooLarge);
    }
    Ok((file, metadata.len()))
}

fn compare_streams(
    mut left: File,
    mut right: File,
    expected_right_bytes: u64,
    mut consume_right: impl FnMut(&[u8]),
) -> Result<bool, LoaderError> {
    let mut left_buffer = vec![0_u8; COPY_BUFFER_BYTES].into_boxed_slice();
    let mut right_buffer = vec![0_u8; COPY_BUFFER_BYTES].into_boxed_slice();
    let mut right_bytes = 0_u64;
    let mut equal = true;
    loop {
        let left_read = left.read(&mut left_buffer)?;
        let right_read = right.read(&mut right_buffer)?;
        right_bytes = right_bytes
            .checked_add(u64::try_from(right_read).expect("buffer length fits u64"))
            .ok_or(LoaderError::StagedArtifactChanged)?;
        if right_bytes > expected_right_bytes {
            return Err(LoaderError::StagedArtifactChanged);
        }
        consume_right(&right_buffer[..right_read]);
        if left_read != right_read || left_buffer[..left_read] != right_buffer[..right_read] {
            equal = false;
        }
        if right_read == 0 {
            if right_bytes != expected_right_bytes {
                return Err(LoaderError::StagedArtifactChanged);
            }
            return Ok(equal);
        }
    }
}

pub(super) fn streams_equal_to_digest(
    left: File,
    right: File,
    expected_right_bytes: u64,
    expected_digest: &str,
) -> Result<bool, LoaderError> {
    let mut hasher = Sha256::new();
    let equal = compare_streams(left, right, expected_right_bytes, |bytes| {
        hasher.update(bytes);
    })?;
    if hex::encode(hasher.finalize()) != expected_digest {
        return Err(LoaderError::StagedArtifactChanged);
    }
    Ok(equal)
}

pub(super) fn copy_exact_digest(
    mut source: File,
    target: &mut File,
    expected_bytes: u64,
    expected_digest: &str,
) -> Result<(), LoaderError> {
    consume_exact_digest(&mut source, expected_bytes, expected_digest, |bytes| {
        target.write_all(bytes)?;
        Ok(())
    })
}

#[cfg(windows)]
fn verify_exact_digest(
    source: &mut File,
    expected_bytes: u64,
    expected_digest: &str,
) -> Result<(), LoaderError> {
    consume_exact_digest(source, expected_bytes, expected_digest, |_| Ok(()))
}

fn consume_exact_digest(
    source: &mut File,
    expected_bytes: u64,
    expected_digest: &str,
    mut consume: impl FnMut(&[u8]) -> Result<(), LoaderError>,
) -> Result<(), LoaderError> {
    let mut copied = 0_u64;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0_u8; COPY_BUFFER_BYTES].into_boxed_slice();
    loop {
        let read = source.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        copied = copied
            .checked_add(u64::try_from(read).expect("buffer length fits u64"))
            .ok_or(LoaderError::StagedArtifactChanged)?;
        if copied > expected_bytes {
            return Err(LoaderError::StagedArtifactChanged);
        }
        hasher.update(&buffer[..read]);
        consume(&buffer[..read])?;
    }
    if copied != expected_bytes || hex::encode(hasher.finalize()) != expected_digest {
        return Err(LoaderError::StagedArtifactChanged);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn managed_cache_names_are_exact_lowercase_digests() {
        assert!(managed_digest(&format!("{}.native", "a".repeat(64))).is_some());
        assert!(managed_digest(&format!("{}.native", "A".repeat(64))).is_none());
        assert!(managed_digest("not-a-digest.native").is_none());
        assert!(managed_digest(".rsi-meta.lock").is_none());
    }

    #[cfg(unix)]
    #[test]
    fn cache_scan_uses_the_claimed_directory_after_path_aba() {
        let parent = tempfile::tempdir().unwrap();
        let cache = parent.path().join("cache");
        fs::create_dir(&cache).unwrap();
        let claimed_digest = "a".repeat(64);
        fs::write(cache_path(&cache, &claimed_digest), b"claimed").unwrap();
        let claim = CacheDirectoryClaim::capture(&cache).unwrap();

        let claimed = parent.path().join("claimed");
        fs::rename(&cache, &claimed).unwrap();
        fs::create_dir(&cache).unwrap();
        let replacement_digest = "b".repeat(64);
        fs::write(cache_path(&cache, &replacement_digest), b"replacement").unwrap();
        let scanned = scan_cache(&claim, &NativeCatalogLimits::default()).unwrap();
        let replacement = parent.path().join("replacement");
        fs::rename(&cache, &replacement).unwrap();
        fs::rename(&claimed, &cache).unwrap();

        assert!(claim.matches_path().unwrap());
        assert_eq!(
            scanned,
            BTreeMap::from([(claimed_digest, u64::try_from(b"claimed".len()).unwrap())])
        );
    }

    #[cfg(unix)]
    #[test]
    fn lock_open_uses_the_claimed_directory_after_path_replacement() {
        let parent = tempfile::tempdir().unwrap();
        let cache = parent.path().join("cache");
        fs::create_dir(&cache).unwrap();
        let claim = CacheDirectoryClaim::capture(&cache).unwrap();
        let claimed = parent.path().join("claimed");
        fs::rename(&cache, &claimed).unwrap();
        fs::create_dir(&cache).unwrap();
        fs::create_dir(cache.join(".rsi-meta.lock")).unwrap();

        claim
            .open_lock()
            .expect("the public pathname is not the Unix lock authority");
        assert!(claimed.join(".rsi-meta.lock").is_file());
        assert!(cache.join(".rsi-meta.lock").is_dir());
    }

    #[cfg(unix)]
    #[test]
    fn cache_open_uses_the_claimed_directory_after_path_aba() {
        let parent = tempfile::tempdir().unwrap();
        let cache = parent.path().join("cache");
        fs::create_dir(&cache).unwrap();
        let digest = "c".repeat(64);
        fs::write(cache_path(&cache, &digest), b"claimed").unwrap();
        let claim = CacheDirectoryClaim::capture(&cache).unwrap();

        let claimed = parent.path().join("claimed");
        fs::rename(&cache, &claimed).unwrap();
        fs::create_dir(&cache).unwrap();
        fs::write(cache_path(&cache, &digest), b"replacement").unwrap();
        let (mut opened, _) = claim.open_cache(&digest).unwrap();
        let replacement = parent.path().join("replacement");
        fs::rename(&cache, &replacement).unwrap();
        fs::rename(&claimed, &cache).unwrap();

        let mut bytes = Vec::new();
        opened.read_to_end(&mut bytes).unwrap();
        assert!(claim.matches_path().unwrap());
        assert_eq!(bytes, b"claimed");
    }
}
