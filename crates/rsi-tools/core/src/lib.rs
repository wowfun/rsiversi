//! Process-wide Tool catalog provider ordinary plugin.

#![deny(unsafe_code)]
#![warn(missing_docs)]
#![allow(clippy::missing_errors_doc)]

use async_trait::async_trait;
use futures_util::FutureExt;
use rsi_meta::{ActivationPlan, ConfigValue, MetaError, PluginFactory, PreparedActivation};
use rsi_tools_protocol::{
    MAXIMUM_REGISTERED_TOOLS, MAXIMUM_RETAINED_TOOL_RESULTS, MAXIMUM_TOOL_CATALOGS,
    MAXIMUM_TOOL_TIMEOUT_MS, PreparedToolCall, Result, RetainedToolFailure,
    RetainedToolFailureKind, RetainedToolResult, ToolBatchLease, ToolCall, ToolCatalogProvider,
    ToolCatalogProviderContract, ToolCatalogStage, ToolEnforcement, ToolError, ToolExecution,
    ToolLease, ToolRegistrar, ToolRegistration, ToolResult, ToolResultIdentity, ToolRuntime,
    ToolStart,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::panic::AssertUnwindSafe;
use std::sync::{Arc, Mutex, Weak};
use std::time::Duration;
use tokio::sync::{Notify, oneshot};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

const DEFAULT_SHUTDOWN_TIMEOUT_MS: u64 = 10_000;
const MAXIMUM_SHUTDOWN_TIMEOUT_MS: u64 = 300_000;

/// Configuration for one process-wide Tool catalog provider.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ToolsConfig {
    /// Maximum provider cleanup wait before unresolved Tool work is reported.
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
struct Provider {
    state: Arc<ProviderState>,
}

#[derive(Debug)]
struct ProviderState {
    inner: Mutex<ProviderInner>,
    settled: Notify,
    shutdown: CancellationToken,
    shutdown_timeout: Duration,
}

#[derive(Debug)]
struct ProviderInner {
    accepting: bool,
    next_catalog: u64,
    active_catalogs: usize,
    retained: BTreeMap<ToolResultIdentity, Arc<RetainedToolResult>>,
    active: BTreeMap<ToolResultIdentity, JoinHandle<()>>,
    active_by_owner: BTreeMap<String, usize>,
    abandoned_owners: BTreeSet<String>,
}

impl Default for ProviderInner {
    fn default() -> Self {
        Self {
            accepting: true,
            next_catalog: 0,
            active_catalogs: 0,
            retained: BTreeMap::new(),
            active: BTreeMap::new(),
            active_by_owner: BTreeMap::new(),
            abandoned_owners: BTreeSet::new(),
        }
    }
}

#[derive(Debug)]
struct CatalogLife {
    provider: Weak<ProviderState>,
}

impl Drop for CatalogLife {
    fn drop(&mut self) {
        let Some(provider) = self.provider.upgrade() else {
            return;
        };
        let mut inner = lock_provider(&provider);
        inner.active_catalogs = inner.active_catalogs.saturating_sub(1);
    }
}

#[derive(Debug)]
struct Stage {
    state: Option<Arc<StageState>>,
}

#[derive(Debug)]
struct Registrar {
    state: Arc<StageState>,
}

#[derive(Debug)]
struct StageState {
    provider: Arc<ProviderState>,
    catalog_id: u64,
    inner: Mutex<StageInner>,
}

#[derive(Debug)]
struct StageInner {
    status: StageStatus,
    life: Option<Arc<CatalogLife>>,
    next_registration: u64,
    definitions: BTreeMap<String, Entry>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum StageStatus {
    Open,
    Sealed,
    Abandoned,
}

#[derive(Clone, Debug)]
struct Entry {
    registration: u64,
    owner_id: String,
    retirement: CancellationToken,
    definition: ToolRegistration,
}

#[derive(Clone, Debug)]
struct BatchMember {
    name: String,
    registration: u64,
}

#[derive(Debug)]
struct Registry {
    provider: Arc<ProviderState>,
    _life: Arc<CatalogLife>,
    definitions: BTreeMap<String, Entry>,
}

impl Drop for Registry {
    fn drop(&mut self) {
        let owners = self
            .definitions
            .values()
            .map(|entry| entry.owner_id.clone())
            .collect::<BTreeSet<_>>();
        for entry in self.definitions.values() {
            entry.retirement.cancel();
        }
        let mut inner = lock_provider(&self.provider);
        inner
            .retained
            .retain(|identity, _| !owners.contains(identity.owner_id()));
        for owner in owners {
            if inner.active_by_owner.contains_key(&owner) {
                inner.abandoned_owners.insert(owner);
            }
        }
        drop(inner);
        self.provider.settled.notify_waiters();
    }
}

impl ToolCatalogProvider for Provider {
    fn begin_stage(&self) -> Result<Box<dyn ToolCatalogStage>> {
        let (catalog_id, life) = {
            let mut inner = lock_provider(&self.state);
            if !inner.accepting {
                return Err(ToolError::Execution(
                    "Tool catalog provider is shutting down".into(),
                ));
            }
            if inner.active_catalogs >= MAXIMUM_TOOL_CATALOGS {
                return Err(ToolError::Execution(
                    "Tool catalog capacity is exhausted".into(),
                ));
            }
            inner.next_catalog = inner
                .next_catalog
                .checked_add(1)
                .ok_or_else(|| ToolError::Execution("Tool catalog identity exhausted".into()))?;
            inner.active_catalogs += 1;
            let life = Arc::new(CatalogLife {
                provider: Arc::downgrade(&self.state),
            });
            (inner.next_catalog, life)
        };
        let state = Arc::new(StageState {
            provider: Arc::clone(&self.state),
            catalog_id,
            inner: Mutex::new(StageInner {
                status: StageStatus::Open,
                life: Some(life),
                next_registration: 0,
                definitions: BTreeMap::new(),
            }),
        });
        Ok(Box::new(Stage { state: Some(state) }))
    }
}

impl ToolCatalogStage for Stage {
    fn registrar(&self) -> Arc<dyn ToolRegistrar> {
        Arc::new(Registrar {
            state: Arc::clone(self.state.as_ref().expect("unconsumed catalog stage")),
        })
    }

    fn seal(mut self: Box<Self>) -> Result<Arc<dyn ToolRuntime>> {
        let state = self.state.take().ok_or(ToolError::Sealed)?;
        if !lock_provider(&state.provider).accepting {
            abandon_stage(&state);
            return Err(ToolError::Execution(
                "Tool catalog provider is shutting down".into(),
            ));
        }
        let (definitions, life) = {
            let mut inner = lock_stage(&state);
            if inner.status != StageStatus::Open {
                return Err(ToolError::Sealed);
            }
            inner.status = StageStatus::Sealed;
            let definitions = std::mem::take(&mut inner.definitions);
            let life = inner
                .life
                .take()
                .expect("open Tool catalog stage owns its capacity permit");
            (definitions, life)
        };
        Ok(Arc::new(Registry {
            provider: Arc::clone(&state.provider),
            _life: life,
            definitions,
        }))
    }
}

impl Drop for Stage {
    fn drop(&mut self) {
        if let Some(state) = self.state.take() {
            abandon_stage(&state);
        }
    }
}

fn abandon_stage(state: &StageState) {
    let (entries, life) = {
        let mut inner = lock_stage(state);
        if inner.status != StageStatus::Open {
            return;
        }
        inner.status = StageStatus::Abandoned;
        (std::mem::take(&mut inner.definitions), inner.life.take())
    };
    for entry in entries.into_values() {
        entry.retirement.cancel();
    }
    drop(life);
}

impl ToolRegistrar for Registrar {
    fn register(&self, registration: ToolRegistration) -> Result<ToolLease> {
        let lease = self.register_batch(vec![registration])?;
        Ok(ToolLease::new(lease))
    }

    fn register_batch(&self, registrations: Vec<ToolRegistration>) -> Result<ToolBatchLease> {
        if registrations.is_empty() {
            return Err(ToolError::InvalidInput(
                "Tool registration batch must be nonempty".into(),
            ));
        }
        for registration in &registrations {
            validate_definition(registration)?;
        }
        let mut batch_names = std::collections::BTreeSet::new();
        for registration in &registrations {
            let name = registration.definition.name().to_owned();
            if !batch_names.insert(name.clone()) {
                return Err(ToolError::Duplicate(name));
            }
        }

        let mut inner = lock_stage(&self.state);
        if inner.status != StageStatus::Open {
            return Err(ToolError::Sealed);
        }
        if !lock_provider(&self.state.provider).accepting {
            return Err(ToolError::Execution(
                "Tool catalog provider is shutting down".into(),
            ));
        }
        if let Some(name) = batch_names
            .iter()
            .find(|name| inner.definitions.contains_key(*name))
        {
            return Err(ToolError::Duplicate(name.clone()));
        }
        if inner
            .definitions
            .len()
            .checked_add(registrations.len())
            .is_none_or(|count| count > MAXIMUM_REGISTERED_TOOLS)
        {
            return Err(ToolError::InvalidInput(format!(
                "Tool catalog cannot register more than {MAXIMUM_REGISTERED_TOOLS} definitions"
            )));
        }
        let final_registration = inner
            .next_registration
            .checked_add(registrations.len() as u64)
            .ok_or_else(|| ToolError::Execution("registration identity exhausted".into()))?;
        let mut members = Vec::with_capacity(registrations.len());
        for definition in registrations {
            inner.next_registration += 1;
            let registration = inner.next_registration;
            let name = definition.definition.name().to_owned();
            let retirement = CancellationToken::new();
            let entry = Entry {
                registration,
                owner_id: format!("tool-c{}-r{registration}", self.state.catalog_id),
                retirement,
                definition,
            };
            inner.definitions.insert(name.clone(), entry);
            members.push(BatchMember { name, registration });
        }
        debug_assert_eq!(inner.next_registration, final_registration);
        drop(inner);

        let withdraw_state = Arc::downgrade(&self.state);
        let withdraw_members = members.clone();
        Ok(ToolBatchLease::new(move || {
            withdraw_batch(&withdraw_state, &withdraw_members);
        }))
    }
}

fn withdraw_batch(state: &Weak<StageState>, members: &[BatchMember]) {
    let Some(state) = state.upgrade() else {
        return;
    };
    let removed = {
        let mut inner = lock_stage(&state);
        if inner.status != StageStatus::Open {
            return;
        }
        members
            .iter()
            .filter_map(|member| {
                if inner
                    .definitions
                    .get(&member.name)
                    .is_some_and(|entry| entry.registration == member.registration)
                {
                    inner.definitions.remove(&member.name)
                } else {
                    None
                }
            })
            .collect::<Vec<_>>()
    };
    for entry in removed {
        entry.retirement.cancel();
    }
}

#[async_trait]
impl ToolRuntime for Registry {
    fn definitions(&self) -> Vec<rsi_tools_protocol::ToolDefinition> {
        self.definitions
            .values()
            .map(|entry| entry.definition.definition.clone())
            .collect()
    }

    fn prepare(&self, invocation_id: &str, call: ToolCall) -> Result<Box<dyn PreparedToolCall>> {
        call.validate()?;
        rsi_tools_protocol::validate_identifier("tool invocation", invocation_id)?;
        let entry = self
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
            provider: Arc::clone(&self.provider),
            entry,
            call,
            identity,
        }))
    }

    fn query(&self, identity: &ToolResultIdentity) -> Result<RetainedToolResult> {
        self.require_owned_identity(identity)?;
        let retained = {
            let inner = lock_provider(&self.provider);
            inner.retained.get(identity).cloned()
        };
        Ok(retained.map_or(RetainedToolResult::Absent, |retained| {
            retained.as_ref().clone()
        }))
    }

    async fn wait(
        &self,
        identity: &ToolResultIdentity,
        cancellation: CancellationToken,
    ) -> Result<RetainedToolResult> {
        loop {
            let notified = self.provider.settled.notified();
            tokio::pin!(notified);
            let _already_notified = notified.as_mut().enable();
            let current = self.query(identity)?;
            if current != RetainedToolResult::Pending {
                return Ok(current);
            }
            tokio::select! {
                () = notified.as_mut() => {}
                () = cancellation.cancelled() => return Err(ToolError::Cancelled),
            }
        }
    }

    fn commit(&self, identity: &ToolResultIdentity) -> Result<()> {
        self.require_owned_identity(identity)?;
        let mut inner = lock_provider(&self.provider);
        match inner.retained.get(identity) {
            Some(retained) if retained.as_ref() == &RetainedToolResult::Pending => Err(
                ToolError::Execution("cannot retire a pending Tool invocation".into()),
            ),
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

impl Registry {
    fn require_owned_identity(&self, identity: &ToolResultIdentity) -> Result<()> {
        if self
            .definitions
            .values()
            .any(|entry| entry.owner_id == identity.owner_id())
        {
            Ok(())
        } else {
            Err(ToolError::InvalidInput(
                "Tool result identity belongs to a different catalog generation".into(),
            ))
        }
    }
}

#[derive(Debug)]
struct Prepared {
    provider: Arc<ProviderState>,
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
            let mut inner = lock_provider(&self.provider);
            if !inner.accepting {
                return Err(ToolError::Execution(
                    "Tool catalog provider is shutting down".into(),
                ));
            }
            if self.entry.retirement.is_cancelled() {
                return Err(ToolError::Withdrawn(self.call.name.clone()));
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
                .insert(self.identity.clone(), Arc::new(RetainedToolResult::Pending));
            let (sender, receiver) = oneshot::channel();
            let provider = Arc::clone(&self.provider);
            let shutdown = self.provider.shutdown.clone();
            let identity = self.identity.clone();
            let entry = self.entry.clone();
            let call = self.call.clone();
            let task_identity = identity.clone();
            let owner_id = identity.owner_id().to_owned();
            let task_owner_id = owner_id.clone();
            let task = tokio::spawn(async move {
                let result = settle(entry, call, start, shutdown).await;
                let retained = Arc::new(match &result {
                    Ok(result) => RetainedToolResult::Returned(result.clone()),
                    Err(error) => RetainedToolResult::Failed(retained_failure(error)),
                });
                let mut inner = lock_provider(&provider);
                if !inner.abandoned_owners.contains(identity.owner_id()) {
                    inner.retained.insert(identity.clone(), retained);
                }
                inner.active.remove(&identity);
                let remove_owner =
                    inner
                        .active_by_owner
                        .get_mut(&task_owner_id)
                        .is_some_and(|active| {
                            *active = active.saturating_sub(1);
                            *active == 0
                        });
                if remove_owner {
                    inner.active_by_owner.remove(&task_owner_id);
                    inner.abandoned_owners.remove(&task_owner_id);
                }
                drop(inner);
                provider.settled.notify_waiters();
                let _ = sender.send(result);
            });
            *inner.active_by_owner.entry(owner_id).or_default() += 1;
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
        job_scope: start.job_scope,
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
        () = entry.retirement.cancelled() => ToolCompletion::Cancelled,
        () = shutdown.cancelled() => ToolCompletion::Cancelled,
        () = &mut timeout => ToolCompletion::Timeout,
    };
    match completion {
        ToolCompletion::Body(result) => settle_body(result, enforcement),
        ToolCompletion::Cancelled => {
            owned_cancellation.cancel();
            settle_body(future.await, enforcement)
        }
        ToolCompletion::Timeout => {
            owned_cancellation.cancel();
            match settle_body(future.await, enforcement) {
                Err(ToolError::Cancelled) => Err(ToolError::Timeout),
                settled => settled,
            }
        }
    }
}

fn settle_body(
    body: std::result::Result<Result<ToolResult>, Box<dyn std::any::Any + Send>>,
    enforcement: ToolEnforcement,
) -> Result<ToolResult> {
    match body {
        Ok(result) => result.and_then(|result| enforcement.attach(result)),
        Err(payload) => {
            if let Err(recursive_payload) =
                std::panic::catch_unwind(AssertUnwindSafe(|| drop(payload)))
                && let Err(final_payload) =
                    std::panic::catch_unwind(AssertUnwindSafe(|| drop(recursive_payload)))
            {
                std::mem::forget(final_payload);
            }
            Err(ToolError::Execution("Tool body panicked".into()))
        }
    }
}

fn lock_provider(state: &ProviderState) -> std::sync::MutexGuard<'_, ProviderInner> {
    state
        .inner
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn lock_stage(state: &StageState) -> std::sync::MutexGuard<'_, StageInner> {
    state
        .inner
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

async fn shutdown(state: Arc<ProviderState>) -> std::result::Result<(), String> {
    state.shutdown.cancel();
    let active = {
        let mut inner = lock_provider(&state);
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
        .map_err(|_| "Tool provider shutdown timed out with unsettled work".to_owned())
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
        | ToolError::Withdrawn(_)
        | ToolError::Sealed
        | ToolError::Sandbox(_)
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

/// Ordinary factory for one process-wide Tool catalog provider.
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
        let state = Arc::new(ProviderState {
            inner: Mutex::new(ProviderInner::default()),
            settled: Notify::new(),
            shutdown: CancellationToken::new(),
            shutdown_timeout: Duration::from_millis(config.shutdown_timeout_ms),
        });
        let provider: Arc<dyn ToolCatalogProvider> = Arc::new(Provider {
            state: Arc::clone(&state),
        });
        let supply = plan
            .context()
            .provide_local::<ToolCatalogProviderContract>(provider)?;
        plan.defer(
            "withdraw Tool catalog provider",
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
