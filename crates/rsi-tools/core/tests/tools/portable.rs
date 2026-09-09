use super::*;
use rsi_meta::{ActivationPlan, ConfigValue, ContractVersion, PluginFactory, PreparedActivation};

#[derive(Debug)]
struct Registrar(Arc<dyn rsi_tools_protocol::ToolRegistrar>);
#[async_trait]
impl PluginFactory for Registrar {
    fn prepare(&self, config: &ConfigValue) -> rsi_meta::Result<PreparedActivation> {
        Ok(PreparedActivation::new(config.clone()))
    }
    async fn activate(&self, plan: ActivationPlan) -> rsi_meta::Result<()> {
        plan.context()
            .provide_local::<rsi_tools_protocol::ToolRegistrarContract>(Arc::clone(&self.0))?;
        Ok(())
    }
}

use rsi_meta::{Message, ProviderChannel, ServiceEndpoint};
use rsi_tools_protocol::portable::{self, Definition, Request, Response, Scheduling};
use rsi_tools_protocol::{ToolCatalogStage, ToolScheduling};

#[derive(Clone, Copy, Debug)]
enum Behavior {
    Echo,
    Confine,
    Duplicate,
    Extra,
    Missing,
    Cancel,
    ForgedEnforcement,
    InvalidTimeout,
    EmptyDescription,
    InvalidConfine,
}

#[derive(Debug)]
struct Endpoint {
    behavior: Behavior,
    calls: Arc<AtomicUsize>,
    entered: Arc<Semaphore>,
}
impl Endpoint {
    fn definitions(&self) -> Vec<Definition> {
        let definition = Definition {
            definition: ToolDefinition::new(
                "native_echo",
                "Portable echo",
                json!({"type":"object"}),
            )
            .unwrap(),
            timeout_ms: if matches!(self.behavior, Behavior::InvalidTimeout) {
                0
            } else {
                1_000
            },
            scheduling: Scheduling::ParallelSafe,
        };
        if matches!(self.behavior, Behavior::EmptyDescription) {
            Vec::new()
        } else if matches!(self.behavior, Behavior::Duplicate) {
            vec![definition.clone(), definition]
        } else {
            vec![definition]
        }
    }
}
#[async_trait]
impl ServiceEndpoint for Endpoint {
    async fn serve(
        &self,
        _: rsi_meta::InvocationContext,
        mut channel: ProviderChannel<'_>,
    ) -> rsi_meta::Result<()> {
        let message = channel.recv().await.unwrap();
        let request: Request = portable::decode(message.as_bytes()).unwrap();
        match request {
            Request::Describe {} => {
                assert!(channel.recv().await.is_none());
                let tools = self.definitions();
                channel
                    .send(Message::new(
                        portable::encode(&Response::Description { tools }).unwrap(),
                    ))
                    .await?;
            }
            Request::Execute { call, policy } => {
                self.calls.fetch_add(1, Ordering::SeqCst);
                self.entered.add_permits(1);
                assert_eq!(policy, tool_start(CancellationToken::new()).policy);
                if matches!(self.behavior, Behavior::Cancel) {
                    channel.cancellation().cancelled().await;
                    return Ok(());
                }
                if matches!(self.behavior, Behavior::InvalidConfine) {
                    channel.send(Message::new(br#"{"op":"confine","program":"/bin/echo","arguments":[],"mode":"danger-full-access"}"#.as_slice())).await?;
                    return Ok(());
                }
                if matches!(self.behavior, Behavior::Confine) {
                    channel
                        .send(Message::new(
                            portable::encode(&Response::Confine {
                                program: "/bin/echo".into(),
                                arguments: vec!["native".into()],
                            })
                            .unwrap(),
                        ))
                        .await?;
                    let message = channel.recv().await.unwrap();
                    let Request::Confined { plan } = portable::decode(message.as_bytes()).unwrap()
                    else {
                        panic!("expected confined plan")
                    };
                    assert_eq!(
                        plan.program.restore().unwrap(),
                        std::ffi::OsString::from("/bin/echo")
                    );
                    assert_eq!(
                        plan.cwd.restore().unwrap(),
                        std::ffi::OsString::from("/workspace")
                    );
                    assert_eq!(
                        plan.arguments
                            .into_iter()
                            .map(|a| a.restore().unwrap())
                            .collect::<Vec<_>>(),
                        ["native"]
                    );
                }
                if !matches!(self.behavior, Behavior::Missing) {
                    let mut result = ToolResult::new(call.arguments, Vec::new(), false).unwrap();
                    if matches!(self.behavior, Behavior::ForgedEnforcement) {
                        result.enforcement.push(
                            TestSandbox
                                .confine(ProcessRequest {
                                    mode: policy.mode,
                                    program: "/bin/echo".into(),
                                    arguments: Vec::new(),
                                    cwd: policy.cwd,
                                    workspace: policy.workspace,
                                })
                                .await
                                .unwrap()
                                .stamp,
                        );
                    }
                    let response = portable::encode(&Response::Result { result }).unwrap();
                    channel.send(Message::new(response.clone())).await?;
                    if matches!(self.behavior, Behavior::Extra) {
                        channel.send(Message::new(response)).await?;
                    }
                }
            }
            Request::Confined { .. } => panic!("unexpected first message"),
        }
        Ok(())
    }
}
#[derive(Debug)]
struct Native(Arc<Endpoint>);
#[async_trait]
impl PluginFactory for Native {
    fn prepare(&self, config: &ConfigValue) -> rsi_meta::Result<PreparedActivation> {
        Ok(PreparedActivation::new(config.clone()))
    }
    async fn activate(&self, plan: ActivationPlan) -> rsi_meta::Result<()> {
        plan.context().provide(
            "test.native.tools",
            portable::CONTRACT,
            ContractVersion(portable::VERSION),
            self.0.clone(),
        )?;
        Ok(())
    }
}
struct Harness {
    runtime: Runtime,
    stage: Box<dyn ToolCatalogStage>,
    bridge: rsi_meta::FiberHandle,
    endpoint: Arc<Endpoint>,
}
async fn harness(behavior: Behavior) -> Harness {
    let runtime = Runtime::default();
    let root = runtime.root();
    let apply = |name: &str, factory: Arc<dyn PluginFactory>, value| {
        root.apply(
            ResolvedFactory::linked(name, "1", UpdateMode::Replayable, factory),
            value,
        )
    };
    apply("tools", Arc::new(ToolsFactory), Value::Null)
        .await
        .unwrap();
    let stage = runtime
        .root()
        .lookup_local::<ToolCatalogProviderContract>()
        .unwrap()
        .begin_stage()
        .unwrap();
    apply(
        "registrar",
        Arc::new(Registrar(stage.registrar())),
        Value::Null,
    )
    .await
    .unwrap();
    let endpoint = Arc::new(Endpoint {
        behavior,
        calls: Arc::new(AtomicUsize::new(0)),
        entered: Arc::new(Semaphore::new(0)),
    });
    apply("native", Arc::new(Native(endpoint.clone())), Value::Null)
        .await
        .unwrap();
    let bridge = apply(
        "bridge",
        Arc::new(rsi_tools::PortableToolsFactory),
        json!({"service":"test.native.tools"}),
    )
    .await
    .unwrap();
    Harness {
        runtime,
        stage,
        bridge,
        endpoint,
    }
}
fn call() -> ToolCall {
    ToolCall {
        id: "call-1".into(),
        name: "native_echo".into(),
        arguments: json!({"answer":42}),
    }
}
#[tokio::test]
async fn portable_registration_and_execution_use_the_existing_atomic_stage() {
    for behavior in [Behavior::Echo, Behavior::Confine] {
        let Harness {
            runtime,
            stage,
            bridge,
            endpoint,
        } = harness(behavior).await;
        assert_eq!(bridge.snapshot().state, rsi_meta::FiberState::Active);
        let tools = stage.seal().unwrap();
        assert_eq!(tools.definitions().len(), 1);
        assert_eq!(
            tools.definitions()[0].scheduling(),
            ToolScheduling::ParallelSafe
        );
        let prepared = tools.prepare("invocation-1", call()).unwrap();
        let identity = prepared.identity().clone();
        prepared
            .start(tool_start(CancellationToken::new()))
            .await
            .unwrap();
        let RetainedToolResult::Returned(result) = tools
            .wait(&identity, CancellationToken::new())
            .await
            .unwrap()
        else {
            panic!("expected result")
        };
        assert_eq!(result.value, json!({"answer":42}));
        assert_eq!(
            result.enforcement.len(),
            usize::from(matches!(behavior, Behavior::Confine))
        );
        tools.commit(&identity).unwrap();
        assert_eq!(endpoint.calls.load(Ordering::SeqCst), 1);
        drop(tools);
        assert!(runtime.shutdown().await.is_clean());
    }
}
#[tokio::test]
async fn portable_failed_or_retired_activation_leaves_no_partial_registration() {
    for behavior in [
        Behavior::Echo,
        Behavior::Duplicate,
        Behavior::InvalidTimeout,
        Behavior::EmptyDescription,
    ] {
        let Harness {
            runtime,
            stage,
            bridge,
            ..
        } = harness(behavior).await;
        if matches!(behavior, Behavior::Echo) {
            bridge.dispose().await;
        } else {
            assert!(matches!(
                bridge.snapshot().state,
                rsi_meta::FiberState::Failed(_)
            ));
        }
        assert!(stage.seal().unwrap().definitions().is_empty());
        assert!(runtime.shutdown().await.is_clean());
    }
}
#[tokio::test]
async fn portable_extra_or_missing_result_cannot_be_committed_as_success() {
    for behavior in [
        Behavior::Extra,
        Behavior::Missing,
        Behavior::ForgedEnforcement,
        Behavior::InvalidConfine,
    ] {
        let Harness { runtime, stage, .. } = harness(behavior).await;
        let tools = stage.seal().unwrap();
        let prepared = tools.prepare("invocation-1", call()).unwrap();
        let identity = prepared.identity().clone();
        assert!(
            prepared
                .start(tool_start(CancellationToken::new()))
                .await
                .is_err()
        );
        assert!(matches!(
            tools
                .wait(&identity, CancellationToken::new())
                .await
                .unwrap(),
            RetainedToolResult::Failed(_)
        ));
        tools.commit(&identity).unwrap();
        drop(tools);
        assert!(runtime.shutdown().await.is_clean());
    }
}
#[tokio::test]
async fn portable_cancellation_fences_pre_cancelled_calls_and_settles_active_calls() {
    for pre_cancelled in [true, false] {
        let Harness {
            runtime,
            stage,
            endpoint,
            ..
        } = harness(Behavior::Cancel).await;
        let tools = stage.seal().unwrap();
        let prepared = tools.prepare("invocation-1", call()).unwrap();
        let identity = prepared.identity().clone();
        let cancellation = CancellationToken::new();
        if pre_cancelled {
            cancellation.cancel();
        }
        let start = tool_start(cancellation.clone());
        let task = tokio::spawn(async move { prepared.start(start).await });
        if !pre_cancelled {
            endpoint.entered.acquire().await.unwrap().forget();
            cancellation.cancel();
        }
        assert!(task.await.unwrap().is_err());
        let retained = tools
            .wait(&identity, CancellationToken::new())
            .await
            .unwrap();
        assert!(
            matches!(retained, RetainedToolResult::Failed(failure) if failure.kind == RetainedToolFailureKind::Cancelled)
        );
        assert_eq!(
            endpoint.calls.load(Ordering::SeqCst),
            usize::from(!pre_cancelled)
        );
        tools.commit(&identity).unwrap();
        drop(tools);
        assert!(runtime.shutdown().await.is_clean());
    }
}

#[cfg(not(target_family = "wasm"))]
#[tokio::test]
async fn real_native_tool_describe_execute_and_confine_use_the_same_catalog_bridge() {
    use rsi_meta_native_loader::{CatalogOptions, NativeCatalog};
    let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../..");
    let target = root.join("target/native-addon-fixture-test");
    let status = std::process::Command::new(env!("CARGO"))
        .args(["build", "--locked", "--manifest-path"])
        .arg(root.join("fixtures/rsi/native-addon/Cargo.toml"))
        .arg("--target-dir")
        .arg(&target)
        .status()
        .unwrap();
    assert!(status.success());
    let artifact = target.join("debug").join(format!(
        "{}rsi_fixture_native_addon{}",
        std::env::consts::DLL_PREFIX,
        std::env::consts::DLL_SUFFIX
    ));
    let directory = tempfile::tempdir().unwrap();
    let catalog = NativeCatalog::new(CatalogOptions::new(directory.path())).unwrap();
    let runtime = Runtime::default();
    runtime
        .root()
        .apply(
            ResolvedFactory::linked("tools", "1", UpdateMode::Replayable, Arc::new(ToolsFactory)),
            Value::Null,
        )
        .await
        .unwrap();
    let stage = runtime
        .root()
        .lookup_local::<ToolCatalogProviderContract>()
        .unwrap()
        .begin_stage()
        .unwrap();
    runtime
        .root()
        .apply(
            ResolvedFactory::linked(
                "registrar",
                "1",
                UpdateMode::Replayable,
                Arc::new(Registrar(stage.registrar())),
            ),
            Value::Null,
        )
        .await
        .unwrap();
    let native = runtime
        .root()
        .apply(
            catalog.load(artifact).unwrap(),
            json!({"label":"native-v1"}),
        )
        .await
        .unwrap();
    assert_eq!(native.snapshot().state, rsi_meta::FiberState::Active);
    let bridge = runtime
        .root()
        .apply(
            ResolvedFactory::linked(
                "bridge",
                "1",
                UpdateMode::Replayable,
                Arc::new(rsi_tools::PortableToolsFactory),
            ),
            json!({"service":"fixture.native.tools"}),
        )
        .await
        .unwrap();
    assert_eq!(bridge.snapshot().state, rsi_meta::FiberState::Active);
    let tools = stage.seal().unwrap();
    assert_eq!(tools.definitions().len(), 2);
    verify_native_calls(&tools).await;
    drop(tools);
    assert!(runtime.shutdown().await.is_clean());
    drop(bridge);
    drop(native);
    drop(runtime);
    tokio::time::timeout(std::time::Duration::from_secs(10), async {
        while catalog.snapshot().staging_bytes != 0 {
            tokio::time::sleep(std::time::Duration::from_millis(1)).await;
        }
    })
    .await
    .unwrap();
    let resources = catalog.snapshot();
    assert_eq!(resources.active_instances, 0);
    assert_eq!(resources.host_capabilities, 0);
    assert_eq!(resources.host_outputs, 0);
    assert_eq!(resources.retained_failed_finalizations, 0);
}

#[cfg(not(target_family = "wasm"))]
async fn verify_native_calls(tools: &Arc<dyn ToolRuntime>) {
    for name in ["native_echo", "native_confine"] {
        let mut call = call();
        call.name = name.into();
        let prepared = tools.prepare(name, call).unwrap();
        let identity = prepared.identity().clone();
        let result = prepared
            .start(tool_start(CancellationToken::new()))
            .await
            .unwrap();
        assert_eq!(result.value["label"], "native-v1");
        if name == "native_echo" {
            assert_eq!(result.value["arguments"], json!({"answer":42}));
            assert!(result.enforcement.is_empty());
        } else {
            assert_eq!(result.enforcement.len(), 1);
            assert_eq!(
                result.enforcement[0].requested,
                SandboxMode::DangerFullAccess
            );
            let plan: portable::ProcessPlan =
                serde_json::from_value(result.value["plan"].clone()).unwrap();
            assert_eq!(
                plan.program.restore().unwrap(),
                std::ffi::OsString::from("/bin/echo")
            );
        }
        assert_eq!(
            tools
                .wait(&identity, CancellationToken::new())
                .await
                .unwrap(),
            RetainedToolResult::Returned(result)
        );
        tools.commit(&identity).unwrap();
    }
}

#[tokio::test]
async fn sealed_portable_catalog_keeps_definitions_but_retirement_fences_new_calls() {
    let Harness {
        runtime,
        stage,
        bridge,
        endpoint,
    } = harness(Behavior::Echo).await;
    let tools = stage.seal().unwrap();
    let definitions = tools.definitions();
    let prepared = tools.prepare("before-retirement", call()).unwrap();
    let identity = prepared.identity().clone();
    bridge.dispose().await;
    assert_eq!(tools.definitions(), definitions);
    assert!(
        prepared
            .start(tool_start(CancellationToken::new()))
            .await
            .is_err()
    );
    let RetainedToolResult::Failed(failure) = tools
        .wait(&identity, CancellationToken::new())
        .await
        .unwrap()
    else {
        panic!("expected failure")
    };
    assert_eq!(failure.kind, RetainedToolFailureKind::Execution);
    assert_eq!(endpoint.calls.load(Ordering::SeqCst), 0);
    tools.commit(&identity).unwrap();
    drop(tools);
    assert!(runtime.shutdown().await.is_clean());
}
