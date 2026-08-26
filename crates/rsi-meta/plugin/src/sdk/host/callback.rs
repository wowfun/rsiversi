use super::effects::CleanupRegistry;
use super::{CallChannel, Capability, EffectTxn, HostPort, SdkError};
use crate::{CapId, STATUS_STALE_CAPABILITY, ServiceRequirement};
use std::marker::PhantomData;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

#[derive(Clone)]
pub(crate) struct CallbackScope {
    open: Arc<AtomicBool>,
}

impl CallbackScope {
    pub(crate) fn new() -> Self {
        Self {
            open: Arc::new(AtomicBool::new(true)),
        }
    }

    pub(super) fn ensure_open(&self) -> Result<(), SdkError> {
        if self.open.load(Ordering::Acquire) {
            Ok(())
        } else {
            Err(SdkError::new(
                STATUS_STALE_CAPABILITY,
                "native callback is sealed",
            ))
        }
    }

    pub(crate) fn seal(&self) {
        self.open.store(false, Ordering::Release);
    }

    pub(crate) fn guard(&self) -> CallbackSeal<'_> {
        CallbackSeal { scope: self }
    }
}

pub(crate) struct CallbackSeal<'a> {
    scope: &'a CallbackScope,
}

impl Drop for CallbackSeal<'_> {
    fn drop(&mut self) {
        self.scope.seal();
    }
}

/// Host operations bound to one native callback lifetime.
#[derive(Clone)]
pub struct Host<'callback> {
    pub(super) port: HostPort,
    pub(super) scope: CallbackScope,
    pub(super) authority: CapId,
    pub(super) lifetime: PhantomData<&'callback mut ()>,
}

impl<'callback> Host<'callback> {
    pub fn open(&self, capability: &Capability) -> Result<CallChannel<'callback>, SdkError> {
        self.scope.ensure_open()?;
        if capability.port.issuer() != self.port.issuer() {
            return Err(SdkError::new(
                crate::STATUS_WRONG_CAPABILITY,
                "capability belongs to another host",
            ));
        }
        let channel = self.port.open(self.authority, capability.id)?;
        Ok(CallChannel::new(self.port, self.scope.clone(), channel))
    }
}

impl std::fmt::Debug for Host<'_> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.debug_struct("Host").finish_non_exhaustive()
    }
}

pub(crate) struct Injection {
    pub(crate) requirement: ServiceRequirement,
    pub(crate) capability: Capability,
}

/// Safe activation view containing exact injections and one setup transaction.
pub struct Activation<'callback> {
    pub(super) host: Host<'callback>,
    pub(super) effects: EffectTxn<'callback>,
    pub(super) injections: Vec<Injection>,
}

impl<'callback> Activation<'callback> {
    pub fn host(&self) -> Host<'_> {
        self.host.clone()
    }

    pub fn injection(&self, key: &str) -> Option<&Capability> {
        self.injections
            .iter()
            .find(|injection| injection.requirement.key() == key)
            .map(|injection| &injection.capability)
    }

    pub fn effects(&mut self) -> &mut EffectTxn<'callback> {
        &mut self.effects
    }
}

impl std::fmt::Debug for Activation<'_> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("Activation")
            .field("injection_count", &self.injections.len())
            .field("effects", &self.effects)
            .finish()
    }
}

pub(crate) fn activation<'callback>(
    port: HostPort,
    scope: &'callback CallbackScope,
    transaction: CapId,
    registry: &'callback dyn CleanupRegistry,
    injections: Vec<Injection>,
) -> Activation<'callback> {
    Activation {
        host: Host {
            port,
            scope: scope.clone(),
            authority: transaction,
            lifetime: PhantomData,
        },
        effects: EffectTxn::new(port, scope, transaction, registry),
        injections,
    }
}

impl Activation<'_> {
    pub(crate) const fn committed(&self) -> bool {
        self.effects.committed()
    }

    pub(crate) const fn commit_requested(&self) -> bool {
        self.effects.commit_requested()
    }

    pub(crate) fn finish_commit(&mut self) -> Result<(), SdkError> {
        self.effects.finish_commit()
    }
}
