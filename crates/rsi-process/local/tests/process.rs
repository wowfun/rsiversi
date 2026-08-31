#![cfg(unix)]

use rsi_meta::{ResolvedFactory, Runtime, UpdateMode};
use rsi_process::{Process, ProcessContract, ProcessError, ProcessSpec};
use rsi_process_local::ProcessLocalFactory;
use rsi_sandbox::{
    ConfinedProcess, EnforcementStamp, SandboxBackend, SandboxFileSystem, SandboxMode,
    SandboxNetwork, SandboxScratch,
};
use serde_json::json;
use std::ffi::OsString;
use std::path::PathBuf;
use std::sync::Arc;

fn unconfined_shell(script: &str) -> ConfinedProcess {
    let workspace = std::env::current_dir().unwrap().canonicalize().unwrap();
    ConfinedProcess {
        program: PathBuf::from("/bin/sh").canonicalize().unwrap(),
        arguments: vec![OsString::from("-c"), OsString::from(script)],
        cwd: workspace.clone(),
        stamp: EnforcementStamp {
            requested: SandboxMode::DangerFullAccess,
            backend: SandboxBackend::Unconfined,
            workspace,
            filesystem: SandboxFileSystem::Unconfined,
            scratch: SandboxScratch::Host,
            network: SandboxNetwork::Host,
        },
    }
}

fn spec(script: &str, capture: usize) -> ProcessSpec {
    ProcessSpec {
        process: unconfined_shell(script),
        stdin: Vec::new(),
        environment: Vec::new(),
        stdout_max_bytes: capture,
        stderr_max_bytes: capture,
        termination_grace_ms: 50,
    }
}

async fn activated(config: serde_json::Value) -> (rsi_meta::FiberHandle, Arc<dyn Process>) {
    let runtime = Runtime::default();
    let fiber = runtime
        .root()
        .apply(
            ResolvedFactory::linked(
                "rsi.process.local",
                "test",
                UpdateMode::Replayable,
                Arc::new(ProcessLocalFactory),
            ),
            config,
        )
        .await
        .unwrap();
    let process = runtime.root().lookup_local::<ProcessContract>().unwrap();
    (fiber, process)
}

#[tokio::test]
async fn raw_tail_reads_use_global_offsets_and_report_an_expired_cursor() {
    let (fiber, process) = activated(json!({})).await;
    let managed = process
        .spawn(spec(
            "i=0; while [ $i -lt 65537 ]; do printf x; i=$((i+1)); done",
            65_536,
        ))
        .unwrap();
    assert_eq!(managed.wait().await.unwrap().exit_code, Some(0));
    let complete = managed.stdout().read_from(0).unwrap();
    assert_eq!(complete.bytes.len(), 65_536);
    assert_eq!(complete.oldest_offset, 1);
    assert_eq!(complete.next_offset, 65_537);
    assert!(complete.lossy);
    assert!(complete.bytes.iter().all(|byte| *byte == b'x'));
    let delta = managed.stdout().read_from(65_536).unwrap();
    assert_eq!(delta.bytes, b"x");
    assert!(!delta.lossy);

    drop(managed);
    drop(process);
    assert!(fiber.dispose().await.is_clean());
}

#[tokio::test]
async fn capture_reservation_survives_exit_until_the_handle_is_dropped() {
    let (fiber, process) = activated(json!({"maximum_capture_bytes":65536})).await;
    let retained = process.spawn(spec("exit 0", 32_768)).unwrap();
    retained.wait().await.unwrap();
    assert!(matches!(
        process.spawn(spec("exit 0", 1)),
        Err(ProcessError::Capacity)
    ));
    drop(retained);
    let after_compaction = process.spawn(spec("exit 0", 1)).unwrap();
    after_compaction.wait().await.unwrap();
    drop(after_compaction);
    drop(process);
    assert!(fiber.dispose().await.is_clean());
}

#[tokio::test]
#[cfg(target_os = "linux")]
async fn active_capacity_releases_after_settlement_and_termination_kills_the_group() {
    let (fiber, process) = activated(json!({"maximum_active_processes":1})).await;
    let managed = process
        .spawn(spec("/bin/sleep 30 & child=$!; echo $child; wait", 1024))
        .unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(1), async {
        loop {
            if !managed.stdout().read_from(0).unwrap().bytes.is_empty() {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert!(matches!(
        process.spawn(spec("exit 0", 1)),
        Err(ProcessError::Capacity)
    ));
    let child = String::from_utf8(managed.stdout().read_from(0).unwrap().bytes)
        .unwrap()
        .trim()
        .parse::<u32>()
        .unwrap();
    managed.terminate();
    let outcome = managed.wait().await.unwrap();
    assert!(outcome.signal.is_some());
    tokio::time::timeout(std::time::Duration::from_secs(1), async {
        while PathBuf::from(format!("/proc/{child}")).exists() {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("managed process-group child must be reaped");

    let replacement = process.spawn(spec("exit 0", 1)).unwrap();
    replacement.wait().await.unwrap();
    drop((replacement, managed));
    drop(process);
    assert!(fiber.dispose().await.is_clean());
}

#[tokio::test]
#[cfg(target_os = "linux")]
async fn successful_leader_exit_closes_descendants_before_outcome_and_capacity_release() {
    let (fiber, process) = activated(json!({"maximum_active_processes":1})).await;
    let managed = process
        .spawn(spec("/bin/sleep 30 & child=$!; echo $child; exit 0", 1024))
        .unwrap();
    let outcome = tokio::time::timeout(std::time::Duration::from_secs(2), managed.wait())
        .await
        .expect("managed descendants must be closed within the termination grace")
        .unwrap();
    assert_eq!(outcome.exit_code, Some(0));
    let child = String::from_utf8(managed.stdout().read_from(0).unwrap().bytes)
        .unwrap()
        .trim()
        .parse::<u32>()
        .unwrap();
    assert!(
        !PathBuf::from(format!("/proc/{child}")).exists(),
        "wait returned while a managed descendant remained live"
    );

    let replacement = process.spawn(spec("exit 0", 1)).unwrap();
    replacement.wait().await.unwrap();
    drop((replacement, managed));
    drop(process);
    assert!(fiber.dispose().await.is_clean());
}

#[tokio::test]
#[cfg(target_os = "linux")]
async fn outcome_waits_for_blocked_stdin_delivery_to_settle() {
    let (fiber, process) = activated(json!({})).await;
    let mut request = spec("/bin/sleep 30 & child=$!; echo $child; exit 0", 1024);
    request.stdin = vec![b'x'; rsi_process::MAXIMUM_PROCESS_STDIN_BYTES];
    let managed = process.spawn(request).unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(2), managed.wait())
        .await
        .expect("group cleanup must unblock and join stdin delivery")
        .unwrap();
    let child = String::from_utf8(managed.stdout().read_from(0).unwrap().bytes)
        .unwrap()
        .trim()
        .parse::<u32>()
        .unwrap();
    assert!(!PathBuf::from(format!("/proc/{child}")).exists());

    drop(managed);
    drop(process);
    assert!(fiber.dispose().await.is_clean());
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn escaped_stdin_reader_cannot_block_settlement_forever() {
    let (fiber, process) = activated(json!({})).await;
    let temporary = tempfile::tempdir().unwrap();
    let marker = temporary.path().join("escaped-pgid");
    let script = format!(
        "exec 3<&0; /usr/bin/setsid /bin/sh -c 'exec 0<&3; echo $$ > {}; /bin/sleep 30' & while [ ! -s {} ]; do :; done; exit 0",
        marker.display(),
        marker.display()
    );
    let mut request = spec(&script, 1024);
    request.stdin = vec![b'x'; rsi_process::MAXIMUM_PROCESS_STDIN_BYTES];
    let managed = process.spawn(request).unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(1), async {
        while marker.metadata().map_or(0, |metadata| metadata.len()) == 0 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("escaped descendant did not publish its process-group id");
    let escaped_group = std::fs::read_to_string(&marker)
        .unwrap()
        .trim()
        .parse::<u32>()
        .unwrap();

    let first_wait =
        tokio::time::timeout(std::time::Duration::from_millis(250), managed.wait()).await;
    if first_wait.is_err() {
        let status = std::process::Command::new("/bin/kill")
            .args(["-KILL", "--", &format!("-{escaped_group}")])
            .status()
            .unwrap();
        assert!(status.success());
        tokio::time::timeout(std::time::Duration::from_secs(2), managed.wait())
            .await
            .expect("escaped descendant cleanup did not release stdin")
            .unwrap();
    }
    assert!(
        first_wait.is_ok(),
        "an escaped descendant kept stdin delivery and process capacity live"
    );

    drop(managed);
    drop(process);
    assert!(fiber.dispose().await.is_clean());
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn escaped_pipe_writer_makes_terminal_output_explicitly_incomplete() {
    let (fiber, process) = activated(json!({})).await;
    let temporary = tempfile::tempdir().unwrap();
    let marker = temporary.path().join("escaped-pipe-pgid");
    let script = format!(
        "/usr/bin/setsid /bin/sh -c 'echo $$ > {}; /bin/sleep 30' & while [ ! -s {} ]; do :; done; exit 0",
        marker.display(),
        marker.display()
    );
    let managed = process.spawn(spec(&script, 1024)).unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(1), async {
        while marker.metadata().map_or(0, |metadata| metadata.len()) == 0 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("escaped pipe writer did not publish its process-group id");
    let escaped_group = std::fs::read_to_string(&marker)
        .unwrap()
        .trim()
        .parse::<u32>()
        .unwrap();

    let outcome = tokio::time::timeout(std::time::Duration::from_secs(1), managed.wait())
        .await
        .expect("escaped pipe writer kept process settlement live");
    let status = std::process::Command::new("/bin/kill")
        .args(["-KILL", "--", &format!("-{escaped_group}")])
        .status()
        .unwrap();
    assert!(status.success());
    assert!(
        matches!(outcome, Err(ProcessError::Io(message)) if message.contains("drain timed out"))
    );

    drop(managed);
    drop(process);
    assert!(fiber.dispose().await.is_clean());
}

#[tokio::test]
async fn readers_preserve_utf8_sequences_split_across_pipe_chunks_as_raw_bytes() {
    let (fiber, process) = activated(json!({})).await;
    let managed = process
        .spawn(spec(
            "printf '\\342\\202'; /bin/sleep 0.05; printf '\\254'",
            16,
        ))
        .unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(1), async {
        loop {
            if managed.stdout().read_from(0).unwrap().next_offset >= 2 {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert_eq!(managed.stdout().read_from(0).unwrap().bytes, [0xe2, 0x82]);
    managed.wait().await.unwrap();
    assert_eq!(managed.stdout().read_from(0).unwrap().bytes, "€".as_bytes());

    drop(managed);
    drop(process);
    assert!(fiber.dispose().await.is_clean());
}

#[tokio::test]
async fn per_stream_capture_limit_rejects_limit_plus_one_before_spawn() {
    let (fiber, process) = activated(json!({})).await;
    let mut request = spec("exit 0", rsi_process::MAXIMUM_PROCESS_STREAM_BYTES);
    request.stderr_max_bytes = 1;
    let maximum = process.spawn(request).unwrap();
    maximum.wait().await.unwrap();
    drop(maximum);

    let mut overflow = spec("printf must-not-run", 1);
    overflow.stdout_max_bytes = rsi_process::MAXIMUM_PROCESS_STREAM_BYTES + 1;
    assert!(matches!(
        process.spawn(overflow),
        Err(ProcessError::InvalidInput(message)) if message.contains("stdout_max_bytes")
    ));
    drop(process);
    assert!(fiber.dispose().await.is_clean());
}

#[tokio::test]
#[cfg(target_os = "linux")]
async fn provider_retirement_escalates_term_to_kill_and_waits_for_reaping() {
    let (fiber, process) = activated(json!({"shutdown_timeout_ms":1000})).await;
    let managed = process
        .spawn(spec("trap '' TERM; while :; do /bin/sleep 1; done", 1024))
        .unwrap();
    let pid = managed.pid();
    tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    drop(process);
    assert!(fiber.dispose().await.is_clean());
    let outcome = managed.wait().await.unwrap();
    assert_eq!(outcome.signal, Some(9));
    assert!(!PathBuf::from(format!("/proc/{pid}")).exists());
}

#[tokio::test]
#[cfg(target_os = "linux")]
async fn timed_out_provider_retirement_keeps_escalation_ownership_until_reaping() {
    let (fiber, process) = activated(json!({"shutdown_timeout_ms":1})).await;
    let mut request = spec(
        "trap '' TERM; printf ready; while :; do /bin/sleep 1; done",
        1024,
    );
    request.termination_grace_ms = 100;
    let managed = process.spawn(request).unwrap();
    let pid = managed.pid();
    tokio::time::timeout(std::time::Duration::from_secs(1), async {
        while managed.stdout().read_from(0).unwrap().bytes != b"ready" {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("fixture process did not install its TERM handler");

    drop(process);
    assert!(
        !fiber.dispose().await.is_clean(),
        "the one-millisecond provider deadline must remain an honest cleanup failure"
    );
    let settled = tokio::time::timeout(std::time::Duration::from_secs(2), managed.wait()).await;
    if settled.is_err() {
        let _cleanup = std::process::Command::new("/bin/kill")
            .args(["-KILL", "--", &format!("-{pid}")])
            .status();
        let _ = tokio::time::timeout(std::time::Duration::from_secs(2), managed.wait()).await;
    }
    let outcome = settled
        .expect("provider timeout dropped ownership before TERM-to-KILL escalation completed")
        .unwrap();
    assert_eq!(outcome.signal, Some(9));
    assert!(!PathBuf::from(format!("/proc/{pid}")).exists());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[cfg(target_os = "linux")]
async fn provider_retirement_joins_every_spawn_racing_admission_publication() {
    const SPAWNS: usize = 128;
    let (fiber, process) = activated(json!({"maximum_active_processes":256})).await;
    let barrier = Arc::new(tokio::sync::Barrier::new(SPAWNS + 1));
    let mut spawns = Vec::with_capacity(SPAWNS);
    for _ in 0..SPAWNS {
        let process = Arc::clone(&process);
        let barrier = Arc::clone(&barrier);
        spawns.push(tokio::spawn(async move {
            barrier.wait().await;
            process.spawn(spec("/bin/sleep 30", 1))
        }));
    }
    drop(process);
    let disposal = tokio::spawn(async move {
        barrier.wait().await;
        fiber.dispose().await
    });

    let mut admitted = Vec::new();
    for spawn in spawns {
        match spawn.await.unwrap() {
            Ok(process) => admitted.push(process),
            Err(ProcessError::ShuttingDown) => {}
            Err(error) => panic!("unexpected racing spawn outcome: {error}"),
        }
    }
    assert!(disposal.await.unwrap().is_clean());
    for process in admitted {
        let pid = process.pid();
        process.wait().await.unwrap();
        assert!(!PathBuf::from(format!("/proc/{pid}")).exists());
    }
}
