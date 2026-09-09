use super::{NativeAddonStore, fs, source};
use rsi::{
    MAXIMUM_NATIVE_ADDON_STATE_BYTES, MAXIMUM_NATIVE_ADDONS, NativeAddonError,
    NativeAddonStoreLimits,
};
use serde_json::json;
use std::os::unix::fs::PermissionsExt as _;

#[test]
fn incompatible_target_installs_without_becoming_enabled() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    let manifest = source(&root.join("source"), b"artifact");
    let text = fs::read_to_string(&manifest)
        .unwrap()
        .replace(&rsi::native_addon_target(), "different-target");
    fs::write(&manifest, text).unwrap();
    let store = NativeAddonStore::open(root.join("store")).unwrap();
    store.install(&manifest).unwrap();
    let before = store.snapshot().unwrap();
    assert!(matches!(
        store.enable("fixture.addon"),
        Err(rsi::NativeAddonError::UnsupportedTarget)
    ));
    assert_eq!(store.snapshot().unwrap(), before);
}

#[test]
fn durable_records_reject_bad_identity_duplicate_membership_and_unknown_format() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    let manifest = source(&root.join("source"), b"artifact");
    let store = NativeAddonStore::open(root.join("store")).unwrap();
    store.install(&manifest).unwrap();
    let index = root.join("store/state.json");
    let valid: serde_json::Value = serde_json::from_slice(&fs::read(&index).unwrap()).unwrap();
    let mut cases = Vec::new();
    for (field, value) in [
        ("id", json!("another.id")),
        ("plugin", json!("../plugin")),
        ("artifact_sha256", json!("A".repeat(64))),
        ("portable_services", json!(["duplicate", "duplicate"])),
        ("unknown", json!(true)),
    ] {
        let mut invalid = valid.clone();
        invalid["installed"]["fixture.addon"][field] = value;
        cases.push(serde_json::to_vec(&invalid).unwrap());
    }
    let mut invalid = valid.clone();
    invalid["format"] = json!(2);
    cases.push(serde_json::to_vec(&invalid).unwrap());
    let mut invalid = valid.clone();
    invalid["enabled"] = invalid["installed"].clone();
    invalid["installed"] = json!({});
    cases.push(serde_json::to_vec(&invalid).unwrap());
    let row = serde_json::to_string(&valid["installed"]["fixture.addon"]).unwrap();
    cases.push(format!(r#"{{"format":1,"revision":1,"installed":{{"fixture.addon":{row},"fixture.addon":{row}}},"enabled":{{}}}}"#).into_bytes());
    cases.push(vec![b' '; MAXIMUM_NATIVE_ADDON_STATE_BYTES + 1]);
    for bytes in cases {
        fs::write(&index, &bytes).unwrap();
        assert!(store.snapshot().is_err());
        assert!(store.disable("fixture.addon").is_err());
        assert_eq!(fs::read(&index).unwrap(), bytes);
    }
}

#[test]
fn installed_cardinality_and_revision_admission_precede_object_publication() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    let manifest = source(&root.join("source"), b"artifact");
    let store = NativeAddonStore::open(root.join("store")).unwrap();
    let row = serde_json::to_value(store.install(&manifest).unwrap().record.unwrap()).unwrap();
    let mut rows = serde_json::Map::new();
    for number in 0..MAXIMUM_NATIVE_ADDONS {
        let id = format!("fixture.{number}");
        let mut row = row.clone();
        row["id"] = json!(&id);
        rows.insert(id, row);
    }
    let index = root.join("store/state.json");
    let mut state = json!({"format":1,"revision":7,"installed":rows,"enabled":{}});
    fs::write(&index, serde_json::to_vec(&state).unwrap()).unwrap();
    assert_eq!(
        store.snapshot().unwrap().installed.len(),
        MAXIMUM_NATIVE_ADDONS
    );
    fs::write(root.join("source/artifact.bin"), b"unpublished").unwrap();
    assert!(matches!(
        store.install(&manifest),
        Err(NativeAddonError::Capacity(_))
    ));
    assert_eq!(fs::read_dir(root.join("store/objects")).unwrap().count(), 1);
    state["installed"]["fixture.addon"] = row.clone();
    fs::write(&index, serde_json::to_vec(&state).unwrap()).unwrap();
    assert!(store.snapshot().is_err());
    state["installed"] = json!({"fixture.addon":row});
    state["revision"] = json!(u64::MAX);
    let saturated = serde_json::to_vec(&state).unwrap();
    fs::write(&index, &saturated).unwrap();
    assert!(matches!(
        store.install(&manifest),
        Err(NativeAddonError::Capacity(_))
    ));
    assert!(matches!(
        store.enable("fixture.addon"),
        Err(NativeAddonError::Capacity(_))
    ));
    assert_eq!(fs::read(&index).unwrap(), saturated);
    assert_eq!(fs::read_dir(root.join("store/objects")).unwrap().count(), 1);
}

#[test]
fn encoded_state_admission_preserves_old_index_even_when_metadata_counts_fit() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    let manifest = source(&root.join("source"), b"artifact");
    let store = NativeAddonStore::open(root.join("store")).unwrap();
    let row = serde_json::to_value(store.install(&manifest).unwrap().record.unwrap()).unwrap();
    let keys: Vec<_> = (0..64)
        .map(|i| format!("{i:03}{}", "k".repeat(253)))
        .collect();
    let mut rows = serde_json::Map::new();
    for i in 0..62 {
        let id = format!("fixture.{i}");
        let mut record = row.clone();
        record["id"] = json!(&id);
        record["portable_services"] = json!(&keys);
        rows.insert(id, record);
    }
    let bytes = serde_json::to_vec(&json!({"format":1,"revision":1,"installed":rows,"enabled":{}}))
        .unwrap();
    assert!(bytes.len() <= MAXIMUM_NATIVE_ADDON_STATE_BYTES);
    assert!(MAXIMUM_NATIVE_ADDON_STATE_BYTES - bytes.len() < 16 * 1024);
    let index = root.join("store/state.json");
    fs::write(&index, &bytes).unwrap();
    assert_eq!(store.snapshot().unwrap().installed.len(), 62);
    assert!(matches!(
        store.enable("fixture.0"),
        Err(NativeAddonError::Capacity(_))
    ));
    assert_eq!(fs::read(index).unwrap(), bytes);
}

#[test]
fn copied_objects_are_addressed_by_actual_bytes_and_retained_after_uninstall() {
    use sha2::{Digest as _, Sha256};
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    let manifest = source(&root.join("source"), b"copied artifact");
    let store = NativeAddonStore::open(root.join("store")).unwrap();
    let receipt = store.install(&manifest).unwrap();
    assert_eq!(receipt.revision, 1);
    assert!(receipt.directory_synced.is_some());
    let row = receipt.record.unwrap();
    assert_eq!(
        row.artifact_sha256(),
        hex::encode(Sha256::digest(b"copied artifact"))
    );
    let object = root.join("store/objects").join(row.artifact_sha256());
    assert_eq!(fs::read(&object).unwrap(), b"copied artifact");
    assert_eq!(
        fs::metadata(&object).unwrap().permissions().mode() & 0o777,
        0o400
    );
    assert_eq!(
        fs::metadata(root.join("store/state.json"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
    store.uninstall("fixture.addon").unwrap();
    assert!(object.exists());
    assert!(!store.disable("fixture.addon").unwrap().changed);
    fs::set_permissions(&object, fs::Permissions::from_mode(0o600)).unwrap();
    fs::write(&object, b"altered-content").unwrap();
    assert!(matches!(
        store.install(&manifest),
        Err(NativeAddonError::DigestMismatch)
    ));
    assert!(store.snapshot().unwrap().installed.is_empty());
}

#[test]
fn aggregate_object_count_and_bytes_are_independently_bounded() {
    for limits in [
        NativeAddonStoreLimits {
            maximum_objects: 2,
            maximum_object_bytes: 5,
        },
        NativeAddonStoreLimits {
            maximum_objects: 1,
            maximum_object_bytes: 10,
        },
    ] {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        let manifest = source(&root.join("source"), b"123");
        let store = NativeAddonStore::with_limits(root.join("store"), limits).unwrap();
        store.install(&manifest).unwrap();
        let before = store.snapshot().unwrap();
        fs::write(root.join("source/artifact.bin"), b"456").unwrap();
        assert!(matches!(
            store.install(&manifest),
            Err(NativeAddonError::Capacity(_))
        ));
        assert_eq!(store.snapshot().unwrap(), before);
        assert_eq!(fs::read_dir(root.join("store/objects")).unwrap().count(), 1);
    }
}
