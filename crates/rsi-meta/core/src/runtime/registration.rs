use super::ownership::{RegistrationEffect, RegistrationRemoval};
use super::{ChildPosition, CleanupReport, Context, MetaError, RegistrationOwnership, Result};
use std::fmt;
use std::sync::Arc;
use std::sync::atomic::Ordering;

/// Narrow authority for a Local registrar to own one exact generation's effects.
/// It has no service, child-application, or Portable invocation authority.
#[derive(Clone, Debug)]
pub struct RegistrationContext {
    context: Context,
}

/// Immutable registration identity and position; it grants no mutation authority.
#[derive(Clone)]
pub struct RegistrationPosition {
    pub(super) position: ChildPosition,
    pub(super) sequence: u64,
    removal: Arc<RegistrationRemoval>,
}
impl fmt::Debug for RegistrationPosition {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RegistrationPosition")
            .field("position", &self.position)
            .field("sequence", &self.sequence)
            .field("admitting", &self.is_admitting())
            .finish_non_exhaustive()
    }
}
impl RegistrationPosition {
    /// Whether the exact contribution still admits newly captured work.
    /// A registrar checks this when capturing a dispatch or admitting an action.
    pub fn is_admitting(&self) -> bool {
        self.removal.is_admitting()
    }
}

/// Exact effect-owned contribution lease. Drop closes admission and removes the
/// contribution; explicit disposal also joins its effect accounting and report.
#[derive(Debug)]
pub struct RegistrationLease {
    ownership: RegistrationOwnership,
    execution: crate::Execution,
}
impl RegistrationLease {
    /// Closes this contribution and joins the same cleanup as generation retirement.
    pub async fn dispose(&self) -> CleanupReport {
        self.ownership.removal.start();
        self.ownership.dispose().await.0
    }
}
impl Drop for RegistrationLease {
    fn drop(&mut self) {
        self.ownership.retire_registration(&self.execution);
    }
}

impl Context {
    /// Issues a narrow Local registration credential for this exact generation.
    /// Root and stale owners are rejected.
    pub fn registration_context(&self) -> Result<RegistrationContext> {
        self.registration_position()?;
        Ok(RegistrationContext {
            context: self.clone(),
        })
    }

    fn registration_position(&self) -> Result<ChildPosition> {
        let _admission = self.runtime.begin_admission(false)?;
        self.with_live_position(|position| {
            position.ok_or_else(|| {
                MetaError::InvalidInput("the root context cannot own a registration".into())
            })
        })
    }

    pub(super) fn own_registration(
        &self,
        removal: &Arc<RegistrationRemoval>,
        label: String,
    ) -> Result<RegistrationOwnership> {
        self.runtime.validate_effect_label(&label)?;
        let owner = self.owner.ok_or_else(|| {
            MetaError::InvalidInput("the root context cannot own a registration".into())
        })?;
        if let Some(setup) = self.setup_effect.as_ref().filter(|setup| setup.is_open()) {
            let effect = setup.defer_owned(label, removal.cleanup())?;
            Ok(RegistrationOwnership::new(
                removal.clone(),
                RegistrationEffect::Setup(effect),
            ))
        } else {
            self.runtime.ensure_dynamic_effect_owner(owner)?;
            let mut transaction = self.runtime.begin_effect(owner, label.clone())?;
            transaction.defer(label, removal.cleanup())?;
            Ok(RegistrationOwnership::new(
                removal.clone(),
                RegistrationEffect::Dynamic(transaction.commit()?),
            ))
        }
    }
}

impl RegistrationContext {
    /// Identifies the Runtime whose exact generation owns this registration.
    pub fn runtime_identity(&self) -> super::RuntimeIdentity {
        self.context.runtime_identity()
    }

    /// Installs exact undo before publishing one Local contribution.
    ///
    /// The synchronous publication closure only performs the bounded registrar
    /// insertion. It must not call plugin code, reenter Meta, or wait. The undo
    /// closure removes only that exact insertion; failures are bounded cleanup
    /// evidence. Business callbacks execute outside this publication operation.
    /// Loading joins setup rollback; Active installs a dynamic effect.
    pub fn register<T>(
        &self,
        label: impl Into<String>,
        undo: impl FnOnce() -> std::result::Result<(), String> + Send + 'static,
        publish: impl FnOnce(RegistrationPosition) -> Result<T>,
    ) -> Result<(T, RegistrationLease)> {
        let position = self.context.registration_position()?;
        let runtime = &self.context.runtime;
        let _admission = runtime.begin_admission(false)?;
        let owner = self
            .context
            .owner
            .expect("registration credential has an exact owner");
        let sequence = runtime
            .inner
            .next_registration
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |value| {
                value.checked_add(1)
            })
            .map_err(|_| MetaError::CapacityExhausted {
                resource: "registration identities",
            })?
            + 1;
        let label = label.into();
        runtime.validate_effect_label(&label)?;
        let removal = RegistrationRemoval::for_local(runtime, owner, label.clone(), Box::new(undo));
        let ownership = self.context.own_registration(&removal, label)?;
        // The returned lease can be retained by a Runtime-owned plugin object:
        // it must not close a structural strong Runtime cycle.
        let lease = RegistrationLease {
            ownership: ownership.registry_clone(),
            execution: runtime.execution().clone(),
        };
        let result = removal.publish(|| {
            self.context.registration_position()?;
            publish(RegistrationPosition {
                position,
                sequence,
                removal: removal.clone(),
            })
        })?;
        Ok((result, lease))
    }
}
