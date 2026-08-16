use std::fs;

use rsi_meta_loader::{
    ApiVersion, ContentHash, ExpectedHashes, HashSubject, LoaderError, MAX_PLUGIN_ARTIFACT_BYTES,
    PluginLoader, PluginPackage, hash_regular_file, read_bounded_file,
    read_bounded_file_following_symlinks,
};
use tempfile::TempDir;

fn package_manifest(target: &str, minimum_minor: u32) -> String {
    format!(
        r#"format_version = 0
provides = ["example.echo"]
capabilities = ["clock.monotonic"]
config_schema = "config.schema.json"

[package]
id = "example.echo"
version = "1.2.3"
process_fixed = false

[host_api]
major = 0
minimum_minor = {minimum_minor}

[[artifacts]]
target = "{target}"
path = "lib/echo.so"

[[injects]]
contract = "example.clock"
required = false
"#
    )
}

struct Fixture {
    temp: TempDir,
    manifest_path: std::path::PathBuf,
    artifact_bytes: Vec<u8>,
    manifest_bytes: Vec<u8>,
    cache: std::path::PathBuf,
}

impl Fixture {
    fn new(manifest_target: &str, minimum_minor: u32) -> Self {
        let temp = TempDir::new().unwrap();
        let package = temp.path().join("package");
        fs::create_dir_all(package.join("lib")).unwrap();
        let artifact_bytes = (0..200_000)
            .map(|index| u8::try_from(index % 251).unwrap())
            .collect::<Vec<_>>();
        fs::write(package.join("lib/echo.so"), &artifact_bytes).unwrap();
        let manifest_bytes = package_manifest(manifest_target, minimum_minor).into_bytes();
        let manifest_path = package.join("plugin.toml");
        fs::write(&manifest_path, &manifest_bytes).unwrap();
        let cache = temp.path().join("cache");
        Self {
            temp,
            manifest_path,
            artifact_bytes,
            manifest_bytes,
            cache,
        }
    }

    fn hashes(&self) -> ExpectedHashes {
        ExpectedHashes::new(
            ContentHash::digest(&self.manifest_bytes),
            ContentHash::digest(&self.artifact_bytes),
        )
    }
}

#[test]
fn sha256_has_a_known_vector_and_round_trips_hex() {
    let hash = ContentHash::digest(b"abc");
    assert_eq!(
        hash.to_string(),
        "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
    );
    assert_eq!(hash.to_string().parse::<ContentHash>().unwrap(), hash);
    assert!("abcd".parse::<ContentHash>().is_err());
    assert!("AA".repeat(32).parse::<ContentHash>().is_err());
}

#[test]
fn shared_file_reader_bounds_bytes_and_streams_a_known_digest() {
    let temp = TempDir::new().unwrap();
    let path = temp.path().join("input");
    fs::write(&path, b"abc").unwrap();

    assert_eq!(
        read_bounded_file(&path, "read test input", 3).unwrap(),
        b"abc"
    );
    assert!(matches!(
        read_bounded_file(&path, "read test input", 2),
        Err(LoaderError::InputTooLarge {
            operation: "read test input",
            maximum_bytes: 2,
            ..
        })
    ));
    assert_eq!(
        hash_regular_file(&path, "hash test input")
            .unwrap()
            .to_string(),
        "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
    );
}

#[test]
fn artifact_hashing_rejects_oversized_regular_files_before_streaming_them() {
    let temp = TempDir::new().unwrap();
    let path = temp.path().join("oversized-artifact");
    let file = fs::File::create(&path).unwrap();
    file.set_len(u64::try_from(MAX_PLUGIN_ARTIFACT_BYTES).unwrap() + 1)
        .unwrap();

    assert!(matches!(
        hash_regular_file(&path, "hash test artifact"),
        Err(LoaderError::InputTooLarge {
            operation: "hash test artifact",
            maximum_bytes: MAX_PLUGIN_ARTIFACT_BYTES,
            ..
        })
    ));
}

#[cfg(unix)]
#[test]
fn shared_file_reader_rejects_symlinks_and_fifos_without_blocking() {
    use std::os::unix::fs::symlink;
    use std::process::Command;

    let temp = TempDir::new().unwrap();
    let target = temp.path().join("target");
    let link = temp.path().join("link");
    let fifo = temp.path().join("fifo");
    fs::write(&target, b"abc").unwrap();
    symlink(&target, &link).unwrap();
    assert!(
        Command::new("mkfifo")
            .arg(&fifo)
            .status()
            .unwrap()
            .success()
    );

    for path in [&link, &fifo] {
        assert!(matches!(
            read_bounded_file(path, "read test input", 16),
            Err(LoaderError::UnsafeInputFile { .. })
        ));
        assert!(matches!(
            hash_regular_file(path, "hash test input"),
            Err(LoaderError::UnsafeInputFile { .. })
        ));
    }
    assert_eq!(
        read_bounded_file_following_symlinks(&link, "read composition", 16).unwrap(),
        b"abc"
    );
    assert!(matches!(
        read_bounded_file_following_symlinks(&fifo, "read composition", 16),
        Err(LoaderError::UnsafeInputFile { .. })
    ));
}

#[test]
fn staging_verifies_both_hashes_and_reuses_a_readonly_cas_entry() {
    let fixture = Fixture::new("test-target", 0);
    let loader = PluginLoader::new(&fixture.cache, "test-target", ApiVersion::CURRENT);

    let first = loader
        .stage(&fixture.manifest_path, fixture.hashes())
        .unwrap();
    assert_eq!(
        fs::read(first.cached_artifact_path()).unwrap(),
        fixture.artifact_bytes
    );
    assert!(
        first
            .cached_artifact_path()
            .starts_with(fixture.cache.join("sha256"))
    );
    assert!(
        first
            .cached_artifact_path()
            .to_string_lossy()
            .contains(&first.artifact_hash().to_hex())
    );
    assert!(
        fs::metadata(first.cached_artifact_path())
            .unwrap()
            .permissions()
            .readonly()
    );
    assert_eq!(first.manifest().package.id, "example.echo");
    assert_eq!(first.manifest().provides, ["example.echo"]);
    assert!(!first.manifest().injects[0].required);

    let second = loader
        .stage(&fixture.manifest_path, fixture.hashes())
        .unwrap();
    assert_eq!(second.cached_artifact_path(), first.cached_artifact_path());
}

#[cfg(unix)]
#[test]
fn staging_rejects_a_cache_root_accessible_to_other_users() {
    use std::os::unix::fs::PermissionsExt;

    let fixture = Fixture::new("test-target", 0);
    fs::create_dir(&fixture.cache).unwrap();
    fs::set_permissions(&fixture.cache, fs::Permissions::from_mode(0o755)).unwrap();
    let loader = PluginLoader::new(&fixture.cache, "test-target", ApiVersion::CURRENT);

    assert!(matches!(
        loader.stage(&fixture.manifest_path, fixture.hashes()),
        Err(LoaderError::UnsafeCacheRoot(path)) if path == fixture.cache
    ));
}

#[test]
fn bad_manifest_and_artifact_hashes_are_rejected_before_publish() {
    let fixture = Fixture::new("test-target", 0);
    let loader = PluginLoader::new(&fixture.cache, "test-target", ApiVersion::CURRENT);
    let zero: ContentHash = "00".repeat(32).parse().unwrap();

    let error = loader
        .stage(
            &fixture.manifest_path,
            ExpectedHashes::new(zero, fixture.hashes().artifact),
        )
        .unwrap_err();
    assert!(matches!(
        error,
        LoaderError::HashMismatch {
            subject: HashSubject::Manifest,
            ..
        }
    ));

    let error = loader
        .stage(
            &fixture.manifest_path,
            ExpectedHashes::new(fixture.hashes().manifest, zero),
        )
        .unwrap_err();
    assert!(matches!(
        error,
        LoaderError::HashMismatch {
            subject: HashSubject::Artifact,
            ..
        }
    ));
    assert!(!fixture.cache.exists());
}

#[test]
fn wrong_target_is_rejected_with_available_targets() {
    let fixture = Fixture::new("some-other-target", 0);
    let loader = PluginLoader::new(&fixture.cache, "test-target", ApiVersion::CURRENT);
    let error = loader
        .stage(&fixture.manifest_path, fixture.hashes())
        .unwrap_err();

    match error {
        LoaderError::BadTarget { target, available } => {
            assert_eq!(target, "test-target");
            assert_eq!(available, ["some-other-target"]);
        }
        other => panic!("expected BadTarget, got {other:?}"),
    }
}

#[test]
fn too_new_or_incompatible_host_api_is_rejected() {
    let too_new = Fixture::new("test-target", 1);
    let loader = PluginLoader::new(&too_new.cache, "test-target", ApiVersion::CURRENT);
    assert!(matches!(
        loader.stage(&too_new.manifest_path, too_new.hashes()),
        Err(LoaderError::IncompatibleHostApi { .. })
    ));

    let wrong_major = Fixture::new("test-target", 0);
    let loader = PluginLoader::new(
        &wrong_major.cache,
        "test-target",
        ApiVersion { major: 1, minor: 0 },
    );
    assert!(matches!(
        loader.stage(&wrong_major.manifest_path, wrong_major.hashes()),
        Err(LoaderError::IncompatibleHostApi { .. })
    ));
}

#[test]
fn oversized_plugin_manifest_is_rejected_before_parsing() {
    let temp = TempDir::new().unwrap();
    let manifest_path = temp.path().join("plugin.toml");
    let manifest = format!(
        "#{}\n{}",
        "x".repeat(2 * 1024 * 1024),
        package_manifest("test-target", 0)
    );
    fs::write(&manifest_path, manifest).unwrap();

    assert!(PluginPackage::open(manifest_path).is_err());
}

#[cfg(unix)]
#[test]
fn package_inputs_must_be_regular_files_opened_without_following_symlinks() {
    use std::os::unix::fs::symlink;

    let manifest_fixture = Fixture::new("test-target", 0);
    let manifest_link = manifest_fixture.temp.path().join("manifest-link.toml");
    symlink(&manifest_fixture.manifest_path, &manifest_link).unwrap();
    assert!(PluginPackage::open(manifest_link).is_err());

    let artifact_fixture = Fixture::new("test-target", 0);
    let artifact_path = artifact_fixture
        .manifest_path
        .parent()
        .unwrap()
        .join("lib/echo.so");
    let artifact_target = artifact_fixture.temp.path().join("outside-artifact.so");
    fs::write(&artifact_target, &artifact_fixture.artifact_bytes).unwrap();
    fs::remove_file(&artifact_path).unwrap();
    symlink(&artifact_target, &artifact_path).unwrap();
    let loader = PluginLoader::new(&artifact_fixture.cache, "test-target", ApiVersion::CURRENT);

    assert!(
        loader
            .stage(&artifact_fixture.manifest_path, artifact_fixture.hashes())
            .is_err()
    );

    let parent_fixture = Fixture::new("test-target", 0);
    let package = parent_fixture.manifest_path.parent().unwrap();
    let outside = parent_fixture.temp.path().join("outside-directory");
    fs::create_dir(&outside).unwrap();
    fs::write(outside.join("echo.so"), &parent_fixture.artifact_bytes).unwrap();
    fs::remove_file(package.join("lib/echo.so")).unwrap();
    fs::remove_dir(package.join("lib")).unwrap();
    symlink(&outside, package.join("lib")).unwrap();
    let loader = PluginLoader::new(&parent_fixture.cache, "test-target", ApiVersion::CURRENT);
    assert!(matches!(
        loader.stage(&parent_fixture.manifest_path, parent_fixture.hashes()),
        Err(LoaderError::UnsafeManifestPath {
            field: "artifacts.path",
            ..
        })
    ));
}

#[test]
fn invalid_package_id_diagnostic_does_not_copy_attacker_controlled_input() {
    let temp = TempDir::new().unwrap();
    let oversized_id = "a".repeat(512);
    let manifest = package_manifest("test-target", 0).replacen(
        "id = \"example.echo\"",
        &format!("id = \"{oversized_id}\""),
        1,
    );
    let manifest_path = temp.path().join("plugin.toml");
    fs::write(&manifest_path, manifest).unwrap();
    let package = PluginPackage::open(&manifest_path).unwrap();
    let loader = PluginLoader::new(
        temp.path().join("cache"),
        "test-target",
        ApiVersion::CURRENT,
    );

    let error = loader.validate_manifest(package.manifest()).unwrap_err();
    assert!(matches!(error, LoaderError::InvalidPackageId));
    assert!(!format!("{error:?} {error}").contains(&oversized_id));
}

#[test]
fn invalid_contract_name_diagnostic_does_not_copy_attacker_controlled_input() {
    let temp = TempDir::new().unwrap();
    let oversized_contract = "s".repeat(512);
    let manifest =
        package_manifest("test-target", 0).replacen("example.echo", &oversized_contract, 1);
    let manifest_path = temp.path().join("plugin.toml");
    fs::write(&manifest_path, manifest).unwrap();
    let package = PluginPackage::open(&manifest_path).unwrap();
    let loader = PluginLoader::new(
        temp.path().join("cache"),
        "test-target",
        ApiVersion::CURRENT,
    );

    let error = loader.validate_manifest(package.manifest()).unwrap_err();
    assert!(!format!("{error:?} {error}").contains(&oversized_contract));
}
