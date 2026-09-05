#[cfg(unix)]
use rsi_agent_presets::open_or_create_preset_root;
use rsi_agent_presets::{
    AgentPresetCatalog as PresetCatalog, AgentPresetCatalogConfig, AgentPresetDefaultStore,
    AgentPresetHealth, AgentPresetId, AgentPresetProfileCompiler, AgentPresetRoot,
    AgentPresetSource, AgentPresetTrust, COMPOSITION_FILE, MAX_COPY_BYTES, MAX_METADATA_BYTES,
    MAX_PROFILE_HEALTH_REASON_BYTES, MAX_ROOTS, MAX_ROSTER_ROWS, METADATA_FILE, PresetError,
};
use rsi_meta_profile::{ProfileCompiler, ProfileEnvironment, ProfileLimits};
use serde_json::Value;
use std::collections::BTreeMap;
use std::fs;
use std::path::Path;
use std::sync::{Arc, Mutex};

#[derive(Clone)]
struct AgentPresetCatalog(PresetCatalog);

impl AgentPresetCatalog {
    fn new(config: AgentPresetCatalogConfig) -> rsi_agent_presets::Result<Self> {
        PresetCatalog::new(config, test_compiler()).map(Self)
    }

    fn with_default_store(
        config: AgentPresetCatalogConfig,
        defaults: Arc<dyn AgentPresetDefaultStore>,
    ) -> rsi_agent_presets::Result<Self> {
        PresetCatalog::with_default_store(config, defaults, test_compiler()).map(Self)
    }
}

impl std::ops::Deref for AgentPresetCatalog {
    type Target = PresetCatalog;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

fn test_compiler() -> AgentPresetProfileCompiler {
    AgentPresetProfileCompiler::new(
        ProfileCompiler::new(
            ProfileEnvironment::new(
                "/test-config",
                "/test-state",
                "/test-cache",
                "test-platform",
                BTreeMap::from([("feature".to_owned(), Value::Bool(true))]),
            )
            .unwrap(),
            ProfileLimits::default(),
        ),
        ["test.child"],
    )
}

#[test]
fn preset_id_is_a_bounded_directory_segment() {
    assert_eq!(
        AgentPresetId::new("coding-agent").unwrap().as_str(),
        "coding-agent"
    );
    for invalid in ["", "Upper", "with space", "../escape", "a/b", "-leading"] {
        assert!(AgentPresetId::new(invalid).is_err(), "accepted {invalid:?}");
    }
    assert!(AgentPresetId::new(format!("a{}", "b".repeat(256))).is_err());
}

#[test]
fn catalog_freezes_only_absolute_unique_bounded_roots() {
    let temporary = tempfile::tempdir().unwrap();
    assert!(AgentPresetRoot::new("relative", AgentPresetTrust::System).is_err());

    let mut too_many = AgentPresetCatalogConfig::new(AgentPresetId::new("standard").unwrap());
    for index in 0..=MAX_ROOTS {
        too_many = too_many.with_configured_root(
            AgentPresetRoot::new(
                temporary.path().join(format!("root-{index}")),
                AgentPresetTrust::System,
            )
            .unwrap(),
        );
    }
    assert!(matches!(
        AgentPresetCatalog::new(too_many),
        Err(PresetError::TooManyRoots { maximum: MAX_ROOTS })
    ));

    let duplicate = temporary.path().join("duplicate");
    let duplicate = AgentPresetCatalogConfig::new(AgentPresetId::new("standard").unwrap())
        .with_system_root(&duplicate)
        .with_configured_root(AgentPresetRoot::new(&duplicate, AgentPresetTrust::User).unwrap());
    assert!(matches!(
        AgentPresetCatalog::new(duplicate),
        Err(PresetError::InvalidRoot(_))
    ));
}

#[tokio::test]
async fn multiple_system_roots_keep_injected_precedence_and_source() {
    let temporary = tempfile::tempdir().unwrap();
    let first = temporary.path().join("first");
    let second = temporary.path().join("second");
    fs::create_dir(&first).unwrap();
    fs::create_dir(&second).unwrap();
    write_preset(&first, "shared", Some("format = 1\n# first\n"), None);
    write_preset(&second, "shared", Some("format = 1\n# second\n"), None);
    write_preset(
        &second,
        "second-only",
        Some("format = 1\n# second-only\n"),
        None,
    );
    let catalog = AgentPresetCatalog::new(
        AgentPresetCatalogConfig::new(AgentPresetId::new("shared").unwrap())
            .with_system_root(&first)
            .with_system_root(&second),
    )
    .unwrap();

    let roster = catalog.roster().await.unwrap();
    assert_eq!(
        roster
            .presets
            .iter()
            .map(|row| (row.id.as_str(), row.source, row.trust))
            .collect::<Vec<_>>(),
        [
            (
                "shared",
                AgentPresetSource::System,
                AgentPresetTrust::System,
            ),
            (
                "second-only",
                AgentPresetSource::System,
                AgentPresetTrust::System,
            ),
        ]
    );
    assert_eq!(
        catalog
            .document(&AgentPresetId::new("shared").unwrap())
            .unwrap()
            .content,
        "format = 1\n# first\n"
    );
}

#[tokio::test]
async fn selected_preset_resolution_does_not_scan_unrelated_later_roots() {
    let temporary = tempfile::tempdir().unwrap();
    let first = temporary.path().join("first");
    let unrelated = temporary.path().join("unrelated-not-a-directory");
    fs::create_dir(&first).unwrap();
    fs::write(&unrelated, "not a preset root\n").unwrap();
    write_preset(
        &first,
        "selected",
        Some("format = 1\n# selected\n"),
        Some("name = \"Selected\"\n"),
    );
    let catalog = AgentPresetCatalog::new(
        AgentPresetCatalogConfig::new(AgentPresetId::new("selected").unwrap())
            .with_system_root(&first)
            .with_configured_root(
                AgentPresetRoot::new(&unrelated, AgentPresetTrust::System).unwrap(),
            ),
    )
    .unwrap();

    assert!(matches!(
        catalog.roster().await,
        Err(PresetError::InvalidRoot(_))
    ));
    assert_eq!(
        catalog
            .location(&AgentPresetId::new("selected").unwrap())
            .unwrap(),
        first.join("selected")
    );
    assert_eq!(
        catalog
            .document(&AgentPresetId::new("selected").unwrap())
            .unwrap()
            .content,
        "format = 1\n# selected\n"
    );
    assert!(
        catalog
            .compile(&AgentPresetId::new("selected").unwrap())
            .is_ok()
    );
}

#[tokio::test]
async fn roster_bounds_all_examined_directory_entries() {
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path().join("root");
    fs::create_dir(&root).unwrap();
    for index in 0..=MAX_ROSTER_ROWS {
        fs::write(root.join(format!(".ignored-{index}")), "residue\n").unwrap();
    }
    let catalog = AgentPresetCatalog::new(
        AgentPresetCatalogConfig::new(AgentPresetId::new("standard").unwrap())
            .with_system_root(&root),
    )
    .unwrap();

    assert!(matches!(
        catalog.roster().await,
        Err(PresetError::RosterCapacity {
            maximum: MAX_ROSTER_ROWS,
        })
    ));
}

fn write_preset(root: &Path, id: &str, composition: Option<&str>, metadata: Option<&str>) {
    let directory = root.join(id);
    fs::create_dir_all(&directory).unwrap();
    if let Some(composition) = composition {
        fs::write(directory.join(COMPOSITION_FILE), composition).unwrap();
    }
    if let Some(metadata) = metadata {
        fs::write(directory.join(METADATA_FILE), metadata).unwrap();
    }
}

#[tokio::test]
async fn roster_keeps_first_root_winners_and_visible_broken_rows() {
    let temporary = tempfile::tempdir().unwrap();
    let system = temporary.path().join("system");
    let configured = temporary.path().join("configured");
    let user = temporary.path().join("user");
    fs::create_dir_all(&system).unwrap();
    fs::create_dir_all(&configured).unwrap();
    fs::create_dir_all(&user).unwrap();
    write_preset(
        &system,
        "standard",
        Some("format = 1\n# system-composition\n"),
        Some("name = \"Standard\"\ndescription = \"Shipped preset\"\norder = 1\n"),
    );
    write_preset(
        &configured,
        "standard",
        Some("format = 1\n# shadowed-composition\n"),
        None,
    );
    write_preset(&configured, "broken", None, Some("name = \"Broken\"\n"));
    write_preset(
        &user,
        "mine",
        Some("format = 1\n# user-composition\n"),
        Some("not = [valid"),
    );

    let config = AgentPresetCatalogConfig::new(AgentPresetId::new("standard").unwrap())
        .with_system_root(&system)
        .with_configured_root(AgentPresetRoot::new(&configured, AgentPresetTrust::System).unwrap())
        .with_user_root(&user);
    let catalog = AgentPresetCatalog::new(config).unwrap();

    let roster = catalog.roster().await.unwrap();
    assert!(roster.authorable);
    assert_eq!(
        roster
            .presets
            .iter()
            .map(|row| row.id.as_str())
            .collect::<Vec<_>>(),
        ["standard", "broken", "mine"]
    );
    assert_eq!(roster.presets[0].trust, AgentPresetTrust::System);
    assert_eq!(roster.presets[0].source, AgentPresetSource::System);
    assert!(roster.presets[0].is_default);
    assert_eq!(roster.presets[0].name.as_deref(), Some("Standard"));
    assert_eq!(
        roster.presets[0].description.as_deref(),
        Some("Shipped preset")
    );
    assert_eq!(roster.presets[0].health, AgentPresetHealth::Healthy);
    assert!(matches!(
        roster.presets[1].health,
        AgentPresetHealth::Broken { .. }
    ));
    assert_eq!(roster.presets[1].source, AgentPresetSource::Configured);
    let AgentPresetHealth::Broken { reason } = &roster.presets[1].health else {
        unreachable!();
    };
    assert!(reason.contains(COMPOSITION_FILE));
    assert!(!reason.contains(temporary.path().to_string_lossy().as_ref()));
    assert_eq!(roster.presets[2].name, None);
    assert_eq!(roster.presets[2].source, AgentPresetSource::User);
    assert_eq!(roster.presets[2].health, AgentPresetHealth::Healthy);

    let document = catalog
        .document(&AgentPresetId::new("standard").unwrap())
        .unwrap();
    assert_eq!(document.content, "format = 1\n# system-composition\n");
    assert_eq!(document.trust, AgentPresetTrust::System);
    assert_eq!(document.source, AgentPresetSource::System);
    assert_eq!(
        catalog
            .location(&AgentPresetId::new("standard").unwrap())
            .unwrap(),
        system.join("standard")
    );
}

#[tokio::test]
async fn exact_system_preset_authority_never_discovers_sibling_directories() {
    let temporary = tempfile::tempdir().unwrap();
    let cache_root = temporary.path().join("cache-root");
    fs::create_dir(&cache_root).unwrap();
    write_preset(
        &cache_root,
        "standard",
        Some("format = 1\n"),
        Some("name = \"Verified\"\n"),
    );
    write_preset(
        &cache_root,
        "forged",
        Some("format = 1\n"),
        Some("name = \"Unverified\"\n"),
    );
    let standard = AgentPresetId::new("standard").unwrap();
    let catalog = AgentPresetCatalog::new(
        AgentPresetCatalogConfig::new(standard.clone())
            .with_system_preset(standard.clone(), cache_root.join("standard")),
    )
    .unwrap();

    let roster = catalog.roster().await.unwrap();
    assert_eq!(
        roster
            .presets
            .iter()
            .map(|row| row.id.as_str())
            .collect::<Vec<_>>(),
        ["standard"]
    );
    assert_eq!(
        catalog.location(&standard).unwrap(),
        cache_root.join("standard")
    );
    assert!(matches!(
        catalog.location(&AgentPresetId::new("forged").unwrap()),
        Err(PresetError::PresetNotFound { .. })
    ));
}

#[tokio::test]
async fn roster_keeps_invalid_profile_syntax_visible_but_broken() {
    let temporary = tempfile::tempdir().unwrap();
    let system = temporary.path().join("system");
    let fallback = temporary.path().join("fallback");
    fs::create_dir(&system).unwrap();
    fs::create_dir(&fallback).unwrap();
    write_preset(
        &system,
        "broken",
        Some("format = 1\nsecret-parser-input = [\"unterminated\"\n"),
        None,
    );
    write_preset(&fallback, "broken", Some("format = 1\n"), None);
    let catalog = AgentPresetCatalog::new(
        AgentPresetCatalogConfig::new(AgentPresetId::new("broken").unwrap())
            .with_system_root(&system)
            .with_system_root(&fallback),
    )
    .unwrap();

    let roster = catalog.roster().await.unwrap();
    assert_eq!(roster.presets.len(), 1);
    let AgentPresetHealth::Broken { reason } = &roster.presets[0].health else {
        panic!("invalid Profile syntax was reported healthy");
    };
    assert!(!reason.contains("secret-parser-input"));
    assert!(!reason.contains(temporary.path().to_string_lossy().as_ref()));
    assert!(matches!(
        catalog.document(&roster.presets[0].id),
        Err(PresetError::BrokenPreset { .. })
    ));
}

#[tokio::test]
async fn roster_preflights_semantics_includes_and_the_frozen_environment() {
    let temporary = tempfile::tempdir().unwrap();
    let system = temporary.path().join("system");
    fs::create_dir(&system).unwrap();
    write_preset(
        &system,
        "duplicate",
        Some(
            "format = 1\n\
             [[steps]]\nkind = \"plugin\"\nid = \"secret-duplicate\"\nplugin = \"one\"\n\
             [[steps]]\nkind = \"plugin\"\nid = \"secret-duplicate\"\nplugin = \"two\"\n",
        ),
        None,
    );
    write_preset(
        &system,
        "missing-include",
        Some("format = 1\n[[steps]]\nkind = \"include\"\npath = \"secret-missing-child.toml\"\n"),
        None,
    );
    write_preset(
        &system,
        "bad-environment",
        Some(
            "format = 1\n\
             [[steps]]\nkind = \"plugin\"\nid = \"environment\"\nplugin = \"test\"\n\
             enabled_rhai = \"defines.secret_missing_flag\"\n",
        ),
        None,
    );
    write_preset(
        &system,
        "unknown-contribution",
        Some(
            "format = 1\n\
             [[steps]]\nkind = \"plugin\"\nid = \"unknown\"\n\
             plugin = \"secret.unknown.contribution\"\n",
        ),
        None,
    );
    write_preset(
        &system,
        "healthy",
        Some("format = 1\n[[steps]]\nkind = \"include\"\npath = \"child.toml\"\n"),
        None,
    );
    fs::write(
        system.join("healthy/child.toml"),
        "format = 1\n\
         [[steps]]\nkind = \"plugin\"\nid = \"child\"\nplugin = \"test.child\"\n\
         enabled_rhai = \"defines.feature\"\n",
    )
    .unwrap();
    let catalog = AgentPresetCatalog::new(
        AgentPresetCatalogConfig::new(AgentPresetId::new("healthy").unwrap())
            .with_system_root(&system),
    )
    .unwrap();

    let roster = catalog.roster().await.unwrap();
    assert_eq!(roster.presets.len(), 5);
    for id in [
        "duplicate",
        "missing-include",
        "bad-environment",
        "unknown-contribution",
    ] {
        let row = roster
            .presets
            .iter()
            .find(|row| row.id.as_str() == id)
            .unwrap();
        let AgentPresetHealth::Broken { reason } = &row.health else {
            panic!("{id} was unexpectedly healthy");
        };
        assert!(reason.len() <= MAX_PROFILE_HEALTH_REASON_BYTES);
        assert!(!reason.contains("secret"));
        assert!(!reason.contains(temporary.path().to_string_lossy().as_ref()));
        assert!(matches!(
            catalog.document(&row.id),
            Err(PresetError::BrokenPreset {
                id: broken_id,
                reason: document_reason,
            }) if broken_id == id && document_reason == *reason
        ));
    }
    let healthy = roster
        .presets
        .iter()
        .find(|row| row.id.as_str() == "healthy")
        .unwrap();
    assert_eq!(healthy.health, AgentPresetHealth::Healthy);
    let candidate = catalog.compile(&healthy.id).unwrap();
    assert_eq!(candidate.leaves().len(), 1);
    assert_eq!(candidate.leaves()[0].id().as_str(), "child");
    assert_eq!(candidate.watch_paths().len(), 2);
}

#[tokio::test]
async fn catalog_bounds_optional_metadata_without_degrading_composition_health() {
    let temporary = tempfile::tempdir().unwrap();
    let system = temporary.path().join("system");
    let user = temporary.path().join("user");
    fs::create_dir(&system).unwrap();
    fs::create_dir(&user).unwrap();
    write_preset(
        &system,
        "standard",
        Some("format = 1\n# composition\n"),
        Some("name = \"ignored\"\n"),
    );
    fs::OpenOptions::new()
        .write(true)
        .open(system.join("standard").join(METADATA_FILE))
        .unwrap()
        .set_len(u64::try_from(MAX_METADATA_BYTES + 1).unwrap())
        .unwrap();
    let catalog = AgentPresetCatalog::new(
        AgentPresetCatalogConfig::new(AgentPresetId::new("standard").unwrap())
            .with_system_root(&system)
            .with_user_root(&user),
    )
    .unwrap();

    let row = catalog.roster().await.unwrap().presets.remove(0);
    assert_eq!(row.health, AgentPresetHealth::Healthy);
    assert_eq!(row.name, None);
    assert_eq!(row.description, None);

    assert!(matches!(
        catalog
            .copy(
                &AgentPresetId::new("standard").unwrap(),
                AgentPresetId::new("mine").unwrap(),
                Some("x".repeat(MAX_METADATA_BYTES)),
            )
            .await,
        Err(PresetError::CopyLimit {
            resource: "metadata bytes",
            maximum,
        }) if maximum == u64::try_from(MAX_METADATA_BYTES).unwrap()
    ));
    assert!(user.read_dir().unwrap().next().is_none());
}

#[tokio::test]
async fn roster_drops_multiline_and_terminal_control_metadata_at_the_catalog_boundary() {
    let temporary = tempfile::tempdir().unwrap();
    let system = temporary.path().join("system");
    fs::create_dir(&system).unwrap();
    write_preset(
        &system,
        "standard",
        Some("format = 1\n"),
        Some("name = \"safe\\nforged-row\"\ndescription = \"safe\\u001b[31mred\"\n"),
    );
    let catalog = AgentPresetCatalog::new(
        AgentPresetCatalogConfig::new(AgentPresetId::new("standard").unwrap())
            .with_system_root(&system),
    )
    .unwrap();

    let row = catalog.roster().await.unwrap().presets.remove(0);
    assert_eq!(row.name, None);
    assert_eq!(row.description, None);
    let document = catalog.document(&row.id).unwrap();
    assert_eq!(document.name, None);
    assert_eq!(document.description, None);
}

#[tokio::test]
async fn catalog_surfaces_non_utf8_and_oversized_compositions_as_broken_rows() {
    let temporary = tempfile::tempdir().unwrap();
    let system = temporary.path().join("system");
    fs::create_dir(&system).unwrap();
    write_preset(&system, "invalid-utf8", None, None);
    fs::write(system.join("invalid-utf8").join(COMPOSITION_FILE), [0xff]).unwrap();
    write_preset(&system, "oversized", None, None);
    fs::File::create(system.join("oversized").join(COMPOSITION_FILE))
        .unwrap()
        .set_len(MAX_COPY_BYTES + 1)
        .unwrap();
    let catalog = AgentPresetCatalog::new(
        AgentPresetCatalogConfig::new(AgentPresetId::new("invalid-utf8").unwrap())
            .with_system_root(&system),
    )
    .unwrap();

    let roster = catalog.roster().await.unwrap();
    assert_eq!(roster.presets.len(), 2);
    for row in &roster.presets {
        let AgentPresetHealth::Broken { reason } = &row.health else {
            panic!("{} was unexpectedly healthy", row.id.as_str());
        };
        assert!(reason.contains("Agent Profile"));
        assert!(reason.len() <= MAX_PROFILE_HEALTH_REASON_BYTES);
        assert!(!reason.contains(temporary.path().to_string_lossy().as_ref()));
        assert!(matches!(
            catalog.document(&row.id),
            Err(PresetError::BrokenPreset { .. })
        ));
    }
}

#[cfg(unix)]
#[tokio::test]
async fn catalog_does_not_follow_a_composition_symlink() {
    use std::os::unix::fs::symlink;

    let temporary = tempfile::tempdir().unwrap();
    let system = temporary.path().join("system");
    fs::create_dir(&system).unwrap();
    write_preset(&system, "linked", None, None);
    let outside = temporary.path().join("outside.toml");
    fs::write(&outside, "outside\n").unwrap();
    symlink(&outside, system.join("linked").join(COMPOSITION_FILE)).unwrap();
    let catalog = AgentPresetCatalog::new(
        AgentPresetCatalogConfig::new(AgentPresetId::new("linked").unwrap())
            .with_system_root(&system),
    )
    .unwrap();

    let roster = catalog.roster().await.unwrap();
    assert!(matches!(
        roster.presets[0].health,
        AgentPresetHealth::Broken { .. }
    ));
    assert!(matches!(
        catalog.document(&AgentPresetId::new("linked").unwrap()),
        Err(PresetError::BrokenPreset { .. })
    ));
}

#[derive(Debug, Default)]
struct TestDefaults {
    selected: Mutex<Option<AgentPresetId>>,
}

#[async_trait::async_trait]
impl AgentPresetDefaultStore for TestDefaults {
    async fn load(&self) -> rsi_agent_presets::Result<Option<AgentPresetId>> {
        Ok(self.selected.lock().unwrap().clone())
    }

    async fn replace(&self, selected: Option<AgentPresetId>) -> rsi_agent_presets::Result<()> {
        *self.selected.lock().unwrap() = selected;
        Ok(())
    }
}

#[tokio::test]
async fn user_default_layers_over_base_without_prevalidating_generation_availability() {
    let temporary = tempfile::tempdir().unwrap();
    let system = temporary.path().join("system");
    fs::create_dir(&system).unwrap();
    write_preset(&system, "standard", Some("standard\n"), None);
    write_preset(&system, "minimal", Some("minimal\n"), None);
    write_preset(&system, "broken", None, None);
    let defaults = Arc::new(TestDefaults::default());
    let catalog = AgentPresetCatalog::with_default_store(
        AgentPresetCatalogConfig::new(AgentPresetId::new("standard").unwrap())
            .with_system_root(&system),
        defaults.clone(),
    )
    .unwrap();

    assert_eq!(catalog.default_id().await.unwrap().as_str(), "standard");
    catalog
        .set_default(&AgentPresetId::new("minimal").unwrap())
        .await
        .unwrap();
    assert_eq!(catalog.default_id().await.unwrap().as_str(), "minimal");
    assert_eq!(
        catalog
            .roster()
            .await
            .unwrap()
            .presets
            .iter()
            .find(|row| row.is_default)
            .unwrap()
            .id
            .as_str(),
        "minimal"
    );

    catalog
        .set_default(&AgentPresetId::new("broken").unwrap())
        .await
        .unwrap();
    assert_eq!(catalog.default_id().await.unwrap().as_str(), "broken");
    catalog
        .set_default(&AgentPresetId::new("unknown").unwrap())
        .await
        .unwrap();
    assert_eq!(catalog.default_id().await.unwrap().as_str(), "unknown");
    assert!(
        catalog
            .roster()
            .await
            .unwrap()
            .presets
            .iter()
            .all(|row| !row.is_default)
    );

    catalog.clear_default().await.unwrap();
    assert_eq!(catalog.default_id().await.unwrap().as_str(), "standard");
}

#[tokio::test]
async fn catalog_without_a_default_store_never_reports_a_discarded_write() {
    let temporary = tempfile::tempdir().unwrap();
    let system = temporary.path().join("system");
    fs::create_dir(&system).unwrap();
    write_preset(&system, "standard", Some("standard\n"), None);
    write_preset(&system, "minimal", Some("minimal\n"), None);
    let catalog = AgentPresetCatalog::new(
        AgentPresetCatalogConfig::new(AgentPresetId::new("standard").unwrap())
            .with_system_root(&system),
    )
    .unwrap();

    assert_eq!(
        catalog
            .set_default(&AgentPresetId::new("minimal").unwrap())
            .await,
        Err(PresetError::DefaultStoreUnavailable)
    );
    assert_eq!(catalog.default_id().await.unwrap().as_str(), "standard");
    catalog.clear_default().await.unwrap();
}

#[tokio::test]
async fn copy_publishes_one_self_contained_user_preset() {
    let temporary = tempfile::tempdir().unwrap();
    let system = temporary.path().join("system");
    let user = temporary.path().join("user");
    fs::create_dir(&system).unwrap();
    fs::create_dir(&user).unwrap();
    write_preset(
        &system,
        "standard",
        Some("format = 1\n# source\n"),
        Some("name = \"Standard\"\ndescription = \"Base description\"\norder = 1\n"),
    );
    let assets = system.join("standard/assets");
    fs::create_dir(&assets).unwrap();
    fs::write(assets.join("instructions.txt"), "copied asset\n").unwrap();
    let catalog = AgentPresetCatalog::new(
        AgentPresetCatalogConfig::new(AgentPresetId::new("standard").unwrap())
            .with_system_root(&system)
            .with_user_root(&user),
    )
    .unwrap();

    catalog
        .copy(
            &AgentPresetId::new("standard").unwrap(),
            AgentPresetId::new("mine").unwrap(),
            Some("My preset".into()),
        )
        .await
        .unwrap();

    let document = catalog
        .document(&AgentPresetId::new("mine").unwrap())
        .unwrap();
    assert_eq!(document.content, "format = 1\n# source\n");
    assert_eq!(document.trust, AgentPresetTrust::User);
    assert_eq!(document.name.as_deref(), Some("My preset"));
    assert_eq!(document.description.as_deref(), Some("Base description"));
    assert_eq!(
        fs::read_to_string(user.join("mine/assets/instructions.txt")).unwrap(),
        "copied asset\n"
    );
    let metadata = fs::read_to_string(user.join("mine").join(METADATA_FILE)).unwrap();
    let metadata = toml::from_str::<toml::Value>(&metadata).unwrap();
    assert_eq!(metadata["name"].as_str(), Some("My preset"));
    assert_eq!(metadata["description"].as_str(), Some("Base description"));
    assert_eq!(metadata["order"].as_integer(), Some(1));
    assert!(!user.read_dir().unwrap().any(|entry| {
        entry
            .unwrap()
            .file_name()
            .to_string_lossy()
            .starts_with('.')
    }));
}

#[tokio::test]
async fn copy_without_a_name_override_preserves_valid_metadata_bytes_exactly() {
    let temporary = tempfile::tempdir().unwrap();
    let system = temporary.path().join("system");
    let user = temporary.path().join("user");
    fs::create_dir(&system).unwrap();
    fs::create_dir(&user).unwrap();
    let metadata = concat!(
        "# deployment presentation\n",
        "order = 7\n",
        "description = \"Preserve this description\"\n",
        "name = \"Preserve this name\"\n",
    );
    write_preset(&system, "standard", Some("format = 1\n"), Some(metadata));
    let catalog = AgentPresetCatalog::new(
        AgentPresetCatalogConfig::new(AgentPresetId::new("standard").unwrap())
            .with_system_root(&system)
            .with_user_root(&user),
    )
    .unwrap();

    catalog
        .copy(
            &AgentPresetId::new("standard").unwrap(),
            AgentPresetId::new("mine").unwrap(),
            None,
        )
        .await
        .unwrap();

    assert_eq!(
        fs::read(user.join("mine").join(METADATA_FILE)).unwrap(),
        metadata.as_bytes()
    );
    let document = catalog
        .document(&AgentPresetId::new("mine").unwrap())
        .unwrap();
    assert_eq!(document.name.as_deref(), Some("Preserve this name"));
    assert_eq!(
        document.description.as_deref(),
        Some("Preserve this description")
    );
}

#[tokio::test]
async fn copy_without_a_name_override_preserves_bounded_opaque_metadata_bytes() {
    let temporary = tempfile::tempdir().unwrap();
    let system = temporary.path().join("system");
    let user = temporary.path().join("user");
    fs::create_dir(&system).unwrap();
    fs::create_dir(&user).unwrap();
    write_preset(&system, "standard", Some("format = 1\n"), None);
    let metadata = b"unknown = true\nmalformed = [\n\xff";
    fs::write(system.join("standard").join(METADATA_FILE), metadata).unwrap();
    let catalog = AgentPresetCatalog::new(
        AgentPresetCatalogConfig::new(AgentPresetId::new("standard").unwrap())
            .with_system_root(&system)
            .with_user_root(&user),
    )
    .unwrap();

    catalog
        .copy(
            &AgentPresetId::new("standard").unwrap(),
            AgentPresetId::new("mine").unwrap(),
            None,
        )
        .await
        .unwrap();

    assert_eq!(
        fs::read(user.join("mine").join(METADATA_FILE)).unwrap(),
        metadata
    );
}

#[tokio::test]
async fn copy_name_override_rejects_invalid_metadata_without_publication() {
    let temporary = tempfile::tempdir().unwrap();
    let system = temporary.path().join("system");
    let user = temporary.path().join("user");
    fs::create_dir(&system).unwrap();
    fs::create_dir(&user).unwrap();
    write_preset(
        &system,
        "standard",
        Some("format = 1\n"),
        Some("unknown = true\n"),
    );
    let catalog = AgentPresetCatalog::new(
        AgentPresetCatalogConfig::new(AgentPresetId::new("standard").unwrap())
            .with_system_root(&system)
            .with_user_root(&user),
    )
    .unwrap();

    assert!(matches!(
        catalog
            .copy(
                &AgentPresetId::new("standard").unwrap(),
                AgentPresetId::new("mine").unwrap(),
                Some("Mine".into()),
            )
            .await,
        Err(PresetError::UnsafeEntry { .. })
    ));
    assert!(!user.join("mine").exists());
    assert!(!user.read_dir().unwrap().any(|entry| {
        entry
            .unwrap()
            .file_name()
            .to_string_lossy()
            .starts_with('.')
    }));
}

#[tokio::test]
async fn copy_rejects_metadata_above_its_independent_byte_bound() {
    let temporary = tempfile::tempdir().unwrap();
    let system = temporary.path().join("system");
    let user = temporary.path().join("user");
    fs::create_dir(&system).unwrap();
    fs::create_dir(&user).unwrap();
    write_preset(&system, "standard", Some("format = 1\n"), None);
    fs::write(
        system.join("standard").join(METADATA_FILE),
        vec![b'x'; MAX_METADATA_BYTES + 1],
    )
    .unwrap();
    let catalog = AgentPresetCatalog::new(
        AgentPresetCatalogConfig::new(AgentPresetId::new("standard").unwrap())
            .with_system_root(&system)
            .with_user_root(&user),
    )
    .unwrap();

    assert!(matches!(
        catalog
            .copy(
                &AgentPresetId::new("standard").unwrap(),
                AgentPresetId::new("mine").unwrap(),
                None,
            )
            .await,
        Err(PresetError::CopyLimit {
            resource: "metadata bytes",
            maximum,
        }) if maximum == MAX_METADATA_BYTES as u64
    ));
    assert!(!user.join("mine").exists());
}

#[cfg(unix)]
#[tokio::test]
async fn copy_creates_only_the_absent_user_root_with_owner_only_mode() {
    use std::os::unix::fs::PermissionsExt as _;

    let temporary = tempfile::tempdir().unwrap();
    let system = temporary.path().join("system");
    let user = temporary.path().join("nested/presets/user");
    fs::create_dir(&system).unwrap();
    write_preset(&system, "standard", Some("composition\n"), None);
    let catalog = AgentPresetCatalog::new(
        AgentPresetCatalogConfig::new(AgentPresetId::new("standard").unwrap())
            .with_system_root(&system)
            .with_user_root(&user),
    )
    .unwrap();

    catalog
        .copy(
            &AgentPresetId::new("standard").unwrap(),
            AgentPresetId::new("mine").unwrap(),
            None,
        )
        .await
        .unwrap();

    for created in [
        temporary.path().join("nested"),
        temporary.path().join("nested/presets"),
        user.clone(),
    ] {
        assert_eq!(
            fs::metadata(created).unwrap().permissions().mode() & 0o777,
            0o700
        );
    }
    assert!(user.join("mine").is_dir());
}

#[cfg(unix)]
#[test]
fn owned_preset_root_rejects_a_symlink_below_the_root_alias_boundary() {
    use std::os::unix::fs::symlink;

    let temporary = tempfile::tempdir().unwrap();
    let outside = temporary.path().join("outside");
    let alias = temporary.path().join("alias");
    fs::create_dir(&outside).unwrap();
    symlink(&outside, &alias).unwrap();

    let logical = alias.join("owned");
    let error = open_or_create_preset_root(&logical).unwrap_err();
    assert!(matches!(error, PresetError::UnsafeEntry { path, .. } if path == logical));
    assert!(!outside.join("owned").exists());
}

#[cfg(target_os = "macos")]
#[test]
fn owned_preset_root_accepts_the_platform_var_alias() {
    let temporary = tempfile::Builder::new()
        .prefix("rsi-preset-root-")
        .tempdir_in("/var/tmp")
        .unwrap();
    let logical = temporary.path().join("nested/presets");

    let _root = open_or_create_preset_root(&logical).unwrap();
    assert!(logical.is_dir());
}

#[cfg(unix)]
#[tokio::test]
async fn copy_sets_private_modes_without_chmodding_an_existing_user_root() {
    use std::os::unix::fs::PermissionsExt as _;

    let temporary = tempfile::tempdir().unwrap();
    let system = temporary.path().join("system");
    let user = temporary.path().join("user");
    fs::create_dir(&system).unwrap();
    fs::create_dir(&user).unwrap();
    fs::set_permissions(&user, fs::Permissions::from_mode(0o750)).unwrap();
    write_preset(
        &system,
        "standard",
        Some("composition\n"),
        Some("description = \"description\"\n"),
    );
    fs::create_dir(system.join("standard/bin")).unwrap();
    let executable = system.join("standard/bin/run");
    fs::write(&executable, "#!/bin/sh\n").unwrap();
    fs::set_permissions(&executable, fs::Permissions::from_mode(0o755)).unwrap();
    let catalog = AgentPresetCatalog::new(
        AgentPresetCatalogConfig::new(AgentPresetId::new("standard").unwrap())
            .with_system_root(&system)
            .with_user_root(&user),
    )
    .unwrap();

    catalog
        .copy(
            &AgentPresetId::new("standard").unwrap(),
            AgentPresetId::new("mine").unwrap(),
            None,
        )
        .await
        .unwrap();

    let mode = |path: &Path| fs::metadata(path).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode(&user), 0o750);
    assert_eq!(mode(&user.join("mine")), 0o700);
    assert_eq!(mode(&user.join("mine/bin")), 0o700);
    assert_eq!(mode(&user.join("mine").join(COMPOSITION_FILE)), 0o600);
    assert_eq!(mode(&user.join("mine/bin/run")), 0o700);
    assert_eq!(mode(&user.join("mine").join(METADATA_FILE)), 0o600);
}

#[tokio::test]
async fn copy_never_overwrites_catalog_or_disk_only_occupants() {
    let temporary = tempfile::tempdir().unwrap();
    let system = temporary.path().join("system");
    let user = temporary.path().join("user");
    fs::create_dir(&system).unwrap();
    fs::create_dir(&user).unwrap();
    write_preset(&system, "standard", Some("format = 1\n# source\n"), None);
    write_preset(&system, "reserved", Some("format = 1\n# reserved\n"), None);
    fs::write(user.join("occupied"), "do not replace\n").unwrap();
    let catalog = AgentPresetCatalog::new(
        AgentPresetCatalogConfig::new(AgentPresetId::new("standard").unwrap())
            .with_system_root(&system)
            .with_user_root(&user),
    )
    .unwrap();

    for target in ["reserved", "occupied"] {
        assert!(matches!(
            catalog
                .copy(
                    &AgentPresetId::new("standard").unwrap(),
                    AgentPresetId::new(target).unwrap(),
                    None,
                )
                .await,
            Err(PresetError::PresetExists { .. })
        ));
    }
    assert_eq!(
        catalog
            .document(&AgentPresetId::new("reserved").unwrap())
            .unwrap()
            .content,
        "format = 1\n# reserved\n"
    );
    assert_eq!(
        fs::read_to_string(user.join("occupied")).unwrap(),
        "do not replace\n"
    );
    assert_eq!(user.read_dir().unwrap().count(), 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn independent_catalogs_publish_the_same_id_without_clobbering() {
    let temporary = tempfile::tempdir().unwrap();
    let system = temporary.path().join("system");
    let user = temporary.path().join("user");
    fs::create_dir(&system).unwrap();
    fs::create_dir(&user).unwrap();
    write_preset(&system, "standard", Some("source\n"), None);
    let config = AgentPresetCatalogConfig::new(AgentPresetId::new("standard").unwrap())
        .with_system_root(&system)
        .with_user_root(&user);
    let first = AgentPresetCatalog::new(config.clone()).unwrap();
    let second = AgentPresetCatalog::new(config).unwrap();
    let copy = |catalog: AgentPresetCatalog| async move {
        catalog
            .copy(
                &AgentPresetId::new("standard").unwrap(),
                AgentPresetId::new("mine").unwrap(),
                None,
            )
            .await
    };

    let first = tokio::spawn(copy(first));
    let second = tokio::spawn(copy(second));
    let results = [first.await.unwrap(), second.await.unwrap()];
    assert_eq!(results.iter().filter(|result| result.is_ok()).count(), 1);
    assert_eq!(
        results
            .iter()
            .filter(|result| matches!(result, Err(PresetError::PresetExists { .. })))
            .count(),
        1
    );
    assert_eq!(
        fs::read_to_string(user.join("mine").join(COMPOSITION_FILE)).unwrap(),
        "source\n"
    );
    assert_eq!(user.read_dir().unwrap().count(), 1);
}

#[cfg(unix)]
#[tokio::test]
async fn copy_materializes_contained_symlinks_as_private_owned_content() {
    use std::os::unix::fs::symlink;

    let temporary = tempfile::tempdir().unwrap();
    let system = temporary.path().join("system");
    let user = temporary.path().join("user");
    fs::create_dir(&system).unwrap();
    fs::create_dir(&user).unwrap();
    write_preset(&system, "standard", Some("composition\n"), None);
    fs::create_dir(system.join("standard/assets")).unwrap();
    fs::write(system.join("standard/assets/instructions"), "contained\n").unwrap();
    symlink("assets/instructions", system.join("standard/linked-file")).unwrap();
    symlink("assets", system.join("standard/linked-directory")).unwrap();
    let catalog = AgentPresetCatalog::new(
        AgentPresetCatalogConfig::new(AgentPresetId::new("standard").unwrap())
            .with_system_root(&system)
            .with_user_root(&user),
    )
    .unwrap();

    catalog
        .copy(
            &AgentPresetId::new("standard").unwrap(),
            AgentPresetId::new("mine").unwrap(),
            None,
        )
        .await
        .unwrap();

    assert_eq!(
        fs::read_to_string(user.join("mine/linked-file")).unwrap(),
        "contained\n"
    );
    assert_eq!(
        fs::read_to_string(user.join("mine/linked-directory/instructions")).unwrap(),
        "contained\n"
    );
    assert!(
        !fs::symlink_metadata(user.join("mine/linked-file"))
            .unwrap()
            .file_type()
            .is_symlink()
    );
    assert!(
        fs::symlink_metadata(user.join("mine/linked-directory"))
            .unwrap()
            .file_type()
            .is_dir()
    );
}

#[cfg(unix)]
#[tokio::test]
async fn copy_rejects_absolute_escape_and_cyclic_symlinks_without_publication() {
    use std::os::unix::fs::symlink;

    let temporary = tempfile::tempdir().unwrap();
    let system = temporary.path().join("system");
    let user = temporary.path().join("user");
    fs::create_dir(&system).unwrap();
    fs::create_dir(&user).unwrap();
    let victim = temporary.path().join("victim");
    fs::write(&victim, "outside\n").unwrap();
    for source in ["absolute", "escape", "cycle"] {
        write_preset(&system, source, Some("composition\n"), None);
    }
    symlink(&victim, system.join("absolute/linked")).unwrap();
    symlink("../escape-target", system.join("escape/linked")).unwrap();
    symlink("second", system.join("cycle/first")).unwrap();
    symlink("first", system.join("cycle/second")).unwrap();
    let catalog = AgentPresetCatalog::new(
        AgentPresetCatalogConfig::new(AgentPresetId::new("absolute").unwrap())
            .with_system_root(&system)
            .with_user_root(&user),
    )
    .unwrap();

    for (source, target) in [
        ("absolute", "absolute-copy"),
        ("escape", "escape-copy"),
        ("cycle", "cycle-copy"),
    ] {
        assert!(matches!(
            catalog
                .copy(
                    &AgentPresetId::new(source).unwrap(),
                    AgentPresetId::new(target).unwrap(),
                    None,
                )
                .await,
            Err(PresetError::UnsafeEntry { .. })
        ));
    }
    assert_eq!(fs::read_to_string(victim).unwrap(), "outside\n");
    assert!(user.read_dir().unwrap().next().is_none());
}

#[cfg(unix)]
#[tokio::test]
async fn copy_charges_symlink_traversal_against_the_entry_bound() {
    use std::os::unix::fs::symlink;

    let temporary = tempfile::tempdir().unwrap();
    let system = temporary.path().join("system");
    let user = temporary.path().join("user");
    fs::create_dir(&system).unwrap();
    fs::create_dir(&user).unwrap();
    write_preset(&system, "standard", Some("composition\n"), None);
    fs::write(system.join("standard/target"), "target\n").unwrap();
    for index in 0..128 {
        symlink(
            "target",
            system.join("standard").join(format!("link-{index}")),
        )
        .unwrap();
    }
    let catalog = AgentPresetCatalog::new(
        AgentPresetCatalogConfig::new(AgentPresetId::new("standard").unwrap())
            .with_system_root(&system)
            .with_user_root(&user),
    )
    .unwrap();

    assert!(matches!(
        catalog
            .copy(
                &AgentPresetId::new("standard").unwrap(),
                AgentPresetId::new("mine").unwrap(),
                None,
            )
            .await,
        Err(PresetError::CopyLimit {
            resource: "filesystem entries",
            ..
        })
    ));
    assert!(user.read_dir().unwrap().next().is_none());

    fs::remove_file(system.join("standard/link-127")).unwrap();
    catalog
        .copy(
            &AgentPresetId::new("standard").unwrap(),
            AgentPresetId::new("within-bound").unwrap(),
            None,
        )
        .await
        .unwrap();
    assert_eq!(
        fs::read_to_string(user.join("within-bound/link-126")).unwrap(),
        "target\n"
    );
}

#[tokio::test]
async fn copy_bounds_every_traversed_entry_including_empty_directories() {
    let temporary = tempfile::tempdir().unwrap();
    let system = temporary.path().join("system");
    let user = temporary.path().join("user");
    fs::create_dir(&system).unwrap();
    fs::create_dir(&user).unwrap();
    write_preset(&system, "standard", Some("composition\n"), None);
    for index in 0..256 {
        fs::create_dir(system.join("standard").join(format!("empty-{index}"))).unwrap();
    }
    let catalog = AgentPresetCatalog::new(
        AgentPresetCatalogConfig::new(AgentPresetId::new("standard").unwrap())
            .with_system_root(&system)
            .with_user_root(&user),
    )
    .unwrap();

    assert!(matches!(
        catalog
            .copy(
                &AgentPresetId::new("standard").unwrap(),
                AgentPresetId::new("mine").unwrap(),
                None,
            )
            .await,
        Err(PresetError::CopyLimit {
            resource: "filesystem entries",
            ..
        })
    ));
    assert!(user.read_dir().unwrap().next().is_none());
}

#[tokio::test]
async fn copy_bounds_aggregate_bytes_before_publication() {
    let temporary = tempfile::tempdir().unwrap();
    let system = temporary.path().join("system");
    let user = temporary.path().join("user");
    fs::create_dir(&system).unwrap();
    fs::create_dir(&user).unwrap();
    write_preset(&system, "standard", Some("composition\n"), None);
    fs::File::create(system.join("standard/large"))
        .unwrap()
        .set_len(MAX_COPY_BYTES + 1)
        .unwrap();
    let catalog = AgentPresetCatalog::new(
        AgentPresetCatalogConfig::new(AgentPresetId::new("standard").unwrap())
            .with_system_root(&system)
            .with_user_root(&user),
    )
    .unwrap();

    assert!(matches!(
        catalog
            .copy(
                &AgentPresetId::new("standard").unwrap(),
                AgentPresetId::new("mine").unwrap(),
                None,
            )
            .await,
        Err(PresetError::CopyLimit {
            resource: "aggregate bytes",
            maximum: MAX_COPY_BYTES,
        })
    ));
    assert!(user.read_dir().unwrap().next().is_none());
}

#[tokio::test]
async fn copy_bounds_directory_depth_before_publication() {
    let temporary = tempfile::tempdir().unwrap();
    let system = temporary.path().join("system");
    let user = temporary.path().join("user");
    fs::create_dir(&system).unwrap();
    fs::create_dir(&user).unwrap();
    write_preset(&system, "standard", Some("composition\n"), None);
    let mut directory = system.join("standard");
    for _ in 0..33 {
        directory = directory.join("nested");
        fs::create_dir(&directory).unwrap();
    }
    let catalog = AgentPresetCatalog::new(
        AgentPresetCatalogConfig::new(AgentPresetId::new("standard").unwrap())
            .with_system_root(&system)
            .with_user_root(&user),
    )
    .unwrap();

    assert!(matches!(
        catalog
            .copy(
                &AgentPresetId::new("standard").unwrap(),
                AgentPresetId::new("mine").unwrap(),
                None,
            )
            .await,
        Err(PresetError::CopyLimit {
            resource: "directory depth",
            ..
        })
    ));
    assert!(user.read_dir().unwrap().next().is_none());
}

#[tokio::test]
async fn delete_uses_origin_authority_and_clears_a_matching_override() {
    let temporary = tempfile::tempdir().unwrap();
    let system = temporary.path().join("system");
    let configured = temporary.path().join("configured");
    let user = temporary.path().join("user");
    fs::create_dir(&system).unwrap();
    fs::create_dir(&configured).unwrap();
    fs::create_dir(&user).unwrap();
    write_preset(&system, "standard", Some("system\n"), None);
    write_preset(&configured, "shared", Some("configured\n"), None);
    write_preset(&user, "mine", Some("format = 1\n# mine\n"), None);
    write_preset(&user, "ghost", None, None);
    let defaults = Arc::new(TestDefaults::default());
    let catalog = AgentPresetCatalog::with_default_store(
        AgentPresetCatalogConfig::new(AgentPresetId::new("standard").unwrap())
            .with_system_root(&system)
            .with_configured_root(
                AgentPresetRoot::new(&configured, AgentPresetTrust::User).unwrap(),
            )
            .with_user_root(&user),
        defaults,
    )
    .unwrap();

    for read_only in ["standard", "shared"] {
        assert!(matches!(
            catalog
                .delete(&AgentPresetId::new(read_only).unwrap())
                .await,
            Err(PresetError::ReadOnlyPreset { .. })
        ));
    }
    catalog
        .set_default(&AgentPresetId::new("mine").unwrap())
        .await
        .unwrap();
    catalog
        .delete(&AgentPresetId::new("mine").unwrap())
        .await
        .unwrap();
    assert_eq!(catalog.default_id().await.unwrap().as_str(), "standard");
    assert!(!user.join("mine").exists());

    catalog
        .delete(&AgentPresetId::new("ghost").unwrap())
        .await
        .unwrap();
    assert!(!user.join("ghost").exists());
    assert!(user.read_dir().unwrap().next().is_none());
}

#[derive(Debug)]
struct RejectingDefaults {
    selected: AgentPresetId,
}

#[async_trait::async_trait]
impl AgentPresetDefaultStore for RejectingDefaults {
    async fn load(&self) -> rsi_agent_presets::Result<Option<AgentPresetId>> {
        Ok(Some(self.selected.clone()))
    }

    async fn replace(&self, _selected: Option<AgentPresetId>) -> rsi_agent_presets::Result<()> {
        Err(PresetError::Io {
            operation: "store default",
            path: "defaults.toml".into(),
            message: "injected failure".to_owned(),
        })
    }
}

#[tokio::test]
async fn delete_keeps_the_row_when_default_clearing_fails() {
    let temporary = tempfile::tempdir().unwrap();
    let user = temporary.path().join("user");
    fs::create_dir(&user).unwrap();
    write_preset(&user, "mine", Some("format = 1\n# mine\n"), None);
    let mine = AgentPresetId::new("mine").unwrap();
    let catalog = AgentPresetCatalog::with_default_store(
        AgentPresetCatalogConfig::new(AgentPresetId::new("standard").unwrap())
            .with_user_root(&user),
        Arc::new(RejectingDefaults {
            selected: mine.clone(),
        }),
    )
    .unwrap();

    assert!(matches!(
        catalog.delete(&mine).await,
        Err(PresetError::Io { .. })
    ));
    assert!(user.join("mine").is_dir());
    assert_eq!(
        catalog.document(&mine).unwrap().content,
        "format = 1\n# mine\n"
    );
}

#[tokio::test]
async fn delete_rejects_a_user_owned_deployment_base_default() {
    let temporary = tempfile::tempdir().unwrap();
    let user = temporary.path().join("user");
    fs::create_dir(&user).unwrap();
    write_preset(&user, "standard", Some("standard\n"), None);
    let standard = AgentPresetId::new("standard").unwrap();
    let catalog = AgentPresetCatalog::new(
        AgentPresetCatalogConfig::new(standard.clone()).with_user_root(&user),
    )
    .unwrap();

    assert!(matches!(
        catalog.delete(&standard).await,
        Err(PresetError::BaseDefaultPreset { .. })
    ));
    assert!(user.join("standard").is_dir());
}

#[cfg(unix)]
#[tokio::test]
async fn delete_unlinks_nested_symlinks_without_following_them() {
    use std::os::unix::fs::symlink;

    let temporary = tempfile::tempdir().unwrap();
    let user = temporary.path().join("user");
    let victim = temporary.path().join("victim");
    fs::create_dir(&user).unwrap();
    fs::create_dir(&victim).unwrap();
    fs::write(victim.join("kept"), "outside\n").unwrap();
    write_preset(&user, "mine", Some("mine\n"), None);
    symlink(&victim, user.join("mine/linked-directory")).unwrap();
    let catalog = AgentPresetCatalog::new(
        AgentPresetCatalogConfig::new(AgentPresetId::new("standard").unwrap())
            .with_user_root(&user),
    )
    .unwrap();

    catalog
        .delete(&AgentPresetId::new("mine").unwrap())
        .await
        .unwrap();

    assert!(!user.join("mine").exists());
    assert_eq!(
        fs::read_to_string(victim.join("kept")).unwrap(),
        "outside\n"
    );
    assert!(user.read_dir().unwrap().next().is_none());
}
