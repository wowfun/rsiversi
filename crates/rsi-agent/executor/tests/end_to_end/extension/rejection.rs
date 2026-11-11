use super::*;
use rsi_agent_composition_protocol::{
    BeforeStepContext, BeforeStepContributor, BeforeStepOutput, ContributionResult,
};
use rsi_agent_session_protocol::StepInputDecision;

#[derive(Debug)]
struct RejectAfterExtension(AtomicUsize);

#[async_trait]
impl BeforeStepContributor for RejectAfterExtension {
    async fn prepare(
        &self,
        context: &BeforeStepContext<'_>,
        _: CancellationToken,
    ) -> ContributionResult<BeforeStepOutput> {
        assert!(context.previous.unwrap().messages().is_some());
        self.0.fetch_add(1, Ordering::SeqCst);
        Ok(BeforeStepOutput {
            decision: Some(StepInputDecision::reject()),
            ..Default::default()
        })
    }
}

#[tokio::test]
#[ignore = "requires RSI_EXTENSION_NODE and RSI_DSH_INSTALLED_COHORT with both packed plugins"]
#[allow(clippy::too_many_lines)] // Trace one actual extension proposal through later rejection and its canonical receipt.
async fn extension_journal_and_later_rejection_publish_once_without_native_prompt_or_provider() {
    let cohort = std::path::PathBuf::from(std::env::var("RSI_DSH_INSTALLED_COHORT").unwrap());
    let stack = BaseStack::activate().await;
    let entry = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/extension-step-receipt.mjs");
    let (process, extension, connection) = connect(&stack, &cohort, &entry).await;
    let definition = rsi_extension_agent::definition("a".repeat(64)).unwrap();
    let domains = DomainCatalog::new([definition.registration()]).unwrap();
    let handle = domains.bind(&definition).unwrap();
    *stack.composition.domains.lock().unwrap() = domains;
    let adapter = rsi_extension_agent::AgentAdapter::new(
        connection.clone(),
        stack.store.clone(),
        stack
            .runtime
            .root()
            .lookup_local::<rsi_agent_turn_protocol::TurnExecutionContract>()
            .unwrap(),
        handle.clone(),
        "a".repeat(64),
    )
    .unwrap();
    let rejection = Arc::new(RejectAfterExtension(AtomicUsize::new(0)));
    let callbacks = contributions::install(
        &stack,
        vec![
            ContributionRegistration::new(
                ContributionId::new("fixture.extension.before-step").unwrap(),
                0,
                ContributionKind::BeforeStep(adapter.clone()),
            ),
            ContributionRegistration::new(
                ContributionId::new("fixture.reject").unwrap(),
                1,
                ContributionKind::BeforeStep(rejection.clone()),
            ),
        ],
    )
    .await;
    let scope = prompt::install_unreachable(&stack).await;
    let provider = Arc::new(LanguageFixture {
        outcomes: Mutex::new(VecDeque::new()),
        requests: Mutex::new(vec![]),
        starts: Arc::new(AtomicUsize::new(0)),
        store: stack.store.clone(),
        retry_policy: RetryPolicy::default(),
    });
    let language = stack
        .activate_language("extension.provider", provider.clone())
        .await;
    let executor = stack.activate_executor("extension-executor").await;
    let (submitted, outcome) = stack
        .submit_and_wait("publish the staged extension even when a later callback rejects")
        .await;
    assert_eq!(outcome, TurnOutcome::Blocked);
    assert_eq!(rejection.0.load(Ordering::SeqCst), 1);
    assert_eq!(provider.starts.load(Ordering::SeqCst), 0);
    let counts = connection
        .request(
            "fixture.step-receipts",
            Value::Null,
            CancellationToken::new(),
        )
        .await
        .unwrap();
    assert_eq!(counts["preparations"], 1);
    let receipts = counts["receipts"].as_array().unwrap();
    assert_eq!(receipts.len(), 1);
    assert_eq!(receipts[0]["outcome"], "committed");
    let states = stack
        .store
        .read_domain_states(&submitted.session_id, None, None)
        .await
        .unwrap();
    assert_eq!(states.states.len(), 1);
    assert_eq!(
        receipts[0]["revision"].as_u64().unwrap(),
        states.states[0].head.revision.get()
    );
    let journal = handle.decode(&states.states[0].snapshot).unwrap();
    assert!(!journal.segments.is_empty());
    let facts = stack
        .store
        .read_facts(
            &submitted.session_id,
            0,
            64,
            rsi_agent_store_protocol::MAXIMUM_STORE_FACT_PAGE_BYTES,
        )
        .await
        .unwrap()
        .facts;
    let decisions: Vec<_> = facts
        .iter()
        .filter_map(|fact| match fact.body() {
            SessionFactBody::StepInputsPrepared { decision, .. } => Some(decision),
            _ => None,
        })
        .collect();
    assert_eq!(decisions.len(), 1);
    assert!(decisions[0].messages().is_none());
    assert!(
        !facts
            .iter()
            .any(|fact| matches!(fact.body(), SessionFactBody::ModelIntent { .. }))
    );
    let request = rsi_agent_session_protocol::DomainRequestId::new(
        receipts[0]["operationId"].as_str().unwrap(),
    )
    .unwrap();
    let receipt = stack
        .store
        .read_domain_request(&submitted.session_id, &request)
        .await
        .unwrap()
        .unwrap();
    let rsi_agent_session_protocol::AgentControlRecordBody::DomainStateCommitted { commit } =
        receipt.body()
    else {
        panic!("missing journal commit")
    };
    let span = commit.fact_span().unwrap();
    let final_decision = facts
        .iter()
        .find(|f| matches!(f.body(), SessionFactBody::StepInputsPrepared { .. }))
        .unwrap();
    assert!(
        final_decision.seq() >= span.first_seq()
            && final_decision.seq() < span.first_seq() + span.count()
    );
    assert!(callbacks.dispose().await.is_clean());
    assert!(scope.dispose().await.is_clean());
    drop(adapter);
    drop(connection);
    assert!(extension.dispose().await.is_clean());
    assert!(process.dispose().await.is_clean());
    stack.dispose(language, executor).await;
}
