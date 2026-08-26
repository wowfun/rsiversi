use super::{Cleanup, ConfigValue, PreparedState};
use crate::{Capability, Context, MetaError, Result, ServiceKey};
use std::collections::BTreeMap;
use std::fmt;
use std::sync::Arc;

/// Single-use activation input with exact generation-fenced injections and
/// optional opaque attempt-local state.
pub struct ActivationPlan {
    context: Context,
    config: Arc<ConfigValue>,
    inject: BTreeMap<ServiceKey, Capability>,
    state: Option<PreparedState>,
}

impl fmt::Debug for ActivationPlan {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ActivationPlan")
            .field("owner", &self.context.owner())
            .field("lineage_call_id", &self.lineage_call_id())
            .field("inject", &self.inject.keys().collect::<Vec<_>>())
            .field("state", &self.state.as_ref().map(|_| "<redacted>"))
            .finish_non_exhaustive()
    }
}

impl ActivationPlan {
    pub(crate) fn new(
        context: Context,
        config: Arc<ConfigValue>,
        inject: BTreeMap<ServiceKey, Capability>,
        state: Option<PreparedState>,
    ) -> Self {
        debug_assert!(context.activation_lineage().is_some());
        Self {
            context,
            config,
            inject,
            state,
        }
    }

    /// Returns the generation Context owned by this activation.
    pub fn context(&self) -> &Context {
        &self.context
    }

    /// Returns the Runtime-issued nonzero root call identity for every service
    /// or event call opened from this activation Context.
    pub fn lineage_call_id(&self) -> crate::CallId {
        self.context
            .activation_lineage()
            .expect("an activation plan always carries a root call lineage")
    }

    /// Returns the normalized configuration for this activation.
    pub fn config(&self) -> &Arc<ConfigValue> {
        &self.config
    }

    /// Returns one exact injected service capability.
    pub fn inject(&self, key: impl AsRef<str>) -> Option<&Capability> {
        self.inject.get(&ServiceKey::new(key.as_ref()))
    }

    /// Takes the opaque prepared state as `T` exactly once.
    ///
    /// A wrong type returns [`MetaError::PreparedStateTypeMismatch`] without
    /// consuming the state, so the factory may retry with the correct type.
    /// Taking the value does not release its declared retained-byte charge;
    /// that conservative charge remains until the activation attempt retires.
    pub fn take_state<T>(&mut self) -> Result<T>
    where
        T: Send + 'static,
    {
        let result = self
            .state
            .as_mut()
            .ok_or(MetaError::PreparedStateUnavailable)?
            .take::<T>();
        if result.is_ok() {
            self.state = None;
        }
        result
    }

    /// Registers one setup undo in this attempt's Runtime-owned root transaction.
    pub fn defer(&self, label: impl Into<String>, cleanup: Cleanup) -> Result<()> {
        self.context.defer(label, cleanup)
    }
}
