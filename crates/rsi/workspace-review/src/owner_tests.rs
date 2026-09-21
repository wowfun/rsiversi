use super::*;
use rsi_agent_session_protocol::{
    AgentPresetId, FrozenAgentSettings, SessionHeader, SessionId, TurnId,
};
use rsi_meta::{ResolvedFactory, Runtime, UpdateMode};
use rsi_storage::StorageError;
use rsi_storage_domain::DomainSpec;
use std::sync::atomic::{AtomicBool, Ordering};
#[derive(Debug)]
struct Storage {
    spec: DomainSpec,
    rows: Mutex<BTreeMap<String, serde_json::Value>>,
    fail: AtomicBool,
}
#[async_trait]
impl Domain for Storage {
    fn spec(&self) -> &DomainSpec {
        &self.spec
    }
    async fn snapshot(&self) -> BTreeMap<String, serde_json::Value> {
        self.rows.lock().unwrap().clone()
    }
    async fn put(
        &self,
        key: &str,
        value: serde_json::Value,
    ) -> std::result::Result<(), StorageError> {
        self.rows.lock().unwrap().insert(key.into(), value);
        if self.fail.load(Ordering::SeqCst) {
            Err(StorageError::Io("acknowledgement lost after write".into()))
        } else {
            Ok(())
        }
    }
    async fn delete(&self, _: &str) -> std::result::Result<bool, StorageError> {
        unreachable!()
    }
}
async fn owner(runtime: &Runtime, path: PathBuf, domain: Arc<Storage>) -> Arc<WorkspaceReview> {
    let git = Git {
        process: runtime
            .root()
            .lookup_local::<rsi_process::ProcessContract>()
            .unwrap(),
        sandbox: Arc::new(crate::tests::TestSandbox),
        files: Arc::new(rsi_files::LocalFiles::new().unwrap()),
        program: "/usr/bin/git".into(),
        quota: Arc::new(Semaphore::new(1024 * 1024 * 1024)),
        tasks: TaskTracker::new(),
    };
    WorkspaceReview::open(
        domain,
        Root::open(path).await.unwrap(),
        git,
        runtime.execution().clone(),
    )
    .await
    .unwrap()
}
fn start(path: &std::path::Path, recovered: bool) -> ExecutionObservationStart {
    ExecutionObservationStart {
        header: SessionHeader::new(
            SessionId::new("review-source").unwrap(),
            1,
            path.to_str().unwrap(),
            AgentPresetId::new("test").unwrap(),
            FrozenAgentSettings::new(
                "test",
                "test",
                rsi_ai_protocol::ModelRef::new("test", "model").unwrap(),
                rsi_sandbox::SandboxMode::WorkspaceWrite,
                false,
            )
            .unwrap(),
        )
        .unwrap(),
        turn: TurnId::new("turn").unwrap(),
        claim: 1,
        accepted_seq: 1,
        live_seq: 2,
        recovered,
    }
}
#[tokio::test]
#[expect(
    clippy::too_many_lines,
    reason = "one causal failure, teardown and recovery sequence"
)]
async fn lost_storage_ack_blocks_reads_and_admission_until_restart_then_pending_is_expired() {
    let temporary = tempfile::tempdir().unwrap();
    let runtime = Runtime::default();
    let process = runtime
        .root()
        .apply(
            ResolvedFactory::linked(
                "process",
                "test",
                UpdateMode::Replayable,
                Arc::new(rsi_process_local::ProcessLocalFactory),
            ),
            serde_json::json!({}),
        )
        .await
        .unwrap();
    let domain = Arc::new(Storage {
        spec: DomainSpec {
            id: "review".into(),
            backend: "base".into(),
            version: 1,
            maximum_records: 8192,
            maximum_bytes: 64 * 1024 * 1024,
        },
        rows: Mutex::new(BTreeMap::new()),
        fail: AtomicBool::new(true),
    });
    let path = temporary.path().join("scratch");
    let first = owner(&runtime, path.clone(), domain.clone()).await;
    let interval = first.admit(start(temporary.path(), false)).unwrap();
    let id = interval.id.clone();
    interval.begin(CancellationToken::new()).await;
    let summary = first.state.lock().unwrap().entries[&id].summary.clone();
    assert_eq!(
        domain.snapshot().await.len(),
        1,
        "test storage wrote before losing its acknowledgement"
    );
    let request = Request::List {
        scope: summary.scope.clone(),
        after: None,
    };
    assert!(matches!(
        first.read(request.clone(), CancellationToken::new()).await,
        Err(ApiError::OutcomeUnknown)
    ));
    assert!(matches!(
        first.admit(start(temporary.path(), false)),
        Err(ApiError::OutcomeUnknown)
    ));
    assert_eq!(
        std::fs::read_dir(&path).unwrap().count(),
        1,
        "no baseline starts after uncertain Pending persistence"
    );
    first.close().await;
    drop(interval);
    drop(first);
    domain.fail.store(false, Ordering::SeqCst);
    let second = owner(&runtime, path, domain).await;
    let Reply::Summaries { epoch, items, .. } = second
        .read(request, CancellationToken::new())
        .await
        .unwrap()
    else {
        panic!("summaries")
    };
    assert_eq!(items, vec![summary.clone()]);
    assert_ne!(epoch, summary.epoch);
    assert!(matches!(
        second
            .read(
                Request::Files {
                    scope: summary.scope,
                    id,
                    offset: 0
                },
                CancellationToken::new()
            )
            .await
            .unwrap(),
        Reply::Expired { .. }
    ));
    let recovered = second.admit(start(temporary.path(), true)).unwrap();
    recovered.begin(CancellationToken::new()).await;
    recovered
        .end(
            ExecutionObservationEnd {
                begin_completed: true,
                controlled_work: ControlledWorkStatus::Unsettled,
            },
            CancellationToken::new(),
        )
        .await;
    let summary = second.state.lock().unwrap().entries[&recovered.id]
        .summary
        .clone();
    assert_eq!(summary.phase, Phase::Partial);
    assert!(
        summary
            .omissions
            .iter()
            .any(|o| o.kind == OmissionKind::RecoveredWithoutBaseline)
    );
    assert!(
        summary
            .omissions
            .iter()
            .any(|o| o.kind == OmissionKind::Unsettled)
    );
    second.close().await;
    drop(recovered);
    drop(second);
    assert!(process.dispose().await.is_clean());
    assert!(runtime.shutdown().await.is_clean());
}

#[tokio::test]
async fn shutdown_retains_an_admitted_read_until_its_task_drains() {
    let temporary = tempfile::tempdir().unwrap();
    let runtime = Runtime::default();
    let process = runtime
        .root()
        .apply(
            ResolvedFactory::linked(
                "process",
                "test",
                UpdateMode::Replayable,
                Arc::new(rsi_process_local::ProcessLocalFactory),
            ),
            serde_json::json!({}),
        )
        .await
        .unwrap();
    let domain = Arc::new(Storage {
        spec: DomainSpec {
            id: "review".into(),
            backend: "base".into(),
            version: 1,
            maximum_records: 8192,
            maximum_bytes: 64 * 1024 * 1024,
        },
        rows: Mutex::new(BTreeMap::new()),
        fail: AtomicBool::new(false),
    });
    let owner = owner(&runtime, temporary.path().join("scratch"), domain).await;
    let interval = owner.admit(start(temporary.path(), false)).unwrap();
    let scope = owner.state.lock().unwrap().entries[&interval.id]
        .summary
        .scope
        .clone();
    let request = Request::List { scope, after: None };
    let mut read = Box::pin(owner.read(request.clone(), CancellationToken::new()));
    assert!(futures_util::poll!(read.as_mut()).is_pending());
    assert_eq!(
        owner.tasks.len(),
        1,
        "read is registered before yielding to shutdown"
    );
    let mut closing = Box::pin(owner.close());
    assert!(
        futures_util::poll!(closing.as_mut()).is_pending(),
        "shutdown must join the admitted task"
    );
    closing.await;
    assert!(owner.tasks.is_empty());
    assert!(matches!(read.await, Err(ApiError::ShuttingDown)));
    assert!(matches!(
        owner.read(request, CancellationToken::new()).await,
        Err(ApiError::ShuttingDown)
    ));
    drop(interval);
    drop(owner);
    assert!(process.dispose().await.is_clean());
    assert!(runtime.shutdown().await.is_clean());
}

#[tokio::test]
#[expect(
    clippy::too_many_lines,
    reason = "one causal capture contention and immutable pagination scenario"
)]
async fn admitted_baseline_waits_for_workers_and_diff_pages_reuse_captured_patch() {
    let temporary = tempfile::tempdir().unwrap();
    let workspace = temporary.path().join("workspace");
    std::fs::create_dir(&workspace).unwrap();
    assert!(
        std::process::Command::new("/usr/bin/git")
            .args(["init", "--quiet"])
            .current_dir(&workspace)
            .status()
            .unwrap()
            .success()
    );
    std::fs::write(workspace.join("file.txt"), "before\n").unwrap();
    let runtime = Runtime::default();
    let process = runtime
        .root()
        .apply(
            ResolvedFactory::linked(
                "process",
                "test",
                UpdateMode::Replayable,
                Arc::new(rsi_process_local::ProcessLocalFactory),
            ),
            serde_json::json!({}),
        )
        .await
        .unwrap();
    let domain = Arc::new(Storage {
        spec: DomainSpec {
            id: "review".into(),
            backend: "base".into(),
            version: 1,
            maximum_records: 8192,
            maximum_bytes: 64 * 1024 * 1024,
        },
        rows: Mutex::new(BTreeMap::new()),
        fail: AtomicBool::new(false),
    });
    let owner = owner(&runtime, temporary.path().join("scratch"), domain.clone()).await;
    let interval = owner.admit(start(&workspace, false)).unwrap();
    let held = owner.workers.clone().acquire_many_owned(2).await.unwrap();
    let baseline = tokio::spawn({
        let interval = interval.clone();
        async move { interval.begin(CancellationToken::new()).await }
    });
    tokio::time::timeout(Duration::from_secs(2), async {
        while domain.snapshot().await.is_empty() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert!(
        !baseline.is_finished(),
        "worker contention must wait instead of permanently losing the baseline"
    );
    drop(held);
    baseline.await.unwrap();
    std::fs::write(
        workspace.join("file.txt"),
        "after long change line\n".repeat(5000),
    )
    .unwrap();
    interval
        .end(
            ExecutionObservationEnd {
                begin_completed: true,
                controlled_work: ControlledWorkStatus::Settled,
            },
            CancellationToken::new(),
        )
        .await;
    let (summary, ready) = {
        let state = owner.state.lock().unwrap();
        let entry = &state.entries[&interval.id];
        (entry.summary.clone(), entry.runtime.clone().unwrap())
    };
    assert_eq!(summary.phase, Phase::Complete);
    let request = |offset| Request::Diff {
        scope: summary.scope.clone(),
        id: interval.id.clone(),
        path: "file.txt".into(),
        offset,
    };
    let Reply::Diff {
        next_offset,
        has_more,
        ..
    } = owner
        .read(request(0), CancellationToken::new())
        .await
        .unwrap()
    else {
        panic!("diff")
    };
    assert!(has_more);
    let private = ready.scratch.path().join("repository");
    let hidden = ready.scratch.path().join("repository-hidden");
    std::fs::rename(&private, &hidden).unwrap();
    let next = owner
        .read(request(next_offset), CancellationToken::new())
        .await;
    std::fs::rename(hidden, private).unwrap();
    let Reply::Diff { offset, text, .. } = next.unwrap() else {
        panic!("cached diff")
    };
    assert_eq!(offset, next_offset);
    assert!(!text.is_empty());
    drop(ready);
    drop(interval);
    owner.close().await;
    drop(owner);
    assert!(process.dispose().await.is_clean());
    assert!(runtime.shutdown().await.is_clean());
}

#[tokio::test]
async fn late_begin_cannot_resurrect_a_finished_interval() {
    let temporary = tempfile::tempdir().unwrap();
    let runtime = Runtime::default();
    let process = runtime
        .root()
        .apply(
            ResolvedFactory::linked(
                "process",
                "test",
                UpdateMode::Replayable,
                Arc::new(rsi_process_local::ProcessLocalFactory),
            ),
            serde_json::json!({}),
        )
        .await
        .unwrap();
    let domain = Arc::new(Storage {
        spec: DomainSpec {
            id: "review".into(),
            backend: "base".into(),
            version: 1,
            maximum_records: 8192,
            maximum_bytes: 64 * 1024 * 1024,
        },
        rows: Mutex::default(),
        fail: AtomicBool::new(false),
    });
    let owner = owner(&runtime, temporary.path().join("scratch"), domain.clone()).await;
    let interval = owner.admit(start(temporary.path(), false)).unwrap();
    interval
        .end(
            ExecutionObservationEnd {
                begin_completed: false,
                controlled_work: ControlledWorkStatus::Settled,
            },
            CancellationToken::new(),
        )
        .await;
    let finished = domain.snapshot().await;
    interval.begin(CancellationToken::new()).await;
    assert!(
        !owner.state.lock().unwrap().entries[&interval.id].active,
        "late begin must not consume a resident slot again"
    );
    assert_eq!(
        domain.snapshot().await,
        finished,
        "finished evidence must not be overwritten"
    );
    assert!(interval.state.lock().await.scratch.is_none());
    owner.close().await;
    drop(interval);
    drop(owner);
    assert!(process.dispose().await.is_clean());
    assert!(runtime.shutdown().await.is_clean());
}
