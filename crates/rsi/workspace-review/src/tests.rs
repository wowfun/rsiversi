#![cfg(target_os = "linux")]
use super::*;
use git::Git;
use rsi_meta::{ResolvedFactory, Runtime, UpdateMode};
use rsi_process::ProcessContract;
use rsi_sandbox::{SandboxContract, SandboxMode};
use rsi_sandbox_local::SandboxLocalFactory;
use rsi_workspace_review_api::OmissionKind;
use std::{collections::BTreeMap, path::Path, sync::Arc};
use tokio_util::{sync::CancellationToken, task::TaskTracker};

fn command(root: &Path, args: &[&str]) {
    let output = std::process::Command::new("/usr/bin/git")
        .current_dir(root)
        .args(["-c", "maintenance.auto=false", "-c", "gc.auto=0"])
        .args(args)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}
fn metadata(root: &Path) -> BTreeMap<String, Vec<u8>> {
    fn walk(root: &Path, path: &Path, out: &mut BTreeMap<String, Vec<u8>>) {
        for entry in std::fs::read_dir(path).unwrap() {
            let entry = entry.unwrap();
            let path = entry.path();
            if entry.file_type().unwrap().is_dir() {
                walk(root, &path, out);
            } else {
                out.insert(
                    path.strip_prefix(root)
                        .unwrap()
                        .to_string_lossy()
                        .into_owned(),
                    std::fs::read(path).unwrap(),
                );
            }
        }
    }
    let mut out = BTreeMap::new();
    walk(root, root, &mut out);
    out
}
#[derive(Debug)]
pub(super) struct TestSandbox;
#[async_trait::async_trait]
impl rsi_sandbox::Sandbox for TestSandbox {
    async fn workspace_read(
        &self,
        request: rsi_sandbox::WorkspaceReadRequest,
    ) -> rsi_sandbox::Result<rsi_sandbox::WorkspaceReadScope> {
        rsi_sandbox::WorkspaceReadScope::new(request, rsi_sandbox::SandboxGeneration::default())
    }
    async fn confine(
        &self,
        request: rsi_sandbox::ProcessRequest,
    ) -> rsi_sandbox::Result<rsi_sandbox::ConfinedProcess> {
        assert!(matches!(
            request.mode,
            SandboxMode::ReadOnly | SandboxMode::WorkspaceWrite
        ));
        assert_eq!(request.cwd, request.workspace);
        Ok(rsi_sandbox::ConfinedProcess {
            program: request.program,
            arguments: request.arguments.into_iter().map(Into::into).collect(),
            cwd: request.cwd,
            stdio: request.stdio,
            stamp: rsi_sandbox::EnforcementStamp {
                requested: request.mode,
                backend: rsi_sandbox::SandboxBackend::Unconfined,
                filesystem: rsi_sandbox::SandboxFileSystem::Unconfined,
                scratch: rsi_sandbox::SandboxScratch::Host,
                network: rsi_sandbox::SandboxNetwork::Host,
                workspace: request.workspace,
            },
            owner: None,
        })
    }
}
#[derive(Debug)]
struct CountedSandbox {
    inner: Arc<dyn rsi_sandbox::Sandbox>,
    calls: Arc<std::sync::atomic::AtomicUsize>,
}
#[async_trait::async_trait]
impl rsi_sandbox::Sandbox for CountedSandbox {
    async fn workspace_read(
        &self,
        request: rsi_sandbox::WorkspaceReadRequest,
    ) -> rsi_sandbox::Result<rsi_sandbox::WorkspaceReadScope> {
        self.inner.workspace_read(request).await
    }
    async fn confine(
        &self,
        request: rsi_sandbox::ProcessRequest,
    ) -> rsi_sandbox::Result<rsi_sandbox::ConfinedProcess> {
        self.calls
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        self.inner.confine(request).await
    }
}
#[expect(
    clippy::too_many_lines,
    reason = "causal actual Git acceptance scenario"
)]
async fn scenario(native: bool) {
    let temporary = tempfile::tempdir().unwrap();
    let workspace = temporary.path().join("workspace");
    std::fs::create_dir(&workspace).unwrap();
    command(&workspace, &["init", "--quiet"]);
    command(&workspace, &["config", "user.name", "Review Fixture"]);
    command(
        &workspace,
        &["config", "user.email", "review@example.invalid"],
    );
    for (name, text) in [
        ("tracked.txt", "committed\n"),
        ("rename.txt", "renamed unchanged\n"),
        ("deleted.txt", "delete me\n"),
        (".gitignore", "ignored*\n"),
    ] {
        std::fs::write(workspace.join(name), text).unwrap();
    }
    for index in 0..130 {
        std::fs::write(
            workspace.join(format!("batch-{index}.txt")),
            "unchanged batch fixture\n",
        )
        .unwrap();
    }
    command(&workspace, &["add", "."]);
    command(&workspace, &["commit", "-qm", "fixture"]);
    std::fs::write(workspace.join("tracked.txt"), "dirty-before\n").unwrap();
    std::fs::write(workspace.join("untracked.txt"), "existing untracked\n").unwrap();
    std::fs::write(workspace.join("ignored.bin"), [0; 128]).unwrap();
    let original = metadata(&workspace.join(".git"));
    let runtime = Runtime::default();
    let process = runtime
        .root()
        .apply(
            ResolvedFactory::linked(
                "process",
                "test",
                UpdateMode::Replayable,
                Arc::new(rsi_process_local::ProcessLocalFactory),
            ),
            serde_json::json!({}),
        )
        .await
        .unwrap();
    let mut sandbox_owner = None;
    let sandbox: Arc<dyn rsi_sandbox::Sandbox> = if native {
        sandbox_owner = Some(
            runtime
                .root()
                .apply(
                    ResolvedFactory::linked(
                        "sandbox",
                        "test",
                        UpdateMode::Replayable,
                        Arc::new(SandboxLocalFactory::default().require_restricted_backend()),
                    ),
                    serde_json::json!({"bubblewrap":["/usr/bin/bwrap"],"landlock":[]}),
                )
                .await
                .unwrap(),
        );
        runtime.root().lookup_local::<SandboxContract>().unwrap()
    } else {
        Arc::new(TestSandbox)
    };
    let files = Arc::new(rsi_files::LocalFiles::new().unwrap());
    let tasks = TaskTracker::new();
    let quota = Arc::new(tokio::sync::Semaphore::new(1024 * 1024 * 1024));
    let process_calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let sandbox = Arc::new(CountedSandbox {
        inner: sandbox,
        calls: process_calls.clone(),
    });
    let git = Git {
        process: runtime.root().lookup_local::<ProcessContract>().unwrap(),
        sandbox,
        files: files.clone(),
        program: "/usr/bin/git".into(),
        quota: quota.clone(),
        tasks: tasks.clone(),
    };
    let root = scratch_root::Root::open(temporary.path().join("scratch"))
        .await
        .unwrap();
    let stop = CancellationToken::new();
    let mut scratch = git.initialize(&root.path, &stop).await.unwrap();
    let held = quota
        .clone()
        .try_acquire_many_owned(u32::try_from(quota.available_permits()).unwrap())
        .unwrap();
    assert!(matches!(
        git.initialize(&root.path, &stop).await,
        Err(OmissionKind::Capacity)
    ));
    assert_eq!(
        std::fs::read_dir(&root.path).unwrap().count(),
        2,
        "quota rejection precedes scratch creation"
    );
    drop(held);
    let before_calls = process_calls.load(std::sync::atomic::Ordering::Relaxed);
    let before = git.capture(&workspace, &mut scratch, &stop).await;
    assert_eq!(
        process_calls.load(std::sync::atomic::Ordering::Relaxed) - before_calls,
        4,
        "one listing and three blob batches for 135 files"
    );
    assert!(before.omissions.is_empty(), "{:?}", before.omissions);
    std::fs::write(workspace.join("tracked.txt"), "after 中文🦀\n").unwrap();
    std::fs::rename(workspace.join("rename.txt"), workspace.join("renamed.txt")).unwrap();
    std::fs::remove_file(workspace.join("deleted.txt")).unwrap();
    std::fs::write(workspace.join(":(glob)literal.txt"), "independent writer\n").unwrap();
    let after = git.capture(&workspace, &mut scratch, &stop).await;
    assert!(after.omissions.is_empty(), "{:?}", after.omissions);
    let comparison = git.compare(&scratch, &before, &after, &stop).await.unwrap();
    assert_eq!(comparison.files.len(), 4, "{:?}", comparison.files);
    let file = comparison
        .files
        .iter()
        .find(|f| f.path == "tracked.txt")
        .unwrap();
    let diff = git.diff(&scratch, &comparison, file, &stop).await.unwrap();
    assert!(diff.contains("-dirty-before\n"));
    assert!(diff.contains("+after 中文🦀\n"));
    assert!(!diff.contains("committed"));
    assert!(
        comparison
            .files
            .iter()
            .any(|f| f.previous_path.as_deref() == Some("rename.txt") && f.path == "renamed.txt")
    );
    let file = comparison
        .files
        .iter()
        .find(|f| f.path == ":(glob)literal.txt")
        .unwrap();
    assert!(
        git.diff(&scratch, &comparison, file, &stop)
            .await
            .unwrap()
            .contains("independent writer")
    );
    assert_eq!(
        metadata(&workspace.join(".git")),
        original,
        "private captures must not change the user's index, objects, refs or config"
    );
    std::fs::write(workspace.join("tracked.txt"), [0, 1, 2]).unwrap();
    let binary = git.capture(&workspace, &mut scratch, &stop).await;
    assert!(
        binary
            .omissions
            .iter()
            .any(|r| r.kind == OmissionKind::Binary)
    );
    let partial = git.compare(&scratch, &after, &binary, &stop).await.unwrap();
    assert!(
        !partial.files.iter().any(|f| f.path == "tracked.txt"),
        "an unreadable/binary original cannot be represented as a deletion"
    );
    std::fs::write(workspace.join("large.txt"), vec![b'x'; 4 * 1024 * 1024 + 1]).unwrap();
    std::os::unix::fs::symlink("/etc/passwd", workspace.join("linked.txt")).unwrap();
    let limited = git.capture(&workspace, &mut scratch, &stop).await;
    assert!(
        limited
            .omissions
            .iter()
            .any(|r| r.kind == OmissionKind::Limit)
    );
    assert!(
        limited
            .omissions
            .iter()
            .any(|r| r.kind == OmissionKind::Unreadable)
    );
    let safe = git
        .compare(&scratch, &after, &limited, &stop)
        .await
        .unwrap();
    assert!(
        !safe
            .files
            .iter()
            .any(|f| matches!(f.path.as_str(), "large.txt" | "linked.txt" | "tracked.txt"))
    );
    let cancelled = CancellationToken::new();
    cancelled.cancel();
    assert!(
        git.capture(&workspace, &mut scratch, &cancelled)
            .await
            .omissions
            .iter()
            .any(|r| r.kind == OmissionKind::Deadline)
    );
    assert_eq!(metadata(&workspace.join(".git")), original);
    drop((comparison, partial, before, after, binary, scratch, git));
    tasks.close();
    tasks.wait().await;
    assert_eq!(quota.available_permits(), 1024 * 1024 * 1024);
    assert_eq!(
        std::fs::read_dir(&root.path).unwrap().count(),
        1,
        "scratch deletion precedes quota release"
    );
    files.close().await;
    assert!(process.dispose().await.is_clean());
    if let Some(owner) = sandbox_owner {
        assert!(owner.dispose().await.is_clean());
    }
    assert!(runtime.shutdown().await.is_clean());
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn private_git_comparison_preserves_user_metadata_dirty_baseline_and_omission_boundaries() {
    scenario(false).await;
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires native Linux Bubblewrap and Git; run explicitly"]
async fn native_sandbox_private_git_comparison() {
    scenario(true).await;
}

#[tokio::test]
async fn scratch_root_leases_recovery_and_unrelated_entries_are_preserved() {
    let temporary = tempfile::tempdir().unwrap();
    let path = temporary.path().join("scratch");
    let first = scratch_root::Root::open(path.clone()).await.unwrap();
    assert!(scratch_root::Root::open(path.clone()).await.is_err());
    let old = scratch_root::interval_directory(&path).unwrap().keep();
    std::fs::write(old.join("pending"), b"private scratch").unwrap();
    drop(first);
    let second = scratch_root::Root::open(path.clone()).await.unwrap();
    assert!(!old.exists());
    std::fs::write(path.join("unrelated"), b"preserve").unwrap();
    drop(second);
    assert!(scratch_root::Root::open(path.clone()).await.is_err());
    assert_eq!(std::fs::read(path.join("unrelated")).unwrap(), b"preserve");
    std::fs::remove_file(path.join("unrelated")).unwrap();
    std::os::unix::fs::symlink(temporary.path(), path.join("interval-link")).unwrap();
    assert!(scratch_root::Root::open(path.clone()).await.is_err());
    assert!(temporary.path().exists());
}
