#![cfg(unix)]
use rsi::{NativeAddonHealth, NativeAddonStore, NativeAddonUpdateError, StandardComposition};
use rsi_agent_composition::AgentCompositionSource as _;
use rsi_host::HostPaths;
use rsi_meta_native_loader::{CatalogOptions, NativeCatalog};
use std::{collections::BTreeMap, fs, path::Path, sync::Arc};

#[path = "native_addons/generations.rs"]
mod generations;
#[path = "native_addons/races.rs"]
mod races;

fn report(name: &str, bytes: &[u8]) {
    if let Some(directory) = std::env::var_os("RSI_NATIVE_ADDON_REPORT") {
        let directory = std::path::PathBuf::from(directory);
        fs::create_dir_all(&directory).unwrap();
        fs::write(directory.join(name), bytes).unwrap();
    }
}
#[path = "native_addons/retention.rs"]
mod retention;

#[test]
fn tampered_source_object_fails_exact_admission_before_any_native_callback() {
    use std::os::unix::fs::PermissionsExt as _;
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    let manifest = source(&root.join("source"), "fixture.native-addon");
    let store = Arc::new(NativeAddonStore::open(root.join("store")).unwrap());
    let record = store.install(&manifest).unwrap().record.unwrap();
    store.enable("fixture.addon").unwrap();
    let object = root.join("store/objects").join(record.artifact_sha256());
    fs::set_permissions(&object, fs::Permissions::from_mode(0o600)).unwrap();
    fs::write(object, b"tampered source bytes").unwrap();
    let catalog = NativeCatalog::new(CatalogOptions::new(root.join("loader"))).unwrap();
    let manager = composition(&root)
        .native_addon_manager(store, catalog.clone())
        .unwrap();
    assert!(matches!(
        manager.refresh(),
        Err(NativeAddonUpdateError::Load(
            rsi_meta_native_loader::LoaderError::ArtifactDigestMismatch
        ))
    ));
    assert_eq!(catalog.snapshot().peak_callbacks, 0);
    assert_eq!(catalog.snapshot().cache_artifacts, 0);
    assert!(manager.snapshot().is_err());
}

fn composition(root: &Path) -> StandardComposition {
    StandardComposition::new(
        HostPaths::new(root.join("config"), root.join("state"), root.join("cache")).unwrap(),
        BTreeMap::new(),
        None,
    )
}

fn source(root: &Path, plugin: &str) -> std::path::PathBuf {
    fs::create_dir_all(root).unwrap();
    fs::write(root.join("artifact.bin"), b"not a library").unwrap();
    let path = root.join("addon.toml");
    fs::write(&path, format!("format = 1\nid = 'fixture.addon'\nplugin = '{plugin}'\ntarget = '{}'\nartifact = 'artifact.bin'\n", rsi::native_addon_target())).unwrap();
    path
}

#[test]
fn pending_and_failed_selection_close_new_snapshots_without_executing_installed_only_bytes() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    let manifest = source(&root.join("source"), "fixture.native-addon");
    let store = Arc::new(NativeAddonStore::open(root.join("store")).unwrap());
    let catalog = NativeCatalog::new(CatalogOptions::new(root.join("loader"))).unwrap();
    let manager = composition(&root)
        .native_addon_manager(store.clone(), catalog.clone())
        .unwrap();
    let initial = manager.snapshot().unwrap();
    store.install(&manifest).unwrap();
    assert!(!manager.refresh().unwrap().changed);
    assert!(Arc::ptr_eq(&initial, &manager.snapshot().unwrap()));
    assert_eq!(catalog.snapshot().cache_artifacts, 0);
    store.enable("fixture.addon").unwrap();
    assert_eq!(manager.inspect().health, NativeAddonHealth::Pending);
    assert!(manager.snapshot().is_err());
    assert!(matches!(
        manager.refresh(),
        Err(NativeAddonUpdateError::Load(_))
    ));
    assert_eq!(manager.inspect().health, NativeAddonHealth::Failed);
    assert!(manager.snapshot().is_err());
    store.disable("fixture.addon").unwrap();
    manager.refresh().unwrap();
    assert!(manager.snapshot().is_ok());
    manager.close();
    assert_eq!(manager.inspect().health, NativeAddonHealth::Closed);
    assert!(manager.snapshot().is_err());
    assert!(matches!(
        manager.refresh(),
        Err(NativeAddonUpdateError::Closed)
    ));
}

#[test]
fn full_selection_preflight_rejects_linked_plugin_collisions_before_loader_admission() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    let manifest = source(&root.join("source"), "rsi.agent.context.default");
    let store = Arc::new(NativeAddonStore::open(root.join("store")).unwrap());
    store.install(&manifest).unwrap();
    store.enable("fixture.addon").unwrap();
    let catalog = NativeCatalog::new(CatalogOptions::new(root.join("loader"))).unwrap();
    let manager = composition(&root)
        .native_addon_manager(store, catalog.clone())
        .unwrap();
    assert!(manager.snapshot().is_err());
    assert!(matches!(
        manager.refresh(),
        Err(NativeAddonUpdateError::Selection(_))
    ));
    assert_eq!(catalog.snapshot().peak_loads, 0);
    assert_eq!(catalog.snapshot().cache_artifacts, 0);
    assert_eq!(manager.inspect().health, NativeAddonHealth::Failed);
}
