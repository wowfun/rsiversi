use super::super::{EffectHandle, EffectRetention};
use super::{RegistrationEffect, RegistrationOwnership, RegistrationRemoval};
use std::sync::Arc;
use std::sync::atomic::Ordering;

impl RegistrationOwnership {
    pub(in crate::runtime) fn begin_once_claim(&self) -> Option<OnceClaim> {
        if self
            .once_claimed
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return None;
        }
        match &self.effect {
            RegistrationEffect::Setup(effect) => {
                let retention = effect.detach()?;
                self.removal.claim_detached_report();
                self.removal.start();
                Some(OnceClaim::Setup {
                    removal: Arc::clone(&self.removal),
                    retention,
                })
            }
            RegistrationEffect::Dynamic(effect) => {
                self.removal.start();
                Some(OnceClaim::Dynamic {
                    removal: Arc::clone(&self.removal),
                    effect: effect.clone(),
                })
            }
            RegistrationEffect::RegistryDynamic(effect) => {
                let effect = effect.upgrade()?;
                self.removal.start();
                Some(OnceClaim::Dynamic {
                    removal: Arc::clone(&self.removal),
                    effect,
                })
            }
        }
    }
}

pub(in crate::runtime) enum OnceClaim {
    Setup {
        removal: Arc<RegistrationRemoval>,
        retention: EffectRetention,
    },
    Dynamic {
        removal: Arc<RegistrationRemoval>,
        effect: EffectHandle,
    },
}

impl OnceClaim {
    pub(in crate::runtime) async fn finish(self) -> bool {
        match self {
            Self::Setup { removal, retention } => {
                let result = removal.join().await;
                drop(retention);
                result.unwrap_or(false)
            }
            Self::Dynamic { removal, effect } => {
                if effect.try_dispose_explicit().await.is_none() {
                    return false;
                }
                removal.join().await.unwrap_or(false)
            }
        }
    }
}
