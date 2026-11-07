use rsi_packages::*;
use sha2::{Digest as _, Sha256};
use std::{collections::BTreeMap, sync::Arc};
fn hash(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}
fn index(bytes: &[u8]) -> &[u8] {
    let size = u32::from_le_bytes(bytes[8..12].try_into().unwrap()) as usize;
    &bytes[..12 + size]
}
fn tree(files: &[(&str, &[u8])]) -> Vec<u8> {
    let header = PayloadHeader {
        format: 1,
        files: files
            .iter()
            .map(|(path, bytes)| FileRecord {
                path: (*path).into(),
                bytes: bytes.len() as u64,
                digest: hash(bytes),
                executable: false,
            })
            .collect(),
    };
    let encoded = serde_json::to_vec(&header).unwrap();
    let mut bytes = b"RSITREE1".to_vec();
    bytes.extend(u32::try_from(encoded.len()).unwrap().to_le_bytes());
    bytes.extend(encoded);
    for (_, file) in files {
        bytes.extend(*file);
    }
    bytes
}
fn source(root: &std::path::Path, tag: &str, bytes: &[u8]) -> InstallSource {
    let directory = root.join(tag);
    std::fs::create_dir(&directory).unwrap();
    std::fs::write(directory.join("payload.rsi"), bytes).unwrap();
    let manifest = Manifest {
        format: 2,
        name: "plugin".into(),
        version: tag.into(),
        target: "portable".into(),
        dependencies: vec![],
        requires: vec![],
        files: vec![FileRecord {
            path: "payload.rsi".into(),
            bytes: bytes.len() as u64,
            digest: hash(bytes),
            executable: false,
        }],
        exports: BTreeMap::from([(
            "runtime".into(),
            Export {
                kind: ExportKind::PayloadTree,
                path: "payload.rsi".into(),
                index_digest: Some(hash(index(bytes))),
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
fn payload_is_immutable_and_its_lease_survives_selection_and_gc() {
    let root = tempfile::tempdir().unwrap();
    let store = PackageStore::open(&root.path().join("store")).unwrap();
    store
        .install(
            &[source(
                root.path(),
                "first",
                &tree(&[
                    ("node_modules/.package-lock.json", b"{}"),
                    ("entry.mjs", b"first"),
                ]),
            )],
            &[],
            &[],
            "windows-x86_64",
        )
        .unwrap();
    let package = Arc::new(store.acquire("plugin").unwrap());
    let payload = package.payload("runtime").unwrap();
    let old = package.digest().to_owned();
    drop(package);
    assert_eq!(
        std::fs::read(payload.path().join("entry.mjs")).unwrap(),
        b"first"
    );
    store
        .install(
            &[source(
                root.path(),
                "second",
                &tree(&[("entry.mjs", b"second")]),
            )],
            &[],
            &[],
            "windows-x86_64",
        )
        .unwrap();
    assert!(!store.collect().unwrap().contains(&old));
    let path = payload.path().to_owned();
    drop(payload);
    assert!(store.collect().unwrap().contains(&old));
    assert!(!path.exists());
}
#[test]
fn installed_payload_retains_only_its_index_and_one_body() {
    let root = tempfile::tempdir().unwrap();
    let store_root = root.path().join("store");
    let store = PackageStore::open(&store_root).unwrap();
    let body = vec![0x5a; 256 * 1024];
    let archive = tree(&[("lib/body", &body)]);
    store
        .install(
            &[source(root.path(), "source", &archive)],
            &[],
            &[],
            "portable",
        )
        .unwrap();
    let lease = Arc::new(store.acquire("plugin").unwrap());
    let payload = lease.payload("runtime").unwrap();
    assert_eq!(
        lease.export("runtime", ExportKind::PayloadTree).unwrap(),
        payload.path()
    );
    let retained = store_root
        .join("objects")
        .join(lease.digest())
        .join("content/payload.rsi");
    assert_eq!(std::fs::read(&retained).unwrap(), index(&archive));
    assert_eq!(
        std::fs::read(payload.path().join("lib/body")).unwrap(),
        body
    );
    assert_eq!(
        std::fs::metadata(retained).unwrap().len() + body.len() as u64,
        archive.len() as u64
    );
    drop(payload);
    drop(lease);
    drop(store);
    assert!(
        PackageStore::open(&store_root)
            .unwrap()
            .acquire("plugin")
            .is_ok()
    );
}

#[test]
fn payload_commitments_and_single_copy_capacity_are_manifest_contracts() {
    let root = tempfile::tempdir().unwrap();
    let input = source(root.path(), "source", &tree(&[("body", b"body")]));
    let original = Manifest::parse(&input.manifest).unwrap();
    let mut absent = original.clone();
    absent.exports.get_mut("runtime").unwrap().index_digest = None;
    assert!(absent.validate().is_err());
    let mut alias = original.clone();
    alias.exports.insert(
        "file".into(),
        Export {
            kind: ExportKind::ApplicationProfile,
            path: "payload.rsi".into(),
            index_digest: None,
        },
    );
    assert!(alias.validate().is_err());
    let mut assets = original.clone();
    assets.files[0].path = "lib/payload.rsi".into();
    assets.exports.get_mut("runtime").unwrap().path = "lib/payload.rsi".into();
    assets.exports.insert(
        "assets".into(),
        Export {
            kind: ExportKind::WebAssets,
            path: "lib".into(),
            index_digest: None,
        },
    );
    assert!(assets.validate().is_err());
    let mut capacity = original.clone();
    capacity.files.clear();
    capacity.exports.clear();
    for i in 0..=4 {
        let path = format!("tree-{i}");
        capacity.files.push(FileRecord {
            path: path.clone(),
            bytes: MAX_FILE_BYTES,
            digest: "a".repeat(64),
            executable: false,
        });
        capacity.exports.insert(
            path.clone(),
            Export {
                kind: ExportKind::PayloadTree,
                path,
                index_digest: Some("b".repeat(64)),
            },
        );
        assert_eq!(capacity.validate().is_ok(), i < 4);
    }
}

#[test]
fn bad_payload_does_not_publish_or_break_previous_selection() {
    let root = tempfile::tempdir().unwrap();
    let store = PackageStore::open(&root.path().join("store")).unwrap();
    store
        .install(
            &[source(
                root.path(),
                "good",
                &tree(&[("entry.mjs", b"good")]),
            )],
            &[],
            &[],
            "windows-x86_64",
        )
        .unwrap();
    let good = store.acquire("plugin").unwrap().digest().to_owned();
    for (index, bytes) in [
        tree(&[("../escape", b"bad")]),
        tree(&[("X/a", b"a"), ("x/b", b"b")]),
        tree(&[("lib/A/x", b""), ("lib/A/y", b""), ("lib/a/z", b"")]),
        tree(&[("lib", b""), ("lib/file", b"")]),
        tree(&[("lib/file", b""), ("LIB", b"")]),
        tree(&[("lib/file", b""), ("lib/FILE", b"")]),
        {
            let mut bytes = tree(&[("entry.mjs", b"same")]);
            *bytes.last_mut().unwrap() = b'!';
            bytes
        },
    ]
    .into_iter()
    .enumerate()
    {
        assert!(
            store
                .install(
                    &[source(root.path(), &format!("bad{index}"), &bytes)],
                    &[],
                    &[],
                    "windows-x86_64"
                )
                .is_err()
        );
        assert_eq!(store.acquire("plugin").unwrap().digest(), good);
    }
    for wrong_index in [false, true] {
        let mut input = source(
            root.path(),
            &format!("wrong-commitment-{wrong_index}"),
            &tree(&[("entry.mjs", b"valid body")]),
        );
        let mut manifest = Manifest::parse(&input.manifest).unwrap();
        if wrong_index {
            manifest.exports.get_mut("runtime").unwrap().index_digest = Some("0".repeat(64));
        } else {
            manifest.files[0].digest = "0".repeat(64);
        }
        input.manifest = serde_json::to_vec(&manifest).unwrap();
        assert!(matches!(
            store.install(&[input], &[], &[], "windows-x86_64"),
            Err(Error::Digest)
        ));
        assert_eq!(store.acquire("plugin").unwrap().digest(), good);
    }
}
#[test]
fn ordinary_acquisition_detects_unknown_entries_without_rescanning_on_process_start() {
    let root = tempfile::tempdir().unwrap();
    let store = PackageStore::open(&root.path().join("store")).unwrap();
    store
        .install(
            &[source(
                root.path(),
                "good",
                &tree(&[("entry.mjs", b"good")]),
            )],
            &[],
            &[],
            "windows-x86_64",
        )
        .unwrap();
    let digest = store.resolution().unwrap().packages["plugin"]
        .digest
        .clone();
    let lease = store.acquire_for_process("plugin", &digest).unwrap();
    #[cfg(unix)]
    {
        let payload = lease.payload("runtime").unwrap();
        let injected = payload.path().join(".rsi-namespace-pin");
        std::fs::write(&injected, b"undeclared").unwrap();
        assert!(store.acquire("plugin").is_err());
        std::fs::remove_file(injected).unwrap();
    }
    std::fs::write(
        lease
            .payload("runtime")
            .unwrap()
            .path()
            .join("injected.mjs"),
        b"bad",
    )
    .unwrap();
    assert!(store.acquire("plugin").is_err());
    let independent = PackageStore::open(&root.path().join("store")).unwrap();
    assert!(independent.acquire_for_process("plugin", &digest).is_ok());
    drop(lease);
    assert!(store.acquire_for_process("plugin", &digest).is_ok());
    assert!(store.acquire_exact("plugin", &digest).is_err());
}

#[test]
fn a_rewritten_index_and_matching_expansion_do_not_replace_the_admitted_record() {
    let root = tempfile::tempdir().unwrap();
    let store_root = root.path().join("store");
    let store = PackageStore::open(&store_root).unwrap();
    store
        .install(
            &[source(root.path(), "source", &tree(&[("tool", b"old")]))],
            &[],
            &[],
            "windows-x86_64",
        )
        .unwrap();
    let lease = store.acquire("plugin").unwrap();
    let object = store_root.join("objects").join(lease.digest());
    let archive = object.join("content/payload.rsi");
    let body = object.join("payloads/runtime/tool");
    drop(lease);
    #[cfg(unix)]
    for path in [&archive, &body] {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).unwrap();
    }
    std::fs::write(&archive, index(&tree(&[("tool", b"new")]))).unwrap();
    std::fs::write(body, b"new").unwrap();
    assert!(matches!(store.acquire("plugin"), Err(Error::Digest)));
}

#[test]
fn acquisition_authenticates_the_exact_header_bytes_even_when_every_file_hash_matches() {
    let root = tempfile::tempdir().unwrap();
    let store_root = root.path().join("store");
    let store = PackageStore::open(&store_root).unwrap();
    let mut archive = tree(&[("one", b"same")]);
    let size = u32::from_le_bytes(archive[8..12].try_into().unwrap());
    archive[8..12].copy_from_slice(&(size + 1).to_le_bytes());
    archive.insert(12, b' ');
    store
        .install(
            &[source(root.path(), "spaced", &archive)],
            &[],
            &[],
            "portable",
        )
        .unwrap();
    let digest = store.acquire("plugin").unwrap().digest().to_owned();
    let path = store_root
        .join("objects")
        .join(digest)
        .join("content/payload.rsi");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
    }
    archive[12] = b'\t';
    std::fs::write(path, index(&archive)).unwrap();
    assert!(matches!(store.acquire("plugin"), Err(Error::Digest)));
}
#[test]
fn arbitrary_file_order_preserves_directory_authority_and_checks_every_file() {
    let root = tempfile::tempdir().unwrap();
    let store = PackageStore::open(&root.path().join("store")).unwrap();
    let files: &[(&str, &[u8])] = &[
        ("a/b/c/same.mjs", b"deep"),
        ("a/b/same.mjs", b"up"),
        ("a/d/same.mjs", b"sibling"),
        ("same.mjs", b"root"),
        ("a/b/c/later.mjs", b"back"),
        ("z/same.mjs", b"other"),
    ];
    store
        .install(
            &[source(root.path(), "ordered", &tree(files))],
            &[],
            &[],
            "portable",
        )
        .unwrap();
    let lease = Arc::new(store.acquire("plugin").unwrap());
    let payload = lease.payload("runtime").unwrap();
    for (path, bytes) in files {
        assert_eq!(std::fs::read(payload.path().join(path)).unwrap(), *bytes);
    }
    let altered = payload.path().join("a/b/c/later.mjs");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(&altered, std::fs::Permissions::from_mode(0o600)).unwrap();
    }
    std::fs::write(altered, b"evil").unwrap();
    assert!(store.acquire("plugin").is_err());
}
#[test]
fn payload_file_count_does_not_reuse_the_small_manifest_limit() {
    let root = tempfile::tempdir().unwrap();
    let store = PackageStore::open(&root.path().join("store")).unwrap();
    let names: Vec<_> = (0..4097).map(|i| format!("modules/p{i}.js")).collect();
    let files: Vec<_> = names
        .iter()
        .map(|name| (name.as_str(), b"x".as_slice()))
        .collect();
    store
        .install(
            &[source(root.path(), "many", &tree(&files))],
            &[],
            &[],
            "windows-x86_64",
        )
        .unwrap();
    let lease = Arc::new(store.acquire("plugin").unwrap());
    let last = lease
        .payload("runtime")
        .unwrap()
        .path()
        .join("modules/p4096.js");
    assert_eq!(std::fs::read(&last).unwrap(), b"x");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(&last, std::fs::Permissions::from_mode(0o600)).unwrap();
    }
    std::fs::write(&last, b"y").unwrap();
    assert!(matches!(store.acquire("plugin"), Err(Error::Digest)));
    std::fs::write(last, b"x").unwrap();
    assert_eq!(store.acquire("plugin").unwrap().digest(), lease.digest());
}

fn process_source(
    root: &std::path::Path,
    tag: &str,
    runtime: &InstallSource,
    reference: &str,
) -> InstallSource {
    use serde_json::json;
    let mut candidate = source(root, tag, &tree(&[("bridge.mjs", b"bridge")]));
    let descriptor = serde_json::to_vec(&json!({"format":1,
        "cwd":{"package":"plugin","payload":"runtime","path":""},
        "arguments":[{"kind":"path","location":{"package":"plugin","payload":"runtime","path":reference}},
            {"kind":"generation"},{"kind":"path","location":{"payload":"runtime","path":"bridge.mjs"}}]})).unwrap();
    std::fs::write(candidate.directory.join("process.json"), &descriptor).unwrap();
    let mut manifest = Manifest::parse(&candidate.manifest).unwrap();
    manifest.name = "bridge".into();
    manifest.dependencies = vec![Dependency {
        name: "plugin".into(),
        digest: hash(&runtime.manifest),
    }];
    manifest.requires = vec![Interface {
        name: "rsi.extension.process".into(),
        version: 1,
    }];
    manifest.files.push(FileRecord {
        path: "process.json".into(),
        bytes: descriptor.len() as u64,
        digest: hash(&descriptor),
        executable: false,
    });
    manifest.exports.insert(
        "process".into(),
        Export {
            kind: ExportKind::ProcessExtension,
            path: "process.json".into(),
            index_digest: None,
        },
    );
    candidate.manifest = serde_json::to_vec(&manifest).unwrap();
    candidate
}

#[test]
fn fresh_process_launch_skips_archives_and_unused_files_but_requires_its_entry() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("store");
    let store = PackageStore::open(&path).unwrap();
    let runtime = source(
        root.path(),
        "runtime",
        &tree(&[("host.mjs", b"entry"), ("unused.txt", b"unused")]),
    );
    let runtime_digest = hash(&runtime.manifest);
    let bridge = process_source(root.path(), "bridge", &runtime, "host.mjs");
    let digest = hash(&bridge.manifest);
    store
        .install(
            &[runtime, bridge],
            &[],
            &[Interface {
                name: "rsi.extension.process".into(),
                version: 1,
            }],
            "portable",
        )
        .unwrap();
    drop(store);
    let object = path.join("objects").join(runtime_digest);
    std::fs::remove_file(object.join("content/payload.rsi")).unwrap();
    std::fs::remove_file(object.join("payloads/runtime/unused.txt")).unwrap();
    let store = PackageStore::open(&path).unwrap();
    let process = store
        .process_extension("bridge", &digest, "process", "fresh")
        .unwrap();
    assert_eq!(std::fs::read(&process.arguments[0]).unwrap(), b"entry");
    assert!(store.acquire_exact("bridge", &digest).is_err());
    std::fs::remove_file(object.join("payloads/runtime/host.mjs")).unwrap();
    assert!(
        store
            .process_extension("bridge", &digest, "process", "live")
            .is_err()
    );
    drop(process);
    assert!(
        store
            .process_extension("bridge", &digest, "process", "new")
            .is_err()
    );
}

#[test]
fn process_admission_authenticates_every_manifest_and_requires_payload_roots() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("store");
    let store = PackageStore::open(&path).unwrap();
    let runtime = source(root.path(), "runtime", &tree(&[("host.mjs", b"entry")]));
    let dependency = hash(&runtime.manifest);
    let bridge = process_source(root.path(), "bridge", &runtime, "host.mjs");
    let digest = hash(&bridge.manifest);
    store
        .install(
            &[runtime, bridge],
            &[],
            &[Interface {
                name: "rsi.extension.process".into(),
                version: 1,
            }],
            "portable",
        )
        .unwrap();
    for current in [&digest, &dependency] {
        let object = path.join("objects").join(current);
        let manifest = object.join("manifest.json");
        let bytes = std::fs::read(&manifest).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::set_permissions(&manifest, std::fs::Permissions::from_mode(0o600)).unwrap();
        }
        std::fs::write(&manifest, b"{}").unwrap();
        assert!(matches!(
            store.acquire_for_process("bridge", &digest),
            Err(Error::Digest)
        ));
        std::fs::write(&manifest, bytes).unwrap();
        let payload = object.join("payloads/runtime");
        let moved = object.join("payloads/moved");
        std::fs::rename(&payload, &moved).unwrap();
        assert!(store.acquire_for_process("bridge", &digest).is_err());
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(&moved, &payload).unwrap();
            assert!(store.acquire_for_process("bridge", &digest).is_err());
            std::fs::remove_file(&payload).unwrap();
        }
        std::fs::rename(moved, payload).unwrap();
    }
    assert!(store.acquire_for_process("wrong-name", &digest).is_err());
    assert!(
        store
            .process_extension("bridge", &digest, "process", "restored")
            .is_ok()
    );
}

#[test]
fn each_launch_authenticates_its_descriptor_even_with_a_live_lease() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("store");
    let store = PackageStore::open(&path).unwrap();
    let runtime = source(root.path(), "runtime", &tree(&[("host.mjs", b"entry")]));
    let bridge = process_source(root.path(), "bridge", &runtime, "host.mjs");
    let digest = hash(&bridge.manifest);
    store
        .install(
            &[runtime, bridge],
            &[],
            &[Interface {
                name: "rsi.extension.process".into(),
                version: 1,
            }],
            "portable",
        )
        .unwrap();
    let process = store
        .process_extension("bridge", &digest, "process", "live")
        .unwrap();
    let descriptor = path
        .join("objects")
        .join(&digest)
        .join("content/process.json");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(&descriptor, std::fs::Permissions::from_mode(0o600)).unwrap();
    }
    std::fs::write(descriptor, b"{}").unwrap();
    assert!(matches!(
        store.process_extension("bridge", &digest, "process", "cached"),
        Err(Error::Digest)
    ));
    drop(process);
    assert!(matches!(
        store.process_extension("bridge", &digest, "process", "fresh"),
        Err(Error::Digest)
    ));
}

#[test]
fn process_paths_retain_exact_dependencies_and_reject_invalid_updates_before_selection() {
    let root = tempfile::tempdir().unwrap();
    let store = PackageStore::open(&root.path().join("store")).unwrap();
    let first = source(root.path(), "runtime-one", &tree(&[("host.mjs", b"first")]));
    let bridge = process_source(root.path(), "bridge-one", &first, "host.mjs");
    let old = hash(&bridge.manifest);
    let interfaces = [Interface {
        name: "rsi.extension.process".into(),
        version: 1,
    }];
    store
        .install(&[first, bridge], &[], &interfaces, "windows-x86_64")
        .unwrap();
    let process = store
        .process_extension("bridge", &old, "process", "generation-one")
        .unwrap();
    assert_eq!(std::fs::read(&process.arguments[0]).unwrap(), b"first");
    assert_eq!(process.arguments[1], "generation-one");
    let concurrent = store
        .process_extension("bridge", &old, "process", "separate-generation")
        .unwrap();
    assert!(std::sync::Arc::ptr_eq(
        &concurrent.package,
        &process.package
    ));
    assert_eq!(concurrent.arguments[1], "separate-generation");
    assert!(store.acquire_for_process("other-name", &old).is_err());
    drop(concurrent);
    assert_eq!(std::fs::read(&process.arguments[2]).unwrap(), b"bridge");
    let dependency = &process.package.manifest().dependencies[0].name;
    let retained = process
        .package
        .dependency_payload(dependency, "runtime")
        .unwrap();
    assert_eq!(
        std::fs::read(retained.path().join("host.mjs")).unwrap(),
        b"first"
    );
    assert!(
        process
            .package
            .dependency_payload(dependency, "process")
            .is_err()
    );
    assert!(
        process
            .package
            .dependency_payload("unselected", "runtime")
            .is_err()
    );
    assert_eq!(
        std::path::Path::new(&process.arguments[0]).parent(),
        Some(process.cwd.as_path())
    );
    let second = source(
        root.path(),
        "runtime-two",
        &tree(&[("host.mjs", b"second")]),
    );
    let invalid = process_source(root.path(), "bridge-invalid", &second, "missing.mjs");
    assert!(
        store
            .install(&[second, invalid], &[], &interfaces, "windows-x86_64")
            .is_err()
    );
    assert_eq!(store.resolution().unwrap().packages["bridge"].digest, old);
    let second = source(
        root.path(),
        "runtime-three",
        &tree(&[("host.mjs", b"second")]),
    );
    let bridge = process_source(root.path(), "bridge-two", &second, "host.mjs");
    let new = hash(&bridge.manifest);
    store
        .install(&[second, bridge], &[], &interfaces, "windows-x86_64")
        .unwrap();
    store.collect().unwrap();
    assert_eq!(std::fs::read(&process.arguments[0]).unwrap(), b"first");
    let current = store
        .process_extension("bridge", &new, "process", "generation-two")
        .unwrap();
    assert_eq!(std::fs::read(&current.arguments[0]).unwrap(), b"second");
    let path = process.arguments[0].clone();
    drop(process);
    assert!(!store.collect().unwrap().contains(&old));
    drop(retained);
    assert!(store.collect().unwrap().contains(&old));
    assert!(!std::path::Path::new(&path).exists());
}

#[test]
fn process_descriptor_rejects_escape_unknown_fields_and_unsupported_format() {
    use serde_json::json;
    let valid = json!({"format":1,"cwd":{"payload":"runtime","path":""},"arguments":[]});
    for (field, value) in [
        ("format", json!(2)),
        ("extra", json!(true)),
        (
            "arguments",
            json!([{"kind":"path","location":{"payload":"runtime","path":"../outside"}}]),
        ),
    ] {
        let mut invalid = valid.clone();
        invalid[field] = value;
        assert!(ProcessExtension::parse(&serde_json::to_vec(&invalid).unwrap()).is_err());
    }
}

#[test]
fn importer_role_and_protocol_are_admitted_before_selection() {
    let root = tempfile::tempdir().unwrap();
    let store = PackageStore::open(&root.path().join("store")).unwrap();
    let runtime = source(
        root.path(),
        "import-runtime",
        &tree(&[("host.mjs", b"host")]),
    );
    let mut importer = process_source(root.path(), "importer", &runtime, "host.mjs");
    let mut manifest = Manifest::parse(&importer.manifest).unwrap();
    manifest.exports.get_mut("process").unwrap().kind = ExportKind::PackageImporter;
    importer.manifest = serde_json::to_vec(&manifest).unwrap();
    let interfaces = [
        Interface {
            name: "rsi.extension.process".into(),
            version: 1,
        },
        importer_protocol_descriptor(),
    ];
    assert!(
        store
            .install(&[runtime, importer], &[], &interfaces, "portable")
            .is_err()
    );
    assert!(store.resolution().unwrap().packages.is_empty());
    let runtime = source(
        root.path(),
        "import-runtime-good",
        &tree(&[("host.mjs", b"host")]),
    );
    let mut importer = process_source(root.path(), "importer-good", &runtime, "host.mjs");
    let mut manifest = Manifest::parse(&importer.manifest).unwrap();
    manifest.exports.get_mut("process").unwrap().kind = ExportKind::PackageImporter;
    manifest.requires.push(importer_protocol_descriptor());
    importer.manifest = serde_json::to_vec(&manifest).unwrap();
    let digest = hash(&importer.manifest);
    store
        .install(&[runtime, importer], &[], &interfaces, "portable")
        .unwrap();
    assert!(
        store
            .process_extension("bridge", &digest, "process", "wrong-role")
            .is_err()
    );
    let resolved = store
        .package_importer("bridge", &digest, "process", "import-generation")
        .unwrap();
    assert_eq!(resolved.arguments[1], "import-generation");
    assert_eq!(std::fs::read(&resolved.arguments[0]).unwrap(), b"host");
}
