use super::*;
use futures_util::StreamExt as _;
use rsi_api_protocol::{ApiError, AuthenticatedDevice, CallOrigin, DeviceId};
use rsi_execution::{ExecutionLease, ExecutionLocation, ExecutionOperation, ExecutionResolver};
use rsi_session_protocol::SessionIngress as _;
use std::sync::atomic::AtomicBool;

#[derive(Debug, Default)]
struct Access {
    granted: AtomicBool,
    slots: Option<Arc<Semaphore>>,
}
impl ExecutionResolver for Access {
    fn visibility(
        &self,
        origin: &rsi_api_protocol::CallOrigin,
    ) -> rsi_api_protocol::Result<rsi_execution::ExecutionVisibility> {
        let operation = self.admit(origin, &ExecutionLocation::Local)?;
        let locations =
            if matches!(origin, CallOrigin::Local) || self.granted.load(Ordering::SeqCst) {
                rsi_execution::ExecutionLocations::all()
            } else {
                rsi_execution::ExecutionLocations::only(std::collections::BTreeSet::from([
                    ExecutionLocation::Local,
                ]))
                .unwrap()
            };
        Ok(rsi_execution::ExecutionVisibility::new(
            locations, operation,
        ))
    }

    fn admit(
        &self,
        origin: &CallOrigin,
        location: &ExecutionLocation,
    ) -> rsi_api_protocol::Result<ExecutionOperation> {
        if let CallOrigin::Device(device) = origin
            && (device.revoked.is_cancelled()
                || (*location != ExecutionLocation::Local && !self.granted.load(Ordering::SeqCst)))
        {
            return Err(ApiError::Unauthorized);
        }
        let permit = self
            .slots
            .as_ref()
            .map(|slots| {
                slots
                    .clone()
                    .try_acquire_owned()
                    .map_err(|_| ApiError::Capacity)
            })
            .transpose()?;
        Ok(ExecutionOperation::new(permit))
    }
    fn lease(
        &self,
        _: CallOrigin,
        _: &ExecutionLocation,
    ) -> rsi_api_protocol::Result<ExecutionLease> {
        panic!("metadata never acquires an execution connection")
    }
}
#[tokio::test]
#[allow(clippy::too_many_lines)] // The same retained handle and stream cross offline access and live revocation.
async fn remote_session_reads_are_offline_caller_bound_and_live_streams_recheck_use() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().canonicalize().unwrap();
    let store = Arc::new(MemoryStore::new());
    let location = ExecutionLocation::Ssh {
        target: rsi_execution::ExecutionTargetId::parse("a".repeat(32)).unwrap(),
    };
    let mut headers = Vec::new();
    for (index, location) in [ExecutionLocation::Local, location].into_iter().enumerate() {
        let header = SessionHeader::new(
            SessionId::new(format!("read-{index}")).unwrap(),
            index as u64 + 1,
            rsi_execution::ExecutionCoordinates::new(location, path.to_str().unwrap()).unwrap(),
            AgentPresetId::new("removed").unwrap(),
            test_settings(),
        )
        .unwrap();
        store
            .append(AppendBatch {
                session_id: header.session_id().clone(),
                expected_seq: 0,
                header: Some(header.clone()),
                facts: vec![
                    SessionFact::new(
                        1,
                        1,
                        SessionFactBody::TurnAccepted {
                            reasoning_effort: None,
                            turn_id: TurnId::new("accepted").unwrap(),
                            text: "private remote history".into(),
                            model: None,
                            sandbox: SandboxMode::WorkspaceWrite,
                            require_approval: false,
                        },
                    )
                    .unwrap()
                    .into(),
                ],
            })
            .await
            .unwrap();
        headers.push(header);
    }
    let slots = Arc::new(Semaphore::new(64));
    let access = Arc::new(Access {
        slots: Some(slots.clone()),
        ..Access::default()
    });
    let service = LocalSessionService::new(
        rsi_meta::Execution::native(tokio::runtime::Handle::current()),
        Arc::new(UnavailableCommands),
        Arc::new(UnavailableProjections),
        Arc::new(UnavailableTurns::default()),
        store,
        rsi_agent_context::ContextBudget::default(),
        Arc::new(UnavailableComposition),
        Arc::new(UnavailableWorkspace),
        Arc::new(UnavailableSettings),
        Arc::new(UnavailableLanguage),
        Arc::new(UnavailableImage),
        Arc::new(UnavailableMedia),
        Arc::new(NoApprovalControl),
    )
    .with_execution(access.clone());
    let revoked = CancellationToken::new();
    let origin = CallOrigin::Device(AuthenticatedDevice {
        id: DeviceId::from_bytes([1; 16]),
        revoked: revoked.clone(),
    });
    let scoped = service.scoped(origin.clone());
    let remote = headers[1].session_id();
    let target = rsi_session_protocol::SessionTarget {
        session_id: remote.clone(),
        header_key: headers[1].fingerprint().unwrap(),
    };
    assert!(matches!(
        rsi_session_protocol::SessionReads::acquire(&service, origin.clone(), &target).await,
        Err(SessionError::Api(ApiError::Unauthorized))
    ));
    assert!(matches!(
        scoped.read_header(remote).await,
        Err(SessionError::Api(ApiError::Unauthorized))
    ));
    assert!(matches!(
        scoped.attach(remote).await,
        Err(SessionError::Api(ApiError::Unauthorized))
    ));
    let visible = scoped.list_recent(None, 1).await.unwrap();
    assert_eq!(visible.sessions[0].header, headers[0]);
    assert!(!visible.has_more);
    access.granted.store(true, Ordering::SeqCst);
    let read = rsi_session_protocol::SessionReads::acquire(&service, origin.clone(), &target)
        .await
        .unwrap();
    assert_eq!(read.header(), &headers[1]);
    drop(read);
    let handle = scoped.attach(remote).await.unwrap();
    assert_eq!(handle.header().await.unwrap(), headers[1]);
    assert_eq!(handle.history_before(None, 8).await.unwrap().facts.len(), 1);
    let mut stream = handle
        .export(rsi_session_protocol::export::ExportOptions::default())
        .await
        .unwrap();
    assert!(stream.next().await.unwrap().is_ok());
    assert_eq!(
        slots.available_permits(),
        64,
        "a paused stream must release its publication permit"
    );
    assert!(scoped.read_header(headers[0].session_id()).await.is_ok());
    access.granted.store(false, Ordering::SeqCst);
    assert!(matches!(
        rsi_session_protocol::SessionReads::acquire(&service, origin, &target).await,
        Err(SessionError::Api(ApiError::Unauthorized))
    ));
    assert!(matches!(
        stream.next().await.unwrap(),
        Err(SessionError::Api(ApiError::Unauthorized))
    ));
    assert!(stream.next().await.is_none());
    assert!(matches!(
        handle.header().await,
        Err(SessionError::Api(ApiError::Unauthorized))
    ));
    assert!(matches!(
        handle.history_before(None, 8).await,
        Err(SessionError::Api(ApiError::Unauthorized))
    ));
    assert!(matches!(
        handle.commands().await,
        Err(SessionError::Api(ApiError::Unauthorized))
    ));
    assert!(matches!(
        handle.pending_approvals().await,
        Err(SessionError::Api(ApiError::Unauthorized))
    ));
    assert!(matches!(
        handle
            .cancel(
                rsi_agent_turn_protocol::CancelTarget::Turn(TurnId::new("accepted").unwrap()),
                None
            )
            .await,
        Err(SessionError::Api(ApiError::Unauthorized))
    ));
    assert!(
        service.read_header(remote).await.is_ok(),
        "Local trust is independent of Device Use"
    );
    revoked.cancel();
    assert!(matches!(
        scoped.list_recent(None, 1).await,
        Err(SessionError::Api(ApiError::Unauthorized))
    ));
    assert!(matches!(
        handle.workflow_readiness().await,
        Err(SessionError::Api(ApiError::Unauthorized))
    ));
    assert!(matches!(
        handle
            .list_workflows(rsi_session_protocol::WorkflowList {
                cursor: None,
                limit: 8,
            })
            .await,
        Err(SessionError::Api(ApiError::Unauthorized))
    ));
    assert!(matches!(
        handle
            .cancel_workflow(
                &rsi_agent_session_protocol::ProgramRunId::new("program-revoked").unwrap()
            )
            .await,
        Err(SessionError::Api(ApiError::Unauthorized))
    ));
    service.stop().await.unwrap();
}

#[allow(dead_code)]
#[path = "../../../../../fixtures/rsi/execution/metadata.rs"]
mod execution_fixture;

#[derive(Debug)]
struct SingleResolution {
    execution: ExecutionLease,
    calls: AtomicUsize,
}
impl ExecutionResolver for SingleResolution {
    fn visibility(
        &self,
        _: &rsi_api_protocol::CallOrigin,
    ) -> rsi_api_protocol::Result<rsi_execution::ExecutionVisibility> {
        panic!("unexpected enumeration in focused resolver fixture")
    }

    fn admit(
        &self,
        _: &CallOrigin,
        _: &ExecutionLocation,
    ) -> rsi_api_protocol::Result<ExecutionOperation> {
        Ok(ExecutionOperation::new(()))
    }
    fn lease(
        &self,
        _: CallOrigin,
        location: &ExecutionLocation,
    ) -> rsi_api_protocol::Result<ExecutionLease> {
        assert_eq!(location, self.execution.binding().location());
        assert_eq!(
            self.calls.fetch_add(1, Ordering::SeqCst),
            0,
            "input must not re-resolve its execution provider"
        );
        Ok(self.execution.clone())
    }
}

#[tokio::test]
async fn input_canonicalization_and_submission_share_one_exact_target_lease() {
    let location = ExecutionLocation::Ssh {
        target: rsi_execution::ExecutionTargetId::parse("b".repeat(32)).unwrap(),
    };
    let path = "/remote-only/not-a-service-workspace";
    let reads = Arc::new(AtomicUsize::new(0));
    let execution = execution_fixture::lease_with_canonical_path(
        location.clone(),
        Arc::new(execution_fixture::Gate::default()),
        path.into(),
        reads.clone(),
    );
    let resolver = Arc::new(SingleResolution {
        execution: execution.clone(),
        calls: AtomicUsize::new(0),
    });
    let store = Arc::new(MemoryStore::new());
    let header = SessionHeader::new(
        SessionId::new("one-execution").unwrap(),
        1,
        rsi_execution::ExecutionCoordinates::new(location, path).unwrap(),
        AgentPresetId::new("image-preset").unwrap(),
        test_settings(),
    )
    .unwrap();
    store
        .append(AppendBatch {
            session_id: header.session_id().clone(),
            expected_seq: 0,
            header: Some(header.clone()),
            facts: vec![
                SessionFact::new(
                    1,
                    1,
                    SessionFactBody::TurnAccepted {
                        reasoning_effort: None,
                        turn_id: TurnId::new("existing").unwrap(),
                        text: "existing".into(),
                        model: None,
                        sandbox: SandboxMode::WorkspaceWrite,
                        require_approval: false,
                    },
                )
                .unwrap()
                .into(),
            ],
        })
        .await
        .unwrap();
    let composition = AvailableComposition
        .pin(header.agent_preset_id(), None)
        .await
        .unwrap();
    let turns = Arc::new(ConcurrentResumeTurns {
        expected_execution: Some(execution),
        header: header.clone(),
        resume_issuer: ResumeAdmissionIssuer::new(),
        composition,
        entered: AtomicUsize::new(0),
        entered_notify: Notify::new(),
        release: Semaphore::new(1),
    });
    let service = LocalSessionService::new(
        rsi_meta::Execution::native(tokio::runtime::Handle::current()),
        Arc::new(UnavailableCommands),
        Arc::new(UnavailableProjections),
        turns,
        store,
        rsi_agent_context::ContextBudget::default(),
        Arc::new(UnavailableComposition),
        Arc::new(UnavailableWorkspace),
        Arc::new(UnavailableSettings),
        Arc::new(AvailableLanguage),
        Arc::new(UnavailableImage),
        Arc::new(UnavailableMedia),
        Arc::new(NoApprovalControl),
    )
    .with_execution(resolver.clone());
    let handle = service.attach(header.session_id()).await.unwrap();
    assert!(
        submit_text(handle, "new-input", "inspect target")
            .await
            .is_ok()
    );
    assert_eq!(reads.load(Ordering::SeqCst), 1);
    assert_eq!(resolver.calls.load(Ordering::SeqCst), 1);
    service.stop().await.unwrap();
}

#[derive(Debug)]
struct RemoteWorkspace(rsi_execution::ExecutionCoordinates);
#[async_trait]
impl WorkspaceRegistry for RemoteWorkspace {
    async fn get(&self, id: &WorkspaceId) -> rsi_workspace_protocol::Result<WorkspaceRecord> {
        assert_eq!(id, &workspace_id());
        Ok(WorkspaceRecord {
            id: id.clone(),
            coordinates: self.0.clone(),
        })
    }
    async fn order_seed(
        &self,
    ) -> rsi_workspace_protocol::Result<rsi_workspace_protocol::WorkspaceOrderSeed> {
        unreachable!()
    }
    async fn list(
        &self,
        _: Option<WorkspaceCursor>,
        _: usize,
    ) -> rsi_workspace_protocol::Result<WorkspacePage> {
        unreachable!()
    }
    async fn register_at(
        &self,
        _: &ExecutionLocation,
        _: &Path,
    ) -> rsi_workspace_protocol::Result<WorkspaceRecord> {
        unreachable!()
    }
    async fn status(&self, _: &WorkspaceId) -> rsi_workspace_protocol::Result<WorkspaceStatus> {
        unreachable!()
    }
    async fn delete_registration(&self, _: &WorkspaceId) -> rsi_workspace_protocol::Result<bool> {
        unreachable!()
    }
}

#[tokio::test]
async fn ssh_draft_requires_current_use_without_resolving_connection_or_local_path() {
    let coordinates = rsi_execution::ExecutionCoordinates::new(
        ExecutionLocation::Ssh {
            target: rsi_execution::ExecutionTargetId::parse("c".repeat(32)).unwrap(),
        },
        "/remote-only/never-read-on-service",
    )
    .unwrap();
    let access = Arc::new(Access::default());
    let store = Arc::new(MemoryStore::new());
    let service = LocalSessionService::new(
        rsi_meta::Execution::native(tokio::runtime::Handle::current()),
        Arc::new(UnavailableCommands),
        Arc::new(UnavailableProjections),
        Arc::new(UnavailableTurns::default()),
        store.clone(),
        rsi_agent_context::ContextBudget::default(),
        Arc::new(AvailableComposition),
        Arc::new(RemoteWorkspace(coordinates.clone())),
        Arc::new(TextSettings),
        Arc::new(UnavailableLanguage),
        Arc::new(UnavailableImage),
        Arc::new(UnavailableMedia),
        Arc::new(NoApprovalControl),
    )
    .with_execution(access.clone());
    let scoped = service.scoped(CallOrigin::Device(AuthenticatedDevice {
        id: DeviceId::from_bytes([8; 16]),
        revoked: CancellationToken::new(),
    }));
    let request = CreateSession {
        workspace_id: workspace_id(),
        session_id: SessionId::new("remote-draft").unwrap(),
        agent_preset_id: Some(AgentPresetId::new("image-preset").unwrap()),
    };
    assert!(matches!(
        scoped.create(request.clone()).await,
        Err(SessionError::Api(ApiError::Unauthorized))
    ));
    access.granted.store(true, Ordering::SeqCst);
    let handle = scoped.create(request.clone()).await.unwrap();
    assert_eq!(handle.header().await.unwrap().coordinates(), &coordinates);
    assert!(matches!(
        store.header(&request.session_id).await,
        Err(rsi_agent_store_protocol::StoreError::NotFound(_))
    ));
    access.granted.store(false, Ordering::SeqCst);
    assert!(matches!(
        scoped.create(request).await,
        Err(SessionError::Api(ApiError::Unauthorized))
    ));
    assert!(matches!(
        handle.header().await,
        Err(SessionError::Api(ApiError::Unauthorized))
    ));
    service.stop().await.unwrap();
}

#[derive(Debug)]
pub(super) struct WorkflowGate {
    pub entered: Semaphore,
    pub release: Semaphore,
    pub panic: std::sync::atomic::AtomicBool,
}
async fn workflow_cancel_fixture(
    protected: bool,
) -> (
    tempfile::TempDir,
    LocalSessionService,
    Arc<WorkflowGate>,
    Arc<Semaphore>,
    Arc<dyn SessionHandle>,
) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().canonicalize().unwrap();
    let store = Arc::new(MemoryStore::new());
    let header = SessionHeader::new(
        SessionId::new("admitted-workflow").unwrap(),
        1,
        rsi_execution::ExecutionCoordinates::new(ExecutionLocation::Local, path.to_str().unwrap())
            .unwrap(),
        AgentPresetId::new("standard").unwrap(),
        test_settings(),
    )
    .unwrap();
    let header = if protected {
        header
            .with_protection(
                rsi_agent_session_protocol::SessionProtectionScope::new("fixture", "workflow")
                    .unwrap(),
            )
            .unwrap()
    } else {
        header
    };
    store
        .append(AppendBatch {
            session_id: header.session_id().clone(),
            expected_seq: 0,
            header: Some(header.clone()),
            facts: vec![
                SessionFact::new(
                    1,
                    1,
                    SessionFactBody::TurnAccepted {
                        turn_id: TurnId::new("accepted").unwrap(),
                        text: "history".into(),
                        model: None,
                        reasoning_effort: None,
                        sandbox: SandboxMode::WorkspaceWrite,
                        require_approval: false,
                    },
                )
                .unwrap()
                .into(),
            ],
        })
        .await
        .unwrap();
    let gate = Arc::new(WorkflowGate {
        panic: std::sync::atomic::AtomicBool::new(false),
        entered: Semaphore::new(0),
        release: Semaphore::new(0),
    });
    let slots = Arc::new(Semaphore::new(1));
    let service = LocalSessionService::new(
        rsi_meta::Execution::native(tokio::runtime::Handle::current()),
        Arc::new(UnavailableCommands),
        Arc::new(UnavailableProjections),
        Arc::new(UnavailableTurns {
            workflow_gate: Some(gate.clone()),
            ..Default::default()
        }),
        store,
        rsi_agent_context::ContextBudget::default(),
        Arc::new(UnavailableComposition),
        Arc::new(UnavailableWorkspace),
        Arc::new(UnavailableSettings),
        Arc::new(UnavailableLanguage),
        Arc::new(UnavailableImage),
        Arc::new(UnavailableMedia),
        Arc::new(NoApprovalControl),
    )
    .with_protection(Arc::new(WorkflowView))
    .with_execution(Arc::new(Access {
        slots: Some(slots.clone()),
        ..Default::default()
    }));
    let handle = service.attach(header.session_id()).await.unwrap();
    (dir, service, gate, slots, handle)
}
#[tokio::test]
async fn dropped_workflow_cancel_waiter_keeps_execution_admission_until_owned_completion() {
    let (_dir, service, gate, slots, handle) = workflow_cancel_fixture(false).await;
    let waiter = tokio::spawn(async move {
        handle
            .cancel_workflow(
                &rsi_agent_session_protocol::ProgramRunId::new("program-admitted").unwrap(),
            )
            .await
    });
    gate.entered.acquire().await.unwrap().forget();
    assert_eq!(slots.available_permits(), 0);
    waiter.abort();
    assert!(waiter.await.unwrap_err().is_cancelled());
    assert_eq!(
        slots.available_permits(),
        0,
        "dropping a waiter cannot release owned mutation admission"
    );
    gate.release.add_permits(1);
    service.stop().await.unwrap();
    assert_eq!(
        slots.available_permits(),
        1,
        "actual completion releases its permit"
    );
}

#[tokio::test(start_paused = true)]
async fn shutdown_reports_incomplete_cancel_without_abandoning_its_admission() {
    let (_dir, service, gate, slots, handle) = workflow_cancel_fixture(false).await;
    let waiter = tokio::spawn(async move {
        handle
            .cancel_workflow(
                &rsi_agent_session_protocol::ProgramRunId::new("program-stalled").unwrap(),
            )
            .await
    });
    gate.entered.acquire().await.unwrap().forget();
    let failure = service.stop().await.unwrap_err();
    assert!(
        matches!(failure, SessionError::Backend(message) if message.contains("cleanup exceeded"))
    );
    assert_eq!(
        slots.available_permits(),
        0,
        "deadline cannot release the running mutation"
    );
    assert!(!waiter.is_finished());
    gate.release.add_permits(1);
    assert!(matches!(
        waiter.await.unwrap().unwrap(),
        rsi_agent_turn_protocol::ProgramCancelReceipt::Accepted { .. }
    ));
    service.stop().await.unwrap();
    assert_eq!(slots.available_permits(), 1);
}

#[tokio::test]
async fn workflow_cancel_panic_preserves_unknown_identity_and_releases_settled_admission() {
    let (_temp, service, gate, slots, handle) = workflow_cancel_fixture(false).await;
    gate.panic.store(true, Ordering::SeqCst);
    let run = rsi_agent_session_protocol::ProgramRunId::new("program-panic").unwrap();
    let requested = run.clone();
    let waiter = tokio::spawn(async move { handle.cancel_workflow(&requested).await });
    gate.entered.acquire().await.unwrap().forget();
    assert_eq!(slots.available_permits(), 0);
    gate.release.add_permits(1);
    match waiter.await.unwrap() {
        Err(SessionError::WorkflowOutcomeUnknown { run: observed, .. }) => {
            assert_eq!(observed, run);
        }
        other => panic!("expected exact uncertain cancellation receipt, got {other:?}"),
    }
    service.stop().await.unwrap();
    assert_eq!(slots.available_permits(), 1);
}

#[derive(Debug)]
struct WorkflowView;
impl rsi_session_protocol::SessionProtection for WorkflowView {
    fn view(
        &self,
        _: &rsi_agent_session_protocol::SessionProtectionScope,
        _: &rsi_api_protocol::CallOrigin,
    ) -> rsi_session_protocol::Result<CancellationToken> {
        Ok(CancellationToken::new())
    }
}

#[derive(Debug)]
struct UnavailableProtection(ApiError);
impl rsi_session_protocol::SessionProtection for UnavailableProtection {
    fn view(
        &self,
        _: &rsi_agent_session_protocol::SessionProtectionScope,
        _: &CallOrigin,
    ) -> rsi_session_protocol::Result<CancellationToken> {
        Err(SessionError::Api(self.0.clone()))
    }
}
#[tokio::test]
async fn recent_protection_infrastructure_errors_are_unavailable() {
    for error in [
        ApiError::Backend("policy read failed".into()),
        ApiError::Capacity,
        ApiError::Unavailable,
    ] {
        let (_dir, service, _gate, _slots, _handle) = workflow_cancel_fixture(true).await;
        let service = service.with_protection(Arc::new(UnavailableProtection(error)));
        assert!(matches!(
            service.list_recent(None, 1).await,
            Err(SessionError::Api(ApiError::Unavailable))
        ));
        service.stop().await.unwrap();
    }
}
#[tokio::test]
async fn protected_target_reference_capture_refuses_before_accessing_a_source() {
    let (_dir, service, _gate, _slots, handle) = workflow_cancel_fixture(true).await;
    assert!(matches!(
        handle
            .capture_reference(SessionId::new("missing-source").unwrap())
            .await,
        Err(SessionError::Api(ApiError::Unauthorized))
    ));
    service.stop().await.unwrap();
}
#[tokio::test]
async fn public_protected_handle_cannot_admit_workflow_cancellation() {
    let (_dir, service, gate, _slots, handle) = workflow_cancel_fixture(true).await;
    gate.release.add_permits(1);
    let result = handle
        .cancel_workflow(&rsi_agent_session_protocol::ProgramRunId::new("protected-run").unwrap())
        .await;
    let entered = gate.entered.available_permits();
    service.stop().await.unwrap();
    assert!(
        matches!(
            result,
            Err(SessionError::Api(rsi_api_protocol::ApiError::Unauthorized))
        ),
        "{result:?}"
    );
    assert_eq!(
        entered, 0,
        "public View authority reached the Kernel mutation"
    );
}
