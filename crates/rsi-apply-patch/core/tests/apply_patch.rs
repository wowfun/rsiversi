#![cfg(target_os = "linux")]

use async_trait::async_trait;
use rsi_apply_patch::{
    ApplyPatchToolFactory, MAXIMUM_APPLY_PATCH_BYTES, maybe_run_apply_patch_helper,
};
use rsi_meta::{
    ActivationPlan, ConfigValue, FiberHandle, MetaError, PluginFactory, PreparedActivation,
    ResolvedFactory, Runtime, UpdateMode,
};
use rsi_process::{
    ManagedProcess, Process, ProcessControl, ProcessOutcome, ProcessOutput, ProcessRead,
    ProcessSpec,
};
use rsi_process_local::ProcessLocalFactory;
use rsi_sandbox::{Sandbox, SandboxContract, SandboxMode};
use rsi_sandbox_local::SandboxLocalFactory;
use rsi_tools::ToolsFactory;
use rsi_tools_protocol::{
    PreparedToolCall, RetainedToolResult, ToolCall, ToolCatalogProviderContract,
    ToolExecutionPolicy, ToolRegistrar, ToolRegistrarContract, ToolResultIdentity, ToolRuntime,
    ToolStart,
};
use serde_json::{Value, json};
use std::ffi::OsString;
use std::os::unix::fs::PermissionsExt as _;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::time::Duration;
use tempfile::TempDir;
use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;

static NEXT_CALL: AtomicU64 = AtomicU64::new(0);

#[derive(Clone, Debug)]
struct RegistrarSupplyFactory {
    registrar: Arc<dyn ToolRegistrar>,
}

#[async_trait]
impl PluginFactory for RegistrarSupplyFactory {
    fn prepare(&self, desired: &ConfigValue) -> rsi_meta::Result<PreparedActivation> {
        if !desired.is_null() {
            return Err(MetaError::InvalidInput(
                "test registrar configuration must be null".into(),
            ));
        }
        Ok(PreparedActivation::new(Value::Null))
    }

    async fn activate(&self, plan: ActivationPlan) -> rsi_meta::Result<()> {
        let supply = plan
            .context()
            .provide_local::<ToolRegistrarContract>(Arc::clone(&self.registrar))?;
        plan.defer(
            "withdraw test Tool registrar",
            Box::new(move || {
                Box::pin(async move {
                    drop(supply);
                    Ok(())
                })
            }),
        )
    }
}

#[derive(Clone, Debug)]
struct ProcessSupplyFactory {
    process: Arc<dyn Process>,
}

#[async_trait]
impl PluginFactory for ProcessSupplyFactory {
    fn prepare(&self, desired: &ConfigValue) -> rsi_meta::Result<PreparedActivation> {
        if !desired.is_null() {
            return Err(MetaError::InvalidInput(
                "test Process supply configuration must be null".into(),
            ));
        }
        Ok(PreparedActivation::new(Value::Null))
    }

    async fn activate(&self, plan: ActivationPlan) -> rsi_meta::Result<()> {
        let supply = plan
            .context()
            .provide_local::<rsi_process::ProcessContract>(Arc::clone(&self.process))?;
        plan.defer(
            "withdraw test Process supply",
            Box::new(move || {
                Box::pin(async move {
                    drop(supply);
                    Ok(())
                })
            }),
        )
    }
}

#[derive(Debug, Default)]
struct EmptyProcessOutput;

impl ProcessOutput for EmptyProcessOutput {
    fn read_from(&self, offset: u64) -> rsi_process::Result<ProcessRead> {
        Ok(ProcessRead {
            bytes: Vec::new(),
            oldest_offset: offset,
            next_offset: offset,
            lossy: false,
        })
    }
}

#[derive(Debug)]
struct BlockingProcessControl {
    terminated: AtomicBool,
    wait_started: AtomicBool,
    wait_returned_after_termination: AtomicBool,
    state_changed: Notify,
    output: Arc<EmptyProcessOutput>,
}

impl BlockingProcessControl {
    fn new() -> Self {
        Self {
            terminated: AtomicBool::new(false),
            wait_started: AtomicBool::new(false),
            wait_returned_after_termination: AtomicBool::new(false),
            state_changed: Notify::new(),
            output: Arc::new(EmptyProcessOutput),
        }
    }

    async fn wait_until_started(&self) {
        loop {
            let changed = self.state_changed.notified();
            if self.wait_started.load(Ordering::Acquire) {
                return;
            }
            changed.await;
        }
    }
}

#[async_trait]
impl ProcessControl for BlockingProcessControl {
    fn pid(&self) -> u32 {
        42
    }

    fn stdout(&self) -> Arc<dyn ProcessOutput> {
        self.output.clone()
    }

    fn stderr(&self) -> Arc<dyn ProcessOutput> {
        self.output.clone()
    }

    fn terminate(&self) {
        self.terminated.store(true, Ordering::Release);
        self.state_changed.notify_waiters();
    }

    async fn wait(&self) -> rsi_process::Result<ProcessOutcome> {
        self.wait_started.store(true, Ordering::Release);
        self.state_changed.notify_waiters();
        loop {
            let changed = self.state_changed.notified();
            if self.terminated.load(Ordering::Acquire) {
                self.wait_returned_after_termination
                    .store(true, Ordering::Release);
                return Ok(ProcessOutcome {
                    exit_code: None,
                    signal: Some(15),
                });
            }
            changed.await;
        }
    }
}

#[derive(Debug)]
struct BlockingProcess {
    control: Arc<BlockingProcessControl>,
    spawns: AtomicUsize,
    effect_marker: Option<PathBuf>,
}

impl BlockingProcess {
    fn new() -> Self {
        Self {
            control: Arc::new(BlockingProcessControl::new()),
            spawns: AtomicUsize::new(0),
            effect_marker: None,
        }
    }

    fn with_effect_marker(effect_marker: PathBuf) -> Self {
        Self {
            control: Arc::new(BlockingProcessControl::new()),
            spawns: AtomicUsize::new(0),
            effect_marker: Some(effect_marker),
        }
    }
}

impl Process for BlockingProcess {
    fn spawn(&self, spec: ProcessSpec) -> rsi_process::Result<ManagedProcess> {
        spec.validate()?;
        self.spawns.fetch_add(1, Ordering::AcqRel);
        if let Some(marker) = &self.effect_marker {
            std::fs::write(marker, b"mutated")
                .map_err(|error| rsi_process::ProcessError::Spawn(error.to_string()))?;
        }
        let control: Arc<dyn ProcessControl> = self.control.clone();
        Ok(ManagedProcess::new(control))
    }
}

struct Fixture {
    runtime: Runtime,
    fibers: Vec<FiberHandle>,
    tools: Arc<dyn ToolRuntime>,
    sandbox: Arc<dyn Sandbox>,
    workspace: TempDir,
    _helper_root: TempDir,
}

impl Fixture {
    async fn activate() -> Self {
        Self::activate_with_process(None).await
    }

    async fn activate_with_process(process: Option<Arc<dyn Process>>) -> Self {
        let helper_root = tempfile::tempdir().unwrap();
        let helper = helper_root.path().join("apply-patch-helper");
        std::fs::write(
            &helper,
            br#"#!/bin/sh
[ "$#" -eq 1 ] && [ "$1" = "--rsi-run-as-apply-patch" ] || exit 2
/bin/cat >/dev/null
printf '%s\n' '{"status":"applied","delta_exact":true,"effects":[{"operation":0,"kind":"add","path":"added.txt","bytes_before":null,"bytes_after":6}],"fuzzy_matches":[],"failure":null}'
"#,
        )
        .unwrap();
        std::fs::set_permissions(&helper, std::fs::Permissions::from_mode(0o755)).unwrap();

        let runtime = Runtime::default();
        let mut fibers = Vec::new();
        fibers.push(
            runtime
                .root()
                .apply(
                    linked("sandbox", Arc::new(SandboxLocalFactory::default())),
                    json!({"bubblewrap":[],"landlock":[]}),
                )
                .await
                .unwrap(),
        );
        let process_factory: Arc<dyn PluginFactory> = process.map_or_else(
            || Arc::new(ProcessLocalFactory) as Arc<dyn PluginFactory>,
            |process| Arc::new(ProcessSupplyFactory { process }),
        );
        fibers.push(
            runtime
                .root()
                .apply(linked("process", process_factory), Value::Null)
                .await
                .unwrap(),
        );
        fibers.push(
            runtime
                .root()
                .apply(linked("tools", Arc::new(ToolsFactory)), Value::Null)
                .await
                .unwrap(),
        );
        let provider = runtime
            .root()
            .lookup_local::<ToolCatalogProviderContract>()
            .unwrap();
        let stage = provider.begin_stage().unwrap();
        fibers.push(
            runtime
                .root()
                .apply(
                    linked(
                        "test-registrar",
                        Arc::new(RegistrarSupplyFactory {
                            registrar: stage.registrar(),
                        }),
                    ),
                    Value::Null,
                )
                .await
                .unwrap(),
        );
        fibers.push(
            runtime
                .root()
                .apply(
                    linked(
                        "apply-patch",
                        Arc::new(ApplyPatchToolFactory::new(helper).unwrap()),
                    ),
                    Value::Null,
                )
                .await
                .unwrap(),
        );
        let tools = stage.seal().unwrap();
        let sandbox = runtime.root().lookup_local::<SandboxContract>().unwrap();
        let workspace = tempfile::tempdir().unwrap();
        Self {
            runtime,
            fibers,
            tools,
            sandbox,
            workspace,
            _helper_root: helper_root,
        }
    }

    async fn call(
        &self,
        patch: &str,
    ) -> rsi_tools_protocol::Result<rsi_tools_protocol::ToolResult> {
        let (prepared, _) = self.prepare_call(patch);
        prepared
            .start(self.tool_start(CancellationToken::new()))
            .await
    }

    fn prepare_call(&self, patch: &str) -> (Box<dyn PreparedToolCall>, ToolResultIdentity) {
        let number = NEXT_CALL.fetch_add(1, Ordering::AcqRel) + 1;
        let prepared = self
            .tools
            .prepare(
                &format!("invocation-{number}"),
                ToolCall {
                    id: format!("call-{number}"),
                    name: "apply_patch".into(),
                    arguments: json!({"patch":patch}),
                },
            )
            .unwrap();
        let identity = prepared.identity().clone();
        (prepared, identity)
    }

    fn tool_start(&self, cancellation: CancellationToken) -> ToolStart {
        ToolStart {
            cancellation,
            policy: ToolExecutionPolicy {
                mode: SandboxMode::DangerFullAccess,
                cwd: self.workspace.path().canonicalize().unwrap(),
                workspace: self.workspace.path().canonicalize().unwrap(),
            },
            sandbox: Arc::clone(&self.sandbox),
            job_scope: None,
        }
    }

    async fn shutdown(mut self) {
        drop((self.tools, self.sandbox));
        while let Some(fiber) = self.fibers.pop() {
            assert!(fiber.dispose().await.is_clean());
        }
        assert!(self.runtime.shutdown().await.is_complete());
    }
}

fn linked(name: &str, factory: Arc<dyn PluginFactory>) -> ResolvedFactory {
    ResolvedFactory::linked(name, "test", UpdateMode::Replayable, factory)
}

async fn settle_blocked_call(
    mut call: tokio::task::JoinHandle<rsi_tools_protocol::Result<rsi_tools_protocol::ToolResult>>,
    control: &BlockingProcessControl,
) -> (
    bool,
    rsi_tools_protocol::Result<rsi_tools_protocol::ToolResult>,
) {
    if let Ok(result) = tokio::time::timeout(Duration::from_millis(100), &mut call).await {
        (true, result.expect("Tool settlement task does not panic"))
    } else {
        control.terminate();
        (
            false,
            call.await.expect("Tool settlement task does not panic"),
        )
    }
}

#[test]
fn helper_dispatch_and_factory_provenance_fail_closed() {
    assert_eq!(maybe_run_apply_patch_helper(Vec::<OsString>::new()), None);
    assert_eq!(
        maybe_run_apply_patch_helper([
            OsString::from("--rsi-run-as-apply-patch"),
            OsString::from("extra"),
        ]),
        None
    );
    assert!(ApplyPatchToolFactory::new(PathBuf::from("relative-helper")).is_err());
    assert!(
        ApplyPatchToolFactory::new(PathBuf::from("/definitely/missing/rsi-apply-patch")).is_err()
    );
}

#[tokio::test]
async fn factory_publishes_only_apply_patch_and_joins_the_exact_helper_protocol() {
    let fixture = Fixture::activate().await;
    let definitions = fixture.tools.definitions();
    assert_eq!(
        definitions
            .iter()
            .map(rsi_tools_protocol::ToolDefinition::name)
            .collect::<Vec<_>>(),
        ["apply_patch"]
    );
    assert_eq!(
        definitions[0].description(),
        "Apply one bounded structured patch relative to the tool cwd. The helper preflights every operation; a later commit failure returns partial effects and is never replayed automatically."
    );
    assert_eq!(
        definitions[0].input_schema(),
        &json!({
            "type":"object",
            "properties":{"patch":{"type":"string"}},
            "required":["patch"],
            "additionalProperties":false
        })
    );

    let applied = fixture
        .call("*** Begin Patch\n*** Add File: added.txt\n+value\n*** End Patch\n")
        .await
        .unwrap();
    assert!(!applied.is_error);
    assert_eq!(applied.value["status"], "applied");
    assert_eq!(applied.value["effects"][0]["path"], "added.txt");
    assert_eq!(applied.enforcement.len(), 1);

    let invalid = fixture.call("not\0a patch").await.unwrap();
    assert!(invalid.is_error);
    assert_eq!(invalid.value["code"], "invalid_patch_text");
    assert!(invalid.enforcement.is_empty());

    let too_large_patch = "x".repeat(MAXIMUM_APPLY_PATCH_BYTES + 1);
    let too_large = fixture.call(&too_large_patch).await.unwrap();
    assert!(too_large.is_error);
    assert_eq!(too_large.value["code"], "patch_too_large");
    assert!(too_large.enforcement.is_empty());
    fixture.shutdown().await;
}

#[tokio::test(start_paused = true)]
async fn external_cancellation_reports_unknown_effects_after_reaping_the_helper() {
    let temporary = tempfile::tempdir().unwrap();
    let marker = temporary.path().join("committed-prefix");
    let process = Arc::new(BlockingProcess::with_effect_marker(marker.clone()));
    let fixture = Fixture::activate_with_process(Some(process.clone())).await;
    let (prepared, identity) =
        fixture.prepare_call("*** Begin Patch\n*** Add File: added.txt\n+value\n*** End Patch\n");
    let cancellation = CancellationToken::new();
    let call = tokio::spawn(prepared.start(fixture.tool_start(cancellation.clone())));
    process.control.wait_until_started().await;

    cancellation.cancel();
    let (settled_promptly, result) = settle_blocked_call(call, &process.control).await;
    let retained = fixture.tools.query(&identity).unwrap();
    fixture.tools.commit(&identity).unwrap();
    let spawned_once = process.spawns.load(Ordering::Acquire) == 1;
    let terminated = process.control.terminated.load(Ordering::Acquire);
    let reaped = process
        .control
        .wait_returned_after_termination
        .load(Ordering::Acquire);
    fixture.shutdown().await;

    assert_eq!(std::fs::read(marker).unwrap(), b"mutated");
    assert!(settled_promptly, "cancellation left the helper running");
    assert!(matches!(
        result,
        Ok(ref result)
            if result.is_error
                && result.value["code"] == "effects_unknown"
                && result.value["replay_safe"] == false
    ));
    assert!(matches!(
        retained,
        RetainedToolResult::Returned(result)
            if result.is_error
                && result.value["code"] == "effects_unknown"
                && result.value["replay_safe"] == false
    ));
    assert!(spawned_once);
    assert!(terminated);
    assert!(reaped, "Tool settlement raced ahead of helper reaping");
}

#[tokio::test(start_paused = true)]
async fn tool_timeout_reports_unknown_effects_after_reaping_the_helper() {
    let process = Arc::new(BlockingProcess::new());
    let fixture = Fixture::activate_with_process(Some(process.clone())).await;
    let (prepared, identity) =
        fixture.prepare_call("*** Begin Patch\n*** Add File: added.txt\n+value\n*** End Patch\n");
    let call = tokio::spawn(prepared.start(fixture.tool_start(CancellationToken::new())));
    process.control.wait_until_started().await;

    tokio::time::advance(Duration::from_secs(45)).await;
    let (settled_promptly, result) = settle_blocked_call(call, &process.control).await;
    let retained = fixture.tools.query(&identity).unwrap();
    fixture.tools.commit(&identity).unwrap();
    let spawned_once = process.spawns.load(Ordering::Acquire) == 1;
    let terminated = process.control.terminated.load(Ordering::Acquire);
    let reaped = process
        .control
        .wait_returned_after_termination
        .load(Ordering::Acquire);
    fixture.shutdown().await;

    assert!(settled_promptly, "Tool timeout left the helper running");
    assert!(matches!(
        result,
        Ok(ref result)
            if result.is_error
                && result.value["code"] == "effects_unknown"
                && result.value["replay_safe"] == false
    ));
    assert!(matches!(
        retained,
        RetainedToolResult::Returned(result)
            if result.is_error
                && result.value["code"] == "effects_unknown"
                && result.value["replay_safe"] == false
    ));
    assert!(spawned_once);
    assert!(terminated);
    assert!(reaped, "Tool settlement raced ahead of helper reaping");
}
