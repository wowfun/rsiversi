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

#[tokio::test]
#[ignore = "requires explicit isolated sshd, musl helper and Linux user systemd"]
#[expect(
    clippy::too_many_lines,
    reason = "one isolated SSH namespace covers concurrent refusal, caller loss and later independent epochs"
)]
async fn concurrent_ssh_cache_contention_is_typed_and_cancelled_startup_is_not_replayed() {
    use std::{
        io::{BufRead, BufReader, Write},
        os::unix::fs::{MetadataExt, PermissionsExt},
        process::Stdio,
    };
    let server = support::Server::new();
    let image = artifact().await;
    let mut random = [0; 16];
    getrandom::fill(&mut random).unwrap();
    let service = hex::encode(random);
    let uid = std::process::Command::new("/usr/bin/id")
        .arg("-u")
        .output()
        .unwrap();
    assert!(uid.status.success());
    let uid = String::from_utf8(uid.stdout)
        .unwrap()
        .trim()
        .parse::<u32>()
        .unwrap();
    let runtime = std::path::PathBuf::from(format!("/run/user/{uid}"));
    let cache = rsi_ssh_helper::ArtifactCache::open(&runtime, &service).unwrap();
    cache
        .collect(rsi_ssh_helper::WriterLockPolicy::Immediate)
        .unwrap();
    let writer = runtime.join("rsi-ssh").join(&service).join("writer.lock");
    let mut locked = std::process::Command::new("/usr/bin/flock")
        .arg(&writer)
        .args(["/bin/sh", "-c", "printf 'ready\\n'; read ignored"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let mut ready = String::new();
    BufReader::new(locked.stdout.take().unwrap())
        .read_line(&mut ready)
        .unwrap();
    assert_eq!(ready, "ready\n");
    let started = Instant::now();
    let (first, second) = tokio::join!(
        bounded(rsi_ssh_client::connect(
            server.prepare(),
            Path::new("/usr/bin/ssh"),
            image.clone(),
            &service,
            1,
            configuration()
        )),
        bounded(rsi_ssh_client::connect(
            server.prepare(),
            Path::new("/usr/bin/ssh"),
            image.clone(),
            &service,
            2,
            configuration()
        ))
    );
    assert!(
        matches!(
            first,
            Err(rsi_ssh_client::SshClientError::CacheContentionTimeout)
        ),
        "{first:?}"
    );
    assert!(
        matches!(
            second,
            Err(rsi_ssh_client::SshClientError::CacheContentionTimeout)
        ),
        "{second:?}"
    );
    assert!(unit_absent(&service, 1) && unit_absent(&service, 2));
    println!("contention_pair_ms={}", started.elapsed().as_millis());
    let prepared = server.prepare();
    let path = prepared.directory().to_path_buf();
    let launch = tempfile::tempdir().unwrap();
    let executable = launch.path().join("ssh");
    std::fs::write(
        &executable,
        b"#!/bin/sh\nprintf 'started\\n' >> \"$0.started\"\nexec /usr/bin/ssh \"$@\"\n",
    )
    .unwrap();
    std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o700)).unwrap();
    let started_path = launch.path().join("ssh.started");
    let pending_image = image.clone();
    let pending_service = service.clone();
    let pending = tokio::spawn(async move {
        rsi_ssh_client::connect(
            prepared,
            &executable,
            pending_image,
            &pending_service,
            3,
            configuration(),
        )
        .await
    });
    bounded(async {
        while !started_path.exists() {
            assert!(
                !pending.is_finished(),
                "startup ended before the native launch barrier"
            );
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await;
    pending.abort();
    assert!(pending.await.unwrap_err().is_cancelled());
    await_absent(&service, 3, &path).await;
    assert_eq!(std::fs::read_to_string(&started_path).unwrap(), "started\n");
    locked
        .stdin
        .take()
        .unwrap()
        .write_all(b"release\n")
        .unwrap();
    assert!(locked.wait().unwrap().success());
    let first_prepared = server.prepare();
    let first_path = first_prepared.directory().to_path_buf();
    let second_prepared = server.prepare();
    let second_path = second_prepared.directory().to_path_buf();
    let started = Instant::now();
    let (first, second) = tokio::join!(
        bounded(rsi_ssh_client::connect(
            first_prepared,
            Path::new("/usr/bin/ssh"),
            image.clone(),
            &service,
            4,
            configuration()
        )),
        bounded(rsi_ssh_client::connect(
            second_prepared,
            Path::new("/usr/bin/ssh"),
            image.clone(),
            &service,
            5,
            configuration()
        ))
    );
    let first = first.unwrap();
    let second = second.unwrap();
    println!("uncontended_pair_ms={}", started.elapsed().as_millis());
    first.shutdown().await.unwrap();
    second.shutdown().await.unwrap();
    await_absent(&service, 4, &first_path).await;
    await_absent(&service, 5, &second_path).await;
    assert_eq!(std::fs::read_to_string(&started_path).unwrap(), "started\n");
    let installed = runtime
        .join("rsi-ssh")
        .join(&service)
        .join(hex::encode(image.digest()));
    let inode = std::fs::metadata(&installed).unwrap().ino();
    std::fs::set_permissions(&installed, std::fs::Permissions::from_mode(0o600)).unwrap();
    std::fs::write(&installed, b"corrupt inactive helper").unwrap();
    std::fs::set_permissions(&installed, std::fs::Permissions::from_mode(0o500)).unwrap();
    assert!(matches!(
        cache.acquire(image.digest(), rsi_ssh_helper::WriterLockPolicy::Immediate),
        Err(rsi_ssh_helper::CacheError::Digest)
    ));
    let repaired_prepared = server.prepare();
    let repaired_path = repaired_prepared.directory().to_path_buf();
    let repaired = bounded(rsi_ssh_client::connect(
        repaired_prepared,
        Path::new("/usr/bin/ssh"),
        image.clone(),
        &service,
        6,
        configuration(),
    ))
    .await
    .unwrap();
    let lease = cache
        .acquire(image.digest(), rsi_ssh_helper::WriterLockPolicy::Immediate)
        .unwrap();
    assert_ne!(std::fs::metadata(lease.path()).unwrap().ino(), inode);
    assert_eq!(
        std::fs::metadata(lease.path())
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o500
    );
    drop(lease);
    repaired.shutdown().await.unwrap();
    await_absent(&service, 6, &repaired_path).await;
    println!(
        "actual_installer_digest_repair=true; successful_epochs_reaped=4,5,6; cancelled_native_launches=1"
    );
}
