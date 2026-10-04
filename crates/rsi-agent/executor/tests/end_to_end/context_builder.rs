use super::*;
use rsi_agent_context::{
    ContextBuilderIdentity, ContextInit, ContextPosition, DefaultContextBuilder,
    ModelContextBuilder, ModelContextCursor,
};

#[derive(Debug)]
struct SelectedBuilder {
    identity: ContextBuilderIdentity,
    events: Arc<Mutex<Vec<&'static str>>>,
    drop_effort: bool,
}

impl SelectedBuilder {
    fn new(id: &str) -> Arc<Self> {
        Arc::new(Self {
            identity: ContextBuilderIdentity::new(id, "1.0.0", "a".repeat(64)).unwrap(),
            events: Arc::new(Mutex::new(Vec::new())),
            drop_effort: false,
        })
    }
}

impl ModelContextBuilder for SelectedBuilder {
    fn identity(&self) -> &ContextBuilderIdentity {
        &self.identity
    }
    fn open(
        &self,
        mut init: ContextInit<'_>,
    ) -> rsi_agent_context::Result<Box<dyn ModelContextCursor>> {
        self.events.lock().unwrap().push("open");
        if let Some(bytes) = init.checkpoint {
            init.checkpoint = Some(
                bytes
                    .strip_prefix(b"fixture-context-payload\0")
                    .ok_or_else(|| {
                        rsi_agent_context::ContextError::Invalid(
                            "selected provider payload missing".into(),
                        )
                    })?,
            );
            self.events.lock().unwrap().push("restore");
        }
        let budget = init.budget.clone();
        Ok(Box::new(SelectedCursor {
            budget,
            inner: DefaultContextBuilder::default().open(init)?,
            marker: self.identity.id().to_owned(),
            events: self.events.clone(),
            drop_effort: self.drop_effort,
        }))
    }
}

#[derive(Debug)]
struct SelectedCursor {
    inner: Box<dyn ModelContextCursor>,
    budget: rsi_agent_context::ContextBudget,
    marker: String,
    events: Arc<Mutex<Vec<&'static str>>>,
    drop_effort: bool,
}
impl ModelContextCursor for SelectedCursor {
    fn ingest(&mut self, page: ContextPage<'_>) -> rsi_agent_context::Result<()> {
        self.inner.ingest(page)
    }
    fn build(
        &self,
        options: rsi_ai_protocol::LanguageRequestOptions,
        profile: &rsi_ai_protocol::LanguageProfile,
    ) -> rsi_agent_context::Result<LanguageRequest> {
        self.events.lock().unwrap().push("build");
        let original = self.inner.build(options.clone(), profile)?;
        let mut credit = self
            .budget
            .reserve(original.encoded_weight() * 2 + self.marker.len() * 6 + 1024)?;
        let mut messages = original.messages().to_vec();
        messages.push(rsi_ai_protocol::Message::system_text(&self.marker).unwrap());
        if self.drop_effort {
            return Ok(LanguageRequest::new(messages)
                .unwrap()
                .with_tools(original.tools().to_vec(), original.tool_choice().clone())
                .unwrap()
                .with_retention(credit));
        }
        let request = LanguageRequest::new_with_options(messages, options).unwrap();
        credit.resize(request.encoded_weight())?;
        Ok(request.with_retention(credit))
    }
    fn checkpoint(&self) -> rsi_agent_context::Result<rsi_api_protocol::RetainedBytes> {
        self.events.lock().unwrap().push("checkpoint");
        let payload = self.inner.checkpoint()?;
        let size = b"fixture-context-payload\0".len() + payload.len();
        let credit = self.budget.reserve(size)?;
        let reservation = rsi_api_protocol::ByteBudget::default()
            .reserve(size)
            .unwrap()
            .with_retention(credit);
        let mut bytes = Vec::with_capacity(size);
        bytes.extend_from_slice(b"fixture-context-payload\0");
        bytes.extend_from_slice(&payload);
        Ok(reservation.retain_vec(bytes).unwrap())
    }
    fn position(&self) -> ContextPosition {
        self.inner.position()
    }
}

#[tokio::test]
async fn builder_cannot_silently_drop_selected_effort_before_provider_prepare() {
    let stack = BaseStack::activate().await;
    let mut selected = SelectedBuilder::new("fixture.context.drops-effort");
    Arc::get_mut(&mut selected).unwrap().drop_effort = true;
    *stack.composition.context_builder.lock().unwrap() = selected;
    let language = Arc::new(LanguageFixture {
        outcomes: Mutex::new(VecDeque::new()),
        requests: Mutex::new(vec![]),
        starts: Arc::new(AtomicUsize::new(0)),
        store: stack.store.clone(),
        retry_policy: RetryPolicy::default(),
    });
    let language_fiber = stack
        .activate_language("test.language.drops-effort", language.clone())
        .await;
    let executor = stack.activate_executor("executor-drops-effort").await;
    let turns = stack
        .runtime
        .root()
        .lookup_local::<TurnServiceContract>()
        .unwrap();
    let submitted = turns
        .submit(SubmitTurn {
            reasoning_effort: Some(rsi_ai_protocol::ReasoningEffortId::new("high").unwrap()),
            turn_id: client_turn_id(),
            session: stack.fresh(header()).await,
            text: "work".into(),
            model: Some(header().settings().default_model().clone()),
            sandbox: None,
        })
        .await
        .unwrap();
    let outcome = wait_for_outcome(&turns, &submitted).await;
    assert!(matches!(outcome, TurnOutcome::Failed { code, .. } if code == "context.settings"));
    assert!(language.requests.lock().unwrap().is_empty());
    assert_eq!(language.starts.load(Ordering::SeqCst), 0);
    stack.dispose(language_fiber, executor).await;
}

#[tokio::test]
async fn execution_and_delayed_checkpoint_keep_the_admitted_builder_after_catalog_replacement() {
    let stack = BaseStack::activate().await;
    let selected = SelectedBuilder::new("fixture.context.admitted");
    let replacement = SelectedBuilder::new("fixture.context.replacement");
    *stack.composition.context_builder.lock().unwrap() = selected.clone();
    let turns = stack
        .runtime
        .root()
        .lookup_local::<TurnServiceContract>()
        .unwrap();
    let submitted = stack
        .submit_fresh(&turns, "selected-builder", "original task")
        .await;
    let admitted = stack.composition.pin.lock().unwrap().clone().unwrap();
    let replacement_pin = AgentCompositionPin::new(
        admitted.preset_id().clone(),
        "c".repeat(64),
        admitted.tools(),
        replacement.clone(),
        rsi_agent_composition_protocol::DomainCatalog::default(),
        rsi_agent_composition_protocol::ContributionCatalog::default(),
        Arc::new(()),
    )
    .unwrap();
    *stack.composition.pin.lock().unwrap() = Some(replacement_pin);
    *stack.composition.context_builder.lock().unwrap() = replacement.clone();
    drop(admitted);

    let language = Arc::new(LanguageFixture {
        outcomes: Mutex::new(VecDeque::from([StartOutcome::Stream(answer_script())])),
        requests: Mutex::new(vec![]),
        starts: Arc::new(AtomicUsize::new(0)),
        store: stack.store.clone(),
        retry_policy: RetryPolicy::default(),
    });
    let language_fiber = stack
        .activate_language("test.language.selected-builder", language.clone())
        .await;
    let executor_fiber = stack.activate_executor("executor-selected-builder").await;
    assert_eq!(
        wait_for_outcome(&turns, &submitted).await,
        TurnOutcome::Completed
    );
    let checkpoint = tokio::time::timeout(std::time::Duration::from_secs(2), async {
        loop {
            if let Some(checkpoint) = stack
                .store
                .read_context_checkpoint(
                    &submitted.session_id,
                    rsi_api_protocol::ByteBudget::default().into(),
                )
                .await
                .unwrap()
            {
                break checkpoint;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    let requests = serde_json::to_string(&*language.requests.lock().unwrap()).unwrap();
    assert!(requests.contains("fixture.context.admitted"));
    assert!(!requests.contains("fixture.context.replacement"));
    {
        let events = selected.events.lock().unwrap();
        assert!(
            events.iter().filter(|event| **event == "open").count() >= 2,
            "execution or maintenance bypassed the provider: {events:?}"
        );
        assert!(events.contains(&"build"));
        assert!(events.contains(&"checkpoint"));
    }
    assert!(replacement.events.lock().unwrap().is_empty());
    let stored_header = stack.store.header(&submitted.session_id).await.unwrap();
    let mut context = ModelContextState::open(
        selected,
        stored_header.clone(),
        ContextLimits::default(),
        rsi_agent_context::ContextBudget::default(),
    )
    .unwrap();
    context.restore(&checkpoint.bytes).unwrap();
    assert_eq!(context.position().through_seq, checkpoint.through_seq);
    let mut other = ModelContextState::open(
        replacement,
        stored_header,
        ContextLimits::default(),
        rsi_agent_context::ContextBudget::default(),
    )
    .unwrap();
    assert!(other.restore(&checkpoint.bytes).is_err());
    stack.dispose(language_fiber, executor_fiber).await;
}

#[tokio::test]
async fn context_capacity_fails_before_provider_dispatch_across_executor_generations() {
    let stack = BaseStack::activate().await;
    let budget = stack
        .runtime
        .root()
        .lookup_local::<rsi_agent_context::ContextBudgetContract>()
        .unwrap();
    let held = budget.reserve(budget.maximum()).unwrap();
    let language = Arc::new(LanguageFixture {
        outcomes: Mutex::new(VecDeque::new()),
        requests: Mutex::new(vec![]),
        starts: Arc::new(AtomicUsize::new(0)),
        store: stack.store.clone(),
        retry_policy: RetryPolicy::default(),
    });
    let language_fiber = stack
        .activate_language("test.language.context-capacity", language.clone())
        .await;
    let first = stack.activate_executor("context-capacity-old").await;
    let (_, outcome) = stack.submit_and_wait("bounded pressure").await;
    assert!(matches!(outcome, TurnOutcome::Failed { code, .. } if code == "context.capacity"));
    assert_eq!(language.starts.load(Ordering::Relaxed), 0);
    assert!(first.dispose().await.is_clean());
    let second = stack.activate_executor("context-capacity-new").await;
    let (_, outcome) = stack
        .submit_and_wait_with_header(
            "same pool after replacement",
            None,
            header_for_session("context-capacity-new-session", TurnBudget::default()),
        )
        .await;
    assert!(matches!(outcome, TurnOutcome::Failed { code, .. } if code == "context.capacity"));
    assert_eq!(budget.used(), budget.maximum());
    drop(held);
    stack.dispose(language_fiber, second).await;
}
