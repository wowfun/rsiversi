use rsi_workspace_protocol::WorkspaceId;

#[test]
fn deserialization_rejects_noncanonical_workspace_identities() {
    for value in [
        String::new(),
        "x".repeat(64),
        "A".repeat(64),
        "a".repeat(63),
        "a".repeat(65),
    ] {
        assert!(serde_json::from_value::<WorkspaceId>(serde_json::json!(value)).is_err());
    }
    let value = "0123456789abcdef".repeat(4);
    let identity: WorkspaceId = serde_json::from_value(serde_json::json!(value)).unwrap();
    assert_eq!(identity.as_str(), value);
    assert_eq!(
        serde_json::to_value(identity).unwrap(),
        serde_json::json!(value)
    );
}

#[test]
fn external_records_bound_paths_and_keep_host_paths_opaque() {
    use rsi_workspace_protocol::{
        MAXIMUM_WORKSPACE_PATH_BYTES, WorkspaceRecord, validate_workspace_path,
    };
    use sha2::Digest as _;
    let path = std::path::PathBuf::from(r"C:\server\project");
    let mut record = WorkspaceRecord {
        id: WorkspaceId::parse(hex::encode(sha2::Sha256::digest(
            path.to_str().unwrap().as_bytes(),
        )))
        .unwrap(),
        path,
    };
    record.validate().unwrap();
    record.id = WorkspaceId::parse("a".repeat(64)).unwrap();
    assert!(record.validate().is_err());
    for path in [
        String::new(),
        "a\0b".into(),
        "a".repeat(MAXIMUM_WORKSPACE_PATH_BYTES + 1),
    ] {
        assert!(validate_workspace_path(std::path::Path::new(&path)).is_err());
    }
}
