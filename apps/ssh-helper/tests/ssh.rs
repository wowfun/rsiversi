#![cfg(target_os = "linux")]
#[path = "support/context.rs"]
mod context;
#[path = "support/files.rs"]
mod files;
#[path = "support/lsp.rs"]
mod lsp;
#[path = "support/mcp.rs"]
mod mcp;
#[path = "support/patch.rs"]
mod patch;
mod support;
use rsi_sandbox::SandboxMode;
use rsi_ssh_client::{HelperArtifact, ProcessConnection};
use rsi_ssh_protocol::{
    execution::{Preparation, StartOptions},
    initialization::{Initialization, ProgramPolicy},
};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    path::Path,
    time::{Duration, Instant},
};

fn configuration() -> Initialization {
    Initialization {
        apply_patch: true,
        workspace_context: true,
        directory_picker: true,
        programs: BTreeMap::from([(
            "shell".into(),
            ProgramPolicy {
                command: "bash".into(),
                environment: vec![],
            },
        )]),
    }
}
async fn artifact() -> HelperArtifact {
    let binary =
        std::env::var_os("RSI_TEST_HELPER_BIN").expect("explicit RSI_TEST_HELPER_BIN musl image");
    let digest = Sha256::digest(std::fs::read(&binary).unwrap()).into();
    HelperArtifact::load(Path::new(&binary), digest)
        .await
        .unwrap()
}
async fn prepare(
    client: &ProcessConnection,
    root: &Path,
    script: &str,
    pty: bool,
) -> rsi_ssh_client::RemotePlan {
    let program = client.resolve("shell").await.unwrap();
    client
        .prepare(Preparation {
            source_reader: false,
            program: program.program,
            environment: program.environment,
            arguments: vec![
                "--noprofile".into(),
                "--norc".into(),
                "-c".into(),
                script.into(),
            ],
            workspace: root.to_str().unwrap().into(),
            cwd: root.to_str().unwrap().into(),
            pty,
            mode: SandboxMode::WorkspaceWrite,
        })
        .await
        .unwrap()
}
async fn bounded<T>(future: impl std::future::Future<Output = T>) -> T {
    tokio::time::timeout(Duration::from_secs(50), future)
        .await
        .expect("SSH fixture deadline")
}
fn unit_absent(service: &str, epoch: u64) -> bool {
    let unit = rsi_ssh_helper::TransientUnit::new(service, epoch).unwrap();
    let output = std::process::Command::new("/usr/bin/systemctl")
        .args([
            "--user",
            "show",
            unit.name(),
            "--property=LoadState",
            "--value",
        ])
        .output()
        .unwrap();
    String::from_utf8_lossy(&output.stdout).trim() == "not-found"
}
async fn await_absent(service: &str, epoch: u64, configuration: &Path) {
    bounded(async {
        while configuration.exists() || !unit_absent(service, epoch) {
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await;
}

#[tokio::test]
#[ignore = "requires explicit isolated sshd fixture, musl helper and native Linux user systemd/bwrap"]
async fn actual_ssh_upload_executes_and_keeps_owner_until_last_client_then_reaps() {
    let server = support::Server::new();
    let image = artifact().await;
    let mut random = [0; 16];
    getrandom::fill(&mut random).unwrap();
    let service = hex::encode(random);
    let prepared = server.prepare();
    let configuration_path = prepared.directory().to_path_buf();
    let connected = bounded(rsi_ssh_client::connect(
        prepared,
        Path::new("/usr/bin/ssh"),
        image.clone(),
        &service,
        1,
        configuration(),
    ))
    .await
    .unwrap();
    assert!(connected.unavailable().is_empty());
    let client = connected.client();
    drop(connected);
    assert!(configuration_path.exists());
    let workspace = tempfile::tempdir().unwrap();
    let plan = prepare(
        &client,
        workspace.path(),
        "cat > proof; printf verified",
        false,
    )
    .await;
    let process = bounded(client.spawn(
        plan,
        StartOptions::Batch {
            stdin_bytes: 4 * 1024 * 1024,
            stdout_max_bytes: 4096,
            stderr_max_bytes: 4096,
            termination_grace_ms: 100,
        },
        vec![42; 4 * 1024 * 1024],
    ))
    .await
    .unwrap();
    assert_eq!(bounded(process.wait()).await.unwrap().exit_code, Some(0));
    assert_eq!(process.stdout().read_from(0).unwrap().bytes, b"verified");
    assert_eq!(
        std::fs::metadata(workspace.path().join("proof"))
            .unwrap()
            .len(),
        4 * 1024 * 1024
    );
    drop(process);
    patch::verify(&client, workspace.path()).await;
    context::verify(&client, workspace.path()).await;
    files::verify(&client, workspace.path()).await;
    lsp::verify(&client, workspace.path()).await;
    mcp::verify(&client, workspace.path()).await;
    saturated_terminal(&client, workspace.path()).await;
    drop(client);
    await_absent(&service, 1, &configuration_path).await;

    let prepared = server.prepare();
    let configuration_path = prepared.directory().to_path_buf();
    let connected = bounded(rsi_ssh_client::connect(
        prepared,
        Path::new("/usr/bin/ssh"),
        image,
        &service,
        2,
        configuration(),
    ))
    .await
    .unwrap();
    let client = connected.client();
    let plan = prepare(
        &client,
        workspace.path(),
        "trap '' TERM; /usr/bin/setsid /bin/sh -c 'trap \"\" TERM; sleep 60' & wait",
        false,
    )
    .await;
    let process = client
        .spawn(
            plan,
            StartOptions::Batch {
                stdin_bytes: 0,
                stdout_max_bytes: 4096,
                stderr_max_bytes: 4096,
                termination_grace_ms: 100,
            },
            vec![],
        )
        .await
        .unwrap();
    bounded(connected.shutdown()).await.unwrap();
    assert!(matches!(
        bounded(process.wait()).await,
        Err(rsi_process::ProcessError::OutcomeUnknown)
    ));
    drop(process);
    drop(client);
    drop(connected);
    await_absent(&service, 2, &configuration_path).await;
}

async fn saturated_terminal(client: &ProcessConnection, workspace: &Path) {
    let start = Instant::now();
    assert_eq!(
        client
            .canonicalize(workspace.to_str().unwrap())
            .await
            .unwrap(),
        workspace.to_str().unwrap()
    );
    let baseline = start.elapsed();
    let plan = prepare(client, workspace, "exec /usr/bin/yes", true).await;
    let terminal = client
        .spawn_pty(
            plan,
            StartOptions::Pty {
                columns: 80,
                rows: 24,
                termination_grace_ms: 100,
            },
        )
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(250)).await;
    let start = Instant::now();
    bounded(terminal.resize(rsi_process::PtySize {
        columns: 100,
        rows: 30,
    }))
    .await
    .unwrap();
    let resize = start.elapsed();
    let start = Instant::now();
    bounded(terminal.terminate_acknowledged()).await.unwrap();
    let cancel = start.elapsed();
    eprintln!(
        "actual SSH: canonicalize RTT {baseline:?}; saturated PTY resize ACK {resize:?}; cancel ACK {cancel:?}"
    );
    assert!(resize <= baseline * 2 + Duration::from_millis(25));
    assert!(cancel <= baseline * 2 + Duration::from_millis(25));
    let _ = bounded(terminal.wait()).await;
    drop(terminal);
}
