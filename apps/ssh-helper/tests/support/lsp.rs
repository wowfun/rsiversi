//! Real SSH transport and target Python peer; language semantics use the separate native RA check.
use rsi_execution::{
    ExecutionAdmission, ExecutionCoordinates, ExecutionLocation, ExecutionOperation,
    ExecutionTargetId, HostEpoch, TargetProgram,
};
use rsi_lsp::{Config, LanguageService, LanguageWorkspace, Operation, Query, QueryResult};
use rsi_ssh_client::ProcessConnection;
use std::{path::Path, sync::Arc};
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
#[derive(Debug)]
struct Unused;
#[async_trait::async_trait]
impl rsi_sandbox::Sandbox for Unused {
    async fn workspace_read(
        &self,
        _: rsi_sandbox::WorkspaceReadRequest,
    ) -> rsi_sandbox::Result<rsi_sandbox::WorkspaceReadScope> {
        panic!("SSH LSP reached Service workspace reader");
    }
    async fn confine(
        &self,
        _: rsi_sandbox::ProcessRequest,
    ) -> rsi_sandbox::Result<rsi_sandbox::ConfinedProcess> {
        panic!("SSH LSP reached Service sandbox");
    }
}
#[async_trait::async_trait]
impl rsi_process::DuplexProcess for Unused {
    async fn spawn(
        &self,
        _: rsi_process::DuplexProcessSpec,
    ) -> rsi_process::Result<rsi_process::ManagedDuplexProcess> {
        panic!("SSH LSP reached Service process");
    }
}
pub async fn verify(client: &ProcessConnection, workspace: &Path) {
    let root = workspace.join("language");
    std::fs::create_dir(&root).unwrap();
    std::fs::write(
        root.join("main.rs"),
        "fn target() {}\nfn main() { target(); }\n",
    )
    .unwrap();
    let target = ExecutionTargetId::parse("c".repeat(32)).unwrap();
    let provider = rsi_ssh_client::execution_provider(
        HostEpoch::from_bytes([1; 16]),
        client.clone(),
        target.clone(),
        1,
    )
    .unwrap();
    let gate = Arc::new(Gate::default());
    let lease = provider.lease(gate.clone()).unwrap();
    let coordinates = ExecutionCoordinates::new(
        ExecutionLocation::Ssh { target },
        client.canonicalize(root.to_str().unwrap()).await.unwrap(),
    )
    .unwrap();
    let authority = LanguageWorkspace::new(coordinates, Some(lease)).unwrap();
    let runtime = rsi_meta::Runtime::default();
    let native_files = Arc::new(rsi_files::LocalFiles::new().unwrap());
    native_files.close().await;
    let service = LanguageService::new(
        remote_config(),
        Arc::new(Unused),
        Arc::new(Unused),
        native_files,
        runtime.execution().clone(),
    )
    .unwrap();
    for operation in [
        Operation::Definition,
        Operation::References,
        Operation::Implementation,
        Operation::Hover,
    ] {
        let output = service
            .query(
                authority.clone(),
                Query {
                    operation,
                    path: "main.rs".into(),
                    line: 2,
                    column: 13,
                },
                CancellationToken::new(),
            )
            .await
            .unwrap();
        match output.result {
            QueryResult::Locations { locations } => {
                assert_eq!(locations.len(), 1);
                assert_eq!(locations[0].path, "main.rs");
            }
            QueryResult::Hover { text, .. } => assert_eq!(text, "fixture hover"),
        }
    }
    std::fs::write(root.join("main.rs"), "// target refresh\n").unwrap();
    assert_eq!(
        service
            .current_file(
                authority.clone(),
                "main.rs".into(),
                CancellationToken::new()
            )
            .await
            .unwrap(),
        "// target refresh\n"
    );
    gate.0.cancel();
    assert_eq!(
        service
            .current_file(authority, "main.rs".into(), CancellationToken::new())
            .await,
        Err(rsi_lsp::Error::Unavailable)
    );
    service.close().await.unwrap();
    assert!(runtime.shutdown().await.is_clean());
}

fn remote_config() -> Config {
    Config {
        program: "/unavailable/Service/python3".into(),
        remote_program: Some(TargetProgram {
            command: "python3".into(),
            environment: vec![
                ("LSP_TEST_MODE".into(), "normal".into()),
                ("LSP_TEST_LOG".into(), "/tmp/lsp-fixture.jsonl".into()),
            ],
        }),
        arguments: vec![
            "-c".into(),
            include_str!("../../../../fixtures/rsi/lsp/test_peer.py").into(),
        ],
        environment: [("HOME".into(), "/Service-only".into())].into(),
        languages: [(".rs".into(), "rust".into())].into(),
        initialization_options: serde_json::Value::Null,
        configuration: serde_json::Value::Null,
    }
}
