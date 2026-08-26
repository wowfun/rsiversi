#![allow(clippy::wildcard_imports)] // This is one implementation partition of runtime.

use super::*;

#[derive(Debug)]
pub(super) struct GenerationBudget {
    limit: usize,
    current: AtomicUsize,
}

impl GenerationBudget {
    pub(super) fn new(limit: usize) -> Self {
        Self {
            limit,
            current: AtomicUsize::new(0),
        }
    }

    fn try_reserve(self: &Arc<Self>) -> Option<GenerationReservation> {
        let mut current = self.current.load(Ordering::Acquire);
        loop {
            let next = current.checked_add(1)?;
            if next > self.limit {
                return None;
            }
            match self.current.compare_exchange_weak(
                current,
                next,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => {
                    return Some(GenerationReservation {
                        budget: Arc::clone(self),
                    });
                }
                Err(observed) => current = observed,
            }
        }
    }
}

struct GenerationReservation {
    budget: Arc<GenerationBudget>,
}

impl Drop for GenerationReservation {
    fn drop(&mut self) {
        let previous = self.budget.current.fetch_sub(1, Ordering::AcqRel);
        debug_assert!(previous > 0, "generation effect budget underflow");
    }
}

struct EffectReservation {
    _generation: GenerationReservation,
    _runtime: ResourceReservation,
}

struct EffectEntry {
    label: String,
    cleanup: Cleanup,
    _reservation: EffectReservation,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum EffectClaim {
    Explicit,
    AutoAbort,
    Retirement,
}

struct EffectRecordState {
    open: bool,
    claim: Option<EffectClaim>,
    transaction_reservation: Option<EffectReservation>,
    next_entry: u64,
    effects: BTreeMap<u64, EffectEntry>,
    result: Option<CleanupReport>,
}

pub(super) struct EffectRecord {
    id: u64,
    owner: Owner,
    label: String,
    started: AtomicBool,
    state: Mutex<EffectRecordState>,
    closed: Notify,
    complete: Notify,
}

impl EffectRecord {
    fn new(
        id: u64,
        owner: Owner,
        label: String,
        transaction_reservation: EffectReservation,
    ) -> Self {
        Self {
            id,
            owner,
            label,
            started: AtomicBool::new(false),
            state: Mutex::new(EffectRecordState {
                open: true,
                claim: None,
                transaction_reservation: Some(transaction_reservation),
                next_entry: 0,
                effects: BTreeMap::new(),
                result: None,
            }),
            closed: Notify::new(),
            complete: Notify::new(),
        }
    }

    fn defer(&self, effect: EffectEntry) -> Result<u64> {
        let mut state = self.state.lock().expect("effect record poisoned");
        if !state.open {
            return Err(MetaError::InvalidInput(
                "effect transaction is already closed".to_owned(),
            ));
        }
        let id = state
            .next_entry
            .checked_add(1)
            .ok_or(MetaError::CapacityExhausted {
                resource: "effect identities",
            })?;
        state.next_entry = id;
        state.effects.insert(id, effect);
        Ok(id)
    }

    fn is_open(&self) -> bool {
        self.state.lock().expect("effect record poisoned").open
    }

    fn is_empty(&self) -> bool {
        self.state
            .lock()
            .expect("effect record poisoned")
            .effects
            .is_empty()
    }

    fn commit(&self) -> Result<()> {
        let claimed = {
            let mut state = self.state.lock().expect("effect record poisoned");
            if !state.open {
                return Err(MetaError::InvalidInput(
                    "effect transaction is already closed".to_owned(),
                ));
            }
            state.open = false;
            state.claim.is_some()
        };
        self.closed.notify_waiters();
        if claimed {
            Err(MetaError::StaleContext {
                fiber: self.owner.fiber,
                generation: self.owner.generation,
            })
        } else {
            Ok(())
        }
    }

    fn close_for_abort(&self) {
        let closed = {
            let mut state = self.state.lock().expect("effect record poisoned");
            let was_open = state.open;
            state.open = false;
            was_open
        };
        if closed {
            self.closed.notify_waiters();
        }
    }

    pub(super) fn claim_retirement(&self) {
        let mut state = self.state.lock().expect("effect record poisoned");
        state.claim.get_or_insert(EffectClaim::Retirement);
    }

    fn claim(&self, claim: EffectClaim) -> EffectClaim {
        let mut state = self.state.lock().expect("effect record poisoned");
        *state.claim.get_or_insert(claim)
    }

    fn try_claim(&self, claim: EffectClaim) -> bool {
        let mut state = self.state.lock().expect("effect record poisoned");
        if state.claim.is_some() {
            return false;
        }
        state.claim = Some(claim);
        true
    }

    fn claim_kind(&self) -> Option<EffectClaim> {
        self.state.lock().expect("effect record poisoned").claim
    }

    fn start(self: &Arc<Self>, runtime: &Runtime, executor: &tokio::runtime::Handle) {
        if self
            .started
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return;
        }
        debug_assert!(
            self.claim_kind().is_some(),
            "only a claimed effect may start cleanup"
        );
        let record = Arc::clone(self);
        let runtime = runtime.clone();
        executor.spawn(async move {
            let outcome = std::panic::AssertUnwindSafe(record.run_driver(&runtime))
                .catch_unwind()
                .await;
            let Err(payload) = outcome else {
                return;
            };
            let payload_drop_panicked = drop_catching_unwind(payload);
            runtime.mark_terminal_owned("runtime effect cleanup driver panicked");
            record.finish_after_driver_panic(&runtime, payload_drop_panicked);
        });
    }

    async fn wait_closed(&self) {
        loop {
            let notified = self.closed.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            if !self.state.lock().expect("effect record poisoned").open {
                return;
            }
            notified.as_mut().await;
        }
    }

    async fn run_driver(self: &Arc<Self>, runtime: &Runtime) {
        self.wait_closed().await;
        let (mut effects, transaction_reservation) = {
            let mut state = self.state.lock().expect("effect record poisoned");
            (
                std::mem::take(&mut state.effects),
                state.transaction_reservation.take(),
            )
        };
        let maximum_entries = runtime.inner.limits.payloads.maximum_diagnostic_entries;
        let maximum_bytes = runtime.inner.limits.payloads.maximum_diagnostic_bytes;
        let mut report = CleanupReport::default();
        while let Some((_id, effect)) = effects.pop_last() {
            let EffectEntry {
                label,
                cleanup,
                _reservation,
            } = effect;
            match std::panic::AssertUnwindSafe(async move { cleanup().await })
                .catch_unwind()
                .await
            {
                Ok(Ok(())) => {}
                Ok(Err(error)) => {
                    report.push_bounded(label, error, maximum_entries, maximum_bytes);
                }
                Err(payload) => {
                    let error = if drop_catching_unwind(payload) {
                        "cleanup and panic payload destruction panicked"
                    } else {
                        "cleanup panicked"
                    };
                    report.push_bounded(label, error, maximum_entries, maximum_bytes);
                }
            }
        }
        runtime.finish_effect_disposal(self.owner, self.id, self, &report);
        drop(transaction_reservation);
        self.finish(report);
    }

    fn finish_after_driver_panic(self: &Arc<Self>, runtime: &Runtime, payload_drop_panicked: bool) {
        let (effects, transaction_reservation) = {
            let mut state = self.state.lock().expect("effect record poisoned");
            (
                std::mem::take(&mut state.effects),
                state.transaction_reservation.take(),
            )
        };
        let mut report = CleanupReport::default();
        report.push_bounded(
            self.label.clone(),
            if payload_drop_panicked {
                "effect cleanup driver and panic payload destruction panicked"
            } else {
                "effect cleanup driver panicked"
            },
            runtime.inner.limits.payloads.maximum_diagnostic_entries,
            runtime.inner.limits.payloads.maximum_diagnostic_bytes,
        );
        runtime.finish_effect_disposal(self.owner, self.id, self, &report);
        drop(effects);
        drop(transaction_reservation);
        self.finish(report);
    }

    fn finish(&self, report: CleanupReport) {
        let mut state = self.state.lock().expect("effect record poisoned");
        if state.result.is_none() {
            state.result = Some(report);
            self.complete.notify_waiters();
        }
    }

    async fn join(&self) -> CleanupReport {
        loop {
            let notified = self.complete.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            if let Some(report) = self
                .state
                .lock()
                .expect("effect record poisoned")
                .result
                .clone()
            {
                return report;
            }
            notified.as_mut().await;
        }
    }
}

/// Wrapper-first transaction for one set of reverse-ordered cleanup effects.
///
/// The owning generation observes the wrapper before this value is returned.
/// Dropping an open transaction requests Runtime-owned abort; [`Self::commit`]
/// returns an independently disposable [`EffectHandle`].
pub struct EffectTxn {
    runtime: Runtime,
    owner: Owner,
    id: u64,
    record: Arc<EffectRecord>,
    effect_budget: Arc<GenerationBudget>,
    executor: tokio::runtime::Handle,
    armed: bool,
    autoabort_on_drop: bool,
}

#[derive(Clone)]
pub(super) struct EffectScope {
    runtime: Runtime,
    record: Arc<EffectRecord>,
    effect_budget: Arc<GenerationBudget>,
}

#[derive(Debug)]
pub(crate) struct CallbackLease {
    // Unlike a registry mutex, this lock protects only a callback-local gate.
    // Recovering poison lets unwind containment close the gate after user code
    // panics; no shared Runtime invariant is reconstructed from this state.
    open: Mutex<bool>,
}

impl CallbackLease {
    pub(crate) fn new() -> Self {
        Self {
            open: Mutex::new(true),
        }
    }

    pub(crate) fn close(&self) {
        *self
            .open
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = false;
    }

    pub(crate) fn with_open<T>(
        &self,
        closed: MetaError,
        operation: impl FnOnce() -> Result<T>,
    ) -> Result<T> {
        let open = self
            .open
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if !*open {
            return Err(closed);
        }
        operation()
    }
}

/// Callback-lifetime authority to register cleanup on one exact caller generation.
///
/// This value cannot be retargeted and exposes no caller [`Context`] mutation
/// surface. It becomes stale when the callback closes or the caller generation
/// starts retirement.
#[derive(Clone)]
pub struct CallerEffect {
    context: Context,
    cancellation: CancellationToken,
    callback: Arc<CallbackLease>,
}

impl CallerEffect {
    /// Registers one cleanup in the exact caller generation's root ownership.
    ///
    /// Loading callers append to their existing activation transaction. Active
    /// callers retain one immediately committed generation-owned effect.
    pub fn defer(&self, label: impl Into<String>, cleanup: Cleanup) -> Result<()> {
        self.context.defer_callback_effect(
            &self.cancellation,
            &self.callback,
            label.into(),
            cleanup,
        )
    }
}

impl fmt::Debug for CallerEffect {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CallerEffect")
            .field("owner", &self.context.owner)
            .finish_non_exhaustive()
    }
}

impl EffectScope {
    pub(super) fn is_open(&self) -> bool {
        self.record.is_open()
    }

    pub(super) fn defer(&self, label: String, cleanup: Cleanup) -> Result<()> {
        defer_effect(
            &self.runtime,
            &self.record,
            &self.effect_budget,
            label,
            cleanup,
        )
        .map(|_id| ())
    }

    pub(super) fn defer_owned(&self, label: String, cleanup: Cleanup) -> Result<OwnedEffect> {
        let id = defer_effect(
            &self.runtime,
            &self.record,
            &self.effect_budget,
            label,
            cleanup,
        )?;
        Ok(OwnedEffect {
            record: Arc::downgrade(&self.record),
            id,
        })
    }
}

#[derive(Clone)]
pub(super) struct OwnedEffect {
    record: Weak<EffectRecord>,
    id: u64,
}

pub(super) struct EffectRetention {
    _entry: EffectEntry,
}

impl OwnedEffect {
    pub(super) fn detach(&self) -> Option<EffectRetention> {
        let record = self.record.upgrade()?;
        let entry = record
            .state
            .lock()
            .expect("effect record poisoned")
            .effects
            .remove(&self.id)?;
        Some(EffectRetention { _entry: entry })
    }
}

fn defer_effect(
    runtime: &Runtime,
    record: &EffectRecord,
    effect_budget: &Arc<GenerationBudget>,
    label: String,
    cleanup: Cleanup,
) -> Result<u64> {
    runtime.validate_effect_label(&label)?;
    let generation = effect_budget.try_reserve().ok_or_else(|| {
        runtime.inner.resources.effects.record_rejection();
        MetaError::CapacityExhausted {
            resource: "effects",
        }
    })?;
    let runtime_reservation =
        runtime
            .inner
            .resources
            .effects
            .try_reserve(1)
            .ok_or(MetaError::CapacityExhausted {
                resource: "effects",
            })?;
    record.defer(EffectEntry {
        label,
        cleanup,
        _reservation: EffectReservation {
            _generation: generation,
            _runtime: runtime_reservation,
        },
    })
}

impl EffectTxn {
    /// Adds one cleanup to this open transaction.
    ///
    /// Cleanups execute in reverse registration order. A transaction already
    /// claimed by Fiber retirement remains open to its original setup owner so
    /// that an in-flight setup can still install the exact undo before closing.
    pub fn defer(&mut self, label: impl Into<String>, cleanup: Cleanup) -> Result<()> {
        defer_effect(
            &self.runtime,
            &self.record,
            &self.effect_budget,
            label.into(),
            cleanup,
        )
        .map(|_id| ())
    }

    pub(super) fn scope(&self) -> EffectScope {
        EffectScope {
            runtime: self.runtime.clone(),
            record: Arc::clone(&self.record),
            effect_budget: Arc::clone(&self.effect_budget),
        }
    }

    /// Transfers an unexpected-drop cleanup claim to generation rollback.
    ///
    /// Activation uses this before any plugin code can run. Stack unwinding
    /// then closes the setup window but cannot start this oldest record ahead
    /// of later generation-owned children or effects.
    pub(super) fn defer_drop_to_generation_rollback(&mut self) {
        self.autoabort_on_drop = false;
    }

    pub(super) fn close_for_runtime_rollback(mut self) {
        self.record.close_for_abort();
        self.armed = false;
    }

    fn discard_empty_locked(mut self, data: &mut FiberData) {
        debug_assert!(self.record.is_empty());
        self.record.close_for_abort();
        if data.generation == self.owner.generation
            && let Some(active) = data.active.as_mut()
            && active
                .effects
                .get(&self.id)
                .is_some_and(|current| Arc::ptr_eq(current, &self.record))
        {
            active.effects.remove(&self.id);
        }
        self.armed = false;
    }

    /// Commits setup ownership without publishing any product contribution.
    ///
    /// If retirement claimed the wrapper first, commit closes the setup window
    /// and returns [`MetaError::StaleContext`]; the Runtime still executes every
    /// installed cleanup.
    pub fn commit(mut self) -> Result<EffectHandle> {
        let result = self.record.commit();
        self.armed = false;
        result.map(|()| EffectHandle {
            runtime: self.runtime.clone(),
            owner: self.owner,
            id: self.id,
            record: Arc::clone(&self.record),
            executor: self.executor.clone(),
        })
    }

    /// Aborts setup and joins the exact Runtime-owned cleanup run.
    pub async fn abort(mut self) -> CleanupReport {
        self.record.close_for_abort();
        self.armed = false;
        let _claim = self.runtime.start_effect_disposal(
            self.owner,
            self.id,
            &self.record,
            EffectClaim::Explicit,
            &self.executor,
        );
        self.record.join().await
    }
}

impl fmt::Debug for EffectTxn {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("EffectTxn")
            .field("owner", &self.owner)
            .field("id", &self.id)
            .finish_non_exhaustive()
    }
}

impl Drop for EffectTxn {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        self.armed = false;
        self.record.close_for_abort();
        if self.autoabort_on_drop {
            self.runtime.schedule_effect_autoabort(
                self.owner,
                self.id,
                Arc::clone(&self.record),
                &self.executor,
            );
        }
    }
}

/// Cloneable handle that joins one exact, idempotent effect cleanup run.
#[derive(Clone)]
pub struct EffectHandle {
    pub(super) runtime: Runtime,
    pub(super) owner: Owner,
    pub(super) id: u64,
    pub(super) record: Arc<EffectRecord>,
    pub(super) executor: tokio::runtime::Handle,
}

impl EffectHandle {
    /// Requests disposal once and joins its bounded cleanup report.
    ///
    /// Once this future is first polled, the cleanup driver remains
    /// Runtime-owned if the caller drops the future.
    pub async fn dispose(&self) -> CleanupReport {
        let _claim = self.runtime.start_effect_disposal(
            self.owner,
            self.id,
            &self.record,
            EffectClaim::Explicit,
            &self.executor,
        );
        self.record.join().await
    }

    pub(crate) async fn try_dispose_explicit(&self) -> Option<CleanupReport> {
        if !self.runtime.try_start_effect_disposal(
            self.owner,
            self.id,
            &self.record,
            EffectClaim::Explicit,
            &self.executor,
        ) {
            return None;
        }
        Some(self.record.join().await)
    }

    pub(super) fn is_empty(&self) -> bool {
        self.record.is_empty()
    }
}

impl fmt::Debug for EffectHandle {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("EffectHandle")
            .field("owner", &self.owner)
            .field("id", &self.id)
            .finish_non_exhaustive()
    }
}

impl Runtime {
    pub(super) fn ensure_dynamic_effect_owner(&self, owner: Owner) -> Result<()> {
        let fiber = self.owner_fiber(owner)?;
        let data = fiber.data.lock().expect("fiber state poisoned");
        if data.generation != owner.generation || !matches!(data.state, FiberState::Active) {
            return Err(MetaError::StaleContext {
                fiber: owner.fiber,
                generation: owner.generation,
            });
        }
        Ok(())
    }

    pub(super) fn begin_effect(&self, owner: Owner, label: String) -> Result<EffectTxn> {
        let _runtime_admission = self.begin_admission(false)?;
        let fiber = self.owner_fiber(owner)?;
        let mut data = fiber.data.lock().expect("fiber state poisoned");
        self.begin_effect_locked(owner, label, &fiber, &mut data)
    }

    fn begin_effect_locked(
        &self,
        owner: Owner,
        label: String,
        fiber: &Arc<Fiber>,
        data: &mut FiberData,
    ) -> Result<EffectTxn> {
        Self::validate_owner_data(owner, data)?;
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
        let generation = active
            .effect_transaction_budget
            .try_reserve()
            .ok_or_else(|| {
                self.inner.resources.effect_transactions.record_rejection();
                MetaError::CapacityExhausted {
                    resource: "effect transactions",
                }
            })?;
        let runtime = self
            .inner
            .resources
            .effect_transactions
            .try_reserve(1)
            .ok_or(MetaError::CapacityExhausted {
                resource: "effect transactions",
            })?;
        let id = self
            .inner
            .next_effect
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |value| {
                value.checked_add(1)
            })
            .map_err(|_| MetaError::CapacityExhausted {
                resource: "effect identities",
            })?
            + 1;
        let record = Arc::new(EffectRecord::new(
            id,
            owner,
            label,
            EffectReservation {
                _generation: generation,
                _runtime: runtime,
            },
        ));
        active.effects.insert(id, Arc::clone(&record));
        Ok(EffectTxn {
            runtime: self.clone(),
            owner,
            id,
            record,
            effect_budget: Arc::clone(&active.effect_budget),
            executor: fiber.executor.clone(),
            armed: true,
            autoabort_on_drop: true,
        })
    }

    pub(super) fn validate_effect_label(&self, label: &str) -> Result<()> {
        if label.len() > self.inner.limits.payloads.maximum_diagnostic_bytes {
            return Err(MetaError::InvalidInput(
                "effect label exceeds the configured diagnostic byte limit".to_owned(),
            ));
        }
        Ok(())
    }

    fn start_effect_disposal(
        &self,
        owner: Owner,
        id: u64,
        record: &Arc<EffectRecord>,
        claim: EffectClaim,
        executor: &tokio::runtime::Handle,
    ) -> Option<EffectClaim> {
        let mut effective_claim = record.claim_kind();
        if let Ok(fiber) = self.owner_fiber(owner) {
            let mut data = fiber.data.lock().expect("fiber state poisoned");
            if data.generation == owner.generation
                && let Some(active) = data.active.as_mut()
                && active
                    .effects
                    .get(&id)
                    .is_some_and(|current| Arc::ptr_eq(current, record))
            {
                effective_claim = Some(record.claim(claim));
            }
        }
        if effective_claim == Some(claim) {
            record.start(self, executor);
        }
        effective_claim
    }

    fn try_start_effect_disposal(
        &self,
        owner: Owner,
        id: u64,
        record: &Arc<EffectRecord>,
        claim: EffectClaim,
        executor: &tokio::runtime::Handle,
    ) -> bool {
        let Ok(fiber) = self.owner_fiber(owner) else {
            return false;
        };
        let data = fiber.data.lock().expect("fiber state poisoned");
        let present = data.generation == owner.generation
            && data.active.as_ref().is_some_and(|active| {
                active
                    .effects
                    .get(&id)
                    .is_some_and(|current| Arc::ptr_eq(current, record))
            });
        if !present || !record.try_claim(claim) {
            return false;
        }
        drop(data);
        record.start(self, executor);
        true
    }

    fn schedule_effect_autoabort(
        &self,
        owner: Owner,
        id: u64,
        record: Arc<EffectRecord>,
        executor: &tokio::runtime::Handle,
    ) {
        let _claim =
            self.start_effect_disposal(owner, id, &record, EffectClaim::AutoAbort, executor);
        drop(record);
    }

    fn finish_effect_disposal(
        &self,
        owner: Owner,
        id: u64,
        record: &Arc<EffectRecord>,
        report: &CleanupReport,
    ) {
        let Ok(fiber) = self.owner_fiber(owner) else {
            return;
        };
        let mut data = fiber.data.lock().expect("fiber state poisoned");
        if data.generation != owner.generation {
            return;
        }
        let Some(active) = data.active.as_mut() else {
            return;
        };
        if active
            .effects
            .get(&id)
            .is_some_and(|current| Arc::ptr_eq(current, record))
        {
            active.effects.remove(&id);
            active.retired_owned_report.extend_bounded(
                report.clone(),
                self.inner.limits.payloads.maximum_diagnostic_entries,
                self.inner.limits.payloads.maximum_diagnostic_bytes,
            );
        }
    }

    pub(super) async fn run_effect(
        &self,
        fiber: &Fiber,
        effect: Arc<EffectRecord>,
    ) -> CleanupReport {
        effect.claim_retirement();
        effect.start(self, &fiber.executor);
        effect.join().await
    }
}

impl Context {
    pub(crate) fn callback_caller_effect(
        &self,
        cancellation: CancellationToken,
        callback: Arc<CallbackLease>,
    ) -> Option<CallerEffect> {
        self.owner.map(|_| CallerEffect {
            context: self.clone(),
            cancellation,
            callback,
        })
    }

    pub(crate) fn validate_callback_view(&self, cancellation: &CancellationToken) -> Result<()> {
        let Some(owner) = self.owner else {
            return if cancellation.is_cancelled() {
                Err(MetaError::Cancelled)
            } else {
                Ok(())
            };
        };
        let stale = || MetaError::StaleContext {
            fiber: owner.fiber,
            generation: owner.generation,
        };
        if cancellation.is_cancelled() {
            return Err(stale());
        }
        let fiber = self.runtime.owner_fiber(owner).map_err(|_| stale())?;
        let data = fiber.data.lock().expect("fiber state poisoned");
        if cancellation.is_cancelled()
            || data.generation != owner.generation
            || !matches!(data.state, FiberState::Loading | FiberState::Active)
        {
            return Err(stale());
        }
        Ok(())
    }

    fn defer_callback_effect(
        &self,
        cancellation: &CancellationToken,
        callback: &CallbackLease,
        label: String,
        cleanup: Cleanup,
    ) -> Result<()> {
        let owner = self
            .owner
            .expect("CallerEffect always has an owned Context");
        let stale = || MetaError::StaleContext {
            fiber: owner.fiber,
            generation: owner.generation,
        };
        callback.with_open(stale(), || {
            if cancellation.is_cancelled() {
                return Err(stale());
            }
            let _runtime_admission = self.runtime.begin_admission(false)?;
            self.runtime.validate_effect_label(&label)?;
            let fiber = self.runtime.owner_fiber(owner).map_err(|_| stale())?;
            let mut data = fiber.data.lock().expect("fiber state poisoned");
            if cancellation.is_cancelled()
                || data.generation != owner.generation
                || !matches!(data.state, FiberState::Loading | FiberState::Active)
            {
                return Err(stale());
            }

            if matches!(data.state, FiberState::Loading) {
                let setup = self
                    .setup_effect
                    .as_ref()
                    .filter(|setup| setup.is_open())
                    .ok_or_else(stale)?;
                if cancellation.is_cancelled() {
                    return Err(stale());
                }
                return setup.defer(label, cleanup);
            }

            let mut transaction =
                self.runtime
                    .begin_effect_locked(owner, label.clone(), &fiber, &mut data)?;
            if cancellation.is_cancelled() {
                transaction.discard_empty_locked(&mut data);
                return Err(stale());
            }
            if let Err(error) = transaction.defer(label, cleanup) {
                transaction.discard_empty_locked(&mut data);
                return Err(error);
            }
            let _effect = transaction.commit()?;
            Ok(())
        })
    }
}
