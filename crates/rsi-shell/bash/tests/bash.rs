#![cfg(target_os = "linux")]

use async_trait::async_trait;
use rsi_jobs::{JobScopeAuthority, JobScopeId, JobStatus, Jobs, JobsContract};
use rsi_jobs_local::JobsLocalFactory;
use rsi_meta::{
    ActivationPlan, ConfigValue, FiberHandle, MetaError, PluginFactory, PreparedActivation,
    ResolvedFactory, Runtime, UpdateMode,
};
use rsi_process_local::ProcessLocalFactory;
use rsi_sandbox::{Sandbox, SandboxContract, SandboxMode};
use rsi_sandbox_local::SandboxLocalFactory;
use rsi_shell_bash::{
    BashJobProducerFactory, BashToolFactory, MAXIMUM_BASH_COMMAND_BYTES, MAXIMUM_BASH_TIMEOUT_MS,
    scrub_child_environment,
};
use rsi_tools::ToolsFactory;
use rsi_tools_protocol::{
    ToolCall, ToolCatalogProviderContract, ToolContent, ToolExecutionExtensions,
    ToolExecutionPolicy, ToolRegistrar, ToolRegistrarContract, ToolRuntime, ToolStart,
};
use serde_json::{Value, json};
use std::ffi::{OsStr, OsString};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use tempfile::TempDir;
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

struct Fixture {
    runtime: Runtime,
    fibers: Vec<FiberHandle>,
    tools: Arc<dyn ToolRuntime>,
    sandbox: Arc<dyn Sandbox>,
    jobs: Arc<dyn Jobs>,
    scope: JobScopeAuthority,
    workspace: TempDir,
}

impl Fixture {
    #[allow(clippy::too_many_lines)] // Activation order is the public dependency chain under test.
    async fn activate() -> Self {
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
        fibers.push(
            runtime
                .root()
                .apply(
                    linked("process", Arc::new(ProcessLocalFactory)),
                    Value::Null,
                )
                .await
                .unwrap(),
        );
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
        let registrar = stage.registrar();
        fibers.push(
            runtime
                .root()
                .apply(
                    linked(
                        "test-registrar",
                        Arc::new(RegistrarSupplyFactory { registrar }),
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
                    linked("bash-producer", Arc::new(BashJobProducerFactory)),
                    Value::Null,
                )
                .await
                .unwrap(),
        );
        let environment = scrub_child_environment([
            (OsString::from("PATH"), OsString::from("/usr/bin:/bin")),
            (OsString::from("VISIBLE"), OsString::from("yes")),
        ]);
        fibers.push(
            runtime
                .root()
                .apply(
                    linked(
                        "bash-tool",
                        Arc::new(
                            BashToolFactory::new(PathBuf::from("/bin/bash"), environment).unwrap(),
                        ),
                    ),
                    Value::Null,
                )
                .await
                .unwrap(),
        );
        let tools = stage.seal().unwrap();
        let sandbox = runtime.root().lookup_local::<SandboxContract>().unwrap();
        let jobs = runtime.root().lookup_local::<JobsContract>().unwrap();
        let scope = jobs
            .acquire_scope(JobScopeId::new("test", ["turn"]).unwrap())
            .unwrap();
        let workspace = tempfile::tempdir().unwrap();
        Self {
            runtime,
            fibers,
            tools,
            sandbox,
            jobs,
            scope,
            workspace,
        }
    }

    async fn call(
        &self,
        arguments: Value,
    ) -> rsi_tools_protocol::Result<rsi_tools_protocol::ToolResult> {
        self.call_with_mode(arguments, SandboxMode::DangerFullAccess)
            .await
    }

    async fn call_with_mode(
        &self,
        arguments: Value,
        mode: SandboxMode,
    ) -> rsi_tools_protocol::Result<rsi_tools_protocol::ToolResult> {
        let number = NEXT_CALL.fetch_add(1, Ordering::AcqRel) + 1;
        self.tools
            .prepare(
                &format!("invocation-{number}"),
                ToolCall {
                    id: format!("call-{number}"),
                    name: "bash".into(),
                    arguments,
                },
            )?
            .start(ToolStart {
                cancellation: CancellationToken::new(),
                policy: ToolExecutionPolicy {
                    mode,
                    cwd: self.workspace.path().canonicalize().unwrap(),
                    workspace: self.workspace.path().canonicalize().unwrap(),
                },
                sandbox: Arc::clone(&self.sandbox),
                job_scope: Some(self.scope.clone()),
                extensions: ToolExecutionExtensions::default(),
            })
            .await
    }

    async fn shutdown(mut self) {
        drop((self.scope, self.jobs, self.tools, self.sandbox));
        while let Some(fiber) = self.fibers.pop() {
            assert!(fiber.dispose().await.is_clean());
        }
        assert!(self.runtime.shutdown().await.is_complete());
    }
}

#[tokio::test]
async fn restricted_bash_fails_closed_without_a_verified_sandbox_backend() {
    let fixture = Fixture::activate().await;
    let marker = fixture.workspace.path().join("restricted-marker");

    assert!(
        fixture
            .call_with_mode(
                json!({"command":"printf blocked > restricted-marker"}),
                SandboxMode::WorkspaceWrite,
            )
            .await
            .is_err()
    );
    assert!(!marker.exists());
    fixture.shutdown().await;
}

fn linked(name: &str, factory: Arc<dyn PluginFactory>) -> ResolvedFactory {
    ResolvedFactory::linked(name, "test", UpdateMode::Replayable, factory)
}

#[test]
fn child_environment_scrub_is_case_insensitive_and_preserves_ordinary_raw_values() {
    #[cfg(unix)]
    use std::os::unix::ffi::OsStringExt as _;

    let scrubbed = scrub_child_environment(vec![
        (OsString::from("API_KEY"), OsString::from("secret")),
        (OsString::from("rSi_INTERNAL"), OsString::from("secret")),
        (
            OsString::from("HTTPS_PROXY"),
            OsString::from("http://user:pass@example.test:8080"),
        ),
        (
            OsString::from("HTTP_PROXY"),
            OsString::from("http://example.test:8080"),
        ),
        (OsString::from("NO_PROXY"), OsString::from("localhost")),
        (OsString::from("VISIBLE"), OsString::from("yes")),
        (
            OsString::from("LD_PRELOAD"),
            OsString::from("/tmp/inject.so"),
        ),
        (OsString::from("BASH_ENV"), OsString::from("/tmp/bash-env")),
        #[cfg(unix)]
        (
            OsString::from("RAW_VALUE"),
            OsString::from_vec(vec![0xff, b'x']),
        ),
        #[cfg(unix)]
        (
            OsString::from_vec(vec![0xff, b'N']),
            OsString::from("bad-name"),
        ),
    ]);
    let get = |name: &str| {
        scrubbed
            .iter()
            .find(|(candidate, _)| candidate == OsStr::new(name))
            .map(|(_, value)| value)
    };
    assert!(get("API_KEY").is_none());
    assert!(get("rSi_INTERNAL").is_none());
    assert!(get("HTTPS_PROXY").is_none());
    assert_eq!(
        get("HTTP_PROXY"),
        Some(&OsString::from("http://example.test:8080"))
    );
    assert_eq!(get("NO_PROXY"), Some(&OsString::from("localhost")));
    assert_eq!(get("VISIBLE"), Some(&OsString::from("yes")));
    assert!(get("LD_PRELOAD").is_none());
    assert!(get("BASH_ENV").is_none());
    #[cfg(unix)]
    assert_eq!(
        get("RAW_VALUE"),
        Some(&OsString::from_vec(vec![0xff, b'x']))
    );
}

#[test]
fn bash_factory_rejects_missing_executables_and_duplicate_retained_names() {
    assert!(
        BashToolFactory::new(PathBuf::from("/definitely/missing/rsi-bash"), Vec::new(),).is_err()
    );
    assert!(
        BashToolFactory::new(
            PathBuf::from("/bin/bash"),
            vec![
                (OsString::from("VISIBLE"), OsString::from("first")),
                (OsString::from("VISIBLE"), OsString::from("second")),
            ],
        )
        .is_err()
    );
}

#[tokio::test]
async fn factories_publish_only_bash_and_preserve_foreground_and_background_behavior() {
    let fixture = Fixture::activate().await;
    let definitions = fixture.tools.definitions();
    assert_eq!(
        definitions
            .iter()
            .map(rsi_tools_protocol::ToolDefinition::name)
            .collect::<Vec<_>>(),
        ["bash"]
    );
    assert_eq!(
        definitions[0].description(),
        "Run an exact Bash command. Foreground commands wait for the complete process group; use run_in_background for long-lived work. Nonzero exits, signals, and command timeout are normal outcomes."
    );
    assert_eq!(
        definitions[0].input_schema(),
        &json!({
            "type":"object",
            "properties":{
                "command":{"type":"string","maxLength":MAXIMUM_BASH_COMMAND_BYTES},
                "timeout_ms":{"type":"integer","minimum":1,"maximum":MAXIMUM_BASH_TIMEOUT_MS},
                "run_in_background":{"type":"boolean"}
            },
            "required":["command"],
            "additionalProperties":false
        })
    );

    let foreground = fixture
        .call(json!({
            "command":"printf '%s|%s|%s|%s|%s' \"$VISIBLE\" \"$NO_COLOR\" \"$TERM\" \"$PAGER\" \"$GIT_PAGER\"; printf warning >&2; exit 7"
        }))
        .await
        .unwrap();
    assert!(!foreground.is_error);
    assert_eq!(foreground.value["status"], "exited");
    assert_eq!(foreground.value["exit_code"], 7);
    assert_eq!(foreground.value["stdout"]["text"], "yes|1|dumb|cat|cat");
    assert_eq!(foreground.value["stderr"]["text"], "warning");

    let truncated = fixture
        .call(json!({
            "command":"head -c 65537 /dev/zero | tr '\\0' x"
        }))
        .await
        .unwrap();
    assert_eq!(truncated.value["stdout"]["truncated"], true);
    assert!(matches!(
        truncated.content.as_slice(),
        [ToolContent::Text { text }] if text.starts_with("[stdout truncated; showing retained tail]\n")
    ));

    let accepted = fixture
        .call(json!({"command":"#".repeat(MAXIMUM_BASH_COMMAND_BYTES)}))
        .await
        .unwrap();
    assert!(!accepted.is_error);
    let rejected = fixture
        .call(json!({"command":"#".repeat(MAXIMUM_BASH_COMMAND_BYTES + 1)}))
        .await
        .unwrap();
    assert!(rejected.is_error);
    assert_eq!(rejected.value["code"], "invalid_arguments");
    assert!(rejected.enforcement.is_empty());

    let timed_out = fixture
        .call(json!({"command":"sleep 1","timeout_ms":10}))
        .await
        .unwrap();
    assert!(!timed_out.is_error);
    assert_eq!(timed_out.value["status"], "timed_out");
    assert!(timed_out.value["signal"].as_i64().is_some());

    let background_timeout = fixture
        .call(json!({
            "command":"true",
            "timeout_ms":10,
            "run_in_background":true
        }))
        .await
        .unwrap();
    assert!(background_timeout.is_error);
    assert_eq!(background_timeout.value["code"], "invalid_arguments");
    assert!(background_timeout.enforcement.is_empty());

    let started = fixture
        .call(json!({
            "command":"printf begin; sleep 0.02; printf end; printf warning >&2",
            "run_in_background":true
        }))
        .await
        .unwrap();
    let id = started.value["job_id"].as_str().unwrap();
    let settled = fixture.jobs.wait(&fixture.scope, id, 0, 0).await.unwrap();
    assert_eq!(settled.job.producer, "rsi.coding.bash");
    assert_eq!(settled.job.status, JobStatus::Completed);
    assert_eq!(settled.stdout.bytes, b"beginend");
    assert_eq!(settled.stderr.bytes, b"warning");
    fixture.shutdown().await;
}

#[tokio::test]
async fn background_nonzero_exit_is_a_failed_job_terminal() {
    let fixture = Fixture::activate().await;
    let started = fixture
        .call(json!({"command":"exit 7","run_in_background":true}))
        .await
        .unwrap();
    let id = started.value["job_id"].as_str().unwrap();
    let failed = fixture.jobs.wait(&fixture.scope, id, 0, 0).await.unwrap();

    assert_eq!(failed.job.status, JobStatus::Failed);
    assert_eq!(failed.job.terminal.unwrap().exit_code, Some(7));
    fixture.shutdown().await;
}

#[tokio::test]
async fn foreground_output_sanitizes_terminal_controls_only_in_model_text() {
    let fixture = Fixture::activate().await;
    let result = fixture
        .call(json!({"command":"printf '\\033[31mred\\007\\177'"}))
        .await
        .unwrap();

    assert_eq!(
        result.value["stdout"]["text"], "\u{1b}[31mred\u{7}\u{7f}",
        "the structured stream remains the authoritative lossy UTF-8 projection"
    );
    assert!(matches!(
        result.content.as_slice(),
        [ToolContent::Text { text }] if text == "\u{fffd}[31mred\u{fffd}\u{fffd}"
    ));
    fixture.shutdown().await;
}
