use crate::listener_registry::{ListenerBinding, ListenerRegistry};
use crate::service::{AdmissionLease, BufferedMessageAdmission, ProviderBinding};
use crate::{
    ActivationPlan, CallId, Capability, CapabilityCall, Cleanup, CleanupPhase, CleanupReport,
    ConfigValue, ContractId, ContractVersion, DispatchMode, EventHandler, EventKey,
    EventListenerId, EventOptions, EventOutcome, EventReceipt, EventTarget, FactoryIdentity,
    FiberGeneration, FiberId, InvocationContext, IsolationId, ListenerView, MetaError,
    PluginFactory, Result, ServiceEndpoint, ServiceKey, ShutdownOutcome, SupplyId,
    UnresolvedCleanup, UnresolvedCleanupReport,
};
use futures_util::FutureExt as _;
use futures_util::future::BoxFuture;
use futures_util::stream::StreamExt as _;
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet, HashMap, VecDeque};
use std::fmt;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, Weak};
use tokio::sync::{Mutex as AsyncMutex, Notify, Semaphore, mpsc, watch};
use tokio_util::sync::CancellationToken;

mod activation_driver;
mod admission;
mod attempts;
mod call_identity;
mod capabilities;
mod configuration;
mod context_api;
mod context_scope;
mod dispatch;
mod effects;
mod event_callback_driver;
mod extensions;
mod generation_activation;
mod lifecycle;
mod limits;
mod ownership;
mod panic_containment;
mod pending_report;
mod preparation;
mod reconciliation_queue;
mod service_bridge;
mod state;
mod supplies;

pub use capabilities::DetachedCapability;
use capabilities::GenerationCapabilitySet;
pub(crate) use capabilities::{CapabilityEntry, CapabilityUse};
use context_api::binding_identities;
pub(crate) use context_scope::InterceptLayers;
pub(crate) use effects::CallbackLease;
pub use effects::{CallerEffect, EffectHandle, EffectTxn};
use effects::{EffectRecord, EffectRetention, EffectScope, GenerationBudget, OwnedEffect};
pub use extensions::ContextExtension;
pub(crate) use extensions::ContextExtensions;
pub use limits::{
    DeadlineLimits, ExecutionLimits, MAXIMUM_JSON_DEPTH, MAXIMUM_OPERATION_DEADLINE, PayloadLimits,
    ResourceUsageSnapshot, RuntimeLimits, RuntimeResourceSnapshot, TopologyLimits,
};
pub(crate) use limits::{ResourceLedger, ResourceReservation};
use limits::{RuntimeResources, ValidatedRuntimeLimits};
pub use ownership::EventHandle;
pub(crate) use ownership::EventOwnership;
pub(crate) use panic_containment::{contain_panic_result, drop_catching_unwind};
use pending_report::PendingReportBuilder;
pub use preparation::PreparedPlugin;
use preparation::{DesiredConfig, FiberReservation, PreparedAttempt, RetainedFactory};
pub use state::{FiberSnapshot, FiberState, PendingReason, PendingReport, RuntimeSnapshot};
pub use supplies::SupplyHandle;
use supplies::{ServiceSlot, SupplyEntry, SupplyVisibility};

const ROOT_FIBER: FiberId = FiberId(0);

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
    preparation_admission: Arc<Semaphore>,
    reconciliation_admission: Arc<Semaphore>,
    service_call_admission: Arc<Semaphore>,
    message_admission: Arc<BufferedMessageAdmission>,
    event_callback_admission: Arc<Semaphore>,
    next_fiber: AtomicU64,
    next_generation: AtomicU64,
    next_isolation: AtomicU64,
    next_listener: AtomicU64,
    next_capability_entry: AtomicU64,
    next_call: AtomicU64,
    next_effect: AtomicU64,
    next_supply: AtomicU64,
    next_attempt: AtomicU64,
}

struct RuntimeState {
    revision: u64,
    fibers: BTreeMap<FiberId, Arc<Fiber>>,
    dependents: HashMap<ServiceSlot, BTreeSet<FiberId>>,
    providers: HashMap<ServiceSlot, SupplyEntry>,
    listeners: HashMap<EventKey, ListenerRegistry>,
    listener_events: HashMap<EventListenerId, EventKey>,
    reconciliations: ReconciliationFrontier,
    reconciliation_worker_running: bool,
    terminal: Option<String>,
}

#[derive(Default)]
struct ReconciliationFrontier {
    ready: BTreeMap<FiberId, u64>,
    ready_order: VecDeque<(FiberId, u64)>,
    next_ready_token: u64,
    active: BTreeSet<FiberId>,
    rerun: BTreeSet<FiberId>,
}

impl RuntimeState {
    fn advance_revision(&mut self) {
        self.revision = self.revision.saturating_add(1);
    }
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
    factory: Option<RetainedFactory>,
    desired: Option<DesiredConfig>,
    attempt: Option<PreparedAttempt>,
    replacement: Option<PreparedAttempt>,
    fiber_reservation: Option<FiberReservation>,
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
    fn new_validated(value: ConfigValue, reservation: ResourceReservation) -> Self {
        Self {
            value: Arc::new(value),
            _reservation: reservation,
        }
    }

    fn as_value(&self) -> &ConfigValue {
        self.value.as_ref()
    }
}

type BindingIdentities = BTreeMap<ServiceKey, SupplyId>;
type ActivationAttempt = (u64, BindingIdentities);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct AttemptStamp {
    id: u64,
    desired_revision: u64,
    consumed: bool,
}

impl AttemptStamp {
    fn permits_loading_install(
        self,
        current: Option<Self>,
        target_revision: u64,
        disposed: bool,
        disposal_requested: bool,
    ) -> bool {
        !disposed
            && !disposal_requested
            && !self.consumed
            && current == Some(self)
            && target_revision == self.desired_revision
    }
}

impl From<&PreparedAttempt> for AttemptStamp {
    fn from(attempt: &PreparedAttempt) -> Self {
        Self {
            id: attempt.id,
            desired_revision: attempt.desired_revision,
            consumed: attempt.consumed,
        }
    }
}

struct ResolvedBindings {
    attempt: AttemptStamp,
    bindings: BTreeMap<ServiceKey, Arc<ProviderBinding>>,
}

struct GenerationData {
    generation: FiberGeneration,
    attempt_id: u64,
    bindings: BTreeMap<ServiceKey, Arc<ProviderBinding>>,
    activation_cancellation: CancellationToken,
    effects: BTreeMap<u64, Arc<EffectRecord>>,
    effect_budget: Arc<GenerationBudget>,
    effect_transaction_budget: Arc<GenerationBudget>,
    services: BTreeMap<ServiceSlot, StagedService>,
    listeners: BTreeMap<EventListenerId, ResourceReservation>,
    children: Vec<Arc<Fiber>>,
    retired_owned_report: CleanupReport,
    cleanup: Arc<CleanupRun>,
    capabilities: Arc<GenerationCapabilitySet>,
    lease: Arc<AdmissionLease>,
    published: bool,
    target_revision: u64,
}

struct StagedService {
    binding: Arc<ProviderBinding>,
    _reservation: ResourceReservation,
}

struct ClaimedCleanup {
    generation: FiberGeneration,
    services: Vec<(ServiceSlot, Arc<ProviderBinding>)>,
    listener_ids: BTreeSet<EventListenerId>,
    capabilities: Arc<GenerationCapabilitySet>,
    lease: Arc<AdmissionLease>,
    children: Vec<Arc<Fiber>>,
    retired_owned_report: CleanupReport,
    effects: Vec<Arc<EffectRecord>>,
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

#[derive(Clone, Debug)]
struct CallTrace {
    origin: FiberId,
    lineage_call: CallId,
    parent_call: Option<CallId>,
}

#[derive(Clone, Debug)]
pub(crate) struct ContextScope {
    isolation: Arc<BTreeMap<ServiceKey, IsolationId>>,
    intercepts: Arc<BTreeMap<ServiceKey, Arc<InterceptLayers>>>,
    extensions: Arc<ContextExtensions>,
    entries: usize,
    encoded_bytes: usize,
    trace: Option<CallTrace>,
}

/// Immutable scoped capability used to apply plugins and access owned resources.
#[derive(Clone)]
pub struct Context {
    runtime: Runtime,
    owner: Option<Owner>,
    setup_effect: Option<EffectScope>,
    isolation: Arc<BTreeMap<ServiceKey, IsolationId>>,
    intercepts: Arc<BTreeMap<ServiceKey, Arc<InterceptLayers>>>,
    extensions: Arc<ContextExtensions>,
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

impl Context {
    pub(crate) fn install_activation_lineage(&mut self, origin: FiberId, lineage: CallId) {
        debug_assert_ne!(lineage, CallId(0));
        self.trace = Some(CallTrace {
            origin,
            lineage_call: lineage,
            parent_call: None,
        });
    }

    pub(crate) fn activation_lineage(&self) -> Option<CallId> {
        self.trace.as_ref().map(|trace| trace.lineage_call)
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
        let preparation_admission = Arc::new(Semaphore::new(
            limits.execution.maximum_concurrent_preparations,
        ));
        let reconciliation_admission = Arc::new(Semaphore::new(
            limits.execution.maximum_concurrent_reconciliations,
        ));
        let service_call_admission = Arc::new(Semaphore::new(
            limits.execution.maximum_concurrent_service_calls,
        ));
        let message_admission = Arc::new(BufferedMessageAdmission::new(
            limits.payloads.maximum_buffered_message_bytes,
            limits.topology.maximum_queued_capability_references,
            Arc::clone(&resources.pending_message_sends),
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
                    providers: HashMap::new(),
                    listeners: HashMap::new(),
                    listener_events: HashMap::new(),
                    reconciliations: ReconciliationFrontier::default(),
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
                preparation_admission,
                reconciliation_admission,
                service_call_admission,
                message_admission,
                event_callback_admission,
                next_fiber: AtomicU64::new(0),
                next_generation: AtomicU64::new(0),
                next_isolation: AtomicU64::new(0),
                next_listener: AtomicU64::new(0),
                next_capability_entry: AtomicU64::new(0),
                next_call: AtomicU64::new(0),
                next_effect: AtomicU64::new(0),
                next_supply: AtomicU64::new(0),
                next_attempt: AtomicU64::new(0),
            }),
        })
    }

    /// Creates an unowned root Context retaining this Runtime.
    pub fn root(&self) -> Context {
        Context {
            runtime: self.clone(),
            owner: None,
            setup_effect: None,
            isolation: Arc::new(BTreeMap::new()),
            intercepts: Arc::new(BTreeMap::new()),
            extensions: Arc::new(ContextExtensions::default()),
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

    #[allow(clippy::too_many_lines)] // Validation proof consumption and atomic ownership insertion stay adjacent.
    async fn apply_prepared(
        &self,
        parent: &Context,
        prepared: PreparedPlugin,
    ) -> Result<FiberHandle> {
        let PreparedPlugin {
            runtime,
            admission,
            identity,
            factory,
            desired,
            attempt,
            fiber_reservation,
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
            extensions: Arc::clone(&parent.extensions),
            entries: parent.entries,
            encoded_bytes: parent.encoded_bytes,
            trace: parent.trace.clone(),
        };
        let required_slots = attempt
            .required_services()
            .map(|service| base_context.service_slot(service))
            .collect::<Vec<_>>();

        let id = self.next_fiber_id()?;
        let initial = FiberSnapshot {
            id,
            generation: FiberGeneration(0),
            factory: identity.clone(),
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
                identity,
                factory: Some(factory),
                desired: Some(desired),
                attempt: Some(attempt),
                replacement: None,
                fiber_reservation: Some(fiber_reservation),
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
            state.fibers.insert(id, Arc::clone(&fiber));
            state.advance_revision();
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

    fn validate_owner_data(owner: Owner, data: &FiberData) -> Result<()> {
        let valid_state = matches!(
            data.state,
            FiberState::Loading | FiberState::Active | FiberState::Unloading
        );
        if data.generation != owner.generation || !valid_state {
            return Err(MetaError::StaleContext {
                fiber: owner.fiber,
                generation: owner.generation,
            });
        }
        Ok(())
    }

    fn validate_live_owner_data(owner: Owner, data: &FiberData) -> Result<()> {
        if data.generation != owner.generation
            || !matches!(data.state, FiberState::Loading | FiberState::Active)
        {
            return Err(MetaError::StaleContext {
                fiber: owner.fiber,
                generation: owner.generation,
            });
        }
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
