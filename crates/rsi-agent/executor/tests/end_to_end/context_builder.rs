use super::*;
use rsi_agent_context::{
    ContextBuilderIdentity, ContextInit, ContextPosition, DefaultContextBuilder,
    ModelContextBuilder, ModelContextCursor,
};

#[derive(Debug)]
struct SelectedBuilder {
    identity: ContextBuilderIdentity,
    events: Arc<Mutex<Vec<&'static str>>>,
}

impl SelectedBuilder {
    fn new(id: &str) -> Arc<Self> {
        Arc::new(Self {
            identity: ContextBuilderIdentity::new(id, "1.0.0", "a".repeat(64)).unwrap(),
            events: Arc::new(Mutex::new(Vec::new())),
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
        Ok(Box::new(SelectedCursor {
            inner: DefaultContextBuilder::default().open(init)?,
            marker: self.identity.id().to_owned(),
            events: self.events.clone(),
        }))
    }
}

#[derive(Debug)]
struct SelectedCursor {
    inner: Box<dyn ModelContextCursor>,
    marker: String,
    events: Arc<Mutex<Vec<&'static str>>>,
}
impl ModelContextCursor for SelectedCursor {
    fn ingest(&mut self, page: ContextPage<'_>) -> rsi_agent_context::Result<()> {
        self.inner.ingest(page)
    }
    fn build(
        &self,
        tools: Vec<rsi_tools_protocol::ToolDefinition>,
    ) -> rsi_agent_context::Result<LanguageRequest> {
        self.events.lock().unwrap().push("build");
        let original = self.inner.build(tools)?;
        let mut messages = original.messages().to_vec();
        messages.push(rsi_ai_protocol::Message::system_text(&self.marker).unwrap());
        Ok(LanguageRequest::new(messages)
            .unwrap()
            .with_tools(original.tools().to_vec(), original.tool_choice().clone())
            .unwrap())
    }
    fn checkpoint(&self) -> rsi_agent_context::Result<Arc<[u8]>> {
        self.events.lock().unwrap().push("checkpoint");
        let payload = self.inner.checkpoint()?;
        let mut bytes = b"fixture-context-payload\0".to_vec();
        bytes.extend_from_slice(&payload);
        Ok(bytes.into())
    }
    fn position(&self) -> ContextPosition {
        self.inner.position()
    }
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
                .read_context_checkpoint(&submitted.session_id)
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
    let mut context =
        ModelContextState::open(selected, stored_header.clone(), ContextLimits::default()).unwrap();
    context.restore(&checkpoint.bytes).unwrap();
    assert_eq!(context.position().through_seq, checkpoint.through_seq);
    let mut other =
        ModelContextState::open(replacement, stored_header, ContextLimits::default()).unwrap();
    assert!(other.restore(&checkpoint.bytes).is_err());
    stack.dispose(language_fiber, executor_fiber).await;
}
