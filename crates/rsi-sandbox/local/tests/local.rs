use async_trait::async_trait;
use rsi_meta::{ResolvedFactory, Runtime, UpdateMode};
#[cfg(unix)]
use rsi_sandbox::MAXIMUM_SANDBOX_WRAPPER_BYTES;
use rsi_sandbox::{
    ProcessRequest, SandboxBackend, SandboxContract, SandboxError, SandboxFileSystem, SandboxMode,
    SandboxNetwork, SandboxScratch,
};
use rsi_sandbox_local::{SandboxLocalFactory, SandboxProbe};
use serde_json::json;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

#[derive(Debug)]
struct Probe {
    replace_during_probe: Option<PathBuf>,
    calls: Mutex<Vec<(PathBuf, Vec<String>)>>,
}

#[derive(Debug)]
struct FailingThenAvailableProbe {
    calls: Mutex<Vec<Vec<u8>>>,
}

#[derive(Debug)]
struct SlowUnavailableProbe {
    calls: AtomicUsize,
    completed: AtomicUsize,
}

#[derive(Debug)]
struct NeverCompletingProbe {
    calls: AtomicUsize,
}

#[async_trait]
impl SandboxProbe for NeverCompletingProbe {
    async fn available(&self, _path: &Path, _arguments: &[&str]) -> rsi_sandbox::Result<bool> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        std::future::pending().await
    }
}

#[async_trait]
impl SandboxProbe for SlowUnavailableProbe {
    async fn available(&self, _path: &Path, _arguments: &[&str]) -> rsi_sandbox::Result<bool> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        tokio::time::sleep(std::time::Duration::from_millis(1_500)).await;
        self.completed.fetch_add(1, Ordering::SeqCst);
        Ok(false)
    }
}

#[async_trait]
impl SandboxProbe for FailingThenAvailableProbe {
    async fn available(&self, path: &Path, _arguments: &[&str]) -> rsi_sandbox::Result<bool> {
        let bytes = std::fs::read(path).unwrap();
        self.calls.lock().unwrap().push(bytes.clone());
        if bytes == b"fail" {
            return Err(SandboxError::Probe("candidate timed out".into()));
        }
        Ok(bytes == b"probe")
    }
}

#[async_trait]
impl SandboxProbe for Probe {
    async fn available(&self, path: &Path, arguments: &[&str]) -> rsi_sandbox::Result<bool> {
        self.calls.lock().unwrap().push((
            path.to_owned(),
            arguments
                .iter()
                .map(|argument| (*argument).to_owned())
                .collect(),
        ));
        let available = std::fs::read(path).is_ok_and(|bytes| bytes == b"probe");
        if available && let Some(source) = &self.replace_during_probe {
            std::fs::write(source, b"replacement").unwrap();
        }
        Ok(available)
    }
}

#[tokio::test]
async fn bubblewrap_precedes_landlock_and_stamp_matches_selected_wrapper() {
    let temporary = tempfile::tempdir().unwrap();
    let bwrap = temporary.path().join("bwrap");
    let landlock = temporary.path().join("landlock-run");
    std::fs::write(&bwrap, b"probe").unwrap();
    std::fs::write(&landlock, b"probe").unwrap();
    let probe = Arc::new(Probe {
        replace_during_probe: Some(bwrap.clone()),
        calls: Mutex::new(vec![]),
    });
    let runtime = Runtime::default();
    let fiber = runtime
        .root()
        .apply(
            ResolvedFactory::linked(
                "rsi.sandbox.local",
                "test",
                UpdateMode::Replayable,
                Arc::new(SandboxLocalFactory::with_probe(probe.clone())),
            ),
            json!({"bubblewrap":[bwrap],"landlock":[landlock]}),
        )
        .await
        .unwrap();
    assert_eq!(probe.calls.lock().unwrap().len(), 1);
    assert_ne!(probe.calls.lock().unwrap()[0].0, bwrap);
    let sandbox = runtime.root().lookup_local::<SandboxContract>().unwrap();
    let plan = sandbox
        .confine(ProcessRequest {
            mode: SandboxMode::WorkspaceWrite,
            program: std::fs::canonicalize("/bin/sh").unwrap(),
            arguments: vec!["-c".into(), "true".into()],
            cwd: temporary.path().to_owned(),
            workspace: temporary.path().to_owned(),
        })
        .await
        .unwrap();
    assert!(matches!(
        plan.stamp.backend,
        SandboxBackend::Bubblewrap { ref sha256 }
            if sha256.len() == 64 && sha256.bytes().all(|byte| byte.is_ascii_hexdigit())
    ));
    assert_eq!(plan.stamp.filesystem, SandboxFileSystem::WorkspaceWrite);
    assert_eq!(plan.stamp.scratch, SandboxScratch::PrivateTmp);
    assert_eq!(plan.stamp.network, SandboxNetwork::Host);
    assert!(plan.arguments.windows(3).any(|args| args[0] == "--bind"));
    assert_eq!(std::fs::read(&plan.program).unwrap(), b"probe");

    drop(sandbox);
    assert!(fiber.dispose().await.is_clean());
}

#[tokio::test]
async fn bubblewrap_private_mounts_precede_descendant_bind_and_unsafe_roots_are_rejected() {
    let temporary = tempfile::tempdir().unwrap();
    let bwrap = temporary.path().join("bwrap");
    std::fs::write(&bwrap, b"probe").unwrap();
    let probe = Arc::new(Probe {
        replace_during_probe: None,
        calls: Mutex::new(vec![]),
    });
    let runtime = Runtime::default();
    let fiber = runtime
        .root()
        .apply(
            ResolvedFactory::linked(
                "rsi.sandbox.local",
                "private-tmp-test",
                UpdateMode::Replayable,
                Arc::new(SandboxLocalFactory::with_probe(probe)),
            ),
            json!({"bubblewrap":[bwrap],"landlock":[]}),
        )
        .await
        .unwrap();
    let sandbox = runtime.root().lookup_local::<SandboxContract>().unwrap();
    let workspace_root = tempfile::tempdir_in("/tmp").unwrap();
    let workspace = std::fs::canonicalize(workspace_root.path()).unwrap();
    let shell = std::fs::canonicalize("/bin/sh").unwrap();
    let plan = sandbox
        .confine(ProcessRequest {
            mode: SandboxMode::WorkspaceWrite,
            program: shell.clone(),
            arguments: vec![],
            cwd: workspace.clone(),
            workspace: workspace.clone(),
        })
        .await
        .unwrap();
    let tmpfs = plan
        .arguments
        .windows(2)
        .position(|pair| pair[0] == "--tmpfs" && pair[1] == "/tmp")
        .unwrap();
    let bind = plan
        .arguments
        .windows(3)
        .position(|triple| triple[0] == "--bind" && triple[1] == workspace)
        .unwrap();
    assert!(tmpfs < bind);
    let read_only = sandbox
        .confine(ProcessRequest {
            mode: SandboxMode::ReadOnly,
            program: shell.clone(),
            arguments: vec![],
            cwd: workspace.clone(),
            workspace: workspace.clone(),
        })
        .await
        .unwrap();
    let read_only_tmpfs = read_only
        .arguments
        .windows(2)
        .position(|pair| pair[0] == "--tmpfs" && pair[1] == "/tmp")
        .unwrap();
    let read_only_bind = read_only
        .arguments
        .windows(3)
        .position(|triple| triple[0] == "--ro-bind" && triple[1] == workspace)
        .expect("read-only workspace below /tmp must be rebound after the private tmpfs");
    assert!(read_only_tmpfs < read_only_bind);
    for mode in [SandboxMode::ReadOnly, SandboxMode::WorkspaceWrite] {
        assert!(matches!(
            sandbox
                .confine(ProcessRequest {
                    mode,
                    program: shell.clone(),
                    arguments: vec![],
                    cwd: PathBuf::from("/tmp"),
                    workspace: PathBuf::from("/tmp"),
                })
                .await,
            Err(SandboxError::InvalidInput(message)) if message.contains("system temporary root")
        ));
        assert!(matches!(
            sandbox
                .confine(ProcessRequest {
                    mode,
                    program: shell.clone(),
                    arguments: vec![],
                    cwd: PathBuf::from("/"),
                    workspace: PathBuf::from("/"),
                })
                .await,
            Err(SandboxError::InvalidInput(message)) if message.contains("filesystem root")
        ));
    }

    drop(sandbox);
    assert!(fiber.dispose().await.is_clean());
}

#[tokio::test]
async fn landlock_rejects_the_filesystem_root_as_a_restricted_workspace() {
    let temporary = tempfile::tempdir().unwrap();
    let landlock = temporary.path().join("landlock-run");
    std::fs::write(&landlock, b"probe").unwrap();
    let runtime = Runtime::default();
    let fiber = runtime
        .root()
        .apply(
            ResolvedFactory::linked(
                "rsi.sandbox.local",
                "landlock-root-test",
                UpdateMode::Replayable,
                Arc::new(SandboxLocalFactory::with_probe(Arc::new(Probe {
                    replace_during_probe: None,
                    calls: Mutex::new(vec![]),
                }))),
            ),
            json!({"bubblewrap":[],"landlock":[landlock]}),
        )
        .await
        .unwrap();
    let sandbox = runtime.root().lookup_local::<SandboxContract>().unwrap();

    assert!(matches!(
        sandbox
            .confine(ProcessRequest {
                mode: SandboxMode::WorkspaceWrite,
                program: std::fs::canonicalize("/bin/sh").unwrap(),
                arguments: vec![],
                cwd: PathBuf::from("/"),
                workspace: PathBuf::from("/"),
            })
            .await,
        Err(SandboxError::InvalidInput(message)) if message.contains("filesystem root")
    ));

    drop(sandbox);
    assert!(fiber.dispose().await.is_clean());
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn restricted_plan_preserves_non_utf8_workspace_bytes() {
    use std::ffi::OsString;
    use std::os::unix::ffi::{OsStrExt as _, OsStringExt as _};

    let temporary = tempfile::tempdir().unwrap();
    let bwrap = temporary.path().join("bwrap");
    std::fs::write(&bwrap, b"probe").unwrap();
    let workspace = temporary
        .path()
        .join(OsString::from_vec(b"workspace-\xff".to_vec()));
    std::fs::create_dir(&workspace).unwrap();
    let probe = Arc::new(Probe {
        replace_during_probe: None,
        calls: Mutex::new(vec![]),
    });
    let runtime = Runtime::default();
    let fiber = runtime
        .root()
        .apply(
            ResolvedFactory::linked(
                "rsi.sandbox.local",
                "test",
                UpdateMode::Replayable,
                Arc::new(SandboxLocalFactory::with_probe(probe)),
            ),
            json!({"bubblewrap":[bwrap],"landlock":[]}),
        )
        .await
        .unwrap();
    let sandbox = runtime.root().lookup_local::<SandboxContract>().unwrap();
    let plan = sandbox
        .confine(ProcessRequest {
            mode: SandboxMode::WorkspaceWrite,
            program: std::fs::canonicalize("/bin/sh").unwrap(),
            arguments: vec![],
            cwd: workspace.clone(),
            workspace: workspace.clone(),
        })
        .await
        .unwrap();
    assert!(
        plan.arguments
            .iter()
            .any(|argument| argument.as_os_str().as_bytes() == workspace.as_os_str().as_bytes())
    );

    drop(sandbox);
    assert!(fiber.dispose().await.is_clean());
}

#[tokio::test]
async fn failed_probe_falls_through_to_the_next_explicit_candidate() {
    let temporary = tempfile::tempdir().unwrap();
    let first = temporary.path().join("first-bwrap");
    let second = temporary.path().join("second-bwrap");
    std::fs::write(&first, b"fail").unwrap();
    std::fs::write(&second, b"probe").unwrap();
    let probe = Arc::new(FailingThenAvailableProbe {
        calls: Mutex::new(vec![]),
    });
    let runtime = Runtime::default();
    let fiber = runtime
        .root()
        .apply(
            ResolvedFactory::linked(
                "rsi.sandbox.local",
                "test",
                UpdateMode::Replayable,
                Arc::new(SandboxLocalFactory::with_probe(probe.clone())),
            ),
            json!({"bubblewrap":[first,second],"landlock":[]}),
        )
        .await
        .unwrap();
    let sandbox = runtime
        .root()
        .lookup_local::<SandboxContract>()
        .expect("later candidate should still activate the sandbox service");
    let plan = sandbox
        .confine(ProcessRequest {
            mode: SandboxMode::ReadOnly,
            program: std::fs::canonicalize("/bin/sh").unwrap(),
            arguments: vec![],
            cwd: temporary.path().to_owned(),
            workspace: temporary.path().to_owned(),
        })
        .await
        .unwrap();
    assert!(matches!(
        plan.stamp.backend,
        SandboxBackend::Bubblewrap { sha256 } if sha256.len() == 64
    ));
    assert_eq!(
        probe.calls.lock().unwrap().as_slice(),
        [b"fail".to_vec(), b"probe".to_vec()]
    );

    drop(sandbox);
    assert!(fiber.dispose().await.is_clean());
}

#[cfg(unix)]
#[tokio::test]
async fn symlink_fifo_and_oversized_candidates_are_rejected_before_copy_or_probe() {
    use std::os::unix::fs::symlink;

    let temporary = tempfile::tempdir().unwrap();
    let victim = temporary.path().join("victim");
    let symlink_candidate = temporary.path().join("symlink-bwrap");
    std::fs::write(&victim, b"probe").unwrap();
    symlink(&victim, &symlink_candidate).unwrap();

    let fifo = temporary.path().join("fifo-bwrap");
    let status = std::process::Command::new("mkfifo")
        .arg(&fifo)
        .status()
        .unwrap();
    assert!(status.success());

    let oversized = temporary.path().join("oversized-bwrap");
    let oversized_file = std::fs::File::create(&oversized).unwrap();
    oversized_file
        .set_len(u64::try_from(MAXIMUM_SANDBOX_WRAPPER_BYTES).unwrap() + 1)
        .unwrap();

    let valid = temporary.path().join("valid-bwrap");
    std::fs::write(&valid, b"probe").unwrap();
    let probe = Arc::new(Probe {
        replace_during_probe: None,
        calls: Mutex::new(vec![]),
    });
    let runtime = Runtime::default();
    let fiber = tokio::time::timeout(
        std::time::Duration::from_secs(2),
        runtime.root().apply(
            ResolvedFactory::linked(
                "rsi.sandbox.local",
                "candidate-boundary-test",
                UpdateMode::Replayable,
                Arc::new(SandboxLocalFactory::with_probe(probe.clone())),
            ),
            json!({
                "bubblewrap":[symlink_candidate, fifo, oversized, valid],
                "landlock":[]
            }),
        ),
    )
    .await
    .expect("a FIFO candidate must not block activation")
    .unwrap();

    {
        let calls = probe.calls.lock().unwrap();
        assert_eq!(
            calls.len(),
            1,
            "only the valid regular file reaches probing"
        );
        assert_eq!(std::fs::read(&calls[0].0).unwrap(), b"probe");
    }
    assert_eq!(std::fs::read(&victim).unwrap(), b"probe");

    assert!(fiber.dispose().await.is_clean());
}

#[tokio::test]
async fn missing_backend_fails_restricted_mode_but_explicit_bypass_is_truthful() {
    let temporary = tempfile::tempdir().unwrap();
    let probe = Arc::new(Probe {
        replace_during_probe: None,
        calls: Mutex::new(vec![]),
    });
    let runtime = Runtime::default();
    let fiber = runtime
        .root()
        .apply(
            ResolvedFactory::linked(
                "rsi.sandbox.local",
                "test",
                UpdateMode::Replayable,
                Arc::new(SandboxLocalFactory::with_probe(probe)),
            ),
            json!({"bubblewrap":[],"landlock":[]}),
        )
        .await
        .unwrap();
    let sandbox = runtime.root().lookup_local::<SandboxContract>().unwrap();
    let request = |mode| ProcessRequest {
        mode,
        program: std::fs::canonicalize("/bin/sh").unwrap(),
        arguments: vec![],
        cwd: temporary.path().to_owned(),
        workspace: temporary.path().to_owned(),
    };
    assert_eq!(
        sandbox.confine(request(SandboxMode::ReadOnly)).await,
        Err(SandboxError::Unsupported(SandboxMode::ReadOnly))
    );
    let bypass = sandbox
        .confine(request(SandboxMode::DangerFullAccess))
        .await
        .unwrap();
    assert_eq!(bypass.stamp.backend, SandboxBackend::Unconfined);

    drop(sandbox);
    assert!(fiber.dispose().await.is_clean());
}

#[tokio::test]
async fn required_backend_rejects_activation_before_publishing_sandbox() {
    let runtime = Runtime::default();
    let fiber = runtime
        .root()
        .apply(
            ResolvedFactory::linked(
                "rsi.sandbox.local",
                "required-test",
                UpdateMode::Replayable,
                Arc::new(SandboxLocalFactory::default().require_restricted_backend()),
            ),
            json!({"bubblewrap":[],"landlock":[]}),
        )
        .await
        .unwrap();

    assert!(matches!(
        fiber.snapshot().state,
        rsi_meta::FiberState::Failed(message)
            if message.contains("restricted sandbox backend is required")
    ));
    assert!(runtime.root().lookup_local::<SandboxContract>().is_none());
}

#[tokio::test]
async fn required_backend_publishes_after_a_valid_probe() {
    let temporary = tempfile::tempdir().unwrap();
    let bwrap = temporary.path().join("bwrap");
    std::fs::write(&bwrap, b"probe").unwrap();
    let runtime = Runtime::default();
    let fiber = runtime
        .root()
        .apply(
            ResolvedFactory::linked(
                "rsi.sandbox.local",
                "required-test",
                UpdateMode::Replayable,
                Arc::new(
                    SandboxLocalFactory::with_probe(Arc::new(Probe {
                        replace_during_probe: None,
                        calls: Mutex::new(vec![]),
                    }))
                    .require_restricted_backend(),
                ),
            ),
            json!({"bubblewrap":[bwrap],"landlock":[]}),
        )
        .await
        .unwrap();

    assert!(runtime.root().lookup_local::<SandboxContract>().is_some());
    assert!(fiber.dispose().await.is_clean());
}

#[tokio::test(start_paused = true)]
async fn all_behavior_probes_share_one_activation_budget() {
    let temporary = tempfile::tempdir().unwrap();
    let first = temporary.path().join("first");
    let second = temporary.path().join("second");
    std::fs::write(&first, b"first").unwrap();
    std::fs::write(&second, b"second").unwrap();
    let probe = Arc::new(SlowUnavailableProbe {
        calls: AtomicUsize::new(0),
        completed: AtomicUsize::new(0),
    });
    let runtime = Runtime::default();
    let fiber = runtime
        .root()
        .apply(
            ResolvedFactory::linked(
                "rsi.sandbox.local",
                "shared-probe-budget-test",
                UpdateMode::Replayable,
                Arc::new(SandboxLocalFactory::with_probe(probe.clone())),
            ),
            json!({"bubblewrap":[first, second],"landlock":[]}),
        )
        .await
        .unwrap();

    assert_eq!(probe.calls.load(Ordering::SeqCst), 2);
    assert_eq!(
        probe.completed.load(Ordering::SeqCst),
        1,
        "the second probe must be cancelled at the shared deadline"
    );
    assert!(fiber.dispose().await.is_clean());
}

#[tokio::test(start_paused = true)]
async fn required_activation_reports_shared_probe_budget_exhaustion() {
    let temporary = tempfile::tempdir().unwrap();
    let bubblewrap = temporary.path().join("bubblewrap");
    let landlock = temporary.path().join("landlock");
    std::fs::write(&bubblewrap, b"bubblewrap").unwrap();
    std::fs::write(&landlock, b"landlock").unwrap();
    let probe = Arc::new(NeverCompletingProbe {
        calls: AtomicUsize::new(0),
    });
    let runtime = Runtime::default();
    let fiber = runtime
        .root()
        .apply(
            ResolvedFactory::linked(
                "rsi.sandbox.local",
                "exhausted-probe-budget-test",
                UpdateMode::Replayable,
                Arc::new(
                    SandboxLocalFactory::with_probe(probe.clone()).require_restricted_backend(),
                ),
            ),
            json!({"bubblewrap":[bubblewrap],"landlock":[landlock]}),
        )
        .await
        .unwrap();

    assert_eq!(
        probe.calls.load(Ordering::SeqCst),
        1,
        "the later tier must remain unprobed after the shared budget expires"
    );
    assert!(matches!(
        fiber.snapshot().state,
        rsi_meta::FiberState::Failed(message)
            if message.contains(
                "shared behavior-probe budget was exhausted during candidate probing; 1 later configured candidate was skipped"
            )
    ));
    assert!(runtime.root().lookup_local::<SandboxContract>().is_none());
}

#[cfg(unix)]
#[tokio::test]
async fn version_only_executable_is_not_accepted_as_a_working_bubblewrap() {
    use std::os::unix::fs::PermissionsExt as _;

    let temporary = tempfile::tempdir().unwrap();
    let fake = temporary.path().join("fake-bwrap");
    std::fs::write(&fake, b"#!/bin/sh\ntest \"$1\" = --version\n").unwrap();
    std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o700)).unwrap();

    let runtime = Runtime::default();
    let fiber = runtime
        .root()
        .apply(
            ResolvedFactory::linked(
                "rsi.sandbox.local",
                "probe-test",
                UpdateMode::Replayable,
                Arc::new(SandboxLocalFactory::default()),
            ),
            json!({"bubblewrap":[fake],"landlock":[]}),
        )
        .await
        .unwrap();
    let sandbox = runtime.root().lookup_local::<SandboxContract>().unwrap();
    let request = ProcessRequest {
        mode: SandboxMode::ReadOnly,
        program: std::fs::canonicalize("/bin/sh").unwrap(),
        arguments: vec![],
        cwd: temporary.path().to_owned(),
        workspace: temporary.path().to_owned(),
    };
    assert_eq!(
        sandbox.confine(request).await,
        Err(SandboxError::Unsupported(SandboxMode::ReadOnly))
    );

    drop(sandbox);
    assert!(fiber.dispose().await.is_clean());
}

#[cfg(unix)]
#[tokio::test]
async fn zero_exit_executable_is_not_accepted_as_a_working_bubblewrap() {
    use std::os::unix::fs::PermissionsExt as _;

    let temporary = tempfile::tempdir().unwrap();
    let fake = temporary.path().join("fake-bwrap");
    std::fs::write(&fake, b"#!/bin/sh\nexit 0\n").unwrap();
    std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o700)).unwrap();

    let runtime = Runtime::default();
    let fiber = runtime
        .root()
        .apply(
            ResolvedFactory::linked(
                "rsi.sandbox.local",
                "probe-test",
                UpdateMode::Replayable,
                Arc::new(SandboxLocalFactory::default()),
            ),
            json!({"bubblewrap":[fake],"landlock":[]}),
        )
        .await
        .unwrap();
    let sandbox = runtime.root().lookup_local::<SandboxContract>().unwrap();
    let request = ProcessRequest {
        mode: SandboxMode::ReadOnly,
        program: std::fs::canonicalize("/bin/sh").unwrap(),
        arguments: vec![],
        cwd: temporary.path().to_owned(),
        workspace: temporary.path().to_owned(),
    };
    assert_eq!(
        sandbox.confine(request).await,
        Err(SandboxError::Unsupported(SandboxMode::ReadOnly))
    );

    drop(sandbox);
    assert!(fiber.dispose().await.is_clean());
}

#[cfg(target_os = "linux")]
#[tokio::test]
#[ignore = "requires native bubblewrap and user namespace support"]
async fn native_bubblewrap_enforces_read_only_and_workspace_write_plans() {
    let bubblewrap = PathBuf::from("/usr/bin/bwrap");
    assert!(bubblewrap.is_file(), "native bubblewrap is unavailable");
    let temporary = tempfile::tempdir().unwrap();
    let workspace = std::fs::canonicalize(temporary.path()).unwrap();
    let host_tmp_marker = PathBuf::from(format!(
        "/tmp/rsi-sandbox-host-marker-{}",
        std::process::id()
    ));
    std::fs::write(&host_tmp_marker, b"host").unwrap();
    let outside = tempfile::tempdir_in("/var/tmp").unwrap();
    let outside_marker = outside.path().join("host-only");
    std::fs::write(&outside_marker, b"unchanged").unwrap();
    let setsid = PathBuf::from("/usr/bin/setsid");
    assert!(setsid.is_file(), "native setsid is unavailable");
    let shell = std::fs::canonicalize("/bin/sh").unwrap();
    let runtime = Runtime::default();
    let fiber = runtime
        .root()
        .apply(
            ResolvedFactory::linked(
                "rsi.sandbox.local",
                "native-test",
                UpdateMode::Replayable,
                Arc::new(SandboxLocalFactory::default()),
            ),
            json!({"bubblewrap":[bubblewrap],"landlock":[]}),
        )
        .await
        .unwrap();
    let sandbox = runtime.root().lookup_local::<SandboxContract>().unwrap();
    let request = |mode, script: &str| ProcessRequest {
        mode,
        program: shell.clone(),
        arguments: vec!["-c".into(), script.into()],
        cwd: workspace.clone(),
        workspace: workspace.clone(),
    };

    let read_only = sandbox
        .confine(request(
            SandboxMode::ReadOnly,
            "/usr/bin/setsid /bin/sh -c 'printf blocked > denied'",
        ))
        .await
        .unwrap();
    let denied = tokio::process::Command::new(&read_only.program)
        .args(&read_only.arguments)
        .current_dir(&read_only.cwd)
        .output()
        .await
        .unwrap();
    assert!(!denied.status.success());
    assert!(!workspace.join("denied").exists());

    let writable = sandbox
        .confine(request(
            SandboxMode::WorkspaceWrite,
            &format!(
                r#"set -eu
                test ! -e '{}'
                test -r /proc/self/status
                test ! -e /proc/{}
                capabilities=missing
                while read -r field value rest; do
                    if [ "$field" = CapEff: ]; then capabilities=$value; fi
                done < /proc/self/status
                test "$capabilities" = 0000000000000000
                test -c /dev/null
                for device in /dev/*; do
                    case "$device" in
                        /dev/core|/dev/fd|/dev/full|/dev/null|/dev/ptmx|/dev/pts|/dev/random|/dev/shm|/dev/stderr|/dev/stdin|/dev/stdout|/dev/tty|/dev/urandom|/dev/zero) ;;
                        *) exit 41 ;;
                    esac
                done
                if printf changed > '{}'; then exit 42; fi
                printf allowed > allowed
                /usr/bin/setsid /bin/sh -c 'printf nested > nested'"#,
                host_tmp_marker.display(), std::process::id(), outside_marker.display()
            ),
        ))
        .await
        .unwrap();
    let allowed = tokio::process::Command::new(&writable.program)
        .args(&writable.arguments)
        .current_dir(&writable.cwd)
        .output()
        .await
        .unwrap();
    assert!(
        allowed.status.success(),
        "{}",
        String::from_utf8_lossy(&allowed.stderr)
    );
    assert_eq!(
        std::fs::read_to_string(workspace.join("allowed")).unwrap(),
        "allowed"
    );
    assert_eq!(
        std::fs::read_to_string(workspace.join("nested")).unwrap(),
        "nested"
    );

    assert_eq!(std::fs::read(&outside_marker).unwrap(), b"unchanged");
    std::fs::remove_file(host_tmp_marker).unwrap();
    drop(sandbox);
    assert!(fiber.dispose().await.is_clean());
}
