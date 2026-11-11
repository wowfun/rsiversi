use super::*;
use rsi_agent_composition_protocol::{
    ContributionKind, ContributionRegistration, ToolAroundContributor, ToolContinuation,
};
use rsi_agent_session_protocol::ContributionId;

#[derive(Debug)]
struct Hook {
    label: &'static str,
    short_circuit: bool,
    trace: Arc<Mutex<Vec<String>>>,
}

#[async_trait]
impl ToolAroundContributor for Hook {
    async fn execute(&self, next: ToolContinuation<'_>) -> ToolResultType<ToolResult> {
        assert_eq!(next.name(), "echo");
        assert_eq!(next.arguments(), &json!({"value":42}));
        let caller = next
            .execution()
            .extension::<rsi_agent_turn_protocol::AgentCallerAuthority>()
            .expect("native hook receives the exact started Tool authority");
        assert!(caller.tool_effect_id().is_some());
        assert_eq!(caller.session_id().as_str(), "session-e2e");
        self.trace
            .lock()
            .unwrap()
            .push(format!("{}:enter", self.label));
        tokio::task::yield_now().await;
        let mut result = if self.short_circuit {
            ToolResult::new(json!({"value":17}), vec![], false)?
        } else {
            next.run().await?
        };
        self.trace
            .lock()
            .unwrap()
            .push(format!("{}:exit", self.label));
        result.metadata = Some(Box::new(json!({"last_hook":self.label})));
        Ok(result)
    }
}

#[derive(Debug)]
struct Application(Arc<Mutex<Vec<String>>>);

#[async_trait]
impl rsi_tools_protocol::ToolMiddleware for Application {
    async fn prepare(
        self: Arc<Self>,
        _: &rsi_tools_protocol::ToolCall,
        _: Option<&rsi_tools_protocol::ToolOutputDeclaration>,
        _: &CancellationToken,
    ) -> ToolResultType<rsi_tools_protocol::ToolGate> {
        self.0.lock().unwrap().push("application:prepare".into());
        Ok(rsi_tools_protocol::ToolGate::Decided(
            rsi_tools_protocol::ToolDecision::Ready(Box::new(Application(self.0.clone()))),
        ))
    }
}

#[async_trait]
impl rsi_tools_protocol::AroundTool for Application {
    async fn execute(
        self: Box<Self>,
        body: rsi_tools_protocol::ToolBody<'_>,
    ) -> ToolResultType<ToolResult> {
        self.0.lock().unwrap().push("application:enter".into());
        let result = body.run().await;
        self.0.lock().unwrap().push("application:exit".into());
        result
    }
}

#[tokio::test]
async fn native_tool_hooks_preserve_the_application_execution_boundary() {
    let stack = BaseStack::activate().await;
    let calls = Arc::new(AtomicUsize::new(0));
    let _tool = register_native_echo(&stack, calls.clone());
    let trace = Arc::new(Mutex::new(Vec::new()));
    let mut source = RequestEcho::new(&stack, Arc::new(AtomicUsize::new(0)));
    source.middleware = Some(Arc::new(Application(trace.clone())));
    let source = Arc::new(source);
    let worker = activate_fixture(
        &stack.runtime,
        "test.request-tools",
        "app",
        Arc::new(RequestSourceFactory(source)),
    )
    .await;
    let callbacks =
        super::super::contributions::install(&stack, vec![hook("native", 0, false, &trace)]).await;
    let language = stack
        .activate_language("test.mixed-hooks", echo_then_answer(&stack))
        .await;
    let executor = stack.activate_executor("mixed-hooks").await;
    let (_, outcome) = stack.submit_and_wait("call echo").await;
    assert!(callbacks.dispose().await.is_clean());
    assert!(worker.dispose().await.is_clean());
    stack.dispose(language, executor).await;
    assert_eq!(outcome, TurnOutcome::Completed);
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert_eq!(
        *trace.lock().unwrap(),
        [
            "application:prepare",
            "native:enter",
            "application:enter",
            "application:exit",
            "native:exit"
        ]
    );
}

#[derive(Debug)]
struct CancelHook {
    entered: Notify,
    cancelled: Notify,
    release: CancellationToken,
}

#[async_trait]
impl ToolAroundContributor for CancelHook {
    async fn execute(&self, next: ToolContinuation<'_>) -> ToolResultType<ToolResult> {
        self.entered.notify_one();
        next.execution().cancellation.cancelled().await;
        self.cancelled.notify_one();
        self.release.cancelled().await;
        let mut result = ToolResult::new(json!({"cancelled":true}), vec![], true)?;
        result.metadata = Some(Box::new(json!({"hook":"drained"})));
        Ok(result)
    }
}

#[tokio::test]
async fn native_tool_hooks_drain_after_cancellation_before_publishing_the_receipt() {
    let stack = BaseStack::activate().await;
    let calls = Arc::new(AtomicUsize::new(0));
    let _tool = register_native_echo(&stack, calls.clone());
    let hook = Arc::new(CancelHook {
        entered: Notify::new(),
        cancelled: Notify::new(),
        release: CancellationToken::new(),
    });
    let callbacks = super::super::contributions::install(
        &stack,
        vec![ContributionRegistration::new(
            ContributionId::new("fixture.cancel-around").unwrap(),
            0,
            ContributionKind::ToolAround(hook.clone()),
        )],
    )
    .await;
    let language = stack
        .activate_language("test.cancel-hook", echo_then_answer(&stack))
        .await;
    let executor = stack.activate_executor("cancel-hook").await;
    let turns = stack
        .runtime
        .root()
        .lookup_local::<TurnServiceContract>()
        .unwrap();
    let submitted = stack
        .submit_fresh(&turns, "session-e2e", "cancel around hook")
        .await;
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        hook.entered.notified().await;
        stack.cancel_current().await;
        hook.cancelled.notified().await;
    })
    .await
    .expect("the active hook receives native cancellation");
    let pending = stack
        .store
        .read_facts(&submitted.session_id, 0, 64, FACT_PAGE_BYTES)
        .await
        .unwrap();
    assert!(
        !pending
            .facts
            .iter()
            .any(|fact| matches!(fact.body(), SessionFactBody::ToolResult { .. }))
    );
    // Retiring the registering Fiber cannot invalidate the admitted callback.
    assert!(callbacks.dispose().await.is_clean());
    hook.release.cancel();
    let outcome = wait_for_outcome(&turns, &submitted).await;
    let facts = stack
        .store
        .read_facts(&submitted.session_id, 0, 64, FACT_PAGE_BYTES)
        .await
        .unwrap()
        .facts;
    stack.dispose(language, executor).await;
    assert_eq!(outcome, TurnOutcome::Cancelled);
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    assert!(facts.iter().any(
        |fact| matches!(fact.body(), SessionFactBody::ToolResult { result, .. }
        if result.is_error && result.metadata.as_deref() == Some(&json!({"hook":"drained"})))
    ));
}

fn hook(
    label: &'static str,
    priority: i32,
    short_circuit: bool,
    trace: &Arc<Mutex<Vec<String>>>,
) -> ContributionRegistration {
    ContributionRegistration::new(
        ContributionId::new(format!("fixture.{label}")).unwrap(),
        priority,
        ContributionKind::ToolAround(Arc::new(Hook {
            label,
            short_circuit,
            trace: trace.clone(),
        })),
    )
}

#[tokio::test]
async fn native_tool_hooks_nest_and_retain_final_output_without_an_application() {
    for short_circuit in [false, true] {
        let stack = BaseStack::activate().await;
        let calls = Arc::new(AtomicUsize::new(0));
        let _tool = register_native_echo(&stack, calls.clone());
        let trace = Arc::new(Mutex::new(Vec::new()));
        let callbacks = super::super::contributions::install(
            &stack,
            vec![
                hook("inner", 1, short_circuit, &trace),
                hook("outer", 0, false, &trace),
            ],
        )
        .await;
        assert!(
            stack
                .runtime
                .root()
                .lookup_local::<rsi_agent_composition_protocol::RequestToolSourceContract>()
                .is_none()
        );
        let language = stack
            .activate_language("test.native-hooks", echo_then_answer(&stack))
            .await;
        let executor = stack.activate_executor("native-hooks").await;
        let (submitted, outcome) = stack.submit_and_wait("call echo").await;
        let facts = stack
            .store
            .read_facts(&submitted.session_id, 0, 64, FACT_PAGE_BYTES)
            .await
            .unwrap()
            .facts;
        assert!(callbacks.dispose().await.is_clean());
        stack.dispose(language, executor).await;
        assert_eq!(outcome, TurnOutcome::Completed);
        assert_eq!(
            *trace.lock().unwrap(),
            ["outer:enter", "inner:enter", "inner:exit", "outer:exit"]
        );
        assert_eq!(calls.load(Ordering::SeqCst), usize::from(!short_circuit));
        let results = facts
            .iter()
            .filter_map(|fact| match fact.body() {
                SessionFactBody::ToolResult { result, .. } => Some(result),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(results.len(), 1);
        assert_eq!(
            results[0].value,
            json!({"value":if short_circuit {17} else {42}})
        );
        assert_eq!(
            results[0].metadata.as_deref(),
            Some(&json!({"last_hook":"outer"}))
        );
    }
}
