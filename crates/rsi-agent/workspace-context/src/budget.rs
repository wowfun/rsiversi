use super::*;
use std::sync::{LazyLock, Mutex};
use tokio::sync::{Notify, OwnedSemaphorePermit, Semaphore};
use tokio_util::sync::CancellationToken;

pub(super) const JOB_BYTES: usize = 16 * 1024 * 1024;
// Covers both bounded rendered texts, selected instruction sections, one raw
// source plus invocation rendering, metadata parse scratch, and collection slots.
const SCRATCH_BYTES: usize = 4 * 1024 * 1024;
static PROCESS_LANES: LazyLock<Arc<Semaphore>> = LazyLock::new(|| Arc::new(Semaphore::new(4)));

#[derive(Debug, Default)]
pub(super) struct SnapshotOwner {
    state: Mutex<OwnerState>,
    changed: Notify,
    pub(super) cancellation: CancellationToken,
}

#[derive(Debug, Default)]
struct OwnerState {
    closed: bool,
    jobs: usize,
}

pub(super) struct JobLease {
    _permit: OwnedSemaphorePermit,
    owner: Arc<SnapshotOwner>,
}

impl SnapshotOwner {
    pub(super) async fn acquire(self: &Arc<Self>) -> Result<JobLease, WorkspaceContextError> {
        let permit = tokio::select! {
            biased;
            () = self.cancellation.cancelled() => return Err(WorkspaceContextError::Closed),
            permit = Arc::clone(&PROCESS_LANES).acquire_owned() => permit.map_err(|_| WorkspaceContextError::Closed)?,
        };
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if state.closed {
            return Err(WorkspaceContextError::Closed);
        }
        state.jobs += 1;
        Ok(JobLease {
            _permit: permit,
            owner: Arc::clone(self),
        })
    }

    pub(super) async fn close(&self) {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .closed = true;
        self.cancellation.cancel();
        loop {
            let changed = self.changed.notified();
            tokio::pin!(changed);
            changed.as_mut().enable();
            if self
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .jobs
                == 0
            {
                break;
            }
            changed.await;
        }
    }
}

impl Drop for JobLease {
    fn drop(&mut self) {
        self.owner
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .jobs -= 1;
        self.owner.changed.notify_waiters();
    }
}

pub(super) struct SnapshotBudget {
    used: usize,
    pub(super) cancellation: CancellationToken,
}
impl SnapshotBudget {
    pub(super) fn new(
        config: &WorkspaceContextConfig,
        cwd: &Path,
        cancellation: CancellationToken,
    ) -> Result<Self, WorkspaceContextError> {
        let mut budget = Self {
            used: SCRATCH_BYTES,
            cancellation,
        };
        budget.reserve(
            config_retained_bytes(config)?
                .checked_add(
                    cwd.as_os_str()
                        .len()
                        .checked_mul(140)
                        .ok_or(WorkspaceContextError::Capacity)?,
                )
                .ok_or(WorkspaceContextError::Capacity)?,
        )?;
        // Bound all borrowed message tokens and their deduplication before copying.
        budget.reserve(4096 * (64 * 2 + std::mem::size_of::<String>() * 3))?;
        Ok(budget)
    }
    pub(super) fn reserve(&mut self, bytes: usize) -> Result<(), WorkspaceContextError> {
        self.check()?;
        self.used = self
            .used
            .checked_add(bytes)
            .filter(|used| *used <= JOB_BYTES)
            .ok_or(WorkspaceContextError::Capacity)?;
        Ok(())
    }
    pub(super) fn release(&mut self, bytes: usize) {
        self.used = self
            .used
            .checked_sub(bytes)
            .expect("release cannot exceed reserved snapshot bytes");
    }
    pub(super) fn check(&self) -> Result<(), WorkspaceContextError> {
        if self.cancellation.is_cancelled() {
            Err(WorkspaceContextError::Closed)
        } else {
            Ok(())
        }
    }
}

pub(super) fn config_retained_bytes(
    config: &WorkspaceContextConfig,
) -> Result<usize, WorkspaceContextError> {
    config
        .user_instruction_file
        .iter()
        .chain(config.user_skill_roots.iter())
        .try_fold(
            std::mem::size_of::<WorkspaceContextConfig>(),
            |bytes, path| {
                bytes
                    .checked_add(path.as_os_str().len())
                    .and_then(|bytes| bytes.checked_add(std::mem::size_of::<PathBuf>()))
                    .filter(|bytes| *bytes <= JOB_BYTES)
                    .ok_or(WorkspaceContextError::Capacity)
            },
        )
}

impl JobLease {
    pub(super) async fn run<T: Send + 'static>(
        self,
        operation: impl FnOnce() -> Result<T, WorkspaceContextError> + Send + 'static,
    ) -> Result<T, WorkspaceContextError> {
        let (result, lease) = tokio::task::spawn_blocking(move || (operation(), self))
            .await
            .map_err(|error| WorkspaceContextError::Failed(error.to_string()))?;
        drop(lease);
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn cancelled_waiters_and_withdrawn_generations_retain_actual_process_jobs() {
        let old = Arc::new(SnapshotOwner::default());
        let new = Arc::new(SnapshotOwner::default());
        let mut releases = Vec::new();
        for _ in 0..4 {
            let lease = old.acquire().await.unwrap();
            let (entered_tx, entered_rx) = tokio::sync::oneshot::channel();
            let (release_tx, release_rx) = std::sync::mpsc::channel();
            let waiter = tokio::spawn(lease.run(move || {
                entered_tx.send(()).unwrap();
                release_rx.recv().unwrap();
                Ok(())
            }));
            entered_rx.await.unwrap();
            waiter.abort();
            assert!(waiter.await.unwrap_err().is_cancelled());
            releases.push(release_tx);
        }
        let fifth = new.acquire();
        tokio::pin!(fifth);
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(10), &mut fifth)
                .await
                .is_err()
        );
        let close = old.close();
        tokio::pin!(close);
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(10), &mut close)
                .await
                .is_err()
        );
        assert!(old.cancellation.is_cancelled());
        assert_eq!(old.state.lock().unwrap().jobs, 4);
        releases.pop().unwrap().send(()).unwrap();
        let lease = tokio::time::timeout(std::time::Duration::from_secs(2), fifth)
            .await
            .unwrap()
            .unwrap();
        drop(lease);
        for release in releases {
            release.send(()).unwrap();
        }
        tokio::time::timeout(std::time::Duration::from_secs(2), close)
            .await
            .unwrap();
        assert!(matches!(
            old.acquire().await,
            Err(WorkspaceContextError::Closed)
        ));
        assert_eq!(old.state.lock().unwrap().jobs, 0);
    }
}
