use super::*;

struct Fixture {
    root: tempfile::TempDir,
    source: HostPaths,
    destination: HostPaths,
}
impl Fixture {
    fn new() -> Self {
        let root = tempfile::tempdir_in(std::env::temp_dir().canonicalize().unwrap()).unwrap();
        let paths = |name: &str| {
            let directory = root.path().join(name);
            let config = directory.join("config");
            let state = directory.join("state");
            std::fs::create_dir_all(&config).unwrap();
            std::fs::create_dir_all(&state).unwrap();
            HostPaths::new(config, state, directory.join("cache")).unwrap()
        };
        let source = paths("source");
        let destination = paths("destination");
        for (paths, value) in [(&source, "new"), (&destination, "old")] {
            std::fs::write(
                paths.config().join("settings.json"),
                format!(r#"{{"rsi.example":{{"label":"{value}"}}}}"#),
            )
            .unwrap();
            std::fs::create_dir(paths.config().join("skills")).unwrap();
            std::fs::write(paths.config().join("skills/tool"), value).unwrap();
            std::fs::create_dir(paths.state().join("automation")).unwrap();
            std::fs::write(paths.state().join("automation/fixture"), value).unwrap();
            std::fs::write(paths.state().join("unrelated"), value).unwrap();
        }
        Self {
            root,
            source,
            destination,
        }
    }
    fn preserved(&self) {
        assert_eq!(
            std::fs::read(self.destination.state().join("unrelated")).unwrap(),
            b"old"
        );
        assert_eq!(
            std::fs::read(self.source.config().join("skills/tool")).unwrap(),
            b"new"
        );
    }
    fn published(&self) {
        assert_eq!(
            std::fs::read(self.destination.config().join("skills/tool")).unwrap(),
            b"new"
        );
        assert_eq!(
            std::fs::read(self.destination.state().join("automation/fixture")).unwrap(),
            b"new"
        );
        LaunchConfiguration::load(self.destination.config(), true).unwrap();
        reject_pending_import(&self.destination).unwrap();
        self.preserved();
    }
}

#[test]
fn explicit_import_dry_run_then_publishes_and_keeps_backups() {
    let fixture = Fixture::new();
    let dry_run = import_product_homes(&fixture.destination, &fixture.source, true).unwrap();
    assert!(dry_run.transaction.is_none());
    assert_eq!(dry_run.entries.len(), 3);
    assert_eq!(
        std::fs::read(fixture.destination.config().join("skills/tool")).unwrap(),
        b"old"
    );
    assert!(!fixture.destination.config().join("rsi.toml").exists());
    let receipt = import_product_homes(&fixture.destination, &fixture.source, false).unwrap();
    assert_eq!(receipt.backups.len(), 3);
    assert!(receipt.backups.iter().all(|path| path.exists()));
    fixture.published();
}

#[test]
fn recovery_settles_each_cross_home_rename_without_source_availability() {
    for stop in 1..=6 {
        let fixture = Fixture::new();
        let result = import_with_checkpoint(&fixture.destination, &fixture.source, false, |step| {
            if step == stop {
                Err(error("injected interruption"))
            } else {
                Ok(())
            }
        });
        assert!(result.is_err(), "step {stop}");
        assert!(reject_pending_import(&fixture.destination).is_err());
        let source_parent = fixture.source.config().parent().unwrap();
        std::fs::rename(source_parent, source_parent.with_file_name("unavailable")).unwrap();
        let receipt = recover_product_import(&fixture.destination).unwrap();
        assert_eq!(receipt.backups.len(), 3);
        assert_eq!(
            std::fs::read(fixture.destination.config().join("skills/tool")).unwrap(),
            b"new"
        );
        assert_eq!(
            std::fs::read(fixture.destination.state().join("automation/fixture")).unwrap(),
            b"new"
        );
        assert!(recover_product_import(&fixture.destination).is_err());
    }
}

#[test]
fn conflicts_invalid_settings_and_changed_recovery_bytes_preserve_payloads() {
    let fixture = Fixture::new();
    assert!(import_product_homes(&fixture.source, &fixture.source, false).is_err());
    let host = ServiceHostPaths::from_host_paths(&fixture.source).unwrap();
    let owner = HostOwnerLease::try_acquire(host).unwrap();
    assert!(import_product_homes(&fixture.destination, &fixture.source, false).is_err());
    drop(owner);
    std::fs::write(fixture.source.config().join("settings.json"), b"[]").unwrap();
    assert!(import_product_homes(&fixture.destination, &fixture.source, false).is_err());
    assert_eq!(
        std::fs::read(fixture.destination.config().join("skills/tool")).unwrap(),
        b"old"
    );
    std::fs::write(fixture.source.config().join("settings.json"), b"{}").unwrap();
    assert!(
        import_with_checkpoint(&fixture.destination, &fixture.source, false, |_| Err(
            error("interrupted")
        ))
        .is_err()
    );
    std::fs::write(
        fixture.destination.config().join("settings.json"),
        b"unexpected",
    )
    .unwrap();
    assert!(recover_product_import(&fixture.destination).is_err());
    assert_eq!(
        std::fs::read(fixture.destination.config().join("settings.json")).unwrap(),
        b"unexpected"
    );
    fixture.preserved();
}

#[test]
fn staged_agent_store_uses_its_owner_verifier_before_any_publication() {
    let fixture = Fixture::new();
    drop(Homes::open(&fixture.source).unwrap());
    let source_agent = fixture.source.state().join("agent");
    drop(rsi_agent_store_sqlite::SqliteStore::open(&source_agent).unwrap());
    import_product_homes(&fixture.destination, &fixture.source, false).unwrap();
    rsi_agent_store_sqlite::SqliteStore::verify(fixture.destination.state().join("agent")).unwrap();
    std::fs::write(source_agent.join("sessions.sqlite3"), b"invalid sqlite").unwrap();
    std::fs::write(
        fixture.source.config().join("skills/tool"),
        b"must not publish",
    )
    .unwrap();
    assert!(import_product_homes(&fixture.destination, &fixture.source, false).is_err());
    assert_eq!(
        std::fs::read(fixture.destination.config().join("skills/tool")).unwrap(),
        b"new"
    );
    reject_pending_import(&fixture.destination).unwrap();
    for home in [fixture.destination.config(), fixture.destination.state()] {
        assert!(!std::fs::read_dir(home).unwrap().any(|entry| {
            entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .contains("-stage-")
        }));
    }
    rsi_agent_store_sqlite::SqliteStore::verify(fixture.destination.state().join("agent")).unwrap();
}

#[test]
fn recovery_discards_partial_preparation_without_touching_payloads() {
    let fixture = Fixture::new();
    let homes = Homes::open(&fixture.destination).unwrap();
    let journal = Journal {
        format: 1,
        id: "a".repeat(32),
        config: canonical(fixture.destination.config()).unwrap(),
        state: canonical(fixture.destination.state()).unwrap(),
        entries: vec![Entry {
            config: true,
            name: "skills".into(),
            old: Some("b".repeat(64)),
            new: "c".repeat(64),
        }],
    };
    // Interruption after only one authority was written is recoverable too.
    homes.authorities[0]
        .create(STAGING, &serde_json::to_vec(&journal).unwrap())
        .unwrap();
    let stage = fixture
        .destination
        .config()
        .join(format!(".import-{}-stage-skills", journal.id));
    std::fs::create_dir(&stage).unwrap();
    std::fs::write(stage.join("partial"), b"incomplete").unwrap();
    drop(homes);
    assert!(reject_pending_import(&fixture.destination).is_err());
    let receipt = recover_product_import(&fixture.destination).unwrap();
    assert!(receipt.discarded_staging);
    assert!(receipt.entries.is_empty());
    assert!(!stage.exists());
    reject_pending_import(&fixture.destination).unwrap();
    assert_eq!(
        std::fs::read(fixture.destination.config().join("skills/tool")).unwrap(),
        b"old"
    );
    fixture.preserved();
}

#[cfg(unix)]
#[test]
fn links_are_refused_without_copying_their_target() {
    let fixture = Fixture::new();
    std::os::unix::fs::symlink(
        fixture.destination.state(),
        fixture.source.config().join("skills/outside"),
    )
    .unwrap();
    assert!(import_product_homes(&fixture.destination, &fixture.source, false).is_err());
    assert!(!fixture.destination.config().join("skills/outside").exists());
    fixture.preserved();
}

#[test]
fn dry_run_creates_no_authorities_or_missing_destination_homes() {
    let fixture = Fixture::new();
    let destination = HostPaths::new(
        fixture.root.path().join("absent/config"),
        fixture.root.path().join("absent/state"),
        fixture.root.path().join("absent/cache"),
    )
    .unwrap();
    #[cfg(windows)]
    let private_before = fs::open_absolute_directory_no_follow(fixture.source.config())
        .unwrap()
        .verify_private()
        .is_ok();
    let receipt = import_product_homes(&destination, &fixture.source, true).unwrap();
    assert_eq!(receipt.entries.len(), 3);
    assert!(!fixture.root.path().join("absent").exists());
    for path in [fixture.source.config(), fixture.source.state()] {
        assert!(!path.join("session-host").exists());
        assert!(!path.join(".rsi-namespace-pin").exists());
    }
    #[cfg(windows)]
    assert_eq!(
        fs::open_absolute_directory_no_follow(fixture.source.config())
            .unwrap()
            .verify_private()
            .is_ok(),
        private_before
    );
}
