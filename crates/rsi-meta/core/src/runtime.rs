use crate::listener_registry::{ListenerBinding, ListenerRegistry};
use crate::service::{AdmissionLease, LeaseGuard, ProviderBinding};
use crate::{
    CallId, Cleanup, CleanupReport, ConfigValue, ContractId, ContractVersion, DispatchMode,
    EventHandler, EventKey, EventListenerId, EventOptions, EventOutcome, EventReceipt,
    FactoryIdentity, FiberGeneration, FiberId, InvocationContext, IsolationId, MetaError,
    PluginDescriptor, PluginFactory, Result, ServiceCall, ServiceEndpoint, ServiceHandle,
    ServiceKey,
};
use futures_util::FutureExt as _;
use futures_util::future::{BoxFuture, join_all};
use futures_util::stream::{FuturesUnordered, StreamExt as _};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fmt;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, Weak};
use std::time::Duration;
use tokio::sync::{Mutex as AsyncMutex, Notify, Semaphore, mpsc, oneshot, watch};
use tokio_util::sync::CancellationToken;

mod configuration;
mod context_api;
mod context_scope;
mod declaration_index;
mod dispatch;
mod lifecycle;
mod limits;
mod reconciliation_queue;
mod service_bridge;

use context_api::binding_identities;
pub(crate) use context_scope::InterceptLayers;
use declaration_index::DeclarationIndex;
pub use limits::RuntimeLimits;

const ROOT_FIBER: FiberId = FiberId(0);

/// Why a non-active Fiber cannot currently resolve and activate.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PendingReason {
    /// No provider is published in the selected isolation slot.
    MissingService {
        /// Missing logical service key.
        service: ServiceKey,
        /// Isolation slot selected by the Fiber Context.
        isolation: IsolationId,
    },
    /// A provider exists but does not match the exact declared contract.
    ContractMismatch {
        /// Logical service key.
        service: ServiceKey,
        /// Required contract identity.
        expected: ContractId,
        /// Required exact version.
        expected_version: ContractVersion,
        /// Published contract identity.
        actual: ContractId,
        /// Published exact version.
        actual_version: ContractVersion,
    },
    /// Reachable pending declarations form a dependency cycle.
    DependencyCycle {
        /// Ordered service path that closes the cycle.
        services: Vec<ServiceKey>,
    },
}

/// Observable lifecycle state of one Fiber.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FiberState {
    /// Dependency convergence has not produced an activatable snapshot.
    Pending(Vec<PendingReason>),
    /// One generation is staging owned resources.
    Loading,
    /// The staged generation is published.
    Active,
    /// The latest activation or retirement transaction failed.
    Failed(String),
    /// Publications are withdrawn and owned resources are retiring.
    Unloading,
    /// Final teardown completed and the Fiber left the registry.
    Disposed,
}

impl FiberState {
    fn is_transitioning(&self) -> bool {
        matches!(self, Self::Loading | Self::Unloading)
    }
}

/// Immutable observation of one Fiber at one Runtime revision.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct FiberSnapshot {
    /// Runtime-local Fiber identity.
    pub id: FiberId,
    /// Latest assigned activation generation.
    pub generation: FiberGeneration,
    /// Factory identity captured during preparation.
    pub factory: FactoryIdentity,
    /// Current lifecycle state.
    pub state: FiberState,
}

/// Point-in-time observation of Runtime lifecycle and registered Fibers.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct RuntimeSnapshot {
    /// Monotonic registry revision.
    pub revision: u64,
    /// Whether shutdown admission has closed.
    pub shutting_down: bool,
    /// First terminal reason, when the Runtime has fenced new work.
    pub terminal: Option<String>,
    /// Fiber snapshots ordered by Fiber identity.
    pub fibers: Vec<FiberSnapshot>,
}

/// Cloneable owner of plugin composition, admission, convergence, and teardown.
#[derive(Clone)]
pub struct Runtime {
    inner: Arc<RuntimeInner>,
}

impl fmt::Debug for Runtime {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let state = self.inner.state.lock().expect("runtime state poisoned");
        formatter
            .debug_struct("Runtime")
            .field("revision", &state.revision)
            .field("fibers", &state.fibers.len())
            .field(
                "shutting_down",
                &self.inner.shutting_down.load(Ordering::Acquire),
            )
            .finish_non_exhaustive()
    }
}

struct RuntimeInner {
    limits: RuntimeLimits,
    state: Mutex<RuntimeState>,
    shutting_down: AtomicBool,
    shutdown_result: Mutex<Option<CleanupReport>>,
    shutdown_complete: Notify,
    terminal_cancellation: CancellationToken,
    service_call_admission: Arc<Semaphore>,
    next_fiber: AtomicU64,
    next_generation: AtomicU64,
    next_isolation: AtomicU64,
    next_listener: AtomicU64,
    next_call: AtomicU64,
}

struct RuntimeState {
    revision: u64,
    fibers: BTreeMap<FiberId, Arc<Fiber>>,
    dependents: HashMap<ServiceKey, BTreeSet<FiberId>>,
    declarations: DeclarationIndex,
    providers: HashMap<ServiceSlot, Arc<ProviderBinding>>,
    listeners: HashMap<EventKey, ListenerRegistry>,
    listener_events: HashMap<EventListenerId, EventKey>,
    staged_listeners: HashMap<(FiberId, FiberGeneration), Vec<ListenerBinding>>,
    pending_reconciliations: BTreeSet<FiberId>,
    reconciliation_worker_running: bool,
    terminal: Option<String>,
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
struct ServiceSlot {
    key: ServiceKey,
    isolation: IsolationId,
}

struct Fiber {
    id: FiberId,
    runtime: Weak<RuntimeInner>,
    parent: Option<Owner>,
    base_context: ContextScope,
    configuration: AsyncMutex<()>,
    transition: AsyncMutex<()>,
    data: Mutex<FiberData>,
    watch: watch::Sender<FiberSnapshot>,
}

struct FiberData {
    factory: Arc<dyn PluginFactory>,
    descriptor: PluginDescriptor,
    config: ConfigValue,
    target_revision: u64,
    generation: FiberGeneration,
    state: FiberState,
    disposed: bool,
    disposal_report: Option<CleanupReport>,
    active: Option<GenerationData>,
    last_attempt: Option<ActivationAttempt>,
}

type BindingIdentities = BTreeMap<ServiceKey, (FiberId, FiberGeneration)>;
type ActivationAttempt = (u64, BindingIdentities);

struct GenerationData {
    generation: FiberGeneration,
    bindings: BTreeMap<ServiceKey, Arc<ProviderBinding>>,
    effects: Vec<EffectRecord>,
    services: Vec<Arc<ProviderBinding>>,
    listeners: Vec<EventListenerId>,
    children: Vec<FiberId>,
    lease: Arc<AdmissionLease>,
    published: bool,
    target_revision: u64,
}

struct EffectRecord {
    label: String,
    cleanup: Cleanup,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct Owner {
    fiber: FiberId,
    generation: FiberGeneration,
}

#[derive(Clone, Debug)]
struct CallTrace {
    origin: FiberId,
    parent_call: Option<CallId>,
}

#[derive(Clone, Debug)]
pub(crate) struct ContextScope {
    isolation: Arc<BTreeMap<ServiceKey, IsolationId>>,
    intercepts: Arc<BTreeMap<ServiceKey, Arc<InterceptLayers>>>,
    trace: Option<CallTrace>,
}

/// Immutable scoped capability used to apply plugins and access owned resources.
#[derive(Clone)]
pub struct Context {
    runtime: Runtime,
    owner: Option<Owner>,
    isolation: Arc<BTreeMap<ServiceKey, IsolationId>>,
    intercepts: Arc<BTreeMap<ServiceKey, Arc<InterceptLayers>>>,
    trace: Option<CallTrace>,
}

impl fmt::Debug for Context {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Context")
            .field("owner", &self.owner)
            .field("isolation", &self.isolation)
            .finish_non_exhaustive()
    }
}

/// Cloneable management handle for one independently owned Fiber.
#[derive(Clone)]
pub struct FiberHandle {
    runtime: Runtime,
    fiber: Arc<Fiber>,
}

/// Opaque proof that descriptor validation and configuration normalization
/// completed successfully exactly once.
pub struct PreparedPlugin {
    factory: Arc<dyn PluginFactory>,
    descriptor: PluginDescriptor,
    config: ConfigValue,
}

impl fmt::Debug for PreparedPlugin {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PreparedPlugin")
            .field("descriptor", &self.descriptor)
            .finish_non_exhaustive()
    }
}

impl Runtime {
    /// Creates an empty Runtime after validating every nonzero capacity and deadline.
    pub fn new(limits: RuntimeLimits) -> Result<Self> {
        limits.validate()?;
        let service_call_admission =
            Arc::new(Semaphore::new(limits.maximum_concurrent_service_calls));
        Ok(Self {
            inner: Arc::new(RuntimeInner {
                limits,
                state: Mutex::new(RuntimeState {
                    revision: 0,
                    fibers: BTreeMap::new(),
                    dependents: HashMap::new(),
                    declarations: DeclarationIndex::default(),
                    providers: HashMap::new(),
                    listeners: HashMap::new(),
                    listener_events: HashMap::new(),
                    staged_listeners: HashMap::new(),
                    pending_reconciliations: BTreeSet::new(),
                    reconciliation_worker_running: false,
                    terminal: None,
                }),
                shutting_down: AtomicBool::new(false),
                shutdown_result: Mutex::new(None),
                shutdown_complete: Notify::new(),
                terminal_cancellation: CancellationToken::new(),
                service_call_admission,
                next_fiber: AtomicU64::new(0),
                next_generation: AtomicU64::new(0),
                next_isolation: AtomicU64::new(0),
                next_listener: AtomicU64::new(0),
                next_call: AtomicU64::new(0),
            }),
        })
    }

    /// Creates an unowned root Context retaining this Runtime.
    pub fn root(&self) -> Context {
        Context {
            runtime: self.clone(),
            owner: None,
            isolation: Arc::new(BTreeMap::new()),
            intercepts: Arc::new(BTreeMap::new()),
            trace: None,
        }
    }

    /// Returns the immutable limits selected at construction.
    pub fn limits(&self) -> &RuntimeLimits {
        &self.inner.limits
    }

    /// Captures bounded membership, then reads each Fiber outside the registry lock.
    pub fn snapshot(&self) -> RuntimeSnapshot {
        let (revision, terminal, fibers) = {
            let state = self.inner.state.lock().expect("runtime state poisoned");
            (
                state.revision,
                state.terminal.clone(),
                state.fibers.values().cloned().collect::<Vec<_>>(),
            )
        };
        RuntimeSnapshot {
            revision,
            shutting_down: self.inner.shutting_down.load(Ordering::Acquire),
            terminal,
            fibers: fibers.into_iter().map(|fiber| fiber.snapshot()).collect(),
        }
    }

    /// Permanently fences new admission with the first supplied reason.
    ///
    /// This is intended for trusted execution adapters that detect a condition
    /// unsafe to isolate within the process.
    pub fn mark_terminal(&self, reason: impl Into<String>) {
        let reason = reason.into();
        self.inner
            .state
            .lock()
            .expect("runtime state poisoned")
            .terminal
            .get_or_insert(reason);
        self.inner.terminal_cancellation.cancel();
    }

    fn ensure_admitting(&self) -> Result<()> {
        let state = self.inner.state.lock().expect("runtime state poisoned");
        if let Some(reason) = state.terminal.clone() {
            return Err(MetaError::RuntimeTerminal(reason));
        }
        if self.inner.shutting_down.load(Ordering::Acquire) {
            return Err(MetaError::RuntimeShuttingDown);
        }
        Ok(())
    }

    fn next_generation(&self) -> FiberGeneration {
        FiberGeneration(self.inner.next_generation.fetch_add(1, Ordering::AcqRel) + 1)
    }

    /// Validates a descriptor and normalizes bounded configuration exactly once.
    ///
    /// The returned proof can be consumed by [`Context::apply_prepared`] without
    /// invoking factory normalization again.
    pub fn prepare(
        &self,
        factory: Arc<dyn PluginFactory>,
        config: ConfigValue,
    ) -> Result<PreparedPlugin> {
        self.ensure_admitting()?;
        let descriptor = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let descriptor = factory.descriptor().clone();
            descriptor.validate()?;
            Ok::<_, MetaError>(descriptor)
        }))
        .map_err(|_| MetaError::Activation("plugin descriptor validation panicked".to_owned()))??;
        let config =
            Self::normalize_config(&factory, config, self.inner.limits.maximum_config_bytes)?;
        Ok(PreparedPlugin {
            factory,
            descriptor,
            config,
        })
    }

    #[allow(clippy::too_many_lines)] // Validation proof consumption and atomic ownership insertion stay adjacent.
    async fn apply_prepared(
        &self,
        parent: &Context,
        prepared: PreparedPlugin,
    ) -> Result<FiberHandle> {
        self.ensure_admitting()?;
        let PreparedPlugin {
            factory,
            descriptor,
            config,
        } = prepared;
        let declared_services = descriptor
            .provides
            .iter()
            .map(|provision| provision.key.clone())
            .collect::<Vec<_>>();
        let required_services = descriptor
            .requires
            .iter()
            .map(|requirement| requirement.key.clone())
            .collect::<Vec<_>>();

        let id = FiberId(self.inner.next_fiber.fetch_add(1, Ordering::AcqRel) + 1);
        let initial = FiberSnapshot {
            id,
            generation: FiberGeneration(0),
            factory: descriptor.identity.clone(),
            state: FiberState::Pending(Vec::new()),
        };
        let (watch, _) = watch::channel(initial);
        let base_context = ContextScope {
            isolation: Arc::clone(&parent.isolation),
            intercepts: Arc::clone(&parent.intercepts),
            trace: parent.trace.clone(),
        };
        let fiber = Arc::new(Fiber {
            id,
            runtime: Arc::downgrade(&self.inner),
            parent: parent.owner,
            base_context,
            configuration: AsyncMutex::new(()),
            transition: AsyncMutex::new(()),
            data: Mutex::new(FiberData {
                factory,
                descriptor,
                config,
                target_revision: 1,
                generation: FiberGeneration(0),
                state: FiberState::Pending(Vec::new()),
                disposed: false,
                disposal_report: None,
                active: None,
                last_attempt: None,
            }),
            watch,
        });

        {
            let mut state = self.inner.state.lock().expect("runtime state poisoned");
            if let Some(reason) = state.terminal.clone() {
                return Err(MetaError::RuntimeTerminal(reason));
            }
            if self.inner.shutting_down.load(Ordering::Acquire) {
                return Err(MetaError::RuntimeShuttingDown);
            }
            if state.fibers.len() >= self.inner.limits.maximum_fibers {
                return Err(MetaError::CapacityExhausted { resource: "fibers" });
            }
            if let Some(owner) = parent.owner {
                let parent_fiber =
                    state
                        .fibers
                        .get(&owner.fiber)
                        .cloned()
                        .ok_or(MetaError::StaleContext {
                            fiber: owner.fiber,
                            generation: owner.generation,
                        })?;
                let mut data = parent_fiber.data.lock().expect("fiber state poisoned");
                if data.generation != owner.generation
                    || !matches!(data.state, FiberState::Loading | FiberState::Active)
                {
                    return Err(MetaError::StaleContext {
                        fiber: owner.fiber,
                        generation: owner.generation,
                    });
                }
                data.active
                    .as_mut()
                    .ok_or(MetaError::StaleContext {
                        fiber: owner.fiber,
                        generation: owner.generation,
                    })?
                    .children
                    .push(id);
            }
            for service in &required_services {
                state
                    .dependents
                    .entry(service.clone())
                    .or_default()
                    .insert(id);
            }
            state.declarations.insert(
                id,
                &fiber.base_context,
                &fiber.data.lock().expect("fiber state poisoned").descriptor,
            );
            state.fibers.insert(id, Arc::clone(&fiber));
            state.revision += 1;
        }
        let (mut sender, receiver) = oneshot::channel();
        let runtime = self.clone();
        tokio::spawn(async move {
            let reconciliation_runtime = runtime.clone();
            let mut reconciliation = tokio::spawn(async move {
                reconciliation_runtime.reconcile_fiber(id).await;
            });
            tokio::select! {
                biased;
                () = sender.closed() => {
                    reconciliation.abort();
                    let _ = reconciliation.await;
                    let _ = runtime.dispose_fiber(id).await;
                }
                _ = &mut reconciliation => {
                    if matches!(fiber.snapshot().state, FiberState::Pending(_)) {
                        // A pending declaration may complete another pending
                        // fiber's cycle diagnostics. Active publication already
                        // notifies consumers with its actual provided services.
                        runtime.notify_service_changes(&declared_services, Some(id));
                    }
                    let handle = FiberHandle {
                        runtime: runtime.clone(),
                        fiber,
                    };
                    let (acknowledge, acknowledged) = oneshot::channel();
                    let rollback_runtime = Arc::downgrade(&runtime.inner);
                    if sender.send((handle, acknowledge)).is_err() {
                        let _ = runtime.dispose_fiber(id).await;
                    } else {
                        drop(runtime);
                        if acknowledged.await.is_err()
                            && let Some(inner) = rollback_runtime.upgrade()
                        {
                            let _ = Runtime { inner }.dispose_fiber(id).await;
                        }
                    }
                }
            }
        });
        let (handle, acknowledge) = receiver.await.map_err(|_| {
            MetaError::Activation("runtime-owned plugin application task failed".to_owned())
        })?;
        acknowledge.send(()).map_err(|()| {
            MetaError::Activation("plugin application ownership transfer failed".to_owned())
        })?;
        Ok(handle)
    }

    fn owner_fiber(&self, owner: Owner) -> Result<Arc<Fiber>> {
        {
            let state = self.inner.state.lock().expect("runtime state poisoned");
            state.fibers.get(&owner.fiber).cloned()
        }
        .ok_or(MetaError::StaleContext {
            fiber: owner.fiber,
            generation: owner.generation,
        })
    }

    fn validate_owner_data(owner: Owner, data: &FiberData, allow_loading: bool) -> Result<()> {
        let valid_state = matches!(data.state, FiberState::Active)
            || (allow_loading && matches!(data.state, FiberState::Loading))
            || matches!(data.state, FiberState::Unloading);
        if data.generation != owner.generation || !valid_state {
            return Err(MetaError::StaleContext {
                fiber: owner.fiber,
                generation: owner.generation,
            });
        }
        Ok(())
    }

    fn resolve_bindings(
        &self,
        fiber: &Fiber,
    ) -> std::result::Result<BTreeMap<ServiceKey, Arc<ProviderBinding>>, Vec<PendingReason>> {
        let descriptor = {
            let data = fiber.data.lock().expect("fiber state poisoned");
            data.descriptor.clone()
        };
        let mut bindings = BTreeMap::new();
        let mut pending = Vec::new();
        // One registry lock is one dependency snapshot. An activation must
        // never observe requirements from different publication revisions.
        let state = self.inner.state.lock().expect("runtime state poisoned");
        for requirement in descriptor.requires {
            let isolation = Self::isolation_for(&fiber.base_context.isolation, &requirement.key);
            let binding = state
                .providers
                .get(&ServiceSlot {
                    key: requirement.key.clone(),
                    isolation,
                })
                .cloned();
            let Some(binding) = binding else {
                pending.push(PendingReason::MissingService {
                    service: requirement.key,
                    isolation,
                });
                continue;
            };
            if binding.contract != requirement.contract || binding.version != requirement.version {
                pending.push(PendingReason::ContractMismatch {
                    service: requirement.key,
                    expected: requirement.contract,
                    expected_version: requirement.version,
                    actual: binding.contract.clone(),
                    actual_version: binding.version,
                });
                continue;
            }
            bindings.insert(requirement.key, binding);
        }
        drop(state);
        if !pending.is_empty()
            && let Some(services) = self.dependency_cycle(fiber.id)
        {
            pending.push(PendingReason::DependencyCycle { services });
        }
        if pending.is_empty() {
            Ok(bindings)
        } else {
            Err(pending)
        }
    }

    fn reconcile_fiber(&self, id: FiberId) -> BoxFuture<'static, ()> {
        let runtime = self.clone();
        Box::pin(async move {
            let fiber = {
                let state = runtime.inner.state.lock().expect("runtime state poisoned");
                state.fibers.get(&id).cloned()
            };
            let Some(fiber) = fiber else {
                return;
            };
            let _transition = fiber.transition.lock().await;
            if std::panic::AssertUnwindSafe(runtime.reconcile_fiber_inner(&fiber))
                .catch_unwind()
                .await
                .is_err()
            {
                let published = fiber
                    .data
                    .lock()
                    .expect("fiber state poisoned")
                    .active
                    .as_ref()
                    .is_some_and(|active| active.published);
                let cleanup = if published {
                    std::panic::AssertUnwindSafe(runtime.unload_generation(&fiber))
                        .catch_unwind()
                        .await
                } else {
                    std::panic::AssertUnwindSafe(runtime.rollback_loading(&fiber))
                        .catch_unwind()
                        .await
                };
                let message = match cleanup {
                    Ok(report) if report.is_clean() => "plugin activation panicked".to_owned(),
                    Ok(report) => format!(
                        "plugin activation panicked; cleanup also failed: {:?}",
                        report.failures
                    ),
                    Err(_) => "plugin activation and cleanup panicked".to_owned(),
                };
                fiber.set_state(FiberState::Failed(message));
            }
        })
    }

    async fn reconcile_fiber_inner(&self, fiber: &Arc<Fiber>) {
        let (disposed, active_bindings, active_revision, target_revision, last_attempt) = {
            let data = fiber.data.lock().expect("fiber state poisoned");
            (
                data.disposed,
                data.active
                    .as_ref()
                    .map(|active| binding_identities(&active.bindings)),
                data.active.as_ref().map(|active| active.target_revision),
                data.target_revision,
                data.last_attempt.clone(),
            )
        };
        if disposed {
            // Disposal owns teardown and its report. A concurrent reconciliation
            // must release the transition lock without consuming cleanup failures.
            return;
        }

        let bindings = match self.resolve_bindings(fiber) {
            Ok(bindings) => bindings,
            Err(reasons) => {
                if active_bindings.is_some() {
                    let cleanup = self.unload_generation(fiber).await;
                    if !cleanup.is_clean() {
                        fiber.set_state(FiberState::Failed(format!(
                            "dependency retirement cleanup failed: {:?}",
                            cleanup.failures
                        )));
                        return;
                    }
                }
                fiber.set_state(FiberState::Pending(reasons));
                return;
            }
        };
        let next_bindings = binding_identities(&bindings);
        let should_activate = {
            let data = fiber.data.lock().expect("fiber state poisoned");
            match (&data.state, active_bindings.as_ref()) {
                (FiberState::Active, Some(current)) => {
                    current != &next_bindings || active_revision != Some(target_revision)
                }
                (FiberState::Failed(_), _) => {
                    last_attempt.as_ref() != Some(&(target_revision, next_bindings.clone()))
                }
                (_, Some(current)) => current != &next_bindings,
                _ => true,
            }
        };
        if !should_activate {
            return;
        }
        if active_bindings.is_some() {
            let cleanup = self.unload_generation(fiber).await;
            if !cleanup.is_clean() {
                fiber.set_state(FiberState::Failed(format!(
                    "reconfiguration cleanup failed: {:?}",
                    cleanup.failures
                )));
                return;
            }
        }
        self.activate_generation(fiber, bindings).await;
    }

    async fn activate_generation(
        &self,
        fiber: &Arc<Fiber>,
        bindings: BTreeMap<ServiceKey, Arc<ProviderBinding>>,
    ) {
        let generation = self.next_generation();
        let (factory, config, target_revision) = {
            let mut data = fiber.data.lock().expect("fiber state poisoned");
            let target_revision = data.target_revision;
            let attempt = binding_identities(&bindings);
            data.generation = generation;
            data.state = FiberState::Loading;
            data.active = Some(GenerationData {
                generation,
                bindings,
                effects: Vec::new(),
                services: Vec::new(),
                listeners: Vec::new(),
                children: Vec::new(),
                lease: Arc::new(AdmissionLease::default()),
                published: false,
                target_revision,
            });
            data.last_attempt = Some((target_revision, attempt));
            let snapshot = data.snapshot(fiber.id);
            fiber.watch.send_replace(snapshot);
            (
                Arc::clone(&data.factory),
                data.config.clone(),
                data.target_revision,
            )
        };
        let context = fiber.context(generation);
        let activation = factory.activate(context, config);
        let result = tokio::time::timeout(self.inner.limits.transition_timeout, activation).await;
        let activation_error = match result {
            Ok(Ok(())) => None,
            Ok(Err(error)) => Some(error.to_string()),
            Err(_) => Some(MetaError::Timeout("plugin activation").to_string()),
        };
        if let Some(error) = activation_error {
            let cleanup = self.rollback_loading(fiber).await;
            let error = if cleanup.is_clean() {
                error
            } else {
                format!(
                    "{error}; activation rollback failed: {:?}",
                    cleanup.failures
                )
            };
            fiber.set_state(FiberState::Failed(error));
            return;
        }

        if let Err(error) = self.publish_generation(fiber, generation, target_revision) {
            let cleanup = self.rollback_loading(fiber).await;
            let error = if cleanup.is_clean() {
                error.to_string()
            } else {
                format!(
                    "{error}; publication rollback failed: {:?}",
                    cleanup.failures
                )
            };
            fiber.set_state(FiberState::Failed(error));
        }
    }

    fn publish_generation(
        &self,
        fiber: &Arc<Fiber>,
        generation: FiberGeneration,
        target_revision: u64,
    ) -> Result<()> {
        let mut state = self.inner.state.lock().expect("runtime state poisoned");
        if let Some(reason) = state.terminal.clone() {
            return Err(MetaError::RuntimeTerminal(reason));
        }
        let mut data = fiber.data.lock().expect("fiber state poisoned");
        if data.disposed || data.generation != generation || data.target_revision != target_revision
        {
            return Err(MetaError::StaleContext {
                fiber: fiber.id,
                generation,
            });
        }
        let services = {
            let active = data.active.as_ref().ok_or(MetaError::StaleContext {
                fiber: fiber.id,
                generation,
            })?;
            active.services.clone()
        };
        for binding in &services {
            let slot = ServiceSlot {
                key: binding.key.clone(),
                isolation: Self::isolation_for(&fiber.base_context.isolation, &binding.key),
            };
            if state.providers.contains_key(&slot) {
                return Err(MetaError::DuplicateProvider {
                    service: binding.key.clone(),
                });
            }
        }
        if state.providers.len() + services.len() > self.inner.limits.maximum_services {
            return Err(MetaError::CapacityExhausted {
                resource: "services",
            });
        }
        for binding in &services {
            let slot = ServiceSlot {
                key: binding.key.clone(),
                isolation: Self::isolation_for(&fiber.base_context.isolation, &binding.key),
            };
            state.providers.insert(slot, Arc::clone(binding));
        }
        let staged = state
            .staged_listeners
            .remove(&(fiber.id, generation))
            .unwrap_or_default();
        for listener in staged {
            let listeners = state.listeners.entry(listener.event.clone()).or_default();
            listeners.insert(listener);
        }
        data.active
            .as_mut()
            .expect("active generation exists")
            .published = true;
        data.state = FiberState::Active;
        let snapshot = data.snapshot(fiber.id);
        fiber.watch.send_replace(snapshot);
        state.revision += 1;
        let changed: Vec<ServiceKey> = services.iter().map(|binding| binding.key.clone()).collect();
        drop(data);
        drop(state);
        self.notify_service_changes(&changed, Some(fiber.id));
        Ok(())
    }

    fn isolation_for(
        isolations: &BTreeMap<ServiceKey, IsolationId>,
        key: &ServiceKey,
    ) -> IsolationId {
        isolations.get(key).copied().unwrap_or(IsolationId(0))
    }

    fn add_effect(&self, owner: Owner, label: String, cleanup: Cleanup) -> Result<()> {
        let fiber = self.owner_fiber(owner)?;
        let mut data = fiber.data.lock().expect("fiber state poisoned");
        Self::validate_owner_data(owner, &data, true)?;
        if !matches!(data.state, FiberState::Loading | FiberState::Active) {
            return Err(MetaError::StaleContext {
                fiber: owner.fiber,
                generation: owner.generation,
            });
        }
        let active = data.active.as_mut().ok_or(MetaError::StaleContext {
            fiber: owner.fiber,
            generation: owner.generation,
        })?;
        if active.effects.len() >= self.inner.limits.maximum_effects_per_fiber {
            return Err(MetaError::CapacityExhausted {
                resource: "effects",
            });
        }
        active.effects.push(EffectRecord { label, cleanup });
        Ok(())
    }

    fn provide(
        &self,
        context: &Context,
        key: ServiceKey,
        contract: ContractId,
        version: ContractVersion,
        endpoint: Arc<dyn ServiceEndpoint>,
    ) -> Result<()> {
        let owner = context.owner.ok_or_else(|| {
            MetaError::InvalidInput("the root context cannot provide a service".to_owned())
        })?;
        let fiber = self.owner_fiber(owner)?;
        let mut data = fiber.data.lock().expect("fiber state poisoned");
        Self::validate_owner_data(owner, &data, true)?;
        if !matches!(data.state, FiberState::Loading) {
            return Err(MetaError::InvalidInput(
                "services may only be provided during plugin activation".to_owned(),
            ));
        }
        let provision = data
            .descriptor
            .provides
            .iter()
            .find(|provision| provision.key == key)
            .ok_or_else(|| MetaError::UndeclaredProvision {
                service: key.clone(),
            })?;
        if provision.contract != contract || provision.version != version {
            return Err(MetaError::ContractMismatch {
                service: key,
                expected_id: provision.contract.clone(),
                expected_version: provision.version,
                actual_id: contract,
                actual_version: version,
            });
        }
        let active = data.active.as_mut().expect("loading generation exists");
        if active.services.iter().any(|binding| binding.key == key) {
            return Err(MetaError::DuplicateProvider { service: key });
        }
        active.services.push(Arc::new(ProviderBinding {
            key,
            contract,
            version,
            provider: owner.fiber,
            generation: owner.generation,
            endpoint,
            lease: Arc::clone(&active.lease),
        }));
        Ok(())
    }

    fn add_listener(
        &self,
        context: &Context,
        event: EventKey,
        handler: Arc<dyn EventHandler>,
        options: EventOptions,
    ) -> Result<EventListenerId> {
        let owner = context.owner.ok_or_else(|| {
            MetaError::InvalidInput("the root context cannot own a listener".to_owned())
        })?;
        let id = EventListenerId(self.inner.next_listener.fetch_add(1, Ordering::AcqRel) + 1);
        let mut state = self.inner.state.lock().expect("runtime state poisoned");
        let fiber = state
            .fibers
            .get(&owner.fiber)
            .cloned()
            .ok_or(MetaError::StaleContext {
                fiber: owner.fiber,
                generation: owner.generation,
            })?;
        if state.listener_events.len() >= self.inner.limits.maximum_event_listeners {
            return Err(MetaError::CapacityExhausted {
                resource: "event listeners",
            });
        }
        let mut data = fiber.data.lock().expect("fiber state poisoned");
        if data.generation != owner.generation
            || !matches!(data.state, FiberState::Loading | FiberState::Active)
        {
            return Err(MetaError::StaleContext {
                fiber: owner.fiber,
                generation: owner.generation,
            });
        }
        let loading = matches!(data.state, FiberState::Loading);
        let active = data.active.as_mut().ok_or(MetaError::StaleContext {
            fiber: owner.fiber,
            generation: owner.generation,
        })?;
        active.listeners.push(id);
        let listener = ListenerBinding {
            id,
            event: event.clone(),
            owner: owner.fiber,
            generation: owner.generation,
            scope: ContextScope {
                isolation: Arc::clone(&context.isolation),
                intercepts: Arc::clone(&context.intercepts),
                trace: context.trace.clone(),
            },
            handler,
            options,
            lease: Arc::clone(&active.lease),
        };
        state.listener_events.insert(id, event.clone());
        if loading {
            state
                .staged_listeners
                .entry((owner.fiber, owner.generation))
                .or_default()
                .push(listener);
        } else {
            state.listeners.entry(event).or_default().insert(listener);
        }
        state.revision += 1;
        Ok(id)
    }

    fn remove_listener(&self, context: &Context, id: EventListenerId) -> bool {
        let Some(owner) = context.owner else {
            return false;
        };
        let mut state = self.inner.state.lock().expect("runtime state poisoned");
        let Some(fiber) = state.fibers.get(&owner.fiber).cloned() else {
            return false;
        };
        let mut data = fiber.data.lock().expect("fiber state poisoned");
        if data.generation != owner.generation
            || !matches!(data.state, FiberState::Loading | FiberState::Active)
        {
            return false;
        }
        let Some(event) = state.listener_events.get(&id).cloned() else {
            return false;
        };
        let published = state
            .listeners
            .get_mut(&event)
            .and_then(|listeners| listeners.remove(id));
        let mut removed = published.as_ref().is_some_and(|listener| {
            listener.owner == owner.fiber && listener.generation == owner.generation
        });
        if !removed && let Some(listener) = published {
            state
                .listeners
                .entry(event.clone())
                .or_default()
                .insert(listener);
        }
        if !removed
            && let Some(listeners) = state
                .staged_listeners
                .get_mut(&(owner.fiber, owner.generation))
            && let Some(index) = listeners.iter().position(|listener| listener.id == id)
        {
            listeners.remove(index);
            removed = true;
        }
        if removed {
            state.listener_events.remove(&id);
            if let Some(active) = data.active.as_mut() {
                active.listeners.retain(|listener| *listener != id);
            }
            state.revision += 1;
        }
        removed
    }
}

impl Default for Runtime {
    fn default() -> Self {
        Self::new(RuntimeLimits::default()).expect("default runtime limits are valid")
    }
}
