use crate::listener_registry::{ListenerBinding, ListenerRegistry};
use crate::service::{AdmissionLease, BufferedByteAdmission, ProviderBinding};
use crate::{
    CallId, Cleanup, CleanupPhase, CleanupReport, ConfigValue, ContractId, ContractVersion,
    DispatchMode, EventHandler, EventKey, EventListenerId, EventOptions, EventOutcome,
    EventReceipt, FactoryIdentity, FiberGeneration, FiberId, InvocationContext, IsolationId,
    MetaError, PluginDescriptor, PluginFactory, Result, ServiceCall, ServiceEndpoint,
    ServiceHandle, ServiceKey, ShutdownOutcome, UnresolvedCleanup, UnresolvedCleanupReport,
};
use futures_util::FutureExt as _;
use futures_util::future::BoxFuture;
use futures_util::stream::StreamExt as _;
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fmt;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, Weak};
use tokio::sync::{Mutex as AsyncMutex, Notify, Semaphore, mpsc, watch};
use tokio_util::sync::CancellationToken;

mod admission;
mod configuration;
mod context_api;
mod context_scope;
mod declaration_index;
mod dispatch;
mod lifecycle;
mod limits;
mod ownership;
mod pending_report;
mod preparation;
mod reconciliation_queue;
mod service_bridge;
mod state;

use context_api::binding_identities;
pub(crate) use context_scope::InterceptLayers;
use declaration_index::DeclarationIndex;
pub use limits::{
    DeadlineLimits, ExecutionLimits, MAXIMUM_JSON_DEPTH, MAXIMUM_OPERATION_DEADLINE, PayloadLimits,
    ResourceUsageSnapshot, RuntimeLimits, RuntimeResourceSnapshot, TopologyLimits,
};
pub(crate) use limits::{ResourceLedger, ResourceReservation};
use limits::{RuntimeResources, ValidatedRuntimeLimits};
use pending_report::PendingReportBuilder;
pub use preparation::PreparedPlugin;
use preparation::{PreparedDescriptor, PreparedReservations};
pub use state::{FiberSnapshot, FiberState, PendingReason, PendingReport, RuntimeSnapshot};

const ROOT_FIBER: FiberId = FiberId(0);

fn drop_catching_unwind<T>(value: T) -> bool {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| drop(value))).is_err()
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
    limits: ValidatedRuntimeLimits,
    resources: RuntimeResources,
    state: Mutex<RuntimeState>,
    shutting_down: AtomicBool,
    shutdown: ShutdownRun,
    runtime_admission: Arc<AdmissionLease>,
    terminal_cancellation: CancellationToken,
    scheduler_idle: Notify,
    scheduler_wakeup: Notify,
    paused_reconciliations: AtomicUsize,
    reconciliation_admission: Arc<Semaphore>,
    service_call_admission: Arc<Semaphore>,
    service_byte_admission: Arc<BufferedByteAdmission>,
    event_callback_admission: Arc<Semaphore>,
    next_fiber: AtomicU64,
    next_generation: AtomicU64,
    next_isolation: AtomicU64,
    next_listener: AtomicU64,
    next_call: AtomicU64,
}

struct RuntimeState {
    revision: u64,
    fibers: BTreeMap<FiberId, Arc<Fiber>>,
    dependents: HashMap<ServiceSlot, BTreeSet<FiberId>>,
    declarations: DeclarationIndex,
    providers: HashMap<ServiceSlot, Arc<ProviderBinding>>,
    listeners: HashMap<EventKey, ListenerRegistry>,
    listener_events: HashMap<EventListenerId, EventKey>,
    staged_listeners:
        HashMap<(FiberId, FiberGeneration), BTreeMap<EventListenerId, Arc<ListenerBinding>>>,
    pending_reconciliations: BTreeSet<FiberId>,
    active_reconciliations: BTreeSet<FiberId>,
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
    depth: usize,
    runtime: Weak<RuntimeInner>,
    executor: tokio::runtime::Handle,
    parent: Option<Owner>,
    base_context: ContextScope,
    configuration: Arc<AsyncMutex<()>>,
    transition: AsyncMutex<()>,
    reconciliation: ReconciliationProgress,
    apply_cancellation: CancellationToken,
    disposal_requested: CancellationToken,
    disposal: Arc<DisposalRun>,
    cleanup_phase: Mutex<CleanupPhase>,
    data: Mutex<FiberData>,
    watch: watch::Sender<FiberSnapshot>,
}

struct FiberData {
    identity: FactoryIdentity,
    factory: Option<Arc<dyn PluginFactory>>,
    descriptor: Option<Arc<PreparedDescriptor>>,
    config: Option<Arc<RetainedConfig>>,
    reservations: Option<PreparedReservations>,
    target_revision: u64,
    generation: FiberGeneration,
    state: FiberState,
    disposed: bool,
    active: Option<GenerationData>,
    last_attempt: Option<ActivationAttempt>,
}

struct RetainedConfig {
    value: Arc<ConfigValue>,
    _reservation: ResourceReservation,
}

impl RetainedConfig {
    fn new(value: ConfigValue, reservation: ResourceReservation) -> Self {
        Self {
            value: Arc::new(value),
            _reservation: reservation,
        }
    }
}

type BindingIdentities = BTreeMap<ServiceKey, (FiberId, FiberGeneration)>;
type ActivationAttempt = (u64, BindingIdentities);

struct GenerationData {
    generation: FiberGeneration,
    bindings: BTreeMap<ServiceKey, Arc<ProviderBinding>>,
    activation_cancellation: CancellationToken,
    effects: Vec<EffectRecord>,
    services: BTreeMap<ServiceKey, StagedService>,
    listeners: BTreeMap<EventListenerId, ResourceReservation>,
    children: Vec<Arc<Fiber>>,
    retired_child_report: CleanupReport,
    cleanup: Arc<CleanupRun>,
    lease: Arc<AdmissionLease>,
    published: bool,
    target_revision: u64,
}

struct StagedService {
    binding: Arc<ProviderBinding>,
    _reservation: ResourceReservation,
}

struct EffectRecord {
    label: String,
    cleanup: Cleanup,
    _reservation: ResourceReservation,
}

struct ClaimedCleanup {
    generation: FiberGeneration,
    services: Vec<Arc<ProviderBinding>>,
    listener_ids: BTreeSet<EventListenerId>,
    published: bool,
    lease: Arc<AdmissionLease>,
    children: Vec<Arc<Fiber>>,
    retired_child_report: CleanupReport,
    effects: Vec<EffectRecord>,
}

struct ReconciliationProgress {
    desired: AtomicU64,
    settled: AtomicU64,
    watch: watch::Sender<u64>,
    completions: Mutex<BTreeMap<u64, Vec<Weak<ReconciliationCompletion>>>>,
}

struct ReconciliationTicket {
    receiver: watch::Receiver<u64>,
    completion: Arc<ReconciliationCompletion>,
}

#[derive(Default)]
struct ReconciliationCompletion {
    snapshot: Mutex<Option<FiberSnapshot>>,
}

struct PendingApplyOwnership {
    runtime: Weak<RuntimeInner>,
    fiber: Arc<Fiber>,
    armed: bool,
}

#[derive(Default)]
struct CleanupRun {
    started: AtomicBool,
    result: Mutex<Option<CleanupReport>>,
    complete: Notify,
}

#[derive(Clone)]
struct DisposalResult {
    report: CleanupReport,
    quiescent: bool,
}

#[derive(Default)]
struct DisposalRun {
    started: AtomicBool,
    result: Mutex<Option<DisposalResult>>,
    complete: Notify,
}

#[derive(Default)]
struct ShutdownRun {
    state: Mutex<ShutdownRunState>,
    complete: Notify,
}

#[derive(Default)]
struct ShutdownRunState {
    report: CleanupReport,
    outcome: Option<CleanupReport>,
    failed: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct Owner {
    fiber: FiberId,
    generation: FiberGeneration,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ListenerRemovalCause {
    Explicit,
    Once,
    Retirement,
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
    entries: usize,
    encoded_bytes: usize,
    trace: Option<CallTrace>,
}

/// Immutable scoped capability used to apply plugins and access owned resources.
#[derive(Clone)]
pub struct Context {
    runtime: Runtime,
    owner: Option<Owner>,
    isolation: Arc<BTreeMap<ServiceKey, IsolationId>>,
    intercepts: Arc<BTreeMap<ServiceKey, Arc<InterceptLayers>>>,
    entries: usize,
    encoded_bytes: usize,
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

impl Runtime {
    /// Creates an empty Runtime after validating capacities, arithmetic, and deadlines.
    pub fn new(limits: RuntimeLimits) -> Result<Self> {
        let limits = ValidatedRuntimeLimits::new(limits)?;
        let resources = RuntimeResources::new(limits.configured());
        let reconciliation_admission = Arc::new(Semaphore::new(
            limits.execution.maximum_concurrent_reconciliations,
        ));
        let service_call_admission = Arc::new(Semaphore::new(
            limits.execution.maximum_concurrent_service_calls,
        ));
        let service_byte_admission = Arc::new(BufferedByteAdmission::new(
            limits.payloads.maximum_buffered_service_bytes,
        ));
        let event_callback_admission = Arc::new(Semaphore::new(
            limits.execution.maximum_concurrent_event_callbacks,
        ));
        Ok(Self {
            inner: Arc::new(RuntimeInner {
                limits,
                resources,
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
                    active_reconciliations: BTreeSet::new(),
                    reconciliation_worker_running: false,
                    terminal: None,
                }),
                shutting_down: AtomicBool::new(false),
                shutdown: ShutdownRun::default(),
                runtime_admission: Arc::new(AdmissionLease::default()),
                terminal_cancellation: CancellationToken::new(),
                scheduler_idle: Notify::new(),
                scheduler_wakeup: Notify::new(),
                paused_reconciliations: AtomicUsize::new(0),
                reconciliation_admission,
                service_call_admission,
                service_byte_admission,
                event_callback_admission,
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
            entries: 0,
            encoded_bytes: 0,
            trace: None,
        }
    }

    /// Returns the immutable limits selected at construction.
    pub fn limits(&self) -> &RuntimeLimits {
        self.inner.limits.configured()
    }

    /// Captures current and peak logical Runtime resource usage.
    pub fn resource_snapshot(&self) -> RuntimeResourceSnapshot {
        self.inner.resources.snapshot()
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
        self.mark_terminal_owned(reason);
    }

    pub(super) fn mark_terminal_owned(&self, reason: impl Into<String>) {
        let reason = dispatch::bound_owned_diagnostic(
            reason.into(),
            self.inner.limits.payloads.maximum_diagnostic_bytes,
        );
        {
            // Keep the lock order aligned with shutdown completion. Once a
            // quiescent outcome is cached, terminalization must not mutate it;
            // before then, the first terminal reason remains diagnostic state.
            let mut state = self.inner.state.lock().expect("runtime state poisoned");
            if self.inner.shutdown.is_complete() {
                return;
            }
            state.terminal.get_or_insert(reason);
        }
        self.inner.runtime_admission.close();
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

    #[allow(clippy::too_many_lines)] // Validation proof consumption and atomic ownership insertion stay adjacent.
    async fn apply_prepared(
        &self,
        parent: &Context,
        prepared: PreparedPlugin,
    ) -> Result<FiberHandle> {
        let PreparedPlugin {
            runtime,
            admission,
            factory,
            descriptor,
            config,
            reservations,
        } = prepared;
        if !runtime.ptr_eq(&Arc::downgrade(&self.inner)) {
            return Err(MetaError::PreparedForDifferentRuntime);
        }
        self.ensure_admitting()?;
        let depth = if let Some(owner) = parent.owner {
            let state = self.inner.state.lock().expect("runtime state poisoned");
            let parent_fiber = state
                .fibers
                .get(&owner.fiber)
                .ok_or(MetaError::StaleContext {
                    fiber: owner.fiber,
                    generation: owner.generation,
                })?;
            let data = parent_fiber.data.lock().expect("fiber state poisoned");
            if data.generation != owner.generation
                || !matches!(data.state, FiberState::Loading | FiberState::Active)
            {
                return Err(MetaError::StaleContext {
                    fiber: owner.fiber,
                    generation: owner.generation,
                });
            }
            parent_fiber
                .depth
                .checked_add(1)
                .ok_or(MetaError::CapacityExhausted {
                    resource: "fiber depth",
                })?
        } else {
            1
        };
        if depth > self.inner.limits.topology.maximum_fiber_depth {
            return Err(MetaError::CapacityExhausted {
                resource: "fiber depth",
            });
        }
        let base_context = ContextScope {
            isolation: Arc::clone(&parent.isolation),
            intercepts: Arc::clone(&parent.intercepts),
            entries: parent.entries,
            encoded_bytes: parent.encoded_bytes,
            trace: parent.trace.clone(),
        };
        let declared_slots = descriptor
            .provided_services()
            .map(|service| base_context.service_slot(service))
            .collect::<Vec<_>>();
        let required_slots = descriptor
            .required_services()
            .map(|service| base_context.service_slot(service))
            .collect::<Vec<_>>();

        let id = FiberId(self.inner.next_fiber.fetch_add(1, Ordering::AcqRel) + 1);
        let initial = FiberSnapshot {
            id,
            generation: FiberGeneration(0),
            factory: descriptor.identity.clone(),
            state: FiberState::Pending(PendingReport::default()),
        };
        let (watch, _) = watch::channel(initial);
        let (reconciliation_watch, _) = watch::channel(0_u64);
        let fiber = Arc::new(Fiber {
            id,
            depth,
            runtime: Arc::downgrade(&self.inner),
            executor: tokio::runtime::Handle::current(),
            parent: parent.owner,
            base_context,
            configuration: Arc::new(AsyncMutex::new(())),
            transition: AsyncMutex::new(()),
            reconciliation: ReconciliationProgress {
                desired: AtomicU64::new(0),
                settled: AtomicU64::new(0),
                watch: reconciliation_watch,
                completions: Mutex::new(BTreeMap::new()),
            },
            apply_cancellation: CancellationToken::new(),
            disposal_requested: CancellationToken::new(),
            disposal: Arc::new(DisposalRun::default()),
            cleanup_phase: Mutex::new(CleanupPhase::Scheduled),
            data: Mutex::new(FiberData {
                identity: descriptor.identity.clone(),
                factory: Some(factory),
                descriptor: Some(descriptor),
                config: Some(config),
                reservations: Some(reservations),
                target_revision: 1,
                generation: FiberGeneration(0),
                state: FiberState::Pending(PendingReport::default()),
                disposed: false,
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
                    .push(Arc::clone(&fiber));
            }
            for slot in &required_slots {
                state.dependents.entry(slot.clone()).or_default().insert(id);
            }
            state.declarations.insert(
                id,
                &fiber.base_context,
                fiber
                    .data
                    .lock()
                    .expect("fiber state poisoned")
                    .descriptor
                    .as_deref()
                    .expect("registered Fiber retains its descriptor"),
            );
            state.fibers.insert(id, Arc::clone(&fiber));
            state.revision += 1;
        }
        // Registry membership now owns every durable reservation and is part
        // of the shutdown root snapshot; the proof's external admission can
        // be released before reconciliation continues.
        drop(admission);
        let mut ownership = PendingApplyOwnership {
            runtime: Arc::downgrade(&self.inner),
            fiber: Arc::clone(&fiber),
            armed: true,
        };
        self.yield_reconciliation_slot(async {
            if let Some(ticket) = self.request_reconciliation(id) {
                self.drive_nested_intent(id).await;
                ticket.join().await;
            }
        })
        .await;
        if matches!(fiber.snapshot().state, FiberState::Pending(_)) {
            // A pending declaration may complete another pending fiber's cycle
            // diagnostics. Active publication already notifies consumers with
            // its actual provided services.
            self.refresh_pending_diagnostics(&declared_slots, Some(id));
        }
        ownership.armed = false;
        Ok(FiberHandle {
            runtime: self.clone(),
            fiber,
        })
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
    ) -> std::result::Result<BTreeMap<ServiceKey, Arc<ProviderBinding>>, PendingReport> {
        let descriptor = {
            let data = fiber.data.lock().expect("fiber state poisoned");
            Arc::clone(
                data.descriptor
                    .as_ref()
                    .expect("registered Fiber retains its descriptor"),
            )
        };
        let slots = descriptor
            .requires
            .iter()
            .enumerate()
            .map(|(index, requirement)| {
                let isolation =
                    Self::isolation_for(&fiber.base_context.isolation, &requirement.key);
                (
                    index,
                    isolation,
                    fiber.base_context.service_slot(&requirement.key),
                )
            })
            .collect::<Vec<_>>();
        // One registry lock is one dependency snapshot. An activation must
        // never observe requirements from different publication revisions. The
        // lock only performs keyed Arc lookups; diagnostics and comparison stay
        // outside the global mutation boundary.
        let candidates = {
            let state = self.inner.state.lock().expect("runtime state poisoned");
            slots
                .iter()
                .map(|(_, _, slot)| state.providers.get(slot).cloned())
                .collect::<Vec<_>>()
        };
        let mut bindings = BTreeMap::new();
        let mut pending = PendingReportBuilder::new(&self.inner.limits.payloads);
        for ((index, isolation, _), binding) in slots.into_iter().zip(candidates) {
            let requirement = &descriptor.requires[index];
            let Some(binding) = binding else {
                pending.push_with(1, requirement.key.as_str().len(), || {
                    PendingReason::MissingService {
                        service: requirement.key.clone(),
                        isolation,
                    }
                });
                continue;
            };
            if binding.contract != requirement.contract || binding.version != requirement.version {
                let retained_bytes = requirement
                    .key
                    .as_str()
                    .len()
                    .saturating_add(requirement.contract.as_str().len())
                    .saturating_add(binding.contract.as_str().len());
                pending.push_with(1, retained_bytes, || PendingReason::ContractMismatch {
                    service: requirement.key.clone(),
                    expected: requirement.contract.clone(),
                    expected_version: requirement.version,
                    actual: binding.contract.clone(),
                    actual_version: binding.version,
                });
                continue;
            }
            bindings.insert(requirement.key.clone(), binding);
        }
        if pending.total_reasons() != 0
            && let Some((services, truncated)) = self.dependency_cycle(
                fiber.id,
                pending.remaining_cycle_services(),
                pending.remaining_bytes(),
            )
        {
            pending.push_cycle(services, truncated);
        }
        if pending.total_reasons() == 0 {
            Ok(bindings)
        } else {
            Err(pending.finish())
        }
    }

    fn reconcile_fiber(&self, fiber: Arc<Fiber>) -> BoxFuture<'static, ()> {
        let runtime = self.clone();
        Box::pin(async move {
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
                let maximum = runtime.inner.limits.payloads.maximum_diagnostic_bytes;
                let message = match cleanup {
                    Ok(report) if report.is_clean() => "plugin activation panicked".to_owned(),
                    Ok(report) => dispatch::bound_formatted_diagnostic(
                        format_args!(
                            "plugin activation panicked; cleanup also failed: {:?}",
                            report.failures
                        ),
                        maximum,
                    ),
                    Err(_) => "plugin activation and cleanup panicked".to_owned(),
                };
                fiber.set_state(FiberState::Failed(dispatch::bound_owned_diagnostic(
                    message, maximum,
                )));
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
                        fiber.set_state(FiberState::Failed(dispatch::bound_formatted_diagnostic(
                            format_args!(
                                "dependency retirement cleanup failed: {:?}",
                                cleanup.failures
                            ),
                            self.inner.limits.payloads.maximum_diagnostic_bytes,
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
                fiber.set_state(FiberState::Failed(dispatch::bound_formatted_diagnostic(
                    format_args!("reconfiguration cleanup failed: {:?}", cleanup.failures),
                    self.inner.limits.payloads.maximum_diagnostic_bytes,
                )));
                return;
            }
        }
        self.activate_generation(fiber, bindings).await;
    }

    #[allow(clippy::too_many_lines)] // Activation, rollback, and publication are one generation transaction.
    async fn activate_generation(
        &self,
        fiber: &Arc<Fiber>,
        bindings: BTreeMap<ServiceKey, Arc<ProviderBinding>>,
    ) {
        let generation = self.next_generation();
        let activation_cancellation = CancellationToken::new();
        let (factory, config, target_revision) = {
            let mut data = fiber.data.lock().expect("fiber state poisoned");
            let target_revision = data.target_revision;
            let attempt = binding_identities(&bindings);
            data.generation = generation;
            data.state = FiberState::Loading;
            data.active = Some(GenerationData {
                generation,
                bindings,
                activation_cancellation: activation_cancellation.clone(),
                effects: Vec::new(),
                services: BTreeMap::new(),
                listeners: BTreeMap::new(),
                children: Vec::new(),
                retired_child_report: CleanupReport::default(),
                cleanup: Arc::new(CleanupRun::default()),
                lease: Arc::new(AdmissionLease::default()),
                published: false,
                target_revision,
            });
            data.last_attempt = Some((target_revision, attempt));
            let snapshot = data.snapshot(fiber.id);
            fiber.watch.send_replace(snapshot);
            (
                Arc::clone(
                    data.factory
                        .as_ref()
                        .expect("registered Fiber retains its factory"),
                ),
                Arc::clone(
                    data.config
                        .as_ref()
                        .expect("registered Fiber retains its configuration"),
                ),
                data.target_revision,
            )
        };
        let context = fiber.context(generation);
        // Keep the sidecar reservation in the same future as plugin activation.
        // The post-await drop also keeps it live while cancellation destroys a
        // plugin future whose destructor may still own the shared Value.
        let activation = async move {
            let result = factory.activate(context, Arc::clone(&config.value)).await;
            drop(config);
            result
        };
        // Plugin code may synchronously await another scheduler-backed
        // operation through a service or a spawned task. It owns no registry
        // mutation while awaiting, so transfer the slot until its result is
        // ready and reacquire before publication or rollback.
        let result = self
            .yield_reconciliation_slot(async {
                tokio::select! {
                    biased;
                    () = fiber.apply_cancellation.cancelled() => {
                        None
                    }
                    () = activation_cancellation.cancelled() => {
                        None
                    }
                    result = tokio::time::timeout(
                        self.inner.limits.deadlines.transition,
                        activation,
                    ) => {
                        Some(result)
                    }
                }
            })
            .await;
        let maximum = self.inner.limits.payloads.maximum_diagnostic_bytes;
        let activation_error = match result {
            Some(Ok(Ok(()))) => None,
            Some(Ok(Err(error))) => Some(dispatch::bound_formatted_diagnostic(
                format_args!("{error}"),
                maximum,
            )),
            Some(Err(_)) => Some(dispatch::bound_formatted_diagnostic(
                format_args!("{}", MetaError::Timeout("plugin activation")),
                maximum,
            )),
            None => Some(dispatch::bound_formatted_diagnostic(
                format_args!("{}", MetaError::Cancelled),
                maximum,
            )),
        };
        if let Some(error) = activation_error {
            let cleanup = self.rollback_loading(fiber).await;
            let error = if cleanup.is_clean() {
                error
            } else {
                dispatch::bound_formatted_diagnostic(
                    format_args!(
                        "{error}; activation rollback failed: {:?}",
                        cleanup.failures
                    ),
                    maximum,
                )
            };
            fiber.set_state(FiberState::Failed(error));
            return;
        }

        if let Err(error) = self.publish_generation(fiber, generation, target_revision) {
            let cleanup = self.rollback_loading(fiber).await;
            let error = if cleanup.is_clean() {
                dispatch::bound_formatted_diagnostic(format_args!("{error}"), maximum)
            } else {
                dispatch::bound_formatted_diagnostic(
                    format_args!(
                        "{error}; publication rollback failed: {:?}",
                        cleanup.failures
                    ),
                    maximum,
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
            if active.activation_cancellation.is_cancelled() {
                return Err(MetaError::Cancelled);
            }
            active
                .services
                .values()
                .map(|service| {
                    let binding = Arc::clone(&service.binding);
                    let slot = fiber.base_context.service_slot(&binding.key);
                    (slot, binding)
                })
                .collect::<Vec<_>>()
        };
        for (slot, binding) in &services {
            if state.providers.contains_key(slot) {
                return Err(MetaError::DuplicateProvider {
                    service: binding.key.clone(),
                });
            }
        }
        for (slot, binding) in &services {
            state.providers.insert(slot.clone(), Arc::clone(binding));
        }
        let staged = state
            .staged_listeners
            .remove(&(fiber.id, generation))
            .unwrap_or_default();
        for listener in staged.into_values() {
            let listeners = state.listeners.entry(listener.event.clone()).or_default();
            listeners.insert(listener);
        }
        data.active
            .as_mut()
            .expect("active generation exists")
            .published = true;
        data.last_attempt = None;
        data.state = FiberState::Active;
        let snapshot = data.snapshot(fiber.id);
        fiber.watch.send_replace(snapshot);
        state.revision += 1;
        let changed = services
            .into_iter()
            .map(|(slot, _binding)| slot)
            .collect::<Vec<_>>();
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
}

impl Default for Runtime {
    fn default() -> Self {
        Self::new(RuntimeLimits::default()).expect("default runtime limits are valid")
    }
}
