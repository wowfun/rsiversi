use super::catalog_io::{
    COPY_BUFFER_BYTES, CacheDirectoryClaim, PersistOutcome, PrivateTemp, cache_path,
    copy_exact_digest, ensure_cache_directory, open_bounded_regular_file, scan_cache,
    streams_equal_to_digest,
};
use super::catalog_resources::{
    CatalogLedger, HostResourceLedger, NativeCatalogLimits, NativeCatalogSnapshot,
    StagingReservation,
};
use super::worker::{
    MAX_LIVE_NATIVE_INSTANCES, MAX_NATIVE_CALLBACK_THREADS, MAX_NATIVE_DESTRUCTION_THREADS,
    NativeExecutor,
};
use super::{LoaderError, MAX_ARTIFACT_BYTES, NativeFactory, NativeModule};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::fmt;
#[cfg(all(test, target_os = "linux"))]
use std::fs;
use std::fs::File;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, Weak};
use std::time::Duration;
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

const MAX_CALLBACK_TIMEOUT: Duration = Duration::from_hours(24);
const MAX_CACHE_ARTIFACTS: usize = 65_536;

mod load;
pub(crate) use load::StagedModuleLoad;

/// Construction policy for a native catalog and its owned resource authority.
#[derive(Clone, Debug)]
pub struct CatalogOptions {
    /// Dedicated content-addressed cache directory claimed exclusively.
    pub cache_directory: PathBuf,
    /// Complete deadline for each admitted native callback (1 ns through 24 h).
    pub callback_timeout: Duration,
    /// Aggregate cache, staging, and worker limits.
    pub limits: NativeCatalogLimits,
}

impl CatalogOptions {
    pub fn new(cache_directory: impl Into<PathBuf>) -> Self {
        Self {
            cache_directory: cache_directory.into(),
            callback_timeout: Duration::from_secs(30),
            limits: NativeCatalogLimits::default(),
        }
    }

    fn validate(&self) -> Result<(), LoaderError> {
        if self.callback_timeout.is_zero()
            || self.callback_timeout > MAX_CALLBACK_TIMEOUT
            || std::time::Instant::now()
                .checked_add(self.callback_timeout)
                .is_none()
        {
            return Err(LoaderError::InvalidInput(format!(
                "native callback timeout must be in 1ns..={}s",
                MAX_CALLBACK_TIMEOUT.as_secs()
            )));
        }
        if self.limits.maximum_cache_bytes == 0 {
            return Err(LoaderError::InvalidInput(
                "maximum_cache_bytes must be nonzero".to_owned(),
            ));
        }
        if self.limits.maximum_cache_artifacts == 0
            || self.limits.maximum_cache_artifacts > MAX_CACHE_ARTIFACTS
        {
            return Err(LoaderError::InvalidInput(format!(
                "maximum_cache_artifacts must be in 1..={MAX_CACHE_ARTIFACTS}"
            )));
        }
        if self.limits.maximum_staging_bytes == 0 {
            return Err(LoaderError::InvalidInput(
                "maximum_staging_bytes must be nonzero".to_owned(),
            ));
        }
        if self.limits.maximum_concurrent_callbacks == 0
            || self.limits.maximum_concurrent_callbacks > MAX_NATIVE_CALLBACK_THREADS
        {
            return Err(LoaderError::InvalidInput(format!(
                "maximum_concurrent_callbacks must be in 1..={MAX_NATIVE_CALLBACK_THREADS}"
            )));
        }
        if self.limits.maximum_concurrent_destructions == 0
            || self.limits.maximum_concurrent_destructions > MAX_NATIVE_DESTRUCTION_THREADS
        {
            return Err(LoaderError::InvalidInput(format!(
                "maximum_concurrent_destructions must be in 1..={MAX_NATIVE_DESTRUCTION_THREADS}"
            )));
        }
        if self.limits.maximum_live_instances == 0
            || self.limits.maximum_live_instances > MAX_LIVE_NATIVE_INSTANCES
        {
            return Err(LoaderError::InvalidInput(format!(
                "maximum_live_instances must be in 1..={MAX_LIVE_NATIVE_INSTANCES}"
            )));
        }
        if self.limits.maximum_host_capabilities == 0
            || self.limits.maximum_host_capabilities > MAX_CACHE_ARTIFACTS
        {
            return Err(LoaderError::InvalidInput(format!(
                "maximum_host_capabilities must be in 1..={MAX_CACHE_ARTIFACTS}"
            )));
        }
        if self.limits.maximum_host_outputs == 0
            || self.limits.maximum_host_outputs > MAX_CACHE_ARTIFACTS
        {
            return Err(LoaderError::InvalidInput(format!(
                "maximum_host_outputs must be in 1..={MAX_CACHE_ARTIFACTS}"
            )));
        }
        if self.limits.maximum_host_output_bytes == 0 {
            return Err(LoaderError::InvalidInput(
                "maximum_host_output_bytes must be nonzero".to_owned(),
            ));
        }
        Ok(())
    }
}

/// Verifies trusted native artifacts and owns their cache and worker budgets.
#[derive(Clone)]
pub struct NativeCatalog {
    inner: Arc<CatalogInner>,
}

pub(super) struct CatalogInner {
    options: CatalogOptions,
    modules: Mutex<BTreeMap<String, Weak<NativeModule>>>,
    load_gates: Mutex<BTreeMap<String, Weak<LoadGate>>>,
    ledger: Arc<CatalogLedger>,
    pub(super) host_resources: Arc<HostResourceLedger>,
    executor: NativeExecutor,
    load_admission: Arc<Semaphore>,
    load_stats: Arc<LoadStats>,
    cache_commit: Mutex<()>,
    cache_poisoned: AtomicBool,
    cache_directory_claim: CacheDirectoryClaim,
    #[cfg(all(test, target_os = "linux"))]
    directory_sync_failures: AtomicUsize,
    cache_lock: File,
}

impl CatalogInner {
    pub(super) fn host_capability_limit(&self) -> usize {
        self.options.limits.maximum_host_capabilities
    }

    pub(super) fn host_output_limit(&self) -> usize {
        self.options.limits.maximum_host_outputs
    }

    pub(super) fn retain_failed_finalization(&self) {
        self.host_resources.retain_failed_finalization();
        self.load_admission.close();
    }
}

#[derive(Default)]
struct LoadStats {
    active: AtomicUsize,
    peak: AtomicUsize,
    rejected: AtomicU64,
}

#[derive(Clone)]
pub(super) struct LoadAdmission {
    inner: Arc<LoadAdmissionInner>,
}

struct LoadAdmissionInner {
    authority: Arc<Semaphore>,
    _permit: OwnedSemaphorePermit,
    stats: Arc<LoadStats>,
}

impl Drop for LoadAdmissionInner {
    fn drop(&mut self) {
        self.stats.active.fetch_sub(1, Ordering::Relaxed);
    }
}

impl Drop for CatalogInner {
    fn drop(&mut self) {
        // Explicit unlock closes the fork-before-exec interval in which a
        // close-on-exec duplicate can otherwise keep the open-file-description
        // lock alive briefly after this owner is dropped.
        let _ = self.cache_lock.unlock();
        #[cfg(unix)]
        self.cache_directory_claim.unlock();
    }
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
            .field("limits", &self.inner.options.limits)
            .finish_non_exhaustive()
    }
}

impl NativeCatalog {
    /// Validates all options, exclusively claims the cache, and starts the
    /// fixed destruction lane.
    pub fn new(options: CatalogOptions) -> Result<Self, LoaderError> {
        options.validate()?;
        ensure_cache_directory(&options.cache_directory)?;
        let cache_directory_claim = CacheDirectoryClaim::capture(&options.cache_directory)?;
        let cache_lock_path = options.cache_directory.join(".rsi-meta.lock");
        #[cfg(unix)]
        if let Err(error) = cache_directory_claim.try_lock() {
            if error.kind() == std::io::ErrorKind::WouldBlock {
                return Err(LoaderError::CacheLocked(cache_lock_path));
            }
            return Err(LoaderError::Io(error));
        }
        let cache_lock = cache_directory_claim.open_lock()?;
        if let Err(error) = cache_lock.try_lock() {
            let error: std::io::Error = error.into();
            if error.kind() == std::io::ErrorKind::WouldBlock {
                return Err(LoaderError::CacheLocked(cache_lock_path));
            }
            return Err(LoaderError::Io(error));
        }
        if !cache_directory_claim.matches_path()? {
            return Err(LoaderError::CachePoisoned);
        }
        let cache_entries = scan_cache(&cache_directory_claim, &options.limits)?;
        if !cache_directory_claim.matches_path()? {
            return Err(LoaderError::CachePoisoned);
        }
        let ledger = Arc::new(CatalogLedger::new(options.limits.clone(), cache_entries));
        let host_resources = HostResourceLedger::new(&options.limits);
        let load_admission = Arc::new(Semaphore::new(options.limits.maximum_concurrent_callbacks));
        let executor = NativeExecutor::new(
            options.limits.maximum_concurrent_callbacks,
            options.limits.maximum_concurrent_destructions,
            options.limits.maximum_cache_artifacts,
            options.limits.maximum_live_instances,
        )?;
        Ok(Self {
            inner: Arc::new(CatalogInner {
                options,
                modules: Mutex::new(BTreeMap::new()),
                load_gates: Mutex::new(BTreeMap::new()),
                ledger,
                host_resources,
                executor,
                load_admission,
                load_stats: Arc::new(LoadStats::default()),
                cache_commit: Mutex::new(()),
                cache_poisoned: AtomicBool::new(false),
                cache_directory_claim,
                #[cfg(all(test, target_os = "linux"))]
                directory_sync_failures: AtomicUsize::new(0),
                cache_lock,
            }),
        })
    }

    /// Captures logical cache, staging, callback, and destruction usage.
    pub fn snapshot(&self) -> NativeCatalogSnapshot {
        let ledger = self.inner.ledger.snapshot();
        let executor = self.inner.executor.snapshot();
        let host = self.inner.host_resources.snapshot();
        NativeCatalogSnapshot {
            cache_bytes: ledger.cache_bytes,
            cache_artifacts: ledger.cache_artifacts,
            peak_cache_bytes: ledger.peak_cache_bytes,
            peak_cache_artifacts: ledger.peak_cache_artifacts,
            staging_bytes: ledger.staging_bytes,
            peak_staging_bytes: ledger.peak_staging_bytes,
            rejected_cache_admissions: ledger.rejected_cache_admissions,
            rejected_staging_admissions: ledger.rejected_staging_admissions,
            active_loads: self.inner.load_stats.active.load(Ordering::Relaxed),
            peak_loads: self.inner.load_stats.peak.load(Ordering::Relaxed),
            rejected_loads: self.inner.load_stats.rejected.load(Ordering::Relaxed),
            active_callbacks: executor.active_callbacks,
            peak_callbacks: executor.peak_callbacks,
            rejected_callbacks: executor.rejected_callbacks,
            active_instances: executor.active_instances,
            peak_instances: executor.peak_instances,
            rejected_instances: executor.rejected_instances,
            pending_instance_destructions: executor.pending_instance_destructions,
            active_destructions: executor.active_destructions,
            peak_destructions: executor.peak_destructions,
            queued_destructions: executor.queued_destructions,
            rejected_destructions: executor.rejected_destructions,
            host_capabilities: host.capabilities,
            peak_host_capabilities: host.peak_capabilities,
            rejected_host_capabilities: host.rejected_capabilities,
            host_outputs: host.outputs,
            peak_host_outputs: host.peak_outputs,
            rejected_host_outputs: host.rejected_outputs,
            host_output_bytes: host.output_bytes,
            peak_host_output_bytes: host.peak_output_bytes,
            rejected_host_output_bytes: host.rejected_output_bytes,
            retained_failed_finalizations: host.retained_failed_finalizations,
        }
    }

    fn ensure_cache_healthy(&self) -> Result<(), LoaderError> {
        if self.inner.cache_poisoned.load(Ordering::Acquire) {
            return Err(LoaderError::CachePoisoned);
        }
        match self.inner.cache_directory_claim.matches_path() {
            Ok(true) => Ok(()),
            Ok(false) | Err(_) => Err(self.poison_cache()),
        }
    }

    fn poison_cache(&self) -> LoaderError {
        self.inner.cache_poisoned.store(true, Ordering::Release);
        LoaderError::CachePoisoned
    }

    fn reopen_staged(&self, staged: &StagedArtifact) -> Result<File, LoaderError> {
        match staged.reopen() {
            Err(LoaderError::CachePoisoned) => Err(self.poison_cache()),
            result => result,
        }
    }

    fn cached_matches_staged(
        &self,
        cached: File,
        staged: &StagedArtifact,
        digest: &str,
    ) -> Result<bool, LoaderError> {
        streams_equal_to_digest(cached, self.reopen_staged(staged)?, staged.bytes, digest)
    }

    #[cfg(target_os = "linux")]
    fn fence_cache_directory(&self) -> Result<(), LoaderError> {
        #[cfg(test)]
        if self
            .inner
            .directory_sync_failures
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |remaining| {
                remaining.checked_sub(1)
            })
            .is_ok()
        {
            return Err(LoaderError::Io(std::io::Error::other(
                "injected cache directory sync failure",
            )));
        }
        self.inner.cache_directory_claim.sync()
    }

    #[cfg(all(test, target_os = "linux"))]
    fn inject_directory_sync_failures(&self, failures: usize) {
        self.inner
            .directory_sync_failures
            .store(failures, Ordering::Release);
    }

    /// Admits, verifies, stages, maps, and describes one trusted artifact.
    pub fn load(&self, source: impl AsRef<Path>) -> Result<Arc<NativeFactory>, LoaderError> {
        let admission = self.try_reserve_load()?;
        self.load_admitted(source, &admission)
    }

    pub(super) fn try_reserve_load(&self) -> Result<LoadAdmission, LoaderError> {
        if self.inner.host_resources.has_retained_failed_finalization() {
            self.inner
                .load_stats
                .rejected
                .fetch_add(1, Ordering::Relaxed);
            return Err(LoaderError::FinalizationPoisoned);
        }
        if let Err(error) = self.ensure_cache_healthy() {
            self.inner
                .load_stats
                .rejected
                .fetch_add(1, Ordering::Relaxed);
            return Err(error);
        }
        let permit = Arc::clone(&self.inner.load_admission)
            .try_acquire_owned()
            .map_err(|_| {
                self.inner
                    .load_stats
                    .rejected
                    .fetch_add(1, Ordering::Relaxed);
                if self.inner.host_resources.has_retained_failed_finalization() {
                    LoaderError::FinalizationPoisoned
                } else {
                    LoaderError::Busy { operation: "load" }
                }
            })?;
        let active = self.inner.load_stats.active.fetch_add(1, Ordering::Relaxed) + 1;
        self.inner
            .load_stats
            .peak
            .fetch_max(active, Ordering::Relaxed);
        Ok(LoadAdmission {
            inner: Arc::new(LoadAdmissionInner {
                authority: Arc::clone(&self.inner.load_admission),
                _permit: permit,
                stats: Arc::clone(&self.inner.load_stats),
            }),
        })
    }

    pub(super) fn load_concurrency_limit(&self) -> usize {
        self.inner.options.limits.maximum_concurrent_callbacks
    }

    pub(super) fn load_admitted(
        &self,
        source: impl AsRef<Path>,
        admission: &LoadAdmission,
    ) -> Result<Arc<NativeFactory>, LoaderError> {
        debug_assert!(Arc::ptr_eq(
            &admission.inner.authority,
            &self.inner.load_admission
        ));
        self.ensure_cache_healthy()?;
        self.load_inner(source.as_ref())
    }

    fn source_digest(source: &Path) -> Result<String, LoaderError> {
        let (mut source, length) = open_bounded_regular_file(source)?;
        if length > MAX_ARTIFACT_BYTES {
            return Err(LoaderError::ArtifactTooLarge);
        }
        let mut hasher = Sha256::new();
        let mut total = 0_u64;
        let mut buffer = vec![0_u8; COPY_BUFFER_BYTES].into_boxed_slice();
        loop {
            let read = source.read(&mut buffer)?;
            if read == 0 {
                break;
            }
            total = total
                .checked_add(u64::try_from(read).expect("buffer length fits u64"))
                .ok_or(LoaderError::ArtifactTooLarge)?;
            if total > MAX_ARTIFACT_BYTES {
                return Err(LoaderError::ArtifactTooLarge);
            }
            hasher.update(&buffer[..read]);
        }
        Ok(hex::encode(hasher.finalize()))
    }

    fn stage_source(&self, source: &Path) -> Result<StagedArtifact, LoaderError> {
        self.ensure_cache_healthy()?;
        let (mut source, length) = open_bounded_regular_file(source)?;
        if length > MAX_ARTIFACT_BYTES {
            return Err(LoaderError::ArtifactTooLarge);
        }
        let mut reservation = self.inner.ledger.reserve_staging();
        reservation.grow_to(length)?;
        let mut reserved = length;
        let mut temporary = PrivateTemp::new_in(&self.inner.cache_directory_claim)?;
        let mut hasher = Sha256::new();
        let mut total = 0_u64;
        let mut buffer = vec![0_u8; COPY_BUFFER_BYTES].into_boxed_slice();
        loop {
            let read = source.read(&mut buffer)?;
            if read == 0 {
                break;
            }
            total = total
                .checked_add(u64::try_from(read).expect("buffer length fits u64"))
                .ok_or(LoaderError::ArtifactTooLarge)?;
            if total > MAX_ARTIFACT_BYTES {
                return Err(LoaderError::ArtifactTooLarge);
            }
            if total > reserved {
                reservation.grow_to(total)?;
                reserved = total;
            }
            hasher.update(&buffer[..read]);
            temporary.file_mut().write_all(&buffer[..read])?;
        }
        if total < reserved {
            reservation.shrink_to(total);
        }
        let digest = hex::encode(hasher.finalize());
        #[cfg(not(windows))]
        let temporary = temporary.seal_for_read(total, &digest);
        #[cfg(windows)]
        let temporary = temporary.seal_for_read(total, &digest)?;
        Ok(StagedArtifact {
            temporary,
            _reservation: reservation,
            digest,
            bytes: total,
        })
    }

    fn commit_cache(&self, digest: &str, staged: &StagedArtifact) -> Result<(), LoaderError> {
        self.ensure_cache_healthy()?;
        let target = cache_path(&self.inner.options.cache_directory, digest);
        match self.inner.cache_directory_claim.open_cache(digest) {
            Ok((cached, _)) => {
                if !self.cached_matches_staged(cached, staged, digest)? {
                    return Err(LoaderError::CacheCollision(target));
                }
                let _commit = self
                    .inner
                    .cache_commit
                    .lock()
                    .expect("cache commit transaction poisoned");
                self.ensure_cache_healthy()?;
                if !self.inner.ledger.contains_cache(digest) {
                    self.inner
                        .ledger
                        .reserve_cache(digest, staged.bytes)?
                        .commit();
                }
                return Ok(());
            }
            Err(LoaderError::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }

        let reservation = self.inner.ledger.reserve_cache(digest, staged.bytes)?;
        let mut temporary = PrivateTemp::new_in(&self.inner.cache_directory_claim)?;
        copy_exact_digest(
            self.reopen_staged(staged)?,
            temporary.file_mut(),
            staged.bytes,
            digest,
        )?;
        temporary.file().sync_all()?;
        // Sync the verified commit copy before no-clobber publication. The
        // transaction lock linearizes pathname publication; the following
        // streamed comparison proves that publication still names those bytes
        // before durability and accounting. Normal-path I/O stays outside the
        // global commit lock, while rollback reacquires it when required.
        let outcome = {
            let _commit = self
                .inner
                .cache_commit
                .lock()
                .expect("cache commit transaction poisoned");
            self.ensure_cache_healthy()?;
            temporary.persist_noclobber(&target)
        };
        let outcome = match outcome {
            Ok(outcome) => outcome,
            Err(LoaderError::CachePoisoned) => return Err(self.poison_cache()),
            Err(error) => return Err(error),
        };
        match outcome {
            PersistOutcome::Published => {
                let publication_matches = match self
                    .inner
                    .cache_directory_claim
                    .open_cache(digest)
                    .and_then(|(published, _)| {
                        self.cached_matches_staged(published, staged, digest)
                    }) {
                    Ok(matches) => matches,
                    Err(error) => return self.rollback_published_cache(&target, error),
                };
                if !publication_matches {
                    return self.rollback_published_cache(
                        &target,
                        LoaderError::CacheCollision(target.clone()),
                    );
                }
                #[cfg(target_os = "linux")]
                if let Err(error) = self.fence_cache_directory() {
                    let _commit = self
                        .inner
                        .cache_commit
                        .lock()
                        .expect("cache commit transaction poisoned");
                    return self.rollback_published_cache(&target, error);
                }
                let _commit = self
                    .inner
                    .cache_commit
                    .lock()
                    .expect("cache commit transaction poisoned");
                if let Err(error) = self.ensure_cache_healthy() {
                    return self.rollback_published_cache(&target, error);
                }
                reservation.commit();
                Ok(())
            }
            PersistOutcome::AlreadyExists => {
                let (cached, _) = self.inner.cache_directory_claim.open_cache(digest)?;
                if !self.cached_matches_staged(cached, staged, digest)? {
                    return Err(LoaderError::CacheCollision(target));
                }
                let _commit = self
                    .inner
                    .cache_commit
                    .lock()
                    .expect("cache commit transaction poisoned");
                self.ensure_cache_healthy()?;
                reservation.commit();
                Ok(())
            }
        }
    }

    fn rollback_published_cache(
        &self,
        target: &Path,
        cause: LoaderError,
    ) -> Result<(), LoaderError> {
        match self.inner.cache_directory_claim.remove_file(target) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(_) => return Err(self.poison_cache()),
        }
        #[cfg(target_os = "linux")]
        if self.fence_cache_directory().is_err() {
            return Err(self.poison_cache());
        }
        Err(cause)
    }
}

pub(super) struct StagedArtifact {
    temporary: PrivateTemp,
    _reservation: StagingReservation,
    digest: String,
    bytes: u64,
}

impl StagedArtifact {
    pub(super) fn loader_path(&self) -> PathBuf {
        #[cfg(target_os = "linux")]
        {
            use std::os::fd::AsRawFd as _;
            PathBuf::from(format!(
                "/proc/self/fd/{}",
                self.temporary.file().as_raw_fd()
            ))
        }
        #[cfg(all(unix, not(target_os = "linux")))]
        {
            use std::os::fd::AsRawFd as _;
            PathBuf::from(format!("/dev/fd/{}", self.temporary.file().as_raw_fd()))
        }
        #[cfg(not(unix))]
        self.temporary.path().to_path_buf()
    }

    fn reopen(&self) -> Result<File, LoaderError> {
        self.temporary.reopen()
    }
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;

    fn staged_bytes(catalog: &NativeCatalog, bytes: &[u8]) -> StagedArtifact {
        let source_directory = tempfile::tempdir().unwrap();
        let source = source_directory.path().join("source.native");
        fs::write(&source, bytes).unwrap();
        catalog.stage_source(&source).unwrap()
    }

    #[test]
    fn rollback_after_path_replacement_only_removes_the_claimed_publication() {
        let parent = tempfile::tempdir().unwrap();
        let cache = parent.path().join("cache");
        fs::create_dir(&cache).unwrap();
        let catalog = NativeCatalog::new(CatalogOptions::new(&cache)).unwrap();
        let digest = "a".repeat(64);
        let target = cache_path(&cache, &digest);
        fs::write(&target, b"claimed publication").unwrap();

        let claimed = parent.path().join("claimed");
        fs::rename(&cache, &claimed).unwrap();
        fs::create_dir(&cache).unwrap();
        let replacement_target = cache_path(&cache, &digest);
        fs::write(&replacement_target, b"replacement owner").unwrap();

        assert!(matches!(
            catalog.rollback_published_cache(&target, LoaderError::CachePoisoned),
            Err(LoaderError::CachePoisoned)
        ));
        assert_eq!(fs::read(&replacement_target).unwrap(), b"replacement owner");
        assert!(!cache_path(&claimed, &digest).exists());
    }

    #[test]
    fn final_directory_sync_failure_rolls_back_publication_and_cache_charge() {
        let cache = tempfile::tempdir().unwrap();
        let catalog = NativeCatalog::new(CatalogOptions::new(cache.path())).unwrap();
        let staged = staged_bytes(&catalog, b"durability-failure");
        let target = cache_path(cache.path(), &staged.digest);
        catalog.inject_directory_sync_failures(1);

        assert!(matches!(
            catalog.commit_cache(&staged.digest, &staged),
            Err(LoaderError::Io(error)) if error.kind() == std::io::ErrorKind::Other
        ));
        assert!(!target.exists());
        assert_eq!(catalog.snapshot().cache_artifacts, 0);
        assert_eq!(catalog.snapshot().cache_bytes, 0);

        catalog.commit_cache(&staged.digest, &staged).unwrap();
        assert!(target.is_file());
        assert_eq!(catalog.snapshot().cache_artifacts, 1);
        assert_eq!(catalog.snapshot().cache_bytes, staged.bytes);
    }

    #[test]
    fn failed_rollback_durability_poisons_later_catalog_admission() {
        let cache = tempfile::tempdir().unwrap();
        let catalog = NativeCatalog::new(CatalogOptions::new(cache.path())).unwrap();
        let staged = staged_bytes(&catalog, b"poisoned-durability");
        let target = cache_path(cache.path(), &staged.digest);
        catalog.inject_directory_sync_failures(2);

        assert!(matches!(
            catalog.commit_cache(&staged.digest, &staged),
            Err(LoaderError::CachePoisoned)
        ));
        assert!(!target.exists());
        assert_eq!(catalog.snapshot().cache_artifacts, 0);
        assert_eq!(catalog.snapshot().cache_bytes, 0);
        assert!(matches!(
            catalog.try_reserve_load(),
            Err(LoaderError::CachePoisoned)
        ));
    }

    #[test]
    fn retained_failed_finalization_closes_later_catalog_load_admission() {
        let cache = tempfile::tempdir().unwrap();
        let catalog = NativeCatalog::new(CatalogOptions::new(cache.path())).unwrap();
        catalog.inner.retain_failed_finalization();

        assert!(matches!(
            catalog.try_reserve_load(),
            Err(LoaderError::FinalizationPoisoned)
        ));
    }

    #[test]
    fn replaced_staging_name_is_preserved_and_poisons_later_catalog_admission() {
        let cache = tempfile::tempdir().unwrap();
        let catalog = NativeCatalog::new(CatalogOptions::new(cache.path())).unwrap();
        let staged = staged_bytes(&catalog, b"replaced-staging");
        let staging_path = staged.temporary.path().to_path_buf();
        fs::remove_file(&staging_path).unwrap();
        fs::write(&staging_path, b"replacement owner").unwrap();

        assert!(matches!(
            catalog.commit_cache(&staged.digest, &staged),
            Err(LoaderError::CachePoisoned)
        ));
        assert!(matches!(
            catalog.try_reserve_load(),
            Err(LoaderError::CachePoisoned)
        ));
        drop(staged);
        assert_eq!(fs::read(staging_path).unwrap(), b"replacement owner");
    }
}
