//! Actual SSH stdio MCP transport with independent creator and business callers.
use rsi_execution::{ExecutionAdmission, ExecutionOperation, ExecutionTargetId, HostEpoch};
use rsi_mcp::{EnvironmentValue, McpConfig, McpError, McpService, ServerConfig, TransportConfig};
use rsi_ssh_client::ProcessConnection;
use std::{
    path::Path,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
};
use tokio_util::sync::CancellationToken;
#[derive(Debug, Default)]
struct Gate(CancellationToken);
impl ExecutionAdmission for Gate {
    fn admit(
        &self,
        _kind: rsi_execution::ExecutionAdmissionKind,
    ) -> rsi_process::Result<ExecutionOperation> {
        if self.0.is_cancelled() {
            return Err(rsi_process::ProcessError::ShuttingDown);
        }
        Ok(ExecutionOperation::new(()))
    }
}
#[derive(Debug, Default)]
struct Credentials(AtomicUsize);
#[async_trait::async_trait]
impl rsi_credentials_protocol::CredentialsResolve for Credentials {
    async fn resolve(
        &self,
        reference: &rsi_credentials_protocol::CredentialRef,
    ) -> rsi_credentials_protocol::Result<rsi_credentials_protocol::ResolvedCredential> {
        assert_eq!(
            reference,
            &rsi_credentials_protocol::CredentialRef::new("rsi.mcp", "allowed").unwrap()
        );
        self.0.fetch_add(1, Ordering::SeqCst);
        Ok(rsi_credentials_protocol::ResolvedCredential {
            secret: rsi_credentials_protocol::SecretValue::new("fixture-target-only-value")
                .unwrap(),
            source: rsi_credentials_protocol::CredentialSource::File,
        })
    }
}
#[derive(Debug)]
struct NoNative;
#[async_trait::async_trait]
impl rsi_sandbox::Sandbox for NoNative {
    async fn workspace_read(
        &self,
        _: rsi_sandbox::WorkspaceReadRequest,
    ) -> rsi_sandbox::Result<rsi_sandbox::WorkspaceReadScope> {
        panic!("SSH MCP reached Service Files");
    }
    async fn confine(
        &self,
        _: rsi_sandbox::ProcessRequest,
    ) -> rsi_sandbox::Result<rsi_sandbox::ConfinedProcess> {
        panic!("SSH MCP reached Service Sandbox");
    }
}
#[async_trait::async_trait]
impl rsi_process::DuplexProcess for NoNative {
    async fn spawn(
        &self,
        _: rsi_process::DuplexProcessSpec,
    ) -> rsi_process::Result<rsi_process::ManagedDuplexProcess> {
        panic!("SSH MCP reached Service Process");
    }
}
pub async fn verify(client: &ProcessConnection, workspace: &Path) {
    for mode in ["modern", "legacy"] {
        verify_mode(client, workspace, mode).await;
    }
}
#[expect(
    clippy::too_many_lines,
    reason = "one real SSH creator-revocation, independent-caller and maintenance sequence"
)]
async fn verify_mode(client: &ProcessConnection, workspace: &Path, mode: &str) {
    let target = ExecutionTargetId::parse("d".repeat(32)).unwrap();
    let provider = rsi_ssh_client::execution_provider(
        HostEpoch::from_bytes([1; 16]),
        client.clone(),
        target.clone(),
        1,
    )
    .unwrap();
    let creator_gate = Arc::new(Gate::default());
    let creator = provider.lease(creator_gate.clone()).unwrap();
    let caller_gate = Arc::new(Gate::default());
    let caller = provider.lease(caller_gate.clone()).unwrap();
    let credentials = Arc::new(Credentials::default());
    let service = McpService::new(credentials.clone(), Arc::new(NoNative), Arc::new(NoNative));
    let marker = workspace.join(format!("mcp-calls-{mode}"));
    service
        .configure(remote_config(&target, workspace, &marker, mode))
        .await
        .unwrap();
    assert_eq!(
        service
            .refresh("target", None, CancellationToken::new())
            .await
            .unwrap_err(),
        McpError::ProcessUnavailable
    );
    assert_eq!(credentials.0.load(Ordering::SeqCst), 0);
    let frozen = service
        .refresh("target", Some(creator.clone()), CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(credentials.0.load(Ordering::SeqCst), 1);
    creator_gate.0.cancel();
    if mode == "legacy" {
        std::fs::write(
            marker.with_file_name(format!("mcp-calls-{mode}.ping-ready")),
            "ready",
        )
        .unwrap();
        let pong = marker.with_file_name(format!("mcp-calls-{mode}.pong"));
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            while !pong.exists() {
                tokio::time::sleep(std::time::Duration::from_millis(5)).await;
            }
        })
        .await
        .expect("idle ping after creator revocation must settle as connection maintenance");
        assert_eq!(std::fs::read_to_string(pong).unwrap(), "acknowledged");
    }
    for lease in [None, Some(creator)] {
        assert_eq!(
            service
                .call(
                    &frozen,
                    "echo",
                    serde_json::json!({"message":"denied"}),
                    lease,
                    CancellationToken::new()
                )
                .await,
            Err(McpError::ProcessUnavailable)
        );
    }
    assert!(!marker.exists());
    let result = service
        .call(
            &frozen,
            "echo",
            serde_json::json!({"message":"authorized independent caller"}),
            Some(caller.clone()),
            CancellationToken::new(),
        )
        .await
        .unwrap();
    assert_eq!(
        result["content"][0]["text"],
        "authorized independent caller"
    );
    let foreign = rsi_ssh_client::execution_provider(
        HostEpoch::from_bytes([1; 16]),
        client.clone(),
        target,
        1,
    )
    .unwrap()
    .lease(Arc::new(Gate::default()))
    .unwrap();
    assert_eq!(
        service
            .call(
                &frozen,
                "echo",
                serde_json::json!({"message":"foreign"}),
                Some(foreign),
                CancellationToken::new()
            )
            .await,
        Err(McpError::ProcessUnavailable)
    );
    caller_gate.0.cancel();
    assert_eq!(
        service
            .call(
                &frozen,
                "echo",
                serde_json::json!({"message":"revoked"}),
                Some(caller),
                CancellationToken::new()
            )
            .await,
        Err(McpError::ProcessUnavailable)
    );
    assert_eq!(std::fs::read_to_string(marker).unwrap(), "once\n");
    assert!(
        service.status()[0].ready,
        "denied callers must not retire another caller's server"
    );
    service.shutdown().await.unwrap();
}

fn remote_config(
    target: &ExecutionTargetId,
    workspace: &Path,
    marker: &Path,
    mode: &str,
) -> McpConfig {
    let mut script = include_str!("../../../../crates/rsi-mcp/core/tests/support/stdio.py")
        .replace("mode = sys.argv[1]", "assert os.environ['ONLY_SECRET'] == 'fixture-target-only-value'\nassert 'NOTIFY_SOCKET' not in os.environ\nassert 'SSH_AUTH_SOCK' not in os.environ\nassert '/Service-only' not in os.environ.get('PATH', '')\nmode = sys.argv[1]")
        .replace("elif method == 'tools/call':", "elif method == 'tools/call':\n        with open(sys.argv[2], 'a') as calls: calls.write('once\\n')");
    if mode == "legacy" {
        script = script.replace("for raw in sys.stdin.buffer:", r"
import threading
def idle_ping():
    while not os.path.exists(sys.argv[2] + '.ping-ready'): time.sleep(0.005)
    send({'jsonrpc':'2.0','id':'idle-ping','method':'ping'})
threading.Thread(target=idle_ping, daemon=True).start()
for raw in sys.stdin.buffer:").replace("    if method is None:", "    if request.get('id') == 'idle-ping' and 'result' in request:\n        with open(sys.argv[2] + '.pong', 'w') as proof: proof.write('acknowledged')\n    if method is None:");
    }
    McpConfig {
        servers: vec![ServerConfig {
            id: "target".into(),
            enabled: true,
            tools: vec!["echo".into()],
            resource_templates: false,
            transport: TransportConfig::SshStdio {
                target: target.clone(),
                command: "python3".into(),
                arguments: vec![
                    "-u".into(),
                    "-c".into(),
                    script,
                    mode.into(),
                    marker.to_str().unwrap().into(),
                ],
                cwd: workspace.to_str().unwrap().into(),
                environment: [(
                    "ONLY_SECRET".into(),
                    EnvironmentValue::Credential {
                        reference: rsi_credentials_protocol::CredentialRef::new(
                            "rsi.mcp", "allowed",
                        )
                        .unwrap(),
                    },
                )]
                .into(),
            },
        }],
    }
}
