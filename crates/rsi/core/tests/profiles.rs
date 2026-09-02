use rsi::{
    ApplicationKind, ApplicationProfileId, HostProfileDocument, HostProfileId, ProfileCatalog,
    ProfileCatalogError, ProfileSource, StandardCodingTools, StandardComposition,
};
use rsi_credentials_protocol::SecretValue;
use rsi_host::HostPaths;
use std::fs;

fn catalog(root: &std::path::Path) -> ProfileCatalog {
    ProfileCatalog::new(
        HostPaths::new(root.join("config"), root.join("state"), root.join("cache")).unwrap(),
    )
}

#[test]
fn builtins_are_explicit_strict_and_non_shadowable() {
    let temp = tempfile::tempdir().unwrap();
    let catalog = catalog(temp.path());
    let session = ApplicationProfileId::new("session").unwrap();
    let standard = HostProfileId::new("standard").unwrap();

    let application = catalog.application(&session).unwrap();
    assert_eq!(application.source, ProfileSource::Builtin);
    assert_eq!(application.profile.application(), ApplicationKind::Session);
    assert_eq!(application.profile.host_profile(), &standard);
    assert_eq!(catalog.host(&standard).unwrap().contents, b"format = 1\n");

    let shadow = catalog.application_path(&session);
    fs::create_dir_all(shadow.parent().unwrap()).unwrap();
    fs::write(
        &shadow,
        b"format = 1\napplication = 'headless'\nhost_profile = 'standard'\n",
    )
    .unwrap();
    assert!(matches!(
        catalog.application(&session),
        Err(ProfileCatalogError::BuiltinShadowed { id, .. }) if id == "session"
    ));
    assert!(catalog.list_applications().is_err());
}

#[test]
fn ids_and_application_documents_are_rejected_at_the_catalog_boundary() {
    for invalid in ["", "Upper", "-prefix", "contains_underscore", "slash/name"] {
        assert!(ApplicationProfileId::new(invalid).is_err(), "{invalid}");
    }
    assert!(ApplicationProfileId::new("a".repeat(255)).is_ok());
    assert!(ApplicationProfileId::new("a".repeat(256)).is_err());
    assert!(ApplicationProfileId::new("9-valid").is_ok());

    let temp = tempfile::tempdir().unwrap();
    let catalog = catalog(temp.path());
    let id = ApplicationProfileId::new("custom").unwrap();
    let path = catalog.application_path(&id);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(
        &path,
        b"format = 1\napplication = 'session'\nhost_profile = 'standard'\nunknown = true\n",
    )
    .unwrap();
    assert!(matches!(
        catalog.application(&id),
        Err(ProfileCatalogError::InvalidDocument { .. })
    ));

    fs::write(
        &path,
        b"format = 2\napplication = 'session'\nhost_profile = 'standard'\n",
    )
    .unwrap();
    assert!(matches!(
        catalog.application(&id),
        Err(ProfileCatalogError::UnsupportedFormat { observed: 2, .. })
    ));

    fs::write(
        &path,
        b"format = 3\napplication = 'session'\nhost_profile = 'standard'\n",
    )
    .unwrap();
    assert!(matches!(
        catalog.application(&id),
        Err(ProfileCatalogError::UnsupportedFormat { observed: 3, .. })
    ));

    fs::write(
        &path,
        b"format = 1\napplication = 'session'\nhost_profile = 'standard'\nprofile = 'legacy'\n",
    )
    .unwrap();
    assert!(matches!(
        catalog.application(&id),
        Err(ProfileCatalogError::InvalidDocument { .. })
    ));
}

#[test]
fn copy_list_and_delete_touch_only_the_owned_document_directory() {
    let temp = tempfile::tempdir().unwrap();
    let catalog = catalog(temp.path());
    let session = ApplicationProfileId::new("session").unwrap();
    let custom = ApplicationProfileId::new("my-session").unwrap();
    let path = catalog.copy_application(&session, &custom).unwrap();
    assert_eq!(path, catalog.application_path(&custom));
    assert_eq!(
        catalog.application(&custom).unwrap().profile.application(),
        ApplicationKind::Session
    );
    assert!(
        catalog
            .list_applications()
            .unwrap()
            .iter()
            .any(|row| row.id == custom && row.source == ProfileSource::User)
    );
    assert!(matches!(
        catalog.copy_application(&session, &custom),
        Err(ProfileCatalogError::AlreadyExists(_))
    ));

    fs::write(path.parent().unwrap().join("unowned"), b"keep").unwrap();
    assert!(matches!(
        catalog.delete_application(&custom),
        Err(ProfileCatalogError::DirectoryNotEmpty(_))
    ));
    fs::remove_file(path.parent().unwrap().join("unowned")).unwrap();
    catalog.delete_application(&custom).unwrap();
    assert!(!path.parent().unwrap().exists());
    assert!(catalog.delete_application(&session).is_err());
}

#[test]
fn host_copy_preserves_profile_source_for_later_pure_preview() {
    let temp = tempfile::tempdir().unwrap();
    let catalog = catalog(temp.path());
    let standard = HostProfileId::new("standard").unwrap();
    let custom = HostProfileId::new("my-host").unwrap();
    let path = catalog.copy_host(&standard, &custom).unwrap();
    assert_eq!(fs::read(&path).unwrap(), b"format = 1\n");
    let loaded = catalog.host(&custom).unwrap();
    assert_eq!(loaded.source, ProfileSource::User);
    assert_eq!(loaded.path.as_deref(), Some(path.as_path()));
    catalog.delete_host(&custom).unwrap();
}

#[cfg(unix)]
#[test]
fn profile_documents_are_not_read_through_symbolic_links() {
    use std::os::unix::fs::symlink;

    let temp = tempfile::tempdir().unwrap();
    let catalog = catalog(temp.path());
    let id = ApplicationProfileId::new("linked").unwrap();
    let path = catalog.application_path(&id);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    let target = temp.path().join("outside.toml");
    fs::write(
        &target,
        b"format = 1\napplication = 'session'\nhost_profile = 'standard'\n",
    )
    .unwrap();
    symlink(&target, &path).unwrap();
    assert!(
        catalog
            .list_applications()
            .unwrap()
            .iter()
            .all(|row| row.id != id)
    );
    assert!(matches!(
        catalog.application(&id),
        Err(ProfileCatalogError::Io {
            operation: "open",
            ..
        })
    ));
}

#[test]
fn host_preview_is_pure_and_launch_key_excludes_current_profile_contents() {
    let temp = tempfile::tempdir().unwrap();
    let catalog = catalog(temp.path());
    let standard = HostProfileId::new("standard").unwrap();
    let custom = HostProfileId::new("custom").unwrap();
    let path = catalog.copy_host(&standard, &custom).unwrap();
    let profile = catalog.host(&custom).unwrap();
    let composition = StandardComposition::new(
        catalog.paths().clone(),
        std::collections::BTreeMap::new(),
        None,
    );

    let first = composition.preview_host(&profile).unwrap();
    assert!(!catalog.paths().cache().exists());
    assert!(!catalog.paths().state().exists());
    fs::write(&path, b"format = 1\n# source generation two\n").unwrap();
    let changed = composition
        .preview_host(&catalog.host(&custom).unwrap())
        .unwrap();
    assert_eq!(first.launch_key, changed.launch_key);
    assert_ne!(first.profile.source_digest, changed.profile.source_digest);

    let builtin = composition
        .preview_host(&catalog.host(&standard).unwrap())
        .unwrap();
    assert_ne!(first.launch_key, builtin.launch_key);
}

#[test]
fn host_preview_rejects_a_profile_path_without_an_authority_root() {
    let temp = tempfile::tempdir().unwrap();
    let catalog = catalog(temp.path());
    let composition = StandardComposition::new(
        catalog.paths().clone(),
        std::collections::BTreeMap::new(),
        None,
    );
    let malformed = HostProfileDocument {
        id: HostProfileId::new("malformed").unwrap(),
        source: ProfileSource::User,
        path: Some(std::path::PathBuf::from("host.profile.toml")),
        contents: b"format = 1\n".to_vec(),
    };

    assert!(composition.preview_host(&malformed).is_err());

    let wrong_directory = temp
        .path()
        .join("not-host-profiles/malformed/host.profile.toml");
    std::fs::create_dir_all(wrong_directory.parent().unwrap()).unwrap();
    std::fs::write(&wrong_directory, b"format = 1\n").unwrap();
    let wrong_shape = HostProfileDocument {
        id: HostProfileId::new("malformed").unwrap(),
        source: ProfileSource::User,
        path: Some(wrong_directory),
        contents: b"format = 1\n".to_vec(),
    };
    assert!(composition.preview_host(&wrong_shape).is_err());
}

#[cfg(target_os = "linux")]
#[test]
fn host_launch_key_includes_owner_inputs_and_excludes_process_local_values() {
    let temp = tempfile::tempdir().unwrap();
    let primary_catalog = catalog(temp.path());
    let standard = primary_catalog
        .host(&HostProfileId::new("standard").unwrap())
        .unwrap();
    let secret_one = StandardComposition::new(
        primary_catalog.paths().clone(),
        std::collections::BTreeMap::from([(
            "RSI_OPENAI_API_KEY".into(),
            SecretValue::new("first-secret").unwrap(),
        )]),
        None,
    )
    .preview_host(&standard)
    .unwrap();
    let secret_two = StandardComposition::new(
        primary_catalog.paths().clone(),
        std::collections::BTreeMap::from([(
            "RSI_OPENAI_API_KEY".into(),
            SecretValue::new("second-secret").unwrap(),
        )]),
        None,
    )
    .preview_host(&standard)
    .unwrap();
    assert_eq!(secret_one.launch_key, secret_two.launch_key);

    let coding = |path: &str| {
        StandardCodingTools::new(
            fs::canonicalize("/bin/bash").unwrap(),
            std::env::current_exe().unwrap().canonicalize().unwrap(),
            vec![("PATH".into(), path.into())],
        )
        .unwrap()
    };
    let coding_one = StandardComposition::new(
        primary_catalog.paths().clone(),
        std::collections::BTreeMap::new(),
        Some(coding("/usr/bin:/bin")),
    )
    .preview_host(&standard)
    .unwrap();
    let coding_two = StandardComposition::new(
        primary_catalog.paths().clone(),
        std::collections::BTreeMap::new(),
        Some(coding("/bin")),
    )
    .preview_host(&standard)
    .unwrap();
    assert_ne!(secret_one.launch_key, coding_one.launch_key);
    assert_eq!(coding_one.launch_key, coding_two.launch_key);

    let other_catalog = catalog(&temp.path().join("other-owner"));
    let other_standard = other_catalog
        .host(&HostProfileId::new("standard").unwrap())
        .unwrap();
    let other_root = StandardComposition::new(
        other_catalog.paths().clone(),
        std::collections::BTreeMap::new(),
        None,
    )
    .preview_host(&other_standard)
    .unwrap();
    assert_ne!(secret_one.launch_key, other_root.launch_key);
}
