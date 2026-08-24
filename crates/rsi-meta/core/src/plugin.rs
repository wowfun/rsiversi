use crate::{Context, Result};
use async_trait::async_trait;
use futures_util::future::BoxFuture;
use serde_json::Value;
use std::fmt;
use std::sync::Arc;

mod descriptor;
pub use descriptor::{FactoryIdentity, PluginDescriptor, Provision, Requirement};

/// Validated JSON configuration retained for one Fiber.
pub type ConfigValue = Value;
/// Async result returned by one owned cleanup effect.
pub type CleanupFuture = BoxFuture<'static, std::result::Result<(), String>>;
/// One-shot cleanup effect registered by an active plugin generation.
pub type Cleanup = Box<dyn FnOnce() -> CleanupFuture + Send + 'static>;

/// Adapter-neutral factory seam implemented by safe-Rust and execution backends.
#[async_trait]
pub trait PluginFactory: fmt::Debug + Send + Sync + 'static {
    /// Returns the immutable declaration inspected during preparation.
    fn descriptor(&self) -> &PluginDescriptor;

    /// Validates and normalizes bounded configuration before Fiber insertion.
    fn validate_config(&self, config: ConfigValue) -> Result<ConfigValue> {
        Ok(config)
    }

    /// Stages services, listeners, children, and effects for one generation.
    ///
    /// Configuration is shared with the Fiber so repeated convergence does not
    /// deep-clone the validated JSON tree.
    async fn activate(&self, context: Context, config: Arc<ConfigValue>) -> Result<()>;
}
