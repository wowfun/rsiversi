#![cfg(all(target_os = "linux", feature = "test-support"))]
use rsi_browser::{BrowserPolicy, NativeRuntime, RuntimeConfig};
use rsi_meta::{ResolvedFactory, Runtime, UpdateMode};
use serde_json::json;
use std::future::Future as _;
use std::{
    collections::BTreeSet,
    path::Path,
    path::PathBuf,
    process::{Command, Stdio},
    sync::Arc,
    time::Duration,
};
async fn runtime() -> (Runtime, NativeRuntime) {
    let owner = Runtime::default();
    owner
        .root()
        .apply(
            ResolvedFactory::linked(
                "process",
                "test",
                UpdateMode::Replayable,
                Arc::new(rsi_process_local::ProcessLocalFactory),
            ),
            json!({}),
        )
        .await
        .unwrap();
    owner
        .root()
        .apply(
            ResolvedFactory::linked(
                "sandbox",
                "test",
                UpdateMode::Replayable,
                Arc::new(rsi_sandbox_local::SandboxLocalFactory::default()),
            ),
            json!({"bubblewrap":["/usr/bin/bwrap"],"landlock":[]}),
        )
        .await
        .unwrap();
    let mut config = RuntimeConfig {
        node: std::env::var("RSI_TEST_BROWSER_NODE").unwrap().into(),
        chromium_directory: std::env::var("RSI_TEST_BROWSER_CHROMIUM").unwrap().into(),
        package_directory: PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("runtime"),
        systemd_run: "/usr/bin/systemd-run".into(),
        user_runtime_directory: std::env::var("RSI_TEST_BROWSER_USER_RUNTIME")
            .unwrap()
            .into(),
        artifact_digest: "0".repeat(64),
    };
    config.artifact_digest = config.digest().unwrap();
    let browser = NativeRuntime::new(
        config,
        owner
            .root()
            .lookup_local::<rsi_process::DuplexProcessContract>()
            .unwrap(),
        owner
            .root()
            .lookup_local::<rsi_sandbox::SandboxContract>()
            .unwrap(),
    )
    .unwrap();
    (owner, browser)
}
fn show(unit: &str) -> String {
    let output=Command::new("/usr/bin/systemctl").args(["--user","show",unit,"--property=ActiveState,MemoryMax,TasksMax,RuntimeMaxUSec,KillMode,ControlGroup,MainPID"]).output().unwrap();
    String::from_utf8(output.stdout).unwrap()
}

async fn wait_for_renderer_sandbox(
    status_path: &Path,
    deadline: tokio::time::Instant,
) -> std::result::Result<String, String> {
    // Command-line publication precedes Chromium's presandbox initialization.
    // Observe both kernel flags under the caller's shared absolute deadline.
    loop {
        let status = std::fs::read_to_string(status_path)
            .map_err(|error| format!("{}: {error}", status_path.display()))?;
        if tokio::time::Instant::now() >= deadline {
            return Err(format!(
                "renderer sandbox readiness deadline at {}: {status}",
                status_path.display()
            ));
        }
        if status.contains("Seccomp:\t2") && status.contains("NoNewPrivs:\t1") {
            return Ok(status);
        }
        tokio::time::sleep_until(
            deadline.min(tokio::time::Instant::now() + Duration::from_millis(10)),
        )
        .await;
    }
}

#[tokio::test(start_paused = true)]
async fn renderer_startup_waits_for_both_kernel_security_flags() {
    let temp = tempfile::tempdir().unwrap();
    let status = temp.path().join("status");
    std::fs::write(&status, "Seccomp:\t0\nNoNewPrivs:\t0\n").unwrap();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
    let mut readiness = Box::pin(wait_for_renderer_sandbox(&status, deadline));
    let mut context = std::task::Context::from_waker(std::task::Waker::noop());
    assert!(readiness.as_mut().poll(&mut context).is_pending());
    std::fs::write(&status, "Seccomp:\t2\nNoNewPrivs:\t0\n").unwrap();
    tokio::time::advance(Duration::from_millis(10)).await;
    assert!(readiness.as_mut().poll(&mut context).is_pending());
    std::fs::write(&status, "Seccomp:\t2\nNoNewPrivs:\t1\n").unwrap();
    tokio::time::advance(Duration::from_millis(10)).await;
    assert_eq!(readiness.await.unwrap(), "Seccomp:\t2\nNoNewPrivs:\t1\n");
}

#[tokio::test(start_paused = true)]
async fn renderer_sandbox_deadline_rejects_persistent_missing_filters() {
    let temp = tempfile::tempdir().unwrap();
    let status = temp.path().join("status");
    std::fs::write(&status, "Seccomp:\t0\nNoNewPrivs:\t1\n").unwrap();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
    let error = wait_for_renderer_sandbox(&status, deadline)
        .await
        .unwrap_err();
    assert!(
        error.contains("renderer sandbox readiness deadline"),
        "{error}"
    );
    assert!(error.contains("Seccomp:\t0"), "{error}");
    assert!(tokio::time::Instant::now() <= deadline);
}

#[tokio::test(start_paused = true)]
async fn renderer_security_flags_observed_after_deadline_do_not_pass() {
    let temp = tempfile::tempdir().unwrap();
    let status = temp.path().join("status");
    std::fs::write(&status, "Seccomp:\t2\nNoNewPrivs:\t1\n").unwrap();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
    tokio::time::advance(Duration::from_secs(2)).await;
    assert!(wait_for_renderer_sandbox(&status, deadline).await.is_err());
}

#[tokio::test(start_paused = true)]
async fn renderer_read_failures_are_not_treated_as_startup_or_reset_the_deadline() {
    let temp = tempfile::tempdir().unwrap();
    let missing = temp.path().join("missing");
    let start = tokio::time::Instant::now();
    assert!(
        wait_for_renderer_sandbox(&missing, start + Duration::from_secs(2))
            .await
            .is_err()
    );
    assert_eq!(tokio::time::Instant::now(), start);
    let status = temp.path().join("status");
    std::fs::write(&status, "Seccomp:\t0\nNoNewPrivs:\t0\n").unwrap();
    let deadline = start + Duration::from_secs(2);
    tokio::time::advance(Duration::from_millis(1900)).await;
    assert!(wait_for_renderer_sandbox(&status, deadline).await.is_err());
    assert_eq!(tokio::time::Instant::now(), deadline);
}
#[tokio::test]
#[ignore = "native helper for abrupt-owner-death acceptance"]
async fn native_death_child() {
    let Ok(marker) = std::env::var("RSI_TEST_DEATH_MARKER") else {
        return;
    };
    let (_owner, browser) = runtime().await;
    let identity = format!("death-{}", std::process::id());
    browser.prepare().await.unwrap();
    let scope = browser
        .open(
            BrowserPolicy {
                entry_url: "https://readiness.invalid/".into(),
                path_prefix: "/".into(),
                dependency_hosts: BTreeSet::new(),
            },
            &identity,
        )
        .await
        .unwrap();
    for role in ["browser", "client"] {
        let unit = format!("rsi-browser-{identity}-{role}.service");
        let evidence = show(&unit);
        assert!(evidence.contains("MemoryMax=1073741824"), "{evidence}");
        assert!(evidence.contains("TasksMax=256"), "{evidence}");
        assert!(evidence.contains("RuntimeMaxUSec=10min"), "{evidence}");
        assert!(evidence.contains("KillMode=control-group"), "{evidence}");
        let group = evidence
            .lines()
            .find_map(|line| line.strip_prefix("ControlGroup="))
            .unwrap();
        let cgroup = PathBuf::from("/sys/fs/cgroup").join(group.trim_start_matches('/'));
        assert_eq!(
            std::fs::read_to_string(cgroup.join("memory.max"))
                .unwrap()
                .trim(),
            "1073741824"
        );
        assert_eq!(
            std::fs::read_to_string(cgroup.join("pids.max"))
                .unwrap()
                .trim(),
            "256"
        );
        if role == "browser" {
            let group = evidence
                .lines()
                .find_map(|l| l.strip_prefix("ControlGroup="))
                .unwrap();
            let pids = std::fs::read_to_string(
                PathBuf::from("/sys/fs/cgroup")
                    .join(group.trim_start_matches('/'))
                    .join("cgroup.procs"),
            )
            .unwrap();
            let mut sandboxed_renderers = 0;
            let renderer_deadline = tokio::time::Instant::now() + Duration::from_secs(2);
            for pid in pids.lines() {
                let root = PathBuf::from("/proc").join(pid);
                let Ok(args) = std::fs::read(root.join("cmdline")) else {
                    continue;
                };
                if args
                    .windows(b"--type=renderer".len())
                    .any(|w| w == b"--type=renderer")
                {
                    let status = wait_for_renderer_sandbox(&root.join("status"), renderer_deadline)
                        .await
                        .unwrap();
                    assert!(
                        status.contains("Seccomp:\t2"),
                        "renderer has no seccomp filter"
                    );
                    assert!(
                        status.contains("NoNewPrivs:\t1"),
                        "renderer lacks no-new-privileges"
                    );
                    assert_ne!(
                        std::fs::read_link(root.join("ns/net")).unwrap(),
                        std::fs::read_link("/proc/self/ns/net").unwrap()
                    );
                    assert_ne!(
                        std::fs::read_link(root.join("ns/pid")).unwrap(),
                        std::fs::read_link("/proc/self/ns/pid").unwrap()
                    );
                    assert!(
                        !args
                            .windows(b"--no-sandbox".len())
                            .any(|w| w == b"--no-sandbox")
                    );
                    sandboxed_renderers += 1;
                }
            }
            assert!(sandboxed_renderers > 0, "no sandboxed renderer observed");
            println!(
                "Observed {sandboxed_renderers} isolated renderers with Seccomp=2 and NoNewPrivs=1"
            );
        }
        println!("{unit}\n{evidence}");
    }
    std::fs::write(marker, &identity).unwrap();
    std::future::pending::<()>().await;
    drop(scope);
}
#[tokio::test]
#[ignore = "explicit Linux SIGKILL and real ten-minute cgroup retirement proof"]
async fn native_abrupt_owner_death_reaps_the_entire_cgroup() {
    let temp = tempfile::tempdir().unwrap();
    let marker = temp.path().join("ready");
    let evidence = temp.path().join("evidence");
    let mut child = Command::new(std::env::current_exe().unwrap())
        .args(["--ignored", "--exact", "native_death_child", "--nocapture"])
        .env("RSI_TEST_DEATH_MARKER", &marker)
        .stdout(std::fs::File::create(&evidence).unwrap())
        .stderr(Stdio::inherit())
        .spawn()
        .unwrap();
    let started = std::time::Instant::now();
    while !marker.exists() {
        if let Some(status) = child.try_wait().unwrap() {
            panic!(
                "native child exited before readiness: {status}; {}",
                std::fs::read_to_string(&evidence).unwrap()
            );
        }
        assert!(started.elapsed() < Duration::from_mins(1));
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    let identity = std::fs::read_to_string(&marker).unwrap();
    println!("{}", std::fs::read_to_string(evidence).unwrap());
    let mut cgroups = vec![];
    for role in ["browser", "client"] {
        let value = show(&format!("rsi-browser-{identity}-{role}.service"));
        let path = value
            .lines()
            .find_map(|l| l.strip_prefix("ControlGroup="))
            .unwrap();
        assert!(!path.is_empty());
        cgroups.push(PathBuf::from("/sys/fs/cgroup").join(path.trim_start_matches('/')));
    }
    child.kill().unwrap();
    assert!(!child.wait().unwrap().success());
    println!("Host SIGKILL observed; awaiting actual cgroup retirement");
    while cgroups.iter().any(|p| p.exists()) {
        assert!(
            started.elapsed() < Duration::from_secs(650),
            "cgroup survived runtime maximum"
        );
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
    println!(
        "Both cgroups removed after {} seconds",
        started.elapsed().as_secs()
    );
}
