//! Tokio-backed process-local Jobs ordinary plugin.

#![deny(unsafe_code)]
#![warn(missing_docs)]
#![allow(clippy::missing_errors_doc)]

use async_trait::async_trait;
use futures_util::FutureExt;
use rsi_jobs::{
    DEFAULT_MAXIMUM_ACTIVE_JOBS, DEFAULT_MAXIMUM_ACTIVE_JOBS_PER_SCOPE,
    DEFAULT_MAXIMUM_RETAINED_JOBS, DEFAULT_MAXIMUM_RETAINED_JOBS_PER_SCOPE, JobControl,
    JobFinalization, JobOutputRead, JobProducer, JobProducerLease, JobProducerRegistration,
    JobRead, JobScopeAuthority, JobScopeAuthorityState, JobScopeId, JobStatus, JobStream,
    JobSubmission, JobSummary, JobTerminal, Jobs, JobsContract, JobsError, MAXIMUM_JOBS_PER_LIST,
    Result, validate_job_identifier,
};
use rsi_meta::{ActivationPlan, ConfigValue, MetaError, PluginFactory, PreparedActivation};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};
use std::panic::AssertUnwindSafe;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, Weak};
use std::time::Duration;
use tokio::sync::Notify;

static NEXT_PROVIDER_ID: AtomicU64 = AtomicU64::new(0);

/// Configuration accepted by [`JobsLocalFactory`].
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct JobsLocalConfig {
    /// Maximum simultaneous live jobs in one exact scope generation.
    #[serde(default = "default_maximum_active_jobs_per_scope")]
    pub maximum_active_jobs_per_scope: usize,
    /// Maximum simultaneous live jobs provider-wide.
    #[serde(default = "default_maximum_active_jobs")]
    pub maximum_active_jobs: usize,
    /// Maximum retained records in one exact scope generation.
    #[serde(default = "default_maximum_retained_jobs_per_scope")]
    pub maximum_retained_jobs_per_scope: usize,
    /// Maximum retained records provider-wide.
    #[serde(default = "default_maximum_retained_jobs")]
    pub maximum_retained_jobs: usize,
    /// Explicit lifecycle wait bound in milliseconds.
    #[serde(default = "default_shutdown_timeout_ms")]
    pub shutdown_timeout_ms: u64,
}

const fn default_maximum_active_jobs_per_scope() -> usize {
    DEFAULT_MAXIMUM_ACTIVE_JOBS_PER_SCOPE
}

const fn default_maximum_active_jobs() -> usize {
    DEFAULT_MAXIMUM_ACTIVE_JOBS
}

const fn default_maximum_retained_jobs_per_scope() -> usize {
    DEFAULT_MAXIMUM_RETAINED_JOBS_PER_SCOPE
}

const fn default_maximum_retained_jobs() -> usize {
    DEFAULT_MAXIMUM_RETAINED_JOBS
}

const fn default_shutdown_timeout_ms() -> u64 {
    10_000
}

impl Default for JobsLocalConfig {
    fn default() -> Self {
        Self {
            maximum_active_jobs_per_scope: default_maximum_active_jobs_per_scope(),
            maximum_active_jobs: default_maximum_active_jobs(),
            maximum_retained_jobs_per_scope: default_maximum_retained_jobs_per_scope(),
            maximum_retained_jobs: default_maximum_retained_jobs(),
            shutdown_timeout_ms: default_shutdown_timeout_ms(),
        }
    }
}

impl JobsLocalConfig {
    fn validate(&self) -> Result<()> {
        for (name, value) in [
            (
                "maximum_active_jobs_per_scope",
                self.maximum_active_jobs_per_scope,
            ),
            ("maximum_active_jobs", self.maximum_active_jobs),
            (
                "maximum_retained_jobs_per_scope",
                self.maximum_retained_jobs_per_scope,
            ),
            ("maximum_retained_jobs", self.maximum_retained_jobs),
        ] {
            if value == 0 || value > 65_536 {
                return Err(JobsError::InvalidInput(format!(
                    "{name} must be within 1..=65536"
                )));
            }
        }
        if self.maximum_active_jobs_per_scope > self.maximum_active_jobs {
            return Err(JobsError::InvalidInput(
                "maximum_active_jobs_per_scope cannot exceed maximum_active_jobs".into(),
            ));
        }
        if self.maximum_retained_jobs_per_scope > self.maximum_retained_jobs {
            return Err(JobsError::InvalidInput(
                "maximum_retained_jobs_per_scope cannot exceed maximum_retained_jobs".into(),
            ));
        }
        if self.maximum_retained_jobs_per_scope > MAXIMUM_JOBS_PER_LIST {
            return Err(JobsError::InvalidInput(format!(
                "maximum_retained_jobs_per_scope cannot exceed the list bound of {MAXIMUM_JOBS_PER_LIST}"
            )));
        }
        if self.maximum_active_jobs > self.maximum_retained_jobs
            || self.maximum_active_jobs_per_scope > self.maximum_retained_jobs_per_scope
        {
            return Err(JobsError::InvalidInput(
                "active job capacity cannot exceed retained record capacity".into(),
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
    provider_id: u64,
    accepting: AtomicBool,
    next_id: AtomicU64,
    next_scope_generation: AtomicU64,
    next_producer_generation: AtomicU64,
    registry: Mutex<Registry>,
    changed: Notify,
    self_weak: Weak<Service>,
}

#[derive(Debug, Default)]
struct Registry {
    producers: HashMap<String, ProducerEntry>,
    scopes: HashMap<JobScopeId, Weak<JobScopeAuthorityState>>,
    jobs: BTreeMap<String, JobRecord>,
    reservations_global: usize,
    reservations_by_scope: HashMap<u64, usize>,
    active_global: usize,
    active_by_scope: HashMap<u64, usize>,
    active_by_producer: HashMap<u64, usize>,
}

#[derive(Clone, Debug)]
struct ProducerEntry {
    generation: u64,
    producer: Arc<dyn JobProducer>,
}

#[derive(Debug)]
struct JobRecord {
    sequence: u64,
    scope: JobScopeAuthority,
    name: String,
    producer: String,
    producer_generation: u64,
    status: JobStatus,
    control: Option<Arc<dyn JobControl>>,
    terminal: Option<JobTerminal>,
    requires_report: bool,
    reported: bool,
    readers: usize,
    stream_ends: [u64; 2],
    settled: Arc<Notify>,
}

impl JobRecord {
    fn summary(&self, id: &str) -> JobSummary {
        JobSummary {
            id: id.to_owned(),
            name: self.name.clone(),
            producer: self.producer.clone(),
            status: self.status,
            requires_report: self.requires_report,
            reported: self.reported,
            terminal: self.terminal.clone(),
            output_retained: self.control.is_some(),
        }
    }
}

#[derive(Clone, Copy, Debug)]
struct Reservation {
    scope_generation: u64,
    producer_generation: u64,
}

#[async_trait]
impl Jobs for Service {
    fn register_producer(&self, registration: JobProducerRegistration) -> Result<JobProducerLease> {
        validate_job_identifier("job producer name", &registration.name)?;
        let generation = next_generation(
            &self.next_producer_generation,
            "job producer generation exhausted",
        )?;
        {
            let mut registry = lock(&self.registry);
            if !self.accepting.load(Ordering::Acquire) {
                return Err(JobsError::ShuttingDown);
            }
            if registry.producers.contains_key(&registration.name) {
                return Err(JobsError::DuplicateProducer(registration.name));
            }
            registry.producers.insert(
                registration.name.clone(),
                ProducerEntry {
                    generation,
                    producer: registration.producer,
                },
            );
        }

        let name = registration.name;
        let withdraw_service = self.self_weak.clone();
        let withdraw_name = name.clone();
        let settle_service = self.self_weak.clone();
        Ok(JobProducerLease::new(
            move || {
                if let Some(service) = withdraw_service.upgrade() {
                    service.withdraw_producer(&withdraw_name, generation);
                }
            },
            move || async move {
                let Some(service) = settle_service.upgrade() else {
                    return Ok(());
                };
                service
                    .wait_for_producer(generation, service.config.shutdown_timeout_ms)
                    .await
            },
        ))
    }

    fn acquire_scope(&self, id: JobScopeId) -> Result<JobScopeAuthority> {
        if !self.accepting.load(Ordering::Acquire) {
            return Err(JobsError::ShuttingDown);
        }
        let mut registry = lock(&self.registry);
        prune_dead_scopes(&mut registry.scopes);
        if !self.accepting.load(Ordering::Acquire) {
            return Err(JobsError::ShuttingDown);
        }
        if let Some(state) = registry.scopes.get(&id).and_then(Weak::upgrade) {
            let authority = JobScopeAuthority::from_provider_state(id.clone(), state);
            if authority.is_active() {
                return Ok(authority);
            }
        }
        let generation = next_generation(
            &self.next_scope_generation,
            "job scope generation exhausted",
        )?;
        let authority = JobScopeAuthority::provider_owned(id.clone(), self.provider_id, generation);
        registry.scopes.insert(id, authority.provider_state());
        Ok(authority)
    }

    #[allow(clippy::too_many_lines)] // Admission, publication, and unpublished ownership form one linearization protocol.
    fn submit(&self, scope: &JobScopeAuthority, submission: JobSubmission) -> Result<String> {
        validate_job_identifier("job name", &submission.name)?;
        validate_job_identifier("job producer name", &submission.producer)?;
        let executor = tokio::runtime::Handle::try_current()
            .map_err(|_| JobsError::Execution("Tokio runtime is unavailable".into()))?;

        let (producer, producer_generation, reservation) = {
            let mut registry = lock(&self.registry);
            self.validate_scope(&registry, scope)?;
            let producer = registry
                .producers
                .get(&submission.producer)
                .cloned()
                .ok_or_else(|| JobsError::UnknownProducer(submission.producer.clone()))?;
            self.reserve(&mut registry, scope, producer.generation)?;
            (
                producer.producer,
                producer.generation,
                Reservation {
                    scope_generation: scope.generation(),
                    producer_generation: producer.generation,
                },
            )
        };

        let started =
            std::panic::catch_unwind(AssertUnwindSafe(|| producer.start(&submission.request)));
        let control = match started {
            Ok(Ok(control)) => control,
            Ok(Err(error)) => {
                self.release_reservation(reservation, true);
                return Err(error);
            }
            Err(_) => {
                self.release_reservation(reservation, true);
                return Err(JobsError::Execution("job producer panicked".into()));
            }
        };

        let publication = {
            let mut registry = lock(&self.registry);
            release_reservation_count(&mut registry, reservation);
            let admission_error = if !self.accepting.load(Ordering::Acquire) {
                Some(JobsError::ShuttingDown)
            } else if !scope_is_current(&registry, self.provider_id, scope) {
                Some(JobsError::ScopeClosed)
            } else if registry
                .producers
                .get(&submission.producer)
                .is_none_or(|entry| entry.generation != producer_generation)
            {
                Some(JobsError::UnknownProducer(submission.producer.clone()))
            } else {
                None
            };
            if let Some(error) = admission_error {
                Err(error)
            } else {
                next_generation(&self.next_id, "job identity exhausted").map(|sequence| {
                    let id = format!("job-{sequence}");
                    registry.jobs.insert(
                        id.clone(),
                        JobRecord {
                            sequence,
                            scope: scope.clone(),
                            name: submission.name,
                            producer: submission.producer,
                            producer_generation,
                            status: JobStatus::Running,
                            control: Some(control.clone()),
                            terminal: None,
                            requires_report: submission.requires_report,
                            reported: false,
                            readers: 0,
                            stream_ends: [0, 0],
                            settled: Arc::new(Notify::new()),
                        },
                    );
                    id
                })
            }
        };
        self.changed.notify_waiters();

        let id = match publication {
            Ok(id) => id,
            Err(error) => {
                let _ = contained_cancel(&control);
                spawn_unpublished_reaper(
                    &executor,
                    self.self_weak
                        .upgrade()
                        .expect("live Jobs service owns itself during submission"),
                    reservation,
                    control,
                );
                return Err(error);
            }
        };
        let service = self.self_weak.clone();
        let watcher_control = control;
        let watcher_id = id.clone();
        executor.spawn(async move {
            let terminal = contained_wait(watcher_control.clone()).await;
            if let Some(service) = service.upgrade() {
                service.settle(&watcher_id, &watcher_control, terminal);
            }
        });
        Ok(id)
    }

    fn list(&self, scope: &JobScopeAuthority) -> Result<Vec<JobSummary>> {
        let registry = lock(&self.registry);
        self.validate_scope(&registry, scope)?;
        let mut records = registry
            .jobs
            .iter()
            .filter(|(_, record)| record.scope.same_generation(scope))
            .map(|(id, record)| (record.sequence, record.summary(id)))
            .collect::<Vec<_>>();
        records.sort_by_key(|(sequence, _)| *sequence);
        debug_assert!(records.len() <= MAXIMUM_JOBS_PER_LIST);
        Ok(records.into_iter().map(|(_, summary)| summary).collect())
    }

    fn get(&self, scope: &JobScopeAuthority, id: &str) -> Result<JobSummary> {
        validate_job_identifier("job identity", id)?;
        let registry = lock(&self.registry);
        self.validate_scope(&registry, scope)?;
        let record = visible_record(&registry, scope, id)?;
        Ok(record.summary(id))
    }

    fn read(
        &self,
        scope: &JobScopeAuthority,
        id: &str,
        stdout_offset: u64,
        stderr_offset: u64,
    ) -> Result<JobRead> {
        validate_job_identifier("job identity", id)?;
        let (control, terminal, stream_ends) = {
            let mut registry = lock(&self.registry);
            self.validate_scope(&registry, scope)?;
            let record = visible_record_mut(&mut registry, scope, id)?;
            record.readers = record.readers.checked_add(1).ok_or(JobsError::Capacity)?;
            (
                record.control.clone(),
                record.status.is_terminal(),
                record.stream_ends,
            )
        };
        let result = (|| {
            let (mut stdout, mut stderr) = if let Some(control) = &control {
                (
                    contained_read(control, JobStream::Stdout, stdout_offset)?,
                    contained_read(control, JobStream::Stderr, stderr_offset)?,
                )
            } else {
                (
                    compacted_read(stream_ends[0], stdout_offset),
                    compacted_read(stream_ends[1], stderr_offset),
                )
            };
            let active_summary = {
                let registry = lock(&self.registry);
                let record = visible_record(&registry, scope, id)?;
                (!terminal && !record.status.is_terminal()).then(|| record.summary(id))
            };
            if let Some(job) = active_summary {
                return Ok(JobRead {
                    job,
                    stdout,
                    stderr,
                });
            }
            if let Some(control) = &control {
                stdout = contained_read(control, JobStream::Stdout, stdout_offset)?;
                stderr = contained_read(control, JobStream::Stderr, stderr_offset)?;
            }
            let job = self.report_job(id)?;
            Ok(JobRead {
                job,
                stdout,
                stderr,
            })
        })();
        if let Some(record) = lock(&self.registry).jobs.get_mut(id) {
            record.readers = record
                .readers
                .checked_sub(1)
                .expect("every admitted read has one release");
        }
        result
    }

    async fn wait(
        &self,
        scope: &JobScopeAuthority,
        id: &str,
        stdout_offset: u64,
        stderr_offset: u64,
    ) -> Result<JobRead> {
        self.wait_visible(scope, id, false, stdout_offset, stderr_offset)
            .await
    }

    async fn kill(&self, scope: &JobScopeAuthority, id: &str) -> Result<JobRead> {
        self.wait_visible(scope, id, true, 0, 0).await
    }

    async fn finalize_scope(&self, scope: &JobScopeAuthority) -> Result<JobFinalization> {
        let controls = {
            let mut registry = lock(&self.registry);
            self.validate_scope(&registry, scope)?;
            scope.revoke();
            if registry
                .scopes
                .get(scope.id())
                .and_then(Weak::upgrade)
                .is_some_and(|state| scope.owns_provider_state(&state))
            {
                registry.scopes.remove(scope.id());
            }
            registry
                .jobs
                .values_mut()
                .filter(|record| {
                    record.scope.same_generation(scope) && !record.status.is_terminal()
                })
                .filter_map(|record| {
                    record.status = JobStatus::Stopping;
                    record.control.clone()
                })
                .collect::<Vec<_>>()
        };
        for control in controls {
            let _ = contained_cancel(&control);
        }
        self.changed.notify_waiters();

        let Some(service) = self.self_weak.upgrade() else {
            return Err(JobsError::ShuttingDown);
        };
        let generation = scope.generation();
        let mut reaper = tokio::spawn(async move { service.finalize_generation(generation).await });
        match tokio::time::timeout(
            Duration::from_millis(self.config.shutdown_timeout_ms),
            &mut reaper,
        )
        .await
        {
            Ok(Ok(result)) => result,
            Ok(Err(error)) => Err(JobsError::Execution(format!(
                "job finalization reaper failed: {error}"
            ))),
            Err(_) => Err(JobsError::CancellationTimeout),
        }
    }

    async fn cancel_all(&self) -> Result<()> {
        self.withdraw_all();
        self.wait_for_all(self.config.shutdown_timeout_ms).await
    }
}

impl Service {
    fn validate_scope(&self, registry: &Registry, scope: &JobScopeAuthority) -> Result<()> {
        if !self.accepting.load(Ordering::Acquire) {
            return Err(JobsError::ShuttingDown);
        }
        if scope_is_current(registry, self.provider_id, scope) {
            Ok(())
        } else {
            Err(JobsError::ScopeClosed)
        }
    }

    fn reserve(
        &self,
        registry: &mut Registry,
        scope: &JobScopeAuthority,
        producer_generation: u64,
    ) -> Result<()> {
        compact_for_admission(registry, &self.config, scope.generation())?;
        if registry.active_global >= self.config.maximum_active_jobs
            || registry
                .active_by_scope
                .get(&scope.generation())
                .copied()
                .unwrap_or(0)
                >= self.config.maximum_active_jobs_per_scope
        {
            return Err(JobsError::Capacity);
        }
        registry.active_global += 1;
        *registry
            .active_by_scope
            .entry(scope.generation())
            .or_default() += 1;
        *registry
            .active_by_producer
            .entry(producer_generation)
            .or_default() += 1;
        registry.reservations_global += 1;
        *registry
            .reservations_by_scope
            .entry(scope.generation())
            .or_default() += 1;
        Ok(())
    }

    fn release_reservation(&self, reservation: Reservation, release_active: bool) {
        let mut registry = lock(&self.registry);
        release_reservation_count(&mut registry, reservation);
        if release_active {
            decrement_active(&mut registry, reservation);
        }
        drop(registry);
        self.changed.notify_waiters();
    }

    fn settle(&self, id: &str, control: &Arc<dyn JobControl>, terminal: JobTerminal) {
        let stream_ends = capture_stream_ends(control);
        let mut registry = lock(&self.registry);
        let Some(record) = registry.jobs.get_mut(id) else {
            return;
        };
        if record.status.is_terminal() {
            return;
        }
        let reservation = Reservation {
            scope_generation: record.scope.generation(),
            producer_generation: record.producer_generation,
        };
        record.status = terminal.status;
        record.terminal = Some(terminal);
        record.stream_ends = stream_ends;
        if !record.requires_report {
            record.reported = true;
            record.control = None;
        }
        let settled = Arc::clone(&record.settled);
        decrement_active(&mut registry, reservation);
        drop(registry);
        settled.notify_waiters();
        self.changed.notify_waiters();
    }

    async fn wait_visible(
        &self,
        scope: &JobScopeAuthority,
        id: &str,
        cancel: bool,
        stdout_offset: u64,
        stderr_offset: u64,
    ) -> Result<JobRead> {
        validate_job_identifier("job identity", id)?;
        let mut cancellation_sent = false;
        loop {
            let (notified, control) = {
                let mut registry = lock(&self.registry);
                self.validate_scope(&registry, scope)?;
                let record = visible_record_mut(&mut registry, scope, id)?;
                if record.status.is_terminal() {
                    drop(registry);
                    return self.read(scope, id, stdout_offset, stderr_offset);
                }
                let notified = Arc::clone(&record.settled).notified_owned();
                let control = if cancel && !cancellation_sent {
                    record.status = JobStatus::Stopping;
                    record.control.clone()
                } else {
                    None
                };
                (notified, control)
            };
            if let Some(control) = control {
                cancellation_sent = true;
                contained_cancel(&control)?;
            }
            notified.await;
        }
    }

    fn report_job(&self, id: &str) -> Result<JobSummary> {
        self.report_job_inner(id, false)?
            .ok_or_else(|| JobsError::UnknownJob(id.to_owned()))
    }

    fn report_job_if_unreported(&self, id: &str) -> Result<Option<JobSummary>> {
        self.report_job_inner(id, true)
    }

    fn report_job_inner(&self, id: &str, only_if_unreported: bool) -> Result<Option<JobSummary>> {
        let control = {
            let registry = lock(&self.registry);
            let Some(record) = registry.jobs.get(id) else {
                return if only_if_unreported {
                    Ok(None)
                } else {
                    Err(JobsError::UnknownJob(id.to_owned()))
                };
            };
            if only_if_unreported && record.reported {
                return Ok(None);
            }
            if !record.status.is_terminal() {
                return if only_if_unreported {
                    Err(JobsError::Execution(
                        "scope finalization observed nonterminal inactive work".into(),
                    ))
                } else {
                    Ok(Some(record.summary(id)))
                };
            }
            record.control.clone()
        };
        let stream_ends = control.as_ref().map_or([0, 0], capture_stream_ends);
        let mut registry = lock(&self.registry);
        let Some(record) = registry.jobs.get_mut(id) else {
            return if only_if_unreported {
                Ok(None)
            } else {
                Err(JobsError::UnknownJob(id.to_owned()))
            };
        };
        if only_if_unreported && record.reported {
            return Ok(None);
        }
        if record.status.is_terminal() {
            record.reported = true;
            if record.control.is_some() {
                record.stream_ends = stream_ends;
                record.control = None;
            }
        }
        Ok(Some(record.summary(id)))
    }

    fn withdraw_producer(&self, name: &str, generation: u64) {
        let controls = {
            let mut registry = lock(&self.registry);
            if registry
                .producers
                .get(name)
                .is_some_and(|entry| entry.generation == generation)
            {
                registry.producers.remove(name);
            }
            registry
                .jobs
                .values_mut()
                .filter(|record| {
                    record.producer_generation == generation && !record.status.is_terminal()
                })
                .filter_map(|record| {
                    record.status = JobStatus::Stopping;
                    record.control.clone()
                })
                .collect::<Vec<_>>()
        };
        for control in controls {
            let _ = contained_cancel(&control);
        }
        self.changed.notify_waiters();
    }

    async fn wait_for_producer(&self, generation: u64, timeout_ms: u64) -> Result<()> {
        let wait = async {
            loop {
                let notified = self.changed.notified();
                if lock(&self.registry)
                    .active_by_producer
                    .get(&generation)
                    .copied()
                    .unwrap_or(0)
                    == 0
                {
                    return;
                }
                notified.await;
            }
        };
        tokio::time::timeout(Duration::from_millis(timeout_ms), wait)
            .await
            .map_err(|_| JobsError::CancellationTimeout)
    }

    async fn finalize_generation(&self, generation: u64) -> Result<JobFinalization> {
        loop {
            let notified = self.changed.notified();
            if lock(&self.registry)
                .active_by_scope
                .get(&generation)
                .copied()
                .unwrap_or(0)
                == 0
            {
                break;
            }
            notified.await;
        }
        let mut pending = lock(&self.registry)
            .jobs
            .iter()
            .filter(|(_, record)| {
                record.scope.generation() == generation
                    && record.requires_report
                    && !record.reported
            })
            .map(|(id, record)| (record.sequence, id.clone()))
            .collect::<Vec<_>>();
        pending.sort_by_key(|(sequence, _)| *sequence);
        let mut unreported = Vec::with_capacity(pending.len());
        for (_, id) in pending {
            if let Some(job) = self.report_job_if_unreported(&id)? {
                unreported.push(job);
            }
        }
        Ok(JobFinalization { unreported })
    }

    fn withdraw_all(&self) {
        self.accepting.store(false, Ordering::Release);
        let controls = {
            let mut registry = lock(&self.registry);
            registry.producers.clear();
            for state in registry.scopes.values().filter_map(Weak::upgrade) {
                state.revoke();
            }
            registry.scopes.clear();
            registry
                .jobs
                .values_mut()
                .filter(|record| !record.status.is_terminal())
                .filter_map(|record| {
                    record.status = JobStatus::Stopping;
                    record.control.clone()
                })
                .collect::<Vec<_>>()
        };
        for control in controls {
            let _ = contained_cancel(&control);
        }
        self.changed.notify_waiters();
    }

    async fn wait_for_all(&self, timeout_ms: u64) -> Result<()> {
        let wait = async {
            loop {
                let notified = self.changed.notified();
                if lock(&self.registry).active_global == 0 {
                    return;
                }
                notified.await;
            }
        };
        tokio::time::timeout(Duration::from_millis(timeout_ms), wait)
            .await
            .map_err(|_| JobsError::CancellationTimeout)
    }

    async fn shutdown(&self) -> std::result::Result<(), String> {
        self.withdraw_all();
        self.wait_for_all(self.config.shutdown_timeout_ms)
            .await
            .map_err(|error| error.to_string())
    }

    fn settle_unpublished(&self, reservation: Reservation) {
        let mut registry = lock(&self.registry);
        decrement_active(&mut registry, reservation);
        drop(registry);
        self.changed.notify_waiters();
    }
}

/// Ordinary factory for one local Jobs control-plane generation.
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
            provider_id: NEXT_PROVIDER_ID.fetch_add(1, Ordering::AcqRel) + 1,
            accepting: AtomicBool::new(true),
            next_id: AtomicU64::new(0),
            next_scope_generation: AtomicU64::new(0),
            next_producer_generation: AtomicU64::new(0),
            registry: Mutex::new(Registry::default()),
            changed: Notify::new(),
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

fn scope_is_current(registry: &Registry, provider_id: u64, scope: &JobScopeAuthority) -> bool {
    scope.belongs_to(provider_id)
        && scope.is_active()
        && registry
            .scopes
            .get(scope.id())
            .and_then(Weak::upgrade)
            .is_some_and(|state| scope.owns_provider_state(&state))
}

fn prune_dead_scopes(scopes: &mut HashMap<JobScopeId, Weak<JobScopeAuthorityState>>) {
    scopes.retain(|_, state| state.strong_count() > 0);
}

fn visible_record<'a>(
    registry: &'a Registry,
    scope: &JobScopeAuthority,
    id: &str,
) -> Result<&'a JobRecord> {
    registry
        .jobs
        .get(id)
        .filter(|record| record.scope.same_generation(scope))
        .ok_or_else(|| JobsError::UnknownJob(id.to_owned()))
}

fn visible_record_mut<'a>(
    registry: &'a mut Registry,
    scope: &JobScopeAuthority,
    id: &str,
) -> Result<&'a mut JobRecord> {
    registry
        .jobs
        .get_mut(id)
        .filter(|record| record.scope.same_generation(scope))
        .ok_or_else(|| JobsError::UnknownJob(id.to_owned()))
}

fn compact_for_admission(
    registry: &mut Registry,
    config: &JobsLocalConfig,
    scope_generation: u64,
) -> Result<()> {
    while scope_record_count(registry, scope_generation)
        .checked_add(
            registry
                .reservations_by_scope
                .get(&scope_generation)
                .copied()
                .unwrap_or(0),
        )
        .is_none_or(|count| count >= config.maximum_retained_jobs_per_scope)
    {
        let Some(id) = oldest_evictable(registry, Some(scope_generation)) else {
            return Err(JobsError::Capacity);
        };
        registry.jobs.remove(&id);
    }
    while registry
        .jobs
        .len()
        .checked_add(registry.reservations_global)
        .is_none_or(|count| count >= config.maximum_retained_jobs)
    {
        let Some(id) = oldest_evictable(registry, None) else {
            return Err(JobsError::Capacity);
        };
        registry.jobs.remove(&id);
    }
    Ok(())
}

fn scope_record_count(registry: &Registry, generation: u64) -> usize {
    registry
        .jobs
        .values()
        .filter(|record| record.scope.generation() == generation)
        .count()
}

fn oldest_evictable(registry: &Registry, scope_generation: Option<u64>) -> Option<String> {
    registry
        .jobs
        .iter()
        .filter(|(_, record)| {
            record.status.is_terminal()
                && record.reported
                && record.readers == 0
                && scope_generation.is_none_or(|generation| record.scope.generation() == generation)
        })
        .min_by_key(|(_, record)| record.sequence)
        .map(|(id, _)| id.clone())
}

fn release_reservation_count(registry: &mut Registry, reservation: Reservation) {
    registry.reservations_global = registry
        .reservations_global
        .checked_sub(1)
        .expect("every reservation release has an admission");
    decrement_map(
        &mut registry.reservations_by_scope,
        reservation.scope_generation,
    );
}

fn decrement_active(registry: &mut Registry, reservation: Reservation) {
    registry.active_global = registry
        .active_global
        .checked_sub(1)
        .expect("every active release has an admission");
    decrement_map(&mut registry.active_by_scope, reservation.scope_generation);
    decrement_map(
        &mut registry.active_by_producer,
        reservation.producer_generation,
    );
}

fn decrement_map(map: &mut HashMap<u64, usize>, key: u64) {
    let count = map
        .get_mut(&key)
        .expect("every generation count release has an admission");
    *count = count
        .checked_sub(1)
        .expect("generation count cannot underflow");
    if *count == 0 {
        map.remove(&key);
    }
}

fn capture_stream_ends(control: &Arc<dyn JobControl>) -> [u64; 2] {
    [JobStream::Stdout, JobStream::Stderr]
        .map(|stream| contained_read(control, stream, 0).map_or(0, |read| read.next_offset))
}

fn contained_read(
    control: &Arc<dyn JobControl>,
    stream: JobStream,
    offset: u64,
) -> Result<JobOutputRead> {
    std::panic::catch_unwind(AssertUnwindSafe(|| control.read(stream, offset)))
        .map_err(|_| JobsError::Execution("job control read panicked".into()))?
}

fn contained_cancel(control: &Arc<dyn JobControl>) -> Result<()> {
    std::panic::catch_unwind(AssertUnwindSafe(|| control.cancel()))
        .map_err(|_| JobsError::Execution("job control cancellation panicked".into()))
}

fn compacted_read(stream_end: u64, offset: u64) -> JobOutputRead {
    JobOutputRead {
        bytes: Vec::new(),
        oldest_offset: stream_end,
        next_offset: stream_end,
        lossy: offset < stream_end,
    }
}

fn next_generation(counter: &AtomicU64, exhausted: &str) -> Result<u64> {
    counter
        .fetch_update(Ordering::AcqRel, Ordering::Acquire, |value| {
            value.checked_add(1)
        })
        .map(|value| value + 1)
        .map_err(|_| JobsError::Execution(exhausted.into()))
}

async fn contained_wait(control: Arc<dyn JobControl>) -> JobTerminal {
    match AssertUnwindSafe(control.wait()).catch_unwind().await {
        Ok(Ok(terminal)) if terminal.validate().is_ok() => terminal,
        Ok(Ok(_)) => failed_terminal("job control returned an invalid terminal value"),
        Ok(Err(error)) => failed_terminal(&error.to_string()),
        Err(_) => failed_terminal("job control panicked"),
    }
}

fn failed_terminal(message: &str) -> JobTerminal {
    let mut message = message.to_owned();
    message.retain(|character| character != '\0');
    let maximum = rsi_jobs::MAXIMUM_JOB_IDENTIFIER_BYTES * 16;
    if message.len() > maximum {
        let mut boundary = maximum;
        while !message.is_char_boundary(boundary) {
            boundary -= 1;
        }
        message.truncate(boundary);
    }
    JobTerminal {
        status: JobStatus::Failed,
        exit_code: None,
        signal: None,
        message: Some(message),
    }
}

fn spawn_unpublished_reaper(
    executor: &tokio::runtime::Handle,
    service: Arc<Service>,
    reservation: Reservation,
    control: Arc<dyn JobControl>,
) {
    executor.spawn(async move {
        let _terminal = contained_wait(control).await;
        service.settle_unpublished(reservation);
    });
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dead_scope_lookup_entries_are_pruned() {
        let id = JobScopeId::new("test", ["dead"]).unwrap();
        let authority = JobScopeAuthority::provider_owned(id.clone(), 1, 1);
        let mut scopes = HashMap::from([(id, authority.provider_state())]);
        drop(authority);

        prune_dead_scopes(&mut scopes);

        assert!(scopes.is_empty());
    }

    #[test]
    fn per_scope_retention_cannot_exceed_the_list_contract() {
        let config = JobsLocalConfig {
            maximum_active_jobs_per_scope: 1,
            maximum_active_jobs: 1,
            maximum_retained_jobs_per_scope: MAXIMUM_JOBS_PER_LIST + 1,
            maximum_retained_jobs: MAXIMUM_JOBS_PER_LIST + 1,
            shutdown_timeout_ms: 1,
        };

        assert!(matches!(
            config.validate(),
            Err(JobsError::InvalidInput(message)) if message.contains("list bound")
        ));
    }
}
