use rsi_workspace_protocol::WorkspaceId;

#[test]
fn workspace_identity_includes_the_machine_and_accepts_foreign_local_paths() {
    use rsi_workspace_protocol::{ExecutionCoordinates, ExecutionLocation};
    let local = ExecutionCoordinates::new(ExecutionLocation::Local, "/project").unwrap();
    let remote = |hex: &str| {
        ExecutionCoordinates::new(
            ExecutionLocation::Ssh {
                target: serde_json::from_value(serde_json::json!(hex.repeat(32))).unwrap(),
            },
            "/project",
        )
        .unwrap()
    };
    let ids = [local, remote("a"), remote("b")]
        .map(|coordinates| WorkspaceId::from_coordinates(&coordinates));
    assert_eq!(
        ids.iter().collect::<std::collections::BTreeSet<_>>().len(),
        3
    );
    let windows = ExecutionCoordinates::new(ExecutionLocation::Local, r"C:\project").unwrap();
    assert_eq!(
        WorkspaceId::from_coordinates(&windows),
        WorkspaceId::from_canonical_path(std::path::Path::new(windows.path())).unwrap()
    );
}

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
    let path = std::path::PathBuf::from(r"C:\server\project");
    let mut record = WorkspaceRecord {
        id: WorkspaceId::from_canonical_path(&path).unwrap(),
        coordinates: rsi_workspace_protocol::ExecutionCoordinates::new(
            rsi_workspace_protocol::ExecutionLocation::Local,
            path.to_str().unwrap(),
        )
        .unwrap(),
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
    for path in ["relative", "/a/../b", "/a//b"] {
        assert!(WorkspaceId::from_canonical_path(std::path::Path::new(path)).is_err());
    }
}

#[test]
fn order_seed_bounds_complete_membership_and_rejects_partial_or_misbound_wire_values() {
    use rsi_workspace_protocol::{
        ExecutionCoordinates, ExecutionLocation, WorkspaceOrderSeed, WorkspaceRecord,
    };
    let mut records = (0..1025)
        .map(|index| {
            WorkspaceRecord::new(
                ExecutionCoordinates::new(ExecutionLocation::Local, format!("/w{index}")).unwrap(),
            )
        })
        .collect::<Vec<_>>();
    records.sort_by(|a, b| a.id.cmp(&b.id));
    let seed = WorkspaceOrderSeed::from_records(&records[..65]);
    seed.validate().unwrap();
    let WorkspaceOrderSeed::Available { records: complete } = seed else {
        panic!("65 fit")
    };
    assert_eq!(complete, records[..65]);
    assert!(matches!(
        WorkspaceOrderSeed::from_records(&records),
        WorkspaceOrderSeed::TooLarge {}
    ));
    let long = WorkspaceRecord::new(
        ExecutionCoordinates::new(ExecutionLocation::Local, format!("/{}", "x".repeat(16000)))
            .unwrap(),
    );
    assert!(matches!(
        WorkspaceOrderSeed::from_records(std::iter::repeat_n(&long, 9)),
        WorkspaceOrderSeed::TooLarge {}
    ));
    let mut duplicate = complete.clone();
    duplicate.insert(1, complete[0].clone());
    assert!(
        WorkspaceOrderSeed::Available { records: duplicate }
            .validate()
            .is_err()
    );
    let mut misbound = complete;
    misbound[0].coordinates = long.coordinates;
    assert!(
        WorkspaceOrderSeed::Available { records: misbound }
            .validate()
            .is_err()
    );
    assert!(
        serde_json::from_value::<WorkspaceOrderSeed>(
            serde_json::json!({"kind":"too_large","records":[]})
        )
        .is_err()
    );
}

#[test]
fn order_seed_builder_and_validator_agree_with_encoded_envelope_at_the_byte_limit() {
    use rsi_workspace_protocol::{
        ExecutionCoordinates, ExecutionLocation, WorkspaceOrderSeed, WorkspaceRecord,
    };
    // Escaped path bytes exercise actual JSON size, not character or path length.
    let mut records = (0..24)
        .map(|index| {
            WorkspaceRecord::new(
                ExecutionCoordinates::new(
                    ExecutionLocation::Local,
                    format!("/w{index}{}", "\"界".repeat(1500)),
                )
                .unwrap(),
            )
        })
        .collect::<Vec<_>>();
    records.sort_by(|a, b| a.id.cmp(&b.id));
    let mut saw_oversized = false;
    for count in 0..=records.len() {
        let raw = WorkspaceOrderSeed::Available {
            records: records[..count].to_vec(),
        };
        let fits = serde_json::to_vec(&raw).unwrap().len() <= 128 * 1024;
        assert_eq!(raw.validate().is_ok(), fits);
        assert_eq!(
            matches!(
                WorkspaceOrderSeed::from_records(&records[..count]),
                WorkspaceOrderSeed::Available { .. }
            ),
            fits
        );
        saw_oversized |= !fits;
    }
    assert!(saw_oversized);
}
