use super::*;
use rsi_settings_protocol::SettingsContract;
use std::time::Duration;
use tokio::sync::mpsc;

#[derive(Debug)]
struct GatedTool {
    started: mpsc::UnboundedSender<()>,
    release: Semaphore,
}

#[async_trait]
impl ToolExecutor for GatedTool {
    async fn execute(&self, args: Value, _: ToolExecution) -> ToolResultType<ToolResult> {
        self.started.send(()).unwrap();
        self.release.acquire().await.unwrap().forget();
        ToolResult::new(args, vec![], false)
    }
}

async fn started(events: &mut mpsc::UnboundedReceiver<()>, count: usize) {
    tokio::time::timeout(Duration::from_secs(10), async {
        for _ in 0..count {
            events.recv().await.unwrap();
        }
    })
    .await
    .expect("tools started while previously admitted calls remained blocked");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn live_parallel_limit_is_sampled_once_per_provider_batch() {
    let stack = BaseStack::activate().await;
    let (tx, mut events) = mpsc::unbounded_channel();
    let tool = Arc::new(GatedTool {
        started: tx,
        release: Semaphore::new(0),
    });
    let mut leases = Vec::new();
    for (name, scheduling) in [
        ("hold", ToolScheduling::Exclusive),
        ("read_probe", ToolScheduling::ParallelSafe),
    ] {
        leases.push(
            stack
                .tool_registrar
                .register(ToolRegistration {
                    definition: ToolDefinition::new(
                        name,
                        "wait for a permit",
                        json!({"type":"object"}),
                    )
                    .unwrap()
                    .with_scheduling(scheduling),
                    output: None,
                    run_policy: rsi_tools_protocol::ToolRunPolicy::execution(Duration::from_secs(
                        30,
                    )),
                    executor: tool.clone(),
                })
                .unwrap(),
        );
    }
    let mut scripts = Vec::new();
    for batch in 0..3 {
        let ids: Vec<_> = (0..10)
            .map(|index| format!("read-{batch}-{index}"))
            .collect();
        let mut calls = Vec::new();
        if batch == 0 {
            calls.push(("hold-first", "hold", "{}"));
        }
        calls.extend(ids.iter().map(|id| (id.as_str(), "read_probe", "{}")));
        scripts.push(StartOutcome::Stream(tool_calls_script(&calls)));
    }
    scripts.push(StartOutcome::Stream(answer_script()));
    let language = stack
        .activate_language("test.loop-limits", language_fixture(&stack, scripts))
        .await;
    let executor = stack
        .activate_executor_with_config(
            json!({"executor_id":"limit","maximum_parallel_tool_calls":2}),
        )
        .await;
    let provider = activate_fixture(
        &stack.runtime,
        "test.settings-memory",
        "test",
        Arc::new(rsi_settings_testkit::MemorySettingsProviderFactory::new(
            json!({}),
        )),
    )
    .await;
    let settings = activate_fixture(
        &stack.runtime,
        "test.settings",
        "test",
        Arc::new(rsi_settings::SettingsFactory),
    )
    .await;
    let registry = stack
        .runtime
        .root()
        .lookup_local::<SettingsContract>()
        .unwrap();
    let scope = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            match registry.scope("rsi.agent-loop") {
                Ok(scope) => break scope,
                Err(rsi_settings_protocol::SettingsError::UnknownNamespace(_)) => {
                    tokio::task::yield_now().await
                }
                Err(error) => panic!("{error}"),
            }
        }
    })
    .await
    .unwrap();
    let base = scope.get().unwrap();
    assert_eq!(base.value["max_parallel_tool_calls"], 2);
    let turns = stack
        .runtime
        .root()
        .lookup_local::<TurnServiceContract>()
        .unwrap();
    let turn = stack
        .submit_fresh(&turns, "limit", "read three batches")
        .await;
    started(&mut events, 1).await;
    let saved = scope
        .replace(base.revision, json!({"max_parallel_tool_calls":10}))
        .await
        .unwrap();
    tool.release.add_permits(1);
    for (batch, expected) in [2, 10, 2].into_iter().enumerate() {
        started(&mut events, expected).await;
        if expected < 10 {
            assert!(
                tokio::time::timeout(Duration::from_millis(150), events.recv())
                    .await
                    .is_err(),
                "batch {batch} admitted a third blocked tool"
            );
        } else {
            assert_eq!(scope.clear(saved.revision).await.unwrap().value, base.value);
        }
        tool.release.add_permits(10);
        started(&mut events, 10 - expected).await;
    }
    tokio::time::timeout(Duration::from_secs(10), turns.wait_idle(&turn.session_id))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        turns
            .outcome(&turn.session_id, &turn.turn_id)
            .await
            .unwrap(),
        Some(TurnOutcome::Completed)
    );
    drop((turns, leases));
    assert!(settings.dispose().await.is_clean());
    assert!(provider.dispose().await.is_clean());
    stack.dispose(language, executor).await;
}
