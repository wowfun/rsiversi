use super::*;
use rsi_agent_store_protocol::{
    CasGcReceipt, CasGcRequest, CasGcTarget, CasObjectRef, MaintenanceOperationId, PurgeBlocker,
    PurgePreview, PurgeReceipt, PurgeRequest, PurgeTarget,
};
use rsi_session_protocol::{NavigationCleanup, SessionIngress, SessionPurgeReceipt};
fn target() -> CasGcTarget {
    CasGcTarget {
        object: CasObjectRef {
            sha256: "a".repeat(64),
            byte_len: 1,
        },
        generation: 1,
    }
}
fn request() -> PurgeRequest {
    PurgeRequest {
        operation_id: MaintenanceOperationId::new("purge").unwrap(),
        targets: vec![PurgeTarget {
            session_id: header().session_id().clone(),
            header_sha256: header().fingerprint().unwrap(),
            fact_seq: 2,
            control_seq: 1,
        }],
    }
}
fn receipt() -> SessionPurgeReceipt {
    SessionPurgeReceipt {
        store: PurgeReceipt {
            operation_id: MaintenanceOperationId::new("purge").unwrap(),
            complete: true,
            deleted_records: 5,
            deleted_payload_bytes: 400,
            free_pages: 2,
            retained_cas_objects: 1,
        },
        navigation: NavigationCleanup::Complete,
    }
}
#[tokio::test]
async fn maintenance_failure_requires_the_original_operation_kind_and_identity() {
    use rsi_session_protocol::MaintenanceKind;
    let remote = Remote::new();
    let client = SessionClient::new(remote.clone());
    for (kind, id) in [
        (MaintenanceKind::Purge, "foreign"),
        (MaintenanceKind::CasGc, "purge"),
    ] {
        remote.domain(&crate::wire::Failure::MaintenanceOutcomeUnknown {
            kind,
            operation_id: MaintenanceOperationId::new(id).unwrap(),
        });
        assert_eq!(
            client.purge(request()).await.unwrap_err(),
            SessionError::Api(ApiError::OutcomeUnknown)
        );
    }
    for (kind, id) in [
        (MaintenanceKind::CasGc, "foreign"),
        (MaintenanceKind::Purge, "collect"),
    ] {
        remote.domain(&crate::wire::Failure::MaintenanceOutcomeUnknown {
            kind,
            operation_id: MaintenanceOperationId::new(id).unwrap(),
        });
        assert_eq!(
            client
                .collect_cas(CasGcRequest {
                    operation_id: MaintenanceOperationId::new("collect").unwrap(),
                    target: target()
                })
                .await
                .unwrap_err(),
            SessionError::Api(ApiError::OutcomeUnknown)
        );
    }
    remote.domain(&crate::wire::Failure::MaintenanceOutcomeUnknown {
        kind: MaintenanceKind::Purge,
        operation_id: MaintenanceOperationId::new("purge").unwrap(),
    });
    assert!(matches!(
        client.purge_status("purge").await,
        Err(SessionError::Api(ApiError::Invalid(_)))
    ));
}
#[tokio::test]
async fn maintenance_rejects_malformed_or_uncorrelated_cas_evidence() {
    let remote = Remote::new();
    let client = SessionClient::new(remote.clone());
    assert!(client.preview_cas_gc(0).await.is_err());
    assert_eq!(remote.calls.load(Ordering::Acquire), 0);
    for bad in [
        CasGcTarget {
            generation: 0,
            ..target()
        },
        CasGcTarget {
            object: CasObjectRef {
                sha256: "invalid".into(),
                byte_len: 1,
            },
            ..target()
        },
        CasGcTarget {
            object: CasObjectRef {
                byte_len: 0,
                ..target().object
            },
            ..target()
        },
    ] {
        remote.reply(&vec![bad]);
        assert!(client.preview_cas_gc(1).await.is_err());
    }
    remote.reply(&vec![target(), target()]);
    assert!(client.preview_cas_gc(1).await.is_err());
    let collect = CasGcRequest {
        operation_id: MaintenanceOperationId::new("collect").unwrap(),
        target: target(),
    };
    for bad in [
        CasGcReceipt {
            operation_id: MaintenanceOperationId::new("foreign").unwrap(),
            target: target(),
            complete: true,
        },
        CasGcReceipt {
            operation_id: MaintenanceOperationId::new("collect").unwrap(),
            target: CasGcTarget {
                generation: 2,
                ..target()
            },
            complete: true,
        },
    ] {
        remote.reply(&bad);
        assert!(matches!(
            client.collect_cas(collect.clone()).await,
            Err(SessionError::Api(ApiError::OutcomeUnknown))
        ));
    }
    for bad in [
        CasGcReceipt {
            operation_id: MaintenanceOperationId::new("foreign").unwrap(),
            target: target(),
            complete: true,
        },
        CasGcReceipt {
            operation_id: MaintenanceOperationId::new("collect").unwrap(),
            target: CasGcTarget {
                generation: 0,
                ..target()
            },
            complete: true,
        },
    ] {
        remote.reply(&Some(bad));
        assert!(client.cas_gc_status("collect").await.is_err());
    }
}

#[tokio::test]
async fn maintenance_rejects_malformed_or_uncorrelated_purge_evidence() {
    let remote = Remote::new();
    let client = SessionClient::new(remote.clone());
    let evidence = PurgePreview {
        blockers: vec![
            PurgeBlocker::Busy {
                session_id: header().session_id().clone(),
            },
            PurgeBlocker::Referenced {
                source: None,
                target: header().session_id().clone(),
            },
            PurgeBlocker::UncertainWork {
                session_id: header().session_id().clone(),
            },
        ],
    };
    remote.reply(&evidence);
    assert_eq!(client.preview_purge(request()).await.unwrap(), evidence);
    for bad in [
        PurgePreview {
            blockers: vec![PurgeBlocker::Busy {
                session_id: SessionId::new("foreign").unwrap(),
            }],
        },
        PurgePreview {
            blockers: vec![
                PurgeBlocker::Busy {
                    session_id: header().session_id().clone()
                };
                3
            ],
        },
    ] {
        remote.reply(&bad);
        assert!(client.preview_purge(request()).await.is_err());
        remote.reply(&Result::<SessionPurgeReceipt, _>::Err(bad));
        assert!(matches!(
            client.purge(request()).await,
            Err(SessionError::Api(ApiError::OutcomeUnknown))
        ));
    }
    let mut foreign = receipt();
    foreign.store.operation_id = MaintenanceOperationId::new("foreign").unwrap();
    remote.reply(&Result::<_, PurgePreview>::Ok(foreign.clone()));
    assert!(matches!(
        client.purge(request()).await,
        Err(SessionError::Api(ApiError::OutcomeUnknown))
    ));
    remote.reply(&Some(foreign));
    assert!(client.purge_status("purge").await.is_err());
}

#[derive(Debug)]
struct Ingress(Arc<SessionClient>);
#[async_trait]
impl SessionIngress for Ingress {
    fn scoped(&self, origin: rsi_api_protocol::CallOrigin) -> Arc<dyn SessionService> {
        assert!(matches!(origin, rsi_api_protocol::CallOrigin::Local));
        self.0.clone()
    }
    async fn create_from(
        &self,
        request: CreateSession,
        origin: rsi_api_protocol::CallOrigin,
    ) -> rsi_session_protocol::Result<Arc<dyn SessionHandle>> {
        self.scoped(origin).create(request).await
    }
}
#[derive(Debug)]
struct Dispatch {
    registry: Arc<rsi_api::ApiRegistry>,
    description: ConnectionDescription,
    operations: Vec<OperationSpec>,
}
#[async_trait]
impl ApiClient for Dispatch {
    fn description(&self) -> &ConnectionDescription {
        &self.description
    }
    async fn operations(&self) -> rsi_api_protocol::Result<rsi_api_protocol::OperationCatalog> {
        rsi_api_protocol::OperationCatalog::new(self.operations.clone())
    }
    fn input_budget(&self, _: OperationClass) -> ByteBudget {
        ByteBudget::default()
    }
    async fn call(
        &self,
        operation: &OperationSpec,
        input: RetainedBytes,
    ) -> rsi_api_protocol::Result<ApiOutput> {
        use rsi_api_protocol::ApiDispatch as _;
        self.registry
            .admit(&operation.call(), rsi_api_protocol::CallOrigin::Local)?
            .invoke(input)
            .await
    }
}
#[tokio::test]
async fn maintenance_and_reference_release_cross_actual_client_server_dispatch() {
    let runtime = rsi_meta::Runtime::default();
    let registry = Arc::new(rsi_api::ApiRegistry::new(runtime.execution().clone()));
    let remote = Remote::new();
    let service = Arc::new(SessionClient::new(remote.clone()));
    let server =
        crate::SessionApi::register(registry.as_ref(), Arc::new(Ingress(service))).unwrap();
    let client = SessionClient::new(Arc::new(Dispatch {
        registry,
        description: remote.description.clone(),
        operations: remote.operations.clone(),
    }));
    for kind in [
        rsi_session_protocol::MaintenanceKind::Purge,
        rsi_session_protocol::MaintenanceKind::CasGc,
    ] {
        let operation_id = match kind {
            rsi_session_protocol::MaintenanceKind::Purge => "purge",
            rsi_session_protocol::MaintenanceKind::CasGc => "collect",
        };
        remote.domain(&crate::wire::Failure::MaintenanceOutcomeUnknown {
            kind,
            operation_id: MaintenanceOperationId::new(operation_id).unwrap(),
        });
        let error = match kind {
            rsi_session_protocol::MaintenanceKind::Purge => {
                client.purge(request()).await.unwrap_err()
            }
            rsi_session_protocol::MaintenanceKind::CasGc => client
                .collect_cas(CasGcRequest {
                    operation_id: MaintenanceOperationId::new(operation_id).unwrap(),
                    target: target(),
                })
                .await
                .unwrap_err(),
        };
        assert_eq!(
            error,
            SessionError::MaintenanceOutcomeUnknown {
                kind,
                operation_id: MaintenanceOperationId::new(operation_id).unwrap()
            }
        );
    }
    remote.reply(&vec![target()]);
    assert_eq!(client.preview_cas_gc(1).await.unwrap(), vec![target()]);
    let gc = CasGcReceipt {
        operation_id: MaintenanceOperationId::new("collect").unwrap(),
        target: target(),
        complete: true,
    };
    remote.reply(&gc);
    assert_eq!(
        client
            .collect_cas(CasGcRequest {
                operation_id: MaintenanceOperationId::new("collect").unwrap(),
                target: target()
            })
            .await
            .unwrap(),
        gc
    );
    remote.reply(&Some(gc.clone()));
    assert_eq!(client.cas_gc_status("collect").await.unwrap(), Some(gc));
    let preview = PurgePreview { blockers: vec![] };
    remote.reply(&preview);
    assert_eq!(client.preview_purge(request()).await.unwrap(), preview);
    remote.reply(&Result::<_, PurgePreview>::Ok(receipt()));
    assert_eq!(client.purge(request()).await.unwrap().unwrap(), receipt());
    remote.reply(&Some(receipt()));
    assert_eq!(client.purge_status("purge").await.unwrap(), Some(receipt()));
    remote.reply(&header());
    let handle = client.attach(header().session_id()).await.unwrap();
    remote.reply(&header());
    remote.reply(&envelope(Value::Null));
    let snapshot = rsi_agent_session_protocol::ReferenceSnapshotRef {
        capture_id: "fixture-capture".into(),
        sha256: "a".repeat(64),
        byte_len: 1,
    };
    handle.release_reference(snapshot.clone()).await.unwrap();
    {
        let requests = remote.requests.lock().unwrap();
        let (operation, value) = requests.last().unwrap();
        assert_eq!(*operation, Operation::ReleaseReference.spec());
        assert_eq!(value["input"], serde_json::to_value(snapshot).unwrap());
        assert_eq!(value["target"]["session_id"], "session");
    }
    server.close().await;
}
