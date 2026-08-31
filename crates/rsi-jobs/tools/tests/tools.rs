use async_trait::async_trait;
use rsi_jobs::{
    JobControl, JobOutputRead, JobProducer, JobProducerLease, JobProducerRegistration, JobRequest,
    JobScopeAuthority, JobScopeId, JobStatus, JobStream, JobSubmission, JobTerminal, Jobs,
    JobsContract,
};
use rsi_jobs_local::JobsLocalFactory;
use rsi_jobs_tools::JobsToolsFactory;
use rsi_meta::{
    ActivationPlan, ConfigValue, FiberHandle, MetaError, PluginFactory, PreparedActivation,
    ResolvedFactory, Runtime, UpdateMode,
};
use rsi_sandbox::{ConfinedProcess, ProcessRequest, Sandbox, SandboxError, SandboxMode};
use rsi_tools::ToolsFactory;
use rsi_tools_protocol::{
    RetainedToolFailureKind, RetainedToolResult, ToolCall, ToolCatalogProviderContract, ToolError,
    ToolExecutionPolicy, ToolRegistrar, ToolRegistrarContract, ToolRuntime, ToolStart,
};
use serde_json::{Value, json};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Duration;
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

#[derive(Debug)]
struct UnusedSandbox;

#[async_trait]
impl Sandbox for UnusedSandbox {
    async fn confine(&self, request: ProcessRequest) -> Result<ConfinedProcess, SandboxError> {
        Err(SandboxError::Unsupported(request.mode))
    }
}

#[derive(Clone, Debug)]
enum TestRequest {
    Complete,
    Block,
    IgnoreCancellation(CancellationToken),
    Output {
        stdout: Arc<[u8]>,
        stderr: Arc<[u8]>,
    },
}

#[derive(Debug)]
struct TestProducer;

impl JobProducer for TestProducer {
    fn start(&self, request: &JobRequest) -> rsi_jobs::Result<Arc<dyn JobControl>> {
        let request = request
            .downcast_ref::<TestRequest>()
            .ok_or_else(|| rsi_jobs::JobsError::InvalidInput("unexpected test request".into()))?;
        let settlement = match request {
            TestRequest::Complete | TestRequest::Output { .. } => TestSettlement::Immediate,
            TestRequest::Block => TestSettlement::Cancellation,
            TestRequest::IgnoreCancellation(release) => TestSettlement::Release(release.clone()),
        };
        let (stdout, stderr) = match request {
            TestRequest::Output { stdout, stderr } => (Arc::clone(stdout), Arc::clone(stderr)),
            _ => (Arc::from(&b"output"[..]), Arc::from(&b"warning"[..])),
        };
        Ok(Arc::new(TestControl {
            settlement,
            stdout,
            stderr,
            cancelled: AtomicBool::new(false),
            cancellation: CancellationToken::new(),
        }))
    }
}

#[derive(Debug)]
enum TestSettlement {
    Immediate,
    Cancellation,
    Release(CancellationToken),
}

#[derive(Debug)]
struct TestControl {
    settlement: TestSettlement,
    stdout: Arc<[u8]>,
    stderr: Arc<[u8]>,
    cancelled: AtomicBool,
    cancellation: CancellationToken,
}

#[async_trait]
impl JobControl for TestControl {
    fn read(&self, stream: JobStream, offset: u64) -> rsi_jobs::Result<JobOutputRead> {
        let bytes = match stream {
            JobStream::Stdout => self.stdout.as_ref(),
            JobStream::Stderr => self.stderr.as_ref(),
        };
        let offset = usize::try_from(offset)
            .unwrap_or(usize::MAX)
            .min(bytes.len());
        Ok(JobOutputRead {
            bytes: bytes[offset..].to_vec(),
            oldest_offset: 0,
            next_offset: bytes.len() as u64,
            lossy: false,
        })
    }

    fn cancel(&self) {
        self.cancelled.store(true, Ordering::Release);
        self.cancellation.cancel();
    }

    async fn wait(&self) -> rsi_jobs::Result<JobTerminal> {
        match &self.settlement {
            TestSettlement::Immediate => {}
            TestSettlement::Cancellation => self.cancellation.cancelled().await,
            TestSettlement::Release(release) => release.cancelled().await,
        }
        Ok(JobTerminal {
            status: if self.cancelled.load(Ordering::Acquire) {
                JobStatus::Cancelled
            } else {
                JobStatus::Completed
            },
            exit_code: Some(0),
            signal: None,
            message: None,
        })
    }
}

struct Fixture {
    runtime: Runtime,
    fibers: Vec<FiberHandle>,
    tools: Arc<dyn ToolRuntime>,
    jobs: Arc<dyn Jobs>,
    scope: JobScopeAuthority,
    producer: Option<JobProducerLease>,
}

impl Fixture {
    async fn activate() -> Self {
        let runtime = Runtime::default();
        let mut fibers = Vec::new();
        fibers.push(
            runtime
                .root()
                .apply(linked("jobs", Arc::new(JobsLocalFactory)), Value::Null)
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
                    linked("jobs-tools", Arc::new(JobsToolsFactory)),
                    Value::Null,
                )
                .await
                .unwrap(),
        );
        let tools = stage.seal().unwrap();
        let jobs = runtime.root().lookup_local::<JobsContract>().unwrap();
        let producer = jobs
            .register_producer(JobProducerRegistration {
                name: "test".into(),
                producer: Arc::new(TestProducer),
            })
            .unwrap();
        let scope = jobs
            .acquire_scope(JobScopeId::new("test", ["turn"]).unwrap())
            .unwrap();
        Self {
            runtime,
            fibers,
            tools,
            jobs,
            scope,
            producer: Some(producer),
        }
    }

    fn submit(&self, request: TestRequest) -> String {
        self.jobs
            .submit(
                &self.scope,
                JobSubmission {
                    name: "test".into(),
                    producer: "test".into(),
                    request: JobRequest::new(request),
                    requires_report: true,
                },
            )
            .unwrap()
    }

    async fn call(
        &self,
        name: &str,
        arguments: Value,
        with_scope: bool,
    ) -> rsi_tools_protocol::Result<rsi_tools_protocol::ToolResult> {
        let number = NEXT_CALL.fetch_add(1, Ordering::AcqRel) + 1;
        self.tools
            .prepare(
                &format!("invocation-{number}"),
                ToolCall {
                    id: format!("call-{number}"),
                    name: name.into(),
                    arguments,
                },
            )?
            .start(ToolStart {
                cancellation: CancellationToken::new(),
                policy: ToolExecutionPolicy {
                    mode: SandboxMode::DangerFullAccess,
                    cwd: "/workspace".into(),
                    workspace: "/workspace".into(),
                },
                sandbox: Arc::new(UnusedSandbox),
                job_scope: with_scope.then(|| self.scope.clone()),
            })
            .await
    }

    fn prepare(
        &self,
        name: &str,
        arguments: Value,
    ) -> rsi_tools_protocol::Result<Box<dyn rsi_tools_protocol::PreparedToolCall>> {
        let number = NEXT_CALL.fetch_add(1, Ordering::AcqRel) + 1;
        self.tools.prepare(
            &format!("invocation-{number}"),
            ToolCall {
                id: format!("call-{number}"),
                name: name.into(),
                arguments,
            },
        )
    }

    fn start(&self, cancellation: CancellationToken) -> ToolStart {
        ToolStart {
            cancellation,
            policy: ToolExecutionPolicy {
                mode: SandboxMode::DangerFullAccess,
                cwd: "/workspace".into(),
                workspace: "/workspace".into(),
            },
            sandbox: Arc::new(UnusedSandbox),
            job_scope: Some(self.scope.clone()),
        }
    }

    async fn shutdown(mut self) {
        self.jobs.finalize_scope(&self.scope).await.unwrap();
        drop((self.scope, self.tools));
        if let Some(producer) = self.producer.take() {
            producer.retire().await.unwrap();
        }
        drop(self.jobs);
        while let Some(fiber) = self.fibers.pop() {
            assert!(fiber.dispose().await.is_clean());
        }
        assert!(self.runtime.shutdown().await.is_complete());
    }
}

fn linked(name: &str, factory: Arc<dyn PluginFactory>) -> ResolvedFactory {
    ResolvedFactory::linked(name, "test", UpdateMode::Replayable, factory)
}

#[tokio::test]
async fn factory_publishes_only_generic_jobs_tools_and_preserves_control_semantics() {
    let fixture = Fixture::activate().await;
    let definitions = fixture.tools.definitions();
    assert_eq!(
        definitions
            .iter()
            .map(rsi_tools_protocol::ToolDefinition::name)
            .collect::<Vec<_>>(),
        ["job_kill", "job_list", "job_output"]
    );
    let expected = [
        (
            "job_kill",
            "Terminate one background job, wait for settlement, and report its final retained output.",
            json!({
                "type":"object",
                "properties":{"job_id":{"type":"string","maxLength":256}},
                "required":["job_id"],
                "additionalProperties":false
            }),
        ),
        (
            "job_list",
            "List background jobs in the current turn scope. Terminal jobs with reported=false still require job_output or job_kill before successful turn completion.",
            json!({"type":"object","properties":{},"additionalProperties":false}),
        ),
        (
            "job_output",
            "Read both retained output streams for one background job. Active reads do not report completion; a terminal read or successful wait does.",
            json!({
                "type":"object",
                "properties":{
                    "job_id":{"type":"string","maxLength":256},
                    "wait":{"type":"boolean"},
                    "timeout_ms":{"type":"integer","minimum":1,"maximum":570_000}
                },
                "required":["job_id"],
                "additionalProperties":false
            }),
        ),
    ];
    for (definition, (name, description, schema)) in definitions.iter().zip(expected) {
        assert_eq!(definition.name(), name);
        assert_eq!(definition.description(), description);
        assert_eq!(definition.input_schema(), &schema);
        assert!(definition.freeform().is_none());
    }

    let missing = fixture.call("job_list", json!({}), false).await.unwrap();
    assert!(missing.is_error);
    assert_eq!(missing.value["code"], "missing_job_scope");

    let complete = fixture.submit(TestRequest::Complete);
    let output = fixture
        .call(
            "job_output",
            json!({"job_id":complete,"wait":true,"timeout_ms":1000}),
            true,
        )
        .await
        .unwrap();
    assert!(!output.is_error);
    assert_eq!(output.value["status"], "completed");
    assert_eq!(output.value["reported"], true);
    assert_eq!(output.value["stdout"]["text"], "output");
    assert_eq!(output.value["stderr"]["text"], "warning");

    let running = fixture.submit(TestRequest::Block);
    let listed = fixture.call("job_list", json!({}), true).await.unwrap();
    assert!(
        listed.value["jobs"]
            .as_array()
            .unwrap()
            .iter()
            .any(|job| job["id"] == running)
    );
    let killed = fixture
        .call("job_kill", json!({"job_id":running}), true)
        .await
        .unwrap();
    assert!(!killed.is_error);
    assert_eq!(killed.value["status"], "cancelled");
    assert_eq!(killed.value["reported"], true);
    fixture.shutdown().await;
}

#[tokio::test]
async fn terminal_output_projection_is_bounded_before_the_job_is_reported() {
    let fixture = Fixture::activate().await;
    let capture_bytes = 4 * 1024 * 1024;
    let complete = fixture.submit(TestRequest::Output {
        stdout: Arc::from(vec![0_u8; capture_bytes]),
        stderr: Arc::from(vec![b'x'; capture_bytes]),
    });

    let output = fixture
        .call(
            "job_output",
            json!({"job_id":complete,"wait":true,"timeout_ms":1000}),
            true,
        )
        .await
        .expect("a legal producer-sized capture must remain a valid Tool result");

    assert!(!output.is_error);
    assert_eq!(output.value["reported"], true);
    for stream in ["stdout", "stderr"] {
        assert_eq!(output.value[stream]["next_offset"], capture_bytes as u64);
        assert_eq!(output.value[stream]["truncated"], true);
        assert!(output.value[stream]["oldest_offset"].as_u64().unwrap() > 0);
        assert!(output.value[stream]["text"].as_str().unwrap().len() < capture_bytes);
    }
    output.validate().unwrap();
    fixture.shutdown().await;
}

#[tokio::test(start_paused = true)]
async fn waiting_jobs_tools_observe_cancellation_and_release_retained_results() {
    let fixture = Fixture::activate().await;
    let running = fixture.submit(TestRequest::Block);
    let prepared = fixture
        .prepare(
            "job_output",
            json!({"job_id":running,"wait":true,"timeout_ms":570_000}),
        )
        .unwrap();
    let output_identity = prepared.identity().clone();
    let cancellation = CancellationToken::new();
    let invocation = tokio::spawn(prepared.start(fixture.start(cancellation.clone())));
    tokio::task::yield_now().await;
    cancellation.cancel();
    let result = tokio::time::timeout(Duration::from_millis(1), invocation)
        .await
        .expect("cancelled job_output must not wait for its 570 second timeout")
        .unwrap();
    assert_eq!(result, Err(ToolError::Cancelled));
    assert!(matches!(
        fixture.tools.query(&output_identity).unwrap(),
        RetainedToolResult::Failed(failure)
            if failure.kind == RetainedToolFailureKind::Cancelled
    ));
    fixture.tools.commit(&output_identity).unwrap();
    assert_eq!(
        fixture.tools.query(&output_identity).unwrap(),
        RetainedToolResult::Absent
    );
    assert_eq!(
        fixture.jobs.get(&fixture.scope, &running).unwrap().status,
        JobStatus::Running
    );

    let release = CancellationToken::new();
    let stubborn = fixture.submit(TestRequest::IgnoreCancellation(release.clone()));
    let prepared = fixture
        .prepare("job_kill", json!({"job_id":stubborn}))
        .unwrap();
    let kill_identity = prepared.identity().clone();
    let cancellation = CancellationToken::new();
    let invocation = tokio::spawn(prepared.start(fixture.start(cancellation.clone())));
    tokio::time::timeout(Duration::from_millis(1), async {
        loop {
            if fixture.jobs.get(&fixture.scope, &stubborn).unwrap().status == JobStatus::Stopping {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("job_kill must send cancellation before the caller cancels its Tool call");
    cancellation.cancel();
    let result = tokio::time::timeout(Duration::from_millis(1), invocation)
        .await
        .expect("cancelled job_kill must not wait for an uncooperative job")
        .unwrap();
    assert_eq!(result, Err(ToolError::Cancelled));
    assert!(matches!(
        fixture.tools.query(&kill_identity).unwrap(),
        RetainedToolResult::Failed(failure)
            if failure.kind == RetainedToolFailureKind::Cancelled
    ));
    fixture.tools.commit(&kill_identity).unwrap();
    assert_eq!(
        fixture.tools.query(&kill_identity).unwrap(),
        RetainedToolResult::Absent
    );
    release.cancel();
    let settled = fixture
        .jobs
        .wait(&fixture.scope, &stubborn, 0, 0)
        .await
        .unwrap();
    assert_eq!(settled.job.status, JobStatus::Cancelled);
    fixture.shutdown().await;
}
