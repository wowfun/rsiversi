use futures_util::future::BoxFuture;
use rsi_meta::{Execution, Runtime, RuntimeLimits};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::watch;

/// Runtime time advances only when the test crosses an explicit behavior gate.
/// Tokio's real clock remains available for watchdogs, including blocking jobs.
#[derive(Debug)]
pub(crate) struct ManualClock(watch::Sender<Duration>);

impl ManualClock {
    pub(crate) fn runtime(limits: RuntimeLimits) -> (Runtime, Arc<Self>) {
        let clock = Arc::new(Self(watch::channel(Duration::ZERO).0));
        let runtime = Runtime::with_execution(limits, Execution::new(clock.clone())).unwrap();
        (runtime, clock)
    }

    pub(crate) fn advance(&self, by: Duration) {
        self.0.send_modify(|now| *now += by);
    }
}

impl rsi_meta_execution::Backend for ManualClock {
    fn spawn(&self, future: BoxFuture<'static, ()>) {
        drop(tokio::spawn(future));
    }

    fn prepare(&self, job: Box<dyn FnOnce() + Send>) {
        drop(tokio::task::spawn_blocking(job));
    }

    fn now(&self) -> Duration {
        *self.0.borrow()
    }

    fn sleep_until(&self, at: Duration) -> BoxFuture<'static, ()> {
        let mut time = self.0.subscribe();
        Box::pin(async move {
            while *time.borrow_and_update() < at {
                time.changed().await.expect("clock outlives its execution");
            }
        })
    }
}
