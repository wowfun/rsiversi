//! Exact-name process-local Tool Runtime ordinary plugin.

#![deny(unsafe_code)]
#![warn(missing_docs)]
#![allow(clippy::missing_errors_doc)]

use async_trait::async_trait;
use futures_util::FutureExt;
use rsi_meta::{ActivationPlan, ConfigValue, MetaError, PluginFactory, PreparedActivation};
use rsi_tools_protocol::{
    MAXIMUM_REGISTERED_TOOLS, MAXIMUM_RETAINED_TOOL_RESULTS, MAXIMUM_TOOL_TIMEOUT_MS,
    PreparedToolCall, Result, RetainedToolFailure, RetainedToolFailureKind, RetainedToolResult,
    ToolCall, ToolError, ToolExecution, ToolLease, ToolRegistration, ToolResult,
    ToolResultIdentity, ToolRuntime, ToolRuntimeContract, ToolStart,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::panic::AssertUnwindSafe;
use std::sync::{Arc, Mutex, Weak};
use std::time::Duration;
use tokio::sync::{Notify, oneshot};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

const DEFAULT_SHUTDOWN_TIMEOUT_MS: u64 = 10_000;
const MAXIMUM_SHUTDOWN_TIMEOUT_MS: u64 = 300_000;

/// Configuration for one Tool Runtime generation.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ToolsConfig {
    /// Maximum generation cleanup wait before unresolved Tool work is reported.
    #[serde(default = "default_shutdown_timeout_ms")]
    pub shutdown_timeout_ms: u64,
}

impl Default for ToolsConfig {
    fn default() -> Self {
        Self {
            shutdown_timeout_ms: default_shutdown_timeout_ms(),
        }
    }
}

const fn default_shutdown_timeout_ms() -> u64 {
    DEFAULT_SHUTDOWN_TIMEOUT_MS
}

impl ToolsConfig {
    fn validate(&self) -> Result<()> {
        if self.shutdown_timeout_ms == 0 || self.shutdown_timeout_ms > MAXIMUM_SHUTDOWN_TIMEOUT_MS {
            return Err(ToolError::InvalidInput(format!(
                "shutdown_timeout_ms must be within 1..={MAXIMUM_SHUTDOWN_TIMEOUT_MS}"
            )));
        }
        Ok(())
    }
}

#[derive(Debug)]
struct Registry {
    state: Arc<RegistryState>,
    owner_prefix: String,
}

#[derive(Debug)]
struct RegistryState {
    inner: Mutex<RegistryInner>,
    settled: Notify,
    shutdown: CancellationToken,
    shutdown_timeout: Duration,
}

#[derive(Debug)]
struct RegistryInner {
    accepting: bool,
    next_registration: u64,
    definitions: BTreeMap<String, Entry>,
    retained: BTreeMap<ToolResultIdentity, RetainedToolResult>,
    active: BTreeMap<ToolResultIdentity, JoinHandle<()>>,
}

impl Default for RegistryInner {
    fn default() -> Self {
        Self {
            accepting: true,
            next_registration: 0,
            definitions: BTreeMap::new(),
            retained: BTreeMap::new(),
            active: BTreeMap::new(),
        }
    }
}

#[derive(Clone, Debug)]
struct Entry {
    registration: u64,
    owner_id: String,
    definition: ToolRegistration,
}

#[async_trait]
impl ToolRuntime for Registry {
    fn register(&self, definition: ToolRegistration) -> Result<ToolLease> {
        validate_definition(&definition)?;
        let name = definition.definition.name().to_owned();
        let mut inner = lock(&self.state);
        if !inner.accepting {
            return Err(ToolError::Execution("Tool Runtime is shutting down".into()));
        }
        if inner.definitions.contains_key(&name) {
            return Err(ToolError::Duplicate(name));
        }
        if inner.definitions.len() >= MAXIMUM_REGISTERED_TOOLS {
            return Err(ToolError::InvalidInput(format!(
                "Tool Runtime generation cannot register more than {MAXIMUM_REGISTERED_TOOLS} definitions"
            )));
        }
        inner.next_registration = inner
            .next_registration
            .checked_add(1)
            .ok_or_else(|| ToolError::Execution("registration identity exhausted".into()))?;
        let registration = inner.next_registration;
        let owner_id = format!("{}-r{registration}", self.owner_prefix);
        inner.definitions.insert(
            name.clone(),
            Entry {
                registration,
                owner_id,
                definition,
            },
        );
        let state = Arc::downgrade(&self.state);
        Ok(ToolLease::new(move || {
            remove_if_current(&state, &name, registration);
        }))
    }

    fn definitions(&self) -> Vec<rsi_tools_protocol::ToolDefinition> {
        lock(&self.state)
            .definitions
            .values()
            .map(|entry| entry.definition.definition.clone())
            .collect()
    }

    fn prepare(&self, invocation_id: &str, call: ToolCall) -> Result<Box<dyn PreparedToolCall>> {
        call.validate()?;
        rsi_tools_protocol::validate_identifier("tool invocation", invocation_id)?;
        let entry = lock(&self.state)
            .definitions
            .get(&call.name)
            .cloned()
            .ok_or_else(|| ToolError::Unknown(call.name.clone()))?;
        let request_sha256 = canonical_request_sha256(&call)?;
        let identity = ToolResultIdentity::new(
            entry.owner_id.clone(),
            invocation_id,
            &call.id,
            request_sha256,
        )?;
        Ok(Box::new(Prepared {
            state: Arc::clone(&self.state),
            entry,
            call,
            identity,
        }))
    }

    fn query(&self, identity: &ToolResultIdentity) -> Result<RetainedToolResult> {
        Ok(lock(&self.state)
            .retained
            .get(identity)
            .cloned()
            .unwrap_or(RetainedToolResult::Absent))
    }

    async fn wait(
        &self,
        identity: &ToolResultIdentity,
        cancellation: CancellationToken,
    ) -> Result<RetainedToolResult> {
        loop {
            let notified = self.state.settled.notified();
            let current = self.query(identity)?;
            if current != RetainedToolResult::Pending {
                return Ok(current);
            }
            tokio::select! {
                () = notified => {}
                () = cancellation.cancelled() => return Err(ToolError::Cancelled),
            }
        }
    }

    fn commit(&self, identity: &ToolResultIdentity) -> Result<()> {
        let mut inner = lock(&self.state);
        match inner.retained.get(identity) {
            Some(RetainedToolResult::Pending) => Err(ToolError::Execution(
                "cannot retire a pending Tool invocation".into(),
            )),
            Some(_) => {
                inner.retained.remove(identity);
                Ok(())
            }
            None => Err(ToolError::InvalidInput(
                "retained Tool invocation is absent".into(),
            )),
        }
    }
}

#[derive(Debug)]
struct Prepared {
    state: Arc<RegistryState>,
    entry: Entry,
    call: ToolCall,
    identity: ToolResultIdentity,
}

#[async_trait]
impl PreparedToolCall for Prepared {
    fn identity(&self) -> &ToolResultIdentity {
        &self.identity
    }

    async fn start(self: Box<Self>, start: ToolStart) -> Result<ToolResult> {
        let receiver = {
            let mut inner = lock(&self.state);
            if !inner.accepting {
                return Err(ToolError::Execution("Tool Runtime is shutting down".into()));
            }
            if inner.retained.contains_key(&self.identity) {
                return Err(ToolError::Execution(
                    "Tool invocation identity was already started".into(),
                ));
            }
            if inner.retained.len() >= MAXIMUM_RETAINED_TOOL_RESULTS {
                return Err(ToolError::Execution(
                    "retained Tool result capacity is exhausted".into(),
                ));
            }
            inner
                .retained
                .insert(self.identity.clone(), RetainedToolResult::Pending);
            let (sender, receiver) = oneshot::channel();
            let state = Arc::clone(&self.state);
            let shutdown = self.state.shutdown.clone();
            let identity = self.identity.clone();
            let entry = self.entry.clone();
            let call = self.call.clone();
            let task_identity = identity.clone();
            let task = tokio::spawn(async move {
                let result = settle(entry, call, start, shutdown).await;
                let retained = match &result {
                    Ok(result) => RetainedToolResult::Returned(result.clone()),
                    Err(error) => RetainedToolResult::Failed(retained_failure(error)),
                };
                let mut inner = lock(&state);
                inner.retained.insert(identity.clone(), retained);
                inner.active.remove(&identity);
                drop(inner);
                state.settled.notify_waiters();
                let _ = sender.send(result);
            });
            inner.active.insert(task_identity, task);
            receiver
        };
        receiver.await.unwrap_or_else(|_| {
            Err(ToolError::Execution(
                "Tool settlement task ended without an outcome".into(),
            ))
        })
    }
}

enum ToolCompletion {
    Body(std::result::Result<Result<ToolResult>, Box<dyn std::any::Any + Send>>),
    Cancelled,
    Timeout,
}

async fn settle(
    entry: Entry,
    call: ToolCall,
    start: ToolStart,
    shutdown: CancellationToken,
) -> Result<ToolResult> {
    let cancellation = start.cancellation.clone();
    let owned_cancellation = CancellationToken::new();
    let execution_start = ToolStart {
        cancellation: owned_cancellation.clone(),
        policy: start.policy,
        sandbox: start.sandbox,
    };
    let (execution, enforcement) = ToolExecution::from_start(call.id, execution_start)?;
    let future = AssertUnwindSafe(entry.definition.executor.execute(call.arguments, execution))
        .catch_unwind();
    tokio::pin!(future);
    let timeout = tokio::time::sleep(Duration::from_millis(entry.definition.timeout_ms));
    tokio::pin!(timeout);

    let completion = tokio::select! {
        biased;
        result = &mut future => ToolCompletion::Body(result),
        () = cancellation.cancelled() => ToolCompletion::Cancelled,
        () = shutdown.cancelled() => ToolCompletion::Cancelled,
        () = &mut timeout => ToolCompletion::Timeout,
    };
    match completion {
        ToolCompletion::Body(Ok(result)) => result
            .and_then(|result| enforcement.attach(result))
            .and_then(validate_result),
        ToolCompletion::Body(Err(_)) => Err(ToolError::Execution("Tool body panicked".into())),
        ToolCompletion::Cancelled => {
            owned_cancellation.cancel();
            let _ = future.await;
            Err(ToolError::Cancelled)
        }
        ToolCompletion::Timeout => {
            owned_cancellation.cancel();
            let _ = future.await;
            Err(ToolError::Timeout)
        }
    }
}

fn lock(state: &RegistryState) -> std::sync::MutexGuard<'_, RegistryInner> {
    state
        .inner
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn remove_if_current(state: &Weak<RegistryState>, name: &str, registration: u64) {
    let Some(state) = state.upgrade() else {
        return;
    };
    let mut inner = lock(&state);
    if inner
        .definitions
        .get(name)
        .is_some_and(|entry| entry.registration == registration)
    {
        inner.definitions.remove(name);
    }
}

async fn shutdown(state: Arc<RegistryState>) -> std::result::Result<(), String> {
    state.shutdown.cancel();
    let active = {
        let mut inner = lock(&state);
        inner.accepting = false;
        std::mem::take(&mut inner.active)
    };
    let wait = async {
        for (_, task) in active {
            let _ = task.await;
        }
    };
    tokio::time::timeout(state.shutdown_timeout, wait)
        .await
        .map_err(|_| "Tool Runtime shutdown timed out with unsettled work".to_owned())
}

fn validate_definition(definition: &ToolRegistration) -> Result<()> {
    definition.definition.validate()?;
    if definition.timeout_ms == 0 || definition.timeout_ms > MAXIMUM_TOOL_TIMEOUT_MS {
        return Err(ToolError::InvalidInput(format!(
            "tool timeout must be within 1..={MAXIMUM_TOOL_TIMEOUT_MS} milliseconds"
        )));
    }
    Ok(())
}

fn validate_result(result: ToolResult) -> Result<ToolResult> {
    result.validate()?;
    Ok(result)
}

fn retained_failure(error: &ToolError) -> RetainedToolFailure {
    match error {
        ToolError::Cancelled => RetainedToolFailure {
            kind: RetainedToolFailureKind::Cancelled,
            summary: "Tool invocation was cancelled".into(),
        },
        ToolError::Timeout => RetainedToolFailure {
            kind: RetainedToolFailureKind::Timeout,
            summary: "Tool invocation timed out".into(),
        },
        ToolError::InvalidInput(_)
        | ToolError::Duplicate(_)
        | ToolError::Unknown(_)
        | ToolError::Execution(_) => RetainedToolFailure {
            kind: RetainedToolFailureKind::Execution,
            summary: "Tool invocation failed".into(),
        },
    }
}

fn canonical_request_sha256(call: &ToolCall) -> Result<String> {
    let request = serde_json::json!({
        "arguments": canonicalize(&call.arguments),
        "name": call.name,
    });
    let bytes =
        serde_json::to_vec(&request).map_err(|error| ToolError::InvalidInput(error.to_string()))?;
    Ok(hex::encode(Sha256::digest(bytes)))
}

fn canonicalize(value: &Value) -> Value {
    match value {
        Value::Array(values) => Value::Array(values.iter().map(canonicalize).collect()),
        Value::Object(values) => {
            let ordered: BTreeMap<_, _> = values
                .iter()
                .map(|(key, value)| (key.clone(), canonicalize(value)))
                .collect();
            Value::Object(ordered.into_iter().collect())
        }
        other => other.clone(),
    }
}

/// Ordinary factory for one Tool Runtime generation.
#[derive(Clone, Debug, Default)]
pub struct ToolsFactory;

#[async_trait]
impl PluginFactory for ToolsFactory {
    fn prepare(&self, desired: &ConfigValue) -> rsi_meta::Result<PreparedActivation> {
        let config = if desired.is_null() {
            ToolsConfig::default()
        } else {
            serde_json::from_value(desired.clone())
                .map_err(|error| MetaError::InvalidInput(error.to_string()))?
        };
        config
            .validate()
            .map_err(|error| MetaError::InvalidInput(error.to_string()))?;
        Ok(PreparedActivation::with_state(
            serde_json::to_value(&config)
                .map_err(|error| MetaError::InvalidInput(error.to_string()))?,
            config,
            std::mem::size_of::<ToolsConfig>(),
        ))
    }

    async fn activate(&self, mut plan: ActivationPlan) -> rsi_meta::Result<()> {
        let config = plan.take_state::<ToolsConfig>()?;
        let (fiber, generation) = plan.context().owner().ok_or_else(|| {
            MetaError::InvalidInput("Tools activation requires a Fiber generation".into())
        })?;
        let state = Arc::new(RegistryState {
            inner: Mutex::new(RegistryInner::default()),
            settled: Notify::new(),
            shutdown: CancellationToken::new(),
            shutdown_timeout: Duration::from_millis(config.shutdown_timeout_ms),
        });
        let runtime: Arc<dyn ToolRuntime> = Arc::new(Registry {
            state: Arc::clone(&state),
            owner_prefix: format!("tool-f{}-g{}", fiber.0, generation.0),
        });
        let supply = plan
            .context()
            .provide_local::<ToolRuntimeContract>(runtime)?;
        plan.defer(
            "withdraw Tool Runtime",
            Box::new(move || {
                Box::pin(async move {
                    let result = shutdown(state).await;
                    drop(supply);
                    result
                })
            }),
        )
    }
}
