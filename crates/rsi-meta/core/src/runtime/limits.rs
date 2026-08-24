use crate::{MetaError, Result};
use std::ops::Deref;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::time::{Duration, Instant};
use tokio::sync::{Notify, Semaphore};

/// Hard ceiling for any deadline accepted by core.
pub const MAXIMUM_OPERATION_DEADLINE: Duration = Duration::from_hours(24);
/// Hard ceiling that keeps downstream JSON serialization within a bounded stack depth.
pub const MAXIMUM_JSON_DEPTH: usize = 128;

/// Registry and ownership bounds enforced by one Runtime.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TopologyLimits {
    /// Maximum reserved and registered Fibers.
    pub maximum_fibers: usize,
    /// Maximum parent/child depth, counting the first root Fiber as one.
    pub maximum_fiber_depth: usize,
    /// Maximum staged and published service providers.
    pub maximum_services: usize,
    /// Maximum retained requirement and provision declarations.
    pub maximum_service_declarations: usize,
    /// Maximum retained requirement edges.
    pub maximum_dependency_edges: usize,
    /// Maximum requirements declared by one Fiber.
    pub maximum_requirements_per_fiber: usize,
    /// Maximum provisions declared by one Fiber.
    pub maximum_provisions_per_fiber: usize,
    /// Maximum staged and published event listeners.
    pub maximum_event_listeners: usize,
    /// Maximum cleanup effects owned by one Fiber generation.
    pub maximum_effects_per_fiber: usize,
    /// Maximum cleanup effects retained across the Runtime.
    pub maximum_effects: usize,
    /// Maximum isolation and intercept entries retained by one Context.
    pub maximum_context_entries: usize,
}

impl Default for TopologyLimits {
    fn default() -> Self {
        Self {
            maximum_fibers: 4_096,
            maximum_fiber_depth: 256,
            maximum_services: 4_096,
            maximum_service_declarations: 16_384,
            maximum_dependency_edges: 65_536,
            maximum_requirements_per_fiber: 256,
            maximum_provisions_per_fiber: 256,
            maximum_event_listeners: 16_384,
            maximum_effects_per_fiber: 4_096,
            maximum_effects: 65_536,
            maximum_context_entries: 256,
        }
    }
}

/// Encoded and retained payload bounds enforced by one Runtime.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PayloadLimits {
    /// Maximum UTF-8 bytes in one descriptor identifier.
    pub maximum_identifier_bytes: usize,
    /// Maximum compact JSON-encoded bytes in one plugin descriptor.
    pub maximum_descriptor_bytes: usize,
    /// Maximum opaque service-frame bytes or compact JSON-encoded event bytes.
    pub maximum_frame_bytes: usize,
    /// Maximum compact JSON-encoded bytes in input or normalized configuration.
    pub maximum_config_bytes: usize,
    /// Maximum descriptor and normalized-configuration bytes retained by Fibers and proofs.
    pub maximum_retained_plugin_bytes: usize,
    /// Maximum logical isolation and encoded-intercept bytes retained by one Context.
    pub maximum_context_bytes: usize,
    /// Maximum logical service-frame bytes queued across the Runtime.
    pub maximum_buffered_service_bytes: usize,
    /// Maximum nesting depth in one retained JSON value, up to [`MAXIMUM_JSON_DEPTH`].
    pub maximum_json_depth: usize,
    /// Maximum scalar, array, and object nodes in one retained JSON value.
    pub maximum_json_nodes: usize,
    /// Maximum diagnostic entries retained in one report.
    pub maximum_diagnostic_entries: usize,
    /// Maximum UTF-8 bytes retained in one diagnostic report.
    pub maximum_diagnostic_bytes: usize,
}

impl Default for PayloadLimits {
    fn default() -> Self {
        Self {
            maximum_identifier_bytes: 256,
            maximum_descriptor_bytes: 256 * 1024,
            maximum_frame_bytes: 1024 * 1024,
            maximum_config_bytes: 1024 * 1024,
            maximum_retained_plugin_bytes: 64 * 1024 * 1024,
            maximum_context_bytes: 1024 * 1024,
            maximum_buffered_service_bytes: 64 * 1024 * 1024,
            maximum_json_depth: 128,
            maximum_json_nodes: 65_536,
            maximum_diagnostic_entries: 256,
            maximum_diagnostic_bytes: 64 * 1024,
        }
    }
}

/// Concurrent-work and channel bounds enforced by one Runtime.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ExecutionLimits {
    /// Maximum configuration preparations executing concurrently.
    pub maximum_concurrent_preparations: usize,
    /// Maximum distinct Fibers reconciling concurrently.
    pub maximum_concurrent_reconciliations: usize,
    /// Maximum admitted live service calls.
    pub maximum_concurrent_service_calls: usize,
    /// Bounded capacity of each request or ordinary response channel.
    pub channel_capacity: usize,
    /// Maximum event dispatches executing concurrently.
    pub maximum_concurrent_event_dispatches: usize,
    /// Maximum event callbacks executing concurrently.
    pub maximum_concurrent_event_callbacks: usize,
}

impl Default for ExecutionLimits {
    fn default() -> Self {
        Self {
            maximum_concurrent_preparations: 32,
            maximum_concurrent_reconciliations: 32,
            maximum_concurrent_service_calls: 1_024,
            channel_capacity: 32,
            maximum_concurrent_event_dispatches: 64,
            maximum_concurrent_event_callbacks: 64,
        }
    }
}

/// Operation deadlines enforced by one Runtime.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DeadlineLimits {
    /// Absolute wait deadline for one admitted async application or reconfiguration caller.
    pub transition: Duration,
    /// Complete service-stream deadline from admission.
    pub service_call: Duration,
    /// Complete event-dispatch deadline from admission.
    pub event_dispatch: Duration,
    /// Maximum time one shutdown caller waits for the persistent shutdown run.
    pub shutdown_wait: Duration,
}

impl Default for DeadlineLimits {
    fn default() -> Self {
        Self {
            transition: Duration::from_secs(30),
            service_call: Duration::from_mins(1),
            event_dispatch: Duration::from_mins(1),
            shutdown_wait: Duration::from_secs(90),
        }
    }
}

/// Immutable capacity and deadline policy enforced by one Runtime.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct RuntimeLimits {
    /// Registry and ownership bounds.
    pub topology: TopologyLimits,
    /// Encoded and retained payload bounds.
    pub payloads: PayloadLimits,
    /// Concurrent-work and channel bounds.
    pub execution: ExecutionLimits,
    /// Operation deadlines.
    pub deadlines: DeadlineLimits,
}

pub(super) struct ValidatedRuntimeLimits(RuntimeLimits);

impl ValidatedRuntimeLimits {
    pub(super) fn new(limits: RuntimeLimits) -> Result<Self> {
        validate_nonzero(&limits)?;
        validate_relationships(&limits)?;
        validate_tokio_bounds(&limits)?;
        validate_deadlines(&limits)?;
        Ok(Self(limits))
    }

    pub(super) fn configured(&self) -> &RuntimeLimits {
        &self.0
    }
}

impl Deref for ValidatedRuntimeLimits {
    type Target = RuntimeLimits;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

fn validate_nonzero(limits: &RuntimeLimits) -> Result<()> {
    let topology = &limits.topology;
    let payloads = &limits.payloads;
    let execution = &limits.execution;
    let capacities = [
        topology.maximum_fibers,
        topology.maximum_fiber_depth,
        topology.maximum_services,
        topology.maximum_service_declarations,
        topology.maximum_dependency_edges,
        topology.maximum_requirements_per_fiber,
        topology.maximum_provisions_per_fiber,
        topology.maximum_event_listeners,
        topology.maximum_effects_per_fiber,
        topology.maximum_effects,
        topology.maximum_context_entries,
        payloads.maximum_identifier_bytes,
        payloads.maximum_descriptor_bytes,
        payloads.maximum_frame_bytes,
        payloads.maximum_config_bytes,
        payloads.maximum_retained_plugin_bytes,
        payloads.maximum_context_bytes,
        payloads.maximum_buffered_service_bytes,
        payloads.maximum_json_depth,
        payloads.maximum_json_nodes,
        payloads.maximum_diagnostic_entries,
        payloads.maximum_diagnostic_bytes,
        execution.maximum_concurrent_preparations,
        execution.maximum_concurrent_reconciliations,
        execution.maximum_concurrent_service_calls,
        execution.channel_capacity,
        execution.maximum_concurrent_event_dispatches,
        execution.maximum_concurrent_event_callbacks,
    ];
    let deadlines = [
        limits.deadlines.transition,
        limits.deadlines.service_call,
        limits.deadlines.event_dispatch,
        limits.deadlines.shutdown_wait,
    ];
    if capacities.contains(&0) || deadlines.iter().any(Duration::is_zero) {
        return Err(MetaError::InvalidInput(
            "runtime capacity limits and deadlines must be nonzero".to_owned(),
        ));
    }
    Ok(())
}

fn validate_relationships(limits: &RuntimeLimits) -> Result<()> {
    let payloads = &limits.payloads;
    if payloads.maximum_json_depth > MAXIMUM_JSON_DEPTH {
        return Err(MetaError::InvalidInput(format!(
            "JSON depth limit exceeds the implementation maximum of {MAXIMUM_JSON_DEPTH}"
        )));
    }
    let retained_per_plugin = payloads
        .maximum_descriptor_bytes
        .checked_add(payloads.maximum_config_bytes)
        .ok_or_else(|| MetaError::InvalidInput("plugin payload limits overflow".to_owned()))?;
    if retained_per_plugin > payloads.maximum_retained_plugin_bytes
        || payloads.maximum_frame_bytes > payloads.maximum_buffered_service_bytes
    {
        return Err(MetaError::InvalidInput(
            "a maximum plugin or frame payload exceeds its aggregate Runtime budget".to_owned(),
        ));
    }
    Ok(())
}

fn validate_tokio_bounds(limits: &RuntimeLimits) -> Result<()> {
    let payloads = &limits.payloads;
    let execution = &limits.execution;
    let response_capacity = execution.channel_capacity.checked_add(1).ok_or_else(|| {
        MetaError::InvalidInput("service response channel capacity overflow".to_owned())
    })?;
    if response_capacity > Semaphore::MAX_PERMITS
        || execution.maximum_concurrent_preparations > Semaphore::MAX_PERMITS
        || execution.maximum_concurrent_reconciliations > Semaphore::MAX_PERMITS
        || execution.maximum_concurrent_service_calls > Semaphore::MAX_PERMITS
        || execution.maximum_concurrent_event_dispatches > Semaphore::MAX_PERMITS
        || execution.maximum_concurrent_event_callbacks > Semaphore::MAX_PERMITS
        || payloads.maximum_buffered_service_bytes > Semaphore::MAX_PERMITS
        || payloads.maximum_frame_bytes > u32::MAX as usize
    {
        return Err(MetaError::InvalidInput(
            "runtime limit exceeds a Tokio primitive maximum".to_owned(),
        ));
    }
    Ok(())
}

fn validate_deadlines(limits: &RuntimeLimits) -> Result<()> {
    let deadlines = [
        limits.deadlines.transition,
        limits.deadlines.service_call,
        limits.deadlines.event_dispatch,
        limits.deadlines.shutdown_wait,
    ];
    if deadlines.iter().any(|deadline| {
        *deadline > MAXIMUM_OPERATION_DEADLINE || Instant::now().checked_add(*deadline).is_none()
    }) {
        return Err(MetaError::InvalidInput(format!(
            "runtime deadlines must not exceed {} seconds",
            MAXIMUM_OPERATION_DEADLINE.as_secs()
        )));
    }
    Ok(())
}

/// One logical Runtime resource's current and historical usage.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ResourceUsageSnapshot {
    /// Currently reserved units.
    pub current: usize,
    /// Configured maximum units.
    pub limit: usize,
    /// Largest observed current value.
    pub high_watermark: usize,
    /// Failed reservation attempts, saturating at `u64::MAX`.
    pub rejected: u64,
}

/// Point-in-time usage of Runtime-owned logical global resources.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RuntimeResourceSnapshot {
    /// Concurrent descriptor/config preparations.
    pub preparations: ResourceUsageSnapshot,
    /// Prepared and registered Fiber slots.
    pub fibers: ResourceUsageSnapshot,
    /// Compact JSON-encoded descriptor and normalized-configuration bytes.
    pub retained_plugin_bytes: ResourceUsageSnapshot,
    /// Prepared and registered requirement plus provision declarations.
    pub service_declarations: ResourceUsageSnapshot,
    /// Prepared and registered requirement edges.
    pub dependency_edges: ResourceUsageSnapshot,
    /// Staged and published service providers.
    pub services: ResourceUsageSnapshot,
    /// Cleanup effects retained by live generations and cleanup runs.
    pub effects: ResourceUsageSnapshot,
    /// Staged and published event listeners.
    pub listeners: ResourceUsageSnapshot,
    /// Admitted service calls whose caller has not released its terminal lease.
    pub service_calls: ResourceUsageSnapshot,
    /// Service-frame bytes retained in request and response queues.
    pub buffered_service_bytes: ResourceUsageSnapshot,
    /// Fiber reconciliation transitions holding a global execution slot.
    pub reconciliations: ResourceUsageSnapshot,
    /// Runtime-owned reconciliation scheduler workers; the limit is always one.
    pub scheduler_workers: ResourceUsageSnapshot,
    /// Admitted event dispatch operations.
    pub event_dispatches: ResourceUsageSnapshot,
    /// Event callbacks holding a global execution slot.
    pub event_callbacks: ResourceUsageSnapshot,
    /// Runtime-owned cleanup runs that have not completed.
    pub cleanup_runs: ResourceUsageSnapshot,
}

pub(super) struct RuntimeResources {
    pub(super) preparations: Arc<ResourceLedger>,
    pub(super) fibers: Arc<ResourceLedger>,
    pub(super) retained_plugin_bytes: Arc<ResourceLedger>,
    pub(super) service_declarations: Arc<ResourceLedger>,
    pub(super) dependency_edges: Arc<ResourceLedger>,
    pub(super) services: Arc<ResourceLedger>,
    pub(super) effects: Arc<ResourceLedger>,
    pub(super) listeners: Arc<ResourceLedger>,
    pub(super) service_calls: Arc<ResourceLedger>,
    pub(super) buffered_service_bytes: Arc<ResourceLedger>,
    pub(super) reconciliations: Arc<ResourceLedger>,
    pub(super) scheduler_workers: Arc<ResourceLedger>,
    pub(super) event_dispatches: Arc<ResourceLedger>,
    pub(super) event_callbacks: Arc<ResourceLedger>,
    pub(super) cleanup_runs: Arc<ResourceLedger>,
}

impl RuntimeResources {
    pub(super) fn new(limits: &RuntimeLimits) -> Self {
        // Semaphores own fail-fast or asynchronous admission; these ledgers
        // mirror the same validated limits to retain current, peak, rejection,
        // and shutdown-drain evidence after a permit changes hands.
        Self {
            preparations: Arc::new(ResourceLedger::new(
                limits.execution.maximum_concurrent_preparations,
            )),
            fibers: Arc::new(ResourceLedger::new(limits.topology.maximum_fibers)),
            retained_plugin_bytes: Arc::new(ResourceLedger::new(
                limits.payloads.maximum_retained_plugin_bytes,
            )),
            service_declarations: Arc::new(ResourceLedger::new(
                limits.topology.maximum_service_declarations,
            )),
            dependency_edges: Arc::new(ResourceLedger::new(
                limits.topology.maximum_dependency_edges,
            )),
            services: Arc::new(ResourceLedger::new(limits.topology.maximum_services)),
            effects: Arc::new(ResourceLedger::new(limits.topology.maximum_effects)),
            listeners: Arc::new(ResourceLedger::new(limits.topology.maximum_event_listeners)),
            service_calls: Arc::new(ResourceLedger::new(
                limits.execution.maximum_concurrent_service_calls,
            )),
            buffered_service_bytes: Arc::new(ResourceLedger::new(
                limits.payloads.maximum_buffered_service_bytes,
            )),
            reconciliations: Arc::new(ResourceLedger::new(
                limits.execution.maximum_concurrent_reconciliations,
            )),
            scheduler_workers: Arc::new(ResourceLedger::new(1)),
            event_dispatches: Arc::new(ResourceLedger::new(
                limits.execution.maximum_concurrent_event_dispatches,
            )),
            event_callbacks: Arc::new(ResourceLedger::new(
                limits.execution.maximum_concurrent_event_callbacks,
            )),
            cleanup_runs: Arc::new(ResourceLedger::new(limits.topology.maximum_fibers)),
        }
    }

    pub(super) fn snapshot(&self) -> RuntimeResourceSnapshot {
        RuntimeResourceSnapshot {
            preparations: self.preparations.snapshot(),
            fibers: self.fibers.snapshot(),
            retained_plugin_bytes: self.retained_plugin_bytes.snapshot(),
            service_declarations: self.service_declarations.snapshot(),
            dependency_edges: self.dependency_edges.snapshot(),
            services: self.services.snapshot(),
            effects: self.effects.snapshot(),
            listeners: self.listeners.snapshot(),
            service_calls: self.service_calls.snapshot(),
            buffered_service_bytes: self.buffered_service_bytes.snapshot(),
            reconciliations: self.reconciliations.snapshot(),
            scheduler_workers: self.scheduler_workers.snapshot(),
            event_dispatches: self.event_dispatches.snapshot(),
            event_callbacks: self.event_callbacks.snapshot(),
            cleanup_runs: self.cleanup_runs.snapshot(),
        }
    }

    pub(super) async fn wait_zero(&self) {
        tokio::join!(
            self.preparations.wait_zero(),
            self.fibers.wait_zero(),
            self.retained_plugin_bytes.wait_zero(),
            self.service_declarations.wait_zero(),
            self.dependency_edges.wait_zero(),
            self.services.wait_zero(),
            self.effects.wait_zero(),
            self.listeners.wait_zero(),
            self.service_calls.wait_zero(),
            self.buffered_service_bytes.wait_zero(),
            self.reconciliations.wait_zero(),
            self.scheduler_workers.wait_zero(),
            self.event_dispatches.wait_zero(),
            self.event_callbacks.wait_zero(),
            self.cleanup_runs.wait_zero(),
        );
    }
}

#[derive(Debug)]
pub(crate) struct ResourceLedger {
    limit: usize,
    current: AtomicUsize,
    high_watermark: AtomicUsize,
    rejected: AtomicU64,
    zero: Notify,
}

impl ResourceLedger {
    fn new(limit: usize) -> Self {
        Self {
            limit,
            current: AtomicUsize::new(0),
            high_watermark: AtomicUsize::new(0),
            rejected: AtomicU64::new(0),
            zero: Notify::new(),
        }
    }

    pub(crate) fn try_reserve(self: &Arc<Self>, amount: usize) -> Option<ResourceReservation> {
        let mut current = self.current.load(Ordering::Acquire);
        loop {
            let Some(next) = current.checked_add(amount) else {
                self.record_rejection();
                return None;
            };
            if next > self.limit {
                self.record_rejection();
                return None;
            }
            match self.current.compare_exchange_weak(
                current,
                next,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => {
                    self.high_watermark.fetch_max(next, Ordering::AcqRel);
                    return Some(ResourceReservation {
                        ledger: Arc::clone(self),
                        amount,
                    });
                }
                Err(observed) => current = observed,
            }
        }
    }

    pub(crate) fn record_rejection(&self) {
        let _ = self
            .rejected
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |value| {
                Some(value.saturating_add(1))
            });
    }

    fn release(&self, amount: usize) {
        let previous = self.current.fetch_sub(amount, Ordering::AcqRel);
        debug_assert!(previous >= amount, "resource reservation underflow");
        if previous == amount {
            self.zero.notify_waiters();
        }
    }

    pub(crate) async fn wait_zero(&self) {
        loop {
            let notified = self.zero.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            if self.current.load(Ordering::Acquire) == 0 {
                return;
            }
            notified.as_mut().await;
        }
    }

    fn snapshot(&self) -> ResourceUsageSnapshot {
        ResourceUsageSnapshot {
            current: self.current.load(Ordering::Acquire),
            limit: self.limit,
            high_watermark: self.high_watermark.load(Ordering::Acquire),
            rejected: self.rejected.load(Ordering::Acquire),
        }
    }
}

#[derive(Debug)]
pub(crate) struct ResourceReservation {
    ledger: Arc<ResourceLedger>,
    amount: usize,
}

impl ResourceReservation {
    pub(super) fn shrink_to(&mut self, amount: usize) {
        assert!(amount <= self.amount, "a reservation may only shrink");
        let released = self.amount - amount;
        self.amount = amount;
        self.ledger.release(released);
    }

    pub(super) fn split_off(&mut self, retained_prefix: usize) -> Self {
        let amount = self
            .amount
            .checked_sub(retained_prefix)
            .expect("retained prefix belongs to the existing reservation");
        self.amount = retained_prefix;
        Self {
            ledger: Arc::clone(&self.ledger),
            amount,
        }
    }
}

impl Drop for ResourceReservation {
    fn drop(&mut self) {
        self.ledger.release(self.amount);
    }
}
