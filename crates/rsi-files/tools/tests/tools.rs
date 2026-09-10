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
    let runtime = Runtime::default();
    runtime
        .root()
        .apply(linked("files", rsi_files::FilesFactory), Value::Null)
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
