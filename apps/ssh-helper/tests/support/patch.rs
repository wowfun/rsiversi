use rsi_ssh_client::ProcessConnection;
use rsi_ssh_protocol::execution::{Preparation, StartOptions};
use std::{path::Path, time::Duration};

pub async fn verify(client: &ProcessConnection, workspace: &Path) {
    let program = client.resolve("apply_patch").await.unwrap();
    assert!(program.environment.is_empty());
    let path = client
        .canonicalize(workspace.to_str().unwrap())
        .await
        .unwrap();
    let plan = client
        .prepare(Preparation {
            source_reader: false,
            mode: rsi_sandbox::SandboxMode::WorkspaceWrite,
            pty: false,
            program: program.program,
            environment: program.environment,
            cwd: path.clone(),
            workspace: path,
            arguments: vec!["--rsi-run-as-apply-patch".into()],
        })
        .await
        .unwrap();
    let stdin = b"{\"version\":2,\"evidence_bytes\":4096}\n*** Begin Patch\n*** Add File: helper-patch-proof\n+immutable helper patch\n*** End Patch\n".to_vec();
    let process = client
        .spawn(
            plan,
            StartOptions::Batch {
                stdin_bytes: stdin.len(),
                stdout_max_bytes: 8192,
                stderr_max_bytes: 4096,
                termination_grace_ms: 100,
            },
            stdin,
        )
        .await
        .unwrap();
    let outcome = tokio::time::timeout(Duration::from_secs(10), process.wait())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        outcome.exit_code,
        Some(0),
        "{:?}",
        process.stderr().read_from(0).unwrap()
    );
    let output = process.stdout().read_from(0).unwrap().bytes;
    let response: serde_json::Value = serde_json::from_slice(&output).unwrap();
    assert_eq!(response["status"], "applied", "{response}");
    assert_eq!(
        std::fs::read(workspace.join("helper-patch-proof")).unwrap(),
        b"immutable helper patch\n"
    );
}
