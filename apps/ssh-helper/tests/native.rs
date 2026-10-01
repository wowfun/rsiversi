#![cfg(target_os = "linux")]
#[path = "support/context.rs"]
mod context;
#[path = "support/files.rs"]
mod files;
#[path = "support/patch.rs"]
mod patch;
use rsi_sandbox::SandboxMode;
use rsi_ssh_client::ProcessConnection;
use rsi_ssh_protocol::{
    execution::{Preparation, StartOptions},
    initialization::{Initialization, ProgramPolicy},
};
use rsi_ssh_transport::{Connection, Role};
use sha2::{Digest, Sha256};
use std::{collections::BTreeMap, process::Stdio, time::Duration};
use tokio::io::AsyncReadExt;

struct Cleanup {
    unit: String,
    child: tokio::process::Child,
}
impl Drop for Cleanup {
    fn drop(&mut self) {
        let _ = std::process::Command::new("/usr/bin/systemctl")
            .args(["--user", "stop", "--", &self.unit])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
        let _ = self.child.start_kill();
    }
}
async fn bounded<T>(future: impl std::future::Future<Output = T>) -> T {
    tokio::time::timeout(Duration::from_secs(15), future)
        .await
        .expect("native helper deadline")
}

#[tokio::test]
#[ignore = "requires native Linux user systemd, cgroup v2, bwrap, openat2 and executable private runtime"]
async fn installed_application_initializes_executes_and_exits_its_actual_transient_unit() {
    installed_application(false).await;
}

#[tokio::test]
#[ignore = "requires native Linux user systemd, cgroup v2, bwrap and executable private runtime"]
async fn helper_sigkill_during_native_input_ack_reports_unknown_and_reaps_unit() {
    installed_application(true).await;
}

async fn installed_application(kill_during_request: bool) {
    let binary = std::env::var_os("RSI_TEST_HELPER_BIN")
        .unwrap_or_else(|| env!("CARGO_BIN_EXE_rsi-ssh-helper").into());
    let bytes = std::fs::read(&binary).unwrap();
    assert!(
        bytes.len() <= 128 * 1024 * 1024,
        "use a release or debug=0 helper image"
    );
    let digest: [u8; 32] = Sha256::digest(&bytes).into();
    let mut namespace = [0; 16];
    getrandom::fill(&mut namespace).unwrap();
    let service = hex::encode(namespace);
    let epoch = 1u64;
    let unit = rsi_ssh_helper::TransientUnit::new(&service, epoch).unwrap();
    let root = tempfile::tempdir().unwrap();
    let child = tokio::process::Command::new(binary)
        .args([
            "install-launch",
            &service,
            &epoch.to_string(),
            &hex::encode(digest),
        ])
        .env("RSI_SERVICE_SECRET", "private-fixture")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut cleanup = Cleanup {
        unit: unit.name().into(),
        child,
    };
    let stderr = cleanup.child.stderr.take().unwrap();
    let diagnostics = tokio::spawn(async move {
        let mut output = Vec::new();
        stderr
            .take(16 * 1024)
            .read_to_end(&mut output)
            .await
            .unwrap();
        String::from_utf8_lossy(&output).into_owned()
    });
    let (connection, _) = Connection::start(
        cleanup.child.stdout.take().unwrap(),
        cleanup.child.stdin.take().unwrap(),
        Role::Client,
        epoch,
    )
    .unwrap();
    let client = ProcessConnection::new(connection.clone()).unwrap();
    let heartbeat_connection = connection.clone();
    let heartbeat = tokio::spawn(async move {
        loop {
            tokio::select! { () = heartbeat_connection.closed() => return, () = tokio::time::sleep(Duration::from_secs(2)) => {} }
            if heartbeat_connection.heartbeat().await.is_err() {
                return;
            }
        }
    });
    let unavailable = bounded(client.initialize(configuration(&service), &digest)).await;
    if let Err(error) = &unavailable {
        connection.close();
        let _ = bounded(cleanup.child.wait()).await;
        panic!(
            "initialization failed: {error}; {}",
            diagnostics.await.unwrap()
        );
    }
    assert_eq!(unavailable.unwrap(), vec!["absent"]);
    if kill_during_request {
        verify_killed_request(&client, root.path(), &cleanup.unit).await;
    } else {
        verify_execution(&client, root.path()).await;
        patch::verify(&client, root.path()).await;
        context::verify(&client, root.path()).await;
        files::verify(&client, root.path()).await;
    }
    connection.close();
    connection.settled().await;
    heartbeat.await.unwrap();
    let status = bounded(cleanup.child.wait()).await.unwrap();
    let diagnostics = diagnostics.await.unwrap();
    if !kill_during_request {
        assert!(status.success(), "{diagnostics}");
    }

    let state = std::process::Command::new("/usr/bin/systemctl")
        .args([
            "--user",
            "show",
            &cleanup.unit,
            "--property=LoadState",
            "--value",
        ])
        .output()
        .unwrap();
    assert_eq!(String::from_utf8_lossy(&state.stdout).trim(), "not-found");
}

async fn verify_killed_request(client: &ProcessConnection, root: &std::path::Path, unit: &str) {
    use rsi_process::ProcessError;
    let program = client.resolve("shell").await.unwrap();
    let path = client.canonicalize(root.to_str().unwrap()).await.unwrap();
    let plan = client
        .prepare(Preparation {
            source_reader: false,
            mode: SandboxMode::WorkspaceWrite,
            pty: false,
            program: program.program,
            environment: program.environment,
            cwd: path.clone(),
            workspace: path,
            arguments: vec![
                "--noprofile".into(),
                "--norc".into(),
                "-c".into(),
                "exec /bin/sleep 60".into(),
            ],
        })
        .await
        .unwrap();
    let process = client
        .spawn_duplex(
            plan,
            StartOptions::Duplex {
                stdout_buffer_bytes: 4096,
                stderr_max_bytes: 4096,
                termination_grace_ms: 100,
            },
        )
        .await
        .unwrap();
    let input = process.stdin();
    let bytes = vec![b'x'; 65536];
    let mut pressured = false;
    for _ in 0..64 {
        match bounded(input.write(&bytes)).await {
            Ok(_) => {}
            Err(ProcessError::Capacity) => {
                pressured = true;
                break;
            }
            other => panic!("unexpected native pressure result: {other:?}"),
        }
    }
    assert!(pressured);
    let waiting = tokio::spawn(async move { input.write(&bytes).await });
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(
        !waiting.is_finished(),
        "native ACK must still be pending at SIGKILL"
    );
    assert!(
        tokio::process::Command::new("/usr/bin/systemctl")
            .args([
                "--user",
                "kill",
                "--kill-whom=main",
                "--signal=KILL",
                "--",
                unit
            ])
            .status()
            .await
            .unwrap()
            .success()
    );
    assert_eq!(
        bounded(waiting).await.unwrap(),
        Err(ProcessError::OutcomeUnknown)
    );
    assert!(bounded(client.resolve("shell")).await.is_err());
    drop(process);
}

async fn verify_execution(client: &ProcessConnection, root: &std::path::Path) {
    let program = client.resolve("shell").await.unwrap();
    assert_eq!(
        program
            .environment
            .iter()
            .find(|(key, _)| key == "PATH")
            .unwrap()
            .1,
        "/usr/local/bin:/usr/bin:/bin"
    );
    let path = client.canonicalize(root.to_str().unwrap()).await.unwrap();
    let plan = client.prepare(Preparation { source_reader: false, mode: SandboxMode::WorkspaceWrite, pty: false, program: program.program, environment: program.environment, cwd: path.clone(), workspace: path, arguments: vec!["--noprofile".into(), "--norc".into(), "-c".into(), "printf target > proof; printf '%s:%s:%s:%s' \"${RSI_SERVICE_SECRET-unset}\" \"${NOTIFY_SOCKET-unset}\" \"${WATCHDOG_USEC-unset}\" \"${DBUS_SESSION_BUS_ADDRESS-unset}\"".into()] }).await.unwrap();
    let process = bounded(client.spawn(
        plan,
        StartOptions::Batch {
            stdin_bytes: 0,
            stdout_max_bytes: 4096,
            stderr_max_bytes: 4096,
            termination_grace_ms: 100,
        },
        vec![],
    ))
    .await
    .unwrap();
    let outcome = bounded(process.wait()).await.unwrap();
    assert_eq!(
        outcome.exit_code,
        Some(0),
        "{:?}",
        process.stderr().read_from(0).unwrap()
    );
    assert_eq!(
        process.stdout().read_from(0).unwrap().bytes,
        b"unset:unset:unset:unset"
    );
    assert_eq!(std::fs::read(root.join("proof")).unwrap(), b"target");
    drop(process);
}

fn configuration(service: &str) -> Initialization {
    Initialization {
        apply_patch: true,
        workspace_context: true,
        directory_picker: true,
        programs: BTreeMap::from([
            (
                "shell".into(),
                ProgramPolicy {
                    command: "bash".into(),
                    environment: vec![],
                },
            ),
            (
                "absent".into(),
                ProgramPolicy {
                    command: format!("rsi-absent-{service}"),
                    environment: vec![],
                },
            ),
        ]),
    }
}
