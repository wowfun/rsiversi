use crate::{EventOutcome, InvocationContext, Result};
use async_trait::async_trait;
use serde_json::Value;
use std::sync::Arc;

/// Async callback implemented by a plugin event listener.
#[async_trait]
pub trait EventHandler: std::fmt::Debug + Send + Sync + 'static {
    /// Handles one generation-fenced invocation and shared immutable input.
    async fn handle(
        &self,
        invocation: InvocationContext,
        value: Arc<Value>,
    ) -> Result<EventOutcome>;
}
