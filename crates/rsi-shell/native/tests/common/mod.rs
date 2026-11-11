use async_trait::async_trait;
use rsi_jobs::{JobScopeAuthority, JobScopeId, Jobs, JobsContract};
use rsi_jobs_local::JobsLocalFactory;
use rsi_meta::{
    ActivationPlan, ConfigValue, FiberHandle, MetaError, PluginFactory, PreparedActivation,
    ResolvedFactory, Runtime, UpdateMode,
};
use rsi_process_local::ProcessLocalFactory;
use rsi_sandbox::{Sandbox, SandboxContract, SandboxMode};
use rsi_sandbox_local::SandboxLocalFactory;
use rsi_shell_native::{ShellJobProducerFactory, ShellKind, ShellToolFactory};
use rsi_tools::ToolsFactory;
use rsi_tools_protocol::{
    ToolCall, ToolCatalogProviderContract, ToolExecutionExtensions, ToolExecutionPolicy,
    ToolRegistrar, ToolRegistrarContract, ToolRuntime, ToolStart,
};
use serde_json::{Value, json};
use std::ffi::OsString;
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

pub(super) struct Fixture {
    kind: ShellKind,
    pub(super) runtime: Runtime,
    pub(super) fibers: Vec<FiberHandle>,
    pub(super) tools: Arc<dyn ToolRuntime>,
    pub(super) sandbox: Arc<dyn Sandbox>,
    pub(super) jobs: Arc<dyn Jobs>,
    pub(super) scope: JobScopeAuthority,
    pub(super) workspace: TempDir,
}

impl Fixture {
    #[cfg(target_os = "linux")]
    pub(super) async fn activate() -> Self {
        Self::activate_with(
            ShellKind::Bash,
            "/bin/bash".into(),
            vec![
                ("PATH".into(), "/usr/bin:/bin".into()),
                ("VISIBLE".into(), "yes".into()),
            ],
        )
        .await
    }

    #[allow(clippy::too_many_lines)] // Activation order is the public dependency chain under test.
    pub(super) async fn activate_with(
        kind: ShellKind,
        program: PathBuf,
        environment: Vec<(OsString, OsString)>,
    ) -> Self {
        let workspace = tempfile::tempdir().unwrap();
        #[cfg(windows)]
        let environment = {
            let mut environment = environment;
            environment.retain(|(name, _)| {
                !name.eq_ignore_ascii_case("TEMP") && !name.eq_ignore_ascii_case("TMP")
            });
            environment.extend(
                ["TEMP", "TMP"].map(|name| (name.into(), workspace.path().as_os_str().to_owned())),
            );
            environment
        };
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
                    linked(
                        "shell-producer",
                        Arc::new(ShellJobProducerFactory::new(kind)),
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
                        "shell-tool",
                        Arc::new(ShellToolFactory::new(kind, program, environment).unwrap()),
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
        Self {
            kind,
            runtime,
            fibers,
            tools,
            sandbox,
            jobs,
            scope,
            workspace,
        }
    }

    pub(super) async fn call(
        &self,
        arguments: Value,
    ) -> rsi_tools_protocol::Result<rsi_tools_protocol::ToolResult> {
        self.call_with_mode(arguments, SandboxMode::DangerFullAccess)
            .await
    }

    pub(super) async fn call_with_mode(
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
                    name: self.kind.name().into(),
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
                extensions: ToolExecutionExtensions::default()
                    .with(Arc::new(
                        rsi_jobs::JobOrigin::new(format!("invocation-{number}")).unwrap(),
                    ))
                    .unwrap(),
            })
            .await
    }

    pub(super) async fn shutdown(mut self) {
        drop((self.scope, self.jobs, self.tools, self.sandbox));
        while let Some(fiber) = self.fibers.pop() {
            assert!(fiber.dispose().await.is_clean());
        }
        assert!(self.runtime.shutdown().await.is_complete());
    }
}

fn linked(name: &str, factory: Arc<dyn PluginFactory>) -> ResolvedFactory {
    ResolvedFactory::linked(name, "test", UpdateMode::Replayable, factory)
}
