use rsi_execution::{ExecutionAdmission, ExecutionOperation};
use rsi_execution_local::NativeCapabilities;
use rsi_meta::{ResolvedFactory, Runtime, UpdateMode};
use rsi_process::{DuplexProcessContract, ProcessContract, PtyProcessContract};
use rsi_sandbox::{SandboxContract, SandboxError, SandboxMode};
use rsi_tools_protocol::{
    ToolError, ToolExecution, ToolExecutionExtensions, ToolExecutionPolicy, ToolStart,
};
use std::{collections::BTreeMap, sync::Arc};
use tokio_util::sync::CancellationToken;

#[derive(Debug)]
struct Admission;
impl ExecutionAdmission for Admission {
    fn admit(
        &self,
        _: rsi_execution::ExecutionAdmissionKind,
    ) -> rsi_process::Result<ExecutionOperation> {
        Ok(ExecutionOperation::new(()))
    }
}

#[tokio::test]
async fn native_preparation_reports_the_same_sandbox_cause_with_or_without_a_lease() {
    let runtime = Runtime::default();
    let sandbox_fiber = runtime
        .root()
        .apply(
            ResolvedFactory::linked(
                "sandbox",
                "test",
                UpdateMode::Replayable,
                Arc::new(rsi_sandbox_local::SandboxLocalFactory::default()),
            ),
            serde_json::json!({"bubblewrap":[],"landlock":[]}),
        )
        .await
        .unwrap();
    let process_fiber = runtime
        .root()
        .apply(
            ResolvedFactory::linked(
                "process",
                "test",
                UpdateMode::Replayable,
                Arc::new(rsi_process_local::ProcessLocalFactory),
            ),
            serde_json::json!({}),
        )
        .await
        .unwrap();
    let sandbox = runtime.root().lookup_local::<SandboxContract>().unwrap();
    let provider = rsi_execution_local::provider(
        rsi_execution::HostEpoch::from_bytes([17; 16]),
        NativeCapabilities {
            sandbox: sandbox.clone(),
            process: runtime.root().lookup_local::<ProcessContract>().unwrap(),
            duplex: runtime
                .root()
                .lookup_local::<DuplexProcessContract>()
                .unwrap(),
            pty: runtime.root().lookup_local::<PtyProcessContract>().unwrap(),
            files: Arc::new(rsi_files::LocalFiles::new().unwrap()),
        },
        BTreeMap::new(),
    )
    .await
    .unwrap();
    let lease = provider.lease(Arc::new(Admission)).unwrap();
    let workspace = tempfile::tempdir().unwrap();
    let path = lease
        .canonicalize(workspace.path().to_str().unwrap())
        .await
        .unwrap();
    let program = std::env::current_exe().unwrap();
    for mode in [SandboxMode::ReadOnly, SandboxMode::WorkspaceWrite] {
        for leased in [false, true] {
            let mut extensions = ToolExecutionExtensions::default();
            if leased {
                extensions = extensions.with(Arc::new(lease.clone())).unwrap();
            }
            let (execution, _enforcement) = ToolExecution::from_start(
                "prepare-only".into(),
                ToolStart {
                    cancellation: CancellationToken::new(),
                    policy: ToolExecutionPolicy {
                        mode,
                        cwd: path.path().into(),
                        workspace: path.path().into(),
                    },
                    sandbox: sandbox.clone(),
                    job_scope: None,
                    extensions,
                },
            )
            .unwrap();
            let error = execution
                .prepare_process("native-probe", program.clone(), vec![], vec![])
                .await
                .unwrap_err();
            assert_eq!(
                error,
                ToolError::Sandbox(SandboxError::Unsupported(mode)),
                "leased={leased}"
            );
        }
    }
    drop(lease);
    drop(provider);
    drop(sandbox);
    assert!(process_fiber.dispose().await.is_clean());
    assert!(sandbox_fiber.dispose().await.is_clean());
}
