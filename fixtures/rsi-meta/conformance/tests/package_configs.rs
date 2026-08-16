use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

use rsi_meta_fixture_conformance::PUBLISHED_PACKAGES;
use rsi_meta_loader::{PluginPackage, prepare_config};

fn repository() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .and_then(Path::parent)
        .expect("conformance package is nested three levels below repository")
        .to_path_buf()
}

#[test]
fn every_published_package_prepares_a_stable_non_secret_config() {
    assert_eq!(PUBLISHED_PACKAGES.len(), 7);
    for case in PUBLISHED_PACKAGES {
        let package =
            PluginPackage::open(repository().join(case.relative_path).join("plugin.toml")).unwrap();
        let config = case.representative_config();
        let cargo: toml::Value = toml::from_str(
            &fs::read_to_string(repository().join(case.relative_path).join("Cargo.toml")).unwrap(),
        )
        .unwrap();
        assert_eq!(cargo["package"]["license"].as_str(), Some("MIT"));
        assert_eq!(
            cargo["package"]["version"].as_str(),
            Some(package.manifest().package.version.as_str())
        );
        let library_name = cargo
            .get("lib")
            .and_then(|lib| lib.get("name"))
            .and_then(toml::Value::as_str)
            .or_else(|| cargo["package"]["name"].as_str())
            .unwrap()
            .replace('-', "_");
        assert_eq!(
            package
                .manifest()
                .artifacts
                .iter()
                .map(|artifact| artifact.target.as_str())
                .collect::<BTreeSet<_>>(),
            BTreeSet::from(["aarch64-apple-darwin", "x86_64-unknown-linux-gnu",]),
            "{} must publish exactly the two release targets",
            case.relative_path
        );
        assert_eq!(package.manifest().artifacts.len(), 2);

        for artifact in &package.manifest().artifacts {
            assert!(
                artifact
                    .path
                    .starts_with(format!("target/{}/release", artifact.target)),
                "{} must use a target-qualified artifact path for {}",
                case.relative_path,
                artifact.target
            );
            let extension = match artifact.target.as_str() {
                "x86_64-unknown-linux-gnu" => "so",
                "aarch64-apple-darwin" => "dylib",
                target => panic!("unexpected published target {target}"),
            };
            let expected_filename = format!("lib{library_name}.{extension}");
            assert_eq!(
                artifact.path.file_name().and_then(|name| name.to_str()),
                Some(expected_filename.as_str()),
                "{} artifact filename must match its Cargo cdylib target",
                case.relative_path
            );
        }

        let prepared = prepare_config(&package, config.clone()).unwrap();

        assert_eq!(prepared.resolved(), &config, "{}", case.relative_path);
        assert_eq!(prepared.redacted(), &config, "{}", case.relative_path);
        assert_eq!(
            prepared.audit_hash().to_hex(),
            case.expected_audit_sha256,
            "{}",
            case.relative_path
        );
        assert!(
            !serde_json::to_string(prepared.redacted())
                .unwrap()
                .contains("$secret"),
            "{} representative config must not expose or resolve a secret",
            case.relative_path
        );
    }
}
