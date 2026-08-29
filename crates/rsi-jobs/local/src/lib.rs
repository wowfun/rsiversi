//! Tokio-backed process-local Jobs ordinary plugin.

#![deny(unsafe_code)]
#![warn(missing_docs)]
#![allow(clippy::missing_errors_doc)]

use async_trait::async_trait;
use futures_util::FutureExt;
use rsi_jobs::{
    JobControl, JobHandle, JobOutcome, JobScope, JobSpec, JobStatus, Jobs, JobsContract, JobsError,
    MAXIMUM_JOB_RESULT_BYTES, Result, validate_job_name,
};
use rsi_meta::{ActivationPlan, ConfigValue, MetaError, PluginFactory, PreparedActivation};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::panic::AssertUnwindSafe;
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, Weak};
use std::time::Duration;
use tokio::sync::{Mutex as AsyncMutex, Notify};
use tokio_util::sync::CancellationToken;

/// Configuration accepted by [`JobsLocalFactory`].
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct JobsLocalConfig {
    /// Maximum simultaneously unsettled jobs.
    #[serde(default = "default_maximum_active_jobs")]
    pub maximum_active_jobs: usize,
    /// Retirement join bound in milliseconds.
    #[serde(default = "default_shutdown_timeout_ms")]
    pub shutdown_timeout_ms: u64,
}

const fn default_maximum_active_jobs() -> usize {
    256
}

const fn default_shutdown_timeout_ms() -> u64 {
    10_000
}

impl Default for JobsLocalConfig {
    fn default() -> Self {
        Self {
            maximum_active_jobs: default_maximum_active_jobs(),
            shutdown_timeout_ms: default_shutdown_timeout_ms(),
        }
    }
}

impl JobsLocalConfig {
    fn validate(&self) -> Result<()> {
        if self.maximum_active_jobs == 0 || self.maximum_active_jobs > 65_536 {
            return Err(JobsError::InvalidInput(
                "maximum_active_jobs must be within 1..=65536".into(),
            ));
        }
        if self.shutdown_timeout_ms == 0 || self.shutdown_timeout_ms > 300_000 {
            return Err(JobsError::InvalidInput(
                "shutdown_timeout_ms must be within 1..=300000".into(),
            ));
        }
        Ok(())
    }
}

#[derive(Debug)]
struct Service {
    config: JobsLocalConfig,
    accepting: AtomicBool,
    draining: AtomicBool,
    global_drain_timed_out: AtomicBool,
    drain_lock: AsyncMutex<()>,
    next_id: AtomicU64,
    registry: Mutex<Registry>,
    self_weak: Weak<Service>,
}

#[derive(Debug, Default)]
struct Registry {
    jobs: HashMap<String, ActiveJob>,
    unsettled_by_scope: HashMap<JobScope, usize>,
    blocked_scopes: HashSet<JobScope>,
    timed_out_scopes: HashSet<JobScope>,
}

#[derive(Debug)]
struct ActiveJob {
    state: Arc<State>,
    scope: Option<JobScope>,
}

#[derive(Debug)]
struct State {
    status: AtomicU8,
    cancellation: CancellationToken,
    outcome: Mutex<Option<JobOutcome>>,
    settled: Notify,
}

impl State {
    fn new() -> Self {
        Self {
            status: AtomicU8::new(status_byte(JobStatus::Queued)),
            cancellation: CancellationToken::new(),
            outcome: Mutex::new(None),
            settled: Notify::new(),
        }
    }

    fn finish(&self, outcome: JobOutcome) {
        let status = match &outcome {
            JobOutcome::Completed(_) => JobStatus::Completed,
            JobOutcome::Failed(_) => JobStatus::Failed,
            JobOutcome::Cancelled => JobStatus::Cancelled,
        };
        *self
            .outcome
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(outcome);
        self.status.store(status_byte(status), Ordering::Release);
        self.settled.notify_waiters();
    }
}

#[async_trait]
impl JobControl for State {
    fn status(&self) -> JobStatus {
        byte_status(self.status.load(Ordering::Acquire))
    }

    fn cancel(&self) {
        self.cancellation.cancel();
    }

    async fn join(&self) -> JobOutcome {
        loop {
            let notified = self.settled.notified();
            if let Some(outcome) = self
                .outcome
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .clone()
            {
                return outcome;
            }
            notified.await;
        }
    }
}

#[async_trait]
impl Jobs for Service {
    fn submit(&self, spec: JobSpec) -> Result<JobHandle> {
        self.submit_inner(None, spec)
    }

    fn submit_scoped(&self, scope: JobScope, spec: JobSpec) -> Result<JobHandle> {
        self.submit_inner(Some(&scope), spec)
    }

    async fn cancel_scope(&self, scope: &JobScope) -> Result<()> {
        self.drain_scope(scope)
            .await
            .map_err(|()| JobsError::CancellationTimeout)
    }

    async fn cancel_all(&self) -> Result<()> {
        self.drain_all()
            .await
            .map_err(|()| JobsError::CancellationTimeout)
    }
}

impl Service {
    fn submit_inner(&self, scope: Option<&JobScope>, spec: JobSpec) -> Result<JobHandle> {
        validate_job_name(&spec.name)?;
        let executor = tokio::runtime::Handle::try_current()
            .map_err(|_| JobsError::Execution("Tokio runtime is unavailable".into()))?;
        if !self.accepting.load(Ordering::Acquire) || self.draining.load(Ordering::Acquire) {
            return Err(JobsError::ShuttingDown);
        }
        let id_number = self
            .next_id
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |value| {
                value.checked_add(1)
            })
            .map_err(|_| JobsError::InvalidInput("job identity exhausted".into()))?
            + 1;
        let id = format!("job-{id_number}");
        let state = Arc::new(State::new());
        {
            let mut registry = self
                .registry
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if !self.accepting.load(Ordering::Acquire) || self.draining.load(Ordering::Acquire) {
                return Err(JobsError::ShuttingDown);
            }
            if scope
                .as_ref()
                .is_some_and(|scope| registry.blocked_scopes.contains(scope))
            {
                return Err(JobsError::ShuttingDown);
            }
            if registry.jobs.len() >= self.config.maximum_active_jobs {
                return Err(JobsError::Capacity);
            }
            registry.jobs.insert(
                id.clone(),
                ActiveJob {
                    state: Arc::clone(&state),
                    scope: scope.cloned(),
                },
            );
            if let Some(scope) = scope {
                let count = registry
                    .unsettled_by_scope
                    .entry(scope.clone())
                    .or_default();
                *count = count.checked_add(1).ok_or(JobsError::Capacity)?;
            }
        }
        let service = self.self_weak.clone();
        let task_state = Arc::clone(&state);
        let task_id = id.clone();
        executor.spawn(async move {
            task_state
                .status
                .store(status_byte(JobStatus::Running), Ordering::Release);
            let result = AssertUnwindSafe(spec.task.run(task_state.cancellation.clone()))
                .catch_unwind()
                .await;
            let outcome = if task_state.cancellation.is_cancelled() {
                JobOutcome::Cancelled
            } else {
                match result {
                    Ok(Ok(value)) => match serde_json::to_vec(&value) {
                        Ok(bytes) if bytes.len() <= MAXIMUM_JOB_RESULT_BYTES => {
                            JobOutcome::Completed(value)
                        }
                        Ok(_) => JobOutcome::failed("job result is too large"),
                        Err(error) => JobOutcome::failed(error.to_string()),
                    },
                    Ok(Err(error)) => JobOutcome::failed(error.to_string()),
                    Err(_) => JobOutcome::failed("job task panicked"),
                }
            };
            if let Some(service) = service.upgrade() {
                service.remove_job(&task_id);
            }
            task_state.finish(outcome);
        });
        let control: Arc<dyn JobControl> = state;
        Ok(JobHandle::new(id, control))
    }

    async fn drain_all(&self) -> std::result::Result<(), ()> {
        let _drain = self.drain_lock.lock().await;
        self.draining.store(true, Ordering::Release);
        let active = self
            .registry
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .jobs
            .values()
            .map(|active| Arc::clone(&active.state))
            .collect::<Vec<_>>();
        for state in &active {
            state.cancellation.cancel();
        }
        let wait = async {
            for state in active {
                let _ = state.join().await;
            }
        };
        let result =
            tokio::time::timeout(Duration::from_millis(self.config.shutdown_timeout_ms), wait)
                .await
                .map_err(|_| ());
        if result.is_ok() {
            self.global_drain_timed_out.store(false, Ordering::Release);
            if self.accepting.load(Ordering::Acquire) {
                self.draining.store(false, Ordering::Release);
            }
            Ok(())
        } else {
            self.global_drain_timed_out.store(true, Ordering::Release);
            self.release_global_drain_if_settled();
            Err(())
        }
    }

    async fn drain_scope(&self, scope: &JobScope) -> std::result::Result<(), ()> {
        let _drain = self.drain_lock.lock().await;
        let active = {
            let mut registry = self
                .registry
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            registry.blocked_scopes.insert(scope.clone());
            registry
                .jobs
                .values()
                .filter(|active| active.scope.as_ref() == Some(scope))
                .map(|active| Arc::clone(&active.state))
                .collect::<Vec<_>>()
        };
        for state in &active {
            state.cancellation.cancel();
        }
        let wait = async {
            for state in active {
                let _ = state.join().await;
            }
        };
        let result =
            tokio::time::timeout(Duration::from_millis(self.config.shutdown_timeout_ms), wait)
                .await
                .map_err(|_| ());
        let mut registry = self
            .registry
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if result.is_ok() {
            registry.blocked_scopes.remove(scope);
            Ok(())
        } else {
            registry.timed_out_scopes.insert(scope.clone());
            release_scope_if_settled(&mut registry, scope);
            Err(())
        }
    }

    fn remove_job(&self, id: &str) {
        let empty = {
            let mut registry = self
                .registry
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let removed = registry.jobs.remove(id);
            if let Some(scope) = removed.and_then(|active| active.scope) {
                let count = registry
                    .unsettled_by_scope
                    .get_mut(&scope)
                    .expect("scoped job retains an unsettled count");
                *count = count
                    .checked_sub(1)
                    .expect("scoped unsettled count cannot underflow");
                if *count == 0 {
                    registry.unsettled_by_scope.remove(&scope);
                }
                release_scope_if_settled(&mut registry, &scope);
            }
            registry.jobs.is_empty()
        };
        if empty {
            self.release_global_drain_if_settled();
        }
    }

    fn release_global_drain_if_settled(&self) {
        let empty = self
            .registry
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .jobs
            .is_empty();
        if empty
            && self.global_drain_timed_out.swap(false, Ordering::AcqRel)
            && self.accepting.load(Ordering::Acquire)
        {
            self.draining.store(false, Ordering::Release);
        }
    }

    async fn shutdown(&self) -> std::result::Result<(), String> {
        self.accepting.store(false, Ordering::Release);
        self.drain_all()
            .await
            .map_err(|()| "Jobs shutdown timed out".to_owned())
    }
}

/// Ordinary factory for one local Jobs scheduler generation.
#[derive(Clone, Debug, Default)]
pub struct JobsLocalFactory;

#[async_trait]
impl PluginFactory for JobsLocalFactory {
    fn prepare(&self, desired: &ConfigValue) -> rsi_meta::Result<PreparedActivation> {
        let config = if desired.is_null() {
            JobsLocalConfig::default()
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
            std::mem::size_of::<JobsLocalConfig>(),
        ))
    }

    async fn activate(&self, mut plan: ActivationPlan) -> rsi_meta::Result<()> {
        let config = plan.take_state::<JobsLocalConfig>()?;
        let service = Arc::new_cyclic(|self_weak| Service {
            config,
            accepting: AtomicBool::new(true),
            draining: AtomicBool::new(false),
            global_drain_timed_out: AtomicBool::new(false),
            drain_lock: AsyncMutex::new(()),
            next_id: AtomicU64::new(0),
            registry: Mutex::new(Registry::default()),
            self_weak: self_weak.clone(),
        });
        let jobs: Arc<dyn Jobs> = service.clone();
        let supply = plan.context().provide_local::<JobsContract>(jobs)?;
        plan.defer(
            "shutdown local Jobs",
            Box::new(move || {
                Box::pin(async move {
                    drop(supply);
                    service.shutdown().await
                })
            }),
        )
    }
}

fn release_scope_if_settled(registry: &mut Registry, scope: &JobScope) {
    if registry.timed_out_scopes.contains(scope) && !registry.unsettled_by_scope.contains_key(scope)
    {
        registry.timed_out_scopes.remove(scope);
        registry.blocked_scopes.remove(scope);
    }
}

const fn status_byte(status: JobStatus) -> u8 {
    match status {
        JobStatus::Queued => 0,
        JobStatus::Running => 1,
        JobStatus::Completed => 2,
        JobStatus::Failed => 3,
        JobStatus::Cancelled => 4,
    }
}

const fn byte_status(value: u8) -> JobStatus {
    match value {
        0 => JobStatus::Queued,
        1 => JobStatus::Running,
        2 => JobStatus::Completed,
        3 => JobStatus::Failed,
        _ => JobStatus::Cancelled,
    }
}
