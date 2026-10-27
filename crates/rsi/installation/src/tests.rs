use crate::{
    filesystem as fs,
    record::{Layout, Phase, Record, Role},
    transaction::Transaction,
    uninstall::{Options, uninstall},
};
use rsi_files_native_fs as native;
use rsi_host::HostPaths;
use std::io::Write as _;
use std::path::PathBuf;

struct Fixture {
    temporary: tempfile::TempDir,
    layout: Layout,
    homes: HostPaths,
}
impl Fixture {
    fn new() -> Self {
        let temporary = tempfile::tempdir().unwrap();
        let layout = Layout::new(&temporary.path().join("prefix")).unwrap();
        let homes = HostPaths::new(
            temporary.path().join("config"),
            temporary.path().join("state"),
            temporary.path().join("cache"),
        )
        .unwrap();
        for root in [
            &layout.bin,
            &layout.control,
            homes.config(),
            homes.state(),
            homes.cache(),
        ] {
            native::create_absolute_directory_no_follow(root).unwrap();
        }
        for role in [Role::Core, Role::Uninstaller, Role::Launcher] {
            std::fs::write(layout.target(role), format!("old {}", role.name())).unwrap();
        }
        let hash = |role| fs::digest_file(fs::source(&layout.target(role)).unwrap()).unwrap();
        let record = Record {
            format: 1,
            id: "a".repeat(32),
            prefix: layout.prefix.clone(),
            phase: Phase::Installed,
            core: hash(Role::Core),
            uninstaller: hash(Role::Uninstaller),
            launcher: hash(Role::Launcher),
            path_edits: vec![],
        };
        record
            .save(
                &native::open_absolute_directory_no_follow(&layout.control).unwrap(),
                false,
            )
            .unwrap();
        Self {
            temporary,
            layout,
            homes,
        }
    }
    fn options(&self, purge: bool) -> Options {
        Options {
            prefix: self.layout.prefix.clone(),
            homes: self.homes.clone(),
            purge,
            dry_run: false,
            no_path: true,
            force: false,
            yes: true,
        }
    }
    fn input(&self, name: &str) -> PathBuf {
        let file = self.temporary.path().join(name);
        std::fs::write(&file, format!("new {name}")).unwrap();
        file
    }
}

#[test]
fn installation_records_bound_input_and_cannot_redirect_targets() {
    let f = Fixture::new();
    let mut record = f.layout.record().unwrap().unwrap();
    record.prefix = f.temporary.path().join("elsewhere");
    assert!(Record::parse(&serde_json::to_vec(&record).unwrap(), &f.layout).is_err());
    assert!(Record::parse(&vec![b' '; 65537], &f.layout).is_err());
    let mut json = serde_json::to_value(f.layout.record().unwrap().unwrap()).unwrap();
    json["delete"] = "../../outside".into();
    assert!(Record::parse(&serde_json::to_vec(&json).unwrap(), &f.layout).is_err());
}

#[cfg(windows)]
#[tokio::test]
async fn managed_upgrade_refuses_shared_acl_but_uninstall_does_not_rewrite_it() {
    let f = Fixture::new();
    let status = std::process::Command::new("icacls.exe")
        .arg(&f.layout.control)
        .args(["/grant", "*S-1-1-0:(OI)(CI)F", "/Q"])
        .output()
        .unwrap();
    assert!(status.status.success(), "{status:?}");
    let directory = native::open_absolute_directory_no_follow(&f.layout.control).unwrap();
    assert!(directory.verify_private().is_err());
    let inputs = [f.input("core"), f.input("uninstaller"), f.input("launcher")];
    let failure = prepare(&f, [&inputs[0], &inputs[1], &inputs[2]]).unwrap_err();
    assert!(
        failure.to_string().contains("private current-user"),
        "{failure}"
    );
    assert!(!f.layout.bin.join(crate::record::PENDING).exists());
    uninstall(f.options(false)).await.unwrap();
    assert!(directory.verify_private().is_err());
}
#[tokio::test]
async fn ordinary_uninstall_preserves_homes_and_unrelated_install_files() {
    let f = Fixture::new();
    std::fs::write(f.homes.config().join("rsi.toml"), "authored").unwrap();
    std::fs::write(f.layout.bin.join("other-tool"), "keep").unwrap();
    uninstall(f.options(false)).await.unwrap();
    for role in Role::ALL {
        assert!(!f.layout.target(role).exists());
    }
    assert_eq!(
        std::fs::read(f.homes.config().join("rsi.toml")).unwrap(),
        b"authored"
    );
    assert_eq!(
        std::fs::read(f.layout.bin.join("other-tool")).unwrap(),
        b"keep"
    );
    uninstall(f.options(false)).await.unwrap();
}
#[tokio::test]
async fn dry_run_does_not_create_locks_or_change_record() {
    let f = Fixture::new();
    let before = std::fs::read(f.layout.target(Role::Record)).unwrap();
    let mut options = f.options(true);
    options.dry_run = true;
    uninstall(options).await.unwrap();
    assert_eq!(
        std::fs::read(f.layout.target(Role::Record)).unwrap(),
        before
    );
    assert!(!f.layout.bin.join(fs::LOCK).exists());
    assert!(!f.homes.state().join("session-host").exists());
}
#[tokio::test]
async fn purge_preserves_authored_content_and_removes_validated_secret_backups() {
    let f = Fixture::new();
    for name in [
        "credentials",
        "skills",
        "agents",
        "application-profiles",
        "host-profiles",
        "agent-presets",
        "native-addons",
    ] {
        std::fs::create_dir(f.homes.config().join(name)).unwrap();
        std::fs::write(f.homes.config().join(name).join("file"), name).unwrap();
    }
    for name in ["agent", "desktop-webview"] {
        std::fs::create_dir(f.homes.state().join(name)).unwrap();
        std::fs::write(f.homes.state().join(name).join("data"), "secret").unwrap();
    }
    std::fs::write(f.homes.state().join("unrelated"), "keep").unwrap();
    native::create_absolute_directory_no_follow(&f.homes.state().join("session-host")).unwrap();
    let id = "b".repeat(32);
    let receipt = crate::receipts::ImportJournal {
        format: 1,
        id: id.clone(),
        config: std::fs::canonicalize(f.homes.config()).unwrap(),
        state: std::fs::canonicalize(f.homes.state()).unwrap(),
        entries: vec![
            crate::receipts::ImportEntry {
                config: true,
                name: "credentials".into(),
                old: Some("c".repeat(64)),
                new: "d".repeat(64),
            },
            crate::receipts::ImportEntry {
                config: true,
                name: "skills".into(),
                old: Some("c".repeat(64)),
                new: "d".repeat(64),
            },
        ],
    };
    std::fs::write(
        f.homes
            .state()
            .join("session-host")
            .join(format!("import-{id}.json")),
        serde_json::to_vec(&receipt).unwrap(),
    )
    .unwrap();
    let secret = f
        .homes
        .config()
        .join(format!(".import-{id}-backup-credentials"));
    std::fs::create_dir(&secret).unwrap();
    std::fs::write(secret.join("key"), "secret").unwrap();
    let authored = f.homes.config().join(format!(".import-{id}-backup-skills"));
    std::fs::create_dir(&authored).unwrap();
    uninstall(f.options(true)).await.unwrap();
    assert!(!secret.exists());
    assert!(authored.exists());
    assert!(!f.homes.config().join("credentials").exists());
    for name in [
        "skills",
        "agents",
        "application-profiles",
        "host-profiles",
        "agent-presets",
        "native-addons",
    ] {
        assert!(f.homes.config().join(name).join("file").is_file());
    }
    assert!(!f.homes.state().join("agent").exists());
    assert!(!f.homes.state().join("desktop-webview").exists());
    assert!(f.homes.state().join("unrelated").exists());
}
#[tokio::test]
async fn force_does_not_override_pending_transactions_or_unknown_backups() {
    let f = Fixture::new();
    std::fs::write(f.layout.bin.join(crate::record::PENDING), "unknown").unwrap();
    let mut options = f.options(true);
    options.force = true;
    assert!(uninstall(options).await.is_err());
    assert!(f.layout.target(Role::Core).exists());
    std::fs::remove_file(f.layout.bin.join(crate::record::PENDING)).unwrap();
    std::fs::write(
        f.homes
            .config()
            .join(".import-unverified-backup-credentials"),
        "secret",
    )
    .unwrap();
    assert!(uninstall(f.options(true)).await.is_err());
    assert!(f.layout.target(Role::Core).exists());
}
#[tokio::test]
async fn missing_record_requires_force_and_explicit_consent() {
    let f = Fixture::new();
    std::fs::remove_file(f.layout.target(Role::Record)).unwrap();
    assert!(uninstall(f.options(false)).await.is_err());
    let mut options = f.options(false);
    options.force = true;
    uninstall(options).await.unwrap();
    assert!(!f.layout.target(Role::Core).exists());
}
#[test]
fn every_cross_directory_publication_boundary_is_recoverable() {
    for boundary in 1..=8 {
        let f = Fixture::new();
        let core = f.input("core");
        let tool = f.input("tool");
        let launcher = f.input("launcher");
        let tx = prepare(&f, [&core, &tool, &launcher]).unwrap();
        let mut step = 0;
        assert!(
            tx.promote_with(|| {
                step += 1;
                if step == boundary {
                    Err(crate::error("interrupted"))
                } else {
                    Ok(())
                }
            })
            .is_err()
        );
        drop(tx);
        let recovery = Transaction::recover(f.layout.clone(), &f.homes).unwrap();
        recovery.rollback().unwrap();
        recovery.finish(false).unwrap();
        assert_eq!(
            std::fs::read(f.layout.target(Role::Core)).unwrap(),
            format!("old {}", Role::Core.name()).as_bytes()
        );
        assert_eq!(f.layout.record().unwrap().unwrap().phase, Phase::Installed);
    }
}
#[test]
fn recovery_refuses_changed_participants_before_any_rollback() {
    let f = Fixture::new();
    let core = f.input("core");
    let tool = f.input("tool");
    let launcher = f.input("launcher");
    let tx = prepare(&f, [&core, &tool, &launcher]).unwrap();
    tx.promote().unwrap();
    drop(tx);
    std::fs::write(f.layout.target(Role::Launcher), "unexpected user bytes").unwrap();
    assert!(Transaction::recover(f.layout.clone(), &f.homes).is_err());
    assert_eq!(
        std::fs::read(f.layout.target(Role::Core)).unwrap(),
        b"new core"
    );
}

#[tokio::test]
async fn uninstall_resumes_owned_install_stages_but_never_bypasses_directory_lock() {
    let f = Fixture::new();
    let mut record = f.layout.record().unwrap().unwrap();
    record.phase = Phase::Installing;
    record
        .save(
            &native::open_absolute_directory_no_follow(&f.layout.control).unwrap(),
            true,
        )
        .unwrap();
    let staged = f.layout.control.join(format!(
        ".rsi-install-{}-{}",
        record.id,
        Role::Uninstaller.name()
    ));
    std::fs::rename(f.layout.target(Role::Uninstaller), &staged).unwrap();
    let lease = fs::Lease::acquire(&f.layout.bin, false, false).unwrap();
    let mut options = f.options(false);
    options.force = true;
    assert!(
        uninstall(options)
            .await
            .unwrap_err()
            .to_string()
            .contains("maintenance operation")
    );
    assert!(f.layout.target(Role::Core).exists());
    drop(lease);
    uninstall(f.options(false)).await.unwrap();
    assert!(!staged.exists());
}
#[cfg(windows)]
#[tokio::test]
async fn locked_main_preserves_retry_entries_and_uninstall_phase() {
    use std::os::windows::fs::OpenOptionsExt as _;
    let f = Fixture::new();
    let held = std::fs::OpenOptions::new()
        .read(true)
        .share_mode(1)
        .open(f.layout.target(Role::Core))
        .unwrap();
    assert!(
        uninstall(f.options(false))
            .await
            .unwrap_err()
            .to_string()
            .contains("close TUI/Desktop")
    );
    for role in Role::ALL {
        assert!(f.layout.target(role).exists());
    }
    assert_eq!(
        f.layout.record().unwrap().unwrap().phase,
        Phase::Uninstalling
    );
    drop(held);
    uninstall(f.options(false)).await.unwrap();
}
#[tokio::test]
async fn purge_recognizes_reset_receipts_using_the_producers_home_spelling() {
    let f = Fixture::new();
    let id = "e".repeat(32);
    let backup = f.homes.state().join(format!(".reset-{id}-agent"));
    std::fs::create_dir(&backup).unwrap();
    std::fs::write(backup.join("session"), b"secret").unwrap();
    let receipt = crate::receipts::ResetJournal {
        format: 1,
        id: id.clone(),
        config: f.homes.config().into(),
        state: f.homes.state().into(),
        selection: crate::receipts::ResetSelection {
            config: false,
            state: true,
        },
        entries: vec![crate::receipts::ResetEntry {
            config: false,
            name: "agent".into(),
            directory: true,
        }],
    };
    let directory = f.homes.state().join("session-host");
    std::fs::create_dir(&directory).unwrap();
    std::fs::write(
        directory.join(format!("reset-{id}.json")),
        serde_json::to_vec(&receipt).unwrap(),
    )
    .unwrap();
    uninstall(f.options(true)).await.unwrap();
    assert!(!backup.exists());
    assert!(!directory.join(format!("reset-{id}.json")).exists());
}

fn prepare(f: &Fixture, inputs: [&std::path::Path; 3]) -> crate::Result<Transaction> {
    let hashes = inputs.map(|input| fs::digest_file(fs::source(input).unwrap()).unwrap());
    Transaction::prepare(
        f.layout.clone(),
        inputs,
        hashes.each_ref().map(String::as_str),
        &f.homes,
        false,
    )
}
#[test]
fn managed_upgrade_refuses_source_bytes_different_from_the_admitted_package() {
    let f = Fixture::new();
    let core = f.input("core");
    let tool = f.input("tool");
    let launcher = f.input("launcher");
    let wrong = "0".repeat(64);
    assert!(
        Transaction::prepare(
            f.layout.clone(),
            [&core, &tool, &launcher],
            [&wrong, &wrong, &wrong],
            &f.homes,
            false
        )
        .unwrap_err()
        .to_string()
        .contains("admitted package digest")
    );
    assert!(!f.layout.bin.join(crate::record::PENDING).exists());
    assert_eq!(
        std::fs::read(f.layout.target(Role::Core)).unwrap(),
        format!("old {}", Role::Core.name()).as_bytes()
    );
}

#[tokio::test]
async fn force_without_a_record_preserves_unattributed_upgrade_artifacts() {
    let f = Fixture::new();
    std::fs::remove_file(f.layout.target(Role::Record)).unwrap();
    let residue = f
        .layout
        .bin
        .join(".rsi-core-upgrade-unattributed.committed.json");
    std::fs::write(&residue, b"unattributed").unwrap();
    let mut options = f.options(false);
    options.force = true;
    uninstall(options).await.unwrap();
    assert_eq!(std::fs::read(residue).unwrap(), b"unattributed");
}

#[test]
fn both_journal_versions_refuse_a_nonempty_shared_lock() {
    let f = Fixture::new();
    let input = f.input("candidate");
    let directory = native::open_absolute_directory_no_follow(&f.layout.bin).unwrap();
    native::create_relative_private_file_no_follow(&directory, std::path::Path::new(fs::LOCK))
        .unwrap()
        .write_all(b"unknown lock payload")
        .unwrap();
    let digest = fs::digest_file(fs::source(&input).unwrap()).unwrap();
    let legacy = crate::legacy::CoreUpgrade::prepare(
        &f.layout.target(Role::Core),
        &input,
        &digest,
        &f.homes,
        false,
    )
    .unwrap_err();
    let managed = prepare(&f, [&input, &input, &input]).unwrap_err();
    for failure in [legacy, managed] {
        assert!(
            failure.to_string().contains("empty regular file"),
            "{failure}"
        );
    }
    assert!(!f.layout.bin.join(crate::record::PENDING).exists());
}

#[test]
fn launch_provenance_validates_the_selected_source_artifact() {
    let f = Fixture::new();
    let executable = f.layout.target(Role::Core);
    let origin = crate::origin::for_executable(&executable).unwrap().unwrap();
    assert_eq!(origin.prefix, f.layout.prefix);
    assert_eq!(origin.id, f.layout.record().unwrap().unwrap().id);
    std::fs::write(executable, "unrecorded replacement").unwrap();
    assert!(crate::origin::for_executable(&f.layout.target(Role::Core)).is_err());
    assert!(
        crate::origin::for_executable(&f.input("unowned-copy"))
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn large_purge_preview_is_bounded_but_does_not_prevent_removal() {
    let f = Fixture::new();
    let data = f.homes.state().join("agent");
    std::fs::create_dir(&data).unwrap();
    for index in 0..16_400 {
        std::fs::write(data.join(format!("file-{index}")), b"data").unwrap();
    }
    let mut options = f.options(true);
    options.dry_run = true;
    uninstall(options).await.unwrap();
    assert_eq!(std::fs::read_dir(&data).unwrap().count(), 16_400);
    assert!(f.layout.target(Role::Core).exists());
    uninstall(f.options(true)).await.unwrap();
    assert!(!data.exists());
}

#[cfg(windows)]
#[test]
fn windows_prefix_admits_drive_and_existing_component_case_variants() {
    let f = Fixture::new();
    let varied = f.layout.prefix.to_string_lossy().to_lowercase();
    let admitted = Layout::new(std::path::Path::new(&varied)).unwrap();
    assert_eq!(admitted.prefix, f.layout.prefix);
    native::open_absolute_directory_no_follow(&admitted.prefix).unwrap();
    native::open_absolute_directory_no_follow(&fs::absolute(f.homes.config()).unwrap()).unwrap();
    assert!(admitted.record().unwrap().is_some());
}
