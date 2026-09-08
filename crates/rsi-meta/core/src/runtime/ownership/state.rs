use super::super::{
    CleanupReport, EffectHandle, EffectRecord, OwnedEffect, Owner, Runtime, RuntimeInner,
};
use super::RegistrationRemoval;
use std::fmt;
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Weak};

#[derive(Clone)]
pub(crate) struct RegistrationOwnership {
    pub(in crate::runtime) removal: Arc<RegistrationRemoval>,
    pub(super) effect: RegistrationEffect,
    pub(super) once_claimed: Arc<AtomicBool>,
}

#[derive(Clone)]
pub(in crate::runtime) enum RegistrationEffect {
    Setup(OwnedEffect),
    Dynamic(EffectHandle),
    RegistryDynamic(RegistryEffectHandle),
}

#[derive(Clone)]
pub(in crate::runtime) struct RegistryEffectHandle {
    runtime: Weak<RuntimeInner>,
    owner: Owner,
    id: u64,
    record: Arc<EffectRecord>,
    executor: crate::Execution,
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

impl RegistrationOwnership {
    pub(in crate::runtime) fn new(
        removal: Arc<RegistrationRemoval>,
        effect: RegistrationEffect,
    ) -> Self {
        Self {
            removal,
            effect,
            once_claimed: Arc::new(AtomicBool::new(false)),
        }
    }

    pub(in crate::runtime) fn retire_registration(&self, executor: &crate::Execution) {
        self.removal.start();
        match &self.effect {
            RegistrationEffect::RegistryDynamic(effect) => {
                if let Some(effect) = effect.upgrade() {
                    executor.spawn(async move {
                        effect.dispose().await;
                    });
                }
            }
            _ => self.rollback_failed_publication(executor),
        }
    }

    pub(in crate::runtime) fn registry_clone(&self) -> Self {
        let effect = match &self.effect {
            RegistrationEffect::Setup(effect) => RegistrationEffect::Setup(effect.clone()),
            RegistrationEffect::Dynamic(effect) => {
                RegistrationEffect::RegistryDynamic(RegistryEffectHandle::new(effect))
            }
            RegistrationEffect::RegistryDynamic(effect) => {
                RegistrationEffect::RegistryDynamic(effect.clone())
            }
        };
        Self {
            removal: Arc::clone(&self.removal),
            effect,
            once_claimed: Arc::clone(&self.once_claimed),
        }
    }

    pub(crate) async fn dispose(&self) -> (CleanupReport, bool) {
        match &self.effect {
            RegistrationEffect::Setup(effect) => {
                let retention = effect.detach();
                if retention.is_some() {
                    self.removal.claim_detached_report();
                }
                self.removal.start();
                let result = self.removal.join().await;
                drop(retention);
                (self.removal.report(&result), result.unwrap_or(false))
            }
            RegistrationEffect::Dynamic(effect) => {
                let report = effect.dispose().await;
                let removed = self.removal.join().await.unwrap_or(false);
                (report, removed)
            }
            RegistrationEffect::RegistryDynamic(effect) => {
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

    pub(in crate::runtime) fn rollback_failed_publication(&self, executor: &crate::Execution) {
        match &self.effect {
            RegistrationEffect::Setup(effect) => {
                let retention = effect.detach();
                if retention.is_some() {
                    self.removal.claim_detached_report();
                }
                self.removal.start();
                drop(retention);
            }
            RegistrationEffect::Dynamic(_) => {
                let ownership = self.clone();
                executor.spawn(async move {
                    ownership.dispose().await;
                });
            }
            RegistrationEffect::RegistryDynamic(_) => {
                self.removal.start();
            }
        }
    }
}

impl fmt::Debug for RegistrationOwnership {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RegistrationOwnership")
            .field("owner", &self.removal.owner())
            .finish_non_exhaustive()
    }
}
