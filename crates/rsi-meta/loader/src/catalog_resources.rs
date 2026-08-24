use super::LoaderError;
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

/// Aggregate disk, staging, and native-worker limits owned by one catalog.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NativeCatalogLimits {
    /// Maximum nonzero bytes retained under managed durable digest names.
    pub maximum_cache_bytes: u64,
    /// Maximum managed durable digest entries and simultaneously mapped
    /// unique factories (1 through 65,536).
    pub maximum_cache_artifacts: usize,
    /// Maximum nonzero bytes retained by live private staging artifacts.
    pub maximum_staging_bytes: u64,
    /// Maximum admitted catalog loads and foreign callback threads (1 through
    /// 256 each).
    pub maximum_concurrent_callbacks: usize,
    /// Maximum native instances retaining create-owned handles (1 through
    /// 65,536).
    pub maximum_live_instances: usize,
    /// Number of dedicated foreign-destruction workers (1 through 64).
    pub maximum_concurrent_destructions: usize,
}

impl Default for NativeCatalogLimits {
    fn default() -> Self {
        Self {
            maximum_cache_bytes: 2 * 1024 * 1024 * 1024,
            maximum_cache_artifacts: 256,
            maximum_staging_bytes: 512 * 1024 * 1024,
            maximum_concurrent_callbacks: 8,
            maximum_live_instances: 4096,
            maximum_concurrent_destructions: 2,
        }
    }
}

/// Logical resource usage observed by one [`super::NativeCatalog`].
///
/// Byte counters describe catalog-accounted artifact bytes, not process RSS.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct NativeCatalogSnapshot {
    /// Durable bytes, including reservations for commits in progress.
    pub cache_bytes: u64,
    /// Durable entries, including reservations for commits in progress.
    pub cache_artifacts: usize,
    /// Largest observed durable-byte total, including commit reservations.
    pub peak_cache_bytes: u64,
    /// Largest observed durable-entry total, including commit reservations.
    pub peak_cache_artifacts: usize,
    /// Bytes retained by private staging artifacts.
    pub staging_bytes: u64,
    /// Largest observed staging-byte total.
    pub peak_staging_bytes: u64,
    /// Rejected durable cache reservations.
    pub rejected_cache_admissions: u64,
    /// Rejected private staging reservations.
    pub rejected_staging_admissions: u64,
    /// Catalog loads currently holding pre-work admission.
    pub active_loads: usize,
    /// Largest observed number of admitted catalog loads.
    pub peak_loads: usize,
    /// Catalog loads rejected before staging or task creation.
    pub rejected_loads: u64,
    /// Foreign callbacks currently owning worker admission.
    pub active_callbacks: usize,
    /// Largest observed active-callback total.
    pub peak_callbacks: usize,
    /// Callbacks rejected before thread creation.
    pub rejected_callbacks: u64,
    /// Admitted native creates or instances retaining create-owned handles.
    pub active_instances: usize,
    /// Largest observed live-instance total.
    pub peak_instances: usize,
    /// Creates rejected before thread creation by the live-instance bound.
    pub rejected_instances: u64,
    /// Instance destructors queued or executing on the destruction lane.
    pub pending_instance_destructions: usize,
    /// Destructors currently executing on dedicated workers.
    pub active_destructions: usize,
    /// Largest observed active-destruction total.
    pub peak_destructions: usize,
    /// Destructors admitted to the bounded queue or awaiting worker pickup.
    pub queued_destructions: usize,
    /// Mapped-module finalizer reservations rejected before native entry.
    pub rejected_destructions: u64,
}

pub(super) struct CatalogLedger {
    limits: NativeCatalogLimits,
    state: Mutex<LedgerState>,
    peak_staging_bytes: AtomicU64,
    rejected_cache_admissions: AtomicU64,
    rejected_staging_admissions: AtomicU64,
}

#[derive(Default)]
struct LedgerState {
    cache_entries: BTreeMap<String, u64>,
    cache_bytes: u64,
    peak_cache_bytes: u64,
    peak_cache_artifacts: usize,
    staging_bytes: u64,
}

pub(super) struct LedgerSnapshot {
    pub(super) cache_bytes: u64,
    pub(super) cache_artifacts: usize,
    pub(super) peak_cache_bytes: u64,
    pub(super) peak_cache_artifacts: usize,
    pub(super) staging_bytes: u64,
    pub(super) peak_staging_bytes: u64,
    pub(super) rejected_cache_admissions: u64,
    pub(super) rejected_staging_admissions: u64,
}

impl CatalogLedger {
    pub(super) fn new(limits: NativeCatalogLimits, cache_entries: BTreeMap<String, u64>) -> Self {
        let cache_bytes = cache_entries.values().copied().sum();
        let cache_artifacts = cache_entries.len();
        Self {
            limits,
            state: Mutex::new(LedgerState {
                cache_entries,
                cache_bytes,
                peak_cache_bytes: cache_bytes,
                peak_cache_artifacts: cache_artifacts,
                staging_bytes: 0,
            }),
            peak_staging_bytes: AtomicU64::new(0),
            rejected_cache_admissions: AtomicU64::new(0),
            rejected_staging_admissions: AtomicU64::new(0),
        }
    }

    pub(super) fn reserve_staging(self: &Arc<Self>) -> StagingReservation {
        StagingReservation {
            ledger: Arc::clone(self),
            bytes: 0,
        }
    }

    pub(super) fn reserve_cache(
        self: &Arc<Self>,
        digest: &str,
        bytes: u64,
    ) -> Result<CacheReservation, LoaderError> {
        let mut state = self.state.lock().expect("catalog ledger poisoned");
        if state.cache_entries.contains_key(digest) {
            return Err(LoaderError::InvalidInput(
                "durable cache changed while its catalog lock was held".to_owned(),
            ));
        }
        let next_artifacts = state.cache_entries.len().saturating_add(1);
        if next_artifacts > self.limits.maximum_cache_artifacts {
            self.rejected_cache_admissions
                .fetch_add(1, Ordering::Relaxed);
            return Err(LoaderError::CapacityExhausted {
                resource: "cache artifacts",
                limit: u64::try_from(self.limits.maximum_cache_artifacts).unwrap_or(u64::MAX),
            });
        }
        let Some(next_bytes) = state.cache_bytes.checked_add(bytes) else {
            self.rejected_cache_admissions
                .fetch_add(1, Ordering::Relaxed);
            return Err(LoaderError::CapacityExhausted {
                resource: "cache bytes",
                limit: self.limits.maximum_cache_bytes,
            });
        };
        if next_bytes > self.limits.maximum_cache_bytes {
            self.rejected_cache_admissions
                .fetch_add(1, Ordering::Relaxed);
            return Err(LoaderError::CapacityExhausted {
                resource: "cache bytes",
                limit: self.limits.maximum_cache_bytes,
            });
        }
        state.cache_entries.insert(digest.to_owned(), bytes);
        state.cache_bytes = next_bytes;
        state.peak_cache_bytes = state.peak_cache_bytes.max(next_bytes);
        state.peak_cache_artifacts = state.peak_cache_artifacts.max(next_artifacts);
        Ok(CacheReservation {
            ledger: Arc::clone(self),
            digest: digest.to_owned(),
            bytes,
            committed: false,
        })
    }

    pub(super) fn contains_cache(&self, digest: &str) -> bool {
        self.state
            .lock()
            .expect("catalog ledger poisoned")
            .cache_entries
            .contains_key(digest)
    }

    pub(super) fn snapshot(&self) -> LedgerSnapshot {
        let state = self.state.lock().expect("catalog ledger poisoned");
        LedgerSnapshot {
            cache_bytes: state.cache_bytes,
            cache_artifacts: state.cache_entries.len(),
            peak_cache_bytes: state.peak_cache_bytes,
            peak_cache_artifacts: state.peak_cache_artifacts,
            staging_bytes: state.staging_bytes,
            peak_staging_bytes: self.peak_staging_bytes.load(Ordering::Relaxed),
            rejected_cache_admissions: self.rejected_cache_admissions.load(Ordering::Relaxed),
            rejected_staging_admissions: self.rejected_staging_admissions.load(Ordering::Relaxed),
        }
    }
}

pub(super) struct StagingReservation {
    ledger: Arc<CatalogLedger>,
    bytes: u64,
}

impl StagingReservation {
    pub(super) fn grow_to(&mut self, bytes: u64) -> Result<(), LoaderError> {
        let mut state = self.ledger.state.lock().expect("catalog ledger poisoned");
        let additional = bytes.checked_sub(self.bytes).ok_or_else(|| {
            LoaderError::InvalidInput("staging reservation cannot shrink".to_owned())
        })?;
        let Some(total) = state.staging_bytes.checked_add(additional) else {
            self.ledger
                .rejected_staging_admissions
                .fetch_add(1, Ordering::Relaxed);
            return Err(LoaderError::CapacityExhausted {
                resource: "staging bytes",
                limit: self.ledger.limits.maximum_staging_bytes,
            });
        };
        if total > self.ledger.limits.maximum_staging_bytes {
            self.ledger
                .rejected_staging_admissions
                .fetch_add(1, Ordering::Relaxed);
            return Err(LoaderError::CapacityExhausted {
                resource: "staging bytes",
                limit: self.ledger.limits.maximum_staging_bytes,
            });
        }
        state.staging_bytes = total;
        self.bytes = bytes;
        self.ledger
            .peak_staging_bytes
            .fetch_max(total, Ordering::Relaxed);
        Ok(())
    }

    pub(super) fn shrink_to(&mut self, bytes: u64) {
        debug_assert!(bytes <= self.bytes);
        let released = self.bytes - bytes;
        let mut state = self.ledger.state.lock().expect("catalog ledger poisoned");
        state.staging_bytes = state
            .staging_bytes
            .checked_sub(released)
            .expect("staging ledger underflow");
        self.bytes = bytes;
    }
}

impl Drop for StagingReservation {
    fn drop(&mut self) {
        let mut state = self.ledger.state.lock().expect("catalog ledger poisoned");
        state.staging_bytes = state
            .staging_bytes
            .checked_sub(self.bytes)
            .expect("staging ledger underflow");
    }
}

pub(super) struct CacheReservation {
    ledger: Arc<CatalogLedger>,
    digest: String,
    bytes: u64,
    committed: bool,
}

impl CacheReservation {
    pub(super) fn commit(mut self) {
        self.committed = true;
    }
}

impl Drop for CacheReservation {
    fn drop(&mut self) {
        if self.committed {
            return;
        }
        let mut state = self.ledger.state.lock().expect("catalog ledger poisoned");
        let removed = state
            .cache_entries
            .remove(&self.digest)
            .expect("pending cache reservation disappeared");
        debug_assert_eq!(removed, self.bytes);
        state.cache_bytes = state
            .cache_bytes
            .checked_sub(self.bytes)
            .expect("cache ledger underflow");
    }
}
