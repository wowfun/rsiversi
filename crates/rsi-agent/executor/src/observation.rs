use futures_util::FutureExt;
use rsi_agent_turn_protocol::{ControlledWork, ExecutionInterval, ExecutionObservationEnd};
use std::{
    panic::AssertUnwindSafe,
    sync::atomic::{AtomicBool, Ordering},
};
use std::{sync::Arc, time::Duration};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;
use tokio_util::task::TaskTracker;

pub(super) const OBSERVATION_WAIT: Duration = Duration::from_secs(30);

#[derive(Debug)]
pub(super) struct Observations {
    slots: Arc<Semaphore>,
    tasks: TaskTracker,
    failed: Arc<AtomicBool>,
}
impl Default for Observations {
    fn default() -> Self {
        Self {
            slots: Arc::new(Semaphore::new(256)),
            tasks: TaskTracker::new(),
            failed: Arc::default(),
        }
    }
}
impl Observations {
    pub(super) async fn reserve(&self, stop: &CancellationToken) -> Option<OwnedSemaphorePermit> {
        tokio::select! { biased;
            () = stop.cancelled() => None,
            permit = self.slots.clone().acquire_owned() => permit.ok(),
        }
    }
    pub(super) fn finish(
        &self,
        interval: Arc<dyn ExecutionInterval>,
        began: bool,
        work: ControlledWork,
        stop: CancellationToken,
        slot: OwnedSemaphorePermit,
    ) {
        let failed = self.failed.clone();
        self.tasks.spawn(async move {
            let _slot = slot;
            if AssertUnwindSafe(end(interval, began, work, &stop))
                .catch_unwind()
                .await
                .is_err()
            {
                failed.store(true, Ordering::Release);
                stop.cancel();
            }
        });
    }
    pub(super) async fn close(&self) {
        self.slots.close();
        self.tasks.close();
        self.tasks.wait().await;
    }
    pub(super) fn failed(&self) -> bool {
        self.failed.load(Ordering::Acquire)
    }
}

pub(super) async fn begin(
    interval: &Arc<dyn ExecutionInterval>,
    retirement: &CancellationToken,
    deadline: Instant,
) -> bool {
    let stop = retirement.child_token();
    let _guard = stop.clone().drop_guard();
    tokio::select! { biased;
        () = retirement.cancelled() => false,
        result = tokio::time::timeout_at(deadline, interval.begin(stop)) => result.is_ok(),
    }
}

pub(super) async fn end(
    interval: Arc<dyn ExecutionInterval>,
    begin_completed: bool,
    work: ControlledWork,
    retirement: &CancellationToken,
) {
    let stop = retirement.child_token();
    let _guard = stop.clone().drop_guard();
    let deadline = Instant::now() + OBSERVATION_WAIT;
    let controlled_work = tokio::select! { biased;
        () = retirement.cancelled() => work.status(),
        result = tokio::time::timeout_at(deadline, work.wait(stop.clone())) => result.unwrap_or_else(|_| work.status()),
    };
    let evidence = ExecutionObservationEnd {
        begin_completed,
        controlled_work,
    };
    if Instant::now() >= deadline {
        stop.cancel();
    }
    tokio::select! { biased;
        _ = tokio::time::timeout_at(deadline, interval.end(evidence, stop)) => {},
        () = retirement.cancelled() => {},
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rsi_agent_turn_protocol::ControlledWorkStatus;
    use std::sync::Mutex;
    #[derive(Debug, Default)]
    struct Hung {
        begin: Mutex<Option<CancellationToken>>,
        end: Mutex<Option<(ExecutionObservationEnd, CancellationToken)>>,
    }
    #[async_trait::async_trait]
    impl ExecutionInterval for Hung {
        async fn begin(&self, stop: CancellationToken) {
            *self.begin.lock().unwrap() = Some(stop);
            std::future::pending::<()>().await;
        }
        async fn end(&self, evidence: ExecutionObservationEnd, stop: CancellationToken) {
            *self.end.lock().unwrap() = Some((evidence, stop));
            std::future::pending::<()>().await;
        }
    }
    #[tokio::test(start_paused = true)]
    async fn deadlines_cancel_observers_and_never_upgrade_running_work() {
        let implementation = Arc::new(Hung::default());
        let interval: Arc<dyn ExecutionInterval> = implementation.clone();
        assert!(
            !begin(
                &interval,
                &CancellationToken::new(),
                Instant::now() + OBSERVATION_WAIT
            )
            .await
        );
        assert!(
            implementation
                .begin
                .lock()
                .unwrap()
                .as_ref()
                .unwrap()
                .is_cancelled()
        );
        let (_tracker, work) = super::super::controlled_work::Tracker::new();
        let before = Instant::now();
        end(interval, false, work, &CancellationToken::new()).await;
        assert_eq!(Instant::now() - before, OBSERVATION_WAIT);
        let evidence = implementation.end.lock().unwrap();
        let (evidence, stop) = evidence.as_ref().unwrap();
        assert!(!evidence.begin_completed);
        assert_eq!(evidence.controlled_work, ControlledWorkStatus::Running);
        assert!(stop.is_cancelled());
    }

    #[tokio::test]
    async fn reserved_observations_are_bounded_and_shutdown_joins_the_tail() {
        let tasks = Observations::default();
        let stop = CancellationToken::new();
        let mut slots = Vec::new();
        for _ in 0..256 {
            slots.push(tasks.reserve(&stop).await.unwrap());
        }
        let reserving = tasks.reserve(&stop);
        tokio::pin!(reserving);
        assert!(futures_util::poll!(reserving.as_mut()).is_pending());
        drop(slots.pop());
        let slot = reserving.await.unwrap();
        let implementation = Arc::new(Hung::default());
        let (_tracker, work) = super::super::controlled_work::Tracker::new();
        tasks.finish(implementation.clone(), true, work, stop.clone(), slot);
        stop.cancel();
        tasks.close().await;
        assert!(tasks.tasks.is_empty());
        assert!(tasks.reserve(&CancellationToken::new()).await.is_none());
        assert!(
            implementation
                .end
                .lock()
                .unwrap()
                .as_ref()
                .unwrap()
                .1
                .is_cancelled()
        );
    }

    #[derive(Debug)]
    struct Panics;
    #[async_trait::async_trait]
    impl ExecutionInterval for Panics {
        async fn begin(&self, _: CancellationToken) {}
        async fn end(&self, _: ExecutionObservationEnd, _: CancellationToken) {
            panic!("observer failure");
        }
    }
    #[tokio::test]
    async fn end_panic_retires_the_executor_and_is_reported_after_join() {
        let tasks = Observations::default();
        let stop = CancellationToken::new();
        let slot = tasks.reserve(&stop).await.unwrap();
        let (tracker, work) = super::super::controlled_work::Tracker::new();
        let guard = tracker.guard();
        tracker.finalized(true);
        guard.confirm();
        tasks.finish(Arc::new(Panics), true, work, stop.clone(), slot);
        tasks.close().await;
        assert!(stop.is_cancelled());
        assert!(tasks.failed());
        assert!(tasks.tasks.is_empty());
    }

    #[tokio::test(start_paused = true)]
    async fn retirement_releases_a_lane_without_waiting_for_its_global_tool_cleanup() {
        let implementation = Arc::new(Hung::default());
        let interval: Arc<dyn ExecutionInterval> = implementation.clone();
        let (_tracker, work) = super::super::controlled_work::Tracker::new();
        let retirement = CancellationToken::new();
        let ending = end(interval, true, work.clone(), &retirement);
        tokio::pin!(ending);
        assert!(futures_util::poll!(ending.as_mut()).is_pending());
        let before = tokio::time::Instant::now();
        retirement.cancel();
        ending.await;
        assert_eq!(tokio::time::Instant::now(), before);
        assert_eq!(work.status(), ControlledWorkStatus::Running);
        let evidence = implementation.end.lock().unwrap();
        let (evidence, stop) = evidence.as_ref().unwrap();
        assert_eq!(evidence.controlled_work, ControlledWorkStatus::Running);
        assert!(stop.is_cancelled());
    }
}
