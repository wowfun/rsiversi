use rsi_api_protocol::{OperationCatalog, OperationId, describe_operation, operations_operation};

#[test]
fn catalogs_validate_descriptors_duplicates_and_entry_bound_during_decode() {
    let mut entries = vec![describe_operation(), operations_operation()];
    let encoded = serde_json::to_vec(&entries).unwrap();
    assert_eq!(
        serde_json::from_slice::<OperationCatalog>(&encoded)
            .unwrap()
            .operations(),
        entries
    );
    entries.push(describe_operation());
    assert!(
        serde_json::from_slice::<OperationCatalog>(&serde_json::to_vec(&entries).unwrap()).is_err()
    );
    entries.pop();
    entries[0].maximum_response_bytes = 128 * 1024 + 1;
    assert!(
        serde_json::from_slice::<OperationCatalog>(&serde_json::to_vec(&entries).unwrap()).is_err()
    );
    let mut entries = Vec::new();
    for index in 0..2048 {
        let mut spec = describe_operation();
        spec.id = OperationId::new("test", format!("operation-{index}"), 1).unwrap();
        entries.push(spec);
    }
    let encoded = serde_json::to_vec(&entries).unwrap();
    assert_eq!(
        serde_json::from_slice::<OperationCatalog>(&encoded)
            .unwrap()
            .operations()
            .len(),
        2048
    );
    entries.push(operations_operation());
    assert!(
        serde_json::from_slice::<OperationCatalog>(&serde_json::to_vec(&entries).unwrap()).is_err()
    );
}
