use crate::Result;
use async_trait::async_trait;
use futures_util::future::BoxFuture;
use std::{fmt, sync::Arc};

#[cfg(test)]
use serde_json::Value;

mod activation_plan;
mod metadata;
mod prepared_activation;
pub use activation_plan::ActivationPlan;
pub use metadata::Requirement;
pub use prepared_activation::PreparedActivation;
pub(crate) use prepared_activation::{LocalRequirement, PreparedState};
pub use rsi_meta_contract::{
    ConfigValue, FactoryIdentity, InstanceId, LocalContract, LocalContractKey, PluginId, UpdateMode,
};

/// Async result returned by one owned cleanup effect.
pub type CleanupFuture = BoxFuture<'static, std::result::Result<(), String>>;
/// One-shot cleanup effect registered by an active plugin generation.
pub type Cleanup = Box<dyn FnOnce() -> CleanupFuture + Send + 'static>;

/// Adapter-neutral factory seam implemented by safe-Rust and execution backends.
#[async_trait]
pub trait PluginFactory: fmt::Debug + Send + Sync + 'static {
    /// Validates and normalizes desired configuration and selects exact
    /// requirements for one activation attempt.
    ///
    /// Preparation has no generation [`crate::Context`] and cannot access the
    /// services it is deciding.
    fn prepare(&self, desired: &ConfigValue) -> Result<PreparedActivation>;

    /// Activates one prepared generation using exact injected capabilities and
    /// the single-use attempt-local state.
    async fn activate(&self, plan: ActivationPlan) -> Result<()>;
}

/// Immutable resolver-owned provenance and behavior for one plugin factory.
#[derive(Clone)]
pub struct ResolvedFactory {
    identity: FactoryIdentity,
    update_mode: UpdateMode,
    implementation: Arc<dyn PluginFactory>,
}

impl ResolvedFactory {
    /// Binds already resolved provenance to one implementation.
    pub fn new(
        identity: FactoryIdentity,
        update_mode: UpdateMode,
        implementation: Arc<dyn PluginFactory>,
    ) -> Self {
        Self {
            identity,
            update_mode,
            implementation,
        }
    }

    /// Resolves one process-linked factory before plugin code executes.
    pub fn linked(
        plugin: impl Into<PluginId>,
        revision: impl Into<String>,
        update_mode: UpdateMode,
        implementation: Arc<dyn PluginFactory>,
    ) -> Self {
        Self {
            identity: FactoryIdentity::linked(plugin, revision),
            update_mode,
            implementation,
        }
    }

    /// Resolves one explicitly loaded native factory before plugin code executes.
    pub fn native(
        plugin: impl Into<PluginId>,
        sha256: impl Into<String>,
        update_mode: UpdateMode,
        implementation: Arc<dyn PluginFactory>,
    ) -> Self {
        Self {
            identity: FactoryIdentity::native(plugin, sha256),
            update_mode,
            implementation,
        }
    }

    /// Returns the immutable code provenance retained by every created Fiber.
    pub const fn identity(&self) -> &FactoryIdentity {
        &self.identity
    }

    /// Returns the factory's static configuration update policy.
    pub const fn update_mode(&self) -> UpdateMode {
        self.update_mode
    }

    pub(crate) fn into_parts(self) -> (FactoryIdentity, UpdateMode, Arc<dyn PluginFactory>) {
        (self.identity, self.update_mode, self.implementation)
    }
}

impl fmt::Debug for ResolvedFactory {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ResolvedFactory")
            .field("identity", &self.identity)
            .field("update_mode", &self.update_mode)
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
pub(crate) fn resolved_test_factory<T: PluginFactory>(factory: Arc<T>) -> ResolvedFactory {
    ResolvedFactory::linked("test", "1", UpdateMode::Replayable, factory)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{CallId, FiberId, MetaError, Runtime};
    use std::any::type_name;
    use std::cell::Cell;
    use std::collections::BTreeMap;
    use std::sync::Arc;

    struct RecursivePanicPayload;

    impl Drop for RecursivePanicPayload {
        fn drop(&mut self) {
            std::panic::panic_any(Self);
        }
    }

    struct PanickingState;

    impl Drop for PanickingState {
        fn drop(&mut self) {
            std::panic::panic_any(RecursivePanicPayload);
        }
    }

    #[test]
    fn wrong_type_preserves_single_owner_state_and_success_consumes_it() {
        // Cell is Send but not Sync, proving the single-owner interface does
        // not impose a false sharing requirement.
        let state = PreparedState::new(Cell::new(7_u8), 1);
        let runtime = Runtime::default();
        let mut context = runtime.root();
        context.install_activation_lineage(FiberId(1), CallId(1));
        let mut plan = ActivationPlan::new(
            context,
            Arc::new(Value::Null),
            BTreeMap::new(),
            BTreeMap::new(),
            Some(state),
        );
        assert_eq!(plan.lineage_call_id(), CallId(1));

        assert_eq!(
            plan.take_state::<String>(),
            Err(MetaError::PreparedStateTypeMismatch {
                expected: type_name::<String>(),
            })
        );
        assert_eq!(plan.take_state::<Cell<u8>>().unwrap().get(), 7);
        assert_eq!(
            plan.take_state::<Cell<u8>>(),
            Err(MetaError::PreparedStateUnavailable)
        );
    }

    #[test]
    fn prepared_debug_redacts_configuration_and_opaque_state() {
        let prepared = PreparedActivation::with_state(
            serde_json::json!({"secret-config": true}),
            "secret-state".to_owned(),
            12,
        );
        let diagnostic = format!("{prepared:?}");
        assert!(!diagnostic.contains("secret-config"));
        assert!(!diagnostic.contains("secret-state"));
        assert!(diagnostic.contains("<redacted>"));
    }

    #[test]
    fn recursively_panicking_state_destructor_cannot_escape() {
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            drop(PreparedActivation::with_state(
                Value::Null,
                PanickingState,
                0,
            ));
        }));
        assert!(result.is_ok());
    }
}
