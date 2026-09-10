#![cfg(unix)]
use rsi_application::ScopedProfile;
use rsi_host::{Host, HostBuilder, Profile, ProfileEntry, ProfileProgram};
use rsi_meta::{Runtime, UpdateMode};
use rsi_meta_native_loader::{CatalogOptions, NativeCatalog};
use rsi_terminal::presentation::{
    FrameRendererContract, LinkedPresentationFactory, PortablePresentationFactory,
};
use rsi_terminal_ui::{
    Input,
    editor::Editor,
    scene::Scene,
    transcript::Transcript,
    wire::{Identity, Request},
};
use std::{
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};
use tokio_util::sync::CancellationToken;
fn artifact(root: &Path, revision: &str) -> PathBuf {
    let target = root.join("target/terminal-native-test").join(revision);
    let mut command = std::process::Command::new(env!("CARGO"));
    command
        .args(["build", "--locked", "--manifest-path"])
        .arg(root.join("crates/rsi/terminal-native/Cargo.toml"))
        .arg("--target-dir")
        .arg(&target);
    if revision == "b" {
        command.args(["--features", "revision-b"]);
    }
    let output = command.output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    target.join("debug").join(format!(
        "{}rsi_terminal_native{}",
        std::env::consts::DLL_PREFIX,
        std::env::consts::DLL_SUFFIX
    ))
}
fn host(loader: &NativeCatalog, path: &Path) -> Host {
    let mut builder = HostBuilder::without_paths(std::env::consts::OS);
    builder
        .register_local_contract::<FrameRendererContract>()
        .unwrap();
    builder
        .register_factory(loader.load(path).unwrap())
        .unwrap();
    builder
        .register_linked(
            "rsi.terminal.portable",
            "1",
            UpdateMode::Replayable,
            Arc::new(PortablePresentationFactory),
        )
        .unwrap();
    builder.build().unwrap()
}
fn program() -> ProfileProgram {
    ProfileProgram::from_profile(Profile::new([
        ProfileEntry::new("native", "rsi.terminal.native", serde_json::Value::Null),
        ProfileEntry::new("adapter", "rsi.terminal.portable", serde_json::Value::Null),
    ]))
}
fn scene() -> Vec<u8> {
    use rsi_agent_session_protocol::{
        AgentPresetId, FrozenAgentSettings, SessionFact, SessionFactBody, SessionHeader, SessionId,
        TurnId,
    };
    let header = SessionHeader::new(
        SessionId::new("native-scene").unwrap(),
        1,
        "/workspace",
        AgentPresetId::new("default").unwrap(),
        FrozenAgentSettings::new(
            "default",
            "system",
            rsi_ai_protocol::ModelRef::new("fixture", "text").unwrap(),
            rsi_sandbox::SandboxMode::WorkspaceWrite,
            false,
        )
        .unwrap(),
    )
    .unwrap();
    let mut transcript = Transcript::default();
    transcript.apply(
        &SessionFact::new(
            1,
            1,
            SessionFactBody::TurnAccepted {
                turn_id: TurnId::new("turn").unwrap(),
                text: "Unicode 中 e\u{301} 👩🏽‍💻".repeat(8),
                model: None,
                sandbox: rsi_sandbox::SandboxMode::WorkspaceWrite,
                require_approval: false,
            },
        )
        .unwrap(),
    );
    let editor = Editor::with_text("Unsubmitted draft stays resident".into(), 1024);
    Scene::capture(
        &Input {
            header: &header,
            transcript: &transcript,
            editor: &editor,
            model: None,
            enter_submit: true,
            menu: None,
            answer: None,
            ui_edit: None,
            detail: None,
            detail_offset: 0,
            selection: None,
            top: None,
            status: "Ready",
            actual_model: None,
            active: false,
            busy: false,
            remote: false,
            questions: 0,
            approvals: 0,
        },
        24,
    )
    .unwrap()
    .encode()
    .unwrap()
}
fn request(presentation: u64, bytes: usize) -> Request {
    Request {
        identity: Identity {
            attachment: 0,
            presentation,
            revision: presentation,
        },
        width: 80,
        height: 24,
        bytes,
    }
}
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn real_native_render_replacement_changes_cells_fences_old_service_and_releases_libraries() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../..")
        .canonicalize()
        .unwrap();
    let a = artifact(&root, "a");
    let b = artifact(&root, "b");
    let temp = tempfile::tempdir().unwrap();
    let loader = NativeCatalog::new(CatalogOptions::new(temp.path())).unwrap();
    let runtime = Runtime::default();
    let original = host(&loader, &a);
    let profile = ScopedProfile::start(&original, &runtime.root(), program())
        .await
        .unwrap();
    drop(original);
    let old = profile.lookup_local::<FrameRendererContract>().unwrap();
    let source = scene();
    let (cells, map) = old
        .render(
            request(1, source.len()),
            source.clone(),
            CancellationToken::new(),
        )
        .await
        .unwrap();
    assert_ne!(cells[(0, 0)].symbol(), "B");
    assert!(!map.hits.is_empty());
    assert_linked_parity(&runtime, &old, &source, &cells).await;
    let replacement = host(&loader, &b);
    let updater = profile.updater();
    let ticket = updater
        .submit(
            updater.input_revision(),
            replacement.profile_input(program()).unwrap(),
        )
        .unwrap();
    drop(replacement);
    assert!(matches!(
        tokio::time::timeout(Duration::from_secs(10), ticket.wait())
            .await
            .unwrap()
            .unwrap(),
        rsi_host::ReloadOutcome::Applied(_)
    ));
    assert!(
        old.render(
            request(2, source.len()),
            source.clone(),
            CancellationToken::new()
        )
        .await
        .is_err()
    );
    drop(old);
    let renderer = profile.lookup_local::<FrameRendererContract>().unwrap();
    let (next, _) = renderer
        .render(
            request(2, source.len()),
            source.clone(),
            CancellationToken::new(),
        )
        .await
        .unwrap();
    assert_eq!(next[(0, 0)].symbol(), "B");
    assert!(
        next.content
            .iter()
            .map(ratatui::buffer::Cell::symbol)
            .collect::<String>()
            .contains("Unsubmitted draft stays resident")
    );
    drop(renderer);
    assert!(profile.shutdown().await.is_clean());
    drop(profile);
    drop(updater);
    assert!(runtime.shutdown().await.is_clean());
    tokio::time::timeout(Duration::from_secs(10), async {
        while loader.snapshot().staging_bytes != 0 {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    let end = loader.snapshot();
    assert_eq!(
        (
            end.active_instances,
            end.active_callbacks,
            end.host_capabilities,
            end.host_outputs
        ),
        (0, 0, 0, 0)
    );
    assert_eq!(end.retained_failed_finalizations, 0);
}

async fn assert_linked_parity(
    runtime: &Runtime,
    portable: &Arc<dyn rsi_terminal::presentation::FrameRenderer>,
    source: &[u8],
    cells: &ratatui::buffer::Buffer,
) {
    let mut linked = HostBuilder::without_paths(std::env::consts::OS);
    linked
        .register_local_contract::<FrameRendererContract>()
        .unwrap();
    linked
        .register_linked(
            "linked",
            "1",
            UpdateMode::Replayable,
            Arc::new(LinkedPresentationFactory),
        )
        .unwrap();
    let linked = ScopedProfile::start(
        &linked.build().unwrap(),
        &runtime.root(),
        ProfileProgram::from_profile(Profile::new([ProfileEntry::new(
            "linked",
            "linked",
            serde_json::Value::Null,
        )])),
    )
    .await
    .unwrap();
    let linked_renderer = linked.lookup_local::<FrameRendererContract>().unwrap();
    for renderer in [portable, &linked_renderer] {
        assert_eq!(
            renderer
                .render(
                    request(1, source.len() + 1),
                    source.to_vec(),
                    CancellationToken::new()
                )
                .await
                .unwrap_err(),
            "scene source length mismatch"
        );
    }
    let linked_frame = linked_renderer
        .render(
            request(1, source.len()),
            source.to_vec(),
            CancellationToken::new(),
        )
        .await
        .unwrap();
    assert_eq!(cells, &linked_frame.0);
    drop(linked_renderer);
    assert!(linked.shutdown().await.is_clean());
}

#[derive(Debug)]
struct StalledRenderer;
#[async_trait::async_trait]
impl rsi_meta::ServiceEndpoint for StalledRenderer {
    async fn serve(
        &self,
        _: rsi_meta::InvocationContext,
        mut channel: rsi_meta::ProviderChannel<'_>,
    ) -> rsi_meta::Result<()> {
        while channel.recv().await.is_some() {}
        std::future::pending().await
    }
}
#[async_trait::async_trait]
impl rsi_meta::PluginFactory for StalledRenderer {
    fn prepare(
        &self,
        desired: &rsi_meta::ConfigValue,
    ) -> rsi_meta::Result<rsi_meta::PreparedActivation> {
        Ok(rsi_meta::PreparedActivation::new(desired.clone()))
    }
    async fn activate(&self, plan: rsi_meta::ActivationPlan) -> rsi_meta::Result<()> {
        use rsi_terminal_ui::wire;
        plan.context().provide(
            wire::SERVICE,
            wire::CONTRACT,
            rsi_meta::ContractVersion(wire::VERSION),
            Arc::new(Self),
        )?;
        Ok(())
    }
}

#[tokio::test(start_paused = true)]
async fn stalled_portable_frame_has_a_local_deadline_and_releases_its_call() {
    let runtime = Runtime::default();
    let mut builder = HostBuilder::without_paths("native");
    builder
        .register_local_contract::<FrameRendererContract>()
        .unwrap();
    builder
        .register_linked(
            "rsi.terminal.native",
            "1",
            UpdateMode::Replayable,
            Arc::new(StalledRenderer),
        )
        .unwrap();
    builder
        .register_linked(
            "rsi.terminal.portable",
            "1",
            UpdateMode::Replayable,
            Arc::new(PortablePresentationFactory),
        )
        .unwrap();
    let profile = ScopedProfile::start(&builder.build().unwrap(), &runtime.root(), program())
        .await
        .unwrap();
    let renderer = profile.lookup_local::<FrameRendererContract>().unwrap();
    let source = scene();
    let error = tokio::time::timeout(
        Duration::from_secs(3),
        renderer.render(request(1, source.len()), source, CancellationToken::new()),
    )
    .await
    .expect("frame must time out before the Meta service deadline")
    .unwrap_err();
    assert_eq!(error, "terminal rendering exceeded 2 seconds");
    let source = scene();
    let stop = CancellationToken::new();
    stop.cancel();
    assert_eq!(
        renderer
            .render(request(1, source.len()), source, stop)
            .await
            .unwrap_err(),
        "presentation stopped"
    );
    drop(renderer);
    assert!(profile.shutdown().await.is_clean());
    assert!(runtime.shutdown().await.is_clean());
    assert_eq!(runtime.resource_snapshot().service_calls.current, 0);
}
