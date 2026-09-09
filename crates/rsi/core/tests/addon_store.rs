#![cfg(unix)]
use rsi::{NativeAddonStore, NativeAddonStoreLimits};
use std::fs;

#[path = "addon_store/boundaries.rs"]
mod boundaries;
#[path = "addon_store/durable.rs"]
mod durable;

fn source(root: &std::path::Path, bytes: &[u8]) -> std::path::PathBuf {
    fs::create_dir_all(root).unwrap();
    fs::write(root.join("artifact.bin"), bytes).unwrap();
    let manifest = root.join("addon.toml");
    fs::write(&manifest, format!("format = 1\nid = 'fixture.addon'\nplugin = 'fixture.native-addon'\ntarget = '{}-{}'\nartifact = 'artifact.bin'\nportable_services = ['fixture.native.tools']\n", std::env::consts::OS, std::env::consts::ARCH)).unwrap();
    manifest
}

#[test]
fn install_is_non_executing_and_enabled_identity_changes_only_on_explicit_enable() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    // Deliberately not a dynamic library: storage must never attempt to execute it.
    let manifest = source(&root.join("source"), b"first artifact");
    let store = NativeAddonStore::open(root.join("store")).unwrap();
    let first = store.install(&manifest).unwrap();
    assert!(first.changed);
    let installed = first.record.unwrap();
    assert_eq!(store.snapshot().unwrap().installed.len(), 1);
    assert!(store.snapshot().unwrap().enabled.is_empty());
    store.enable("fixture.addon").unwrap();
    let enabled = store.snapshot().unwrap().enabled[0].clone();
    assert_eq!(installed, enabled);
    fs::write(root.join("source/artifact.bin"), b"second artifact").unwrap();
    store.install(&manifest).unwrap();
    let snapshot = store.snapshot().unwrap();
    assert_eq!(snapshot.enabled, [enabled]);
    assert_ne!(snapshot.installed[0], snapshot.enabled[0]);
    assert!(store.uninstall("fixture.addon").is_err());
    store.enable("fixture.addon").unwrap();
    let snapshot = store.snapshot().unwrap();
    assert_eq!(snapshot.installed, snapshot.enabled);
    store.disable("fixture.addon").unwrap();
    store.uninstall("fixture.addon").unwrap();
    assert!(store.snapshot().unwrap().installed.is_empty());
    assert!(store.snapshot().unwrap().enabled.is_empty());
}

#[test]
fn object_quotas_reject_without_publishing_a_new_installation() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    let manifest = source(&root.join("source"), b"first");
    let store = NativeAddonStore::with_limits(
        root.join("store"),
        NativeAddonStoreLimits {
            maximum_objects: 1,
            maximum_object_bytes: 5,
        },
    )
    .unwrap();
    store.install(&manifest).unwrap();
    let before = store.snapshot().unwrap();
    assert!(!store.install(&manifest).unwrap().changed);
    fs::write(root.join("source/artifact.bin"), b"second").unwrap();
    assert!(store.install(&manifest).is_err());
    assert_eq!(store.snapshot().unwrap(), before);
}
