#![cfg(windows)]
use rsi_execution::{ExecutionAdmission, ExecutionLease, ExecutionOperation, ResolvedProgram};
use rsi_execution_local::NativeCapabilities;
use rsi_meta::{FiberHandle, ResolvedFactory, Runtime, UpdateMode};
use rsi_process::{DuplexProcessContract, ProcessContract, ProcessSpec, PtyProcessContract};
use rsi_sandbox::{
    ProcessRequest, ProcessStdio, SandboxBackend, SandboxContract, SandboxFileSystem, SandboxMode,
    SandboxScratch,
};
use std::{
    collections::BTreeMap,
    ffi::OsString,
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

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
struct Fixture {
    _runtime: Runtime,
    fibers: Vec<FiberHandle>,
    lease: ExecutionLease,
}
impl Fixture {
    async fn new() -> Self {
        let runtime = Runtime::default();
        let mut fibers = Vec::new();
        for (id, factory, config) in [
            (
                "sandbox",
                Arc::new(rsi_sandbox_local::SandboxLocalFactory::default())
                    as Arc<dyn rsi_meta::PluginFactory>,
                serde_json::json!({}),
            ),
            (
                "process",
                Arc::new(rsi_process_local::ProcessLocalFactory)
                    as Arc<dyn rsi_meta::PluginFactory>,
                serde_json::json!({}),
            ),
        ] {
            fibers.push(
                runtime
                    .root()
                    .apply(
                        ResolvedFactory::linked(
                            id,
                            "windows-acl-test",
                            UpdateMode::Replayable,
                            factory,
                        ),
                        config,
                    )
                    .await
                    .unwrap(),
            );
        }
        let capabilities = NativeCapabilities {
            sandbox: runtime.root().lookup_local::<SandboxContract>().unwrap(),
            process: runtime.root().lookup_local::<ProcessContract>().unwrap(),
            duplex: runtime
                .root()
                .lookup_local::<DuplexProcessContract>()
                .unwrap(),
            pty: runtime.root().lookup_local::<PtyProcessContract>().unwrap(),
            files: Arc::new(rsi_files::LocalFiles::new().unwrap()),
        };
        let provider = rsi_execution_local::provider(
            rsi_execution::HostEpoch::from_bytes([23; 16]),
            capabilities,
            BTreeMap::new(),
        )
        .unwrap();
        let lease = provider.lease(Arc::new(Admission)).unwrap();
        Self {
            _runtime: runtime,
            fibers,
            lease,
        }
    }
    async fn prepare(
        &self,
        workspace: &Path,
        mode: SandboxMode,
        arguments: Vec<String>,
        extra: Vec<(OsString, OsString)>,
    ) -> rsi_execution::PreparedProcess {
        let root = std::env::var_os("SystemRoot").unwrap();
        let mut environment = vec![
            ("SystemRoot".into(), root.clone()),
            ("Temp".into(), workspace.as_os_str().to_owned()),
            ("tMp".into(), workspace.as_os_str().to_owned()),
        ];
        environment.extend(extra);
        let program = self
            .lease
            .resolve_local_program(ResolvedProgram {
                program: std::env::var_os("RSI_ACL_TEST_SHELL").map_or_else(
                    || PathBuf::from(root).join(r"System32\WindowsPowerShell\v1.0\powershell.exe"),
                    PathBuf::from,
                ),
                environment,
            })
            .await
            .unwrap();
        self.lease
            .prepare(ProcessRequest {
                program,
                arguments,
                stdio: ProcessStdio::Pipes,
                mode,
                workspace: workspace.into(),
                cwd: workspace.into(),
            })
            .await
            .unwrap()
    }
    async fn close(self) {
        drop(self.lease);
        for fiber in self.fibers.into_iter().rev() {
            assert!(fiber.dispose().await.is_clean());
        }
    }
    async fn run(&self, plan: rsi_execution::PreparedProcess) -> serde_json::Value {
        let environment = plan.environment().to_vec();
        let process = self
            .lease
            .spawn(ProcessSpec {
                process: plan,
                environment,
                stdin: Vec::new(),
                stdout_max_bytes: 8192,
                stderr_max_bytes: 8192,
                termination_grace_ms: 1000,
            })
            .await
            .unwrap();
        let outcome = tokio::time::timeout(Duration::from_secs(20), process.wait())
            .await
            .unwrap()
            .unwrap();
        let stderr = process.stderr().read_from(0).unwrap().bytes;
        assert_eq!(
            outcome.exit_code,
            Some(7),
            "{}",
            String::from_utf8_lossy(&stderr)
        );
        serde_json::from_slice(&process.stdout().read_from(0).unwrap().bytes)
            .unwrap_or_else(|error| panic!("{error}; stderr={}", String::from_utf8_lossy(&stderr)))
    }
}
fn temporary(plan: &rsi_execution::PreparedProcess) -> PathBuf {
    let temp = plan
        .environment()
        .iter()
        .find(|(name, _)| name == "TEMP")
        .unwrap()
        .1
        .clone();
    assert_eq!(
        plan.environment()
            .iter()
            .find(|(name, _)| name == "TMP")
            .unwrap()
            .1,
        temp
    );
    PathBuf::from(temp)
}

#[tokio::test]
#[ignore = "native Windows token and ACL integration; mutates only new owned fixture directories"]
async fn workspace_token_scratch_and_execution_pin_stay_together() {
    let fixture = Fixture::new().await;
    // Current checkout volume exercises a new Modify-only inherited directory
    // on this host; the test never edits the checkout's own security descriptor.
    let root = tempfile::Builder::new()
        .prefix(".rsi-acl-fixture-")
        .tempdir_in(std::env::current_dir().unwrap())
        .unwrap();
    let first = root.path().join("first");
    let second = root.path().join("second");
    std::fs::create_dir(&first).unwrap();
    std::fs::create_dir(&second).unwrap();
    let other = fixture
        .prepare(&second, SandboxMode::WorkspaceWrite, vec![], vec![])
        .await;
    let other_temp = temporary(&other);
    let script = r"
$result = @{language = [string]$ExecutionContext.SessionState.LanguageMode}
foreach ($entry in @(@('workspace', (Join-Path $pwd 'write.txt')), @('other', (Join-Path $env:RSI_OTHER 'write.txt')), @('scratch', (Join-Path $env:TEMP 'write.txt')), @('otherScratch', (Join-Path $env:RSI_OTHER_TEMP 'write.txt')))) {
    try { Set-Content -LiteralPath $entry[1] -Value 'written' -NoNewline -ErrorAction Stop; $result[$entry[0]] = $true } catch { $result[$entry[0]] = $false }
}
$result | ConvertTo-Json -Compress
exit 7
";
    for mode in [SandboxMode::WorkspaceWrite, SandboxMode::ReadOnly] {
        let plan = fixture
            .prepare(
                &first,
                mode,
                vec![
                    "-NoProfile".into(),
                    "-NonInteractive".into(),
                    "-Command".into(),
                    script.into(),
                ],
                vec![
                    ("RSI_OTHER".into(), second.as_os_str().to_owned()),
                    ("RSI_OTHER_TEMP".into(), other_temp.as_os_str().to_owned()),
                ],
            )
            .await;
        assert_eq!(plan.enforcement().backend, SandboxBackend::WindowsAcl);
        let writable = mode == SandboxMode::WorkspaceWrite;
        assert_eq!(
            plan.enforcement().filesystem,
            if writable {
                SandboxFileSystem::WindowsAclWorkspaceWrite
            } else {
                SandboxFileSystem::WindowsAclReadOnly
            }
        );
        assert_eq!(plan.enforcement().scratch, SandboxScratch::PrivateDirectory);
        let temp = temporary(&plan);
        assert!(!temp.starts_with(&first));
        assert_ne!(temp, other_temp);
        let receipt = fixture.run(plan).await;
        assert_eq!(
            receipt,
            serde_json::json!({"workspace": writable, "other": false, "scratch": true, "otherScratch": false, "language": "FullLanguage"})
        );
        tokio::time::timeout(Duration::from_secs(10), async {
            while temp.exists() {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
    }
    assert_eq!(
        std::fs::read_to_string(first.join("write.txt")).unwrap(),
        "written"
    );
    assert!(!second.join("write.txt").exists());
    drop(other);
    tokio::time::timeout(Duration::from_secs(10), async {
        while other_temp.exists() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    fixture.close().await;
}
