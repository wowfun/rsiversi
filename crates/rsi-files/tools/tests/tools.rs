#![cfg(unix)]
use async_trait::async_trait;
use rsi_files_protocol::{FilesContract, MAXIMUM_FILE_TOKENS};
use rsi_files_tools::FilesToolsFactory;
use rsi_meta::{
    ActivationPlan, ConfigValue, PluginFactory, PreparedActivation, ResolvedFactory, Runtime,
    UpdateMode,
};
use rsi_sandbox::{
    Sandbox, SandboxGeneration, SandboxMode, WorkspaceReadRequest, WorkspaceReadScope,
};
use rsi_tools_protocol::{
    ToolCall, ToolCatalogProviderContract, ToolExecutionExtensions, ToolExecutionPolicy,
    ToolRegistrar, ToolRegistrarContract, ToolRuntime, ToolStart,
};
use serde_json::{Value, json};
use std::{
    path::Path,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
};
use tokio_util::sync::CancellationToken;

#[derive(Debug)]
struct Registrar(Arc<dyn ToolRegistrar>);
#[async_trait]
impl PluginFactory for Registrar {
    fn prepare(&self, _: &ConfigValue) -> rsi_meta::Result<PreparedActivation> {
        Ok(PreparedActivation::new(ConfigValue::Null))
    }
    async fn activate(&self, plan: ActivationPlan) -> rsi_meta::Result<()> {
        let supply = plan
            .context()
            .provide_local::<ToolRegistrarContract>(self.0.clone())?;
        plan.defer(
            "withdraw fixture registrar",
            Box::new(move || {
                Box::pin(async move {
                    drop(supply);
                    Ok(())
                })
            }),
        )
    }
}
#[derive(Debug, Default)]
struct Planner {
    generation: SandboxGeneration,
    scopes: Mutex<Vec<WorkspaceReadScope>>,
}
#[async_trait]
impl Sandbox for Planner {
    async fn workspace_read(
        &self,
        request: WorkspaceReadRequest,
    ) -> rsi_sandbox::Result<WorkspaceReadScope> {
        let scope = WorkspaceReadScope::new(request, self.generation.clone())?;
        self.scopes.lock().unwrap().push(scope.clone());
        Ok(scope)
    }
    async fn confine(
        &self,
        _: rsi_sandbox::ProcessRequest,
    ) -> rsi_sandbox::Result<rsi_sandbox::ConfinedProcess> {
        panic!("Files Tool must not confine a process")
    }
}
fn linked(name: &str, factory: impl PluginFactory) -> ResolvedFactory {
    ResolvedFactory::linked(name, "test", UpdateMode::Replayable, Arc::new(factory))
}
fn start(root: &Path, mode: SandboxMode, planner: Arc<Planner>) -> ToolStart {
    ToolStart {
        cancellation: CancellationToken::new(),
        policy: ToolExecutionPolicy {
            mode,
            cwd: root.join("cwd"),
            workspace: root.to_owned(),
        },
        sandbox: planner,
        job_scope: None,
        extensions: ToolExecutionExtensions::default(),
    }
}
async fn call(
    tools: &dyn ToolRuntime,
    name: &str,
    arguments: Value,
    start: ToolStart,
) -> rsi_tools_protocol::Result<rsi_tools_protocol::ToolResult> {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let id = NEXT.fetch_add(1, Ordering::Relaxed).to_string();
    tools
        .prepare(
            &id,
            ToolCall {
                id: id.clone(),
                name: name.into(),
                arguments,
            },
        )?
        .start(start)
        .await
}
async fn fixture() -> (Runtime, Arc<dyn ToolRuntime>) {
    fixture_with_inspection(false).await
}
async fn fixture_with_inspection(inspect: bool) -> (Runtime, Arc<dyn ToolRuntime>) {
    let runtime = Runtime::default();
    runtime
        .root()
        .apply(
            if inspect {
                linked("files", InspectFiles)
            } else {
                linked("files", rsi_files::FilesFactory)
            },
            Value::Null,
        )
        .await
        .unwrap();
    runtime
        .root()
        .apply(linked("tools", rsi_tools::ToolsFactory), Value::Null)
        .await
        .unwrap();
    let provider = runtime
        .root()
        .lookup_local::<ToolCatalogProviderContract>()
        .unwrap();
    let stage = provider.begin_stage().unwrap();
    runtime
        .root()
        .apply(
            linked("registrar", Registrar(stage.registrar())),
            Value::Null,
        )
        .await
        .unwrap();
    runtime
        .root()
        .apply(linked("files-tools", FilesToolsFactory), Value::Null)
        .await
        .unwrap();
    (runtime, stage.seal().unwrap())
}

#[tokio::test]
async fn actual_catalog_reads_all_modes_from_pinned_cwd_and_never_fabricates_process_enforcement() {
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path().canonicalize().unwrap();
    std::fs::create_dir(root.join("cwd")).unwrap();
    std::fs::write(root.join("cwd/file"), b"a\0\xff\x1bhello").unwrap();
    std::fs::write(root.join("file"), b"wrong cwd").unwrap();
    let (runtime, tools) = fixture().await;
    let planner = Arc::new(Planner::default());
    for mode in [
        SandboxMode::ReadOnly,
        SandboxMode::WorkspaceWrite,
        SandboxMode::DangerFullAccess,
    ] {
        let result = call(
            tools.as_ref(),
            "file_read",
            json!({"path":"file","offset":1,"maximum":4}),
            start(&root, mode, planner.clone()),
        )
        .await
        .unwrap();
        assert!(!result.is_error);
        assert_eq!(result.value["bytes_hex"], "00ff1b68");
        assert_eq!(result.value["next_offset"], 5);
        assert_eq!(result.value["text_changed"], true);
        assert!(result.enforcement.is_empty());
        let rsi_tools_protocol::ToolContent::Text { text } = &result.content[0] else {
            panic!("text")
        };
        assert!(text.contains("Exact bytes (hex): 00ff1b68"));
        assert!(!text.contains('\x1b'));
        let scopes = planner.scopes.lock().unwrap();
        let scope = scopes.last().unwrap();
        assert_eq!(scope.mode(), mode);
        assert_eq!(scope.cwd(), root.join("cwd"));
        assert_eq!(scope.workspace(), root);
        assert_eq!(scope.generation(), &planner.generation);
    }
    let replacement = Arc::new(Planner::default());
    assert_ne!(replacement.generation, planner.generation);
    call(
        tools.as_ref(),
        "file_read",
        json!({"path_hex":"66696c65","maximum":1}),
        start(&root, SandboxMode::ReadOnly, replacement.clone()),
    )
    .await
    .unwrap();
    assert_eq!(
        replacement.scopes.lock().unwrap()[0].generation(),
        &replacement.generation
    );
    for _ in 0..=MAXIMUM_FILE_TOKENS {
        assert!(
            !call(
                tools.as_ref(),
                "file_read",
                json!({"path":"file","maximum":1}),
                start(&root, SandboxMode::ReadOnly, planner.clone())
            )
            .await
            .unwrap()
            .is_error
        );
    }
    drop(tools);
    assert!(runtime.shutdown().await.is_clean());
}

#[tokio::test]
async fn unknown_roots_parent_paths_symlinks_and_oversized_pages_cannot_expand_read_scope() {
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path().canonicalize().unwrap();
    std::fs::create_dir(root.join("cwd")).unwrap();
    std::fs::write(root.join("secret"), "outside cwd").unwrap();
    std::os::unix::fs::symlink(root.join("secret"), root.join("cwd/link")).unwrap();
    let (runtime, tools) = fixture().await;
    let planner = Arc::new(Planner::default());
    for arguments in [
        json!({"path":"secret", "workspace":"/"}),
        json!({"path":"secret", "mode":"danger-full-access"}),
        json!({"path":"secret", "provider":"other"}),
        json!({"path":"secret", "path_hex":"736563726574"}),
        json!({"path":"secret", "maximum":0}),
        json!({"path":"secret", "maximum":65537}),
    ] {
        let result = call(
            tools.as_ref(),
            "file_read",
            arguments,
            start(&root, SandboxMode::ReadOnly, planner.clone()),
        )
        .await
        .unwrap();
        assert!(result.is_error);
        assert_eq!(result.value["error"], "invalid");
    }
    assert!(planner.scopes.lock().unwrap().is_empty());
    for arguments in [
        json!({"path":"../secret"}),
        json!({"path":"/secret"}),
        json!({"path_hex":"2e2e2f736563726574"}),
    ] {
        assert!(
            call(
                tools.as_ref(),
                "file_read",
                arguments,
                start(&root, SandboxMode::ReadOnly, planner.clone())
            )
            .await
            .unwrap()
            .is_error
        );
    }
    assert!(planner.scopes.lock().unwrap().is_empty());
    assert!(
        call(
            tools.as_ref(),
            "file_read",
            json!({"path":"link"}),
            start(&root, SandboxMode::DangerFullAccess, planner.clone())
        )
        .await
        .unwrap()
        .is_error
    );
    let cancelled = start(&root, SandboxMode::ReadOnly, planner.clone());
    cancelled.cancellation.cancel();
    assert!(
        call(
            tools.as_ref(),
            "file_read",
            json!({"path":"secret"}),
            cancelled
        )
        .await
        .is_err()
    );
    assert!(runtime.root().lookup_local::<FilesContract>().is_some());
    drop(tools);
    assert!(runtime.shutdown().await.is_clean());
}

#[tokio::test]
async fn directory_paths_are_cwd_relative_and_each_invocation_captures_a_fresh_snapshot() {
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path().canonicalize().unwrap();
    std::fs::create_dir_all(root.join("cwd/sub")).unwrap();
    std::fs::write(root.join("cwd/sub/a"), "a").unwrap();
    std::fs::write(root.join("cwd/sub/b"), "b").unwrap();
    let (runtime, tools) = fixture().await;
    let planner = Arc::new(Planner::default());
    let page = call(
        tools.as_ref(),
        "directory_list",
        json!({"path":"sub", "offset":1, "maximum":1}),
        start(&root, SandboxMode::ReadOnly, planner.clone()),
    )
    .await
    .unwrap();
    assert_eq!(page.value["entries"][0]["path"], "7375622f62");
    assert_eq!(page.value["entries"][0]["name"], "b");
    let read = call(
        tools.as_ref(),
        "file_read",
        json!({"path_hex":page.value["entries"][0]["path"]}),
        start(&root, SandboxMode::ReadOnly, planner.clone()),
    )
    .await
    .unwrap();
    assert_eq!(read.value["bytes_hex"], "62");
    std::fs::write(root.join("cwd/sub/c"), "c").unwrap();
    let next = call(
        tools.as_ref(),
        "directory_list",
        json!({"path":"sub", "offset":2}),
        start(&root, SandboxMode::ReadOnly, planner.clone()),
    )
    .await
    .unwrap();
    assert_eq!(next.value["total"], 3);
    assert_eq!(next.value["entries"][0]["name"], "c");
    assert!(
        !call(
            tools.as_ref(),
            "directory_list",
            json!({"path":"."}),
            start(&root, SandboxMode::ReadOnly, planner)
        )
        .await
        .unwrap()
        .is_error
    );
    drop(tools);
    assert!(runtime.shutdown().await.is_clean());
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn listed_non_utf8_filename_can_be_read_through_the_exact_byte_argument() {
    use std::os::unix::ffi::OsStrExt as _;
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path().canonicalize().unwrap();
    std::fs::create_dir(root.join("cwd")).unwrap();
    std::fs::write(
        root.join("cwd").join(std::ffi::OsStr::from_bytes(b"\xff")),
        "exact",
    )
    .unwrap();
    let (runtime, tools) = fixture().await;
    let planner = Arc::new(Planner::default());
    let page = call(
        tools.as_ref(),
        "directory_list",
        json!({}),
        start(&root, SandboxMode::ReadOnly, planner.clone()),
    )
    .await
    .unwrap();
    assert_eq!(page.value["entries"][0]["path"], "ff");
    let file = call(
        tools.as_ref(),
        "file_read",
        json!({"path_hex":page.value["entries"][0]["path"]}),
        start(&root, SandboxMode::ReadOnly, planner),
    )
    .await
    .unwrap();
    assert_eq!(file.value["bytes_hex"], hex::encode("exact"));
    drop(tools);
    assert!(runtime.shutdown().await.is_clean());
}

#[tokio::test]
async fn present_checks_regular_files_with_pinned_read_authority_and_releases_every_token() {
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path().canonicalize().unwrap();
    std::fs::create_dir(root.join("cwd")).unwrap();
    std::fs::write(root.join("cwd/report.txt"), "first").unwrap();
    std::fs::write(root.join("report.txt"), "wrong root").unwrap();
    std::os::unix::fs::symlink(root.join("report.txt"), root.join("cwd/link")).unwrap();
    let (runtime, tools) = fixture().await;
    let planner = Arc::new(Planner::default());
    for args in [
        json!({"files":[]}),
        json!({"files":vec![json!({"path":"report.txt"});9]}),
        json!({"files":[{"path":"../report.txt"}]}),
        json!({"files":[{"path":"/report.txt"}]}),
        json!({"files":[{"path":"report.txt","path_hex":"ff"}]}),
        json!({"files":[{"path":"report.txt","description":"界".repeat(86)}]}),
        json!({"files":[{"path":"report.txt","description":"\u{1b}unsafe"}]}),
        json!({"files":[{"path":"report.txt","description":"safe\u{202e}txt.exe"}]}),
        json!({"files":[{"path":"report.txt","description":"\u{2066}hidden\u{2069}"}]}),
        json!({"files":[{"path":"report.txt"}],"workspace":"/"}),
    ] {
        assert!(
            call(
                tools.as_ref(),
                "present",
                args,
                start(&root, SandboxMode::ReadOnly, planner.clone())
            )
            .await
            .unwrap()
            .is_error
        );
    }
    assert!(
        planner.scopes.lock().unwrap().is_empty(),
        "reject malformed declarations before Sandbox admission"
    );
    for path in ["link", "missing", "."] {
        assert!(
            call(
                tools.as_ref(),
                "present",
                json!({"files":[{"path":path}]}),
                start(&root, SandboxMode::DangerFullAccess, planner.clone())
            )
            .await
            .unwrap()
            .is_error
        );
    }
    let mut first = None;
    for iteration in 0..=MAXIMUM_FILE_TOKENS {
        let result = call(
            tools.as_ref(),
            "present",
            json!({"files":[{"path_hex":hex::encode("report.txt"),"description":"报告"}]}),
            start(&root, SandboxMode::ReadOnly, planner.clone()),
        )
        .await
        .unwrap();
        assert!(!result.is_error);
        let declared =
            rsi_files_tools::PresentedFilesV1::decode(&result.value["presented"]).unwrap();
        assert_eq!(declared.files[0].path_hex.as_bytes(), b"report.txt");
        assert_eq!(declared.files[0].length, if iteration == 0 { 5 } else { 7 });
        assert!(result.enforcement.is_empty());
        if iteration == 0 {
            first = Some(result);
            std::fs::write(root.join("cwd/report.txt"), "changed").unwrap();
        }
    }
    assert_eq!(
        first.unwrap().value["presented"]["files"][0]["length"],
        5,
        "recorded declaration remains unchanged"
    );
    let cancelled = start(&root, SandboxMode::ReadOnly, planner.clone());
    cancelled.cancellation.cancel();
    assert!(
        call(
            tools.as_ref(),
            "present",
            json!({"files":[{"path":"report.txt"}]}),
            cancelled
        )
        .await
        .is_err()
    );
    drop(tools);
    assert!(runtime.shutdown().await.is_clean());
}

// Enforce the one-live-token behavior at the actual contribution/provider seam.
#[derive(Debug)]
struct InspectFiles;
#[async_trait]
impl PluginFactory for InspectFiles {
    fn prepare(&self, _: &ConfigValue) -> rsi_meta::Result<PreparedActivation> {
        Ok(PreparedActivation::new(Value::Null))
    }
    async fn activate(&self, plan: ActivationPlan) -> rsi_meta::Result<()> {
        let inner = Arc::new(rsi_files::LocalFiles::new().unwrap());
        let cleanup = inner.clone();
        plan.defer(
            "close files",
            Box::new(move || {
                Box::pin(async move {
                    cleanup.close().await;
                    Ok(())
                })
            }),
        )?;
        let service = Arc::new(OneTokenFiles {
            inner,
            held: Mutex::new(None),
        });
        let supply = plan.context().provide_local::<FilesContract>(service)?;
        plan.defer(
            "release inspected files",
            Box::new(move || {
                Box::pin(async move {
                    drop(supply);
                    Ok(())
                })
            }),
        )
    }
}
#[derive(Debug)]
struct OneTokenFiles {
    inner: Arc<dyn rsi_files_protocol::Files>,
    held: Mutex<Option<rsi_files_protocol::FileToken>>,
}
#[async_trait]
impl rsi_files_protocol::Files for OneTokenFiles {
    fn release_caller(&self, caller: &rsi_files_protocol::FilesCaller) {
        self.inner.release_caller(caller);
        self.held.lock().unwrap().take();
    }
    fn describe(
        &self,
        binding: &rsi_files_protocol::FilesBinding,
        token: &rsi_files_protocol::FileToken,
    ) -> rsi_files_protocol::Result<rsi_files_protocol::OpenedFile> {
        self.inner.describe(binding, token)
    }
    async fn open(
        &self,
        binding: rsi_files_protocol::FilesBinding,
        path: rsi_files_protocol::RelativePath,
        kind: rsi_files_protocol::FileKind,
        cancellation: CancellationToken,
    ) -> rsi_files_protocol::Result<rsi_files_protocol::OpenedFile> {
        assert!(
            self.held.lock().unwrap().is_none(),
            "present must release the previous token before another open"
        );
        let opened = self.inner.open(binding, path, kind, cancellation).await?;
        *self.held.lock().unwrap() = Some(opened.token.clone());
        Ok(opened)
    }
    async fn read(
        &self,
        _: rsi_files_protocol::FilesBinding,
        _: rsi_files_protocol::FileToken,
        _: u64,
        _: usize,
        _: CancellationToken,
    ) -> rsi_files_protocol::Result<rsi_files_protocol::FilePage> {
        panic!("present must not read contents")
    }
    async fn list(
        &self,
        _: rsi_files_protocol::FilesBinding,
        _: rsi_files_protocol::FileToken,
        _: usize,
        _: usize,
        _: CancellationToken,
    ) -> rsi_files_protocol::Result<rsi_files_protocol::DirectoryPage> {
        panic!("present must not enumerate")
    }
    fn release(
        &self,
        binding: &rsi_files_protocol::FilesBinding,
        token: &rsi_files_protocol::FileToken,
    ) -> rsi_files_protocol::Result<()> {
        self.inner.release(binding, token)?;
        assert_eq!(self.held.lock().unwrap().take().as_ref(), Some(token));
        Ok(())
    }
}
#[tokio::test]
async fn present_releases_each_metadata_token_before_opening_the_next_file() {
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path().canonicalize().unwrap();
    std::fs::create_dir(root.join("cwd")).unwrap();
    std::fs::write(root.join("cwd/file"), "body").unwrap();
    let (runtime, tools) = fixture_with_inspection(true).await;
    let result = call(
        tools.as_ref(),
        "present",
        json!({"files":vec![json!({"path":"file"});8]}),
        start(&root, SandboxMode::ReadOnly, Arc::new(Planner::default())),
    )
    .await
    .unwrap();
    assert!(!result.is_error);
    assert_eq!(
        result.value["presented"]["files"].as_array().unwrap().len(),
        8
    );
    drop(tools);
    assert!(runtime.shutdown().await.is_clean());
}
