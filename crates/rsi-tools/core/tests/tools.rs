use async_trait::async_trait;
use rsi_meta::{ResolvedFactory, Runtime, UpdateMode};
use rsi_sandbox::{
    ConfinedProcess, EnforcementStamp, ProcessRequest, Sandbox, SandboxBackend, SandboxFileSystem,
    SandboxMode, SandboxNetwork, SandboxScratch,
};
use rsi_tools::ToolsFactory;
use rsi_tools_protocol::{
    MAXIMUM_ADMITTED_TOOL_INVOCATIONS, MAXIMUM_TOOL_CATALOGS, Result, RetainedToolFailureKind,
    RetainedToolResult, ToolCall, ToolCatalogProvider, ToolCatalogProviderContract, ToolDefinition,
    ToolError, ToolExecution, ToolExecutionExtensions, ToolExecutionPolicy, ToolExecutor,
    ToolRegistration, ToolResult, ToolRuntime, ToolStart,
};
use serde_json::{Value, json};
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
use tokio::sync::{Notify, Semaphore};
use tokio_util::sync::CancellationToken;

#[derive(Debug)]
struct TestSandbox;

#[derive(Debug)]
struct RejectingSandbox;

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
                filesystem: SandboxFileSystem::Unconfined,
                scratch: SandboxScratch::Host,
                network: SandboxNetwork::Host,
            },
        })
    }
}

#[async_trait]
impl Sandbox for RejectingSandbox {
    async fn confine(&self, request: ProcessRequest) -> rsi_sandbox::Result<ConfinedProcess> {
        Err(rsi_sandbox::SandboxError::Unsupported(request.mode))
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
        job_scope: None,
        extensions: ToolExecutionExtensions::default(),
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
        let _ = arguments;
        Err(ToolError::Cancelled)
    }
}

#[derive(Debug)]
struct EchoTool;

#[derive(Debug)]
struct RecursivePanicPayload;

impl Drop for RecursivePanicPayload {
    fn drop(&mut self) {
        panic!("panic payload destructor panicked");
    }
}

#[derive(Debug)]
struct RecursivePanicTool;

#[async_trait]
impl ToolExecutor for RecursivePanicTool {
    async fn execute(&self, _arguments: Value, _execution: ToolExecution) -> Result<ToolResult> {
        std::panic::panic_any(RecursivePanicPayload)
    }
}

#[derive(Debug)]
struct ConfineTool;

#[async_trait]
impl ToolExecutor for EchoTool {
    async fn execute(&self, arguments: Value, _execution: ToolExecution) -> Result<ToolResult> {
        ToolResult::new(arguments, vec![], false)
    }
}

#[async_trait]
impl ToolExecutor for ConfineTool {
    async fn execute(&self, _arguments: Value, execution: ToolExecution) -> Result<ToolResult> {
        execution
            .confine("/bin/sh".into(), vec![])
            .await
            .map(|_| unreachable!("rejecting sandbox cannot return a plan"))
    }
}

#[derive(Debug)]
struct StubbornTool {
    entered: Arc<Notify>,
    release: Arc<Notify>,
}

#[derive(Debug)]
struct AdmissionHoldingTool {
    entered: Arc<AtomicUsize>,
    entered_changed: Arc<Notify>,
    release: Arc<Semaphore>,
}

#[async_trait]
impl ToolExecutor for AdmissionHoldingTool {
    async fn execute(&self, arguments: Value, _execution: ToolExecution) -> Result<ToolResult> {
        self.entered.fetch_add(1, Ordering::SeqCst);
        self.entered_changed.notify_waiters();
        let permit = self
            .release
            .acquire()
            .await
            .expect("test release semaphore remains open");
        permit.forget();
        ToolResult::new(arguments, vec![], false)
    }
}

async fn wait_for_entered(entered: &AtomicUsize, changed: &Notify, expected: usize) {
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            let notified = changed.notified();
            tokio::pin!(notified);
            let _already_notified = notified.as_mut().enable();
            if entered.load(Ordering::SeqCst) >= expected {
                return;
            }
            notified.await;
        }
    })
    .await
    .expect("Tool test body did not enter within 5 seconds");
}

#[async_trait]
impl ToolExecutor for StubbornTool {
    async fn execute(&self, arguments: Value, _execution: ToolExecution) -> Result<ToolResult> {
        self.entered.notify_one();
        self.release.notified().await;
        ToolResult::new(arguments, vec![], false)
    }
}

async fn activated() -> (rsi_meta::FiberHandle, Arc<dyn ToolCatalogProvider>) {
    activated_with(Value::Null).await
}

async fn activated_with(config: Value) -> (rsi_meta::FiberHandle, Arc<dyn ToolCatalogProvider>) {
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
            config,
        )
        .await
        .unwrap();
    let provider = runtime
        .root()
        .lookup_local::<ToolCatalogProviderContract>()
        .unwrap();
    (fiber, provider)
}

fn seal(
    provider: &Arc<dyn ToolCatalogProvider>,
    registrations: Vec<ToolRegistration>,
) -> Arc<dyn ToolRuntime> {
    let stage = provider.begin_stage().unwrap();
    let registrar = stage.registrar();
    let lease =
        (!registrations.is_empty()).then(|| registrar.register_batch(registrations).unwrap());
    let tools = stage.seal().unwrap();
    drop(lease);
    tools
}

fn echo_registration(name: &str) -> ToolRegistration {
    ToolRegistration {
        definition: ToolDefinition::new(name, "echo", true.into()).unwrap(),
        timeout_ms: 1_000,
        executor: Arc::new(EchoTool),
    }
}

#[tokio::test]
async fn sealed_catalog_is_an_immutable_exact_authority_snapshot() {
    let (fiber, provider) = activated().await;
    let stage = provider.begin_stage().unwrap();
    let registrar = stage.registrar();
    let _lease = registrar
        .register_batch(vec![echo_registration("before-seal")])
        .unwrap();
    let tools = stage.seal().unwrap();

    assert_eq!(
        tools
            .definitions()
            .into_iter()
            .map(|definition| definition.name().to_owned())
            .collect::<Vec<_>>(),
        vec!["before-seal"]
    );
    assert!(matches!(
        registrar.register(echo_registration("after-seal")),
        Err(ToolError::Sealed)
    ));
    assert_eq!(tools.definitions()[0].name(), "before-seal");

    drop(tools);
    drop(registrar);
    drop(provider);
    assert!(fiber.dispose().await.is_clean());
}

#[tokio::test]
async fn sealing_after_provider_shutdown_fails_without_reentering_the_provider_lock() {
    let (fiber, provider) = activated().await;
    let stage = provider.begin_stage().unwrap();
    let registrar = stage.registrar();
    let _lease = registrar
        .register(echo_registration("unpublished"))
        .unwrap();
    drop(provider);
    assert!(fiber.dispose().await.is_clean());

    let (settled, receiver) = std::sync::mpsc::sync_channel(1);
    std::thread::spawn(move || {
        settled.send(stage.seal()).unwrap();
    });
    let result = receiver
        .recv_timeout(std::time::Duration::from_secs(1))
        .expect("seal must not deadlock while abandoning a stopped provider stage");
    assert!(matches!(result, Err(ToolError::ShuttingDown)));
}

#[tokio::test]
async fn retiring_a_registration_lease_after_seal_cannot_mutate_the_catalog() {
    let (fiber, provider) = activated().await;
    let stage = provider.begin_stage().unwrap();
    let registrar = stage.registrar();
    let lease = registrar.register(echo_registration("published")).unwrap();
    let tools = stage.seal().unwrap();

    lease.retire().unwrap();
    assert_eq!(
        tools
            .definitions()
            .into_iter()
            .map(|definition| definition.name().to_owned())
            .collect::<Vec<_>>(),
        ["published"]
    );
    let prepared = tools
        .prepare(
            "published-effect",
            ToolCall {
                id: "published-call".into(),
                name: "published".into(),
                arguments: json!({"authority":"catalog"}),
            },
        )
        .unwrap();
    let identity = prepared.identity().clone();
    assert_eq!(
        prepared
            .start(tool_start(CancellationToken::new()))
            .await
            .unwrap()
            .value,
        json!({"authority":"catalog"})
    );
    tools.commit(&identity).unwrap();

    drop(tools);
    drop(registrar);
    drop(provider);
    assert!(fiber.dispose().await.is_clean());
}

#[tokio::test]
async fn retiring_a_registration_lease_withdraws_its_batch_while_the_stage_is_open() {
    let (fiber, provider) = activated().await;
    let stage = provider.begin_stage().unwrap();
    let registrar = stage.registrar();
    let lease = registrar
        .register_batch(vec![
            echo_registration("withdrawn-a"),
            echo_registration("withdrawn-b"),
        ])
        .unwrap();

    lease.retire().unwrap();
    let replacement = registrar
        .register(echo_registration("withdrawn-a"))
        .expect("open-stage retirement must release the exact names");
    let tools = stage.seal().unwrap();
    assert_eq!(
        tools
            .definitions()
            .into_iter()
            .map(|definition| definition.name().to_owned())
            .collect::<Vec<_>>(),
        ["withdrawn-a"]
    );
    assert!(matches!(
        tools.prepare(
            "withdrawn-effect",
            ToolCall {
                id: "withdrawn-call".into(),
                name: "withdrawn-b".into(),
                arguments: json!({}),
            },
        ),
        Err(ToolError::Unknown(name)) if name == "withdrawn-b"
    ));

    drop(replacement);
    drop(tools);
    drop(registrar);
    drop(provider);
    assert!(fiber.dispose().await.is_clean());
}

#[tokio::test]
async fn batch_registration_is_atomic_and_abandoned_stages_publish_nothing() {
    let (fiber, provider) = activated().await;
    let stage = provider.begin_stage().unwrap();
    let registrar = stage.registrar();
    let existing = registrar
        .register_batch(vec![echo_registration("existing")])
        .unwrap();
    assert!(matches!(
        registrar.register_batch(vec![
            echo_registration("candidate"),
            echo_registration("existing"),
        ]),
        Err(ToolError::Duplicate(name)) if name == "existing"
    ));
    drop(existing);
    let tools = stage.seal().unwrap();
    assert!(tools.definitions().is_empty());

    let abandoned = provider.begin_stage().unwrap();
    let stale_registrar = abandoned.registrar();
    let _lease = stale_registrar
        .register(echo_registration("unpublished"))
        .unwrap();
    drop(abandoned);
    assert!(matches!(
        stale_registrar.register(echo_registration("late")),
        Err(ToolError::Sealed)
    ));

    drop(stale_registrar);
    drop(tools);
    drop(provider);
    assert!(fiber.dispose().await.is_clean());
}

#[tokio::test]
async fn stale_registrars_do_not_retain_abandoned_catalog_capacity() {
    let (fiber, provider) = activated().await;
    let mut stale_registrars = Vec::with_capacity(MAXIMUM_TOOL_CATALOGS);
    for _ in 0..MAXIMUM_TOOL_CATALOGS {
        let stage = provider.begin_stage().unwrap();
        stale_registrars.push(stage.registrar());
        drop(stage);
    }
    let final_stage = provider
        .begin_stage()
        .expect("abandon must release capacity even while stale registrars escape");

    drop(final_stage);
    drop(stale_registrars);
    drop(provider);
    assert!(fiber.dispose().await.is_clean());
}

#[tokio::test]
async fn independent_catalogs_can_reuse_names_without_schema_or_dispatch_drift() {
    let (fiber, provider) = activated().await;
    let old = seal(&provider, vec![echo_registration("echo")]);
    let new = seal(&provider, vec![echo_registration("echo")]);
    let old_call = old
        .prepare(
            "old-effect",
            ToolCall {
                id: "old-call".into(),
                name: "echo".into(),
                arguments: json!({"generation":"old"}),
            },
        )
        .unwrap();
    let new_call = new
        .prepare(
            "new-effect",
            ToolCall {
                id: "new-call".into(),
                name: "echo".into(),
                arguments: json!({"generation":"new"}),
            },
        )
        .unwrap();
    let old_identity = old_call.identity().clone();
    let new_identity = new_call.identity().clone();
    assert_ne!(old_identity.owner_id(), new_identity.owner_id());
    assert_eq!(
        old_call
            .start(tool_start(CancellationToken::new()))
            .await
            .unwrap()
            .value,
        json!({"generation":"old"})
    );
    assert_eq!(
        new_call
            .start(tool_start(CancellationToken::new()))
            .await
            .unwrap()
            .value,
        json!({"generation":"new"})
    );
    assert!(matches!(
        new.query(&old_identity),
        Err(ToolError::InvalidInput(message)) if message.contains("different catalog generation")
    ));
    assert!(matches!(
        old.commit(&new_identity),
        Err(ToolError::InvalidInput(message)) if message.contains("different catalog generation")
    ));
    old.commit(&old_identity).unwrap();
    new.commit(&new_identity).unwrap();

    drop(old);
    drop(new);
    drop(provider);
    assert!(fiber.dispose().await.is_clean());
}

#[tokio::test]
async fn retained_result_capacity_is_shared_across_catalog_generations() {
    let (fiber, provider) = activated().await;
    let old = seal(&provider, vec![echo_registration("echo")]);
    let new = seal(&provider, vec![echo_registration("echo")]);
    let mut retained = Vec::with_capacity(MAXIMUM_ADMITTED_TOOL_INVOCATIONS);

    for index in 0..MAXIMUM_ADMITTED_TOOL_INVOCATIONS {
        let tools = if index % 2 == 0 { &old } else { &new };
        let prepared = tools
            .prepare(
                &format!("effect-{index}"),
                ToolCall {
                    id: format!("call-{index}"),
                    name: "echo".into(),
                    arguments: json!({"index":index}),
                },
            )
            .unwrap();
        let identity = prepared.identity().clone();
        prepared
            .start(tool_start(CancellationToken::new()))
            .await
            .unwrap();
        retained.push((index % 2, identity));
    }

    let overflow = new
        .prepare(
            "effect-overflow",
            ToolCall {
                id: "call-overflow".into(),
                name: "echo".into(),
                arguments: json!({}),
            },
        )
        .unwrap();
    assert!(matches!(
        overflow.start(tool_start(CancellationToken::new())).await,
        Err(ToolError::Capacity)
    ));

    let (owner, identity) = retained.pop().unwrap();
    (if owner == 0 { &old } else { &new })
        .commit(&identity)
        .unwrap();
    let replacement = new
        .prepare(
            "effect-replacement",
            ToolCall {
                id: "call-replacement".into(),
                name: "echo".into(),
                arguments: json!({}),
            },
        )
        .unwrap();
    let replacement_identity = replacement.identity().clone();
    replacement
        .start(tool_start(CancellationToken::new()))
        .await
        .unwrap();
    new.commit(&replacement_identity).unwrap();
    for (owner, identity) in retained {
        (if owner == 0 { &old } else { &new })
            .commit(&identity)
            .unwrap();
    }

    drop(old);
    drop(new);
    drop(provider);
    assert!(fiber.dispose().await.is_clean());
}

#[tokio::test]
async fn catalog_withdrawal_cannot_recycle_admission_owned_by_active_bodies() {
    let (fiber, provider) = activated().await;
    let entered = Arc::new(AtomicUsize::new(0));
    let entered_changed = Arc::new(Notify::new());
    let release = Arc::new(Semaphore::new(0));
    let mut invocations = Vec::with_capacity(MAXIMUM_ADMITTED_TOOL_INVOCATIONS);

    for index in 0..MAXIMUM_ADMITTED_TOOL_INVOCATIONS {
        let tools = seal(
            &provider,
            vec![ToolRegistration {
                definition: ToolDefinition::new("hold", "hold admission", true.into()).unwrap(),
                timeout_ms: 600_000,
                executor: Arc::new(AdmissionHoldingTool {
                    entered: Arc::clone(&entered),
                    entered_changed: Arc::clone(&entered_changed),
                    release: Arc::clone(&release),
                }),
            }],
        );
        let prepared = tools
            .prepare(
                &format!("held-effect-{index}"),
                ToolCall {
                    id: format!("held-call-{index}"),
                    name: "hold".into(),
                    arguments: json!({"index":index}),
                },
            )
            .unwrap();
        invocations.push(tokio::spawn(
            prepared.start(tool_start(CancellationToken::new())),
        ));
        wait_for_entered(&entered, &entered_changed, index + 1).await;
        drop(tools);
    }

    let overflow_tools = seal(&provider, vec![echo_registration("echo")]);
    let overflow = overflow_tools
        .prepare(
            "overflow-effect",
            ToolCall {
                id: "overflow-call".into(),
                name: "echo".into(),
                arguments: json!({}),
            },
        )
        .unwrap();
    assert_eq!(
        overflow.start(tool_start(CancellationToken::new())).await,
        Err(ToolError::Capacity)
    );

    release.add_permits(1);
    invocations
        .remove(0)
        .await
        .expect("first held invocation task")
        .expect("body result remains authoritative after catalog withdrawal");

    let replacement = overflow_tools
        .prepare(
            "replacement-effect",
            ToolCall {
                id: "replacement-call".into(),
                name: "echo".into(),
                arguments: json!({}),
            },
        )
        .unwrap();
    let replacement_identity = replacement.identity().clone();
    replacement
        .start(tool_start(CancellationToken::new()))
        .await
        .unwrap();
    overflow_tools.commit(&replacement_identity).unwrap();

    release.add_permits(invocations.len());
    for invocation in invocations {
        invocation
            .await
            .expect("held invocation task")
            .expect("late body result");
    }

    drop(overflow_tools);
    drop(provider);
    assert!(fiber.dispose().await.is_clean());
}

#[tokio::test]
async fn retained_wait_observes_settlement_without_an_unrelated_notification() {
    let (fiber, provider) = activated().await;
    let entered = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    let tools = seal(
        &provider,
        vec![ToolRegistration {
            definition: ToolDefinition::new("stubborn", "stubborn", true.into()).unwrap(),
            timeout_ms: 1_000,
            executor: Arc::new(StubbornTool {
                entered: Arc::clone(&entered),
                release: Arc::clone(&release),
            }),
        }],
    );
    let prepared = tools
        .prepare(
            "effect-wait",
            ToolCall {
                id: "call-wait".into(),
                name: "stubborn".into(),
                arguments: json!({"settled":true}),
            },
        )
        .unwrap();
    let identity = prepared.identity().clone();
    let caller = tokio::spawn(prepared.start(tool_start(CancellationToken::new())));
    entered.notified().await;
    caller.abort();
    let waiter = tokio::spawn({
        let tools = Arc::clone(&tools);
        let identity = identity.clone();
        async move { tools.wait(&identity, CancellationToken::new()).await }
    });
    tokio::task::yield_now().await;

    release.notify_one();
    assert!(matches!(
        tokio::time::timeout(std::time::Duration::from_secs(1), waiter)
            .await
            .unwrap()
            .unwrap()
            .unwrap(),
        RetainedToolResult::Returned(_)
    ));
    tools.commit(&identity).unwrap();

    drop(tools);
    drop(provider);
    assert!(fiber.dispose().await.is_clean());
}

#[tokio::test]
async fn dropping_a_catalog_reclaims_settled_and_late_retained_results() {
    let (fiber, provider) = activated().await;
    let entered = Arc::new(Notify::new());
    let old = seal(
        &provider,
        vec![
            echo_registration("echo"),
            ToolRegistration {
                definition: ToolDefinition::new("blocking", "blocking", true.into()).unwrap(),
                timeout_ms: 1_000,
                executor: Arc::new(BlockingTool {
                    entered: Arc::clone(&entered),
                }),
            },
        ],
    );
    let settled = old
        .prepare(
            "old-settled-effect",
            ToolCall {
                id: "old-settled-call".into(),
                name: "echo".into(),
                arguments: json!({"generation":"old"}),
            },
        )
        .unwrap();
    settled
        .start(tool_start(CancellationToken::new()))
        .await
        .unwrap();
    let active = old
        .prepare(
            "old-active-effect",
            ToolCall {
                id: "old-active-call".into(),
                name: "blocking".into(),
                arguments: json!({}),
            },
        )
        .unwrap();
    let active = tokio::spawn(active.start(tool_start(CancellationToken::new())));
    entered.notified().await;

    drop(old);
    assert!(matches!(active.await.unwrap(), Err(ToolError::Cancelled)));

    let current = seal(&provider, vec![echo_registration("echo")]);
    let mut identities = Vec::with_capacity(MAXIMUM_ADMITTED_TOOL_INVOCATIONS);
    for index in 0..MAXIMUM_ADMITTED_TOOL_INVOCATIONS {
        let prepared = current
            .prepare(
                &format!("current-effect-{index}"),
                ToolCall {
                    id: format!("current-call-{index}"),
                    name: "echo".into(),
                    arguments: json!({"index":index}),
                },
            )
            .unwrap();
        identities.push(prepared.identity().clone());
        prepared
            .start(tool_start(CancellationToken::new()))
            .await
            .unwrap();
    }
    for identity in identities {
        current.commit(&identity).unwrap();
    }

    drop(current);
    drop(provider);
    assert!(fiber.dispose().await.is_clean());
}

#[tokio::test]
async fn sandbox_rejection_remains_structured_across_tool_execution() {
    let (fiber, provider) = activated().await;
    let tools = seal(
        &provider,
        vec![ToolRegistration {
            definition: ToolDefinition::new("confine", "confine", true.into()).unwrap(),
            timeout_ms: 1_000,
            executor: Arc::new(ConfineTool),
        }],
    );
    let prepared = tools
        .prepare(
            "effect-1",
            ToolCall {
                id: "call-1".into(),
                name: "confine".into(),
                arguments: json!({}),
            },
        )
        .unwrap();
    let mut start = tool_start(CancellationToken::new());
    start.policy.mode = SandboxMode::ReadOnly;
    start.sandbox = Arc::new(RejectingSandbox);
    assert_eq!(
        prepared.start(start).await,
        Err(ToolError::Sandbox(rsi_sandbox::SandboxError::Unsupported(
            SandboxMode::ReadOnly
        )))
    );

    drop(tools);
    drop(provider);
    assert!(fiber.dispose().await.is_clean());
}

#[tokio::test]
async fn dropping_a_catalog_withdraws_prepared_calls_and_cancels_admitted_calls() {
    let (fiber, provider) = activated().await;
    let prepared_tools = seal(&provider, vec![echo_registration("echo")]);
    let prepared = prepared_tools
        .prepare(
            "prepared-effect",
            ToolCall {
                id: "prepared-call".into(),
                name: "echo".into(),
                arguments: json!({}),
            },
        )
        .unwrap();
    drop(prepared_tools);
    assert_eq!(
        prepared.start(tool_start(CancellationToken::new())).await,
        Err(ToolError::Withdrawn("echo".into()))
    );

    let entered = Arc::new(Notify::new());
    let active_tools = seal(
        &provider,
        vec![ToolRegistration {
            definition: ToolDefinition::new("wait", "wait", true.into()).unwrap(),
            timeout_ms: 10_000,
            executor: Arc::new(BlockingTool {
                entered: entered.clone(),
            }),
        }],
    );
    let active = active_tools
        .prepare(
            "active-effect",
            ToolCall {
                id: "active-call".into(),
                name: "wait".into(),
                arguments: json!({}),
            },
        )
        .unwrap();
    let invocation =
        tokio::spawn(async move { active.start(tool_start(CancellationToken::new())).await });
    entered.notified().await;
    drop(active_tools);
    assert_eq!(invocation.await.unwrap(), Err(ToolError::Cancelled));

    drop(provider);
    assert!(fiber.dispose().await.is_clean());
}

#[tokio::test]
async fn catalog_bounds_tool_count_and_per_call_timeout_before_seal() {
    let (fiber, provider) = activated().await;
    let stage = provider.begin_stage().unwrap();
    let registrar = stage.registrar();
    assert!(
        registrar
            .register(ToolRegistration {
                definition: ToolDefinition::new("too-slow", "", json!({})).unwrap(),
                timeout_ms: 600_001,
                executor: Arc::new(EchoTool),
            })
            .is_err()
    );
    let registrations = (0..64)
        .map(|index| echo_registration(&format!("tool-{index}")))
        .collect();
    let _lease = registrar.register_batch(registrations).unwrap();
    assert!(
        registrar
            .register(echo_registration("tool-overflow"))
            .is_err()
    );
    let tools = stage.seal().unwrap();
    assert_eq!(tools.definitions().len(), 64);

    drop(tools);
    drop(registrar);
    drop(provider);
    assert!(fiber.dispose().await.is_clean());
}

#[tokio::test(start_paused = true)]
async fn timeout_is_retained_until_the_orchestrator_commits_it() {
    let (fiber, provider) = activated().await;
    let entered = Arc::new(Notify::new());
    let tools = seal(
        &provider,
        vec![ToolRegistration {
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
        }],
    );
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

    drop(tools);
    drop(provider);
    assert!(fiber.dispose().await.is_clean());
}

#[tokio::test]
async fn recursive_panic_payload_destruction_cannot_abandon_settlement() {
    let (fiber, provider) = activated().await;
    let tools = seal(
        &provider,
        vec![ToolRegistration {
            definition: ToolDefinition::new("panic", "panic", true.into()).unwrap(),
            timeout_ms: 1_000,
            executor: Arc::new(RecursivePanicTool),
        }],
    );
    let prepared = tools
        .prepare(
            "effect-recursive-panic",
            ToolCall {
                id: "call-recursive-panic".into(),
                name: "panic".into(),
                arguments: json!({}),
            },
        )
        .unwrap();
    let identity = prepared.identity().clone();

    assert!(matches!(
        prepared.start(tool_start(CancellationToken::new())).await,
        Err(ToolError::Execution(message)) if message.contains("panicked")
    ));
    assert!(matches!(
        tools.query(&identity).unwrap(),
        RetainedToolResult::Failed(failure)
            if failure.kind == RetainedToolFailureKind::Execution
    ));
    tools.commit(&identity).unwrap();

    drop(tools);
    drop(provider);
    assert!(fiber.dispose().await.is_clean());
}

#[tokio::test]
async fn canonical_identity_duplicate_start_and_retained_outcome_are_exact() {
    let (fiber, provider) = activated().await;
    let tools = seal(&provider, vec![echo_registration("echo")]);
    let left = tools
        .prepare(
            "turn-a:effect-1",
            ToolCall {
                id: "same-call".into(),
                name: "echo".into(),
                arguments: json!({"b":2,"a":1}),
            },
        )
        .unwrap();
    let duplicate = tools
        .prepare(
            "turn-a:effect-1",
            ToolCall {
                id: "same-call".into(),
                name: "echo".into(),
                arguments: json!({"a":1,"b":2}),
            },
        )
        .unwrap();
    assert_eq!(left.identity(), duplicate.identity());
    let independent = tools
        .prepare(
            "turn-b:effect-1",
            ToolCall {
                id: "same-call".into(),
                name: "echo".into(),
                arguments: json!({"a":1,"b":2}),
            },
        )
        .unwrap();
    assert_ne!(left.identity(), independent.identity());
    let identity = left.identity().clone();
    let result = left
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
    drop(provider);
    assert!(fiber.dispose().await.is_clean());
}

#[tokio::test]
async fn cancellation_and_dropped_waiters_do_not_abandon_tool_settlement() {
    let (fiber, provider) = activated().await;
    let entered = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    let tools = seal(
        &provider,
        vec![ToolRegistration {
            definition: ToolDefinition::new("stubborn", "stubborn", true.into()).unwrap(),
            timeout_ms: 10_000,
            executor: Arc::new(StubbornTool {
                entered: entered.clone(),
                release: release.clone(),
            }),
        }],
    );
    let prepared = tools
        .prepare(
            "effect-1",
            ToolCall {
                id: "call-1".into(),
                name: "stubborn".into(),
                arguments: json!({"value":1}),
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
    assert!(!invocation.is_finished());
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
    .expect("provider-owned settlement should complete");
    tools.commit(&identity).unwrap();

    drop(tools);
    drop(provider);
    assert!(fiber.dispose().await.is_clean());
}

#[tokio::test]
async fn provider_cleanup_reports_unsettled_noncooperative_tools_within_its_bound() {
    let (fiber, provider) = activated_with(json!({"shutdown_timeout_ms":5})).await;
    let entered = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    let tools = seal(
        &provider,
        vec![ToolRegistration {
            definition: ToolDefinition::new("stubborn", "stubborn", true.into()).unwrap(),
            timeout_ms: 10_000,
            executor: Arc::new(StubbornTool {
                entered: entered.clone(),
                release: release.clone(),
            }),
        }],
    );
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
        .expect("provider cleanup must have a finite wait bound");
    assert!(!report.is_clean());
    assert!(report.failures()[0].error.contains("unsettled work"));

    release.notify_one();
    assert_eq!(invocation.await.unwrap().unwrap().value, json!({}));
    drop(tools);
    drop(provider);
}
