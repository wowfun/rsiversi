use super::LoaderError;
use std::sync::atomic::{AtomicU8, AtomicU64, AtomicUsize, Ordering};
use std::sync::mpsc::{Receiver, SyncSender, TrySendError};
use std::sync::{Arc, Mutex};
use tokio::sync::{Notify, OwnedSemaphorePermit, Semaphore};

pub(super) const MAX_NATIVE_CALLBACK_THREADS: usize = 256;
pub(super) const MAX_NATIVE_DESTRUCTION_THREADS: usize = 64;
pub(super) const MAX_LIVE_NATIVE_INSTANCES: usize = 65_536;

type DestructionCallback = Box<dyn FnOnce() + Send + 'static>;

struct DestructionJob {
    callback: DestructionCallback,
    queue_permit: Option<OwnedSemaphorePermit>,
}

pub(super) struct DestructionReservation {
    _permit: OwnedSemaphorePermit,
}

pub(super) struct InstanceReservation {
    _permit: OwnedSemaphorePermit,
    stats: Arc<ExecutorStats>,
}

impl Drop for InstanceReservation {
    fn drop(&mut self) {
        self.stats.active_instances.fetch_sub(1, Ordering::Relaxed);
    }
}

#[derive(Default)]
struct ExecutorStats {
    active_callbacks: AtomicUsize,
    peak_callbacks: AtomicUsize,
    rejected_callbacks: AtomicU64,
    active_instances: AtomicUsize,
    peak_instances: AtomicUsize,
    rejected_instances: AtomicU64,
    pending_instance_destructions: AtomicUsize,
    active_destructions: AtomicUsize,
    peak_destructions: AtomicUsize,
    rejected_destructions: AtomicU64,
    queued_destructions: AtomicUsize,
}

#[derive(Clone, Copy, Debug, Default)]
pub(super) struct ExecutorSnapshot {
    pub(super) active_callbacks: usize,
    pub(super) peak_callbacks: usize,
    pub(super) rejected_callbacks: u64,
    pub(super) active_instances: usize,
    pub(super) peak_instances: usize,
    pub(super) rejected_instances: u64,
    pub(super) pending_instance_destructions: usize,
    pub(super) active_destructions: usize,
    pub(super) peak_destructions: usize,
    pub(super) rejected_destructions: u64,
    pub(super) queued_destructions: usize,
}

#[derive(Clone)]
pub(super) struct NativeExecutor {
    callback_permits: Arc<Semaphore>,
    destruction_sender: SyncSender<DestructionJob>,
    destruction_queue_permits: Arc<Semaphore>,
    factory_destruction_permits: Arc<Semaphore>,
    instance_permits: Arc<Semaphore>,
    stats: Arc<ExecutorStats>,
}

impl NativeExecutor {
    pub(super) fn new(
        maximum_concurrent_callbacks: usize,
        maximum_concurrent_destructions: usize,
        maximum_live_factories: usize,
        maximum_live_instances: usize,
    ) -> Result<Self, LoaderError> {
        debug_assert!((1..=MAX_NATIVE_CALLBACK_THREADS).contains(&maximum_concurrent_callbacks));
        debug_assert!(
            (1..=MAX_NATIVE_DESTRUCTION_THREADS).contains(&maximum_concurrent_destructions)
        );
        debug_assert!(maximum_live_factories != 0);
        debug_assert!((1..=MAX_LIVE_NATIVE_INSTANCES).contains(&maximum_live_instances));

        let stats = Arc::new(ExecutorStats::default());
        let destruction_queue_capacity = maximum_concurrent_destructions
            .checked_add(maximum_live_factories)
            .and_then(|capacity| capacity.checked_add(maximum_live_instances))
            .ok_or_else(|| {
                LoaderError::InvalidInput("native destruction capacity overflow".to_owned())
            })?;
        let (destruction_sender, destruction_receiver) =
            std::sync::mpsc::sync_channel(destruction_queue_capacity);
        let destruction_receiver = Arc::new(Mutex::new(destruction_receiver));
        for index in 0..maximum_concurrent_destructions {
            let receiver = Arc::clone(&destruction_receiver);
            let worker_stats = Arc::clone(&stats);
            std::thread::Builder::new()
                .name(format!("rsi-meta-native-destroy-{index}"))
                .spawn(move || destruction_worker(&receiver, &worker_stats))?;
        }

        Ok(Self {
            callback_permits: Arc::new(Semaphore::new(maximum_concurrent_callbacks)),
            destruction_sender,
            destruction_queue_permits: Arc::new(Semaphore::new(maximum_concurrent_destructions)),
            factory_destruction_permits: Arc::new(Semaphore::new(maximum_live_factories)),
            instance_permits: Arc::new(Semaphore::new(maximum_live_instances)),
            stats,
        })
    }

    pub(super) fn spawn_callback<T>(
        &self,
        name: &'static str,
        callback: impl FnOnce() -> T + Send + 'static,
    ) -> Result<tokio::sync::oneshot::Receiver<T>, LoaderError>
    where
        T: Send + 'static,
    {
        let permit = self.callback_permit(name)?;
        let activity = Activity::begin_callback(Arc::clone(&self.stats));
        let (sender, receiver) = tokio::sync::oneshot::channel();
        std::thread::Builder::new()
            .name(format!("rsi-meta-native-{name}"))
            .spawn(move || {
                // Reverse local drop order decrements activity before making
                // admission reusable, after send and rejected-result drop.
                let _permit = permit;
                let _activity = activity;
                let result = callback();
                let _ = sender.send(result);
            })?;
        Ok(receiver)
    }

    pub(super) fn spawn_blocking_callback<T>(
        &self,
        name: &'static str,
        callback: impl FnOnce() -> T + Send + 'static,
    ) -> Result<Receiver<T>, LoaderError>
    where
        T: Send + 'static,
    {
        let permit = self.callback_permit(name)?;
        let activity = Activity::begin_callback(Arc::clone(&self.stats));
        let (sender, receiver) = std::sync::mpsc::sync_channel(1);
        std::thread::Builder::new()
            .name(format!("rsi-meta-native-{name}"))
            .spawn(move || {
                // Reverse local drop order decrements activity before making
                // admission reusable, after send and rejected-result drop.
                let _permit = permit;
                let _activity = activity;
                let result = callback();
                let _ = sender.send(result);
            })?;
        Ok(receiver)
    }

    fn callback_permit(
        &self,
        operation: &'static str,
    ) -> Result<OwnedSemaphorePermit, LoaderError> {
        Arc::clone(&self.callback_permits)
            .try_acquire_owned()
            .map_err(|_| {
                self.stats
                    .rejected_callbacks
                    .fetch_add(1, Ordering::Relaxed);
                LoaderError::Busy { operation }
            })
    }

    pub(super) async fn spawn_destruction<T>(
        &self,
        callback: impl FnOnce() -> T + Send + 'static,
    ) -> Result<tokio::sync::oneshot::Receiver<T>, LoaderError>
    where
        T: Send + 'static,
    {
        let pending = PendingInstanceDestruction::begin(Arc::clone(&self.stats));
        let queue_permit = Arc::clone(&self.destruction_queue_permits)
            .acquire_owned()
            .await
            .map_err(|_| LoaderError::Callback {
                operation: "destruction",
                message: "native destruction executor is closed".to_owned(),
            })?;
        let (sender, receiver) = tokio::sync::oneshot::channel();
        let job = DestructionJob {
            callback: Box::new(move || {
                let _pending = pending;
                let _ = sender.send(callback());
            }),
            queue_permit: Some(queue_permit),
        };
        self.stats
            .queued_destructions
            .fetch_add(1, Ordering::Relaxed);
        self.destruction_sender.try_send(job).map_err(|error| {
            self.stats
                .queued_destructions
                .fetch_sub(1, Ordering::Relaxed);
            LoaderError::Callback {
                operation: "destruction",
                message: match error {
                    TrySendError::Full(_) => "native destruction queue invariant failed",
                    TrySendError::Disconnected(_) => "native destruction executor disconnected",
                }
                .to_owned(),
            }
        })?;
        Ok(receiver)
    }

    #[cfg(test)]
    pub(super) fn try_submit_destruction(&self, callback: impl FnOnce() + Send + 'static) -> bool {
        let Ok(queue_permit) = Arc::clone(&self.destruction_queue_permits).try_acquire_owned()
        else {
            self.stats
                .rejected_destructions
                .fetch_add(1, Ordering::Relaxed);
            return false;
        };
        let job = DestructionJob {
            callback: Box::new(callback),
            queue_permit: Some(queue_permit),
        };
        self.stats
            .queued_destructions
            .fetch_add(1, Ordering::Relaxed);
        if self.destruction_sender.try_send(job).is_err() {
            self.stats
                .queued_destructions
                .fetch_sub(1, Ordering::Relaxed);
            self.stats
                .rejected_destructions
                .fetch_add(1, Ordering::Relaxed);
            return false;
        }
        true
    }

    pub(super) fn reserve_factory_destruction(
        &self,
    ) -> Result<DestructionReservation, LoaderError> {
        Arc::clone(&self.factory_destruction_permits)
            .try_acquire_owned()
            .map(|permit| DestructionReservation { _permit: permit })
            .map_err(|_| {
                self.stats
                    .rejected_destructions
                    .fetch_add(1, Ordering::Relaxed);
                LoaderError::Busy { operation: "load" }
            })
    }

    pub(super) fn reserve_instance(&self) -> Result<InstanceReservation, LoaderError> {
        let permit = Arc::clone(&self.instance_permits)
            .try_acquire_owned()
            .map_err(|_| {
                self.stats
                    .rejected_instances
                    .fetch_add(1, Ordering::Relaxed);
                LoaderError::Busy {
                    operation: "create",
                }
            })?;
        let active = self.stats.active_instances.fetch_add(1, Ordering::Relaxed) + 1;
        self.stats
            .peak_instances
            .fetch_max(active, Ordering::Relaxed);
        Ok(InstanceReservation {
            _permit: permit,
            stats: Arc::clone(&self.stats),
        })
    }

    pub(super) fn submit_reserved_destruction(
        &self,
        permit: DestructionReservation,
        callback: impl FnOnce(DestructionReservation) + Send + 'static,
    ) {
        self.submit_reserved(permit, callback);
    }

    pub(super) fn submit_reserved_instance_destruction(
        &self,
        reservation: InstanceReservation,
        callback: impl FnOnce(InstanceReservation) + Send + 'static,
    ) {
        let pending = PendingInstanceDestruction::begin(Arc::clone(&self.stats));
        self.submit_reserved(reservation, move |reservation| {
            let _pending = pending;
            callback(reservation);
        });
    }

    fn submit_reserved<R>(&self, reservation: R, callback: impl FnOnce(R) + Send + 'static)
    where
        R: Send + 'static,
    {
        let job = DestructionJob {
            callback: Box::new(move || callback(reservation)),
            queue_permit: None,
        };
        self.stats
            .queued_destructions
            .fetch_add(1, Ordering::Relaxed);
        match self.destruction_sender.try_send(job) {
            Ok(()) => {}
            Err(TrySendError::Full(_)) => {
                unreachable!("reserved teardown admission always owns one queue slot");
            }
            Err(TrySendError::Disconnected(_)) => {
                unreachable!("a live native executor always retains a destruction receiver");
            }
        }
    }

    pub(super) fn snapshot(&self) -> ExecutorSnapshot {
        ExecutorSnapshot {
            active_callbacks: self.stats.active_callbacks.load(Ordering::Relaxed),
            peak_callbacks: self.stats.peak_callbacks.load(Ordering::Relaxed),
            rejected_callbacks: self.stats.rejected_callbacks.load(Ordering::Relaxed),
            active_instances: self.stats.active_instances.load(Ordering::Relaxed),
            peak_instances: self.stats.peak_instances.load(Ordering::Relaxed),
            rejected_instances: self.stats.rejected_instances.load(Ordering::Relaxed),
            pending_instance_destructions: self
                .stats
                .pending_instance_destructions
                .load(Ordering::Relaxed),
            active_destructions: self.stats.active_destructions.load(Ordering::Relaxed),
            peak_destructions: self.stats.peak_destructions.load(Ordering::Relaxed),
            rejected_destructions: self.stats.rejected_destructions.load(Ordering::Relaxed),
            queued_destructions: self.stats.queued_destructions.load(Ordering::Relaxed),
        }
    }
}

struct PendingInstanceDestruction {
    stats: Arc<ExecutorStats>,
}

impl PendingInstanceDestruction {
    fn begin(stats: Arc<ExecutorStats>) -> Self {
        stats
            .pending_instance_destructions
            .fetch_add(1, Ordering::Relaxed);
        Self { stats }
    }
}

impl Drop for PendingInstanceDestruction {
    fn drop(&mut self) {
        self.stats
            .pending_instance_destructions
            .fetch_sub(1, Ordering::Relaxed);
    }
}

fn destruction_worker(receiver: &Mutex<Receiver<DestructionJob>>, stats: &Arc<ExecutorStats>) {
    loop {
        let job = receiver
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .recv();
        let Ok(job) = job else {
            return;
        };
        stats.queued_destructions.fetch_sub(1, Ordering::Relaxed);
        let DestructionJob {
            callback,
            queue_permit,
        } = job;
        drop(queue_permit);
        let _activity = Activity::begin_destruction(Arc::clone(stats));
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(callback));
    }
}

enum ActivityKind {
    Callback,
    Destruction,
}

struct Activity {
    stats: Arc<ExecutorStats>,
    kind: ActivityKind,
}

impl Activity {
    fn begin_callback(stats: Arc<ExecutorStats>) -> Self {
        let active = stats.active_callbacks.fetch_add(1, Ordering::Relaxed) + 1;
        stats.peak_callbacks.fetch_max(active, Ordering::Relaxed);
        Self {
            stats,
            kind: ActivityKind::Callback,
        }
    }

    fn begin_destruction(stats: Arc<ExecutorStats>) -> Self {
        let active = stats.active_destructions.fetch_add(1, Ordering::Relaxed) + 1;
        stats.peak_destructions.fetch_max(active, Ordering::Relaxed);
        Self {
            stats,
            kind: ActivityKind::Destruction,
        }
    }
}

impl Drop for Activity {
    fn drop(&mut self) {
        match self.kind {
            ActivityKind::Callback => {
                self.stats.active_callbacks.fetch_sub(1, Ordering::Relaxed);
            }
            ActivityKind::Destruction => {
                self.stats
                    .active_destructions
                    .fetch_sub(1, Ordering::Relaxed);
            }
        }
    }
}

pub(super) enum CallbackWaitError {
    TimedOut,
    Disconnected(tokio::sync::oneshot::error::RecvError),
}

pub(super) async fn run_bounded_callback<T>(
    mut receiver: tokio::sync::oneshot::Receiver<T>,
    completion: Arc<CallbackCompletion>,
    deadline: tokio::time::Instant,
    on_timeout: Arc<dyn Fn() + Send + Sync>,
) -> Result<T, CallbackWaitError> {
    let watchdog_completion = Arc::clone(&completion);
    let watchdog_timeout = Arc::clone(&on_timeout);
    tokio::spawn(async move {
        tokio::select! {
            biased;
            () = watchdog_completion.wait_finished() => {}
            () = tokio::time::sleep_until(deadline) => {
                watchdog_completion.time_out(watchdog_timeout.as_ref());
            }
        }
    });
    let result = if let Ok(result) = tokio::time::timeout_at(deadline, &mut receiver).await {
        result
    } else {
        completion.time_out(on_timeout.as_ref());
        if completion.is_timed_out() {
            return Err(CallbackWaitError::TimedOut);
        }
        receiver.await
    };
    if completion.is_timed_out() {
        Err(CallbackWaitError::TimedOut)
    } else {
        result.map_err(CallbackWaitError::Disconnected)
    }
}

/// Marks a watchdog operation complete before its adapter-owned callback gate
/// is released, closing the interval in which a successful callback could be
/// mistaken for an in-flight one.
pub(super) struct CallbackCompletion {
    state: AtomicU8,
    transition: Mutex<()>,
    finished: Notify,
}

impl CallbackCompletion {
    const RUNNING: u8 = 0;
    const COMPLETED: u8 = 1;
    const TIMING_OUT: u8 = 2;
    const TIMED_OUT: u8 = 3;

    pub(super) fn new() -> Self {
        Self {
            state: AtomicU8::new(Self::RUNNING),
            transition: Mutex::new(()),
            finished: Notify::new(),
        }
    }

    pub(super) fn complete(&self) -> bool {
        let _transition = self
            .transition
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let completed = self
            .state
            .compare_exchange(
                Self::RUNNING,
                Self::COMPLETED,
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .is_ok();
        if completed {
            self.finished.notify_waiters();
        }
        completed
    }

    pub(super) fn time_out(&self, on_timeout: &(dyn Fn() + Send + Sync)) -> bool {
        let _transition = self
            .transition
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let claimed = self
            .state
            .compare_exchange(
                Self::RUNNING,
                Self::TIMING_OUT,
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .is_ok();
        if !claimed {
            return false;
        }
        let publication = TimeoutPublication(self);
        on_timeout();
        drop(publication);
        true
    }

    pub(super) fn is_timed_out(&self) -> bool {
        self.state.load(Ordering::Acquire) == Self::TIMED_OUT
    }

    async fn wait_finished(&self) {
        loop {
            let notified = self.finished.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            if matches!(
                self.state.load(Ordering::Acquire),
                Self::COMPLETED | Self::TIMED_OUT
            ) {
                return;
            }
            notified.as_mut().await;
        }
    }
}

struct TimeoutPublication<'a>(&'a CallbackCompletion);

impl Drop for TimeoutPublication<'_> {
    fn drop(&mut self) {
        self.0
            .state
            .store(CallbackCompletion::TIMED_OUT, Ordering::Release);
        self.0.finished.notify_waiters();
    }
}

pub(super) struct CompletionOnDrop<'a>(pub(super) &'a CallbackCompletion);

impl Drop for CompletionOnDrop<'_> {
    fn drop(&mut self) {
        self.0.complete();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    struct DropSignal(Arc<Notify>);

    impl Drop for DropSignal {
        fn drop(&mut self) {
            self.0.notify_one();
        }
    }

    struct BlockingDrop {
        entered: std::sync::mpsc::SyncSender<()>,
        release: std::sync::mpsc::Receiver<()>,
    }

    impl Drop for BlockingDrop {
        fn drop(&mut self) {
            self.entered.send(()).unwrap();
            let _ = self.release.recv();
        }
    }

    #[test]
    fn dropped_callback_result_retains_thread_admission_until_destructed() {
        let executor = NativeExecutor::new(1, 1, 1, 1).unwrap();
        let (callback_entered_sender, callback_entered_receiver) = std::sync::mpsc::sync_channel(1);
        let (return_sender, return_receiver) = std::sync::mpsc::sync_channel(1);
        let (drop_entered_sender, drop_entered_receiver) = std::sync::mpsc::sync_channel(1);
        let (drop_release_sender, drop_release_receiver) = std::sync::mpsc::sync_channel(1);
        let receiver = executor
            .spawn_blocking_callback("blocking-drop", move || {
                callback_entered_sender.send(()).unwrap();
                return_receiver.recv().unwrap();
                BlockingDrop {
                    entered: drop_entered_sender,
                    release: drop_release_receiver,
                }
            })
            .unwrap();
        callback_entered_receiver
            .recv_timeout(Duration::from_secs(1))
            .expect("callback did not start");
        drop(receiver);
        return_sender.send(()).unwrap();
        drop_entered_receiver
            .recv_timeout(Duration::from_secs(1))
            .expect("dropped receiver did not destroy its result on the callback thread");

        assert_eq!(executor.snapshot().active_callbacks, 1);
        assert!(matches!(
            executor.spawn_blocking_callback("second", || ()),
            Err(LoaderError::Busy {
                operation: "second"
            })
        ));

        drop_release_sender.send(()).unwrap();
        let deadline = std::time::Instant::now() + Duration::from_secs(1);
        while executor.snapshot().active_callbacks != 0 && std::time::Instant::now() < deadline {
            std::thread::yield_now();
        }
        assert_eq!(executor.snapshot().active_callbacks, 0);
        let deadline = std::time::Instant::now() + Duration::from_secs(1);
        let reused = loop {
            match executor.spawn_blocking_callback("reused", || ()) {
                Ok(receiver) => break receiver,
                Err(LoaderError::Busy { .. }) if std::time::Instant::now() < deadline => {
                    std::thread::yield_now();
                }
                Err(error) => panic!("callback admission was not reusable: {error}"),
            }
        };
        reused
            .recv_timeout(Duration::from_secs(1))
            .expect("reused callback did not finish");
    }

    #[test]
    fn callback_handoffs_never_exceed_the_admitted_activity_limit() {
        let executor = NativeExecutor::new(1, 1, 1, 1).unwrap();
        for _ in 0..1_024 {
            let receiver = loop {
                match executor.spawn_blocking_callback("handoff", || ()) {
                    Ok(receiver) => break receiver,
                    Err(LoaderError::Busy { .. }) => std::thread::yield_now(),
                    Err(error) => panic!("callback handoff failed: {error}"),
                }
            };
            receiver
                .recv_timeout(Duration::from_secs(1))
                .expect("callback handoff stalled");
        }
        assert_eq!(executor.snapshot().peak_callbacks, 1);
    }

    #[tokio::test(start_paused = true)]
    async fn completed_callback_releases_watchdog_resources_before_its_deadline() {
        let completion = Arc::new(CallbackCompletion::new());
        let (sender, receiver) = tokio::sync::oneshot::channel();
        assert!(completion.complete());
        sender.send(7_u8).unwrap();

        let dropped = Arc::new(Notify::new());
        let drop_signal = DropSignal(Arc::clone(&dropped));
        let on_timeout: Arc<dyn Fn() + Send + Sync> = Arc::new(move || {
            let _ = &drop_signal;
        });

        let deadline = tokio::time::Instant::now() + Duration::from_mins(1);
        let result = run_bounded_callback(receiver, completion, deadline, on_timeout).await;
        assert!(matches!(result, Ok(7)));
        tokio::time::timeout(Duration::from_secs(1), dropped.notified())
            .await
            .expect("completed callback retained its watchdog until the deadline");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn timeout_publication_precedes_callback_gate_release() {
        let completion = Arc::new(CallbackCompletion::new());
        let callback_completion = Arc::clone(&completion);
        let gate = Arc::new(Semaphore::new(1));
        let gate_permit = Arc::clone(&gate).try_acquire_owned().unwrap();
        let (callback_sender, callback_receiver) = std::sync::mpsc::sync_channel(1);
        let (released_sender, released_receiver) = std::sync::mpsc::sync_channel(1);
        let (result_sender, result_receiver) = tokio::sync::oneshot::channel();
        let callback = std::thread::spawn(move || {
            callback_receiver.recv().unwrap();
            assert!(!callback_completion.complete());
            drop(gate_permit);
            released_sender.send(()).unwrap();
            let _ = result_sender.send(());
        });

        let timeout_entered = Arc::new(std::sync::Barrier::new(2));
        let timeout_release = Arc::new(std::sync::Barrier::new(2));
        let timeout_entered_callback = Arc::clone(&timeout_entered);
        let timeout_release_callback = Arc::clone(&timeout_release);
        let on_timeout: Arc<dyn Fn() + Send + Sync> = Arc::new(move || {
            timeout_entered_callback.wait();
            timeout_release_callback.wait();
        });
        let deadline = tokio::time::Instant::now() + Duration::from_millis(10);
        let waiter = tokio::spawn(run_bounded_callback(
            result_receiver,
            completion,
            deadline,
            on_timeout,
        ));

        timeout_entered.wait();
        callback_sender.send(()).unwrap();
        let released_before_timeout_fencing = released_receiver
            .recv_timeout(Duration::from_millis(50))
            .is_ok();
        timeout_release.wait();
        if !released_before_timeout_fencing {
            released_receiver
                .recv_timeout(Duration::from_secs(1))
                .expect("callback did not release its gate after timeout fencing");
        }
        callback.join().unwrap();
        assert!(matches!(
            waiter.await.unwrap(),
            Err(CallbackWaitError::TimedOut)
        ));
        assert!(
            !released_before_timeout_fencing,
            "callback released its gate before timeout fencing finished"
        );
    }

    #[test]
    fn panicking_timeout_publication_does_not_strand_completion() {
        let completion = CallbackCompletion::new();
        let panic = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            completion.time_out(&|| panic!("injected timeout publication panic"));
        }));

        assert!(panic.is_err());
        assert!(completion.is_timed_out());
        assert!(!completion.complete());
    }

    #[test]
    fn factory_finalizer_has_reserved_capacity_when_the_ordinary_queue_is_full() {
        let executor = NativeExecutor::new(1, 1, 1, 1).unwrap();
        let (release_sender, release_receiver) = std::sync::mpsc::sync_channel(1);
        assert!(executor.try_submit_destruction(move || {
            release_receiver.recv().unwrap();
        }));
        let deadline = std::time::Instant::now() + Duration::from_secs(1);
        while executor.snapshot().active_destructions != 1 && std::time::Instant::now() < deadline {
            std::thread::yield_now();
        }
        assert_eq!(executor.snapshot().active_destructions, 1);
        assert!(executor.try_submit_destruction(|| {}));

        let permit = executor.reserve_factory_destruction().unwrap();
        let finalizer_executor = executor.clone();
        let (submitted_sender, submitted_receiver) = std::sync::mpsc::sync_channel(1);
        let (finished_sender, finished_receiver) = std::sync::mpsc::sync_channel(1);
        let submitter = std::thread::spawn(move || {
            finalizer_executor.submit_reserved_destruction(permit, move |_permit| {
                finished_sender.send(()).unwrap();
            });
            submitted_sender.send(()).unwrap();
        });
        let admission_result = submitted_receiver.recv_timeout(Duration::from_millis(100));
        if admission_result.is_err() {
            release_sender.send(()).unwrap();
            submitter.join().unwrap();
            panic!("factory finalizer waited behind ordinary queue admission");
        }
        assert_eq!(executor.snapshot().queued_destructions, 2);

        release_sender.send(()).unwrap();
        finished_receiver
            .recv_timeout(Duration::from_secs(1))
            .expect("reserved factory finalizer did not run");
        submitter.join().unwrap();
        assert_eq!(executor.snapshot().rejected_destructions, 0);
    }

    #[test]
    fn reserved_destruction_rejections_are_observable() {
        let executor = NativeExecutor::new(1, 1, 1, 1).unwrap();
        let _factory = executor.reserve_factory_destruction().unwrap();
        assert!(matches!(
            executor.reserve_factory_destruction(),
            Err(LoaderError::Busy { operation: "load" })
        ));
        assert_eq!(executor.snapshot().rejected_destructions, 1);
    }

    #[test]
    fn live_instance_admission_is_exact_bounded_and_reusable() {
        let executor = NativeExecutor::new(1, 1, 1, 2).unwrap();
        let first = executor.reserve_instance().unwrap();
        let second = executor.reserve_instance().unwrap();
        assert!(matches!(
            executor.reserve_instance(),
            Err(LoaderError::Busy {
                operation: "create"
            })
        ));
        let full = executor.snapshot();
        assert_eq!(full.active_instances, 2);
        assert_eq!(full.peak_instances, 2);
        assert_eq!(full.rejected_instances, 1);

        drop(first);
        let reused = executor.reserve_instance().unwrap();
        assert_eq!(executor.snapshot().active_instances, 2);
        drop(second);
        drop(reused);
        assert_eq!(executor.snapshot().active_instances, 0);
    }

    #[test]
    fn blocked_instance_finalizer_keeps_admission_and_pending_work_bounded() {
        let executor = NativeExecutor::new(1, 1, 1, 1).unwrap();
        let reservation = executor.reserve_instance().unwrap();
        let (release_sender, release_receiver) = std::sync::mpsc::sync_channel(1);
        executor.submit_reserved_instance_destruction(reservation, move |_reservation| {
            release_receiver.recv().unwrap();
        });
        let deadline = std::time::Instant::now() + Duration::from_secs(1);
        while executor.snapshot().active_destructions != 1 && std::time::Instant::now() < deadline {
            std::thread::yield_now();
        }

        assert!(matches!(
            executor.reserve_instance(),
            Err(LoaderError::Busy {
                operation: "create"
            })
        ));
        let blocked = executor.snapshot();
        assert_eq!(blocked.active_instances, 1);
        assert_eq!(blocked.pending_instance_destructions, 1);
        assert_eq!(blocked.rejected_instances, 1);

        release_sender.send(()).unwrap();
        let deadline = std::time::Instant::now() + Duration::from_secs(1);
        while {
            let snapshot = executor.snapshot();
            (snapshot.active_instances != 0 || snapshot.pending_instance_destructions != 0)
                && std::time::Instant::now() < deadline
        } {
            std::thread::yield_now();
        }
        let drained = executor.snapshot();
        assert_eq!(drained.active_instances, 0);
        assert_eq!(drained.pending_instance_destructions, 0);
        executor
            .reserve_instance()
            .expect("completed finalization must release live-instance admission");
    }
}
