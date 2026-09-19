use super::*;
use rsi_process::{ManagedPtyProcess, PtyProcessContract, PtyProcessSpec, PtySize};
use rsi_sandbox::{ProcessRequest, ProcessStdio, SandboxContract};
use rsi_sandbox_local::SandboxLocalFactory;
use std::time::{Duration, Instant};

async fn output_until(process: &ManagedPtyProcess, marker: &[u8]) -> Vec<u8> {
    tokio::time::timeout(Duration::from_secs(5), async {
        let mut bytes = Vec::new();
        loop {
            let chunk = process.read().await.unwrap();
            bytes.extend(chunk.bytes);
            assert!(bytes.len() <= 1024 * 1024);
            if bytes.windows(marker.len()).any(|part| part == marker) {
                return bytes;
            }
            assert!(
                !chunk.eof,
                "PTY ended before {:?}: {}",
                String::from_utf8_lossy(marker),
                String::from_utf8_lossy(&bytes)
            );
        }
    })
    .await
    .expect("PTY output deadline")
}
fn tagged_processes(tag: &str) -> Vec<PathBuf> {
    std::fs::read_dir("/proc")
        .unwrap()
        .flatten()
        .filter_map(|entry| {
            let command = std::fs::read(entry.path().join("cmdline")).ok()?;
            (command.split(|byte| *byte == 0).next() == Some(tag.as_bytes()))
                .then_some(entry.path())
        })
        .collect()
}
async fn write(process: &ManagedPtyProcess, bytes: &[u8]) {
    let mut offset = 0;
    while offset < bytes.len() {
        offset += process.write(&bytes[offset..]).await.unwrap();
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires native Linux user namespaces and an installed Bubblewrap; run explicitly"]
#[allow(clippy::too_many_lines)] // One native lifecycle proves controlling tty, both policies, groups and provider retirement.
async fn native_pty_has_job_control_confines_writes_and_reaps_on_provider_retirement() {
    for mode in [SandboxMode::ReadOnly, SandboxMode::WorkspaceWrite] {
        let temporary = tempfile::tempdir().unwrap();
        let workspace = temporary.path().join("workspace");
        std::fs::create_dir(&workspace).unwrap();
        let workspace = workspace.canonicalize().unwrap();
        let runtime = Runtime::default();
        let sandbox_fiber = runtime
            .root()
            .apply(
                ResolvedFactory::linked(
                    "sandbox",
                    "test",
                    UpdateMode::Replayable,
                    Arc::new(SandboxLocalFactory::default()),
                ),
                json!({"bubblewrap":["/usr/bin/bwrap"],"landlock":[]}),
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
                    Arc::new(ProcessLocalFactory),
                ),
                json!({}),
            )
            .await
            .unwrap();
        let sandbox = runtime.root().lookup_local::<SandboxContract>().unwrap();
        let plan = sandbox
            .confine(ProcessRequest {
                stdio: ProcessStdio::Pty,
                mode,
                program: "/bin/bash".into(),
                arguments: vec!["--noprofile".into(), "--norc".into(), "-i".into()],
                cwd: workspace.clone(),
                workspace: workspace.clone(),
            })
            .await
            .unwrap();
        assert!(
            !plan
                .arguments
                .iter()
                .any(|argument| argument == "--new-session")
        );
        assert!(
            plan.arguments
                .iter()
                .any(|argument| argument == "--unshare-all")
        );
        assert!(
            plan.arguments
                .iter()
                .any(|argument| argument == "--die-with-parent")
        );
        let pipes = runtime.root().lookup_local::<ProcessContract>().unwrap();
        let mut bad = spec("exit 0", 1024);
        bad.process = plan.clone();
        assert!(matches!(
            pipes.spawn(bad),
            Err(ProcessError::InvalidInput(_))
        ));
        let original = temporary.path().join("original");
        std::fs::rename(&workspace, &original).unwrap();
        std::fs::create_dir(&workspace).unwrap();
        let startup = temporary.path().join("startup");
        let escaped = temporary.path().join("OUTSIDE_STARTUP");
        std::fs::write(
            &startup,
            format!("printf unsafe > '{}'\n", escaped.display()),
        )
        .unwrap();
        let provider = runtime.root().lookup_local::<PtyProcessContract>().unwrap();
        let process = provider
            .spawn(PtyProcessSpec {
                process: plan,
                environment: vec![
                    ("BASH_ENV".into(), startup.into_os_string()),
                    ("PATH".into(), "/usr/bin:/bin".into()),
                    ("HOME".into(), workspace.as_os_str().to_owned()),
                    ("TERM".into(), "xterm-256color".into()),
                    ("PS1".into(), "probe> ".into()),
                    ("HISTFILE".into(), "/dev/null".into()),
                ],
                size: PtySize {
                    rows: 24,
                    columns: 80,
                },
                termination_grace_ms: 500,
            })
            .unwrap();
        assert!(
            process
                .resize(PtySize {
                    rows: 0,
                    columns: 80
                })
                .is_err()
        );
        write(
            &process,
            b"stty -echo; test -t 0 && test -t 1 && printf '\\nTTY_READY\\n'\n",
        )
        .await;
        output_until(&process, b"\r\nTTY_READY\r\n").await;
        assert!(
            !escaped.exists(),
            "launcher evaluated ambient Bash startup input"
        );
        process
            .resize(PtySize {
                rows: 40,
                columns: 120,
            })
            .unwrap();
        write(&process, b"stty size\n").await;
        output_until(&process, b"40 120").await;
        write(
            &process,
            b"(printf payload > PROBE_WRITE) 2>/dev/null && echo WRITE_OK || echo WRITE_DENIED\n",
        )
        .await;
        let expected = if mode == SandboxMode::WorkspaceWrite {
            b"WRITE_OK".as_slice()
        } else {
            b"WRITE_DENIED".as_slice()
        };
        output_until(&process, expected).await;
        assert_eq!(
            original.join("PROBE_WRITE").exists(),
            mode == SandboxMode::WorkspaceWrite
        );
        assert!(
            !workspace.join("PROBE_WRITE").exists(),
            "replacement path received a write"
        );
        write(
            &process,
            b"bash -c 'echo FOREGROUND_READY; exec sleep 60'\n",
        )
        .await;
        output_until(&process, b"FOREGROUND_READY\r\n").await;
        write(&process, &[26]).await;
        output_until(&process, b"Stopped").await;
        write(&process, b"bg\njobs\n").await;
        output_until(&process, b"Running").await;
        let tag = format!("rsi-pty-provider-{}", process.pid());
        write(
            &process,
            format!("bash -c 'exec -a {tag} sleep 60' &\n").as_bytes(),
        )
        .await;
        let deadline = Instant::now() + Duration::from_secs(5);
        while tagged_processes(&tag).is_empty() {
            assert!(Instant::now() < deadline);
            tokio::task::yield_now().await;
        }
        // A raw-mode foreground program stops consuming input. Filling its kernel
        // queue must not leave a deferred writer or consume input admission forever.
        write(
            &process,
            b"stty -icanon -echo; printf '\\nINPUT_BLOCKED\\n'; sleep 60\n",
        )
        .await;
        output_until(&process, b"\r\nINPUT_BLOCKED\r\n").await;
        let input = vec![b'x'; 65536];
        let filled = tokio::time::timeout(Duration::from_secs(3), async {
            for _ in 0..256 {
                match process.write(&input).await {
                    Ok(count) => assert!(count > 0),
                    Err(ProcessError::Capacity) => {
                        panic!("previous native writer still owns input")
                    }
                    Err(_) => return,
                }
            }
            panic!("input did not saturate within 16 MiB");
        })
        .await;
        if filled.is_err() {
            process.terminate();
        }
        assert!(
            filled.is_ok(),
            "native write remained blocked on a full PTY"
        );
        assert!(!matches!(
            process.write(b"x").await,
            Err(ProcessError::Capacity)
        ));
        process
            .resize(PtySize {
                rows: 24,
                columns: 80,
            })
            .unwrap();
        assert!(process_fiber.dispose().await.is_clean());
        tokio::time::timeout(Duration::from_secs(5), process.wait())
            .await
            .unwrap()
            .unwrap();
        assert!(
            tagged_processes(&tag).is_empty(),
            "namespace leaked a background process"
        );
        assert!(process.write(b"echo forbidden\n").await.is_err());
        drop((process, provider, pipes, sandbox));
        assert!(sandbox_fiber.dispose().await.is_clean());
        assert!(runtime.shutdown().await.is_clean());
    }
}
