use super::*;

#[test]
fn failed_current_selection_cannot_be_bypassed_by_a_saved_catalog() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().canonicalize().unwrap();
    let paths = HostPaths::new(
        root.join("config"),
        root.join("data"),
        root.join("state"),
        root.join("cache"),
        root.join("tmp"),
    )
    .unwrap();
    let composition = crate::StandardComposition::new(
        paths,
        std::collections::BTreeMap::new(),
        None,
        crate::ApplicationCatalogMetadata::default(),
    );
    let store = Arc::new(NativeAddonStore::open(root.join("addons")).unwrap());
    let loader = NativeCatalog::new(rsi_meta_native_loader::CatalogOptions::new(
        root.join("loader"),
    ))
    .unwrap();
    let manager = composition
        .native_addon_manager(store.clone(), loader)
        .unwrap();
    manager.refresh().unwrap();
    let _retained = manager.snapshot().unwrap();
    std::fs::write(root.join("bad.bin"), b"invalid executable").unwrap();
    let manifest = root.join("addon.toml");
    std::fs::write(&manifest, format!("format = 2\nscope = 'agent'\nid = 'fixture.failed'\nplugin = 'fixture.failed'\ntarget = '{}'\nartifact = 'bad.bin'\n", crate::native_addon_target())).unwrap();
    store.install(&manifest).unwrap();
    store.enable("fixture.failed").unwrap();
    assert!(manager.refresh().is_err());
    assert!(manager.snapshot().is_err());
    assert_eq!(manager.inspect().health, NativeAddonHealth::Failed);
    assert!(
        manager.snapshot().is_err(),
        "retained catalogs cannot bypass current admission"
    );
}
