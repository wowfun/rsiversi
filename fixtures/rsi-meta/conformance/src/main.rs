use std::fs;
use std::path::{Path, PathBuf};

use rsi_meta_fixture_conformance::{PUBLISHED_PACKAGES, PublishedPackage};
use rsi_meta_loader::{
    BUILD_TARGET, ContentHash, ExpectedHashes, PluginLoader, PluginMailboxOptions, PluginPackage,
    prepare_config,
};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let repository = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .and_then(Path::parent)
        .expect("conformance package is nested three levels below repository");
    let cache = tempfile::tempdir()?;
    let loader = PluginLoader::for_current_process(cache.path().join("plugin-cache"));

    for case in PUBLISHED_PACKAGES {
        validate_package(repository, case, &loader)?;
        println!(
            "validated {} config={} target={BUILD_TARGET}",
            case.relative_path, case.expected_audit_sha256
        );
    }
    Ok(())
}

fn validate_package(
    repository: &Path,
    case: &PublishedPackage,
    loader: &PluginLoader,
) -> Result<(), Box<dyn std::error::Error>> {
    let package_directory = repository.join(case.relative_path);
    let manifest_path = package_directory.join("plugin.toml");
    let manifest_bytes = fs::read(&manifest_path)?;
    let package = PluginPackage::open(&manifest_path)?;
    let artifact = loader.validate_manifest(package.manifest())?;

    assert!(package.manifest().artifacts.iter().any(|artifact| {
        artifact.target == "x86_64-unknown-linux-gnu"
            && artifact
                .path
                .extension()
                .is_some_and(|extension| extension == "so")
    }));
    assert!(package.manifest().artifacts.iter().any(|artifact| {
        artifact.target == "aarch64-apple-darwin"
            && artifact
                .path
                .extension()
                .is_some_and(|extension| extension == "dylib")
    }));

    let schema_path = package_directory.join(
        package
            .manifest()
            .config_schema
            .as_ref()
            .expect("published fixture packages require config_schema"),
    );
    let schema: serde_json::Value = serde_json::from_slice(&fs::read(schema_path)?)?;
    assert_eq!(
        schema["$schema"],
        "https://json-schema.org/draft/2020-12/schema"
    );
    assert_eq!(schema["type"], "object");

    let representative_config = case.representative_config();
    let prepared = prepare_config(&package, representative_config.clone())?;
    assert_eq!(prepared.resolved(), &representative_config);
    assert_eq!(prepared.redacted(), &representative_config);
    assert_eq!(prepared.audit_hash().to_hex(), case.expected_audit_sha256);
    assert!(!serde_json::to_string(prepared.redacted())?.contains("$secret"));

    let artifact_path: PathBuf = package_directory.join(&artifact.path);
    let artifact_bytes = fs::read(&artifact_path).map_err(|error| {
        format!(
            "current-target artifact was not built at {}: {error}",
            artifact_path.display()
        )
    })?;
    let staged = loader.stage(
        &manifest_path,
        ExpectedHashes::new(
            ContentHash::digest(manifest_bytes),
            ContentHash::digest(artifact_bytes),
        ),
    )?;
    let (plugin, _mailbox) = loader.load_queued(&staged, PluginMailboxOptions::default())?;
    drop(plugin);

    if case.relative_path == "plugins/rsi-meta/hmr-consumer" {
        assert!(package.manifest().package.process_fixed);
    } else {
        assert!(!package.manifest().package.process_fixed);
    }
    Ok(())
}
