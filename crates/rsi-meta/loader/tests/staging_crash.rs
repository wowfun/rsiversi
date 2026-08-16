#![cfg(all(unix, not(feature = "test-failpoints")))]

use std::ffi::OsString;
use std::fs;
use std::io::{self, Read};
use std::os::unix::net::UnixListener;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use rsi_meta_loader::{ApiVersion, ContentHash, ExpectedHashes, PluginLoader};
use serde_json::json;
use tempfile::TempDir;

const GATE_ENV: &str = "RSI_META_LOADER_TEST_STAGE_GATE";
const READY_BYTE: u8 = 1;
const CHILD_DEADLINE: Duration = Duration::from_secs(20);

struct ChildGuard(Child);

impl Drop for ChildGuard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

struct Fixture {
    _temporary: TempDir,
    manifest_path: PathBuf,
    cache: PathBuf,
    artifact_bytes: Vec<u8>,
    hashes: ExpectedHashes,
    cache_path: PathBuf,
    gate_path: PathBuf,
}

impl Fixture {
    fn create() -> Self {
        let temporary = tempfile::tempdir().unwrap();
        let package = temporary.path().join("package");
        fs::create_dir_all(package.join("lib")).unwrap();
        let artifact_bytes = b"crash-safe staged artifact bytes".to_vec();
        fs::write(package.join("lib/echo.so"), &artifact_bytes).unwrap();
        let manifest = br#"format_version = 0
provides = ["example.echo"]

[package]
id = "example.echo"
version = "1.0.0"
process_fixed = false

[host_api]
major = 0
minimum_minor = 0

[[artifacts]]
target = "test-target"
path = "lib/echo.so"
"#
        .to_vec();
        let manifest_path = package.join("plugin.toml");
        fs::write(&manifest_path, &manifest).unwrap();
        let hashes = ExpectedHashes::new(
            ContentHash::digest(&manifest),
            ContentHash::digest(&artifact_bytes),
        );
        // Ask the production loader for its current CAS layout in an isolated
        // cache. The crash target must follow that reported path instead of
        // duplicating the private `sha256/<digest>/artifact.<ext>` layout.
        let layout_probe = temporary.path().join("layout-probe");
        let staged = PluginLoader::new(&layout_probe, "test-target", ApiVersion::CURRENT)
            .stage(&manifest_path, hashes)
            .unwrap();
        let relative_cache_path = staged
            .cached_artifact_path()
            .strip_prefix(&layout_probe)
            .unwrap()
            .to_path_buf();
        let cache = temporary.path().join("cache");
        let cache_path = cache.join(relative_cache_path);
        let gate_path = temporary.path().join("stage-before-publish.sock");
        Self {
            _temporary: temporary,
            manifest_path,
            cache,
            artifact_bytes,
            hashes,
            cache_path,
            gate_path,
        }
    }
}

fn build_feature_child(target_directory: &Path) -> PathBuf {
    let cargo = std::env::var_os("CARGO").unwrap_or_else(|| OsString::from("cargo"));
    let status = Command::new(cargo)
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .arg("build")
        .arg("--quiet")
        .arg("--locked")
        .arg("--offline")
        .arg("--example")
        .arg("stage-failpoint-child")
        .arg("--features")
        .arg("test-failpoints")
        .arg("--target-dir")
        .arg(target_directory)
        .status()
        .unwrap();
    assert!(status.success(), "feature child failed to build");
    target_directory
        .join("debug")
        .join("examples")
        .join(format!(
            "stage-failpoint-child{}",
            std::env::consts::EXE_SUFFIX
        ))
}

fn accept_without_sleep(
    listener: &UnixListener,
    child: &mut Child,
) -> std::os::unix::net::UnixStream {
    listener.set_nonblocking(true).unwrap();
    let deadline = Instant::now() + CHILD_DEADLINE;
    loop {
        match listener.accept() {
            Ok((stream, _)) => {
                // Accepted-stream nonblocking inheritance differs across
                // Unix implementations. The gate read below is deadline-
                // bounded blocking I/O, so configure that contract explicitly.
                stream.set_nonblocking(false).unwrap();
                return stream;
            }
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                if let Some(status) = child.try_wait().unwrap() {
                    panic!("feature child exited before the staging gate: {status}");
                }
                assert!(Instant::now() < deadline, "staging gate timed out");
                thread::yield_now();
            }
            Err(error) => panic!("accept staging gate: {error}"),
        }
    }
}

#[test]
fn crash_before_publish_leaves_no_cas_target_and_default_retry_publishes_exact_hash() {
    let fixture = Fixture::create();
    let listener = UnixListener::bind(&fixture.gate_path).unwrap();
    let build = tempfile::tempdir().unwrap();
    let child_binary = build_feature_child(build.path());
    let gate = json!({
        "artifact_hash": fixture.hashes.artifact.to_hex(),
        "cache_path": fixture.cache_path,
        "gate_path": fixture.gate_path,
    });
    let child = Command::new(child_binary)
        .arg(&fixture.manifest_path)
        .arg(&fixture.cache)
        .arg(fixture.hashes.manifest.to_hex())
        .arg(fixture.hashes.artifact.to_hex())
        .arg("test-target")
        .env(GATE_ENV, serde_json::to_string(&gate).unwrap())
        .stdout(Stdio::null())
        .stderr(Stdio::inherit())
        .spawn()
        .unwrap();
    let mut child = ChildGuard(child);

    let mut gate_connection = accept_without_sleep(&listener, &mut child.0);
    gate_connection
        .set_read_timeout(Some(CHILD_DEADLINE))
        .unwrap();
    let mut ready = [0_u8; 1];
    gate_connection.read_exact(&mut ready).unwrap();
    assert_eq!(ready, [READY_BYTE]);
    child.0.kill().unwrap();
    assert!(!child.0.wait().unwrap().success());

    assert!(
        !fixture.cache_path.exists(),
        "the canonical CAS target was visible before publish"
    );
    if let Some(digest_directory) = fixture.cache_path.parent()
        && digest_directory.exists()
    {
        for entry in fs::read_dir(digest_directory).unwrap() {
            let entry = entry.unwrap();
            if entry.file_name() == ".stage.lock" {
                continue;
            }
            let path = entry.path();
            assert_ne!(path, fixture.cache_path);
            assert_eq!(
                fs::read(&path).unwrap(),
                fixture.artifact_bytes,
                "crash left partial bytes in the CAS directory"
            );
        }
    }

    let staged = PluginLoader::new(&fixture.cache, "test-target", ApiVersion::CURRENT)
        .stage(&fixture.manifest_path, fixture.hashes)
        .unwrap();
    assert_eq!(staged.cached_artifact_path(), fixture.cache_path);
    assert_eq!(
        ContentHash::digest(fs::read(staged.cached_artifact_path()).unwrap()),
        fixture.hashes.artifact
    );
    let digest_directory = fixture.cache_path.parent().unwrap();
    assert!(
        fs::read_dir(digest_directory).unwrap().all(|entry| !entry
            .unwrap()
            .file_name()
            .to_string_lossy()
            .starts_with(".rsi-meta-stage-")),
        "a successful retry must reap crash-orphaned staging files"
    );
}
