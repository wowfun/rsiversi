use super::*;

#[test]
fn new_core_can_remove_legacy_selections_wait_for_leases_and_reinstall_without_resetting_trust() {
    let temp = tempfile::tempdir_in(std::env::temp_dir().canonicalize().unwrap()).unwrap();
    let store = PackageStore::open(&temp.path().join("packages")).unwrap();
    let payload = b"new";
    let manifest = |format, name| {
        serde_json::to_vec(&serde_json::json!({
        "format":format,"name":name,"version":"1","target":"portable",
        "dependencies":[],"requires":[],"exports":{},
        "files":[{"path":"data","bytes":3,"digest":hex::encode(Sha256::digest(payload)),"executable":false}]
    })).unwrap()
    };
    let mut legacy = Resolution::default();
    legacy.trust.insert(
        "fixture".into(),
        TrustReceipt {
            repository: "fixture".into(),
            sequence: 7,
            digest: "b".repeat(64),
        },
    );
    for name in ["core", "web"] {
        let bytes = manifest(1, name);
        let digest = hex::encode(Sha256::digest(&bytes));
        store
            .objects
            .child(Path::new(&digest), true)
            .unwrap()
            .write("manifest.json", &bytes, false)
            .unwrap();
        legacy.packages.insert(
            name.into(),
            Selection {
                digest,
                provenance: Provenance::Local,
            },
        );
    }
    store
        .root
        .write(
            "resolution.json",
            &serde_json::to_vec(&legacy).unwrap(),
            true,
        )
        .unwrap();
    let leased_digest = legacy.packages["web"].digest.clone();
    let legacy_lease = store.leases.lock(&leased_digest, true).unwrap();
    assert_eq!(store.resolution().unwrap().packages.len(), 2);
    assert!(
        store
            .acquire("core")
            .unwrap_err()
            .to_string()
            .contains("expected 2")
    );
    let source = temp.path().join("input");
    std::fs::create_dir(&source).unwrap();
    std::fs::write(source.join("data"), payload).unwrap();
    let inputs: Vec<_> = ["core", "web"]
        .into_iter()
        .map(|name| InstallSource {
            directory: source.clone(),
            manifest: manifest(2, name),
            provenance: Provenance::Local,
        })
        .collect();
    assert!(store.install(&inputs, &[], &[], "windows-x86_64").is_err());
    store.uninstall(&["core".into(), "web".into()]).unwrap();
    assert_eq!(
        store.collect().unwrap(),
        [legacy.packages["core"].digest.clone()]
    );
    assert!(
        store.install(&inputs, &[], &[], "windows-x86_64").is_err(),
        "a live legacy object still prevents admission"
    );
    drop(legacy_lease);
    assert_eq!(store.collect().unwrap(), [leased_digest]);
    let current = store.install(&inputs, &[], &[], "windows-x86_64").unwrap();
    assert_eq!(current.packages.len(), 2);
    assert_eq!(current.trust, legacy.trust);
    assert_eq!(store.acquire("core").unwrap().manifest().format, 2);
}
