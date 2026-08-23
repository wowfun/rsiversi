use crate::{InvocationContext, Result};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::sync::Arc;

/// Listener scheduling and value-flow policy for one dispatch.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DispatchMode {
    /// Invoke listeners in registration order and ignore returned values.
    Emit,
    /// Invoke every listener concurrently and aggregate failures.
    Parallel,
    /// Invoke in order until a listener completes the dispatch.
    Serial,
    /// Pass each continued value to the next listener in order.
    Waterfall,
}

/// Value-flow decision returned by an event listener.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "value", rename_all = "snake_case")]
pub enum EventOutcome {
    /// Continue dispatch, optionally replacing the waterfall value.
    Continue(Value),
    /// Stop serial or waterfall dispatch with a completed value.
    Complete(Value),
}

impl EventOutcome {
    /// Creates a continuing outcome.
    pub fn continuing(value: impl Into<Value>) -> Self {
        Self::Continue(value.into())
    }

    /// Creates a completing outcome.
    pub fn complete(value: impl Into<Value>) -> Self {
        Self::Complete(value.into())
    }
}

/// Publication and invocation options for one listener.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct EventOptions {
    /// Insert before existing listeners rather than after them.
    pub prepend: bool,
    /// Make the listener visible across service-isolation scopes.
    pub global: bool,
    /// Atomically claim the listener for at most one invocation.
    pub once: bool,
}

/// Observable result of one completed dispatch.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct EventReceipt {
    /// Number of listeners whose callback was admitted.
    pub invoked: usize,
    /// Value from a completing serial or waterfall listener.
    pub completed: Option<Value>,
}

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
