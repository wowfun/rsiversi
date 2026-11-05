//! Opt-in acceptance through the actual standard composition and preset loader.
use rsi_agent_composition_protocol::AgentCompositionContract;
use rsi_agent_presets::AgentPresetId;
use rsi_host::{HostPaths, Profile, ProfileProgram};
use serde_json::json;
use std::{collections::BTreeMap, path::PathBuf};
#[path = "process_extension/live.rs"]
mod live;

#[tokio::test]
#[ignore = "requires RSI_EXTENSION_NODE and RSI_DSH_INSTALLED_COHORT"]
async fn original_plugins_activate_from_an_ordinary_standard_agent_preset() {
    activate(None, Scenario::Activate).await;
}

#[tokio::test]
#[ignore = "requires RSI_EXTENSION_NODE and RSI_DSH_PACKAGES"]
async fn installed_plugin_pair_activates_with_leases_through_the_standard_preset() {
    activate(
        Some(PathBuf::from(
            std::env::var("RSI_DSH_PACKAGES").expect("independent RSI plugin packages"),
        )),
        Scenario::Activate,
    )
    .await;
}

#[tokio::test]
#[ignore = "requires explicit Node/cohort and DEEPSEEK_API_KEY; spends live API quota"]
async fn original_todo_settles_live_deepseek_through_the_standard_kernel() {
    activate(None, Scenario::LiveTodo).await;
}

#[tokio::test]
#[ignore = "requires explicit Node/packages and DEEPSEEK_API_KEY; spends live API quota"]
async fn installed_plugin_pair_settles_live_deepseek_through_the_standard_kernel() {
    activate(
        Some(PathBuf::from(
            std::env::var("RSI_DSH_PACKAGES").expect("independent RSI plugin packages"),
        )),
        Scenario::LiveTodo,
    )
    .await;
}

#[derive(Clone, Copy, PartialEq)]
enum Scenario {
    Activate,
    LiveTodo,
    Reject,
    LargeEdit,
}

#[tokio::test]
#[ignore = "requires RSI_EXTENSION_NODE and RSI_DSH_REJECT_PACKAGES with an original prompt.submit drop mod"]
async fn installed_mod_rejects_and_publishes_its_state_in_one_native_commit() {
    let packages =
        std::env::var("RSI_DSH_REJECT_PACKAGES").expect("explicit original Mods fixture");
    activate(Some(PathBuf::from(packages)), Scenario::Reject).await;
}

#[tokio::test]
#[ignore = "requires RSI_EXTENSION_NODE and RSI_DSH_EDIT_PACKAGES with an original large prompt.submit rewrite"]
async fn installed_mod_publishes_fragmented_replacement_with_its_journal() {
    let packages = std::env::var("RSI_DSH_EDIT_PACKAGES").expect("explicit original Mods fixture");
    activate(Some(PathBuf::from(packages)), Scenario::LargeEdit).await;
}

fn write_preset(
    paths: &HostPaths,
    packages: Option<PathBuf>,
) -> Option<(rsi_packages::PackageStore, rsi_packages::Resolution)> {
    #[derive(serde::Serialize)]
    struct Document {
        format: u32,
        steps: serde_json::Value,
    }
    let node =
        PathBuf::from(std::env::var("RSI_EXTENSION_NODE").expect("explicit Node executable"));
    let preset = paths.config().join("agent-presets/dsh-fixture");
    std::fs::create_dir_all(&preset).unwrap();
    let environment: BTreeMap<_, _> = ["SystemRoot", "WINDIR", "TEMP", "TMP", "PATH"]
        .into_iter()
        .filter_map(|name| std::env::var(name).ok().map(|value| (name, value)))
        .collect();
    let mut installed = None;
    let (process_factory, process_config, digest) = if let Some(packages) = packages {
        let store = rsi_packages::PackageStore::open(&paths.state().join("packages")).unwrap();
        let names = ["rsi-dsh-cordis", "rsi-dsh-plugin-bridge"];
        let sources: Vec<_> = names
            .iter()
            .map(|name| {
                let directory = packages.join(name);
                rsi_packages::InstallSource {
                    manifest: std::fs::read(directory.join("package.json")).unwrap(),
                    directory,
                    provenance: rsi_packages::Provenance::Local,
                }
            })
            .collect();
        let mut old_protocols = rsi_serve::supported_protocols();
        old_protocols
            .iter_mut()
            .find(|protocol| protocol.name == "rsi.extension.agent")
            .unwrap()
            .version = 6;
        let unsupported =
            serde_json::from_slice::<rsi_packages::Manifest>(&sources[1].manifest).unwrap();
        assert!(unsupported.admit_interfaces(&old_protocols).is_err());
        let resolution = store
            .install(
                &sources,
                &[],
                &rsi_serve::supported_protocols(),
                "windows-x86_64",
            )
            .unwrap();
        let digest = resolution.packages["rsi-dsh-plugin-bridge"].digest.clone();
        let config = json!({"package":"rsi-dsh-plugin-bridge", "digest":digest, "export":"process", "program":node,"environment":environment});
        installed = Some((store, resolution));
        ("rsi.extension.package", config, digest)
    } else {
        let cohort = PathBuf::from(
            std::env::var("RSI_DSH_INSTALLED_COHORT").expect("independent installed cohort"),
        );
        (
            "rsi.extension.process",
            json!({"program":node,"cwd":cohort,"generation":"standard-original-plugins","environment":environment,
            "arguments":[cohort.join("node_modules/rsi-dsh-cordis/src/host.mjs"),PathBuf::from("standard-original-plugins"),
                cohort.join("node_modules/rsi-dsh-plugin-bridge/src/index.mjs"),cohort.clone()]}),
            "a".repeat(64),
        )
    };
    let document = Document {
        format: 1,
        steps: json!([
            {"kind":"plugin","id":"context","plugin":"rsi.agent.context.default"},
            {"kind":"group","id":"runtime","isolation":{"local":["rsi.extension.connection"]},"nodes":[
                {"kind":"plugin","id":"process","plugin":process_factory,"config":process_config},
                {"kind":"plugin","id":"adapter","plugin":"rsi.extension.agent","config":{"artifact_sha256":digest}}
            ]}
        ]),
    };
    let encoded = toml::to_string(&document).unwrap();
    std::fs::write(preset.join("agent.profile.toml"), encoded).unwrap();
    installed
}

async fn activate(packages: Option<PathBuf>, scenario: Scenario) {
    let root = tempfile::tempdir().unwrap();
    let paths = HostPaths::new(
        root.path().join("config"),
        root.path().join("state"),
        root.path().join("cache"),
    )
    .unwrap();
    let installed = write_preset(&paths, packages);
    let (credentials, provider) = if scenario == Scenario::LiveTodo {
        live::configuration()
    } else {
        (BTreeMap::new(), Profile::default())
    };
    let host = rsi::StandardComposition::new(paths, credentials, None, rsi_app_catalog::metadata())
        .build()
        .unwrap();
    let host = host
        .start_program(ProfileProgram::from_profile(provider))
        .await
        .unwrap();
    let service = host.lookup_local::<AgentCompositionContract>().unwrap();
    let pin = service
        .pin(&AgentPresetId::new("dsh-fixture").unwrap(), None)
        .await
        .unwrap_or_else(|error| panic!("preset activation: {error:?}"));
    let names: Vec<_> = pin
        .tools()
        .definitions()
        .iter()
        .map(|tool| tool.name().to_owned())
        .collect();
    assert_eq!(names, ["exit_plan_mode", "todo_write"]);
    assert!(
        pin.domains()
            .baseline()
            .iter()
            .any(|state| state.identity().id() == "rsi.extension-journal")
    );
    assert!(
        pin.contributions()
            .entries()
            .iter()
            .any(|entry| entry.id().as_str() == "rsi.extension.journal"
                && matches!(
                    entry.kind(),
                    rsi_agent_composition_protocol::ContributionKind::ResourceRead(_)
                ))
    );
    if scenario == Scenario::Activate {
        // A Fresh draft can remain attached while the scheduler restores a durable
        // Session. Both generations must be usable at once with their own process.
        let seed = rsi_agent_composition_protocol::AgentGenerationSeed::new(
            pin.domains().baseline().to_vec(),
        )
        .unwrap();
        let restored = service
            .pin(&AgentPresetId::new("dsh-fixture").unwrap(), Some(&seed))
            .await
            .expect("the seeded generation must not collide with the fresh generation");
        assert_eq!(
            restored.tools().definitions().len(),
            pin.tools().definitions().len()
        );
    }
    if scenario != Scenario::Activate {
        live::run(&host, pin.clone(), root.path(), scenario).await;
    }
    if let Some((store, resolution)) = &installed {
        store
            .uninstall(&resolution.packages.keys().cloned().collect::<Vec<_>>())
            .unwrap();
        let removed = store.collect().unwrap();
        assert!(
            resolution
                .packages
                .values()
                .all(|selection| !removed.contains(&selection.digest))
        );
    }
    drop(pin);
    drop(service);
    assert!(host.shutdown().await.is_clean());
    if let Some((store, resolution)) = installed {
        let removed = store.collect().unwrap();
        assert!(
            resolution
                .packages
                .values()
                .all(|selection| removed.contains(&selection.digest))
        );
    }
}
