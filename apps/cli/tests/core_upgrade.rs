#![cfg(any(target_os = "linux", windows))]
mod support;
use sha2::{Digest as _, Sha256};
use std::{
    io::{Read as _, Write as _},
    path::{Path, PathBuf},
    process::{Command, Output},
};

struct Fixture {
    root: tempfile::TempDir,
    paths: rsi_host::HostPaths,
    executable: PathBuf,
    package: PathBuf,
    candidate: PathBuf,
}
impl Fixture {
    fn new() -> Self {
        let root = tempfile::tempdir_in(rsi_workspace_path::canonical_native_path(
            std::env::temp_dir().canonicalize().unwrap(),
        ))
        .unwrap();
        let bin = root.path().join("bin");
        let package = root.path().join("package");
        let directory = rsi_files_native_fs::create_absolute_directory_no_follow(&bin).unwrap();
        std::fs::create_dir(&package).unwrap();
        let filename = if cfg!(windows) { "rsi.exe" } else { "rsi" };
        let executable = bin.join(filename);
        let candidate = package.join(filename);
        rsi_installation::filesystem::copy(&directory, filename, &support::product_binary(), true)
            .unwrap();
        std::fs::copy(&executable, &candidate).unwrap();
        // ELF and PE permit an uninterpreted trailing overlay. The executable
        // behavior stays the same while its independently verified artifact changes.
        std::fs::OpenOptions::new()
            .append(true)
            .open(&candidate)
            .unwrap()
            .write_all(b"\nrsi core transaction integration generation 2\n")
            .unwrap();
        let paths = rsi_host::HostPaths::new(
            root.path().join("config"),
            root.path().join("state"),
            root.path().join("cache"),
        )
        .unwrap();
        Self {
            root,
            paths,
            executable,
            package,
            candidate,
        }
    }
    fn command(&self) -> Command {
        let mut command = Command::new(&self.executable);
        command.env_clear();
        for key in [
            "SystemRoot",
            "WINDIR",
            "COMSPEC",
            "PATH",
            "PATHEXT",
            "TEMP",
            "TMP",
        ] {
            if let Some(value) = std::env::var_os(key) {
                command.env(key, value);
            }
        }
        command
            .current_dir(self.root.path())
            .env("HOME", self.root.path().join("home"))
            .arg("--config-home")
            .arg(self.paths.config())
            .arg("--state-home")
            .arg(self.paths.state())
            .arg("--cache-home")
            .arg(self.paths.cache());
        command
    }
    fn success(&self, args: &[&str]) -> Output {
        let output = self.command().args(args).output().unwrap();
        assert!(
            output.status.success(),
            "{args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        output
    }
    fn metadata(&self) -> rsi_service_host::HostOwnerMetadata {
        rsi_service_host::ServiceHostPaths::from_host_paths(&self.paths)
            .unwrap()
            .read_metadata()
            .unwrap()
            .unwrap()
    }
    fn manifest(&self) {
        let name = self.candidate.file_name().unwrap().to_str().unwrap();
        std::fs::write(self.package.join("package.json"), serde_json::to_vec(&serde_json::json!({
            "format":2, "name":"core", "version":"fixture", "target":format!("{}-{}",std::env::consts::OS,std::env::consts::ARCH),
            "dependencies":[], "requires":[],
            "exports":{"core":{"kind":"executable","path":name}},
            "files":[{"path":name,"bytes":std::fs::metadata(&self.candidate).unwrap().len(),"digest":hash(&self.candidate),"executable":true}]
        })).unwrap()).unwrap();
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = self.command().args(["host", "stop", "--force"]).output();
    }
}
fn hash(path: &Path) -> String {
    let mut source = std::fs::File::open(path).unwrap();
    let mut digest = Sha256::new();
    let mut buffer = vec![0; 64 * 1024].into_boxed_slice();
    loop {
        let size = source.read(&mut buffer).unwrap();
        if size == 0 {
            break;
        }
        digest.update(&buffer[..size]);
    }
    format!("{:x}", digest.finalize())
}

#[test]
fn installed_core_upgrades_recovers_and_preserves_active_host_on_invalid_candidate() {
    let fixture = Fixture::new();
    fixture.success(&["--reset-config", "--reset-state", "tui", "--help"]);
    for command in ["config", "core", "import", "plugin"] {
        fixture.success(&["--reset-config", "--reset-state", command, "--help"]);
    }
    assert!(!fixture.paths.config().exists());
    assert!(!fixture.paths.state().exists());
    fixture.success(&["config", "init"]);
    fixture.success(&["host", "start"]);
    let original = hash(&fixture.executable);
    let first = fixture.metadata();
    fixture.manifest();
    let upgraded = fixture
        .command()
        .args(["core", "upgrade", "--local"])
        .arg(&fixture.package)
        .output()
        .unwrap();
    assert!(
        upgraded.status.success(),
        "{}",
        String::from_utf8_lossy(&upgraded.stderr)
    );
    assert!(String::from_utf8_lossy(&upgraded.stdout).contains("core_upgraded"));
    assert_ne!(original, hash(&fixture.executable));
    assert_eq!(hash(&fixture.executable), hash(&fixture.candidate));
    let second = fixture.metadata();
    assert_ne!(first.pid, second.pid);
    assert_ne!(first.host_epoch, second.host_epoch);
    fixture.success(&["host", "status"]);

    // Simulate interruption after executable promotion. Recovery runs through the
    // real CLI and retained Host coordinator, and reactivates the previous core.
    let transaction = rsi_installation::legacy::CoreUpgrade::prepare(
        &fixture.executable,
        &support::product_binary(),
        &original,
        &fixture.paths,
        true,
    )
    .unwrap();
    transaction.promote().unwrap();
    drop(transaction);
    let denied = fixture.command().args(["config", "show"]).output().unwrap();
    assert!(!denied.status.success());
    fixture.success(&["core", "recover"]);
    assert_eq!(hash(&fixture.executable), hash(&fixture.candidate));
    let recovered = fixture.metadata();
    assert_ne!(second.pid, recovered.pid);

    std::fs::write(&fixture.candidate, b"invalid executable").unwrap();
    fixture.manifest();
    let invalid = fixture
        .command()
        .args(["core", "upgrade", "--local"])
        .arg(&fixture.package)
        .output()
        .unwrap();
    assert!(!invalid.status.success());
    assert!(String::from_utf8_lossy(&invalid.stderr).contains("candidate executable"));
    assert_eq!(recovered.pid, fixture.metadata().pid);
    assert!(
        !rsi_installation::legacy::CoreUpgrade::has_pending(fixture.executable.parent().unwrap())
            .unwrap()
    );
    fixture.success(&["host", "stop"]);
}

fn uninstaller() -> PathBuf {
    std::env::var_os("RSI_TEST_UNINSTALLER").map_or_else(
        || {
            support::product_binary()
                .parent()
                .unwrap()
                .join(if cfg!(windows) {
                    "rsi-uninstall.exe"
                } else {
                    "rsi-uninstall"
                })
        },
        PathBuf::from,
    )
}
impl Fixture {
    fn managed(&self) -> rsi_installation::Layout {
        use rsi_installation::{
            Record,
            record::{Phase, Role},
        };
        let layout = rsi_installation::Layout::new(self.root.path()).unwrap();
        let directory =
            rsi_files_native_fs::create_absolute_directory_no_follow(&layout.control).unwrap();
        rsi_installation::filesystem::copy(
            &directory,
            Role::Uninstaller.name(),
            &uninstaller(),
            true,
        )
        .expect("build rsi-uninstall before integration tests");
        let launcher = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../scripts")
            .join(Role::Launcher.name())
            .canonicalize()
            .map(rsi_workspace_path::canonical_native_path)
            .unwrap();
        rsi_installation::filesystem::copy(
            &directory,
            Role::Launcher.name(),
            &launcher,
            cfg!(unix),
        )
        .unwrap();
        Record {
            format: 1,
            id: "a".repeat(32),
            prefix: layout.prefix.clone(),
            phase: Phase::Installed,
            core: hash(&self.executable),
            uninstaller: hash(&layout.target(Role::Uninstaller)),
            launcher: hash(&layout.target(Role::Launcher)),
            path_edits: vec![],
        }
        .save(&directory, false)
        .unwrap();
        layout
    }
    fn full_manifest(&self, layout: &rsi_installation::Layout) -> PathBuf {
        let produced = self.root.path().join("produced-core-package");
        let output = Command::new("node")
            .arg(Path::new(env!("CARGO_MANIFEST_DIR")).join("../devtools/packages.mjs"))
            .args(["assemble", "core"])
            .arg(&produced)
            .arg(&self.candidate)
            .arg(layout.target(rsi_installation::record::Role::Uninstaller))
            .output()
            .expect("Node is required for the producer-to-consumer package fixture");
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        produced
    }
    fn interrupt_managed_upgrade(&self, layout: &rsi_installation::Layout) {
        let inputs = [
            support::product_binary(),
            uninstaller(),
            PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("../../scripts")
                .join(rsi_installation::record::LAUNCHER)
                .canonicalize()
                .map(rsi_workspace_path::canonical_native_path)
                .unwrap(),
        ];
        let hashes = inputs.each_ref().map(|path| hash(path));
        let host = rsi_service_host::ServiceHostPaths::from_host_paths(&self.paths).unwrap();
        let startup = rsi_service_host::HostStartupLease::try_acquire(&host).unwrap();
        let transaction = rsi_installation::transaction::Transaction::prepare(
            layout.clone(),
            inputs.each_ref().map(PathBuf::as_path),
            hashes.each_ref().map(String::as_str),
            &self.paths,
            true,
        )
        .unwrap();
        transaction.promote().unwrap();
        drop(transaction);
        drop(startup);
    }
    fn uninstall(&self, prefix: &Path, purge: bool) -> Output {
        let mut command = Command::new(uninstaller());
        command
            .args(["uninstall", "--interface-version", "1", "--prefix"])
            .arg(prefix)
            .arg("--config-home")
            .arg(self.paths.config())
            .arg("--state-home")
            .arg(self.paths.state())
            .arg("--cache-home")
            .arg(self.paths.cache())
            .args(["--yes", "--no-path"]);
        if purge {
            command.arg("--purge");
        }
        command.output().unwrap()
    }
}
#[test]
fn managed_upgrade_updates_all_roles_and_uninstall_stops_only_its_own_host() {
    use rsi_installation::record::Role;
    let fixture = Fixture::new();
    let layout = fixture.managed();
    fixture.success(&["config", "init"]);
    fixture.success(&["host", "start"]);
    let first = fixture.metadata();
    assert_eq!(first.format, 3);
    assert_eq!(first.installation.as_ref().unwrap().prefix, layout.prefix);
    fixture.manifest();
    let refused = fixture
        .command()
        .args(["core", "upgrade", "--local"])
        .arg(&fixture.package)
        .output()
        .unwrap();
    assert!(!refused.status.success());
    assert_eq!(first.pid, fixture.metadata().pid);
    assert!(!rsi_installation::legacy::CoreUpgrade::has_pending(&layout.bin).unwrap());
    let produced = fixture.full_manifest(&layout);
    let upgraded = fixture
        .command()
        .args(["core", "upgrade", "--local"])
        .arg(&produced)
        .output()
        .unwrap();
    assert!(
        upgraded.status.success(),
        "{}",
        String::from_utf8_lossy(&upgraded.stderr)
    );
    let event: serde_json::Value = serde_json::from_slice(&upgraded.stdout)
        .expect("core upgrade stdout contains exactly one JSON receipt");
    assert_eq!(event["event"], "core_upgraded");
    let record = layout.record().unwrap().unwrap();
    assert_eq!(record.core, hash(&fixture.candidate));
    assert_eq!(record.uninstaller, hash(&layout.target(Role::Uninstaller)));
    assert_eq!(record.launcher, hash(&layout.target(Role::Launcher)));
    let second = fixture.metadata();
    assert_eq!(second.installation.as_ref().unwrap().prefix, layout.prefix);
    assert_ne!(first.pid, second.pid);
    // Exercise version dispatch through the real CLI after a v2 publication crash.
    let before_recovery: Vec<_> = Role::ALL
        .into_iter()
        .map(|role| hash(&layout.target(role)))
        .collect();
    fixture.interrupt_managed_upgrade(&layout);
    assert!(
        !fixture
            .command()
            .args(["config", "show"])
            .output()
            .unwrap()
            .status
            .success()
    );
    fixture.success(&["core", "recover"]);
    assert_eq!(
        before_recovery,
        Role::ALL
            .into_iter()
            .map(|role| hash(&layout.target(role)))
            .collect::<Vec<_>>()
    );
    let recovered = fixture.metadata();
    assert_eq!(
        recovered.installation.as_ref().unwrap().prefix,
        layout.prefix
    );
    assert_ne!(second.pid, recovered.pid);
    let second = recovered;
    let removed = fixture.uninstall(&layout.prefix, false);
    assert!(
        removed.status.success(),
        "{}",
        String::from_utf8_lossy(&removed.stderr)
    );
    assert!(!rsi_service_host::owner_process_is_current(&second).unwrap());
    assert!(fixture.paths.config().join("rsi.toml").exists());
    assert!(
        fixture
            .paths
            .state()
            .join("session-host/artifacts")
            .exists()
    );
    for role in Role::ALL {
        assert!(!layout.target(role).exists());
    }
}
#[test]
fn same_build_in_another_prefix_does_not_own_a_shared_host_but_purge_stops_it() {
    let owner = Fixture::new();
    let owner_layout = owner.managed();
    owner.success(&["config", "init"]);
    owner.success(&["host", "start"]);
    let metadata = owner.metadata();
    let other = Fixture::new();
    let other_layout = other.managed();
    // Deliberately retain the same fixture installation ID and build: prefix still differs.
    let removed = owner.uninstall(&other_layout.prefix, false);
    assert!(
        removed.status.success(),
        "{}",
        String::from_utf8_lossy(&removed.stderr)
    );
    assert!(rsi_service_host::owner_process_is_current(&metadata).unwrap());
    let purged = owner.uninstall(&other_layout.prefix, true);
    assert!(
        purged.status.success(),
        "{}",
        String::from_utf8_lossy(&purged.stderr)
    );
    assert!(!rsi_service_host::owner_process_is_current(&metadata).unwrap());
    assert!(
        owner_layout
            .target(rsi_installation::record::Role::Core)
            .exists()
    );
    assert!(owner.paths.config().join("rsi.toml").exists());
}

#[test]
fn stale_host_stop_does_not_claim_it_stopped_a_process() {
    let fixture = Fixture::new();
    fixture.success(&["config", "init"]);
    fixture.success(&["host", "start"]);
    let metadata = fixture.metadata();
    fixture.success(&["host", "stop"]);
    let host = rsi_service_host::ServiceHostPaths::from_host_paths(&fixture.paths).unwrap();
    let owner = rsi_service_host::HostOwnerLease::try_acquire(host.clone()).unwrap();
    // Recreate a stopped product generation's record, not this test process's build.
    let directory =
        rsi_files_native_fs::open_absolute_directory_no_follow(host.owner_directory()).unwrap();
    rsi_installation::filesystem::write(
        &directory,
        "owner.json",
        &serde_json::to_vec(&metadata).unwrap(),
        true,
    )
    .unwrap();
    drop(owner);
    let output = fixture.success(&["host", "stop"]);
    assert!(output.stdout.is_empty());
    assert!(String::from_utf8_lossy(&output.stderr).contains("is not running"));
    assert!(host.owner_metadata().exists());
}
