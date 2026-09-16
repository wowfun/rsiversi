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
}
#[async_trait]
impl SessionResourceReader for Reader {
    async fn read(
        &self,
        header: &SessionHeader,
        _: Option<&str>,
        cancellation: CancellationToken,
    ) -> ContributionResult<SessionResourceValue> {
        match self {
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
