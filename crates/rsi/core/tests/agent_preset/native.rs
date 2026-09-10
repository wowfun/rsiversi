use super::*;

#[tokio::test]
async fn authoring_uses_declared_native_selection_without_loading_or_changing_host_identity() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    let paths =
        HostPaths::new(root.join("config"), root.join("state"), root.join("cache")).unwrap();
    let composition = rsi::StandardComposition::new(paths, std::collections::BTreeMap::new(), None);
    let presets = root.join("presets");
    write_preset(&presets, "standard", "format = 1\n");
    write_preset(
        &presets,
        "native",
        "format = 1\n[[steps]]\nkind = 'plugin'\nid = 'native'\nplugin = 'fixture.native-addon'\n",
    );
    let manager = AgentPresetManager::open(&composition, [&presets])
        .await
        .unwrap();
    let id = AgentPresetId::new("native").unwrap();
    assert!(
        manager
            .authoring_catalog(&composition)
            .unwrap()
            .compile(&id)
            .is_err()
    );
    assert!(!root.join("config/native-addons").exists());
    assert!(!root.join("cache/native-addons").exists());
    let host_profile = rsi::ProfileCatalog::new(composition.paths().clone())
        .host(&rsi::HostProfileId::new("standard").unwrap())
        .unwrap();
    let launch_key = composition.preview_host(&host_profile).unwrap().launch_key;
    let source = root.join("source");
    fs::create_dir(&source).unwrap();
    fs::write(source.join("artifact.bin"), b"not executable native code").unwrap();
    let manifest = source.join("native.toml");
    fs::write(&manifest, format!("format = 2\nscope = 'agent'\nid = 'fixture.native'\nplugin = 'fixture.native-addon'\ntarget = '{}'\nartifact = 'artifact.bin'\n", rsi::native_addon_target())).unwrap();
    let store = rsi::NativeAddonStore::open(root.join("config/native-addons")).unwrap();
    store.install(&manifest).unwrap();
    assert!(
        manager
            .authoring_catalog(&composition)
            .unwrap()
            .compile(&id)
            .is_err()
    );
    store.enable("fixture.native").unwrap();
    let a = manager.authoring_catalog(&composition).unwrap();
    let digest = a.compile(&id).unwrap().source_digest().to_owned();
    assert!(manager.catalog().compile(&id).is_err());
    assert!(!root.join("cache/native-addons").exists());
    a.set_default(&id).await.unwrap();
    assert_eq!(manager.catalog().default_id().await.unwrap(), id);
    fs::write(source.join("artifact.bin"), b"another non-library").unwrap();
    store.install(&manifest).unwrap();
    assert_eq!(
        manager
            .authoring_catalog(&composition)
            .unwrap()
            .compile(&id)
            .unwrap()
            .source_digest(),
        digest
    );
    store.enable("fixture.native").unwrap();
    assert_ne!(
        manager
            .authoring_catalog(&composition)
            .unwrap()
            .compile(&id)
            .unwrap()
            .source_digest(),
        digest
    );
    assert_eq!(a.compile(&id).unwrap().source_digest(), digest);
    assert_eq!(
        composition.preview_host(&host_profile).unwrap().launch_key,
        launch_key
    );
    store.disable("fixture.native").unwrap();
    assert!(
        manager
            .authoring_catalog(&composition)
            .unwrap()
            .compile(&id)
            .is_err()
    );
    fs::write(
        root.join("config/native-addons/state.json"),
        b"invalid source metadata",
    )
    .unwrap();
    assert!(manager.authoring_catalog(&composition).is_err());
    assert_eq!(manager.catalog().default_id().await.unwrap(), id);
    assert!(manager.shutdown().await.is_clean());
}
