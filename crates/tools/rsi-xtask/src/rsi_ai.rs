use std::{fs, path::Path};

use rsi_meta_loader::{
    ApiVersion, ContentHash, ExpectedHashes, PluginLoader, PluginMailboxOptions, PluginPackage,
    compile_config_schema,
};
use rsi_meta_plugin::{CallOutcome, Frame, FrameBody, Lane, LifecyclePhase};
use serde_json::{Value, json};

use crate::cargo_step::{self, CargoStep};
use crate::repository_root;

pub fn run(repository: &Path) -> Result<(), String> {
    repository_root::require(repository, "rsi-ai conformance")?;
    let manifest = repository.join("plugins/rsi-ai/Cargo.toml");
    if !manifest.is_file() {
        return Err(format!(
            "required rsi-ai plugin workspace manifest `{}` is missing",
            cargo_step::display_relative(repository, &manifest)
        ));
    }
    let native_target = cargo_step::detect_native_target(repository)?;
    let target = native_target.triple();
    let target_dir = repository.join("plugins/rsi-ai/target");
    let manifest = manifest.into_os_string();
    let steps = [
        CargoStep::new(
            "fetch rsi-ai plugin workspace",
            [
                "fetch".into(),
                "--locked".into(),
                "--manifest-path".into(),
                manifest.clone(),
            ],
        ),
        CargoStep::new(
            "format rsi-ai plugin workspace",
            [
                "fmt".into(),
                "--manifest-path".into(),
                manifest.clone(),
                "--all".into(),
                "--check".into(),
            ],
        ),
        CargoStep::new(
            "clippy rsi-ai plugin workspace",
            [
                "clippy".into(),
                "--locked".into(),
                "--offline".into(),
                "--manifest-path".into(),
                manifest.clone(),
                "--target-dir".into(),
                target_dir.as_os_str().into(),
                "--workspace".into(),
                "--all-targets".into(),
                "--".into(),
                "-D".into(),
                "warnings".into(),
            ],
        ),
        CargoStep::new(
            "test rsi-ai plugin workspace",
            [
                "test".into(),
                "--locked".into(),
                "--offline".into(),
                "--manifest-path".into(),
                manifest.clone(),
                "--target-dir".into(),
                target_dir.as_os_str().into(),
                "--workspace".into(),
            ],
        ),
        CargoStep::new(
            "build rsi-ai native plugins",
            [
                "build".into(),
                "--locked".into(),
                "--offline".into(),
                "--manifest-path".into(),
                manifest,
                "--target-dir".into(),
                target_dir.into_os_string(),
                "--workspace".into(),
                "--release".into(),
                "--target".into(),
                target.into(),
            ],
        ),
    ];
    cargo_step::execute(repository, &steps)?;
    stage_and_validate_plugins(repository, native_target)
}

const PLUGINS: [(&str, &str); 4] = [
    ("openai", "rsi_ai_plugin_openai"),
    (
        "openai-compatible-chat",
        "rsi_ai_plugin_openai_compatible_chat",
    ),
    ("deepseek", "rsi_ai_plugin_deepseek"),
    ("xiaomi", "rsi_ai_plugin_xiaomi"),
];

fn stage_and_validate_plugins(
    repository: &Path,
    target: cargo_step::NativeTarget,
) -> Result<(), String> {
    let target_triple = target.triple();
    let loader = PluginLoader::new(
        repository.join("plugins/rsi-ai/target/loader-cache"),
        target_triple,
        ApiVersion::CURRENT,
    );
    for (package_name, library_stem) in PLUGINS {
        let library = native_library_name(target, library_stem);
        let source = repository
            .join("plugins/rsi-ai/target")
            .join(target_triple)
            .join("release")
            .join(&library);
        if !source.is_file() {
            return Err(format!(
                "built rsi-ai plugin is missing: {}",
                source.display()
            ));
        }
        let package_root = repository.join("plugins/rsi-ai").join(package_name);
        let destination = package_root
            .join("target/native")
            .join(target_triple)
            .join(&library);
        fs::create_dir_all(destination.parent().expect("staged artifact parent"))
            .map_err(|error| format!("could not create plugin staging directory: {error}"))?;
        fs::copy(&source, &destination).map_err(|error| {
            format!(
                "could not stage `{}` as `{}`: {error}",
                source.display(),
                destination.display()
            )
        })?;

        let package = PluginPackage::open(package_root.join("plugin.toml"))
            .map_err(|error| format!("loader rejected {package_name} package: {error}"))?;
        let artifact = loader
            .validate_manifest(package.manifest())
            .map_err(|error| format!("loader rejected {package_name} manifest: {error}"))?;
        let resolved = package_root.join(&artifact.path);
        if resolved != destination || !resolved.is_file() {
            return Err(format!(
                "loader-selected artifact for {package_name} was not staged at {}",
                destination.display()
            ));
        }
        validate_schema_and_native_entry(
            &loader,
            &package,
            &destination,
            representative_config(package_name),
        )?;
    }
    Ok(())
}

fn validate_schema_and_native_entry(
    loader: &PluginLoader,
    package: &PluginPackage,
    artifact: &Path,
    config: Value,
) -> Result<(), String> {
    let package_root = package
        .manifest_path()
        .parent()
        .ok_or_else(|| "plugin manifest has no package directory".to_owned())?;
    let schema_relative = package
        .manifest()
        .config_schema
        .as_ref()
        .ok_or_else(|| "rsi-ai plugin must declare config_schema".to_owned())?;
    let schema = fs::read(package_root.join(schema_relative))
        .map_err(|error| format!("could not read plugin config schema: {error}"))?;
    compile_config_schema(package, Some(&schema))
        .map_err(|error| format!("loader rejected plugin config schema: {error}"))?;

    let manifest = fs::read(package.manifest_path())
        .map_err(|error| format!("could not read plugin manifest: {error}"))?;
    let artifact = fs::read(artifact)
        .map_err(|error| format!("could not read staged plugin artifact: {error}"))?;
    let staged = loader
        .stage(
            package.manifest_path(),
            ExpectedHashes::new(ContentHash::digest(manifest), ContentHash::digest(artifact)),
        )
        .map_err(|error| format!("could not stage plugin through loader: {error}"))?;
    let (mut plugin, mut mailbox) = loader
        .load_queued(&staged, PluginMailboxOptions::default())
        .map_err(|error| format!("could not load plugin native entry point: {error}"))?;

    dispatch_lifecycle(
        &mut plugin,
        &Frame::lifecycle(LifecyclePhase::Prepare, 1, Some(config)),
    )?;
    let prepared = mailbox
        .try_recv_control()
        .map_err(|error| format!("plugin did not acknowledge lifecycle prepare: {error}"))?;
    let prepared = Frame::decode(prepared.payload())
        .map_err(|error| format!("plugin returned an invalid lifecycle frame: {error}"))?;
    if !matches!(
        &prepared.body,
        FrameBody::Lifecycle {
            phase: LifecyclePhase::Prepared,
            generation: 1,
            config: None,
        }
    ) {
        return Err(format!(
            "plugin returned an unexpected prepare acknowledgement: {prepared:?}"
        ));
    }
    dispatch_lifecycle(
        &mut plugin,
        &Frame::lifecycle(LifecyclePhase::Committed, 1, None),
    )?;
    dispatch_lifecycle(
        &mut plugin,
        &Frame::lifecycle(LifecyclePhase::Retire, 1, None),
    )?;
    let retired = mailbox
        .try_recv_control()
        .map_err(|error| format!("plugin did not acknowledge lifecycle retire: {error}"))?;
    let retired = Frame::decode(retired.payload())
        .map_err(|error| format!("plugin returned an invalid retire frame: {error}"))?;
    if !matches!(
        &retired.body,
        FrameBody::Lifecycle {
            phase: LifecyclePhase::Retired,
            generation: 1,
            config: None,
        }
    ) {
        return Err(format!(
            "plugin returned an unexpected retire acknowledgement: {retired:?}"
        ));
    }
    if !matches!(plugin.shutdown(), CallOutcome::Ok | CallOutcome::Closed) {
        return Err("plugin rejected graceful shutdown".to_owned());
    }
    Ok(())
}

fn dispatch_lifecycle(
    plugin: &mut rsi_meta_loader::LoadedPlugin,
    frame: &Frame,
) -> Result<(), String> {
    let payload = frame
        .encode()
        .map_err(|error| format!("could not encode lifecycle frame: {error}"))?;
    match plugin.dispatch(Lane::Control, &payload) {
        CallOutcome::Ok => Ok(()),
        outcome => Err(format!("plugin rejected lifecycle frame: {outcome:?}")),
    }
}

fn representative_config(package_name: &str) -> Value {
    match package_name {
        "openai-compatible-chat" => json!({
            "endpoint": "https://conformance.invalid",
            "api_key": "conformance-placeholder"
        }),
        "openai" | "deepseek" | "xiaomi" => {
            json!({"api_key": "conformance-placeholder"})
        }
        _ => unreachable!("PLUGINS contains only known package names"),
    }
}

fn native_library_name(target: cargo_step::NativeTarget, stem: &str) -> String {
    format!("lib{stem}.{}", target.dynamic_library_extension())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn supported_host_has_a_manifest_target() {
        let target = cargo_step::detect_native_target(Path::new(env!("CARGO_MANIFEST_DIR")))
            .expect("CI-supported host")
            .triple();
        assert!(target.contains(std::env::consts::ARCH));
    }

    #[test]
    fn plugin_manifests_use_package_local_artifacts_and_runtime_ticks() {
        let repository = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..");
        let target = cargo_step::detect_native_target(&repository)
            .expect("supported target")
            .triple();
        for package in ["openai", "openai-compatible-chat", "deepseek", "xiaomi"] {
            let source = std::fs::read_to_string(
                repository
                    .join("plugins/rsi-ai")
                    .join(package)
                    .join("plugin.toml"),
            )
            .expect("plugin manifest");
            assert!(
                source.contains(&format!("path = \"target/native/{target}/")),
                "{package} artifact must be package-relative"
            );
            assert!(source.contains("contract = \"runtime.tick\""));
        }
    }
}
