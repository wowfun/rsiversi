use super::*;
use rsi_agent_session_protocol::{AgentControlRecord, AgentControlRecordBody, TurnOutcome};
use rsi_agent_store_protocol::{AtomicAgentCommit, AtomicSessionAppend, PurgeRequest, PurgeTarget};
use rsi_session_protocol::SessionIngress as _;
use rsi_session_protocol::{SessionReads, SessionTarget};
#[derive(Debug)]
struct RemovedExecution;
impl rsi_execution::ExecutionResolver for RemovedExecution {
    fn admit(
        &self,
        _: &rsi_api_protocol::CallOrigin,
        _: &rsi_execution::ExecutionLocation,
    ) -> rsi_api_protocol::Result<rsi_execution::ExecutionOperation> {
        Err(rsi_api_protocol::ApiError::Unavailable)
    }
    fn lease(
        &self,
        _: rsi_api_protocol::CallOrigin,
        _: &rsi_execution::ExecutionLocation,
    ) -> rsi_api_protocol::Result<rsi_execution::ExecutionLease> {
        Err(rsi_api_protocol::ApiError::Unavailable)
    }
    fn visibility(
        &self,
        _: &rsi_api_protocol::CallOrigin,
    ) -> rsi_api_protocol::Result<rsi_execution::ExecutionVisibility> {
        Err(rsi_api_protocol::ApiError::Unavailable)
    }
}
#[tokio::test]
async fn purge_cancels_soft_reads_but_waits_for_actual_ownership_and_fences_old_handles() {
    purge_scenario(0).await;
}
#[tokio::test]
async fn purge_drain_timeout_is_a_known_refusal_before_the_response_deadline() {
    purge_scenario(1).await;
}
#[tokio::test]
async fn uncertain_purge_without_commit_reconciles_absence_and_allows_retry() {
    purge_scenario(2).await;
}
#[tokio::test]
async fn uncertain_purge_with_commit_reconciles_retirement_and_resumes() {
    purge_scenario(3).await;
}
#[tokio::test]
async fn purge_timeout_retry_observes_inflight_work_without_readmitting_it() {
    purge_scenario(4).await;
}
#[expect(
    clippy::too_many_lines,
    reason = "One retirement scenario covers read drain, durable commit and old-handle fencing."
)]
async fn purge_scenario(scenario: u8) {
    let directory = tempfile::tempdir().unwrap();
    let store = Arc::new(
        rsi_agent_store_sqlite::SqliteStore::open(directory.path().join("store")).unwrap(),
    );
    let header = SessionHeader::new_local(
        SessionId::new("purge-service").unwrap(),
        1,
        directory
            .path()
            .canonicalize()
            .unwrap()
            .to_string_lossy()
            .into_owned(),
        AgentPresetId::new("image-preset").unwrap(),
        test_settings(),
    )
    .unwrap();
    let turn = TurnId::new("completed").unwrap();
    store
        .commit_agent(AtomicAgentCommit {
            sessions: vec![AtomicSessionAppend {
                session_id: header.session_id().clone(),
                expected_fact_seq: 0,
                expected_control_seq: 0,
                header: Some(header.clone()),
                facts: vec![
                    Arc::new(
                        SessionFact::new(
                            1,
                            1,
                            SessionFactBody::TurnAccepted {
                                turn_id: turn.clone(),
                                text: "fixture".into(),
                                model: None,
                                reasoning_effort: None,
                                sandbox: SandboxMode::WorkspaceWrite,
                                require_approval: false,
                            },
                        )
                        .unwrap(),
                    ),
                    Arc::new(
                        SessionFact::new(
                            2,
                            2,
                            SessionFactBody::TurnTerminal {
                                turn_id: turn.clone(),
                                outcome: TurnOutcome::Completed,
                                result: None,
                            },
                        )
                        .unwrap(),
                    ),
                ],
                controls: vec![
                    AgentControlRecord::new(
                        1,
                        2,
                        AgentControlRecordBody::TurnBoundaryRecorded {
                            turn_id: turn,
                            terminal_fact_seq: 2,
                        },
                    )
                    .unwrap(),
                ],
            }],
            required_active_activations: vec![],
            quiescent_descendants_of: None,
        })
        .await
        .unwrap();
    let composition = Arc::new(AvailableComposition);
    let kernel = Arc::new(
        rsi_agent_kernel::AgentKernel::recover(store.clone(), composition.clone())
            .await
            .unwrap(),
    );
    let entered = Arc::new(tokio::sync::Semaphore::new(0));
    let service = Arc::new(LocalSessionService::new(
        rsi_meta::Execution::native(tokio::runtime::Handle::current()),
        Arc::new(UnavailableCommands),
        Arc::new(UnavailableProjections),
        Arc::new(UnavailableTurns {
            retirement: Some(kernel.clone()),
            workflow_read_entered: Some(entered.clone()),
            ..Default::default()
        }),
        store.clone(),
        rsi_agent_context::ContextBudget::default(),
        composition,
        Arc::new(AvailableWorkspace::at(directory.path())),
        Arc::new(TextSettings),
        Arc::new(UnavailableLanguage),
        Arc::new(UnavailableImage),
        Arc::new(UnavailableMedia),
        Arc::new(NoApprovalControl),
    ));
    let device = service.scoped(rsi_api_protocol::CallOrigin::Device(
        rsi_api_protocol::AuthenticatedDevice {
            id: rsi_api_protocol::DeviceId::from_bytes([7; 16]),
            revoked: tokio_util::sync::CancellationToken::new(),
        },
    ));
    assert!(matches!(
        device.preview_cas_gc(1).await,
        Err(SessionError::Api(rsi_api_protocol::ApiError::Unauthorized))
    ));
    assert!(matches!(
        device.cas_gc_status("private-operation").await,
        Err(SessionError::Api(rsi_api_protocol::ApiError::Unauthorized))
    ));
    let handle = service.attach(header.session_id()).await.unwrap();
    use futures_util::StreamExt as _;
    let mut exporting = handle.export(Default::default()).await.unwrap();
    assert!(exporting.next().await.unwrap().is_ok());
    let listing = tokio::spawn({
        let handle = handle.clone();
        async move {
            handle
                .list_workflows(rsi_session_protocol::WorkflowList {
                    cursor: None,
                    limit: 8,
                })
                .await
        }
    });
    let detail = tokio::spawn({
        let handle = handle.clone();
        async move {
            handle
                .read_workflow(rsi_session_protocol::WorkflowRead {
                    run_id: rsi_agent_session_protocol::ProgramRunId::new("pending-read").unwrap(),
                    expected_control_seq: None,
                    children_offset: 0,
                    result_offset: None,
                    script_offset: None,
                })
                .await
        }
    });
    tokio::time::timeout(std::time::Duration::from_secs(5), entered.acquire_many(2))
        .await
        .unwrap()
        .unwrap()
        .forget();
    let read = service
        .acquire(
            rsi_api_protocol::CallOrigin::Local,
            &SessionTarget {
                session_id: header.session_id().clone(),
                header_key: header.fingerprint().unwrap(),
            },
        )
        .await
        .unwrap();
    let request = PurgeRequest {
        operation_id: rsi_agent_store_protocol::MaintenanceOperationId::new("service-purge")
            .unwrap(),
        targets: vec![PurgeTarget {
            session_id: header.session_id().clone(),
            header_sha256: header.fingerprint().unwrap(),
            fact_seq: 2,
            control_seq: 1,
        }],
    };
    assert!(
        service
            .preview_purge(request.clone())
            .await
            .unwrap()
            .blockers
            .is_empty()
    );
    // A later busy target must not be attributed to the first selected Session.
    let first = SessionHeader::new_local(
        SessionId::new("purge-first").unwrap(),
        1,
        directory
            .path()
            .canonicalize()
            .unwrap()
            .to_string_lossy()
            .into_owned(),
        AgentPresetId::new("image-preset").unwrap(),
        test_settings(),
    )
    .unwrap();
    store
        .commit_agent(AtomicAgentCommit {
            sessions: vec![AtomicSessionAppend {
                session_id: first.session_id().clone(),
                expected_fact_seq: 0,
                expected_control_seq: 0,
                header: Some(first.clone()),
                facts: store
                    .read_facts(header.session_id(), 0, 2)
                    .await
                    .unwrap()
                    .facts
                    .into_iter()
                    .map(Arc::new)
                    .collect(),
                controls: store
                    .read_controls(header.session_id(), 0, 1)
                    .await
                    .unwrap()
                    .records,
            }],
            required_active_activations: vec![],
            quiescent_descendants_of: None,
        })
        .await
        .unwrap();
    let busy = kernel
        .prepare_retirement(std::slice::from_ref(header.session_id()))
        .unwrap();
    let mut selected = request.clone();
    selected.targets.insert(
        0,
        PurgeTarget {
            session_id: first.session_id().clone(),
            header_sha256: first.fingerprint().unwrap(),
            fact_seq: 2,
            control_seq: 1,
        },
    );
    let expected = vec![rsi_agent_store_protocol::PurgeBlocker::Busy {
        session_id: header.session_id().clone(),
    }];
    assert_eq!(
        service
            .preview_purge(selected.clone())
            .await
            .unwrap()
            .blockers,
        expected
    );
    assert_eq!(
        service.purge(selected).await.unwrap().unwrap_err().blockers,
        expected
    );
    assert!(!read.retiring().is_cancelled());
    // A rejected selection releases earlier reservations.
    drop(
        kernel
            .prepare_retirement(std::slice::from_ref(first.session_id()))
            .unwrap(),
    );
    drop(busy);
    if matches!(scenario, 2 | 3) {
        store.fail_next_purge_admission(scenario == 3);
    }
    let pause = (scenario == 4).then(|| store.pause_next_purge_admission());
    let purging = tokio::spawn({
        let service = service.clone();
        let request = request.clone();
        async move { service.purge(request).await }
    });
    tokio::time::timeout(
        std::time::Duration::from_secs(5),
        read.retiring().cancelled(),
    )
    .await
    .unwrap();
    assert!(matches!(
        exporting.next().await,
        Some(Err(SessionError::ShuttingDown))
    ));
    assert!(exporting.next().await.is_none());
    assert!(matches!(
        listing.await.unwrap(),
        Err(SessionError::ShuttingDown)
    ));
    assert!(matches!(
        detail.await.unwrap(),
        Err(SessionError::ShuttingDown)
    ));
    assert!(!purging.is_finished());
    assert!(store.header(header.session_id()).await.is_ok());
    assert!(matches!(
        handle.header().await,
        Err(SessionError::ShuttingDown)
    ));
    if scenario == 1 {
        tokio::time::pause();
        tokio::time::advance(std::time::Duration::from_secs(31)).await;
    }
    let receipt = if let Some((entered, pause)) = pause {
        drop(read);
        entered.await.unwrap();
        tokio::time::pause();
        tokio::time::advance(std::time::Duration::from_secs(61)).await;
        assert!(matches!(
            purging.await.unwrap(),
            Err(SessionError::MaintenanceOutcomeUnknown { .. })
        ));
        tokio::time::resume();
        assert!(matches!(
            service.purge(request.clone()).await,
            Err(SessionError::MaintenanceOutcomeUnknown { .. })
        ));
        let mut other = request.clone();
        other.operation_id =
            rsi_agent_store_protocol::MaintenanceOperationId::new("different-operation").unwrap();
        assert!(matches!(
            service.purge(other).await,
            Err(SessionError::Capacity)
        ));
        drop(pause);
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            loop {
                match service.purge(request.clone()).await {
                    Ok(Ok(receipt)) if receipt.store.complete => break receipt,
                    Ok(Ok(_))
                    | Err(
                        SessionError::MaintenanceOutcomeUnknown { .. } | SessionError::Capacity,
                    ) => tokio::task::yield_now().await,
                    other => panic!("unexpected reconciliation: {other:?}"),
                }
            }
        })
        .await
        .unwrap()
    } else if scenario == 0 {
        drop(read);
        purging.await.unwrap().unwrap().unwrap()
    } else {
        if scenario == 1 {
            assert!(matches!(
                purging.await.unwrap(),
                Err(SessionError::Capacity)
            ));
            tokio::time::resume();
            drop(read);
        } else {
            drop(read);
            assert!(matches!(
                purging.await.unwrap(),
                Err(SessionError::MaintenanceOutcomeUnknown { .. })
            ));
            assert!(
                service.attach(header.session_id()).await.is_err(),
                "unreadable admission retains its owners"
            );
        }
        if scenario >= 2 {
            let revoked = tokio_util::sync::CancellationToken::new();
            revoked.cancel();
            let denied = service.scoped(rsi_api_protocol::CallOrigin::Device(
                rsi_api_protocol::AuthenticatedDevice {
                    id: rsi_api_protocol::DeviceId::from_bytes([8; 16]),
                    revoked,
                },
            ));
            assert!(matches!(
                denied.purge_status("unrelated-operation").await,
                Err(SessionError::Api(rsi_api_protocol::ApiError::Unauthorized))
            ));
            let removed = service
                .as_ref()
                .clone()
                .with_execution(Arc::new(RemovedExecution));
            assert_eq!(
                removed.purge_status("unrelated-operation").await.unwrap(),
                None
            );
            if scenario == 3 {
                assert!(matches!(
                    removed.purge_status("service-purge").await,
                    Err(SessionError::Api(rsi_api_protocol::ApiError::Unavailable))
                ));
            }
        }
        let status = service.purge_status("service-purge").await.unwrap();
        assert_eq!(status.is_some(), scenario == 3);
        if scenario != 3 {
            assert!(
                service.attach(header.session_id()).await.is_ok(),
                "confirmed absence reopens fresh handles"
            );
            assert!(
                handle.header().await.is_err(),
                "the old handle remains fenced"
            );
        }
        service.purge(request.clone()).await.unwrap().unwrap()
    };
    assert!(receipt.store.complete);
    assert_eq!(
        receipt.navigation,
        rsi_session_protocol::NavigationCleanup::Unavailable
    );
    assert!(service.attach(header.session_id()).await.is_err());
    assert_eq!(
        service.purge_status("service-purge").await.unwrap(),
        Some(receipt.clone())
    );
    assert_eq!(service.purge(request).await.unwrap().unwrap(), receipt);
    service.stop().await.unwrap();
}
