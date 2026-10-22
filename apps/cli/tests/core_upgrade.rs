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
        let root = tempfile::tempdir().unwrap();
        let bin = root.path().join("bin");
        let package = root.path().join("package");
        rsi_files_native_fs::create_absolute_directory_no_follow(&bin).unwrap();
        std::fs::create_dir(&package).unwrap();
        let filename = if cfg!(windows) { "rsi.exe" } else { "rsi" };
        let executable = bin.join(filename);
        let candidate = package.join(filename);
        std::fs::copy(support::product_binary(), &executable).unwrap();
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
            "format":1, "name":"core", "version":"fixture", "target":format!("{}-{}",std::env::consts::OS,std::env::consts::ARCH),
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
    let transaction = rsi::CoreUpgrade::prepare(
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
    assert!(!rsi::CoreUpgrade::has_pending(fixture.executable.parent().unwrap()).unwrap());
    fixture.success(&["host", "stop"]);
}
