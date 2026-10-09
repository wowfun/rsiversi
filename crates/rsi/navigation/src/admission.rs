use futures_util::future::BoxFuture;
use rsi_api_protocol::{ApiError, Result};
use rsi_meta::Execution;
use std::sync::{Arc, Mutex};
use tokio::sync::Semaphore;
use tokio_util::{sync::CancellationToken, task::TaskTracker};

#[derive(Debug)]
pub(super) struct Admission {
    closed: Mutex<bool>,
    pub(super) slots: Arc<Semaphore>,
    pub(super) tasks: TaskTracker,
    read_stop: CancellationToken,
    execution: Execution,
}

impl Admission {
    pub(super) fn new(slots: usize, execution: Execution) -> Arc<Self> {
        Arc::new(Self {
            closed: Mutex::new(false),
            slots: Arc::new(Semaphore::new(slots)),
            tasks: TaskTracker::new(),
            read_stop: CancellationToken::new(),
            execution,
        })
    }

    pub(super) fn run_mutation<T: Send + 'static>(
        &self,
        work: impl FnOnce() -> BoxFuture<'static, Result<T>>,
    ) -> Result<BoxFuture<'static, Result<T>>> {
        let closed = self.closed.lock().expect("request admission poisoned");
        if *closed {
            return Err(ApiError::ShuttingDown);
        }
        let permit = self
            .slots
            .clone()
            .try_acquire_owned()
            .map_err(|_| ApiError::Capacity)?;
        let future = work();
        let task = self.execution.spawn(self.tasks.track_future(async move {
            let _permit = permit;
            future.await
        }));
        drop(closed);
        Ok(Box::pin(async move {
            task.await.map_err(|_| ApiError::OutcomeUnknown)?
        }))
    }

    pub(super) fn run_read<T: Send + 'static>(
        self: &Arc<Self>,
        work: impl FnOnce() -> BoxFuture<'static, Result<T>> + Send + 'static,
    ) -> Result<BoxFuture<'static, Result<T>>> {
        let closed = self.closed.lock().expect("request admission poisoned");
        if *closed {
            return Err(ApiError::ShuttingDown);
        }
        let admission = self.clone();
        drop(closed);
        Ok(Box::pin(async move {
            let (task, abandon) = {
                let closed = admission.closed.lock().expect("request admission poisoned");
                if *closed {
                    return Err(ApiError::ShuttingDown);
                }
                let permit = admission
                    .slots
                    .clone()
                    .try_acquire_owned()
                    .map_err(|_| ApiError::Capacity)?;
                let stop = admission.read_stop.clone();
                let abandoned = CancellationToken::new();
                let abandon = abandoned.clone().drop_guard();
                let task = admission.execution.spawn(admission.tasks.track_future(async move {
                    let _permit = permit;
                    tokio::select! { biased; () = stop.cancelled() => Err(ApiError::ShuttingDown), () = abandoned.cancelled() => Err(ApiError::ShuttingDown), result = async move { work().await } => result }
                }));
                (task, abandon)
            };
            let result = task.await.map_err(|_| ApiError::Unavailable)?;
            abandon.disarm();
            result
        }))
    }

    pub(super) async fn close(&self, writer: &Semaphore) {
        {
            let mut closed = self.closed.lock().expect("request admission poisoned");
            *closed = true;
            self.read_stop.cancel();
            self.slots.close();
            writer.close();
            self.tasks.close();
        }
        self.tasks.wait().await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn retirement_settles_a_polled_read_even_while_its_waiter_is_parked() {
        let admission = Admission::new(1, Execution::native(tokio::runtime::Handle::current()));
        let mut read = admission
            .run_read(|| Box::pin(std::future::pending::<Result<()>>()))
            .unwrap();
        assert!(futures_util::poll!(&mut read).is_pending());
        tokio::time::timeout(
            std::time::Duration::from_millis(100),
            admission.close(&Semaphore::new(1)),
        )
        .await
        .expect("retirement must not require the caller to resume polling");
        assert_eq!(admission.slots.available_permits(), 1);
        assert_eq!(read.await, Err(ApiError::ShuttingDown));
    }
    #[tokio::test]
    async fn unpolled_reads_do_not_construct_work_or_reserve_request_capacity() {
        let admission = Admission::new(1, Execution::native(tokio::runtime::Handle::current()));
        let constructed = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let captured = constructed.clone();
        let lazy = admission
            .run_read(move || {
                captured.store(true, std::sync::atomic::Ordering::SeqCst);
                Box::pin(async { Ok(()) })
            })
            .unwrap();
        assert!(!constructed.load(std::sync::atomic::Ordering::SeqCst));
        assert_eq!(admission.slots.available_permits(), 1);
        admission
            .run_read(|| Box::pin(async { Ok(()) }))
            .unwrap()
            .await
            .unwrap();
        admission.close(&Semaphore::new(1)).await;
        assert_eq!(lazy.await, Err(ApiError::ShuttingDown));
        assert!(!constructed.load(std::sync::atomic::Ordering::SeqCst));
    }

    #[tokio::test]
    async fn retirement_drains_an_accepted_mutation_after_its_waiter_is_dropped() {
        let admission = Admission::new(2, Execution::native(tokio::runtime::Handle::current()));
        let writer = Semaphore::new(1);
        let (release, wait) = tokio::sync::oneshot::channel();
        let (published, receipt) = tokio::sync::oneshot::channel();
        let waiter = admission
            .run_mutation(|| {
                Box::pin(async move {
                    wait.await.unwrap();
                    published.send(()).unwrap();
                    Ok(())
                })
            })
            .unwrap();
        drop(waiter);
        let mut close = Box::pin(admission.close(&writer));
        assert!(futures_util::poll!(&mut close).is_pending());
        assert_eq!(admission.slots.available_permits(), 1);
        release.send(()).unwrap();
        close.await;
        receipt.await.unwrap();
        assert_eq!(admission.slots.available_permits(), 2);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn concurrent_first_poll_and_two_closes_settle_without_leaking_admission() {
        let admission = Admission::new(2, Execution::native(tokio::runtime::Handle::current()));
        let writer = Arc::new(Semaphore::new(1));
        let start = Arc::new(tokio::sync::Barrier::new(3));
        let read = admission
            .run_read(|| Box::pin(std::future::pending::<Result<()>>()))
            .unwrap();
        let reading = {
            let start = start.clone();
            tokio::spawn(async move {
                start.wait().await;
                read.await
            })
        };
        let mut closing = Vec::new();
        for _ in 0..2 {
            let admission = admission.clone();
            let writer = writer.clone();
            let start = start.clone();
            closing.push(tokio::spawn(async move {
                start.wait().await;
                admission.close(&writer).await;
            }));
        }
        tokio::time::timeout(std::time::Duration::from_secs(2), async {
            assert_eq!(reading.await.unwrap(), Err(ApiError::ShuttingDown));
            for close in closing {
                close.await.unwrap();
            }
        })
        .await
        .unwrap();
        assert!(admission.tasks.is_empty());
        assert_eq!(admission.slots.available_permits(), 2);
    }

    #[tokio::test]
    async fn first_poll_and_two_retirements_share_one_registration_boundary() {
        for poll_first in [false, true] {
            let admission = Admission::new(2, Execution::native(tokio::runtime::Handle::current()));
            let writer = Semaphore::new(1);
            let mut read = admission
                .run_read(|| Box::pin(std::future::pending::<Result<()>>()))
                .unwrap();
            if poll_first {
                assert!(futures_util::poll!(&mut read).is_pending());
            }
            let mut first = Box::pin(admission.close(&writer));
            let mut second = Box::pin(admission.close(&writer));
            if poll_first {
                assert!(futures_util::poll!(&mut first).is_pending());
                assert!(futures_util::poll!(&mut second).is_pending());
            } else {
                assert!(futures_util::poll!(&mut first).is_ready());
                assert!(futures_util::poll!(&mut second).is_ready());
            }
            assert_eq!(read.await, Err(ApiError::ShuttingDown));
            if poll_first {
                first.await;
                second.await;
            }
            assert!(admission.tasks.is_empty());
            assert_eq!(admission.slots.available_permits(), 2);
        }
    }
}
