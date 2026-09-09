use super::*;
use rsi_meta_native_loader::{CatalogOptions, NativeCatalog};

fn artifact() -> std::path::PathBuf {
    let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../..");
    let target = root.join("target/native-addon-fixture-test");
    assert!(
        std::process::Command::new(env!("CARGO"))
            .env("CARGO_BUILD_JOBS", "2")
            .args(["build", "--locked", "--manifest-path"])
            .arg(root.join("fixtures/rsi/native-addon/Cargo.toml"))
            .arg("--target-dir")
            .arg(&target)
            .status()
            .unwrap()
            .success()
    );
    target.join("debug").join(format!(
        "{}rsi_fixture_native_addon{}",
        std::env::consts::DLL_PREFIX,
        std::env::consts::DLL_SUFFIX
    ))
}

#[tokio::test]
async fn real_native_addon_keeps_provenance_and_executes_through_the_standard_agent_pin() {
    let root = tempfile::tempdir().unwrap();
    let config = root.path().join("config");
    let preset = config.join("agent-presets/native");
    std::fs::create_dir_all(&preset).unwrap();
    std::fs::write(
        config.join("settings.json"),
        br#"{"rsi.agent":{"default_model":{"deployment":"fixture","model":"unused"}}}"#,
    )
    .unwrap();
    std::fs::write(
        preset.join("agent.profile.toml"),
        r#"format = 1
[[steps]]
kind = "plugin"
id = "context"
plugin = "rsi.agent.context.default"
[[steps]]
kind = "plugin"
id = "native"
plugin = "fixture.native-addon"
config = { label = "product-native" }
[[steps]]
kind = "plugin"
id = "bridge"
plugin = "rsi.tools.portable"
config = { service = "fixture.native.tools" }
"#,
    )
    .unwrap();
    let cache = tempfile::tempdir().unwrap();
    let catalog = NativeCatalog::new(CatalogOptions::new(cache.path())).unwrap();
    let factory = catalog.load(artifact()).unwrap();
    let identity = factory.identity().clone();
    assert!(matches!(identity, rsi_meta::FactoryIdentity::Native { .. }));
    let mut addon = StandardAddonBuilder::new("fixture.native");
    addon.register_resolved(AddonScope::Agent, factory).unwrap();
    addon
        .isolate_agent_portable("fixture.native.tools")
        .unwrap();
    let set = StandardAddonSet::new([addon.build().unwrap()]).unwrap();
    assert_eq!(set.descriptions().next().unwrap().identity, identity);
    let host = composition(root.path())
        .with_addons(set)
        .build()
        .unwrap()
        .start(Profile::default())
        .await
        .unwrap();
    let service = host
        .lookup_local::<rsi_agent_composition_protocol::AgentCompositionContract>()
        .unwrap();
    let pin = service
        .pin(&rsi_agent_presets::AgentPresetId::new("native").unwrap())
        .await
        .unwrap();
    let inspected = host
        .inspect(rsi_meta::InspectionRequest {
            maximum_fibers: 64,
            ..Default::default()
        })
        .unwrap();
    assert!(
        inspected
            .fibers
            .iter()
            .any(|fiber| fiber.factory == identity)
    );
    assert_eq!(pin.tools().definitions().len(), 2);
    verify_echo(&pin, &host, root.path()).await;
    drop((pin, service));
    assert!(host.shutdown().await.is_clean());
    assert!(
        catalog.snapshot().staging_bytes > 0,
        "the frozen Host still owns its catalog"
    );
    drop(host);
    tokio::time::timeout(std::time::Duration::from_secs(10), async {
        while catalog.snapshot().staging_bytes != 0 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    let resources = catalog.snapshot();
    assert_eq!(resources.active_instances, 0);
    assert_eq!(resources.host_capabilities, 0);
    assert_eq!(resources.host_outputs, 0);
    assert_eq!(resources.retained_failed_finalizations, 0);
}

async fn verify_echo(
    pin: &rsi_agent_composition_protocol::AgentCompositionPin,
    host: &rsi_host::RunningHost,
    root: &std::path::Path,
) {
    use rsi_tools_protocol::{
        RetainedToolResult, ToolCall, ToolExecutionExtensions, ToolExecutionPolicy, ToolStart,
    };
    let tools = pin.tools();
    let prepared = tools
        .prepare(
            "native-call",
            ToolCall {
                id: "native-call".into(),
                name: "native_echo".into(),
                arguments: json!({"answer":42}),
            },
        )
        .unwrap();
    let identity = prepared.identity().clone();
    let result = prepared
        .start(ToolStart {
            cancellation: tokio_util::sync::CancellationToken::new(),
            policy: ToolExecutionPolicy {
                mode: rsi_sandbox::SandboxMode::ReadOnly,
                cwd: root.into(),
                workspace: root.into(),
            },
            sandbox: host.lookup_local::<rsi_sandbox::SandboxContract>().unwrap(),
            job_scope: None,
            extensions: ToolExecutionExtensions::default(),
        })
        .await
        .unwrap();
    assert_eq!(result.value["label"], "product-native");
    assert_eq!(result.value["arguments"], json!({"answer":42}));
    assert_eq!(result.value["policy"]["mode"], "read-only");
    assert_eq!(
        tools
            .wait(&identity, tokio_util::sync::CancellationToken::new())
            .await
            .unwrap(),
        RetainedToolResult::Returned(result)
    );
    tools.commit(&identity).unwrap();
}
