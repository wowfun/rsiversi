use crate::{ProfileError, Result};
use std::sync::{Arc, Mutex};
use tokio::sync::watch;

const MAX_LEASES: usize = 256;

/// Embedder-owned permission to observe source files automatically.
#[derive(Clone, Debug)]
pub struct SourceWatchPolicy {
    state: Arc<State>,
    leased: bool,
}
#[derive(Debug)]
struct State {
    count: Mutex<usize>,
    enabled: watch::Sender<bool>,
}
impl PartialEq for SourceWatchPolicy {
    fn eq(&self, other: &Self) -> bool {
        if self.leased || other.leased {
            Arc::ptr_eq(&self.state, &other.state)
        } else {
            self.enabled() == other.enabled()
        }
    }
}
impl From<bool> for SourceWatchPolicy {
    fn from(enabled: bool) -> Self {
        Self::new(enabled, false)
    }
}
impl SourceWatchPolicy {
    fn new(enabled: bool, leased: bool) -> Self {
        let (enabled, _) = watch::channel(enabled);
        Self {
            state: Arc::new(State {
                count: Mutex::new(0),
                enabled,
            }),
            leased,
        }
    }
    /// Creates an initially inactive authority, enabled only by retained leases.
    #[must_use]
    pub fn leased() -> Self {
        Self::new(false, true)
    }
    /// Whether automatic observation may currently admit work.
    #[must_use]
    pub fn enabled(&self) -> bool {
        *self.state.enabled.borrow()
    }
    /// Observes transitions without creating a worker, clock or runtime.
    #[must_use]
    pub fn subscribe(&self) -> watch::Receiver<bool> {
        self.state.enabled.subscribe()
    }
    /// Acquires one of at most 256 leases; dropping it is safe outside a runtime.
    ///
    /// # Panics
    /// Panics if a previous panic poisoned the lease-count mutex.
    pub fn acquire(&self) -> Result<SourceWatchLease> {
        if !self.leased {
            return Err(ProfileError::InvalidProgram("watch policy is fixed".into()));
        }
        let mut count = self.state.count.lock().expect("source watch lease count");
        if *count == MAX_LEASES {
            return Err(ProfileError::CapacityExceeded {
                resource: "source watch leases",
                maximum: MAX_LEASES,
            });
        }
        *count += 1;
        if *count == 1 {
            self.state.enabled.send_replace(true);
        }
        Ok(SourceWatchLease(self.state.clone()))
    }
    pub(crate) fn can_watch(&self) -> bool {
        self.leased || self.enabled()
    }
}
/// One observation owner's lifetime. Cloning a policy does not acquire a lease.
#[derive(Debug)]
pub struct SourceWatchLease(Arc<State>);
impl Drop for SourceWatchLease {
    fn drop(&mut self) {
        let mut count = self.0.count.lock().expect("source watch lease count");
        *count -= 1;
        if *count == 0 {
            self.0.enabled.send_replace(false);
        }
    }
}
