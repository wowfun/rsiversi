use super::*;
use rsi_agent_composition::{AgentCompositionFactory, AgentGenerationRootFactory};
use rsi_agent_composition_protocol::AgentCompositionContract;
use rsi_agent_presets::AgentPresetId;
use rsi_meta::{FactoryIdentity, PluginFactory, ResolvedFactory, Runtime, UpdateMode};
use rsi_meta_scope::ScopeRoot;

fn artifacts(destination: &Path) -> [std::path::PathBuf; 2] {
    let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../..");
    let target = root.join("target/native-addon-manager-fixture-test");
    let mut copies = Vec::new();
    for variant in ["a", "b"] {
        let mut command = std::process::Command::new(env!("CARGO"));
        command
            .env("CARGO_BUILD_JOBS", "2")
            .args(["build", "--locked", "--manifest-path"])
            .arg(root.join("fixtures/rsi/native-addon/Cargo.toml"))
            .arg("--target-dir")
            .arg(&target);
        if variant == "b" {
            command.args(["--features", "revision-b"]);
        }
        assert!(command.status().unwrap().success());
        let artifact = target.join("debug").join(format!(
            "{}rsi_fixture_native_addon{}",
            std::env::consts::DLL_PREFIX,
            std::env::consts::DLL_SUFFIX
        ));
        let snapshot = destination.join(variant);
        fs::copy(artifact, &snapshot).unwrap();
        report(&format!("{variant}.native"), &fs::read(&snapshot).unwrap());
        copies.push(snapshot);
    }
    copies.try_into().unwrap()
}

async fn apply(runtime: &Runtime, id: &str, factory: impl PluginFactory) -> rsi_meta::FiberHandle {
    runtime
        .root()
        .apply(
            ResolvedFactory::linked(
                id,
                "fixture",
                UpdateMode::RestartRequired,
                Arc::new(factory),
            ),
            serde_json::Value::Null,
        )
        .await
        .unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn two_real_artifacts_update_new_generations_while_old_pins_keep_their_code() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    let [a, b] = artifacts(&root);
    let manifest = source(&root.join("source"), "fixture.native-addon");
    let text = fs::read_to_string(&manifest).unwrap();
    fs::write(
        &manifest,
        format!("{text}portable_services = ['fixture.native.tools']\n"),
    )
    .unwrap();
    fs::copy(&a, root.join("source/artifact.bin")).unwrap();
    let store = Arc::new(NativeAddonStore::open(root.join("store")).unwrap());
    let a_record = store.install(&manifest).unwrap().record.unwrap();
    store.enable("fixture.addon").unwrap();
    let preset = root.join("config/agent-presets/native");
    fs::create_dir_all(&preset).unwrap();
    let profile = "format = 1\n[[steps]]\nkind = 'plugin'\nid = 'context'\nplugin = 'rsi.agent.context.default'\n[[steps]]\nkind = 'plugin'\nid = 'native'\nplugin = 'fixture.native-addon'\nconfig = { label = 'same-profile' }\n[[steps]]\nkind = 'plugin'\nid = 'bridge'\nplugin = 'rsi.tools.portable'\nconfig = { service = 'fixture.native.tools' }\n";
    fs::write(preset.join("agent.profile.toml"), profile).unwrap();
    let catalog = NativeCatalog::new(CatalogOptions::new(root.join("loader"))).unwrap();
    let manager = composition(&root)
        .native_addon_manager(store.clone(), catalog.clone())
        .unwrap();
    assert!(manager.refresh().unwrap().changed);
    let runtime = Runtime::default();
    let generation_root = apply(&runtime, "root", AgentGenerationRootFactory).await;
    let tools = apply(&runtime, "tools", rsi_tools::ToolsFactory).await;
    let provider = apply(
        &runtime,
        "composition",
        AgentCompositionFactory::with_source(manager.clone(), ScopeRoot::new(128).unwrap()),
    )
    .await;
    let service = runtime
        .root()
        .lookup_local::<AgentCompositionContract>()
        .unwrap();
    let id = AgentPresetId::new("native").unwrap();
    let old = service.pin(&id).await.unwrap();
    assert_eq!(
        old.tools().definitions()[0].description(),
        "Native fixture tool"
    );
    fs::copy(&b, root.join("source/artifact.bin")).unwrap();
    let b_record = store.install(&manifest).unwrap().record.unwrap();
    assert_ne!(a_record.artifact_sha256(), b_record.artifact_sha256());
    assert!(!manager.refresh().unwrap().changed);
    assert_eq!(
        old.source_digest(),
        service.pin(&id).await.unwrap().source_digest()
    );
    store.enable("fixture.addon").unwrap();
    assert!(service.pin(&id).await.is_err());
    manager.refresh().unwrap();
    let new = service.pin(&id).await.unwrap();
    assert_ne!(old.source_digest(), new.source_digest());
    assert_eq!(
        new.tools().definitions()[0].description(),
        "Native fixture tool revision B"
    );
    assert_eq!(
        old.tools().definitions()[0].description(),
        "Native fixture tool"
    );
    assert_eq!(
        fs::read_to_string(preset.join("agent.profile.toml")).unwrap(),
        profile
    );
    let fibers = runtime
        .inspect(rsi_meta::InspectionRequest {
            maximum_fibers: 64,
            ..Default::default()
        })
        .unwrap()
        .fibers;
    for record in [&a_record, &b_record] {
        assert!(fibers.iter().any(|fiber| fiber.factory
            == FactoryIdentity::native(record.plugin(), record.artifact_sha256())));
    }
    store.disable("fixture.addon").unwrap();
    assert!(service.pin(&id).await.is_err());
    manager.refresh().unwrap();
    assert!(
        service.pin(&id).await.is_err(),
        "removed factory must also leave the compiler allowlist"
    );
    assert_eq!(
        old.tools().definitions()[0].description(),
        "Native fixture tool"
    );
    manager.close();
    drop((old, new));
    assert!(provider.dispose().await.is_clean());
    drop(service);
    assert!(tools.dispose().await.is_clean());
    assert!(generation_root.dispose().await.is_clean());
    assert!(runtime.shutdown().await.is_clean());
    wait_for_native_release(&catalog).await;
}

async fn wait_for_native_release(catalog: &NativeCatalog) {
    tokio::time::timeout(std::time::Duration::from_secs(10), async {
        loop {
            let resources = catalog.snapshot();
            if resources.staging_bytes == 0
                && resources.active_instances == 0
                && resources.active_callbacks == 0
            {
                assert_eq!(resources.host_capabilities, 0);
                assert_eq!(resources.host_outputs, 0);
                assert_eq!(resources.retained_failed_finalizations, 0);
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
}
