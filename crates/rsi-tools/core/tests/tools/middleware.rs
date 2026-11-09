use super::*;

#[derive(Debug)]
struct Count(Arc<AtomicUsize>);

#[async_trait]
impl ToolExecutor for Count {
    async fn execute(&self, arguments: Value, _: ToolExecution) -> Result<ToolResult> {
        self.0.fetch_add(1, Ordering::SeqCst);
        ToolResult::new(arguments, vec![], false)
    }
}

#[derive(Debug)]
struct AwaitCancellation(Arc<Notify>);

#[async_trait]
impl rsi_tools_protocol::AroundTool for AwaitCancellation {
    async fn execute(
        self: Box<Self>,
        body: rsi_tools_protocol::ToolBody<'_>,
    ) -> Result<ToolResult> {
        self.0.notify_one();
        body.execution().cancellation.cancelled().await;
        body.run().await
    }
}

#[tokio::test]
async fn native_body_is_not_entered_when_cancellation_overtakes_an_around_hook() {
    let (fiber, provider) = activated().await;
    let calls = Arc::new(AtomicUsize::new(0));
    let mut registration = echo_registration("echo");
    registration.executor = Arc::new(Count(calls.clone()));
    let tools = seal(&provider, vec![registration]);
    let mut prepared = tools
        .prepare(
            "cancel-in-wrapper",
            ToolCall {
                id: "call".into(),
                name: "echo".into(),
                arguments: json!({"value":42}),
            },
        )
        .unwrap();
    let identity = prepared.identity().clone();
    let entered = Arc::new(Notify::new());
    *prepared.native_plan().unwrap().continuation =
        Some(Box::new(AwaitCancellation(entered.clone())));
    let cancellation = CancellationToken::new();
    let start = tool_start(cancellation.clone());
    let waiter = tokio::spawn(async move { prepared.start(start).await });
    entered.notified().await;
    cancellation.cancel();
    let outcome = waiter.await.unwrap();
    let retained = tools.query(&identity).unwrap();
    tools.commit(&identity).unwrap();
    drop((tools, provider));
    assert!(fiber.dispose().await.is_clean());
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    assert!(matches!(outcome, Err(ToolError::Cancelled)));
    assert!(
        matches!(retained, RetainedToolResult::Failed(failure) if failure.kind == RetainedToolFailureKind::Cancelled)
    );
}
