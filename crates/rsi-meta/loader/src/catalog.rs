use super::{LoaderError, MAX_ARTIFACT_BYTES, NativeFactory, NativeModule};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::fmt;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::RecvTimeoutError;
use std::sync::{Arc, Mutex, Weak};
use std::time::Duration;
use tempfile::NamedTempFile;

#[derive(Clone, Debug)]
pub struct CatalogOptions {
    pub cache_directory: PathBuf,
    pub callback_timeout: Duration,
}

impl CatalogOptions {
    pub fn new(cache_directory: impl Into<PathBuf>) -> Self {
        Self {
            cache_directory: cache_directory.into(),
            callback_timeout: Duration::from_secs(30),
        }
    }
}

#[derive(Clone)]
pub struct NativeCatalog {
    inner: Arc<CatalogInner>,
}

struct CatalogInner {
    options: CatalogOptions,
    modules: Mutex<BTreeMap<String, Weak<NativeModule>>>,
    load_gates: Mutex<BTreeMap<String, Weak<LoadGate>>>,
}

struct LoadGate {
    callback: Mutex<()>,
    timed_out: AtomicBool,
}

impl Default for LoadGate {
    fn default() -> Self {
        Self {
            callback: Mutex::new(()),
            timed_out: AtomicBool::new(false),
        }
    }
}

impl fmt::Debug for NativeCatalog {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("NativeCatalog")
            .field("cache_directory", &self.inner.options.cache_directory)
            .finish_non_exhaustive()
    }
}

impl NativeCatalog {
    pub fn new(options: CatalogOptions) -> Result<Self, LoaderError> {
        if options.callback_timeout.is_zero() {
            return Err(LoaderError::InvalidInput(
                "native callback timeout must be nonzero".to_owned(),
            ));
        }
        fs::create_dir_all(&options.cache_directory)?;
        Ok(Self {
            inner: Arc::new(CatalogInner {
                options,
                modules: Mutex::new(BTreeMap::new()),
                load_gates: Mutex::new(BTreeMap::new()),
            }),
        })
    }

    /// Verifies, stages, maps, and describes one trusted native artifact.
    pub fn load(&self, source: impl AsRef<Path>) -> Result<Arc<NativeFactory>, LoaderError> {
        self.inner
            .modules
            .lock()
            .expect("catalog poisoned")
            .retain(|_, module| module.strong_count() != 0);
        let bytes = read_bounded(source.as_ref())?;
        let digest = hex::encode(Sha256::digest(&bytes));
        let load_gate = {
            let mut gates = self.inner.load_gates.lock().expect("catalog poisoned");
            gates.retain(|_, gate| gate.strong_count() != 0);
            if let Some(gate) = gates.get(&digest).and_then(Weak::upgrade) {
                gate
            } else {
                let gate = Arc::new(LoadGate::default());
                gates.insert(digest.clone(), Arc::downgrade(&gate));
                gate
            }
        };
        let _load = load_gate
            .callback
            .lock()
            .expect("native load gate poisoned");
        if load_gate.timed_out.load(Ordering::Acquire) {
            return Err(LoaderError::Callback {
                operation: "load",
                message: "a previous timed-out worker is still inside this artifact".to_owned(),
            });
        }
        let cached = self
            .inner
            .modules
            .lock()
            .expect("catalog poisoned")
            .get(&digest)
            .and_then(Weak::upgrade);
        let module = if let Some(module) = cached {
            module
        } else {
            let cached_artifact = self.cache_artifact(&digest, &bytes)?;
            let staged = self.mapping_artifact(cached_artifact, &bytes)?;
            let timeout = self.inner.options.callback_timeout;
            let worker_digest = digest.clone();
            let worker_gate = Arc::clone(&load_gate);
            let (sender, receiver) = std::sync::mpsc::sync_channel(1);
            std::thread::Builder::new()
                .name("rsi-meta-native-load".to_owned())
                .spawn(move || {
                    // SAFETY: The pinned staged file contains the bytes hashed
                    // by the catalog and the caller trusts their native code.
                    let result = unsafe { NativeModule::load(staged, worker_digest) }.map(Arc::new);
                    drop(worker_gate);
                    let _ = sender.send(result);
                })?;
            let loaded = match receiver.recv_timeout(timeout) {
                Ok(result) => result?,
                Err(RecvTimeoutError::Timeout) => {
                    load_gate.timed_out.store(true, Ordering::Release);
                    return Err(LoaderError::Timeout("library load, entry, or descriptor"));
                }
                Err(RecvTimeoutError::Disconnected) => {
                    return Err(LoaderError::Callback {
                        operation: "load",
                        message: "native load worker disconnected".to_owned(),
                    });
                }
            };
            let mut modules = self.inner.modules.lock().expect("catalog poisoned");
            if let Some(existing) = modules.get(&digest).and_then(Weak::upgrade) {
                existing
            } else {
                modules.insert(digest.clone(), Arc::downgrade(&loaded));
                loaded
            }
        };
        let descriptor = module.descriptor.clone();
        Ok(Arc::new(NativeFactory {
            module,
            descriptor,
            callback_timeout: self.inner.options.callback_timeout,
        }))
    }

    #[cfg(all(test, unix))]
    fn stage(&self, digest: &str, bytes: &[u8]) -> Result<StagedArtifact, LoaderError> {
        let cached = self.cache_artifact(digest, bytes)?;
        self.mapping_artifact(cached, bytes)
    }

    fn cache_artifact(&self, digest: &str, bytes: &[u8]) -> Result<StagedArtifact, LoaderError> {
        let target = self
            .inner
            .options
            .cache_directory
            .join(format!("{digest}.native"));
        let cached = if let Ok((file, existing)) = read_bounded_file(&target) {
            verify_staged(target, file, &existing, bytes)?
        } else {
            let mut temporary = NamedTempFile::new_in(&self.inner.options.cache_directory)?;
            temporary.write_all(bytes)?;
            temporary.as_file().sync_all()?;
            match temporary.persist_noclobber(&target) {
                Ok(file) => StagedArtifact::new(target, file),
                Err(error) if error.error.kind() == std::io::ErrorKind::AlreadyExists => {
                    let (file, existing) = read_bounded_file(&target)?;
                    verify_staged(target, file, &existing, bytes)?
                }
                Err(error) => return Err(LoaderError::Io(error.error)),
            }
        };
        Ok(cached)
    }

    fn mapping_artifact(
        &self,
        cached: StagedArtifact,
        bytes: &[u8],
    ) -> Result<StagedArtifact, LoaderError> {
        #[cfg(unix)]
        {
            drop(cached);
            let mut private = tempfile::tempfile_in(&self.inner.options.cache_directory)?;
            private.write_all(bytes)?;
            Ok(StagedArtifact::new(PathBuf::new(), private))
        }
        #[cfg(not(unix))]
        Ok(cached)
    }
}

fn read_bounded(path: &Path) -> Result<Vec<u8>, LoaderError> {
    let (_, bytes) = read_bounded_file(path)?;
    Ok(bytes)
}

pub(super) struct StagedArtifact {
    #[cfg(not(unix))]
    path: PathBuf,
    pub(super) file: File,
}

impl StagedArtifact {
    fn new(path: PathBuf, file: File) -> Self {
        #[cfg(unix)]
        drop(path);
        Self {
            #[cfg(not(unix))]
            path,
            file,
        }
    }

    pub(super) fn loader_path(&self) -> PathBuf {
        #[cfg(target_os = "linux")]
        {
            use std::os::fd::AsRawFd as _;
            PathBuf::from(format!("/proc/self/fd/{}", self.file.as_raw_fd()))
        }
        #[cfg(all(unix, not(target_os = "linux")))]
        {
            use std::os::fd::AsRawFd as _;
            PathBuf::from(format!("/dev/fd/{}", self.file.as_raw_fd()))
        }
        #[cfg(not(unix))]
        self.path.clone()
    }
}

fn verify_staged(
    path: PathBuf,
    file: File,
    existing: &[u8],
    expected: &[u8],
) -> Result<StagedArtifact, LoaderError> {
    if existing != expected {
        return Err(LoaderError::CacheCollision(path));
    }
    Ok(StagedArtifact::new(path, file))
}

fn read_bounded_file(path: &Path) -> Result<(File, Vec<u8>), LoaderError> {
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
        options.share_mode(FILE_SHARE_READ);
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
    let mut bytes = Vec::with_capacity(usize::try_from(metadata.len()).unwrap_or(0));
    (&file)
        .take(MAX_ARTIFACT_BYTES + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_ARTIFACT_BYTES {
        return Err(LoaderError::ArtifactTooLarge);
    }
    Ok((file, bytes))
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::io::{Seek, SeekFrom};

    #[test]
    fn staged_artifact_is_unchanged_by_in_place_cache_mutation() {
        let directory = tempfile::tempdir().unwrap();
        let catalog = NativeCatalog::new(CatalogOptions::new(directory.path())).unwrap();
        let expected = b"trusted artifact";
        let digest = hex::encode(Sha256::digest(expected));
        let mut artifact = catalog.stage(&digest, expected).unwrap();

        fs::write(
            directory.path().join(format!("{digest}.native")),
            b"changed artifact",
        )
        .unwrap();
        artifact.file.seek(SeekFrom::Start(0)).unwrap();
        let mut observed = Vec::new();
        artifact.file.read_to_end(&mut observed).unwrap();

        assert_eq!(observed, expected);
    }

    #[test]
    fn catalog_prunes_dead_module_identities_on_each_load_attempt() {
        let directory = tempfile::tempdir().unwrap();
        let catalog = NativeCatalog::new(CatalogOptions::new(directory.path())).unwrap();
        catalog
            .inner
            .modules
            .lock()
            .unwrap()
            .insert("dead".to_owned(), Weak::new());

        assert!(catalog.load(directory.path().join("missing")).is_err());
        assert!(catalog.inner.modules.lock().unwrap().is_empty());
    }
}
