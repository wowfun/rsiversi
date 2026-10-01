use super::*;
use rsi_agent_composition_protocol::{
    ContributionCatalog, ContributionError, ContributionKind, ContributionRegistration,
    ContributionResult, DomainCatalog, SessionResourceAdapter, SessionResourceReader,
};
use rsi_agent_session_protocol::{ContributionId, SessionResourceRequest, SessionResourceValue};

#[derive(Debug)]
enum Reader {
    Header,
    Panic,
    Pending(Arc<Mutex<Option<CancellationToken>>>),
    Execution(
        rsi_execution::ExecutionBinding,
        Arc<std::sync::atomic::AtomicUsize>,
    ),
}
#[async_trait]
impl SessionResourceReader for Reader {
    async fn read(
        &self,
        header: &SessionHeader,
        execution: Option<&rsi_execution::ExecutionLease>,
        _: Option<&str>,
        cancellation: CancellationToken,
    ) -> ContributionResult<SessionResourceValue> {
        match self {
            Self::Execution(expected, calls) => {
                assert_eq!(execution.unwrap().binding(), expected);
                calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                Ok(SessionResourceValue::List { entries: vec![] })
            }
            Self::Header => Ok(SessionResourceValue::Sources {
                sources: vec![ContributionId::new(header.session_id().as_str()).unwrap()],
            }),
            Self::Panic => panic!("injected resource failure"),
            Self::Pending(saved) => {
                *saved.lock().unwrap() = Some(cancellation);
                std::future::pending().await
            }
        }
    }
}
async fn adapter(
    reader: Reader,
) -> (
    rsi_meta::Runtime,
    rsi_meta::RegistrationLease,
    SessionResourceAdapter,
) {
    let runtime = rsi_meta::Runtime::default();
    let (_, context) = rsi_agent_testkit::activate_contribution_owner(&runtime.root())
        .await
        .unwrap();
    let (position, lease) = context
        .registration_context()
        .unwrap()
        .register("resource fixture", || Ok(()), Ok)
        .unwrap();
    let catalog = ContributionCatalog::freeze(vec![(
        ContributionRegistration::new(
            ContributionId::new("fixture.reader").unwrap(),
            0,
            ContributionKind::ResourceRead(Arc::new(reader)),
        ),
        position,
    )])
    .unwrap();
    let pin = AgentCompositionPin::new(
        AgentPresetId::new("alpha").unwrap(),
        "a".repeat(64),
        Arc::new(EmptyTools),
        Arc::new(rsi_agent_context::DefaultContextBuilder::default()),
        DomainCatalog::default(),
        catalog,
        Arc::new(GenerationOwner),
    )
    .unwrap();
    (runtime, lease, SessionResourceAdapter::new(pin))
}
fn request() -> SessionResourceRequest {
    SessionResourceRequest::List {
        source: ContributionId::new("fixture.reader").unwrap(),
    }
}

#[tokio::test(start_paused = true)]
async fn finite_reads_fence_header_response_shape_panic_and_timeout() {
    for reader in [Reader::Header, Reader::Panic] {
        let (runtime, lease, adapter) = adapter(reader).await;
        let context = Arc::new(header("alpha"));
        let result = adapter
            .read(
                context.clone(),
                None,
                SessionResourceRequest::Sources.validated().unwrap(),
                runtime.execution(),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert_eq!(result.response().session_id, *context.session_id());
        assert!(
            adapter
                .read(
                    context,
                    None,
                    request().validated().unwrap(),
                    runtime.execution(),
                    CancellationToken::new()
                )
                .await
                .is_err()
        );
        assert!(
            adapter
                .read(
                    Arc::new(header("beta")),
                    None,
                    SessionResourceRequest::Sources.validated().unwrap(),
                    runtime.execution(),
                    CancellationToken::new()
                )
                .await
                .is_err()
        );
        drop(adapter);
        drop(lease);
        assert!(runtime.shutdown().await.is_clean());
    }
    let saved = Arc::new(Mutex::new(None));
    let (runtime, lease, adapter) = adapter(Reader::Pending(saved.clone())).await;
    assert!(
        adapter
            .read(
                Arc::new(header("alpha")),
                None,
                request().validated().unwrap(),
                runtime.execution(),
                CancellationToken::new()
            )
            .await
            .is_err()
    );
    assert!(saved.lock().unwrap().as_ref().unwrap().is_cancelled());
    let stop = CancellationToken::new();
    stop.cancel();
    assert!(matches!(
        adapter
            .read(
                Arc::new(header("alpha")),
                None,
                SessionResourceRequest::Sources.validated().unwrap(),
                runtime.execution(),
                stop
            )
            .await,
        Err(ContributionError::Closed)
    ));
    drop(adapter);
    drop(lease);
    assert!(runtime.shutdown().await.is_clean());
}

#[tokio::test]
async fn resource_reads_require_current_matching_execution_before_reader_io() {
    use super::execution_fixture::{Gate, lease};
    use rsi_execution::{ExecutionCoordinates, ExecutionLocation, ExecutionTargetId};
    use std::sync::atomic::{AtomicUsize, Ordering};
    let location = ExecutionLocation::Ssh {
        target: ExecutionTargetId::parse("a".repeat(32)).unwrap(),
    };
    let gate = Arc::new(Gate::default());
    let execution = lease(location.clone(), gate.clone(), 1);
    let calls = Arc::new(AtomicUsize::new(0));
    let (runtime, registration, adapter) = adapter(Reader::Execution(
        execution.binding().clone(),
        calls.clone(),
    ))
    .await;
    let local = header("alpha");
    let remote = Arc::new(
        SessionHeader::new(
            local.session_id().clone(),
            1,
            ExecutionCoordinates::new(location, "/remote/workspace").unwrap(),
            local.agent_preset_id().clone(),
            local.settings().clone(),
        )
        .unwrap(),
    );
    let read = |lease| {
        adapter.read(
            remote.clone(),
            lease,
            request().validated().unwrap(),
            runtime.execution(),
            CancellationToken::new(),
        )
    };
    adapter
        .read(
            remote.clone(),
            None,
            SessionResourceRequest::Sources.validated().unwrap(),
            runtime.execution(),
            CancellationToken::new(),
        )
        .await
        .unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    assert!(read(None).await.is_err());
    assert!(
        read(Some(lease(
            ExecutionLocation::Local,
            Arc::new(Gate::default()),
            0
        )))
        .await
        .is_err()
    );
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    read(Some(execution.clone())).await.unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    gate.revoked.store(true, Ordering::SeqCst);
    assert!(matches!(
        read(Some(execution)).await,
        Err(ContributionError::Closed)
    ));
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    drop(adapter);
    drop(registration);
    assert!(runtime.shutdown().await.is_clean());
}
