use rsi_packages::*;
use sha2::{Digest as _, Sha256};
use std::collections::BTreeMap;

fn source(root: &std::path::Path, name: &str, bytes: &[u8]) -> InstallSource {
    let directory = root.join(format!("input-{name}-{}", bytes.len()));
    std::fs::create_dir_all(&directory).unwrap();
    std::fs::write(directory.join("application.toml"), bytes).unwrap();
    let manifest = Manifest {
        format: 2,
        name: name.into(),
        version: "1".into(),
        target: "portable".into(),
        dependencies: vec![],
        requires: vec![],
        files: vec![FileRecord {
            path: "application.toml".into(),
            bytes: bytes.len() as u64,
            digest: hex::encode(Sha256::digest(bytes)),
            executable: false,
        }],
        exports: BTreeMap::from([(
            "main".into(),
            Export {
                kind: ExportKind::ApplicationProfile,
                path: "application.toml".into(),
            },
        )]),
    };
    InstallSource {
        directory,
        manifest: serde_json::to_vec(&manifest).unwrap(),
        provenance: Provenance::Local,
    }
}

#[test]
fn producer_fence_rejects_after_staging_without_changing_selection() {
    let root = tempfile::tempdir().unwrap();
    let store = PackageStore::open(&root.path().join("packages")).unwrap();
    let initial = store
        .install(
            &[source(root.path(), "web", b"first")],
            &[],
            &[],
            "windows-x86_64",
        )
        .unwrap();
    let result = store.install_checked(
        &[source(root.path(), "web", b"second")],
        &[],
        &[],
        "windows-x86_64",
        || Err(Error::Invalid("source changed".into())),
    );
    assert!(result.is_err());
    assert_eq!(
        serde_json::to_value(store.resolution().unwrap()).unwrap(),
        serde_json::to_value(initial).unwrap()
    );
    assert!(store.collect().unwrap().is_empty());
}

#[test]
fn installation_recovers_an_abandoned_stage_without_a_manifest() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("packages");
    let store = PackageStore::open(&path).unwrap();
    let stage = path
        .join("objects")
        .join(format!("stage-{}", "a".repeat(32)));
    rsi_files_native_fs::create_absolute_directory_no_follow(&stage).unwrap();
    store
        .install(
            &[source(root.path(), "web", b"ready")],
            &[],
            &[],
            "windows-x86_64",
        )
        .unwrap();
    assert!(!stage.exists());
    assert_eq!(store.acquire("web").unwrap().manifest().name, "web");
}

#[test]
fn debug_executable_lengths_are_admitted_with_a_finite_disk_bound() {
    let root = tempfile::tempdir().unwrap();
    let source = source(root.path(), "core", b"length declaration");
    let mut manifest: Manifest = serde_json::from_slice(&source.manifest).unwrap();
    manifest.files[0].bytes = 1_601_116_312; // Measured ordinary Linux debug CLI.
    Manifest::parse(&serde_json::to_vec(&manifest).unwrap()).unwrap();
    manifest.files[0].bytes = MAX_FILE_BYTES + 1;
    assert!(Manifest::parse(&serde_json::to_vec(&manifest).unwrap()).is_err());
}

#[test]
fn update_keeps_leased_bytes_and_failed_install_keeps_resolution() {
    let root = tempfile::tempdir().unwrap();
    let store = PackageStore::open(&root.path().join("packages")).unwrap();
    let first = source(root.path(), "web", b"first");
    let initial = store.install(&[first], &[], &[], "windows-x86_64").unwrap();
    let lease = store.acquire("web").unwrap();
    let first_path = lease
        .export("main", ExportKind::ApplicationProfile)
        .unwrap();
    let second = source(root.path(), "web", b"second");
    store
        .install(&[second], &[], &[], "windows-x86_64")
        .unwrap();
    assert_eq!(std::fs::read(first_path).unwrap(), b"first");
    let current = store.acquire("web").unwrap();
    assert_eq!(
        std::fs::read(
            current
                .export("main", ExportKind::ApplicationProfile)
                .unwrap()
        )
        .unwrap(),
        b"second"
    );
    let before = store.resolution().unwrap();
    assert_ne!(initial.packages, before.packages);
    let exact = store
        .acquire_exact("web", &initial.packages["web"].digest)
        .unwrap();
    assert_eq!(exact.digest(), lease.digest());
    assert!(store.collect().unwrap().is_empty());
    drop(exact);
    drop(lease);
    assert_eq!(
        store.collect().unwrap(),
        vec![initial.packages["web"].digest.clone()]
    );
    let broken = source(root.path(), "web", b"third");
    std::fs::write(broken.directory.join("application.toml"), b"bad").unwrap();
    assert!(
        store
            .install(&[broken], &[], &[], "windows-x86_64")
            .is_err()
    );
    assert_eq!(before.packages, store.resolution().unwrap().packages);
    assert!(
        store.collect().unwrap().is_empty(),
        "failed stage was reclaimed before returning"
    );
    store.uninstall(&["web".into()]).unwrap();
    assert!(
        store.collect().unwrap().is_empty(),
        "live current lease survives uninstall"
    );
    drop(current);
    assert_eq!(
        store.collect().unwrap(),
        vec![before.packages["web"].digest.clone()]
    );
}

#[test]
fn exact_dependencies_publish_together_and_cannot_be_partially_updated() {
    let root = tempfile::tempdir().unwrap();
    let store = PackageStore::open(&root.path().join("packages")).unwrap();
    let dependency = source(root.path(), "worker", b"worker");
    let digest = hex::encode(Sha256::digest(&dependency.manifest));
    let mut application = source(root.path(), "web", b"web");
    let mut manifest = Manifest::parse(&application.manifest).unwrap();
    manifest.dependencies.push(Dependency {
        name: "worker".into(),
        digest,
    });
    application.manifest = serde_json::to_vec(&manifest).unwrap();
    assert!(
        store
            .install(
                &[InstallSource {
                    directory: application.directory.clone(),
                    manifest: application.manifest.clone(),
                    provenance: Provenance::Local
                }],
                &[],
                &[],
                "linux-x86_64"
            )
            .is_err()
    );
    store
        .install(&[application, dependency], &[], &[], "linux-x86_64")
        .unwrap();
    let old = store.resolution().unwrap();
    assert!(store.uninstall(&["worker".into()]).is_err());
    assert_eq!(old.packages, store.resolution().unwrap().packages);
    let update = source(root.path(), "worker", b"new worker");
    assert!(store.install(&[update], &[], &[], "linux-x86_64").is_err());
    assert_eq!(old.packages, store.resolution().unwrap().packages);
    store.acquire("web").unwrap();
    let mut corrupt = old.clone();
    corrupt.packages.remove("worker");
    std::fs::write(
        root.path().join("packages/resolution.json"),
        serde_json::to_vec(&corrupt).unwrap(),
    )
    .unwrap();
    assert!(
        store.collect().is_err(),
        "durable dependency corruption must stop GC before removal"
    );
    store
        .acquire_exact("worker", &old.packages["worker"].digest)
        .unwrap();
}

#[test]
fn manifest_rejects_escaping_case_aliases_devices_and_missing_interfaces() {
    let root = tempfile::tempdir().unwrap();
    let source = source(root.path(), "web", b"test");
    let base = Manifest::parse(&source.manifest).unwrap();
    for name in [
        "../outside",
        "/absolute",
        "C:/file",
        "a\\b",
        "NUL.txt",
        "a/../b",
        "a.",
        "a//b",
    ] {
        let mut manifest = base.clone();
        manifest.files[0].path = name.into();
        assert!(manifest.validate().is_err(), "{name}");
    }
    let mut duplicate = base.clone();
    let mut alias = duplicate.files[0].clone();
    alias.path = alias.path.to_ascii_uppercase();
    duplicate.files.push(alias);
    assert!(duplicate.validate().is_err());
    let interface = Interface {
        name: "session".into(),
        version: 1,
    };
    let mut required = base;
    required.requires.push(interface.clone());
    assert!(required.admit_interfaces(&[]).is_err());
    required.admit_interfaces(&[interface]).unwrap();
}

#[test]
fn interrupted_stages_do_not_exhaust_recovery_enumeration() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("packages");
    let store = PackageStore::open(&path).unwrap();
    for index in 0..257 {
        rsi_files_native_fs::create_absolute_directory_no_follow(
            &path.join("objects").join(format!("stage-{index:032x}")),
        )
        .unwrap();
    }
    store
        .install(
            &[source(root.path(), "web", b"ready")],
            &[],
            &[],
            "windows-x86_64",
        )
        .unwrap();
    assert_eq!(store.acquire("web").unwrap().manifest().name, "web");
}

#[test]
fn read_only_acquisition_shares_the_coordinator_and_pins_before_gc() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("packages");
    let store = PackageStore::open(&path).unwrap();
    store
        .install(
            &[source(root.path(), "web", b"ready")],
            &[],
            &[],
            "windows-x86_64",
        )
        .unwrap();
    let reader = std::fs::File::open(path.join("writer.lock")).unwrap();
    reader.lock_shared().unwrap();
    assert_eq!(store.resolution().unwrap().packages.len(), 1);
    let package = store.acquire("web").unwrap();
    assert!(matches!(store.collect(), Err(Error::Busy)));
    drop(reader);
    store.uninstall(&["web".into()]).unwrap();
    assert!(store.collect().unwrap().is_empty());
    drop(package);
    assert_eq!(store.collect().unwrap().len(), 1);
}

#[test]
fn signed_admission_requires_verified_manifest_and_retains_failed_download_high_water() {
    use ring::signature::KeyPair as _;
    let root = tempfile::tempdir().unwrap();
    let store = PackageStore::open(&root.path().join("packages")).unwrap();
    let key = ring::signature::Ed25519KeyPair::from_seed_unchecked(&[9; 32]).unwrap();
    let public = key.public_key().as_ref().try_into().unwrap();
    let mut input = source(root.path(), "web", b"first");
    let digest = hex::encode(Sha256::digest(&input.manifest));
    let sign = |sequence| {
        let bytes = serde_json::to_vec(&Repository {
            format: 1,
            name: "fixture".into(),
            sequence,
            issued: 100,
            expires: 200,
            packages: BTreeMap::from([(
                "web".into(),
                BTreeMap::from([(
                    "portable".into(),
                    RepositoryTarget {
                        digest: digest.clone(),
                        manifest: "web/package.json".into(),
                    },
                )]),
            )]),
        })
        .unwrap();
        SignedRepository {
            payload: hex::encode(&bytes),
            signature: hex::encode(key.sign(&bytes).as_ref()),
        }
    };
    let verified = store
        .accept_repository(&sign(2), &public, "fixture", 150)
        .unwrap();
    input.provenance = Provenance::Signed {
        repository: "fixture".into(),
        sequence: 2,
        payload: verified.receipt().digest.clone(),
    };
    // A durable receipt alone must not grant authority to install content.
    assert!(store.install(&[input], &[], &[], "windows-x86_64").is_err());
    assert!(store.resolution().unwrap().packages.is_empty());
    assert_eq!(store.resolution().unwrap().trust["fixture"].sequence, 2);
    assert!(
        store
            .accept_repository(&sign(1), &public, "fixture", 150)
            .is_err()
    );
    let verified = store
        .accept_repository(&sign(2), &public, "fixture", 150)
        .unwrap();
    let mut other = source(root.path(), "web", b"unauthorized");
    other.provenance = Provenance::Signed {
        repository: "fixture".into(),
        sequence: 2,
        payload: verified.receipt().digest.clone(),
    };
    assert!(
        store
            .install(&[other], &[verified], &[], "windows-x86_64")
            .is_err()
    );
    let verified = store
        .accept_repository(&sign(2), &public, "fixture", 150)
        .unwrap();
    let mut input = source(root.path(), "web", b"first");
    input.provenance = Provenance::Signed {
        repository: "fixture".into(),
        sequence: 2,
        payload: verified.receipt().digest.clone(),
    };
    store
        .install(&[input], &[verified], &[], "windows-x86_64")
        .unwrap();
    assert_eq!(store.acquire("web").unwrap().manifest().name, "web");
}

#[cfg(unix)]
#[test]
fn retained_store_parent_does_not_follow_a_replaced_root() {
    let root = tempfile::tempdir().unwrap();
    let selected = root.path().join("packages");
    let store = PackageStore::open(&selected).unwrap();
    store
        .install(
            &[source(root.path(), "web", b"original")],
            &[],
            &[],
            "linux-x86_64",
        )
        .unwrap();
    std::fs::rename(&selected, root.path().join("retained")).unwrap();
    std::fs::create_dir(&selected).unwrap();
    assert_eq!(store.acquire("web").unwrap().manifest().files[0].bytes, 8);
}

#[test]
fn publisher_and_consumer_share_manifest_acceptance_cases() {
    let cases: serde_json::Value =
        serde_json::from_str(include_str!("../manifest-conformance.json")).unwrap();
    for case in cases.as_array().unwrap() {
        let bytes = serde_json::to_vec(&case["manifest"]).unwrap();
        assert_eq!(
            Manifest::parse(&bytes).is_ok(),
            case["valid"].as_bool().unwrap(),
            "{}",
            case["name"]
        );
    }
}

#[test]
fn unknown_object_is_retained_with_actionable_recovery() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("packages");
    let store = PackageStore::open(&path).unwrap();
    std::fs::create_dir(path.join("objects/operator-backup")).unwrap();
    let failure = store.collect().unwrap_err().to_string();
    assert!(failure.contains("operator-backup") && failure.contains("move it outside"));
    std::fs::rename(
        path.join("objects/operator-backup"),
        root.path().join("retained-backup"),
    )
    .unwrap();
    assert!(store.collect().unwrap().is_empty());
}

#[test]
fn manifest_integer_tokens_and_duplicate_fields_are_not_silently_coerced() {
    let root = tempfile::tempdir().unwrap();
    let input = source(root.path(), "web", b"test");
    let bytes = String::from_utf8(input.manifest).unwrap();
    assert!(Manifest::parse(bytes.replace("\"format\":2", "\"format\":2.0").as_bytes()).is_err());
    assert!(
        Manifest::parse(
            bytes
                .replace("\"format\":2", "\"format\":2,\"format\":2")
                .as_bytes()
        )
        .is_err()
    );
}
