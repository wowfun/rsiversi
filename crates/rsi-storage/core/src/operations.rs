use crate::{BackendLease, Result, StorageError};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use tokio::sync::{Mutex, OwnedMutexGuard};

/// Owned serialization and generation health for blocking backend operations.
#[derive(Debug, Default)]
pub struct BackendOperations {
    slot: Arc<Mutex<()>>,
    admission: std::sync::Mutex<Admission>,
    recovery_required: AtomicBool,
}

#[derive(Debug, Default)]
struct Admission {
    closed: bool,
    active: bool,
    registration: Option<BackendLease>,
}

/// Withdraws a backend only after actual admitted work has finished.
#[derive(Debug)]
pub struct BackendRegistration(Arc<BackendOperations>);
impl BackendRegistration {
    /// Closes admission and drains work. Dropping this future also closes admission.
    pub async fn close(self) {
        self.0.close().await;
    }
}
impl Drop for BackendRegistration {
    fn drop(&mut self) {
        self.0.close_admission();
    }
}

impl BackendOperations {
    /// Attaches the registration before installing its generation cleanup callback.
    ///
    /// # Panics
    /// Panics if this generation already has a registration or is closed.
    pub fn retain_registration(self: &Arc<Self>, lease: BackendLease) -> BackendRegistration {
        let mut admission = self
            .admission
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        assert!(
            admission.registration.is_none() && !admission.closed,
            "backend registration already attached or closed"
        );
        admission.registration = Some(lease);
        BackendRegistration(Arc::clone(self))
    }

    fn close_admission(&self) {
        let mut admission = self
            .admission
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        admission.closed = true;
        let registration = if admission.active {
            None
        } else {
            admission.registration.take()
        };
        drop(admission);
        drop(registration);
    }

    /// Checks local health, without accessing the durable medium.
    pub fn ensure_available(&self) -> Result<()> {
        if self.recovery_required.load(Ordering::Acquire) {
            Err(StorageError::RecoveryRequired)
        } else if self
            .admission
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .closed
        {
            Err(StorageError::BackendUnavailable("closed generation".into()))
        } else {
            Ok(())
        }
    }

    /// Acquires serialization before reserving resources belonging to one I/O
    /// operation. Dispatch rechecks generation admission after this acquisition.
    pub async fn acquire(self: &Arc<Self>) -> Result<BackendSlot> {
        self.ensure_available()?;
        let slot = Arc::clone(&self.slot).lock_owned().await;
        self.ensure_available()?;
        Ok(BackendSlot {
            owner: Arc::clone(self),
            slot,
        })
    }

    /// Acquires admission before dispatch; admitted work survives waiter cancellation.
    pub async fn run<T, F>(self: &Arc<Self>, work: F) -> Result<T>
    where
        T: Send + 'static,
        F: FnOnce() -> Result<T> + Send + 'static,
    {
        self.acquire().await?.run(work).await
    }

    /// Permanently closes admission and waits for the admitted worker to finish.
    pub async fn close(&self) {
        self.close_admission();
        let _slot = self.slot.lock().await;
    }
}

/// A serialized backend operation slot, acquired before unit-name reservation.
/// Dropping an undispatched slot has no durable effect.
#[derive(Debug)]
pub struct BackendSlot {
    owner: Arc<BackendOperations>,
    slot: OwnedMutexGuard<()>,
}
impl BackendSlot {
    /// Dispatches one cancellation-owned blocking operation after checking health.
    pub async fn run<T, F>(self, work: F) -> Result<T>
    where
        T: Send + 'static,
        F: FnOnce() -> Result<T> + Send + 'static,
    {
        {
            let mut admission = self
                .owner
                .admission
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if self.owner.recovery_required.load(Ordering::Acquire) {
                return Err(StorageError::RecoveryRequired);
            }
            if admission.closed {
                return Err(StorageError::BackendUnavailable("closed generation".into()));
            }
            admission.active = true;
        }
        let completion = Completion {
            owner: Arc::clone(&self.owner),
            finished: false,
        };
        tokio::task::spawn_blocking(move || {
            let _slot = self.slot;
            let mut completion = completion;
            let result = work();
            if result.as_ref().is_err_and(StorageError::requires_recovery) {
                completion
                    .owner
                    .recovery_required
                    .store(true, Ordering::Release);
            }
            completion.finished = true;
            result
        })
        .await
        .map_err(|error| {
            StorageError::OutcomeUnknown(format!("storage worker did not complete: {error}"))
        })?
    }
}

struct Completion {
    owner: Arc<BackendOperations>,
    finished: bool,
}
impl Drop for Completion {
    fn drop(&mut self) {
        if !self.finished {
            self.owner.recovery_required.store(true, Ordering::Release);
        }
        let mut admission = self
            .owner
            .admission
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        admission.active = false;
        let registration = if admission.closed {
            admission.registration.take()
        } else {
            None
        };
        drop(admission);
        drop(registration);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        future::Future,
        pin::pin,
        task::{Context, Poll, Waker},
    };

    #[derive(Debug, Default)]
    struct UnusedBackend {
        drop_probe: Option<(std::sync::Weak<BackendOperations>, Arc<AtomicBool>)>,
    }
    impl Drop for UnusedBackend {
        fn drop(&mut self) {
            if let Some((owner, unlocked)) = &self.drop_probe {
                unlocked.store(
                    owner.upgrade().unwrap().admission.try_lock().is_ok(),
                    Ordering::SeqCst,
                );
            }
        }
    }
    #[async_trait::async_trait]
    impl crate::KvBackend for UnusedBackend {
        fn ensure_available(&self) -> Result<()> {
            Ok(())
        }
        fn capabilities(&self, _: crate::Layout) -> crate::Capabilities {
            crate::Capabilities::default()
        }
        async fn open(&self, _: crate::UnitDescriptor) -> crate::Result<crate::OpenedUnit> {
            unreachable!()
        }
    }

    #[tokio::test]
    async fn dropped_cleanup_retains_registration_until_the_worker_finishes() {
        use crate::StorageHub as _;
        for polled in [false, true] {
            let hub = crate::Hub::new();
            let backend = Arc::new(UnusedBackend::default());
            let owner = Arc::new(BackendOperations::default());
            let registration =
                owner.retain_registration(hub.register("owned", backend.clone()).unwrap());
            let (entered, started) = tokio::sync::oneshot::channel();
            let (release, released) = tokio::sync::oneshot::channel();
            let worker = tokio::spawn({
                let owner = owner.clone();
                async move {
                    owner
                        .run(move || {
                            entered.send(()).unwrap();
                            released.blocking_recv().unwrap();
                            Ok(())
                        })
                        .await
                }
            });
            started.await.unwrap();
            {
                let mut cleanup = pin!(registration.close());
                if polled {
                    assert!(matches!(
                        cleanup
                            .as_mut()
                            .poll(&mut Context::from_waker(Waker::noop())),
                        Poll::Pending
                    ));
                }
            }
            assert!(matches!(
                owner.ensure_available(),
                Err(StorageError::BackendUnavailable(_))
            ));
            assert!(hub.register("owned", backend.clone()).is_err());
            release.send(()).unwrap();
            worker.await.unwrap().unwrap();
            assert!(hub.resolve("owned").is_err());
            let _replacement = hub.register("owned", backend).unwrap();
        }
    }

    #[tokio::test]
    async fn withdrawal_drops_the_backend_outside_admission_for_idle_and_active_owners() {
        use crate::StorageHub as _;
        for active in [false, true] {
            let hub = crate::Hub::new();
            let owner = Arc::new(BackendOperations::default());
            let unlocked = Arc::new(AtomicBool::new(false));
            let backend = Arc::new(UnusedBackend {
                drop_probe: Some((Arc::downgrade(&owner), unlocked.clone())),
            });
            let registration = owner.retain_registration(hub.register("probe", backend).unwrap());
            if active {
                let (entered, started) = tokio::sync::oneshot::channel();
                let (release, released) = tokio::sync::oneshot::channel();
                let worker = tokio::spawn({
                    let owner = owner.clone();
                    async move {
                        owner
                            .run(move || {
                                entered.send(()).unwrap();
                                released.blocking_recv().unwrap();
                                Ok(())
                            })
                            .await
                    }
                });
                tokio::time::timeout(std::time::Duration::from_secs(2), started)
                    .await
                    .unwrap()
                    .unwrap();
                drop(registration);
                assert!(!unlocked.load(Ordering::SeqCst));
                release.send(()).unwrap();
                tokio::time::timeout(std::time::Duration::from_secs(2), worker)
                    .await
                    .unwrap()
                    .unwrap()
                    .unwrap();
            } else {
                drop(registration);
            }
            assert!(
                unlocked.load(Ordering::SeqCst),
                "backend destructor must be able to reenter admission"
            );
        }
    }

    #[tokio::test]
    async fn cancelled_waiters_retain_worker_slot_and_close_drains_it() {
        let owner = Arc::new(BackendOperations::default());
        let (entered, started) = tokio::sync::oneshot::channel();
        let (release, released) = tokio::sync::oneshot::channel();
        let first = tokio::spawn({
            let owner = owner.clone();
            async move {
                owner
                    .run(move || {
                        entered.send(()).unwrap();
                        released.blocking_recv().unwrap();
                        Ok(())
                    })
                    .await
            }
        });
        started.await.unwrap();
        first.abort();
        assert!(first.await.unwrap_err().is_cancelled());
        {
            let mut queued =
                pin!(owner.run(|| -> Result<()> { panic!("cancelled queue entry dispatched") }));
            assert!(matches!(
                queued
                    .as_mut()
                    .poll(&mut Context::from_waker(Waker::noop())),
                Poll::Pending
            ));
        }
        let mut close = pin!(owner.close());
        assert!(matches!(
            close.as_mut().poll(&mut Context::from_waker(Waker::noop())),
            Poll::Pending
        ));
        assert!(matches!(
            owner.ensure_available(),
            Err(StorageError::BackendUnavailable(_))
        ));
        release.send(()).unwrap();
        close.await;
        assert!(matches!(
            owner.run(|| Ok(())).await,
            Err(StorageError::BackendUnavailable(_))
        ));
    }

    #[tokio::test]
    async fn unknown_outcome_and_worker_panic_fence_before_next_operation() {
        for panic in [false, true] {
            let owner = Arc::new(BackendOperations::default());
            assert!(matches!(
                owner
                    .run(move || -> Result<()> {
                        assert!(!panic, "injected worker failure");
                        Err(StorageError::OutcomeUnknown(
                            "injected durability failure".into(),
                        ))
                    })
                    .await,
                Err(StorageError::OutcomeUnknown(_))
            ));
            assert_eq!(
                owner.run(|| Ok(())).await,
                Err(StorageError::RecoveryRequired)
            );
        }
        let fenced = Arc::new(BackendOperations::default());
        assert_eq!(
            fenced
                .run(|| -> Result<()> { Err(StorageError::RecoveryRequired) })
                .await,
            Err(StorageError::RecoveryRequired)
        );
        assert_eq!(
            fenced.run(|| Ok(())).await,
            Err(StorageError::RecoveryRequired)
        );
        let owner = Arc::new(BackendOperations::default());
        assert!(
            owner
                .run(|| -> Result<()> { Err(StorageError::Io("before commit".into())) })
                .await
                .is_err()
        );
        owner.run(|| Ok(())).await.unwrap();
    }
}
