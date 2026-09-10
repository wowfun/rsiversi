use rsi_meta_execution::Execution;
use std::sync::Arc;
use std::time::Duration;

#[tokio::test]
async fn dropping_task_waiter_does_not_cancel_owned_work() {
    let execution = Execution::native(tokio::runtime::Handle::current());
    let release = Arc::new(tokio::sync::Notify::new());
    let (completed, completion) = tokio::sync::oneshot::channel();
    let task = execution.spawn({
        let release = release.clone();
        async move {
            release.notified().await;
            completed.send(17).unwrap();
        }
    });
    drop(task);
    release.notify_one();
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(5), completion)
            .await
            .unwrap()
            .unwrap(),
        17
    );
}

#[tokio::test]
async fn dropping_preparation_waiter_keeps_the_job_owned() {
    let execution = Execution::native(tokio::runtime::Handle::current());
    let (release, released) = std::sync::mpsc::sync_channel(1);
    let (completed, completion) = tokio::sync::oneshot::channel();
    let job = execution.prepare(move || {
        released.recv_timeout(Duration::from_secs(5)).unwrap();
        completed.send(23).unwrap();
    });
    drop(job);
    release.send(()).unwrap();
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(5), completion)
            .await
            .unwrap()
            .unwrap(),
        23
    );
}

#[tokio::test(start_paused = true)]
async fn expired_deadline_does_not_poll_a_ready_mutation() {
    let execution = Execution::native(tokio::runtime::Handle::current());
    let deadline = execution.deadline_after(Duration::from_secs(1));
    tokio::time::advance(Duration::from_secs(2)).await;
    let mut polled = false;
    assert!(
        deadline
            .timeout(async {
                polled = true;
            })
            .await
            .is_err()
    );
    assert!(!polled);
}

#[tokio::test]
async fn synchronous_work_cannot_publish_after_its_deadline() {
    use futures_util::future::BoxFuture;
    use rsi_meta_execution::Backend;
    use std::sync::atomic::{AtomicU64, Ordering};

    #[derive(Debug, Default)]
    struct ManualClock(AtomicU64);

    impl Backend for ManualClock {
        fn spawn(&self, _: BoxFuture<'static, ()>) {
            panic!("this scenario does not schedule tasks");
        }

        fn prepare(&self, _: Box<dyn FnOnce() + Send>) {
            panic!("this scenario does not prepare tasks");
        }

        fn now(&self) -> Duration {
            Duration::from_secs(self.0.load(Ordering::SeqCst))
        }

        fn sleep_until(&self, _: Duration) -> BoxFuture<'static, ()> {
            // No timer wake can interrupt a synchronous poll in a Worker.
            Box::pin(std::future::pending())
        }
    }

    let clock = Arc::new(ManualClock::default());
    let execution = Execution::new(clock.clone());
    let deadline = execution.deadline_after(Duration::from_secs(1));
    let mut completed = false;
    let result = deadline
        .timeout(async {
            clock.0.store(2, Ordering::SeqCst);
            completed = true;
            17
        })
        .await;
    assert!(
        completed,
        "the deadline must initially allow the work to run"
    );
    assert!(result.is_err(), "the late result must not be published");
}
