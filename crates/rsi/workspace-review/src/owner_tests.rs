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
    fenced: AtomicBool,
}
impl Storage {
    fn ensure_available(&self) -> std::result::Result<(), StorageError> {
        if self.fenced.load(Ordering::Acquire) {
            Err(StorageError::RecoveryRequired)
        } else {
            Ok(())
        }
    }
    fn snapshot(&self) -> std::result::Result<BTreeMap<String, serde_json::Value>, StorageError> {
        self.ensure_available()?;
        Ok(self.rows.lock().unwrap().clone())
    }
    async fn open(self: &Arc<Self>, runtime: &Runtime) -> Domain {
        if runtime
            .root()
            .lookup_local::<rsi_storage_domain::DomainFacilityContract>()
            .is_none()
        {
            for (name, factory) in [
                (
                    "storage",
                    Arc::new(rsi_storage::StorageFactory) as Arc<dyn rsi_meta::PluginFactory>,
                ),
                ("domains", Arc::new(rsi_storage_domain::DomainFactory)),
            ] {
                runtime
                    .root()
                    .apply(
                        ResolvedFactory::linked(name, "test", UpdateMode::Replayable, factory),
                        serde_json::Value::Null,
                    )
                    .await
                    .unwrap();
            }
        }
        let route = format!("review-{:p}", Arc::as_ptr(self));
        if runtime
            .root()
            .lookup_local::<rsi_storage::StorageHubContract>()
            .unwrap()
            .resolve(&route)
            .is_err()
        {
            let factory =
                rsi_storage_testkit::BackendFactory::new(&route, Arc::new(Backend(self.clone())));
            let fiber = runtime
                .root()
                .apply(
                    ResolvedFactory::linked(
                        route.as_str(),
                        "test",
                        UpdateMode::Replayable,
                        Arc::new(factory),
                    ),
                    serde_json::Value::Null,
                )
                .await
                .unwrap();
            assert_eq!(fiber.snapshot().state, rsi_meta::FiberState::Active);
        }
        let mut spec = self.spec.clone();
        spec.backend = route;
        runtime
            .root()
            .lookup_local::<rsi_storage_domain::DomainFacilityContract>()
            .unwrap()
            .open(
                spec,
                rsi_storage_domain::Schema::default()
                    .table::<Summary>("summaries")
                    .unwrap(),
            )
            .await
            .unwrap()
    }
}
#[derive(Debug)]
struct Backend(Arc<Storage>);
#[derive(Debug)]
struct Unit(Arc<Storage>, rsi_storage::UnitDescriptor);
#[async_trait]
impl rsi_storage::KvBackend for Backend {
    fn ensure_available(&self) -> std::result::Result<(), StorageError> {
        self.0.ensure_available()
    }
    fn capabilities(&self, _: rsi_storage::Layout) -> rsi_storage::Capabilities {
        rsi_storage::Capabilities {
            atomic_batch: true,
            backup_record: false,
        }
    }
    async fn open(
        &self,
        descriptor: rsi_storage::UnitDescriptor,
    ) -> std::result::Result<rsi_storage::OpenedUnit, StorageError> {
        self.ensure_available()?;
        descriptor.admit_version(self.0.spec.version)?;
        let records = self
            .0
            .rows
            .lock()
            .unwrap()
            .iter()
            .map(|(key, value)| {
                (
                    rsi_storage::Address::record("summaries", key).unwrap(),
                    rsi_storage::EncodedValue::encode(value).unwrap(),
                )
            })
            .collect();
        Ok(rsi_storage::OpenedUnit {
            snapshot: rsi_storage::UnitSnapshot {
                version: Some(self.0.spec.version),
                records,
                recoveries: vec![],
            },
            unit: Arc::new(Unit(self.0.clone(), descriptor)),
        })
    }
}
#[async_trait]
impl rsi_storage::KvUnit for Unit {
    fn descriptor(&self) -> &rsi_storage::UnitDescriptor {
        &self.1
    }
    fn ensure_available(&self) -> std::result::Result<(), StorageError> {
        self.0.ensure_available()
    }
    async fn write(
        &self,
        changes: Vec<rsi_storage::Mutation>,
    ) -> std::result::Result<(), StorageError> {
        self.ensure_available()?;
        for change in changes {
            assert_eq!(change.address.table(), "summaries");
            let value = change.value.expect("review never deletes summaries");
            self.0
                .rows
                .lock()
                .unwrap()
                .insert(change.address.key().into(), value.decode()?);
        }
        if self.0.fail.load(Ordering::SeqCst) {
            self.0.fenced.store(true, Ordering::Release);
            Err(StorageError::OutcomeUnknown(
                "acknowledgement lost after write".into(),
            ))
        } else {
            Ok(())
        }
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
        domain.open(runtime).await,
        Root::open(path).await.unwrap(),
        git,
        runtime.execution().clone(),
    )
    .unwrap()
}
fn start(path: &std::path::Path, recovered: bool) -> ExecutionObservationStart {
    ExecutionObservationStart {
        header: SessionHeader::new_local(
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
        execution: None,
        turn: TurnId::new("turn").unwrap(),
        claim: 1,
        accepted_seq: 1,
        live_seq: 2,
        recovered,
    }
}
#[tokio::test]
async fn full_durable_history_is_preserved_and_reports_omission_after_restart() {
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
            version: 2,
            maximum_records: 8192,
            maximum_bytes: 64 * 1024 * 1024,
            ..DomainSpec::default()
        },
        rows: Mutex::new(BTreeMap::new()),
        fail: AtomicBool::new(false),
        fenced: AtomicBool::new(false),
    });
    let path = temporary.path().join("scratch");
    let initial = owner(&runtime, path.clone(), domain.clone()).await;
    let interval = initial
        .admit(start(temporary.path(), false))
        .unwrap()
        .unwrap();
    let mut template = initial.state.lock().unwrap().entries[&interval.id]
        .summary
        .clone();
    template.phase = Phase::Complete;
    template.finished_ms = Some(template.started_ms.clone());
    initial.close().await;
    drop(interval);
    drop(initial);
    seed_history(&domain, &mut template);
    let original = domain.rows.lock().unwrap().clone();
    for _ in 0..2 {
        let current = owner(&runtime, path.clone(), domain.clone()).await;
        assert!(matches!(
            Observer(current.clone())
                .observe(start(temporary.path(), false), CancellationToken::new())
                .await
                .unwrap(),
            ExecutionObservationAdmission::OmittedCapacity
        ));
        let denied = start(temporary.path(), false);
        // Backend recovery remains a hard refusal even when history is full.
        domain.fenced.store(true, Ordering::Release);
        assert!(
            Observer(current.clone())
                .observe(denied, CancellationToken::new())
                .await
                .is_err()
        );
        domain.fenced.store(false, Ordering::Release);
        let mut after = None;
        let mut count = 0;
        loop {
            let Reply::Summaries {
                items,
                next,
                capacity_limited,
                ..
            } = current
                .read(
                    Request::List {
                        scope: template.scope.clone(),
                        after,
                    },
                    ExecutionOperation::new(()),
                    CancellationToken::new(),
                )
                .await
                .unwrap()
            else {
                panic!("expected summaries")
            };
            assert!(capacity_limited);
            count += items.len();
            after = next;
            if after.is_none() {
                break;
            }
        }
        assert_eq!(count, 8192);
        assert_eq!(*domain.rows.lock().unwrap(), original);
        current.close().await;
        drop(current);
    }
    assert!(process.dispose().await.is_clean());
    assert!(runtime.shutdown().await.is_clean());
}

fn seed_history(domain: &Storage, template: &mut Summary) {
    let mut rows = domain.rows.lock().unwrap();
    for index in 0..8192 {
        template.id = format!("{index:032x}");
        rows.insert(
            template.id.clone(),
            serde_json::to_value(&*template).unwrap(),
        );
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
            version: 2,
            maximum_records: 8192,
            maximum_bytes: 64 * 1024 * 1024,
            ..DomainSpec::default()
        },
        rows: Mutex::new(BTreeMap::new()),
        fail: AtomicBool::new(true),
        fenced: AtomicBool::new(false),
    });
    let path = temporary.path().join("scratch");
    let first = owner(&runtime, path.clone(), domain.clone()).await;
    let interval = first
        .admit(start(temporary.path(), false))
        .unwrap()
        .unwrap();
    let id = interval.id.clone();
    interval.begin(CancellationToken::new()).await;
    assert!(domain.snapshot().unwrap().is_empty());
    interval.first_tool(CancellationToken::new()).await;
    let summary = first.state.lock().unwrap().entries[&id].summary.clone();
    assert_eq!(
        domain.rows.lock().unwrap().len(),
        1,
        "test storage wrote before losing its acknowledgement"
    );
    let request = Request::List {
        scope: summary.scope.clone(),
        after: None,
    };
    assert!(matches!(
        first
            .read(
                request.clone(),
                ExecutionOperation::new(()),
                CancellationToken::new()
            )
            .await,
        Err(ApiError::Unavailable)
    ));
    assert!(matches!(
        first.admit(start(temporary.path(), false)),
        Err(ApiError::Unavailable)
    ));
    assert_eq!(
        std::fs::read_dir(&path).unwrap().count(),
        1,
        "the non-Git baseline requires no private repository"
    );
    first.close().await;
    drop(interval);
    drop(first);
    let recovered_domain = Arc::new(Storage {
        spec: domain.spec.clone(),
        rows: Mutex::new(domain.rows.lock().unwrap().clone()),
        fail: AtomicBool::new(false),
        fenced: AtomicBool::new(false),
    });
    let second = owner(&runtime, path, recovered_domain).await;
    let Reply::Summaries { epoch, items, .. } = second
        .read(
            request,
            ExecutionOperation::new(()),
            CancellationToken::new(),
        )
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
                ExecutionOperation::new(()),
                CancellationToken::new()
            )
            .await
            .unwrap(),
        Reply::Expired { .. }
    ));
    let recovered = second
        .admit(start(temporary.path(), true))
        .unwrap()
        .unwrap();
    recovered.begin(CancellationToken::new()).await;
    recovered.first_tool(CancellationToken::new()).await;
    assert_eq!(
        std::fs::read_dir(&second.root.path).unwrap().count(),
        1,
        "a non-Git baseline needs only the scratch owner's lock file"
    );
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
        summary.omissions.iter().any(|row| row.kind == OmissionKind::NonGit && row.count == 2),
        "both inventories report the unavailable repository"
    );
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
            version: 2,
            maximum_records: 8192,
            maximum_bytes: 64 * 1024 * 1024,
            ..DomainSpec::default()
        },
        rows: Mutex::new(BTreeMap::new()),
        fail: AtomicBool::new(false),
        fenced: AtomicBool::new(false),
    });
    let owner = owner(&runtime, temporary.path().join("scratch"), domain).await;
    let interval = owner
        .admit(start(temporary.path(), false))
        .unwrap()
        .unwrap();
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
            version: 2,
            maximum_records: 8192,
            maximum_bytes: 64 * 1024 * 1024,
            ..DomainSpec::default()
        },
        rows: Mutex::new(BTreeMap::new()),
        fail: AtomicBool::new(false),
        fenced: AtomicBool::new(false),
    });
    let owner = owner(&runtime, temporary.path().join("scratch"), domain.clone()).await;
    let interval = owner.admit(start(&workspace, false)).unwrap().unwrap();
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
    assert!(domain.snapshot().unwrap().is_empty());
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
    assert_eq!(domain.snapshot().unwrap().len(), 1);
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
        (summary.changed_files, summary.added_lines, summary.removed_lines),
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
    let Ready::Changes(ready) = ready else { panic!("changed file has captured evidence"); };
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
            version: 2,
            maximum_records: 8192,
            maximum_bytes: 64 * 1024 * 1024,
            ..DomainSpec::default()
        },
        rows: Mutex::default(),
        fail: AtomicBool::new(false),
        fenced: AtomicBool::new(false),
    });
    let owner = owner(&runtime, temporary.path().join("scratch"), domain.clone()).await;
    let interval = owner
        .admit(start(temporary.path(), false))
        .unwrap()
        .unwrap();
    interval
        .end(
            ExecutionObservationEnd {
                begin_completed: false,
                controlled_work: ControlledWorkStatus::Settled,
            },
            CancellationToken::new(),
        )
        .await;
    assert!(domain.snapshot().unwrap().is_empty());
    interval.begin(CancellationToken::new()).await;
    interval.first_tool(CancellationToken::new()).await;
    assert!(
        !owner.state.lock().unwrap().entries.contains_key(&interval.id),
        "late begin must not consume a resident slot again"
    );
    assert!(domain.snapshot().unwrap().is_empty());
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
            version: 2,
            maximum_records: 8192,
            maximum_bytes: 64 * 1024 * 1024,
            ..DomainSpec::default()
        },
        rows: Mutex::default(),
        fail: AtomicBool::new(false),
        fenced: AtomicBool::new(false),
    });
    let owner = owner(&runtime, temporary.path().join("scratch"), domain.clone()).await;
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
    assert!(domain.rows.lock().unwrap().is_empty());
    assert!(owner.state.lock().unwrap().entries.is_empty());
    assert_eq!(std::fs::read_dir(&owner.root.path).unwrap().count(), 1);
    gate.revoked.store(false, Ordering::SeqCst);
    let interval = owner
        .admit(remote)
        .expect("admission performs no source I/O")
        .unwrap();
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
