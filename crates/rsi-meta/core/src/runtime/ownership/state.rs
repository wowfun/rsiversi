use super::super::{
    CleanupReport, EffectHandle, EffectRecord, OwnedEffect, Owner, Runtime, RuntimeInner,
};
use super::EventRemoval;
use std::fmt;
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Weak};

#[derive(Clone)]
pub(crate) struct EventOwnership {
    pub(super) removal: Arc<EventRemoval>,
    pub(super) effect: EventEffect,
    pub(super) once_claimed: Arc<AtomicBool>,
}

#[derive(Clone)]
pub(super) enum EventEffect {
    Setup(OwnedEffect),
    Dynamic(EffectHandle),
    RegistryDynamic(RegistryEffectHandle),
}

#[derive(Clone)]
pub(super) struct RegistryEffectHandle {
    runtime: Weak<RuntimeInner>,
    owner: Owner,
    id: u64,
    record: Arc<EffectRecord>,
    executor: tokio::runtime::Handle,
}

impl RegistryEffectHandle {
    fn new(effect: &EffectHandle) -> Self {
        Self {
            runtime: Arc::downgrade(&effect.runtime.inner),
            owner: effect.owner,
            id: effect.id,
            record: Arc::clone(&effect.record),
            executor: effect.executor.clone(),
        }
    }

    pub(super) fn upgrade(&self) -> Option<EffectHandle> {
        Some(EffectHandle {
            runtime: Runtime {
                inner: self.runtime.upgrade()?,
            },
            owner: self.owner,
            id: self.id,
            record: Arc::clone(&self.record),
            executor: self.executor.clone(),
        })
    }
}

impl EventOwnership {
    pub(super) fn new(removal: Arc<EventRemoval>, effect: EventEffect) -> Self {
        Self {
            removal,
            effect,
            once_claimed: Arc::new(AtomicBool::new(false)),
        }
    }

    pub(super) fn registry_clone(&self) -> Self {
        let effect = match &self.effect {
            EventEffect::Setup(effect) => EventEffect::Setup(effect.clone()),
            EventEffect::Dynamic(effect) => {
                EventEffect::RegistryDynamic(RegistryEffectHandle::new(effect))
            }
            EventEffect::RegistryDynamic(effect) => EventEffect::RegistryDynamic(effect.clone()),
        };
        Self {
            removal: Arc::clone(&self.removal),
            effect,
            once_claimed: Arc::clone(&self.once_claimed),
        }
    }

    pub(crate) async fn withdraw_for_retirement(&self) -> bool {
        self.removal.start();
        self.removal.join().await.unwrap_or(false)
    }

    pub(crate) async fn dispose(&self) -> (CleanupReport, bool) {
        match &self.effect {
            EventEffect::Setup(effect) => {
                let retention = effect.detach();
                if retention.is_some() {
                    self.removal.claim_detached_report();
                }
                self.removal.start();
                let result = self.removal.join().await;
                drop(retention);
                (self.removal.report(&result), result.unwrap_or(false))
            }
            EventEffect::Dynamic(effect) => {
                let report = effect.dispose().await;
                let removed = self.removal.join().await.unwrap_or(false);
                (report, removed)
            }
            EventEffect::RegistryDynamic(effect) => {
                let report = if let Some(effect) = effect.upgrade() {
                    effect.dispose().await
                } else {
                    self.removal.start();
                    let result = self.removal.join().await;
                    self.removal.report(&result)
                };
                let removed = self.removal.join().await.unwrap_or(false);
                (report, removed)
            }
        }
    }

    pub(in super::super) fn retain_destructor_failure(&self, error: &str) {
        self.removal.retain_detached_failure(error);
    }

    pub(super) fn rollback_failed_publication(&self, executor: &tokio::runtime::Handle) {
        match &self.effect {
            EventEffect::Setup(effect) => {
                let retention = effect.detach();
                if retention.is_some() {
                    self.removal.claim_detached_report();
                }
                self.removal.start();
                drop(retention);
            }
            EventEffect::Dynamic(_) => {
                let ownership = self.clone();
                executor.spawn(async move {
                    ownership.dispose().await;
                });
            }
            EventEffect::RegistryDynamic(_) => {
                self.removal.start();
            }
        }
    }
}

impl fmt::Debug for EventOwnership {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("EventOwnership")
            .field("owner", &self.removal.owner())
            .finish_non_exhaustive()
    }
}
