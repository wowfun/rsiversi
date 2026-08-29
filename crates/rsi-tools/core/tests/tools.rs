use async_trait::async_trait;
use rsi_meta::{ResolvedFactory, Runtime, UpdateMode};
use rsi_sandbox::{
    ConfinedProcess, EnforcementStamp, ProcessRequest, Sandbox, SandboxBackend, SandboxMode,
};
use rsi_tools::ToolsFactory;
use rsi_tools_protocol::{
    Result, RetainedToolFailureKind, RetainedToolResult, ToolCall, ToolDefinition, ToolError,
    ToolExecution, ToolExecutionPolicy, ToolExecutor, ToolRegistration, ToolResult,
    ToolRuntimeContract, ToolStart,
};
use serde_json::{Value, json};
use std::sync::Arc;
use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;

#[derive(Debug)]
struct TestSandbox;

#[async_trait]
impl Sandbox for TestSandbox {
    async fn confine(&self, request: ProcessRequest) -> rsi_sandbox::Result<ConfinedProcess> {
        Ok(ConfinedProcess {
            program: request.program,
            arguments: request.arguments.into_iter().map(Into::into).collect(),
            cwd: request.cwd,
            stamp: EnforcementStamp {
                requested: request.mode,
                backend: SandboxBackend::Unconfined,
                workspace: request.workspace,
                workspace_writable: true,
                network_restricted: false,
            },
        })
    }
}

fn tool_start(cancellation: CancellationToken) -> ToolStart {
    ToolStart {
        cancellation,
        policy: ToolExecutionPolicy {
            mode: SandboxMode::DangerFullAccess,
            cwd: "/workspace".into(),
            workspace: "/workspace".into(),
        },
        sandbox: Arc::new(TestSandbox),
    }
}

#[derive(Debug)]
struct BlockingTool {
    entered: Arc<Notify>,
}

#[async_trait]
impl ToolExecutor for BlockingTool {
    async fn execute(&self, arguments: Value, execution: ToolExecution) -> Result<ToolResult> {
        self.entered.notify_one();
        execution.cancellation.cancelled().await;
        ToolResult::new(arguments, vec![], false)
    }
}

#[derive(Debug)]
struct EchoTool;

#[async_trait]
impl ToolExecutor for EchoTool {
    async fn execute(&self, arguments: Value, _execution: ToolExecution) -> Result<ToolResult> {
        ToolResult::new(arguments, vec![], false)
    }
}

#[derive(Debug)]
struct StubbornTool {
    entered: Arc<Notify>,
    release: Arc<Notify>,
}

#[async_trait]
impl ToolExecutor for StubbornTool {
    async fn execute(&self, arguments: Value, _execution: ToolExecution) -> Result<ToolResult> {
        self.entered.notify_one();
        self.release.notified().await;
        ToolResult::new(arguments, vec![], false)
    }
}

async fn activated() -> (
    rsi_meta::FiberHandle,
    Arc<dyn rsi_tools_protocol::ToolRuntime>,
) {
    let runtime = Runtime::default();
    let fiber = runtime
        .root()
        .apply(
            ResolvedFactory::linked(
                "rsi.tools",
                "test",
                UpdateMode::Replayable,
                Arc::new(ToolsFactory),
            ),
            Value::Null,
        )
        .await
        .unwrap();
    let tools = runtime
        .root()
        .lookup_local::<ToolRuntimeContract>()
        .unwrap();
    (fiber, tools)
}

#[tokio::test]
async fn registry_bounds_tool_count_and_per_call_timeout() {
    let (fiber, tools) = activated().await;
    assert!(
        tools
            .register(ToolRegistration {
                definition: ToolDefinition::new("too-slow", "", json!({})).unwrap(),
                timeout_ms: 600_001,
                executor: Arc::new(EchoTool),
            })
            .is_err()
    );

    let mut leases = Vec::new();
    for index in 0..64 {
        leases.push(
            tools
                .register(ToolRegistration {
                    definition: ToolDefinition::new(format!("tool-{index}"), "", json!({}))
                        .unwrap(),
                    timeout_ms: 1_000,
                    executor: Arc::new(EchoTool),
                })
                .unwrap(),
        );
    }
    assert!(
        tools
            .register(ToolRegistration {
                definition: ToolDefinition::new("tool-overflow", "", json!({})).unwrap(),
                timeout_ms: 1_000,
                executor: Arc::new(EchoTool),
            })
            .is_err()
    );

    drop(leases);
    drop(tools);
    assert!(fiber.dispose().await.is_clean());
}

#[tokio::test(start_paused = true)]
async fn prepared_timeout_is_retained_and_schema_lease_is_exact() {
    let (fiber, tools) = activated().await;
    let entered = Arc::new(Notify::new());
    let lease = tools
        .register(ToolRegistration {
            definition: ToolDefinition::new(
                "wait",
                "wait until cancelled",
                json!({"type":"object"}),
            )
            .unwrap(),
            timeout_ms: 100,
            executor: Arc::new(BlockingTool {
                entered: entered.clone(),
            }),
        })
        .unwrap();
    let prepared = tools
        .prepare(
            "turn-1:effect-1",
            ToolCall {
                id: "call-1".into(),
                name: "wait".into(),
                arguments: json!({}),
            },
        )
        .unwrap();
    let identity = prepared.identity().clone();
    let invocation =
        tokio::spawn(async move { prepared.start(tool_start(CancellationToken::new())).await });
    entered.notified().await;
    assert_eq!(tools.query(&identity).unwrap(), RetainedToolResult::Pending);
    tokio::time::advance(std::time::Duration::from_millis(100)).await;
    assert_eq!(invocation.await.unwrap(), Err(ToolError::Timeout));
    assert!(matches!(
        tools.query(&identity).unwrap(),
        RetainedToolResult::Failed(failure)
            if failure.kind == RetainedToolFailureKind::Timeout
    ));
    tools.commit(&identity).unwrap();
    assert_eq!(tools.query(&identity).unwrap(), RetainedToolResult::Absent);

    drop(lease);
    assert!(tools.definitions().is_empty());
    assert!(matches!(
        tools.prepare("turn-1:effect-2", ToolCall {
            id: "call-2".into(),
            name: "wait".into(),
            arguments: json!({}),
        }),
        Err(ToolError::Unknown(name)) if name == "wait"
    ));
    drop(tools);
    assert!(fiber.dispose().await.is_clean());
}

#[tokio::test]
async fn canonical_request_identity_and_returned_result_are_retained() {
    let (fiber, tools) = activated().await;
    let _lease = tools
        .register(ToolRegistration {
            definition: ToolDefinition::new("echo", "echo", true.into()).unwrap(),
            timeout_ms: 1_000,
            executor: Arc::new(EchoTool),
        })
        .unwrap();
    let left = tools
        .prepare(
            "turn-a:effect-1",
            ToolCall {
                id: "same-call".into(),
                name: "echo".into(),
                arguments: json!({"b": 2, "a": 1}),
            },
        )
        .unwrap();
    let canonical = tools
        .prepare(
            "turn-a:effect-1",
            ToolCall {
                id: "same-call".into(),
                name: "echo".into(),
                arguments: json!({"a": 1, "b": 2}),
            },
        )
        .unwrap();
    assert_eq!(left.identity(), canonical.identity());
    let independent = tools
        .prepare(
            "turn-b:effect-1",
            ToolCall {
                id: "same-call".into(),
                name: "echo".into(),
                arguments: json!({"a": 1, "b": 2}),
            },
        )
        .unwrap();
    assert_ne!(left.identity(), independent.identity());
    let identity = left.identity().clone();
    let result = left
        .start(tool_start(CancellationToken::new()))
        .await
        .unwrap();
    assert_eq!(
        tools.query(&identity).unwrap(),
        RetainedToolResult::Returned(result)
    );
    drop(tools);
    assert!(fiber.dispose().await.is_clean());
}

#[tokio::test]
async fn duplicate_start_preserves_the_first_retained_outcome() {
    let (fiber, tools) = activated().await;
    let _lease = tools
        .register(ToolRegistration {
            definition: ToolDefinition::new("echo", "echo", true.into()).unwrap(),
            timeout_ms: 1_000,
            executor: Arc::new(EchoTool),
        })
        .unwrap();
    let call = ToolCall {
        id: "call-1".into(),
        name: "echo".into(),
        arguments: json!({"value": 1}),
    };
    let first = tools.prepare("effect-1", call.clone()).unwrap();
    let duplicate = tools.prepare("effect-1", call).unwrap();
    let identity = first.identity().clone();
    let result = first
        .start(tool_start(CancellationToken::new()))
        .await
        .unwrap();

    assert!(matches!(
        duplicate.start(tool_start(CancellationToken::new())).await,
        Err(ToolError::Execution(message)) if message.contains("already started")
    ));
    assert_eq!(
        tools.query(&identity).unwrap(),
        RetainedToolResult::Returned(result)
    );
    tools.commit(&identity).unwrap();
    drop(tools);
    assert!(fiber.dispose().await.is_clean());
}

#[tokio::test]
async fn cancellation_waits_until_a_noncooperative_tool_body_settles() {
    let (fiber, tools) = activated().await;
    let entered = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    let _lease = tools
        .register(ToolRegistration {
            definition: ToolDefinition::new("stubborn", "stubborn", true.into()).unwrap(),
            timeout_ms: 10_000,
            executor: Arc::new(StubbornTool {
                entered: entered.clone(),
                release: release.clone(),
            }),
        })
        .unwrap();
    let prepared = tools
        .prepare(
            "effect-1",
            ToolCall {
                id: "call-1".into(),
                name: "stubborn".into(),
                arguments: json!({}),
            },
        )
        .unwrap();
    let identity = prepared.identity().clone();
    let cancellation = CancellationToken::new();
    let invocation_cancellation = cancellation.clone();
    let invocation =
        tokio::spawn(async move { prepared.start(tool_start(invocation_cancellation)).await });
    entered.notified().await;
    cancellation.cancel();
    tokio::task::yield_now().await;
    assert!(
        !invocation.is_finished(),
        "cancellation returned before the body settled"
    );

    release.notify_one();
    assert_eq!(invocation.await.unwrap(), Err(ToolError::Cancelled));
    assert!(matches!(
        tools.query(&identity).unwrap(),
        RetainedToolResult::Failed(failure)
            if failure.kind == RetainedToolFailureKind::Cancelled
    ));
    tools.commit(&identity).unwrap();
    drop(tools);
    assert!(fiber.dispose().await.is_clean());
}

#[tokio::test]
async fn dropping_the_start_waiter_does_not_abandon_settlement() {
    let (fiber, tools) = activated().await;
    let entered = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    let _lease = tools
        .register(ToolRegistration {
            definition: ToolDefinition::new("stubborn", "stubborn", true.into()).unwrap(),
            timeout_ms: 10_000,
            executor: Arc::new(StubbornTool {
                entered: entered.clone(),
                release: release.clone(),
            }),
        })
        .unwrap();
    let prepared = tools
        .prepare(
            "effect-1",
            ToolCall {
                id: "call-1".into(),
                name: "stubborn".into(),
                arguments: json!({"value": 1}),
            },
        )
        .unwrap();
    let identity = prepared.identity().clone();
    let invocation =
        tokio::spawn(async move { prepared.start(tool_start(CancellationToken::new())).await });
    entered.notified().await;
    invocation.abort();
    assert!(invocation.await.unwrap_err().is_cancelled());
    release.notify_one();

    tokio::time::timeout(std::time::Duration::from_secs(1), async {
        loop {
            if matches!(
                tools.query(&identity).unwrap(),
                RetainedToolResult::Returned(_)
            ) {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("runtime-owned settlement should complete");
    tools.commit(&identity).unwrap();
    drop(tools);
    assert!(fiber.dispose().await.is_clean());
}

#[tokio::test]
async fn generation_cleanup_reports_unsettled_noncooperative_tool_within_its_bound() {
    let runtime = Runtime::default();
    let fiber = runtime
        .root()
        .apply(
            ResolvedFactory::linked(
                "rsi.tools",
                "test",
                UpdateMode::Replayable,
                Arc::new(ToolsFactory),
            ),
            json!({"shutdown_timeout_ms":5}),
        )
        .await
        .unwrap();
    let tools = runtime
        .root()
        .lookup_local::<ToolRuntimeContract>()
        .unwrap();
    let entered = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    let _lease = tools
        .register(ToolRegistration {
            definition: ToolDefinition::new("stubborn", "stubborn", true.into()).unwrap(),
            timeout_ms: 10_000,
            executor: Arc::new(StubbornTool {
                entered: entered.clone(),
                release: release.clone(),
            }),
        })
        .unwrap();
    let prepared = tools
        .prepare(
            "effect-1",
            ToolCall {
                id: "call-1".into(),
                name: "stubborn".into(),
                arguments: json!({}),
            },
        )
        .unwrap();
    let invocation =
        tokio::spawn(async move { prepared.start(tool_start(CancellationToken::new())).await });
    entered.notified().await;

    let report = tokio::time::timeout(std::time::Duration::from_secs(1), fiber.dispose())
        .await
        .expect("generation cleanup must have a finite wait bound");
    assert!(!report.is_clean());
    assert!(report.failures()[0].error.contains("unsettled work"));

    release.notify_one();
    assert_eq!(invocation.await.unwrap(), Err(ToolError::Cancelled));
    drop(tools);
}
