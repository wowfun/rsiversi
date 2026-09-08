use super::*;
use rsi_agent_composition_protocol::{
    ContributionCatalog, ContributionError, ContributionKind, ContributionRegistration,
    ContributionResult, DomainCatalog, SessionProjection, SessionProjectionAdapter,
    SessionProjectionContext,
};
use rsi_agent_session_protocol::{
    ContributionId, DomainIdentity, DomainRevision, DomainSnapshot, DomainStateValue,
    DomainStateView, ProjectionCursor, ProjectionValue,
};

#[derive(Debug)]
enum Unit {
    Value,
    Panic,
    Oversized,
    InvalidDiagnostic,
    Pending(Arc<Mutex<Option<CancellationToken>>>),
}
#[async_trait]
impl SessionProjection for Unit {
    async fn project(
        &self,
        context: &SessionProjectionContext,
        token: CancellationToken,
    ) -> ContributionResult<ProjectionValue> {
        match self {
            Self::Value => ProjectionValue::new(serde_json::json!({"cursor":context.cursor(),"state":context.domains().first().map(|state| state.snapshot.state().value())})).map_err(|error| ContributionError::Invalid(error.to_string())),
            Self::Panic => panic!("injected projection panic"),
            Self::Oversized => ProjectionValue::new(serde_json::json!("x".repeat(64 * 1024))).map_err(|error| ContributionError::Invalid(error.to_string())),
            Self::InvalidDiagnostic => Err(ContributionError::Invalid("\0\u{1b}[31m".into())),
            Self::Pending(saved) => { *saved.lock().unwrap() = Some(token); std::future::pending().await }
        }
    }
}

async fn adapter(
    units: Vec<Unit>,
) -> (
    rsi_meta::Runtime,
    rsi_meta::RegistrationLease,
    SessionProjectionAdapter,
) {
    let runtime = rsi_meta::Runtime::default();
    let (_, context) = rsi_agent_testkit::activate_contribution_owner(&runtime.root())
        .await
        .unwrap();
    let (position, lease) = context
        .registration_context()
        .unwrap()
        .register("projection fixture", || Ok(()), Ok)
        .unwrap();
    let catalog = ContributionCatalog::freeze(
        units
            .into_iter()
            .enumerate()
            .map(|(index, unit)| {
                (
                    ContributionRegistration::new(
                        ContributionId::new(format!("fixture.projection-{index:02}")).unwrap(),
                        0,
                        ContributionKind::Projection(Arc::new(unit)),
                    ),
                    position.clone(),
                )
            })
            .collect(),
    )
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
    (runtime, lease, SessionProjectionAdapter::new(pin))
}

fn capture() -> SessionProjectionContext {
    SessionProjectionContext::new(
        Arc::new(header("alpha")),
        ProjectionCursor::Durable {
            fact_seq: 8,
            control_seq: 5,
        },
        vec![DomainStateView {
            revision: DomainRevision::new(1),
            snapshot: DomainSnapshot::new(
                DomainIdentity::new("fixture.state", 1).unwrap(),
                DomainStateValue::new(true.into()).unwrap(),
            ),
        }]
        .into(),
    )
    .unwrap()
}

#[tokio::test(start_paused = true)]
async fn failures_panic_and_deadline_preserve_other_complete_values_at_the_same_cut() {
    let saved = Arc::new(Mutex::new(None));
    let (runtime, lease, adapter) = adapter(vec![
        Unit::Value,
        Unit::Panic,
        Unit::Oversized,
        Unit::InvalidDiagnostic,
        Unit::Pending(saved.clone()),
        Unit::Value,
    ])
    .await;
    let context = capture();
    let snapshot = adapter
        .snapshot(
            &context,
            &rsi_meta::Execution::native(tokio::runtime::Handle::current()),
            CancellationToken::new(),
        )
        .await
        .unwrap();
    assert_eq!(snapshot.cursor(), context.cursor());
    assert_eq!(
        snapshot.header_sha256(),
        header("alpha").fingerprint().unwrap()
    );
    assert_eq!(snapshot.entries().len(), 6);
    assert_eq!(snapshot.entries()[0].view(), snapshot.entries()[5].view());
    assert_eq!(snapshot.entries()[0].view().unwrap().value()["state"], true);
    for entry in &snapshot.entries()[1..5] {
        assert!(entry.failure().is_some());
    }
    assert_eq!(
        snapshot.entries()[3].failure(),
        Some("projection returned an invalid diagnostic")
    );
    assert!(saved.lock().unwrap().as_ref().unwrap().is_cancelled());
    drop(lease);
    assert!(runtime.shutdown().await.is_clean());
}

#[tokio::test(start_paused = true)]
async fn aggregate_deadline_bounds_many_slow_units_and_rejects_cancelled_capture() {
    let (runtime, lease, adapter) = adapter(
        (0..32)
            .map(|_| Unit::Pending(Arc::new(Mutex::new(None))))
            .collect(),
    )
    .await;
    let execution = rsi_meta::Execution::native(tokio::runtime::Handle::current());
    let before = tokio::time::Instant::now();
    let snapshot = adapter
        .snapshot(&capture(), &execution, CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(snapshot.entries().len(), 32);
    assert!(
        snapshot
            .entries()
            .iter()
            .all(|entry| entry.failure().is_some())
    );
    assert!(before.elapsed() <= std::time::Duration::from_secs(31));
    let cancellation = CancellationToken::new();
    cancellation.cancel();
    assert!(matches!(
        adapter.snapshot(&capture(), &execution, cancellation).await,
        Err(ContributionError::Closed)
    ));
    drop(lease);
    assert!(runtime.shutdown().await.is_clean());
}

#[tokio::test]
async fn cancelling_an_active_capture_cancels_its_child_and_skips_later_units() {
    let active = Arc::new(Mutex::new(None));
    let later = Arc::new(Mutex::new(None));
    let (runtime, lease, adapter) = adapter(vec![
        Unit::Pending(active.clone()),
        Unit::Pending(later.clone()),
    ])
    .await;
    let cancellation = CancellationToken::new();
    let task = tokio::spawn({
        let cancellation = cancellation.clone();
        async move {
            adapter
                .snapshot(
                    &capture(),
                    &rsi_meta::Execution::native(tokio::runtime::Handle::current()),
                    cancellation,
                )
                .await
        }
    });
    tokio::time::timeout(std::time::Duration::from_secs(1), async {
        while active.lock().unwrap().is_none() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    cancellation.cancel();
    assert!(matches!(
        task.await.unwrap(),
        Err(ContributionError::Closed)
    ));
    assert!(active.lock().unwrap().as_ref().unwrap().is_cancelled());
    assert!(later.lock().unwrap().is_none());
    drop(lease);
    assert!(runtime.shutdown().await.is_clean());
}

#[test]
fn projection_capture_rejects_duplicate_domains_and_inconsistent_phase_before_callbacks() {
    let context = capture();
    assert!(
        SessionProjectionContext::new(
            Arc::new(header("alpha")),
            context.cursor(),
            vec![context.domains()[0].clone(); 2].into()
        )
        .is_err()
    );
    assert!(
        SessionProjectionContext::new(
            Arc::new(header("alpha")),
            ProjectionCursor::Draft { revision: 0 },
            context.domains().to_vec().into()
        )
        .is_err()
    );
}
