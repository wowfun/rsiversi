use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use rsi_meta_fixture_conformance::PUBLISHED_PACKAGES;

fn repository() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .and_then(Path::parent)
        .expect("conformance package is nested three levels below repository")
        .to_path_buf()
}

#[test]
fn runtime_artifacts_are_ignored_at_any_repository_depth() {
    let repository = repository();
    for path in [
        "nested/.rsi-meta/state.sqlite3",
        "nested/.rsi-meta/daemon.token",
        "nested/.rsi-meta/cache/sha256/plugin.so",
        "nested/custom-state/state.sqlite3",
        "nested/custom-state/daemon.token",
        "nested/.rsi-meta-plugin-candidate-deadbeef.lock",
    ] {
        let status = Command::new("git")
            .args(["check-ignore", "--no-index", "--quiet", path])
            .current_dir(&repository)
            .status()
            .unwrap_or_else(|error| panic!("could not check ignore contract for {path}: {error}"));
        assert!(status.success(), "runtime artifact is not ignored: {path}");
    }
}

#[test]
fn published_package_catalog_matches_owned_plugin_manifests() {
    let repository = repository();
    let mut discovered = BTreeSet::new();
    for namespace in ["fixtures/rsi-meta", "plugins/rsi-meta"] {
        for entry in fs::read_dir(repository.join(namespace)).unwrap() {
            let entry = entry.unwrap();
            if entry.file_type().unwrap().is_dir() && entry.path().join("plugin.toml").is_file() {
                discovered.insert(
                    entry
                        .path()
                        .strip_prefix(&repository)
                        .unwrap()
                        .to_string_lossy()
                        .replace('\\', "/"),
                );
            }
        }
    }
    let catalog = PUBLISHED_PACKAGES
        .iter()
        .map(|package| package.relative_path.to_owned())
        .collect::<BTreeSet<_>>();

    assert_eq!(catalog, discovered);
}
