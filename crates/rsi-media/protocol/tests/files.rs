use rsi_media_protocol::{FileRef, MediaId};
use serde_json::json;

#[test]
fn verbatim_references_preserve_names_identity_and_empty_files() {
    let id = MediaId::new("a".repeat(64)).unwrap();
    for (name, bytes) in [
        ("报告.pdf", 0),
        ("_CON.txt", 9_007_199_254_740_991),
        ("input.bin", 1024),
    ] {
        let reference = FileRef::new(id.clone(), name, bytes).unwrap();
        assert_eq!(reference.name(), name);
        assert_eq!(reference.bytes(), bytes);
        assert_eq!(
            serde_json::from_value::<FileRef>(serde_json::to_value(&reference).unwrap()).unwrap(),
            reference
        );
    }
}

#[test]
fn external_file_metadata_cannot_supply_paths_or_unsafe_names() {
    for name in [
        "", ".", "..", "../x", "a/b", "a\\b", "C:x", "x\0", "x.", "x ", "CON", "CON .txt",
        "lpt1.pdf", "COM²", "a|b",
    ] {
        assert!(
            serde_json::from_value::<FileRef>(json!({"id":"a".repeat(64),"name":name,"bytes":0}))
                .is_err(),
            "{name:?}"
        );
    }
    for value in [
        json!({"id":"a".repeat(64),"name":"x".repeat(256),"bytes":0}),
        json!({"id":"a".repeat(64),"name":"file","bytes":9_007_199_254_740_992_u64}),
        json!({"id":"A".repeat(64),"name":"file","bytes":0}),
        json!({"id":"a".repeat(64),"name":"file","bytes":0,"path":"/etc/passwd"}),
    ] {
        assert!(serde_json::from_value::<FileRef>(value).is_err());
    }
}
