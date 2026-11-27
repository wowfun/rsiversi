use super::*;
use rsi_agent_session_protocol::{
    AgentPresetId, FrozenAgentSettings, SessionHeader, SessionId, TurnId,
};
use rsi_agent_session_protocol::{CancelReason, TurnOutcome};
use rsi_agent_turn_protocol::{
    CancelResult, PreparedResumeSession, Result as TurnResult, SubmitImage, SubmitTurn,
    SubmittedTurn, TurnObservation,
};
use rsi_meta::{ResolvedFactory, Runtime, UpdateMode};
use std::sync::atomic::Ordering;

#[derive(Default, Debug)]
struct Markers(Mutex<Vec<(SessionId, TurnId)>>);
#[async_trait]
impl TurnService for Markers {
    async fn record_workspace_changes(&self, session: &SessionId, turn: &TurnId) -> TurnResult<()> {
        self.0.lock().unwrap().push((session.clone(), turn.clone()));
        Ok(())
    }
    async fn prepare_resume(&self, _: &SessionId) -> TurnResult<PreparedResumeSession> {
        unreachable!()
    }
    async fn submit(&self, _: SubmitTurn) -> TurnResult<SubmittedTurn> {
        unreachable!()
    }
    async fn submit_image(&self, _: SubmitImage) -> TurnResult<SubmittedTurn> {
        unreachable!()
    }
    async fn cancel(
        &self,
        _: &SessionId,
        _: &TurnId,
        _: Option<CancelReason>,
    ) -> TurnResult<CancelResult> {
        unreachable!()
    }
    async fn observe(&self, _: &SessionId, _: u64) -> TurnResult<TurnObservation> {
        unreachable!()
    }
    async fn outcome(&self, _: &SessionId, _: &TurnId) -> TurnResult<Option<TurnOutcome>> {
        unreachable!()
    }
    async fn session_header(&self, _: &SessionId) -> TurnResult<SessionHeader> {
        unreachable!()
    }
}
async fn install_process(runtime: &Runtime) -> rsi_meta::FiberHandle {
    runtime
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
        .unwrap()
}
async fn owner(runtime: &Runtime, path: PathBuf, markers: Arc<Markers>) -> Arc<WorkspaceReview> {
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
        markers,
        Root::open(path).await.unwrap(),
        git,
        runtime.execution().clone(),
    )
}
fn start(path: &std::path::Path, recovered: bool) -> ExecutionObservationStart {
    session_start(path, recovered, "review-source")
}
fn session_start(
    path: &std::path::Path,
    recovered: bool,
    session: &str,
) -> ExecutionObservationStart {
    ExecutionObservationStart {
        header: SessionHeader::new_local(
            SessionId::new(session).unwrap(),
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
        execution: None,
        turn: TurnId::new("turn").unwrap(),
        claim: 1,
        accepted_seq: 1,
        live_seq: 2,
        recovered,
    }
}
#[tokio::test]
async fn independent_sessions_retain_their_diffs_until_scratch_is_needed() {
    let temporary = tempfile::tempdir().unwrap();
    let runtime = Runtime::default();
    let process = install_process(&runtime).await;
    let owner = owner(&runtime, temporary.path().join("scratch"), Arc::default()).await;
    let mut intervals = Vec::new();
    let stop = CancellationToken::new();
    for i in 0..9 {
        let session = format!("review-{i}");
        let workspace = temporary.path().join(&session);
        assert!(
            std::process::Command::new("/usr/bin/git")
                .args(["init", "--quiet"])
                .arg(&workspace)
                .status()
                .unwrap()
                .success()
        );
        std::fs::write(workspace.join("note.txt"), "before\n").unwrap();
        let interval = owner
            .admit(session_start(&workspace, false, &session))
            .unwrap();
        interval.begin(stop.clone()).await;
        intervals.push(interval);
    }
    let end = ExecutionObservationEnd {
        begin_completed: true,
        controlled_work: ControlledWorkStatus::Settled,
    };
    for (i, interval) in intervals.iter().enumerate() {
        interval.first_tool(stop.clone()).await;
        std::fs::write(interval.workspace.join("note.txt"), format!("after{i}\n")).unwrap();
        interval.end(end, stop.clone()).await;
    }
    for (i, interval) in intervals.iter().enumerate() {
        let summary = owner.state.lock().unwrap().entries[&interval.id]
            .summary
            .clone();
        assert_eq!((summary.phase, summary.changed_files), (Phase::Complete, 1));
        let reply = owner
            .read_inner(
                &Request::Diff {
                    scope: summary.scope,
                    id: interval.id.clone(),
                    path: "note.txt".into(),
                    offset: 0,
                },
                &stop,
            )
            .await
            .unwrap();
        assert!(
            matches!(reply, Reply::Diff { text, .. } if text.contains("-before\n") && text.contains(&format!("+after{i}\n")))
        );
    }
    // A live reader of the oldest comparison keeps its files even under pressure.
    let reader = owner.state.lock().unwrap().entries[&intervals[0].id]
        .runtime
        .clone()
        .unwrap();
    let quota = &owner.git.quota;
    let held = quota
        .clone()
        .try_acquire_many_owned(u32::try_from(quota.available_permits()).unwrap())
        .unwrap();
    let next = owner
        .admit(session_start(&intervals[0].workspace, false, "next"))
        .unwrap();
    next.begin(stop.clone()).await;
    assert!(
        next.state
            .lock()
            .await
            .before
            .as_ref()
            .unwrap()
            .0
            .directory
            .is_some()
    );
    {
        let state = owner.state.lock().unwrap();
        assert!(state.entries[&intervals[0].id].runtime.is_some());
        assert!(!state.entries.contains_key(&intervals[1].id));
        assert!(
            intervals[2..]
                .iter()
                .all(|interval| state.entries[&interval.id].runtime.is_some())
        );
    }
    next.end(end, stop).await;
    drop((reader, held));
    owner.close().await;
    assert!(process.dispose().await.is_clean());
    assert!(runtime.shutdown().await.is_clean());
}

#[tokio::test]
async fn shutdown_retains_an_admitted_read_until_its_task_drains() {
    let temporary = tempfile::tempdir().unwrap();
    let runtime = Runtime::default();
    let process = install_process(&runtime).await;
    let markers = Arc::new(Markers::default());
    let owner = owner(&runtime, temporary.path().join("scratch"), markers).await;
    let interval = owner.admit(start(temporary.path(), false)).unwrap();
    let scope = owner.state.lock().unwrap().entries[&interval.id]
        .summary
        .scope
        .clone();
    let request = Request::List { scope, after: None };
    let mut read = Box::pin(owner.read(
        request.clone(),
        ExecutionOperation::new(()),
        CancellationToken::new(),
    ));
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
        owner
            .read(
                request,
                ExecutionOperation::new(()),
                CancellationToken::new()
            )
            .await,
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
    let runtime = Runtime::default();
    let process = install_process(&runtime).await;
    let markers = Arc::new(Markers::default());
    let owner = owner(&runtime, temporary.path().join("scratch"), markers.clone()).await;
    let interval = owner.admit(start(&workspace, false)).unwrap();
    let held = owner.workers.clone().acquire_many_owned(2).await.unwrap();
    let baseline = tokio::spawn({
        let interval = interval.clone();
        async move { interval.begin(CancellationToken::new()).await }
    });
    tokio::time::timeout(Duration::from_secs(2), async {
        while interval.state.try_lock().is_ok() {
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
    assert!(markers.0.lock().unwrap().is_empty());
    let scope = owner.state.lock().unwrap().entries[&interval.id]
        .summary
        .scope
        .clone();
    let request = Request::List { scope, after: None };
    assert!(matches!(
        owner.read_inner(&request, &CancellationToken::new()).await.unwrap(),
        Reply::Summaries { items, .. } if items.is_empty()
    ));
    interval.first_tool(CancellationToken::new()).await;
    assert!(markers.0.lock().unwrap().is_empty());
    assert!(matches!(
        owner.read_inner(&request, &CancellationToken::new()).await.unwrap(),
        Reply::Summaries { items, .. } if items.len() == 1 && items[0].phase == Phase::Pending
    ));
    assert_eq!(std::fs::read_dir(&owner.root.path).unwrap().count(), 1);
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
    assert_eq!(
        (
            summary.changed_files,
            summary.added_lines,
            summary.removed_lines
        ),
        (1, 5000, 0)
    );
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
        .read(
            request(0),
            ExecutionOperation::new(()),
            CancellationToken::new(),
        )
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
        .read(
            request(next_offset),
            ExecutionOperation::new(()),
            CancellationToken::new(),
        )
        .await;
    std::fs::rename(hidden, private).unwrap();
    let Reply::Diff { offset, text, .. } = next.unwrap() else {
        panic!("cached diff")
    };
    assert_eq!(offset, next_offset);
    assert!(!text.is_empty());
    drop(ready);
    drop(interval);
    let unchanged = owner
        .admit(session_start(&workspace, false, "read-only"))
        .unwrap();
    let stop = CancellationToken::new();
    unchanged.begin(stop.clone()).await;
    unchanged.first_tool(stop.clone()).await;
    assert!(
        std::fs::read_to_string(workspace.join("file.txt"))
            .unwrap()
            .starts_with("after")
    );
    unchanged
        .end(
            ExecutionObservationEnd {
                begin_completed: true,
                controlled_work: ControlledWorkStatus::Settled,
            },
            stop,
        )
        .await;
    assert!(
        !owner
            .state
            .lock()
            .unwrap()
            .entries
            .contains_key(&unchanged.id)
    );
    assert_eq!(
        *markers.0.lock().unwrap(),
        vec![(
            SessionId::new("review-source").unwrap(),
            TurnId::new("turn").unwrap()
        )]
    );
    drop(unchanged);
    owner.close().await;
    drop(owner);
    assert!(process.dispose().await.is_clean());
    assert!(runtime.shutdown().await.is_clean());
}

#[tokio::test]
async fn late_begin_cannot_resurrect_a_finished_interval() {
    let temporary = tempfile::tempdir().unwrap();
    let runtime = Runtime::default();
    let process = install_process(&runtime).await;
    let markers = Arc::new(Markers::default());
    let owner = owner(&runtime, temporary.path().join("scratch"), markers.clone()).await;
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
    assert!(markers.0.lock().unwrap().is_empty());
    interval.begin(CancellationToken::new()).await;
    interval.first_tool(CancellationToken::new()).await;
    assert!(
        !owner
            .state
            .lock()
            .unwrap()
            .entries
            .contains_key(&interval.id),
        "late begin must not consume a resident slot again"
    );
    assert!(markers.0.lock().unwrap().is_empty());
    assert!(interval.state.lock().await.before.is_none());
    owner.close().await;
    drop(interval);
    drop(owner);
    assert!(process.dispose().await.is_clean());
    assert!(runtime.shutdown().await.is_clean());
}

#[path = "../../../../fixtures/rsi/execution/metadata.rs"]
#[allow(dead_code)]
mod metadata;

#[tokio::test]
async fn observation_rejects_missing_wrong_or_revoked_execution_before_state_or_io() {
    let temporary = tempfile::tempdir().unwrap();
    let runtime = Runtime::default();
    let process = install_process(&runtime).await;
    let markers = Arc::new(Markers::default());
    let owner = owner(&runtime, temporary.path().join("scratch"), markers.clone()).await;
    let location = ExecutionLocation::Ssh {
        target: rsi_execution::ExecutionTargetId::parse("b".repeat(32)).unwrap(),
    };
    let mut remote = start(temporary.path(), false);
    remote.header = SessionHeader::new(
        remote.header.session_id().clone(),
        1,
        rsi_execution::ExecutionCoordinates::new(
            location.clone(),
            temporary.path().to_str().unwrap(),
        )
        .unwrap(),
        remote.header.agent_preset_id().clone(),
        remote.header.settings().clone(),
    )
    .unwrap();
    assert!(matches!(
        owner.admit(remote.clone()),
        Err(ApiError::Unauthorized)
    ));
    remote.execution = Some(metadata::lease(ExecutionLocation::Local, Arc::default(), 1));
    assert!(matches!(
        owner.admit(remote.clone()),
        Err(ApiError::Unauthorized)
    ));
    let gate = Arc::new(metadata::Gate::default());
    remote.execution = Some(metadata::lease(location, gate.clone(), 1));
    gate.revoked.store(true, Ordering::SeqCst);
    assert!(matches!(
        owner.admit(remote.clone()),
        Err(ApiError::Unauthorized)
    ));
    assert!(markers.0.lock().unwrap().is_empty());
    assert!(owner.state.lock().unwrap().entries.is_empty());
    assert_eq!(std::fs::read_dir(&owner.root.path).unwrap().count(), 1);
    gate.revoked.store(false, Ordering::SeqCst);
    let interval = owner
        .admit(remote)
        .expect("admission performs no source I/O");
    assert_eq!(gate.active.load(Ordering::SeqCst), 1);
    let retained = interval.clone();
    drop(interval);
    assert_eq!(gate.active.load(Ordering::SeqCst), 1);
    drop(retained);
    assert_eq!(gate.active.load(Ordering::SeqCst), 0);
    owner.close().await;
    drop(owner);
    assert!(process.dispose().await.is_clean());
    assert!(runtime.shutdown().await.is_clean());
}
