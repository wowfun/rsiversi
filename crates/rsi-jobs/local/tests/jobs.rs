use async_trait::async_trait;
use rsi_jobs::{JobOutcome, JobScope, JobSpec, JobTask, JobsContract, JobsError, Result};
use rsi_jobs_local::JobsLocalFactory;
use rsi_meta::{ResolvedFactory, Runtime, UpdateMode};
use serde_json::{Value, json};
use std::sync::Arc;
use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;

#[derive(Debug)]
struct WaitForCancellation {
    entered: Arc<Notify>,
    settled: Arc<Notify>,
}

#[derive(Debug)]
struct Panics;

#[derive(Debug)]
struct FailsWithLargeDiagnostic;

#[derive(Debug)]
struct IgnoresCancellationBriefly {
    entered: Arc<Notify>,
}

#[derive(Debug)]
struct PausesAfterCancellation {
    entered: Arc<Notify>,
    cancellation_seen: Arc<Notify>,
    release: Arc<Notify>,
}

#[async_trait]
impl JobTask for Panics {
    async fn run(&self, _cancellation: CancellationToken) -> Result<Value> {
        panic!("test panic must be contained by Jobs")
    }
}

#[async_trait]
impl JobTask for FailsWithLargeDiagnostic {
    async fn run(&self, _cancellation: CancellationToken) -> Result<Value> {
        Err(JobsError::Execution(
            "x".repeat(rsi_jobs::MAXIMUM_JOB_FAILURE_BYTES + 1),
        ))
    }
}

#[async_trait]
impl JobTask for WaitForCancellation {
    async fn run(&self, cancellation: CancellationToken) -> Result<Value> {
        self.entered.notify_one();
        cancellation.cancelled().await;
        self.settled.notify_one();
        Ok(json!({"ignored":true}))
    }
}

#[async_trait]
impl JobTask for IgnoresCancellationBriefly {
    async fn run(&self, _cancellation: CancellationToken) -> Result<Value> {
        self.entered.notify_one();
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        Ok(Value::Null)
    }
}

#[async_trait]
impl JobTask for PausesAfterCancellation {
    async fn run(&self, cancellation: CancellationToken) -> Result<Value> {
        self.entered.notify_one();
        cancellation.cancelled().await;
        self.cancellation_seen.notify_one();
        self.release.notified().await;
        Ok(Value::Null)
    }
}

#[test]
fn submission_without_an_entered_tokio_runtime_fails_without_panicking_or_consuming_capacity() {
    let asynchronous = tokio::runtime::Runtime::new().unwrap();
    let (runtime, fiber, jobs) = asynchronous.block_on(async {
        let runtime = Runtime::default();
        let fiber = runtime
            .root()
            .apply(
                ResolvedFactory::linked(
                    "rsi.jobs.local",
                    "test",
                    UpdateMode::Replayable,
                    Arc::new(JobsLocalFactory),
                ),
                json!({"maximum_active_jobs":1,"shutdown_timeout_ms":1000}),
            )
            .await
            .unwrap();
        let jobs = runtime.root().lookup_local::<JobsContract>().unwrap();
        (runtime, fiber, jobs)
    });
    assert!(matches!(
        jobs.submit(JobSpec {
            name: "outside-runtime".into(),
            task: Arc::new(IgnoresCancellationBriefly {
                entered: Arc::new(Notify::new()),
            }),
        }),
        Err(JobsError::Execution(message)) if message == "Tokio runtime is unavailable"
    ));
    asynchronous.block_on(async {
        drop(jobs);
        assert!(fiber.dispose().await.is_clean());
        assert!(runtime.shutdown().await.is_complete());
    });
}

#[tokio::test]
async fn handle_cancel_is_joinable_and_generation_cleanup_waits_for_settlement() {
    let runtime = Runtime::default();
    let fiber = runtime
        .root()
        .apply(
            ResolvedFactory::linked(
                "rsi.jobs.local",
                "test",
                UpdateMode::Replayable,
                Arc::new(JobsLocalFactory),
            ),
            json!({"maximum_active_jobs":2,"shutdown_timeout_ms":1000}),
        )
        .await
        .unwrap();
    let jobs = runtime.root().lookup_local::<JobsContract>().unwrap();
    let entered = Arc::new(Notify::new());
    let settled = Arc::new(Notify::new());
    let handle = jobs
        .submit(JobSpec {
            name: "wait".into(),
            task: Arc::new(WaitForCancellation {
                entered: entered.clone(),
                settled: settled.clone(),
            }),
        })
        .unwrap();
    entered.notified().await;
    handle.cancel();
    settled.notified().await;
    assert_eq!(handle.join().await, JobOutcome::Cancelled);
    assert_eq!(handle.join().await, JobOutcome::Cancelled);
    let panicked = jobs
        .submit(JobSpec {
            name: "panic".into(),
            task: Arc::new(Panics),
        })
        .unwrap();
    assert_eq!(
        panicked.join().await,
        JobOutcome::failed("job task panicked")
    );
    let failed = jobs
        .submit(JobSpec {
            name: "large-failure".into(),
            task: Arc::new(FailsWithLargeDiagnostic),
        })
        .unwrap()
        .join()
        .await;
    assert!(matches!(
        failed,
        JobOutcome::Failed(message)
            if message.as_str().len() == rsi_jobs::MAXIMUM_JOB_FAILURE_BYTES
    ));
    drop(jobs);
    assert!(fiber.dispose().await.is_clean());
    assert!(runtime.root().lookup_local::<JobsContract>().is_none());
}

#[tokio::test]
async fn cancel_all_is_bounded_and_reports_unsettled_cooperative_work() {
    let runtime = Runtime::default();
    let fiber = runtime
        .root()
        .apply(
            ResolvedFactory::linked(
                "rsi.jobs.local",
                "test",
                UpdateMode::Replayable,
                Arc::new(JobsLocalFactory),
            ),
            json!({"maximum_active_jobs":2,"shutdown_timeout_ms":5}),
        )
        .await
        .unwrap();
    let jobs = runtime.root().lookup_local::<JobsContract>().unwrap();
    let entered = Arc::new(Notify::new());
    let handle = jobs
        .submit(JobSpec {
            name: "slow-ignore".into(),
            task: Arc::new(IgnoresCancellationBriefly {
                entered: entered.clone(),
            }),
        })
        .unwrap();
    entered.notified().await;
    assert_eq!(jobs.cancel_all().await, Err(JobsError::CancellationTimeout));
    assert_eq!(handle.join().await, JobOutcome::Cancelled);
    assert!(jobs.cancel_all().await.is_ok());
    drop(jobs);
    assert!(fiber.dispose().await.is_clean());
}

#[tokio::test]
async fn cancel_all_closes_admission_for_its_complete_snapshot_window() {
    let runtime = Runtime::default();
    let fiber = runtime
        .root()
        .apply(
            ResolvedFactory::linked(
                "rsi.jobs.local",
                "test",
                UpdateMode::Replayable,
                Arc::new(JobsLocalFactory),
            ),
            json!({"maximum_active_jobs":2,"shutdown_timeout_ms":1000}),
        )
        .await
        .unwrap();
    let jobs = runtime.root().lookup_local::<JobsContract>().unwrap();
    let entered = Arc::new(Notify::new());
    let cancellation_seen = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    let first = jobs
        .submit(JobSpec {
            name: "paused-cancel".into(),
            task: Arc::new(PausesAfterCancellation {
                entered: entered.clone(),
                cancellation_seen: cancellation_seen.clone(),
                release: release.clone(),
            }),
        })
        .unwrap();
    entered.notified().await;

    let draining_jobs = jobs.clone();
    let drain = tokio::spawn(async move { draining_jobs.cancel_all().await });
    cancellation_seen.notified().await;
    assert!(matches!(
        jobs.submit(JobSpec {
            name: "must-not-escape-drain".into(),
            task: Arc::new(IgnoresCancellationBriefly {
                entered: Arc::new(Notify::new()),
            }),
        }),
        Err(JobsError::ShuttingDown)
    ));

    release.notify_one();
    assert_eq!(drain.await.unwrap(), Ok(()));
    assert_eq!(first.join().await, JobOutcome::Cancelled);
    let after = jobs
        .submit(JobSpec {
            name: "after-drain".into(),
            task: Arc::new(IgnoresCancellationBriefly {
                entered: Arc::new(Notify::new()),
            }),
        })
        .unwrap();
    assert_eq!(after.join().await, JobOutcome::Completed(Value::Null));

    drop(jobs);
    assert!(fiber.dispose().await.is_clean());
}

#[tokio::test]
async fn owner_finalization_is_isolated_and_a_timeout_keeps_only_that_scope_closed() {
    let runtime = Runtime::default();
    let fiber = runtime
        .root()
        .apply(
            ResolvedFactory::linked(
                "rsi.jobs.local",
                "test",
                UpdateMode::Replayable,
                Arc::new(JobsLocalFactory),
            ),
            json!({"maximum_active_jobs":4,"shutdown_timeout_ms":5}),
        )
        .await
        .unwrap();
    let jobs = runtime.root().lookup_local::<JobsContract>().unwrap();
    let first_scope = JobScope::new("rsi.agent.turn", ["session-a", "turn-a"]).unwrap();
    let other_scope = JobScope::new("rsi.agent.turn", ["session-b", "turn-b"]).unwrap();
    let entered = Arc::new(Notify::new());
    let cancellation_seen = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    let first = jobs
        .submit_scoped(
            first_scope.clone(),
            JobSpec {
                name: "first-scope".into(),
                task: Arc::new(PausesAfterCancellation {
                    entered: entered.clone(),
                    cancellation_seen: cancellation_seen.clone(),
                    release: release.clone(),
                }),
            },
        )
        .unwrap();
    entered.notified().await;
    let other = jobs
        .submit_scoped(
            other_scope.clone(),
            JobSpec {
                name: "other-scope".into(),
                task: Arc::new(IgnoresCancellationBriefly {
                    entered: Arc::new(Notify::new()),
                }),
            },
        )
        .unwrap();

    assert_eq!(
        jobs.cancel_scope(&first_scope).await,
        Err(JobsError::CancellationTimeout)
    );
    cancellation_seen.notified().await;
    assert!(matches!(
        jobs.submit_scoped(
            first_scope.clone(),
            JobSpec {
                name: "must-not-reopen".into(),
                task: Arc::new(IgnoresCancellationBriefly {
                    entered: Arc::new(Notify::new()),
                }),
            },
        ),
        Err(JobsError::ShuttingDown)
    ));
    assert_eq!(other.join().await, JobOutcome::Completed(Value::Null));

    release.notify_one();
    assert_eq!(first.join().await, JobOutcome::Cancelled);
    let after_settlement = jobs
        .submit_scoped(
            first_scope,
            JobSpec {
                name: "after-settlement".into(),
                task: Arc::new(IgnoresCancellationBriefly {
                    entered: Arc::new(Notify::new()),
                }),
            },
        )
        .unwrap();
    assert_eq!(
        after_settlement.join().await,
        JobOutcome::Completed(Value::Null)
    );

    drop(jobs);
    assert!(fiber.dispose().await.is_clean());
}
