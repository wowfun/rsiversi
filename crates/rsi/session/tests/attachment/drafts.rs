use super::*;
use rsi_session_protocol::SessionIngress as _;

fn device(byte: u8) -> rsi_api_protocol::CallOrigin {
    rsi_api_protocol::CallOrigin::Device(rsi_api_protocol::AuthenticatedDevice {
        id: rsi_api_protocol::DeviceId::from_bytes([byte; 16]),
        revoked: CancellationToken::new(),
    })
}

#[tokio::test(start_paused = true)]
async fn device_slots_cover_creating_and_ready_drafts_and_retries_share_the_first_owner() {
    let directory = tempfile::tempdir().unwrap();
    let preparation = Preparation::new(true);
    let service = service(directory.path(), preparation.clone());
    let mut waiters = Vec::new();
    for index in 0..64 {
        let input = request(directory.path(), format!("device-draft-{index}"));
        let service = service.clone();
        waiters.push(tokio::spawn(async move {
            service.create_from(input, device(1)).await
        }));
        preparation.entered.acquire().await.unwrap().forget();
    }
    assert_eq!(preparation.calls.load(Ordering::SeqCst), 64);
    assert!(matches!(
        service
            .create_from(request(directory.path(), "over-device"), device(1))
            .await,
        Err(SessionError::Capacity)
    ));
    for waiter in waiters {
        waiter.abort();
        assert!(waiter.await.unwrap_err().is_cancelled());
    }
    assert!(matches!(
        service
            .create_from(request(directory.path(), "still-full"), device(1))
            .await,
        Err(SessionError::Capacity)
    ));
    let first = request(directory.path(), "device-draft-0");
    let peer = tokio::spawn({
        let service = service.clone();
        let first = first.clone();
        async move { service.create_from(first, device(2)).await }
    });
    let separate = tokio::spawn({
        let service = service.clone();
        let input = request(directory.path(), "other-device");
        async move { service.create_from(input, device(2)).await }
    });
    preparation.entered.acquire().await.unwrap().forget();
    assert_eq!(preparation.calls.load(Ordering::SeqCst), 65);
    preparation.release.as_ref().unwrap().add_permits(65);
    let shared = peer.await.unwrap().unwrap();
    separate.await.unwrap().unwrap();
    assert!(Arc::ptr_eq(
        &shared,
        &service.create_from(first.clone(), device(1)).await.unwrap()
    ));
    assert!(Arc::ptr_eq(
        &shared,
        &service.create(first.clone()).await.unwrap()
    ));
    let mut conflict = first;
    conflict.workspace_trust = WorkspaceTrust::Trusted;
    assert!(matches!(
        service.create_from(conflict, device(2)).await,
        Err(SessionError::DraftConflict { .. })
    ));
    assert!(matches!(
        service
            .create_from(request(directory.path(), "ready-full"), device(1))
            .await,
        Err(SessionError::Capacity)
    ));
    tokio::time::advance(std::time::Duration::from_mins(61)).await;
    tokio::task::yield_now().await;
    assert_eq!(preparation.leases.load(Ordering::SeqCst), 0);
    assert!(matches!(
        shared.header().await,
        Err(SessionError::NotFound(_))
    ));
    preparation.release.as_ref().unwrap().add_permits(1);
    service
        .create_from(request(directory.path(), "after-device-expiry"), device(1))
        .await
        .unwrap();
    assert_eq!(preparation.calls.load(Ordering::SeqCst), 66);
    service.stop().await;
    assert_eq!(preparation.leases.load(Ordering::SeqCst), 0);
}

#[derive(Debug)]
struct Preparation {
    calls: AtomicUsize,
    leases: Arc<AtomicUsize>,
    entered: Semaphore,
    release: Option<Semaphore>,
}

impl Preparation {
    fn new(blocked: bool) -> Arc<Self> {
        Arc::new(Self {
            calls: AtomicUsize::new(0),
            leases: Arc::new(AtomicUsize::new(0)),
            entered: Semaphore::new(0),
            release: blocked.then(|| Semaphore::new(0)),
        })
    }
}

#[async_trait]
impl AgentComposition for Preparation {
    async fn default_preset_id(&self) -> Result<AgentPresetId, AgentCompositionError> {
        Ok(AgentPresetId::new("tracked").unwrap())
    }
    async fn pin(
        &self,
        preset: &AgentPresetId,
    ) -> Result<AgentCompositionPin, AgentCompositionError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.entered.add_permits(1);
        if let Some(release) = &self.release {
            release.acquire().await.unwrap().forget();
        }
        PinTracker(self.leases.clone()).pin(preset).await
    }
}

fn service(path: &Path, preparation: Arc<dyn AgentComposition>) -> Arc<LocalSessionService> {
    Arc::new(LocalSessionService::new(
        rsi_meta::Execution::native(tokio::runtime::Handle::current()),
        Arc::new(UnavailableCommands),
        Arc::new(UnavailableProjections),
        Arc::new(UnavailableTurns::default()),
        Arc::new(MemoryStore::new()),
        preparation,
        Arc::new(AvailableWorkspace::at(path)),
        Arc::new(TextSettings),
        Arc::new(AvailableLanguage),
        Arc::new(UnavailableImage),
        Arc::new(UnavailableMedia),
        Arc::new(NoApprovalControl),
    ))
}

fn request(_path: &Path, id: impl Into<String>) -> CreateSession {
    CreateSession {
        workspace_id: workspace_id(),
        session_id: SessionId::new(id).unwrap(),
        agent_preset_id: None,
        workspace_trust: WorkspaceTrust::Untrusted,
    }
}

#[tokio::test]
async fn concurrent_creates_keep_one_preparation_after_the_first_waiter_is_dropped() {
    let directory = tempfile::tempdir().unwrap();
    let preparation = Preparation::new(true);
    let service = service(directory.path(), preparation.clone());
    let request = request(directory.path(), "concurrent-draft");
    let first = tokio::spawn({
        let service = service.clone();
        let request = request.clone();
        async move { service.create(request).await }
    });
    preparation.entered.acquire().await.unwrap().forget();
    let mut peers = Vec::new();
    for _ in 0..32 {
        peers.push(tokio::spawn({
            let service = service.clone();
            let request = request.clone();
            async move { service.create(request).await }
        }));
    }
    first.abort();
    assert!(first.await.unwrap_err().is_cancelled());
    preparation.release.as_ref().unwrap().add_permits(33);
    let expected = service.create(request.clone()).await.unwrap();
    for peer in peers {
        assert!(Arc::ptr_eq(&expected, &peer.await.unwrap().unwrap()));
    }
    assert_eq!(preparation.calls.load(Ordering::SeqCst), 1);
    assert_eq!(preparation.leases.load(Ordering::SeqCst), 1);
    assert_eq!(
        service
            .attach(&request.session_id)
            .await
            .unwrap()
            .header()
            .await
            .unwrap(),
        expected.header().await.unwrap()
    );
    service.stop().await;
    assert_eq!(preparation.leases.load(Ordering::SeqCst), 0);
    assert!(expected.header().await.is_err());
}

#[tokio::test(start_paused = true)]
async fn capacity_precedes_preparation_and_idle_sweep_releases_pins_held_by_local_handles() {
    let directory = tempfile::tempdir().unwrap();
    let preparation = Preparation::new(false);
    let service = service(directory.path(), preparation.clone());
    let mut first = None;
    for index in 0..1024 {
        let handle = service
            .create(request(directory.path(), format!("draft-{index}")))
            .await
            .unwrap();
        if index == 0 {
            first = Some(handle);
        }
    }
    assert_eq!(preparation.leases.load(Ordering::SeqCst), 1024);
    assert!(matches!(
        service.create(request(directory.path(), "overflow")).await,
        Err(SessionError::Capacity)
    ));
    assert_eq!(preparation.calls.load(Ordering::SeqCst), 1024);
    let first = first.unwrap();
    tokio::time::advance(std::time::Duration::from_mins(59)).await;
    first.header().await.unwrap();
    tokio::time::advance(std::time::Duration::from_mins(2)).await;
    tokio::task::yield_now().await;
    assert_eq!(
        preparation.leases.load(Ordering::SeqCst),
        1,
        "only the active draft renews"
    );
    let after = service
        .create(request(directory.path(), "after-expiry"))
        .await
        .unwrap();
    assert_eq!(preparation.leases.load(Ordering::SeqCst), 2);
    tokio::time::advance(std::time::Duration::from_mins(61)).await;
    tokio::task::yield_now().await;
    assert_eq!(
        preparation.leases.load(Ordering::SeqCst),
        0,
        "escaped handles cannot retain expired pins"
    );
    assert!(matches!(
        first.header().await,
        Err(SessionError::NotFound(_))
    ));
    assert!(matches!(
        after.header().await,
        Err(SessionError::NotFound(_))
    ));
    service
        .create(request(directory.path(), "draft-0"))
        .await
        .unwrap();
    assert!(
        matches!(first.header().await, Err(SessionError::NotFound(_))),
        "a new lease cannot revive an old handle"
    );
    service.stop().await;
}

#[tokio::test(start_paused = true)]
async fn an_active_operation_owns_its_pin_until_completion_then_gets_a_fresh_idle_lease() {
    let directory = tempfile::tempdir().unwrap();
    let preparation = Preparation::new(false);
    let approvals = Arc::new(TreeApprovals::default());
    let service = LocalSessionService::new(
        rsi_meta::Execution::native(tokio::runtime::Handle::current()),
        Arc::new(UnavailableCommands),
        Arc::new(UnavailableProjections),
        Arc::new(UnavailableTurns::default()),
        Arc::new(MemoryStore::new()),
        preparation.clone(),
        Arc::new(AvailableWorkspace::at(directory.path())),
        Arc::new(TextSettings),
        Arc::new(AvailableLanguage),
        Arc::new(UnavailableImage),
        Arc::new(UnavailableMedia),
        approvals.clone(),
    );
    let handle = service
        .create(request(directory.path(), "active-draft"))
        .await
        .unwrap();
    let gate = approvals.pending.lock().await;
    let mut pending = Box::pin(handle.pending_approvals());
    assert!(futures_util::poll!(&mut pending).is_pending());
    assert_eq!(approvals.reads.load(Ordering::SeqCst), 1);
    tokio::time::advance(std::time::Duration::from_mins(61)).await;
    tokio::task::yield_now().await;
    assert_eq!(preparation.leases.load(Ordering::SeqCst), 1);
    drop(gate);
    pending.await.unwrap();
    tokio::time::advance(std::time::Duration::from_mins(59)).await;
    handle.header().await.unwrap();
    service.stop().await;
    assert_eq!(preparation.leases.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn failed_preparation_releases_its_reserved_slot_for_the_next_attempt() {
    let directory = tempfile::tempdir().unwrap();
    let service = service(directory.path(), Arc::new(FailingComposition));
    for index in 0..1025 {
        assert!(matches!(
            service
                .create(request(directory.path(), format!("failed-{index}")))
                .await,
            Err(SessionError::Backend(_))
        ));
    }
    service.stop().await;
}

#[tokio::test]
async fn retirement_cancels_owned_preparation_and_rejects_later_admission() {
    let directory = tempfile::tempdir().unwrap();
    let preparation = Preparation::new(true);
    let service = service(directory.path(), preparation.clone());
    let request = request(directory.path(), "retiring-draft");
    let waiter = tokio::spawn({
        let service = service.clone();
        let request = request.clone();
        async move { service.create(request).await }
    });
    preparation.entered.acquire().await.unwrap().forget();
    service.stop().await;
    assert!(matches!(
        waiter.await.unwrap(),
        Err(SessionError::ShuttingDown)
    ));
    assert!(matches!(
        service.create(request).await,
        Err(SessionError::ShuttingDown)
    ));
    assert_eq!(preparation.leases.load(Ordering::SeqCst), 0);
}

#[tokio::test(start_paused = true)]
async fn hung_preparation_expires_its_waiters_and_releases_device_capacity() {
    let directory = tempfile::tempdir().unwrap();
    let preparation = Preparation::new(true);
    let service = service(directory.path(), preparation.clone());
    let mut waiters = Vec::new();
    for index in 0..64 {
        let service = service.clone();
        let input = request(directory.path(), format!("hung-{index}"));
        waiters.push(tokio::spawn(async move {
            service.create_from(input, device(1)).await
        }));
        preparation.entered.acquire().await.unwrap().forget();
    }
    assert!(matches!(
        service
            .create_from(request(directory.path(), "full"), device(1))
            .await,
        Err(SessionError::Capacity)
    ));
    tokio::time::advance(std::time::Duration::from_mins(61)).await;
    for waiter in waiters {
        let result = tokio::time::timeout(std::time::Duration::from_secs(1), waiter)
            .await
            .expect("creation must expire")
            .unwrap();
        assert!(matches!(result, Err(SessionError::Backend(error)) if error.contains("deadline")));
    }
    preparation.release.as_ref().unwrap().add_permits(1);
    service
        .create_from(request(directory.path(), "hung-0"), device(1))
        .await
        .unwrap();
    assert_eq!(preparation.calls.load(Ordering::SeqCst), 65);
    service.stop().await;
    assert_eq!(preparation.leases.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn fresh_reads_reconcile_competing_publications_without_submitting() {
    for conflicting in [false, true] {
        for first_read in ["header", "history", "attach"] {
            let directory = tempfile::tempdir().unwrap();
            let store = Arc::new(MemoryStore::new());
            let preparation = Preparation::new(false);
            let service = LocalSessionService::new(
                rsi_meta::Execution::native(tokio::runtime::Handle::current()),
                Arc::new(UnavailableCommands),
                Arc::new(UnavailableProjections),
                Arc::new(UnavailableTurns::default()),
                store.clone(),
                preparation.clone(),
                Arc::new(AvailableWorkspace::at(directory.path())),
                Arc::new(TextSettings),
                Arc::new(AvailableLanguage),
                Arc::new(UnavailableImage),
                Arc::new(UnavailableMedia),
                Arc::new(NoApprovalControl),
            );
            let handle = service
                .create(request(directory.path(), "published-elsewhere"))
                .await
                .unwrap();
            let draft = handle.header().await.unwrap();
            assert!(
                handle
                    .history_before(None, 128)
                    .await
                    .unwrap()
                    .facts
                    .is_empty()
            );
            let durable = publish_competing(store.as_ref(), &draft, conflicting).await;
            match first_read {
                "header" => {
                    let result = handle.header().await;
                    if conflicting {
                        assert!(matches!(result, Err(SessionError::NotFound(_))));
                    } else {
                        assert_eq!(result.unwrap(), durable);
                    }
                }
                "history" => {
                    let result = handle.history_before(None, 128).await;
                    if conflicting {
                        assert!(matches!(result, Err(SessionError::NotFound(_))));
                    } else {
                        assert_eq!(result.unwrap().facts.len(), 1);
                    }
                }
                "attach" => {
                    assert_eq!(
                        service
                            .attach(draft.session_id())
                            .await
                            .unwrap()
                            .header()
                            .await
                            .unwrap(),
                        durable
                    );
                }
                _ => unreachable!(),
            }
            assert_eq!(preparation.leases.load(Ordering::SeqCst), 0);
            if conflicting {
                assert!(matches!(
                    handle.header().await,
                    Err(SessionError::NotFound(_))
                ));
                assert!(matches!(
                    handle.history_before(None, 128).await,
                    Err(SessionError::NotFound(_))
                ));
            }
            let attached = service.attach(draft.session_id()).await.unwrap();
            assert_eq!(attached.header().await.unwrap(), durable);
            assert_eq!(
                attached
                    .history_before(None, 128)
                    .await
                    .unwrap()
                    .facts
                    .len(),
                1
            );
            service.stop().await;
        }
    }
}

#[tokio::test]
async fn stop_during_fresh_publication_keeps_the_active_lease_until_io_finishes() {
    let directory = tempfile::tempdir().unwrap();
    let preparation = Preparation::new(false);
    let gate = Arc::new(Semaphore::new(0));
    let workspace = Arc::new(RejectingWorkspace {
        gate: Some(gate.clone()),
        ..Default::default()
    });
    let service = LocalSessionService::new(
        rsi_meta::Execution::native(tokio::runtime::Handle::current()),
        Arc::new(UnavailableCommands),
        Arc::new(UnavailableProjections),
        Arc::new(UnavailableTurns::default()),
        Arc::new(MemoryStore::new()),
        preparation.clone(),
        workspace.clone(),
        Arc::new(TextSettings),
        Arc::new(AvailableLanguage),
        Arc::new(UnavailableImage),
        Arc::new(UnavailableMedia),
        Arc::new(NoApprovalControl),
    );
    let handle = service
        .create(request(directory.path(), "stop-active"))
        .await
        .unwrap();
    let work = tokio::spawn(submit_text(handle.clone(), "blocked", "input"));
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        while workspace.registrations.load(Ordering::Acquire) == 0 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    service.stop().await;
    assert_eq!(preparation.leases.load(Ordering::SeqCst), 1);
    assert!(matches!(
        handle.header().await,
        Err(SessionError::ShuttingDown)
    ));
    assert!(matches!(
        submit_text(handle.clone(), "new", "rejected").await,
        Err(SessionError::ShuttingDown)
    ));
    gate.add_permits(1);
    assert!(matches!(work.await.unwrap(), Err(SessionError::Backend(_))));
    assert_eq!(preparation.leases.load(Ordering::SeqCst), 0);
    assert!(handle.header().await.is_err());
}

async fn publish_competing(
    store: &dyn SessionStore,
    draft: &SessionHeader,
    conflicting: bool,
) -> SessionHeader {
    let durable = SessionHeader::new(
        draft.session_id().clone(),
        draft.created_at_ms() + u64::from(conflicting),
        draft.canonical_cwd(),
        draft.agent_preset_id().clone(),
        draft.settings().clone(),
    )
    .unwrap()
    .with_workspace_trust(draft.workspace_trust())
    .unwrap();
    store
        .append(AppendBatch {
            session_id: durable.session_id().clone(),
            expected_seq: 0,
            header: Some(durable.clone()),
            facts: vec![
                SessionFact::new(
                    1,
                    1,
                    SessionFactBody::TurnAccepted {
                        turn_id: TurnId::new("competing").unwrap(),
                        text: "durable truth".into(),
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
    durable
}
