use crate::invocation::Invocation;
use async_trait::async_trait;
use rsi_api_protocol::{
    ApiDispatch, ApiError, ApiHandler, ApiInvocation, ApiRegistrar, ApiRegistration,
    ApiResponseCapacity, ByteBudget, CallOrigin, DeviceId, OperationClass, OperationId,
    OperationSpec, RegistrationControl, Result,
};
use rsi_meta::Execution;
use std::collections::BTreeMap;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, AtomicUsize, Ordering},
};
use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;

const GLOBAL_CALLS: [usize; 3] = [16, 16, 64];
const DEVICE_CALLS: [usize; 3] = [4, 4, 16];

/// One registry generation with explicit task execution and bounded resource lanes.
#[derive(Clone, Debug)]
pub struct ApiRegistry {
    pub(crate) execution: Execution,
    pub(crate) state: Arc<Shared>,
    input: [ByteBudget; 3],
    output: [ByteBudget; 3],
}

#[derive(Debug, Default)]
pub(crate) struct Shared {
    inner: Mutex<State>,
}

#[derive(Debug, Default)]
struct State {
    closed: bool,
    operations: BTreeMap<OperationId, Arc<Entry>>,
    calls: [usize; 3],
    devices: BTreeMap<DeviceId, [usize; 3]>,
}

#[derive(Debug)]
pub(crate) struct Entry {
    pub spec: OperationSpec,
    pub handler: Arc<dyn ApiHandler>,
    pub retired: AtomicBool,
    pub retiring: CancellationToken,
    active: AtomicUsize,
    drained: Notify,
}

impl ApiRegistry {
    /// Creates independent input/output budgets and the standard non-queuing limits.
    pub fn new(execution: Execution) -> Self {
        Self {
            execution,
            state: Arc::default(),
            input: budgets(),
            output: budgets(),
        }
    }

    /// Fences all registrations and awaits their active calls and streams.
    pub async fn close(&self) {
        let entries = {
            let mut state = self
                .state
                .inner
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            state.closed = true;
            let entries: Vec<_> = state.operations.values().cloned().collect();
            for entry in &entries {
                entry.retired.store(true, Ordering::Release);
            }
            state
                .operations
                .retain(|_, entry| entry.active.load(Ordering::Acquire) != 0);
            entries
        };
        for entry in &entries {
            entry.retiring.cancel();
        }
        futures_util::future::join_all(entries.iter().map(|entry| entry.drain())).await;
    }
}

fn budgets() -> [ByteBudget; 3] {
    [
        ByteBudget::new(2 * 1024 * 1024).expect("constant is below the API ceiling"),
        ByteBudget::default(),
        ByteBudget::default(),
    ]
}

impl ApiRegistrar for ApiRegistry {
    fn register(
        &self,
        spec: OperationSpec,
        handler: Arc<dyn ApiHandler>,
    ) -> Result<ApiRegistration> {
        spec.validate()?;
        let mut state = self
            .state
            .inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if state.closed {
            return Err(ApiError::ShuttingDown);
        }
        if state.operations.contains_key(&spec.id) {
            return Err(ApiError::Invalid(
                "operation is already registered or draining".into(),
            ));
        }
        if state.operations.len() == rsi_api_protocol::MAXIMUM_OPERATIONS {
            return Err(ApiError::Capacity);
        }
        let entry = Arc::new(Entry {
            spec,
            handler,
            retired: AtomicBool::new(false),
            retiring: CancellationToken::new(),
            active: AtomicUsize::new(0),
            drained: Notify::new(),
        });
        state
            .operations
            .insert(entry.spec.id.clone(), entry.clone());
        Ok(ApiRegistration::new(Arc::new(Registration {
            shared: self.state.clone(),
            entry,
        })))
    }
}

impl ApiDispatch for ApiRegistry {
    fn admit(&self, operation: &OperationId, origin: CallOrigin) -> Result<Box<dyn ApiInvocation>> {
        if let CallOrigin::Device(device) = &origin
            && device.revoked.is_cancelled()
        {
            return Err(ApiError::Unauthorized);
        }
        let (entry, lane) = {
            let mut state = self
                .state
                .inner
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if state.closed {
                return Err(ApiError::ShuttingDown);
            }
            let entry = state
                .operations
                .get(operation)
                .ok_or(ApiError::Unavailable)?
                .clone();
            if entry.retired.load(Ordering::Acquire) {
                return Err(ApiError::ShuttingDown);
            }
            if !entry.spec.access.permits(&origin) {
                return Err(ApiError::Unauthorized);
            }
            let lane = lane(entry.spec.class);
            if state.calls[lane] >= GLOBAL_CALLS[lane] {
                return Err(ApiError::Capacity);
            }
            if let CallOrigin::Device(device) = &origin {
                if state
                    .devices
                    .get(&device.id)
                    .is_some_and(|calls| calls[lane] >= DEVICE_CALLS[lane])
                {
                    return Err(ApiError::Capacity);
                }
                state.devices.entry(device.id.clone()).or_default()[lane] += 1;
            }
            state.calls[lane] += 1;
            entry.active.fetch_add(1, Ordering::AcqRel);
            (entry, lane)
        };
        // Failure below drops the call owner, including its per-device admission.
        let owner = CallOwner {
            shared: self.state.clone(),
            entry: entry.clone(),
            origin: origin.clone(),
            quota: Arc::new(QuotaOwner {
                shared: self.state.clone(),
                origin,
                class: entry.spec.class,
            }),
        };
        let output = if entry.spec.class == OperationClass::Subscription {
            ApiResponseCapacity::Subscription {
                budget: self.output[lane].clone(),
                maximum: entry.spec.maximum_response_bytes,
            }
        } else if entry.spec.effect == rsi_api_protocol::OperationEffect::Read {
            ApiResponseCapacity::Finite(rsi_api_protocol::FiniteResponseCapacity::Measured {
                budget: self.output[lane].clone(),
                maximum: entry.spec.maximum_response_bytes,
            })
        } else {
            ApiResponseCapacity::Finite(
                self.output[lane]
                    .reserve(entry.spec.maximum_response_bytes)?
                    .into(),
            )
        };
        Ok(Box::new(Invocation {
            execution: self.execution.clone(),
            owner,
            input: self.input[lane].clone(),
            output,
        }))
    }

    fn operations(&self) -> Vec<OperationSpec> {
        let state = self
            .state
            .inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state
            .operations
            .values()
            .filter(|entry| !entry.retired.load(Ordering::Acquire))
            .map(|entry| entry.spec.clone())
            .collect()
    }
}

fn lane(class: OperationClass) -> usize {
    match class {
        OperationClass::Control => 0,
        OperationClass::Data => 1,
        OperationClass::Subscription => 2,
    }
}

#[derive(Debug)]
struct Registration {
    shared: Arc<Shared>,
    entry: Arc<Entry>,
}
#[async_trait]
impl RegistrationControl for Registration {
    fn retire(&self) {
        {
            let mut state = self
                .shared
                .inner
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            self.entry.retired.store(true, Ordering::Release);
            if self.entry.active.load(Ordering::Acquire) == 0 {
                remove_entry(&mut state, &self.entry);
            }
        }
        self.entry.retiring.cancel();
    }
    async fn drain(&self) {
        self.entry.drain().await;
    }
}

impl Entry {
    async fn drain(&self) {
        loop {
            let changed = self.drained.notified();
            tokio::pin!(changed);
            changed.as_mut().enable();
            if self.active.load(Ordering::Acquire) == 0 {
                return;
            }
            changed.await;
        }
    }
}

/// Owns domain work and one share of its device/global admission.
#[derive(Debug)]
pub(crate) struct CallOwner {
    shared: Arc<Shared>,
    pub entry: Arc<Entry>,
    pub origin: CallOrigin,
    pub quota: Arc<QuotaOwner>,
}
impl Drop for CallOwner {
    fn drop(&mut self) {
        let last = {
            let mut state = self
                .shared
                .inner
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let last = self.entry.active.fetch_sub(1, Ordering::AcqRel) == 1;
            if last && self.entry.retired.load(Ordering::Acquire) {
                remove_entry(&mut state, &self.entry);
            }
            last
        };
        if last {
            self.entry.drained.notify_waiters();
        }
    }
}

/// May outlive domain work while an adapter finishes queued writes.
#[derive(Debug)]
pub(crate) struct QuotaOwner {
    shared: Arc<Shared>,
    origin: CallOrigin,
    class: OperationClass,
}
impl Drop for QuotaOwner {
    fn drop(&mut self) {
        let mut state = self
            .shared
            .inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let lane = lane(self.class);
        state.calls[lane] -= 1;
        if let CallOrigin::Device(device) = &self.origin {
            let calls = state
                .devices
                .get_mut(&device.id)
                .expect("active device owns its counter");
            calls[lane] -= 1;
            if *calls == [0; 3] {
                state.devices.remove(&device.id);
            }
        }
    }
}

fn remove_entry(state: &mut State, entry: &Arc<Entry>) {
    // An old lease must never remove a replacement registered under the same name.
    if state
        .operations
        .get(&entry.spec.id)
        .is_some_and(|current| Arc::ptr_eq(current, entry))
    {
        state.operations.remove(&entry.spec.id);
    }
}
