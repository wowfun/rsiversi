use rsi::{
    ApplicationCatalogMetadata, ApplicationProfileId, HostProfileId, ProfileCatalog,
    StandardComposition,
};
use rsi_host::HostPaths;
use std::collections::BTreeMap;

fn metadata(plugin: &str, content: &[u8]) -> ApplicationCatalogMetadata {
    ApplicationCatalogMetadata::new(
        [plugin.to_owned()],
        [(
            ApplicationProfileId::new("fixture").unwrap(),
            content.to_vec(),
        )],
    )
    .unwrap()
}

#[test]
fn metadata_is_order_independent_and_rejects_ambiguous_input() {
    let a = ApplicationCatalogMetadata::new(["fixture.a".into(), "fixture.b".into()], []).unwrap();
    let b = ApplicationCatalogMetadata::new(["fixture.b".into(), "fixture.a".into()], []).unwrap();
    assert_eq!(a.digest(), b.digest());
    assert!(ApplicationCatalogMetadata::new(["fixture.a".into(), "fixture.a".into()], []).is_err());
    assert!(ApplicationCatalogMetadata::new(["invalid plugin".into()], []).is_err());
    assert!(
        ApplicationCatalogMetadata::new(
            [],
            [(
                ApplicationProfileId::new("fixture").unwrap(),
                b"not TOML".to_vec()
            )]
        )
        .is_err()
    );
}

#[test]
fn service_launch_identity_includes_profiles_and_reserved_ids() {
    let root = tempfile::tempdir().unwrap();
    let paths = HostPaths::new(
        root.path().join("config"),
        root.path().join("state"),
        root.path().join("cache"),
    )
    .unwrap();
    let key = |metadata: ApplicationCatalogMetadata| {
        let profile = ProfileCatalog::new(paths.clone(), metadata.clone())
            .host(&HostProfileId::new("standard").unwrap())
            .unwrap();
        StandardComposition::new(paths.clone(), BTreeMap::new(), None, metadata)
            .preview_host(&profile)
            .unwrap()
            .launch_key
    };
    let a = key(metadata("fixture.a", b"format = 1\n"));
    assert_eq!(a, key(metadata("fixture.a", b"format = 1\n")));
    assert_ne!(a, key(metadata("fixture.b", b"format = 1\n")));
    assert_ne!(a, key(metadata("fixture.a", b"format = 1\n# revised\n")));
    assert!(
        !paths.state().exists(),
        "metadata preview must not activate a Service"
    );
}

#[test]
fn service_catalog_lists_and_protects_explicit_builtins() {
    let root = tempfile::tempdir().unwrap();
    let paths = HostPaths::new(
        root.path().join("config"),
        root.path().join("state"),
        root.path().join("cache"),
    )
    .unwrap();
    let catalog = ProfileCatalog::new(paths, metadata("fixture.a", b"format = 1\n"));
    let id = ApplicationProfileId::new("fixture").unwrap();
    assert_eq!(catalog.list_applications().unwrap().len(), 1);
    assert_eq!(catalog.application(&id).unwrap().contents, b"format = 1\n");
    assert!(catalog.delete_application(&id).is_err());
    let path = catalog.application_path(&id);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, "format = 1\n").unwrap();
    assert!(catalog.application(&id).is_err());
    assert!(catalog.list_applications().is_err());
}

#[tokio::test]
async fn standard_preset_manager_preserves_the_service_launch_identity() {
    let root = tempfile::tempdir().unwrap();
    let paths = HostPaths::new(
        root.path().join("config"),
        root.path().join("state"),
        root.path().join("cache"),
    )
    .unwrap();
    let metadata = metadata("fixture.a", b"format = 1\n");
    let profile = ProfileCatalog::new(paths.clone(), metadata.clone())
        .host(&HostProfileId::new("standard").unwrap())
        .unwrap();
    let composition = StandardComposition::new(paths.clone(), BTreeMap::new(), None, metadata);
    let before = composition.preview_host(&profile).unwrap();
    let manager = rsi::AgentPresetManager::open_standard_preview(&composition)
        .await
        .unwrap();
    let with_manager = composition.clone().with_agent_presets(&manager).unwrap();
    assert_eq!(
        with_manager.preview_host(&profile).unwrap().launch_key,
        before.launch_key
    );
    let ids = manager
        .catalog()
        .launch_identity()
        .roots
        .into_iter()
        .filter_map(|root| root.exact_id)
        .map(|id| id.as_str().to_owned())
        .collect::<Vec<_>>();
    assert_eq!(ids, ["standard", "workflow", "automation"]);
    assert!(
        !paths.cache().exists(),
        "preview never materializes presets"
    );
    assert!(manager.shutdown().await.is_clean());
}

#[test]
fn base_metadata_matches_registered_application_factories() {
    let root = tempfile::tempdir().unwrap();
    let paths = HostPaths::new(
        root.path().join("config"),
        root.path().join("state"),
        root.path().join("cache"),
    )
    .unwrap();
    let catalog = rsi::base_application_catalog(StandardComposition::new(
        paths.clone(),
        BTreeMap::new(),
        None,
        ApplicationCatalogMetadata::new([], []).unwrap(),
    ))
    .unwrap();
    let actual: std::collections::BTreeSet<_> = catalog
        .addons
        .descriptions()
        .map(|description| description.plugin.as_str())
        .collect();
    let expected: std::collections::BTreeSet<_> =
        rsi::BASE_APPLICATION_PLUGINS.iter().copied().collect();
    assert_eq!(actual, expected);
    assert!(
        !paths.state().exists(),
        "enumeration must not activate factories"
    );
}

#[derive(Debug)]
struct BaseProvider(ApplicationCatalogMetadata);
impl rsi::ApplicationCatalogProvider for BaseProvider {
    fn metadata(&self) -> &ApplicationCatalogMetadata {
        &self.0
    }
    fn build(
        &self,
        service: &StandardComposition,
        _: Vec<std::ffi::OsString>,
    ) -> rsi::Result<rsi::ApplicationCatalog> {
        rsi::base_application_catalog(service.clone())
    }
}
#[test]
fn application_build_must_match_all_reserved_factory_ids_before_activation() {
    let root = tempfile::tempdir().unwrap();
    let paths = HostPaths::new(
        root.path().join("config"),
        root.path().join("state"),
        root.path().join("cache"),
    )
    .unwrap();
    for extra in [false, true] {
        let plugins = if extra {
            rsi::BASE_APPLICATION_PLUGINS
                .iter()
                .copied()
                .chain(["fixture.missing"])
                .map(str::to_owned)
                .collect()
        } else {
            Vec::new()
        };
        let metadata = ApplicationCatalogMetadata::new(plugins, []).unwrap();
        let service =
            StandardComposition::new(paths.clone(), BTreeMap::new(), None, metadata.clone());
        let composition = rsi::ApplicationComposition::new(
            service,
            std::sync::Arc::new(BaseProvider(metadata)),
            rsi::StandardAddonSet::default(),
        )
        .unwrap();
        assert!(
            rsi::standard_application_host(composition, vec![]).is_err(),
            "catalog reservation drift must fail closed"
        );
    }
    assert!(!paths.state().exists());
}
