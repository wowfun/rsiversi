use crate::Result;
use async_trait::async_trait;
use futures_util::future::BoxFuture;
use serde_json::Value;
use std::fmt;

mod activation_plan;
mod metadata;
mod prepared_activation;
pub use activation_plan::ActivationPlan;
pub use metadata::{FactoryIdentity, Requirement};
pub use prepared_activation::PreparedActivation;
pub(crate) use prepared_activation::PreparedState;

/// Validated JSON configuration retained for one Fiber.
pub type ConfigValue = Value;
/// Async result returned by one owned cleanup effect.
pub type CleanupFuture = BoxFuture<'static, std::result::Result<(), String>>;
/// One-shot cleanup effect registered by an active plugin generation.
pub type Cleanup = Box<dyn FnOnce() -> CleanupFuture + Send + 'static>;

/// Adapter-neutral factory seam implemented by safe-Rust and execution backends.
#[async_trait]
pub trait PluginFactory: fmt::Debug + Send + Sync + 'static {
    /// Returns bounded diagnostic identity captured once for the Fiber lifetime.
    fn identity(&self) -> FactoryIdentity;

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
        let mut plan =
            ActivationPlan::new(context, Arc::new(Value::Null), BTreeMap::new(), Some(state));
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
