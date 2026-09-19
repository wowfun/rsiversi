use super::*;
use rsi_meta_native_loader::{CatalogOptions, NativeCatalog};

#[tokio::test]
#[expect(
    clippy::too_many_lines,
    reason = "One generated-project scenario crosses the real public native boundaries"
)]
async fn generated_external_locked_workspace_loads_describes_and_executes() {
    let repository = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../..")
        .canonicalize()
        .unwrap();
    let external = tempfile::tempdir().unwrap();
    let project = external.path().join("Tool addon 空间");
    assert!(
        std::process::Command::new(env!("CARGO"))
            .current_dir(&repository)
            .args([
                "run",
                "--locked",
                "-p",
                "rsi-xtask",
                "--",
                "addon",
                "new",
                "scaffold-probe",
                "--directory"
            ])
            .arg(&project)
            .status()
            .unwrap()
            .success()
    );
    let lock = std::fs::read(project.join("Cargo.lock")).unwrap();
    let target = repository.join("target/addon-scaffold-test");
    assert!(
        std::process::Command::new(env!("CARGO"))
            .current_dir(&project)
            .args(["build", "--locked", "--offline", "--target-dir"])
            .arg(&target)
            .status()
            .unwrap()
            .success()
    );
    assert_eq!(std::fs::read(project.join("Cargo.lock")).unwrap(), lock);
    let cache = tempfile::tempdir().unwrap();
    let catalog = NativeCatalog::new(CatalogOptions::new(cache.path())).unwrap();
    let factory = catalog
        .load(target.join("debug/librsi_addon_scaffold_probe.so"))
        .unwrap();
    let mut addon = StandardAddonBuilder::new("addon.scaffold-probe");
    addon.register_resolved(AddonScope::Agent, factory).unwrap();
    addon
        .isolate_agent_portable("addon.scaffold-probe.tools")
        .unwrap();
    let root = tempfile::tempdir().unwrap();
    let preset = root.path().join("config/agent-presets/scaffold");
    std::fs::create_dir_all(&preset).unwrap();
    std::fs::write(
        root.path().join("config/settings.json"),
        br#"{"rsi.agent":{"default_model":{"deployment":"fixture","model":"unused"}}}"#,
    )
    .unwrap();
    let snippet = std::fs::read_to_string(project.join("agent-profile.toml")).unwrap();
    std::fs::write(preset.join("agent.profile.toml"), format!("format = 1\n[[steps]]\nkind = \"plugin\"\nid = \"context\"\nplugin = \"rsi.agent.context.default\"\n{snippet}")).unwrap();
    let host = composition(root.path())
        .with_addons(StandardAddonSet::new([addon.build().unwrap()]).unwrap())
        .build()
        .unwrap()
        .start(Profile::default())
        .await
        .unwrap();
    let service = host
        .lookup_local::<rsi_agent_composition_protocol::AgentCompositionContract>()
        .unwrap();
    let pin = service
        .pin(
            &rsi_agent_presets::AgentPresetId::new("scaffold").unwrap(),
            None,
        )
        .await
        .unwrap();
    assert_eq!(pin.tools().definitions().len(), 1);
    assert_eq!(pin.tools().definitions()[0].name(), "scaffold_probe_echo");
    let prepared = pin
        .tools()
        .prepare(
            "scaffold-call",
            rsi_tools_protocol::ToolCall {
                id: "scaffold-call".into(),
                name: "scaffold_probe_echo".into(),
                arguments: json!({"answer":42}),
            },
        )
        .unwrap();
    let result = prepared
        .start(rsi_tools_protocol::ToolStart {
            cancellation: tokio_util::sync::CancellationToken::new(),
            policy: rsi_tools_protocol::ToolExecutionPolicy {
                mode: rsi_sandbox::SandboxMode::ReadOnly,
                cwd: root.path().into(),
                workspace: root.path().into(),
            },
            sandbox: host.lookup_local::<rsi_sandbox::SandboxContract>().unwrap(),
            job_scope: None,
            extensions: rsi_tools_protocol::ToolExecutionExtensions::default(),
        })
        .await
        .unwrap();
    assert_eq!(
        result.value,
        json!({"label":"scaffold-probe","arguments":{"answer":42}})
    );
    drop((pin, service));
    assert!(host.shutdown().await.is_clean());
}
