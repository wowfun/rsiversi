#![cfg(target_os = "linux")]
use async_trait::async_trait;
use futures_util::FutureExt as _;
use rsi_agent_program::{
    AdmittedProgram, ProgramRpc, ProgramRuntimeContract, ProgramRuntimeFactory,
};
use rsi_jobs::{JobScopeAuthority, JobScopeId, JobStatus, Jobs, JobsContract};
use rsi_meta::{FiberHandle, PluginFactory, ResolvedFactory, Runtime, UpdateMode};
use rsi_sandbox::{SandboxContract, SandboxMode};
use rsi_tools_protocol::{ToolExecution, ToolExecutionExtensions, ToolExecutionPolicy, ToolStart};
use serde_json::{Value, json};
use std::{sync::Arc, time::Duration};
use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;

struct Fixture {
    runtime: Runtime,
    fibers: Vec<FiberHandle>,
    jobs: Arc<dyn Jobs>,
    scope: JobScopeAuthority,
    workspace: tempfile::TempDir,
}

#[derive(Debug)]
struct PanickingEffect {
    construction: bool,
    effects: std::sync::atomic::AtomicUsize,
}
impl ProgramRpc for PanickingEffect {
    fn definitions(&self) -> Value {
        json!([])
    }
    fn call<'life0, 'async_trait>(
        &'life0 self,
        _: String,
        _: Value,
        _: CancellationToken,
    ) -> std::pin::Pin<
        Box<
            dyn std::future::Future<Output = Result<Value, rsi_agent_program::ProgramError>>
                + Send
                + 'async_trait,
        >,
    >
    where
        'life0: 'async_trait,
        Self: 'async_trait,
    {
        if self.construction {
            self.effects
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            panic!("injected callback construction panic after effect");
        }
        Box::pin(async move {
            self.effects
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            panic!("injected callback polling panic after effect");
        })
    }
}

#[tokio::test]
#[ignore = "requires explicit RSI_TEST_NODE native integration"]
async fn actual_node_cannot_retry_panicked_effect_in_construction_or_polling() {
    for construction in [false, true] {
        let fixture = Fixture::new().await;
        let rpc = Arc::new(PanickingEffect {
            construction,
            effects: std::sync::atomic::AtomicUsize::default(),
        });
        let mut program = fixture.prepare(
            "try { await tools.call('effect', {}); } catch { require('fs').writeFileSync('retried', 'yes'); await tools.call('effect', {}); } return 'continued';", rpc.clone()).await;
        program.start().unwrap();
        let (first, second) = tokio::time::timeout(Duration::from_secs(5), async {
            tokio::join!(program.result(), program.result())
        })
        .await
        .unwrap();
        assert_eq!(first, Err(rsi_agent_program::ProgramError::OutcomeUnknown));
        assert_eq!(second, first);
        assert_eq!(rpc.effects.load(std::sync::atomic::Ordering::SeqCst), 1);
        assert!(!fixture.workspace.path().join("retried").exists());
        assert_eq!(
            fixture
                .jobs
                .wait(&fixture.scope, program.job_id(), 0, 0)
                .await
                .unwrap()
                .job
                .status,
            JobStatus::OutcomeUnknown
        );
        fixture.close().await;
    }
}

#[derive(Debug)]
struct DefinitionsPanic;
#[async_trait]
impl ProgramRpc for DefinitionsPanic {
    fn definitions(&self) -> Value {
        panic!("injected pure definitions panic");
    }
    async fn call(
        &self,
        _: String,
        _: Value,
        _: CancellationToken,
    ) -> Result<Value, rsi_agent_program::ProgramError> {
        unreachable!()
    }
}

#[tokio::test]
#[ignore = "requires explicit RSI_TEST_NODE native integration"]
async fn definitions_panic_refuses_job_admission() {
    let fixture = Fixture::new().await;
    let execution = fixture.execution(
        CancellationToken::new(),
        fixture
            .runtime
            .root()
            .lookup_local::<SandboxContract>()
            .unwrap(),
    );
    let runtime = fixture
        .runtime
        .root()
        .lookup_local::<ProgramRuntimeContract>()
        .unwrap();
    let process = runtime.prepare_process(&execution).await.unwrap();
    let result = runtime
        .admit(
            "return 42".into(),
            &execution,
            &fixture.scope,
            Arc::new(DefinitionsPanic),
            process,
        )
        .await;
    assert!(
        matches!(result, Err(rsi_agent_program::ProgramError::Failed(message)) if message.contains("before admission"))
    );
    assert!(fixture.jobs.list(&fixture.scope).unwrap().is_empty());
    fixture.close().await;
}

#[derive(Debug, Default)]
struct Noncooperative {
    entered: Notify,
    release: CancellationToken,
    pid: std::sync::atomic::AtomicUsize,
}
#[async_trait]
impl ProgramRpc for Noncooperative {
    fn definitions(&self) -> Value {
        json!([])
    }
    async fn call(
        &self,
        _: String,
        args: Value,
        _: CancellationToken,
    ) -> Result<Value, rsi_agent_program::ProgramError> {
        self.pid.store(
            usize::try_from(args["arguments"]["pid"].as_u64().unwrap()).unwrap(),
            std::sync::atomic::Ordering::SeqCst,
        );
        self.entered.notify_one();
        self.release.cancelled().await;
        Err("handler released".into())
    }
}

#[tokio::test]
#[ignore = "requires explicit RSI_TEST_NODE native integration"]
async fn cancellation_terminates_node_before_noncooperative_rpc_settles() {
    let fixture = Fixture::new().await;
    let gate = Arc::new(Noncooperative::default());
    let mut program = fixture
        .prepare(
            "return await tools.call('gate', {pid:process.pid})",
            gate.clone(),
        )
        .await;
    program.start().unwrap();
    tokio::time::timeout(Duration::from_secs(5), gate.entered.notified())
        .await
        .unwrap();
    let pid = gate.pid.load(std::sync::atomic::Ordering::SeqCst);
    assert!(pid > 0);
    program.cancel();
    tokio::time::timeout(Duration::from_secs(5), async {
        while std::path::Path::new(&format!("/proc/{pid}")).exists() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert!(
        program.result().now_or_never().is_none(),
        "unsettled RPC must remain owned"
    );
    gate.release.cancel();
    assert!(
        tokio::time::timeout(Duration::from_secs(5), program.result())
            .await
            .unwrap()
            .is_err()
    );
    assert_eq!(
        fixture
            .jobs
            .wait(&fixture.scope, program.job_id(), 0, 0)
            .await
            .unwrap()
            .job
            .status,
        JobStatus::Cancelled
    );
    fixture.close().await;
}
impl Fixture {
    async fn new() -> Self {
        Self::with_node(
            std::env::var("RSI_TEST_NODE")
                .expect("explicit absolute Node executable")
                .into(),
        )
        .await
    }
    async fn with_node(node: std::path::PathBuf) -> Self {
        let runtime = Runtime::default();
        let mut fibers = vec![];
        for (name, factory, config) in [
            (
                "sandbox",
                Arc::new(rsi_sandbox_local::SandboxLocalFactory::default())
                    as Arc<dyn PluginFactory>,
                json!({"bubblewrap":[], "landlock":[]}),
            ),
            (
                "process",
                Arc::new(rsi_process_local::ProcessLocalFactory),
                Value::Null,
            ),
            (
                "jobs",
                Arc::new(rsi_jobs_local::JobsLocalFactory),
                Value::Null,
            ),
            (
                "program",
                Arc::new(ProgramRuntimeFactory),
                json!({"node":node}),
            ),
        ] {
            fibers.push(
                runtime
                    .root()
                    .apply(
                        ResolvedFactory::linked(name, "test", UpdateMode::Replayable, factory),
                        config,
                    )
                    .await
                    .unwrap(),
            );
        }
        let jobs = runtime.root().lookup_local::<JobsContract>().unwrap();
        let scope = jobs
            .acquire_scope(JobScopeId::new("test", ["program"]).unwrap())
            .unwrap();
        Self {
            runtime,
            fibers,
            jobs,
            scope,
            workspace: tempfile::tempdir().unwrap(),
        }
    }
    async fn prepare(&self, script: &str, rpc: Arc<dyn ProgramRpc>) -> AdmittedProgram {
        self.prepare_with_token(script, rpc, CancellationToken::new())
            .await
    }
    fn execution(
        &self,
        cancellation: CancellationToken,
        sandbox: Arc<dyn rsi_sandbox::Sandbox>,
    ) -> ToolExecution {
        let (execution, _) = ToolExecution::from_start(
            "test".into(),
            ToolStart {
                cancellation,
                sandbox,
                policy: ToolExecutionPolicy {
                    mode: SandboxMode::DangerFullAccess,
                    cwd: self.workspace.path().into(),
                    workspace: self.workspace.path().into(),
                },
                job_scope: Some(self.scope.clone()),
                extensions: ToolExecutionExtensions::default(),
            },
        )
        .unwrap();
        execution
    }
    async fn prepare_with_token(
        &self,
        script: &str,
        rpc: Arc<dyn ProgramRpc>,
        cancellation: CancellationToken,
    ) -> AdmittedProgram {
        let execution = self.execution(
            cancellation,
            self.runtime
                .root()
                .lookup_local::<SandboxContract>()
                .unwrap(),
        );
        let runtime = self
            .runtime
            .root()
            .lookup_local::<ProgramRuntimeContract>()
            .unwrap();
        let process = runtime.prepare_process(&execution).await.unwrap();
        runtime
            .admit(script.into(), &execution, &self.scope, rpc, process)
            .await
            .unwrap()
    }
    async fn close(self) {
        self.jobs.finalize_scope(&self.scope).await.unwrap();
        for fiber in self.fibers.into_iter().rev() {
            assert!(fiber.dispose().await.is_clean());
        }
    }
}
#[derive(Debug, Default)]
struct Echo;
#[async_trait]
impl ProgramRpc for Echo {
    fn definitions(&self) -> Value {
        json!([])
    }
    async fn call(
        &self,
        method: String,
        value: Value,
        _: CancellationToken,
    ) -> Result<Value, rsi_agent_program::ProgramError> {
        assert_eq!(method, "tool");
        Ok(value["arguments"].clone())
    }
}
#[tokio::test]
#[ignore = "requires explicit RSI_TEST_NODE native integration"]
async fn real_node_frames_large_results_and_keeps_curated_output() {
    let fixture = Fixture::new().await;
    let mut program = fixture.prepare("const v = await tools.call('echo', {text:'汉'.repeat(24000)}); return {bytes: Buffer.byteLength(v.text), pid: process.pid, inherited: process.env.NODE_OPTIONS ?? null};", Arc::new(Echo)).await;
    program.start().unwrap();
    let value = tokio::time::timeout(Duration::from_secs(5), program.result())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(value["bytes"], 72000);
    assert_eq!(value["inherited"], Value::Null);
    let pid = value["pid"].as_u64().unwrap();
    assert!(
        !std::path::Path::new(&format!("/proc/{pid}")).exists(),
        "result waits for direct child reaping"
    );
    assert_eq!(
        fixture
            .jobs
            .wait(&fixture.scope, program.job_id(), 0, 0)
            .await
            .unwrap()
            .job
            .status,
        JobStatus::Completed
    );
    drop(program);
    fixture.close().await;
}
#[tokio::test]
#[ignore = "requires explicit RSI_TEST_NODE native integration"]
async fn stderr_tail_keeps_whole_stream_coordinates() {
    let fixture = Fixture::new().await;
    let mut program = fixture
        .prepare(
            "require('fs').writeSync(2, 'x'.repeat(96 * 1024)); return 42;",
            Arc::new(Echo),
        )
        .await;
    program.start().unwrap();
    assert_eq!(program.result().await.unwrap(), json!(42));
    let read = fixture
        .jobs
        .wait(&fixture.scope, program.job_id(), 0, 0)
        .await
        .unwrap();
    assert_eq!(read.stderr.bytes.len(), 64 * 1024);
    assert_eq!(read.stderr.oldest_offset, 32 * 1024);
    assert_eq!(read.stderr.next_offset, 96 * 1024);
    assert!(read.stderr.lossy);
    drop(program);
    fixture.close().await;
}

#[tokio::test]
#[ignore = "requires explicit RSI_TEST_NODE native integration"]
async fn admission_does_not_start_and_cancel_before_latch_never_executes_script() {
    let fixture = Fixture::new().await;
    let mut program = fixture
        .prepare(
            "require('fs').writeFileSync('started', 'bad'); return 42",
            Arc::new(Echo),
        )
        .await;
    assert!(!fixture.workspace.path().join("started").exists());
    program.cancel();
    assert!(program.start().is_err());
    assert!(
        tokio::time::timeout(Duration::from_secs(2), program.result())
            .await
            .unwrap()
            .is_err()
    );
    assert!(!fixture.workspace.path().join("started").exists());
    fixture.close().await;
}
#[derive(Debug, Default)]
struct Gate {
    entered: Notify,
    joined: CancellationToken,
}
#[async_trait]
impl ProgramRpc for Gate {
    fn definitions(&self) -> Value {
        json!([])
    }
    async fn call(
        &self,
        _: String,
        _: Value,
        cancellation: CancellationToken,
    ) -> Result<Value, rsi_agent_program::ProgramError> {
        self.entered.notify_one();
        cancellation.cancelled().await;
        self.joined.cancel();
        Err("cancelled".into())
    }
}
#[tokio::test]
#[ignore = "requires explicit RSI_TEST_NODE native integration"]
async fn cancellation_joins_admitted_rpc_and_process_before_terminal() {
    let fixture = Fixture::new().await;
    let gate = Arc::new(Gate::default());
    let mut program = fixture
        .prepare("return await tools.call('gate', {})", gate.clone())
        .await;
    program.start().unwrap();
    tokio::time::timeout(Duration::from_secs(5), gate.entered.notified())
        .await
        .unwrap();
    program.cancel();
    assert!(
        tokio::time::timeout(Duration::from_secs(5), program.result())
            .await
            .unwrap()
            .is_err()
    );
    assert!(gate.joined.is_cancelled());
    assert_eq!(
        fixture
            .jobs
            .wait(&fixture.scope, program.job_id(), 0, 0)
            .await
            .unwrap()
            .job
            .status,
        JobStatus::Cancelled
    );
    fixture.close().await;
}
#[tokio::test]
#[ignore = "requires explicit RSI_TEST_NODE native integration"]
async fn result_and_rpc_capacity_are_enforced_and_owner_retirement_cancels_unstarted_job() {
    let mut fixture = Fixture::new().await;
    for (script, expected) in [
        ("return 'x'.repeat(262144)", "result exceeds"),
        (
            "return await Promise.all(Array.from({length:17}, () => tools.call('gate', {})))",
            "capacity",
        ),
    ] {
        let mut program = fixture.prepare(script, Arc::new(Gate::default())).await;
        program.start().unwrap();
        let error = tokio::time::timeout(Duration::from_secs(5), program.result())
            .await
            .unwrap()
            .unwrap_err();
        assert!(error.to_string().contains(expected), "{error}");
    }
    let program = fixture.prepare("return 42", Arc::new(Echo)).await;
    assert!(fixture.fibers.pop().unwrap().dispose().await.is_clean());
    assert!(program.result().await.is_err());
    fixture.close().await;
}

#[derive(Debug)]
struct GatedSandbox {
    inner: Arc<dyn rsi_sandbox::Sandbox>,
    entered: Notify,
    release: Notify,
}
#[async_trait]
impl rsi_sandbox::Sandbox for GatedSandbox {
    async fn workspace_read(
        &self,
        request: rsi_sandbox::WorkspaceReadRequest,
    ) -> rsi_sandbox::Result<rsi_sandbox::WorkspaceReadScope> {
        self.inner.workspace_read(request).await
    }
    async fn confine(
        &self,
        request: rsi_sandbox::ProcessRequest,
    ) -> rsi_sandbox::Result<rsi_sandbox::ConfinedProcess> {
        self.entered.notify_one();
        self.release.notified().await;
        self.inner.confine(request).await
    }
}
#[tokio::test]
#[ignore = "requires explicit RSI_TEST_NODE native integration"]
async fn tool_cancellation_during_confinement_or_after_admission_cannot_launch_node() {
    let fixture = Fixture::new().await;
    let sandbox = Arc::new(GatedSandbox {
        inner: fixture
            .runtime
            .root()
            .lookup_local::<SandboxContract>()
            .unwrap(),
        entered: Notify::new(),
        release: Notify::new(),
    });
    let cancellation = CancellationToken::new();
    let execution = fixture.execution(cancellation.clone(), sandbox.clone());
    let runtime = fixture
        .runtime
        .root()
        .lookup_local::<ProgramRuntimeContract>()
        .unwrap();
    let script = "require('fs').writeFileSync('started', 'bad'); return 42";
    {
        let preparing = runtime.prepare_process(&execution);
        tokio::pin!(preparing);
        tokio::select! {
            () = sandbox.entered.notified() => {}
            result = &mut preparing => panic!("preparation passed closed confinement: {result:?}"),
        }
        cancellation.cancel();
        sandbox.release.notify_one();
        assert!(preparing.await.is_err());
    }
    assert!(fixture.jobs.list(&fixture.scope).unwrap().is_empty());
    let cancellation = CancellationToken::new();
    let mut program = fixture
        .prepare_with_token(script, Arc::new(Echo), cancellation.clone())
        .await;
    cancellation.cancel();
    assert!(program.start().is_err());
    assert!(
        tokio::time::timeout(Duration::from_secs(2), program.result())
            .await
            .unwrap()
            .is_err()
    );
    assert!(!fixture.workspace.path().join("started").exists());
    fixture.close().await;
}

#[derive(Debug)]
struct UncertainRpc;
#[async_trait]
impl ProgramRpc for UncertainRpc {
    fn definitions(&self) -> Value {
        json!([])
    }
    async fn call(
        &self,
        _: String,
        _: Value,
        _: CancellationToken,
    ) -> Result<Value, rsi_agent_program::ProgramError> {
        Err(rsi_agent_program::ProgramError::OutcomeUnknown)
    }
}
#[derive(Debug)]
struct KnownRefusalRpc(rsi_agent_turn_protocol::TurnError);
#[async_trait]
impl ProgramRpc for KnownRefusalRpc {
    fn definitions(&self) -> Value {
        json!([])
    }
    async fn call(
        &self,
        _: String,
        _: Value,
        _: CancellationToken,
    ) -> Result<Value, rsi_agent_program::ProgramError> {
        Err(self.0.clone().into())
    }
}
#[tokio::test]
#[ignore = "requires explicit RSI_TEST_NODE native integration"]
async fn actual_node_can_catch_known_rpc_refusals_and_return_a_curated_result() {
    let fixture = Fixture::new().await;
    for refusal in [
        rsi_agent_turn_protocol::TurnError::Cancelled,
        rsi_agent_turn_protocol::TurnError::Capacity,
        rsi_agent_turn_protocol::TurnError::Invalid("known invalid request".into()),
    ] {
        let mut program = fixture.prepare(
            "try { await tools.call('effect', {}); } catch { return 'caught known refusal'; } throw Error('RPC should refuse');",
            Arc::new(KnownRefusalRpc(refusal)),
        ).await;
        program.start().unwrap();
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(5), program.result())
                .await
                .unwrap()
                .unwrap(),
            json!("caught known refusal"),
        );
        let read = fixture
            .jobs
            .wait(&fixture.scope, program.job_id(), 0, 0)
            .await
            .unwrap();
        assert_eq!(read.job.status, JobStatus::Completed);
    }
    fixture.close().await;
}
#[tokio::test]
#[ignore = "requires explicit RSI_TEST_NODE native integration"]
async fn actual_node_cannot_catch_uncertain_rpc_and_continue() {
    let fixture = Fixture::new().await;
    let mut program = fixture.prepare(
        "try { await tools.call('effect', {}); } catch { require('fs').writeFileSync('retried', 'unsafe'); } return 'continued';",
        Arc::new(UncertainRpc),
    ).await;
    program.start().unwrap();
    assert_eq!(
        program.result().await.unwrap_err(),
        rsi_agent_program::ProgramError::OutcomeUnknown
    );
    let read = fixture
        .jobs
        .wait(&fixture.scope, program.job_id(), 0, 0)
        .await
        .unwrap();
    assert_eq!(read.job.status, JobStatus::OutcomeUnknown);
    assert!(!fixture.workspace.path().join("retried").exists());
    fixture.close().await;
}

#[tokio::test]
async fn missing_or_non_regular_node_is_rejected_before_job_admission() {
    let root = tempfile::tempdir().unwrap();
    for node in [root.path().to_path_buf(), root.path().join("absent")] {
        let fixture = Fixture::with_node(node).await;
        let execution = fixture.execution(
            CancellationToken::new(),
            fixture
                .runtime
                .root()
                .lookup_local::<SandboxContract>()
                .unwrap(),
        );
        let runtime = fixture
            .runtime
            .root()
            .lookup_local::<ProgramRuntimeContract>()
            .unwrap();
        let error = runtime
            .prepare_process(&execution)
            .await
            .unwrap_err()
            .to_string();
        assert!(error.contains("Node executable"), "{error}");
        assert!(fixture.jobs.list(&fixture.scope).unwrap().is_empty());
        fixture.close().await;
    }
}
