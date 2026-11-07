use rsi_packages::*;
use sha2::{Digest as _, Sha256};
use std::collections::BTreeMap;

fn temporary_root() -> tempfile::TempDir {
    let root = std::env::temp_dir();
    #[cfg(not(windows))]
    let root = root.canonicalize().unwrap();
    tempfile::tempdir_in(root).unwrap()
}

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
                index_digest: None,
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
fn unrelated_installs_and_rejected_publications_leave_retired_objects_for_gc() {
    let root = temporary_root();
    let store = PackageStore::open(&root.path().join("packages")).unwrap();
    let install =
        |name, bytes| store.install(&[source(root.path(), name, bytes)], &[], &[], "portable");
    let retired = install("web", b"old").unwrap().packages["web"]
        .digest
        .clone();
    install("web", b"new").unwrap();
    install("side", b"side").unwrap();
    let object = root.path().join("packages/objects").join(&retired);
    assert!(
        object.is_dir(),
        "an unrelated publication collected a reusable generation"
    );
    let failed = store.install_checked(
        &[source(root.path(), "side", b"rejected")],
        &[],
        &[],
        "portable",
        || Err(Error::Invalid("cancelled".into())),
    );
    assert!(failed.is_err());
    assert_eq!(store.collect().unwrap(), [retired]);
}

#[test]
fn object_pressure_reclaims_retired_generations_and_keeps_pinned_bytes() {
    let root = temporary_root();
    let store = PackageStore::open(&root.path().join("packages")).unwrap();
    let batch = |generation| {
        (0..128)
            .map(|n| source(root.path(), &format!("p-{n}"), &[generation]))
            .collect::<Vec<_>>()
    };
    let initial = store.install(&batch(1), &[], &[], "portable").unwrap();
    let held = store.acquire("p-0").unwrap();
    store.install(&batch(2), &[], &[], "portable").unwrap();
    let objects = root.path().join("packages/objects");
    assert!(objects.join(&initial.packages["p-1"].digest).exists());
    store
        .install(
            &[source(root.path(), "p-1", b"third")],
            &[],
            &[],
            "portable",
        )
        .unwrap();
    assert_eq!(
        initial
            .packages
            .values()
            .filter(|entry| !objects.join(&entry.digest).exists())
            .count(),
        1,
        "one free slot suffices"
    );
    assert_eq!(
        std::fs::read(held.export("main", ExportKind::ApplicationProfile).unwrap()).unwrap(),
        [1]
    );
}

#[test]
fn impossible_byte_reservation_preserves_retired_objects_and_rejects_before_copying() {
    let root = temporary_root();
    let store = PackageStore::open(&root.path().join("packages")).unwrap();
    let retired = store
        .install(&[source(root.path(), "web", b"one")], &[], &[], "portable")
        .unwrap();
    store
        .install(&[source(root.path(), "web", b"two")], &[], &[], "portable")
        .unwrap();
    let mut full = source(root.path(), "full", b"");
    let mut manifest = Manifest::parse(&full.manifest).unwrap();
    let file = manifest.files[0].clone();
    for i in 1..4 {
        manifest.files.push(FileRecord {
            path: format!("part-{i}"),
            ..file.clone()
        });
    }
    for file in &mut manifest.files {
        file.bytes = MAX_FILE_BYTES;
    }
    full.manifest = serde_json::to_vec(&manifest).unwrap();
    let error = store.install(&[full], &[], &[], "portable").unwrap_err();
    assert!(
        error.to_string().contains("installed object capacity"),
        "{error}"
    );
    assert_eq!(
        store.collect().unwrap(),
        [retired.packages["web"].digest.clone()]
    );
}

#[test]
fn reselection_reuses_a_verified_object_and_repairs_a_damaged_unleased_one() {
    let root = temporary_root();
    let path = root.path().join("packages");
    let store = PackageStore::open(&path).unwrap();
    let first = source(root.path(), "app", b"original");
    let install = || store.install(std::slice::from_ref(&first), &[], &[], "portable");
    let installed = install().unwrap();
    let digest = &installed.packages["app"].digest;
    let content = path
        .join("objects")
        .join(digest)
        .join("content/application.toml");
    store.uninstall(&["app".into()]).unwrap();
    std::fs::remove_file(first.directory.join("application.toml")).unwrap();
    assert_eq!(&install().unwrap().packages["app"].digest, digest);
    store.uninstall(&["app".into()]).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(&content, std::fs::Permissions::from_mode(0o600)).unwrap();
    }
    std::fs::write(&content, b"tampered").unwrap();
    std::fs::write(first.directory.join("application.toml"), b"original").unwrap();
    install().unwrap();
    assert_eq!(std::fs::read(content).unwrap(), b"original");
}

#[test]
fn profiles_retain_independent_versions_and_their_dependencies_through_restart_and_gc() {
    let root = temporary_root();
    let path = root.path().join("packages");
    let store = PackageStore::open(&path).unwrap();
    let install = |bytes: &[u8]| {
        let dependency = source(root.path(), "runtime", bytes);
        let digest = hex::encode(Sha256::digest(&dependency.manifest));
        let mut package = source(root.path(), "extension", bytes);
        let mut manifest = Manifest::parse(&package.manifest).unwrap();
        manifest.dependencies.push(Dependency {
            name: "runtime".into(),
            digest: digest.clone(),
        });
        package.manifest = serde_json::to_vec(&manifest).unwrap();
        let result = store
            .install(&[dependency, package], &[], &[], "portable")
            .unwrap();
        (result.packages["extension"].digest.clone(), digest)
    };
    let (first, first_runtime) = install(b"one");
    let held = store.acquire_for_process("extension", &first).unwrap();
    let (second, second_runtime) = install(b"second");
    let a = BTreeMap::from([(first.clone(), "extension".into())]);
    let b = BTreeMap::from([(second.clone(), "extension".into())]);
    store
        .replace_profile_roots("one", &BTreeMap::new(), &a)
        .unwrap();
    store
        .replace_profile_roots("two", &BTreeMap::new(), &b)
        .unwrap();
    drop(held);
    drop(store);
    let store = PackageStore::open(&path).unwrap();
    store
        .uninstall(&["extension".into(), "runtime".into()])
        .unwrap();
    assert!(store.collect().unwrap().is_empty());
    assert_eq!(store.profile_roots("one").unwrap(), a);
    assert!(
        store
            .replace_profile_roots("one", &BTreeMap::new(), &b)
            .is_err()
    );
    assert!(
        store
            .replace_profile_roots(
                "two",
                &b,
                &BTreeMap::from([("f".repeat(64), "extension".into())])
            )
            .is_err()
    );
    store
        .replace_profile_roots("one", &a, &BTreeMap::new())
        .unwrap();
    let mut reclaimed = store.collect().unwrap();
    reclaimed.sort();
    let mut expected = vec![first, first_runtime];
    expected.sort();
    assert_eq!(reclaimed, expected);
    assert_eq!(store.profile_roots("two").unwrap(), b);
    store
        .replace_profile_roots("two", &b, &BTreeMap::new())
        .unwrap();
    let mut reclaimed = store.collect().unwrap();
    reclaimed.sort();
    let mut expected = vec![second, second_runtime];
    expected.sort();
    assert_eq!(reclaimed, expected);
}

#[test]
fn candidate_admission_reserves_the_profile_without_changing_cli_or_other_profiles() {
    let root = temporary_root();
    let store = PackageStore::open(&root.path().join("store")).unwrap();
    let global = store
        .install(
            &[source(root.path(), "runtime", b"shared")],
            &[],
            &[],
            "portable",
        )
        .unwrap();
    let input = |bytes: &[u8]| {
        let private = source(root.path(), "private", bytes);
        let mut plugin = source(root.path(), "extension", bytes);
        let mut manifest = Manifest::parse(&plugin.manifest).unwrap();
        manifest.dependencies = vec![
            Dependency {
                name: "private".into(),
                digest: hex::encode(Sha256::digest(&private.manifest)),
            },
            Dependency {
                name: "runtime".into(),
                digest: global.packages["runtime"].digest.clone(),
            },
        ];
        plugin.manifest = serde_json::to_vec(&manifest).unwrap();
        vec![private, plugin]
    };
    let empty = BTreeMap::new();
    let first = store
        .stage_for_profile("one", &empty, &input(b"first"), &[], "portable", || Ok(()))
        .unwrap();
    let second = store
        .stage_for_profile("two", &empty, &input(b"second"), &[], "portable", || Ok(()))
        .unwrap();
    assert_eq!(
        first.roots,
        BTreeMap::from([(first.packages["extension"].clone(), "extension".into())])
    );
    assert_eq!(store.resolution().unwrap().packages, global.packages);
    let candidate = store
        .stage_for_profile(
            "one",
            &first.roots,
            &input(b"third"),
            &[],
            "portable",
            || Ok(()),
        )
        .unwrap();
    assert_eq!(candidate.roots.len(), 2);
    assert!(
        store
            .stage_for_profile(
                "one",
                &first.roots,
                &input(b"conflict"),
                &[],
                "portable",
                || Ok(())
            )
            .is_err()
    );
    store
        .replace_profile_roots("one", &candidate.roots, &first.roots)
        .unwrap();
    let freed = store.collect().unwrap();
    assert!(
        candidate
            .packages
            .values()
            .all(|digest| freed.contains(digest))
    );
    assert!(
        store
            .stage_for_profile(
                "two",
                &second.roots,
                &input(b"cancel"),
                &[],
                "portable",
                || Err(Error::Invalid("cancelled".into()))
            )
            .is_err()
    );
    assert_eq!(store.profile_roots("two").unwrap(), second.roots);
    assert!(store.collect().unwrap().is_empty());
}

#[test]
fn producer_fence_rejects_after_staging_without_changing_selection() {
    let root = temporary_root();
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
    let root = temporary_root();
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
    let root = temporary_root();
    let source = source(root.path(), "core", b"length declaration");
    let mut manifest: Manifest = serde_json::from_slice(&source.manifest).unwrap();
    manifest.files[0].bytes = 1_601_116_312; // Measured ordinary Linux debug CLI.
    Manifest::parse(&serde_json::to_vec(&manifest).unwrap()).unwrap();
    manifest.files[0].bytes = MAX_FILE_BYTES + 1;
    assert!(Manifest::parse(&serde_json::to_vec(&manifest).unwrap()).is_err());
}

#[test]
fn update_keeps_leased_bytes_and_failed_install_keeps_resolution() {
    let root = temporary_root();
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
    let root = temporary_root();
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
    let root = temporary_root();
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
fn purge_admits_supported_interrupted_stages_and_preflights_before_deletion() {
    let root = temporary_root();
    let path = root.path().join("packages");
    let store = PackageStore::open(&path).unwrap();
    for index in 0..257 {
        rsi_files_native_fs::create_absolute_directory_no_follow(
            &path.join("objects").join(format!("stage-{index:032x}")),
        )
        .unwrap();
    }
    store.into_purge().unwrap().purge().unwrap();
    let remaining: Vec<_> = std::fs::read_dir(path.join("objects"))
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .collect();
    #[cfg(windows)]
    assert_eq!(remaining, [std::ffi::OsString::from(".rsi-namespace-pin")]);
    #[cfg(not(windows))]
    assert!(remaining.is_empty());

    let store = PackageStore::open(&path).unwrap();
    let stage = path.join("objects").join(format!("stage-{:032x}", 0));
    rsi_files_native_fs::create_absolute_directory_no_follow(&stage).unwrap();
    std::fs::write(stage.join("payload"), b"preserved").unwrap();
    rsi_files_native_fs::create_absolute_directory_no_follow(&path.join("objects/unknown"))
        .unwrap();
    assert!(store.into_purge().is_err());
    assert_eq!(std::fs::read(stage.join("payload")).unwrap(), b"preserved");
}

#[test]
fn interrupted_stages_do_not_exhaust_recovery_enumeration() {
    let root = temporary_root();
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
    let root = temporary_root();
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
    let root = temporary_root();
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
    let root = temporary_root();
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
    let root = temporary_root();
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
    let root = temporary_root();
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

#[test]
fn offline_purge_excludes_users_and_writers_and_preserves_unknown_root_files() {
    let root = temporary_root();
    let path = root.path().join("packages");
    let store = PackageStore::open(&path).unwrap();
    let installed = store
        .install(
            &[source(root.path(), "web", b"payload")],
            &[],
            &[],
            "windows-x86_64",
        )
        .unwrap();
    let lease = store.acquire("web").unwrap();
    assert!(PackageStore::open(&path).unwrap().into_purge().is_err());
    assert!(
        path.join("objects")
            .join(&installed.packages["web"].digest)
            .exists()
    );
    drop(lease);
    std::fs::write(path.join("user-file"), "keep").unwrap();
    let purge = PackageStore::open(&path).unwrap().into_purge().unwrap();
    assert!(
        store
            .install(
                &[source(root.path(), "other", b"new")],
                &[],
                &[],
                "windows-x86_64"
            )
            .is_err()
    );
    purge.purge().unwrap();
    assert!(
        !path
            .join("objects")
            .join(&installed.packages["web"].digest)
            .exists()
    );
    assert!(!path.join("resolution.json").exists());
    assert!(
        path.join("leases")
            .join(&installed.packages["web"].digest)
            .exists()
    );
    assert_eq!(std::fs::read(path.join("user-file")).unwrap(), b"keep");
}

#[test]
fn offline_purge_removes_multiple_large_abandoned_stages_in_one_pass() {
    let root = temporary_root();
    let path = root.path().join("packages");
    let store = PackageStore::open(&path).unwrap();
    // Each interrupted stage is within the per-object file limit; together they
    // exceed the former unrelated 16384-entry cap on the whole store.
    for object in 0..5 {
        let stage = path.join("objects").join(format!("stage-{object:032x}"));
        std::fs::create_dir(&stage).unwrap();
        for file in 0..3400 {
            std::fs::write(stage.join(format!("{file}")), []).unwrap();
        }
    }
    store.into_purge().unwrap().purge().unwrap();
    let remaining: Vec<_> = std::fs::read_dir(path.join("objects"))
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .collect();
    #[cfg(windows)]
    assert_eq!(remaining, [std::ffi::OsString::from(".rsi-namespace-pin")]);
    #[cfg(not(windows))]
    assert!(remaining.is_empty());
}
