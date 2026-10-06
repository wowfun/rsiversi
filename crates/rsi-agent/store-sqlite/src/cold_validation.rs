//! Bounded cold-read ownership and connection-scoped cooperative cancellation.
use super::*;
use std::cell::RefCell;
use tokio::sync::{OwnedSemaphorePermit, watch};
use tokio_util::sync::CancellationToken;

const CAPACITY: usize = 256;
type Proof = Arc<preparation::ValidatedSessionProof>;

pub(super) struct ColdValidation {
    callers: Arc<Semaphore>,
    jobs: Arc<Semaphore>,
    pub(super) flights: Mutex<BTreeMap<SessionId, Arc<Flight>>>,
    cache_hits: AtomicU64,
    shared_flights: AtomicU64,
    refusals: AtomicU64,
}
impl Default for ColdValidation {
    fn default() -> Self {
        Self {
            callers: Arc::new(Semaphore::new(CAPACITY)),
            jobs: Arc::new(Semaphore::new(CAPACITY)),
            flights: Mutex::new(BTreeMap::new()),
            cache_hits: AtomicU64::new(0),
            shared_flights: AtomicU64::new(0),
            refusals: AtomicU64::new(0),
        }
    }
}
impl ColdValidation {
    fn shared(&self) {
        self.shared_flights.fetch_add(1, Ordering::Relaxed);
    }
    fn hit(&self) {
        self.cache_hits.fetch_add(1, Ordering::Relaxed);
    }
    fn caller(&self) -> Result<OwnedSemaphorePermit> {
        self.callers.clone().try_acquire_owned().map_err(|_| {
            self.refusals.fetch_add(1, Ordering::Relaxed);
            StoreError::ValidationBusy
        })
    }
    fn job(&self) -> Result<OwnedSemaphorePermit> {
        self.jobs.clone().try_acquire_owned().map_err(|_| {
            self.refusals.fetch_add(1, Ordering::Relaxed);
            StoreError::ValidationBusy
        })
    }
}

pub(super) struct Flight {
    #[cfg(test)]
    pub(super) state: Mutex<FlightState>,
    result: watch::Sender<Option<Result<Proof>>>,
}
#[cfg(test)]
pub(super) struct FlightState {
    pub(super) waiters: usize,
}
struct FlightCompletion {
    owner: Arc<StoreInner>,
    selected: SessionId,
    flight: Arc<Flight>,
    _job: Arc<OwnedSemaphorePermit>,
    result: Mutex<Option<Result<Proof>>>,
}
impl Drop for FlightCompletion {
    fn drop(&mut self) {
        let result = self
            .result
            .get_mut()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take()
            .unwrap_or_else(|| Err(StoreError::Io("SQLite validation worker stopped".into())));
        self.flight.result.send_replace(Some(result));
        let mut flights = self
            .owner
            .cold_validation
            .flights
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if flights
            .get(&self.selected)
            .is_some_and(|flight| Arc::ptr_eq(flight, &self.flight))
        {
            flights.remove(&self.selected);
        }
    }
}
struct Waiter {
    #[cfg(test)]
    flight: Arc<Flight>,
    _caller: OwnedSemaphorePermit,
}
#[cfg(test)]
impl Drop for Waiter {
    fn drop(&mut self) {
        let mut state = self
            .flight
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.waiters -= 1;
        // A Session proof is reusable work owned by the bounded job, not its waiters.
    }
}
struct StopOnDrop(CancellationToken);
impl Drop for StopOnDrop {
    fn drop(&mut self) {
        self.0.cancel();
    }
}

thread_local! {
    static READ_SCOPE: RefCell<Option<CancellationToken>> = const { RefCell::new(None) };
}
struct ReadScope(Option<CancellationToken>);
impl ReadScope {
    fn enter(stop: CancellationToken) -> Self {
        Self(READ_SCOPE.with(|scope| scope.replace(Some(stop))))
    }
}
impl Drop for ReadScope {
    fn drop(&mut self) {
        READ_SCOPE.with(|scope| scope.replace(self.0.take()));
    }
}
pub(super) fn in_read_scope() -> bool {
    READ_SCOPE.with(|scope| scope.borrow().is_some())
}
pub(super) fn check() -> Result<()> {
    if READ_SCOPE.with(|scope| {
        scope
            .borrow()
            .as_ref()
            .is_some_and(CancellationToken::is_cancelled)
    }) {
        Err(abandoned())
    } else {
        Ok(())
    }
}
fn abandoned() -> StoreError {
    StoreError::ValidationBusy
}

struct ProgressGuard<'a>(&'a Connection);
impl Drop for ProgressGuard<'_> {
    fn drop(&mut self) {
        self.0
            .progress_handler(0, None::<fn() -> bool>)
            .unwrap_or_else(|error| eprintln!("SQLite validation hook cleanup failed: {error}"));
    }
}

async fn read_job<T, U, F, G>(
    owner: Arc<StoreInner>,
    stop: CancellationToken,
    job: Arc<OwnedSemaphorePermit>,
    operation: F,
    publish: G,
) -> Result<U>
where
    T: Send + 'static,
    U: Send + 'static,
    F: FnOnce(&Transaction<'_>) -> Result<T> + Send + 'static,
    G: FnOnce(T) -> Result<U> + Send + 'static,
{
    let queued = std::time::Instant::now();
    let permit = tokio::select! { biased;
        () = stop.cancelled() => return Err(abandoned()),
        permit = owner.validation_admission.clone().acquire_owned() =>
            permit.map_err(|_| StoreError::Io("SQLite validation admission closed".into()))?,
    };
    #[cfg(feature = "test-support")]
    let dispatched = std::time::Instant::now();
    tokio::task::spawn_blocking(move || {
        #[cfg(feature = "test-support")]
        let _measurement = test_support::LaneProbe::start(
            Some(owner.validation_measurements.clone()),
            queued,
            dispatched,
            std::any::type_name::<U>(),
        );
        let _job = job;
        let _permit = permit;
        let _scope = ReadScope::enter(stop.clone());
        check()?;
        owner.validation_queue_ns.fetch_add(
            u64::try_from(queued.elapsed().as_nanos()).unwrap_or(u64::MAX),
            Ordering::Relaxed,
        );
        let mut connection = owner.connections.validation_reader.lock().map_err(|_| {
            StoreError::Io("SQLite validation connection mutex was poisoned".into())
        })?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Deferred)
            .map_err(sql_error)?;
        #[cfg(any(test, feature = "test-support"))]
        let steps = owner.validation_vm_steps.clone();
        transaction
            .progress_handler(
                1_000,
                Some(move || {
                    #[cfg(any(test, feature = "test-support"))]
                    steps.fetch_add(1_000, Ordering::Relaxed);
                    // rusqlite returns INTERRUPT for true, unlike the SQLx reference.
                    stop.is_cancelled()
                }),
            )
            .map_err(sql_error)?;
        // Declared after the transaction: unwinding removes the hook before rollback.
        let progress = ProgressGuard(&transaction);
        let started = std::time::Instant::now();
        let result = operation(&transaction);
        owner.validation_work_ns.fetch_add(
            u64::try_from(started.elapsed().as_nanos()).unwrap_or(u64::MAX),
            Ordering::Relaxed,
        );
        drop(progress);
        let result = result?;
        transaction.commit().map_err(sql_error)?;
        // A completed, committed read is valid even if its final waiter just left.
        publish(result)
    })
    .await
    .map_err(|error| StoreError::Io(format!("SQLite worker failed: {error}")))?
}

impl SqliteStore {
    #[cfg(test)]
    pub(super) async fn with_validation<T, F>(&self, operation: F) -> Result<T>
    where
        T: Send + 'static,
        F: FnOnce(&Transaction<'_>) -> Result<T> + Send + 'static,
    {
        self.with_validation_publish(operation, Ok).await
    }

    pub(super) async fn with_validation_publish<T, U, F, G>(
        &self,
        operation: F,
        publish: G,
    ) -> Result<U>
    where
        T: Send + 'static,
        U: Send + 'static,
        F: FnOnce(&Transaction<'_>) -> Result<T> + Send + 'static,
        G: FnOnce(T) -> Result<U> + Send + 'static,
    {
        let _caller = self.inner.cold_validation.caller()?;
        let job = Arc::new(self.inner.cold_validation.job()?);
        let stop = StopOnDrop(CancellationToken::new());
        read_job(self.inner.clone(), stop.0.clone(), job, operation, publish).await
    }

    pub(super) async fn session_proof(&self, id: &SessionId) -> Result<Proof> {
        if let Some(proof) = self.inner.session_proof(id) {
            self.inner.cold_validation.hit();
            return Ok(proof);
        }
        let caller = self.inner.cold_validation.caller()?;
        let (flight, job) = {
            let mut flights = self
                .inner
                .cold_validation
                .flights
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if let Some(proof) = self.inner.session_proof(id) {
                self.inner.cold_validation.hit();
                return Ok(proof);
            }
            if let Some(flight) = flights.get(id) {
                self.inner.cold_validation.shared();
                #[cfg(test)]
                {
                    let mut state = flight
                        .state
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner);
                    state.waiters += 1;
                }
                (flight.clone(), None)
            } else {
                let job = Arc::new(self.inner.cold_validation.job()?);
                let flight = Arc::new(Flight {
                    #[cfg(test)]
                    state: Mutex::new(FlightState { waiters: 1 }),
                    result: watch::channel(None).0,
                });
                flights.insert(id.clone(), flight.clone());
                (flight, Some(job))
            }
        };
        let waiter = Waiter {
            #[cfg(test)]
            flight: flight.clone(),
            _caller: caller,
        };
        if let Some(job) = job {
            let owner = self.inner.clone();
            let selected = id.clone();
            let flight = flight.clone();
            // Move completion into the task before its first poll. Blocking closures
            // retain it too, so abort/unwind cannot retire an actual running job.
            let completion = Arc::new(FlightCompletion {
                owner: owner.clone(),
                selected: selected.clone(),
                flight,
                _job: job.clone(),
                result: Mutex::new(None),
            });
            tokio::spawn(async move {
                let reading = owner.clone();
                let publishing = owner.clone();
                let candidate = selected.clone();
                let operation_owner = completion.clone();
                let publication_owner = completion.clone();
                let result = read_job(
                    owner.clone(),
                    CancellationToken::new(),
                    job.clone(),
                    move |transaction| {
                        let _completion = operation_owner;
                        if reading.session_proof(&candidate).is_none() {
                            reading.validate_selected(transaction, &candidate)?;
                        }
                        Ok(candidate)
                    },
                    move |candidate| {
                        let _completion = publication_owner;
                        let mut cache = publishing.validated_sessions.lock().map_err(|_| {
                            StoreError::Io("SQLite Session proof cache mutex was poisoned".into())
                        })?;
                        Ok(cache.insert(candidate))
                    },
                )
                .await;
                *completion
                    .result
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(result);
            });
        }
        let mut result = flight.result.subscribe();
        loop {
            let completed = result.borrow_and_update().clone();
            if let Some(completed) = completed {
                drop(waiter);
                return completed;
            }
            result
                .changed()
                .await
                .map_err(|_| StoreError::Io("SQLite validation worker stopped".into()))?;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::future::Future;
    use std::task::Poll;

    #[tokio::test]
    async fn failed_flight_owner_resolves_waiters_and_allows_a_new_proof() {
        let root = tempfile::tempdir().unwrap();
        let store = SqliteStore::open(root.path()).unwrap();
        let id = super::super::tests::seed_session(&store, "failed-flight").await;
        drop(store);
        let store = SqliteStore::open(root.path()).unwrap();
        let flight = Arc::new(Flight {
            state: Mutex::new(FlightState { waiters: 0 }),
            result: watch::channel(None).0,
        });
        store
            .inner
            .cold_validation
            .flights
            .lock()
            .unwrap()
            .insert(id.clone(), flight.clone());
        let completion = Arc::new(FlightCompletion {
            owner: store.inner.clone(),
            selected: id.clone(),
            flight: flight.clone(),
            _job: Arc::new(store.inner.cold_validation.job().unwrap()),
            result: Mutex::new(None),
        });
        let mut waiter = Box::pin(store.session_proof(&id));
        std::future::poll_fn(|cx| {
            assert!(waiter.as_mut().poll(cx).is_pending());
            Poll::Ready(())
        })
        .await;
        let task = tokio::spawn(async move {
            let _completion = completion;
            panic!("injected validation owner failure");
        });
        assert!(task.await.unwrap_err().is_panic());
        assert!(matches!(
            tokio::time::timeout(Duration::from_secs(2), waiter)
                .await
                .unwrap(),
            Err(StoreError::Io(_))
        ));
        assert!(
            store
                .inner
                .cold_validation
                .flights
                .lock()
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            store.inner.cold_validation.jobs.available_permits(),
            CAPACITY
        );
        store.session_proof(&id).await.unwrap();
    }

    #[tokio::test]
    async fn poisoned_proof_cache_refuses_publication_and_releases_flight() {
        let root = tempfile::tempdir().unwrap();
        let store = SqliteStore::open(root.path()).unwrap();
        let id = super::super::tests::seed_session(&store, "poisoned-proof").await;
        drop(store);
        let store = SqliteStore::open(root.path()).unwrap();
        let owner = store.inner.clone();
        assert!(
            std::thread::spawn(move || {
                let _cache = owner.validated_sessions.lock().unwrap();
                panic!("injected cache failure");
            })
            .join()
            .is_err()
        );
        assert!(matches!(
            store.session_proof(&id).await,
            Err(StoreError::Io(_))
        ));
        assert!(
            store
                .inner
                .cold_validation
                .flights
                .lock()
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            store.inner.cold_validation.jobs.available_permits(),
            CAPACITY
        );
        assert!(matches!(
            store.session_proof(&id).await,
            Err(StoreError::Io(_))
        ));
    }

    #[tokio::test]
    async fn cancelled_private_queued_read_never_dispatches() {
        let root = tempfile::tempdir().unwrap();
        let store = SqliteStore::open(root.path()).unwrap();
        let lane = store
            .inner
            .validation_admission
            .clone()
            .acquire_owned()
            .await
            .unwrap();
        let ran = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let observed = ran.clone();
        let mut read = Box::pin(store.with_validation(move |_| {
            observed.store(true, Ordering::Relaxed);
            Ok(())
        }));
        std::future::poll_fn(|cx| {
            assert!(read.as_mut().poll(cx).is_pending());
            Poll::Ready(())
        })
        .await;
        drop(read);
        drop(lane);
        assert!(!ran.load(Ordering::Relaxed));
        assert_eq!(
            store.inner.cold_validation.jobs.available_permits(),
            CAPACITY
        );
    }

    #[tokio::test]
    async fn proof_scan_survives_all_waiter_deadlines_and_publishes_once() {
        let root = tempfile::tempdir().unwrap();
        let store = SqliteStore::open(root.path()).unwrap();
        let id = super::super::tests::seed_session(&store, "proof-deadlines").await;
        drop(store);
        let store = SqliteStore::open(root.path()).unwrap();
        let (entered_tx, entered) = tokio::sync::oneshot::channel();
        let (release, gate) = std::sync::mpsc::channel();
        *store.inner.validation_barrier.lock().unwrap() = Some((entered_tx, gate));
        let owner = store.clone();
        let selected = id.clone();
        let first = tokio::spawn(async move { owner.session_proof(&selected).await });
        entered.await.unwrap();
        first.abort();
        assert!(first.await.unwrap_err().is_cancelled());
        for _ in 0..3 {
            let mut next = Box::pin(store.session_proof(&id));
            std::future::poll_fn(|cx| {
                assert!(
                    next.as_mut().poll(cx).is_pending(),
                    "next caller must join the ongoing scan"
                );
                Poll::Ready(())
            })
            .await;
            drop(next);
        }
        assert_eq!(
            store.inner.cold_validation.jobs.available_permits(),
            CAPACITY - 1
        );
        release.send(()).unwrap();
        let drained = tokio::time::timeout(
            Duration::from_secs(2),
            store
                .inner
                .cold_validation
                .jobs
                .clone()
                .acquire_many_owned(u32::try_from(CAPACITY).unwrap()),
        )
        .await
        .unwrap()
        .unwrap();
        assert!(store.inner.session_proof(&id).is_some());
        assert_eq!(store.inner.validation_runs.load(Ordering::Relaxed), 1);
        drop(drained);
        store.session_proof(&id).await.unwrap();
        assert_eq!(store.inner.validation_runs.load(Ordering::Relaxed), 1);
    }

    #[tokio::test]
    async fn saturated_jobs_allow_joining_a_flight_but_refuse_new_work() {
        let root = tempfile::tempdir().unwrap();
        let store = SqliteStore::open(root.path()).unwrap();
        let id = super::super::tests::seed_session(&store, "shared-at-capacity").await;
        drop(store);
        let store = SqliteStore::open(root.path()).unwrap();
        let lane = store
            .inner
            .validation_admission
            .clone()
            .acquire_owned()
            .await
            .unwrap();
        let mut first = std::pin::pin!(store.session_proof(&id));
        std::future::poll_fn(|cx| {
            assert!(first.as_mut().poll(cx).is_pending());
            Poll::Ready(())
        })
        .await;
        // The lane barrier keeps the first flight incomplete while unrelated
        // ownership fills every remaining job slot without consuming callers.
        let jobs = store
            .inner
            .cold_validation
            .jobs
            .clone()
            .try_acquire_many_owned(u32::try_from(CAPACITY - 1).unwrap())
            .unwrap();
        assert_eq!(store.inner.cold_validation.jobs.available_permits(), 0);
        let mut second = std::pin::pin!(store.session_proof(&id));
        std::future::poll_fn(|cx| {
            assert!(second.as_mut().poll(cx).is_pending());
            Poll::Ready(())
        })
        .await;
        assert_eq!(store.cold_validation_metrics().shared_flights, 1);
        let fresh = SessionId::new("new-flight-at-capacity").unwrap();
        assert!(matches!(
            store.session_proof(&fresh).await,
            Err(StoreError::ValidationBusy)
        ));
        assert_eq!(store.cold_validation_metrics().admission_refusals, 1);
        assert_eq!(
            store.inner.cold_validation.callers.available_permits(),
            CAPACITY - 2
        );
        drop(jobs);
        drop(lane);
        let (first, second) = tokio::time::timeout(Duration::from_secs(2), async {
            tokio::join!(first, second)
        })
        .await
        .unwrap();
        assert!(Arc::ptr_eq(&first.unwrap(), &second.unwrap()));
        assert_eq!(store.inner.validation_runs.load(Ordering::Relaxed), 1);
        assert_eq!(
            store.inner.cold_validation.callers.available_permits(),
            CAPACITY
        );
        let drained = tokio::time::timeout(
            Duration::from_secs(2),
            store
                .inner
                .cold_validation
                .jobs
                .clone()
                .acquire_many_owned(u32::try_from(CAPACITY).unwrap()),
        )
        .await
        .unwrap()
        .unwrap();
        drop(drained);
    }

    #[tokio::test]
    async fn abandoned_sql_is_interrupted_and_hook_does_not_poison_reuse() {
        let root = tempfile::tempdir().unwrap();
        let store = SqliteStore::open(root.path()).unwrap();
        let worker = store.clone();
        let task = tokio::spawn(async move {
            worker.with_validation(|tx| {
                tx.query_row("WITH RECURSIVE n(x) AS (VALUES(1) UNION ALL SELECT x+1 FROM n WHERE x<100000000) SELECT sum(x) FROM n", [], |row| row.get::<_, i64>(0)).map_err(sql_error)
            }).await
        });
        tokio::time::timeout(Duration::from_secs(2), async {
            while store.inner.validation_vm_steps.load(Ordering::Relaxed) < 10_000 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        task.abort();
        assert!(task.await.unwrap_err().is_cancelled());
        let drained = tokio::time::timeout(
            Duration::from_secs(2),
            store
                .inner
                .cold_validation
                .jobs
                .clone()
                .acquire_many_owned(u32::try_from(CAPACITY).unwrap()),
        )
        .await
        .unwrap()
        .unwrap();
        assert!(store.inner.validation_vm_steps.load(Ordering::Relaxed) < 10_000_000);
        drop(drained);
        assert_eq!(
            store
                .with_validation(|tx| tx
                    .query_row("SELECT 42", [], |row| row.get::<_, i64>(0))
                    .map_err(sql_error))
                .await
                .unwrap(),
            42
        );
    }

    #[tokio::test]
    async fn completed_read_can_publish_after_its_waiter_leaves() {
        let root = tempfile::tempdir().unwrap();
        let store = SqliteStore::open(root.path()).unwrap();
        let (entered_tx, entered) = tokio::sync::oneshot::channel();
        let (release, gate) = std::sync::mpsc::channel();
        let published = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let output = published.clone();
        let worker = store.clone();
        let task = tokio::spawn(async move {
            worker
                .with_validation_publish(
                    |tx| {
                        tx.query_row("SELECT 1", [], |row| row.get::<_, i64>(0))
                            .map_err(sql_error)
                    },
                    move |value| {
                        entered_tx.send(()).unwrap();
                        gate.recv().unwrap();
                        output.store(true, Ordering::Release);
                        Ok(value)
                    },
                )
                .await
        });
        entered.await.unwrap();
        task.abort();
        assert!(task.await.unwrap_err().is_cancelled());
        assert_eq!(
            store.inner.cold_validation.jobs.available_permits(),
            CAPACITY - 1
        );
        release.send(()).unwrap();
        let drained = tokio::time::timeout(
            Duration::from_secs(2),
            store
                .inner
                .cold_validation
                .jobs
                .clone()
                .acquire_many_owned(u32::try_from(CAPACITY).unwrap()),
        )
        .await
        .unwrap()
        .unwrap();
        assert!(published.load(Ordering::Acquire));
        drop(drained);
    }

    #[tokio::test]
    async fn cold_queue_refuses_capacity_before_dispatch_and_releases_cancelled_jobs() {
        let root = tempfile::tempdir().unwrap();
        let store = SqliteStore::open(root.path()).unwrap();
        let lane = store
            .inner
            .validation_admission
            .clone()
            .acquire_owned()
            .await
            .unwrap();
        let mut tasks = Vec::new();
        for _ in 0..CAPACITY {
            let worker = store.clone();
            tasks.push(tokio::spawn(async move {
                worker.with_validation(|_| Ok(())).await
            }));
        }
        tokio::time::timeout(Duration::from_secs(2), async {
            while store.inner.cold_validation.jobs.available_permits() != 0 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        assert!(matches!(
            store
                .with_validation(|_| -> Result<()> { panic!("refused job dispatched") })
                .await,
            Err(StoreError::ValidationBusy)
        ));
        for task in tasks {
            task.abort();
            assert!(task.await.unwrap_err().is_cancelled());
        }
        drop(lane);
        let drained = tokio::time::timeout(
            Duration::from_secs(2),
            store
                .inner
                .cold_validation
                .jobs
                .clone()
                .acquire_many_owned(u32::try_from(CAPACITY).unwrap()),
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(
            store.inner.cold_validation.callers.available_permits(),
            CAPACITY
        );
        drop(drained);
    }
    #[test]
    fn cancelled_read_scope_refuses_instead_of_reporting_io() {
        let stop = CancellationToken::new();
        stop.cancel();
        let scope = ReadScope::enter(stop);
        assert!(matches!(check(), Err(StoreError::ValidationBusy)));
        drop(scope);
        assert!(check().is_ok());
    }

    #[test]
    fn sqlite_interruption_is_a_refusal_only_for_owned_read_validation() {
        let interrupted = || {
            rusqlite::Error::SqliteFailure(
                rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_INTERRUPT),
                None,
            )
        };
        assert!(matches!(sql_error(interrupted()), StoreError::Io(_)));
        let scope = ReadScope::enter(CancellationToken::new());
        assert!(matches!(
            sql_error(interrupted()),
            StoreError::ValidationBusy
        ));
        drop(scope);
        assert!(matches!(sql_error(interrupted()), StoreError::Io(_)));
    }
}

/// Cumulative observations of this Store's bounded cold-validation lane.
#[derive(Clone, Copy, Debug, serde::Serialize)]
pub struct ColdValidationMetrics {
    /// Calls satisfied by an existing validated Session proof.
    pub cache_hits: u64,
    /// Callers joining an already-owned proof flight.
    pub shared_flights: u64,
    /// Refused cold caller or job admissions.
    pub admission_refusals: u64,
    /// Total nanoseconds queued and dispatched before validation starts.
    pub queue_dispatch_ns: u64,
    /// Total nanoseconds executing validation operations.
    pub work_ns: u64,
}
impl SqliteStore {
    /// Returns cumulative observations without retaining identities or individual samples.
    pub fn cold_validation_metrics(&self) -> ColdValidationMetrics {
        let cold = &self.inner.cold_validation;
        ColdValidationMetrics {
            cache_hits: cold.cache_hits.load(Ordering::Relaxed),
            shared_flights: cold.shared_flights.load(Ordering::Relaxed),
            admission_refusals: cold.refusals.load(Ordering::Relaxed),
            queue_dispatch_ns: self.inner.validation_queue_ns.load(Ordering::Relaxed),
            work_ns: self.inner.validation_work_ns.load(Ordering::Relaxed),
        }
    }
}
