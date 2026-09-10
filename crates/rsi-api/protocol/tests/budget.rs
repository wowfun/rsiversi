use rsi_api_protocol::{ApiError, ByteAccumulator, ByteBudget, OperationId};
use serde::Serialize;
use std::sync::atomic::{AtomicUsize, Ordering};

#[test]
fn attached_count_admission_survives_last_transport_slice() {
    let budget = ByteBudget::new(8).unwrap();
    let guard = std::sync::Arc::new(());
    let weak = std::sync::Arc::downgrade(&guard);
    let bytes = budget.copy(b"payload").unwrap().with_retention(guard);
    let wire = bytes.slice(1..4).unwrap().into_bytes();
    drop(bytes);
    assert!(weak.upgrade().is_some());
    assert_eq!(budget.used(), 7);
    assert_eq!(wire.as_ref(), b"ayl");
    drop(wire);
    assert!(weak.upgrade().is_none());
    assert_eq!(budget.used(), 0);
}

#[test]
fn empty_slice_does_not_retain_admission_but_sibling_payload_does() {
    let budget = ByteBudget::new(8).unwrap();
    let guard = std::sync::Arc::new(());
    let weak = std::sync::Arc::downgrade(&guard);
    let bytes = budget.copy(b"payload").unwrap().with_retention(guard);
    let sibling = bytes.clone();
    let empty = bytes.slice(3..3).unwrap();
    drop(bytes);
    assert!(weak.upgrade().is_some());
    assert_eq!(budget.used(), 7);
    drop(sibling);
    assert!(weak.upgrade().is_none());
    assert_eq!(budget.used(), 0);
    assert!(empty.is_empty());
}

#[test]
fn growing_payloads_share_receiving_capacity_and_transfer_last_slice_ownership() {
    let receiving = ByteBudget::new(7).unwrap();
    let retained = ByteBudget::new(7).unwrap();
    let mut first = ByteAccumulator::new(&receiving, 40).unwrap();
    let mut second = ByteAccumulator::new(&receiving, 40).unwrap();
    assert_eq!(receiving.used(), 0);
    first.append(b"ab").unwrap();
    second.append(b"xyz").unwrap();
    assert_eq!(receiving.used(), 6);
    // Doubling the first allocation would not fit, but its exact next byte does.
    first.append(b"c").unwrap();
    assert_eq!(receiving.used(), 7);
    assert_eq!(first.append(b"d").unwrap_err(), ApiError::Capacity);
    assert_eq!(receiving.used(), 7);
    let bytes = first.finish_into(&retained).unwrap();
    assert_eq!(
        bytes.as_bytes(),
        b"abc",
        "rejected append never copied input"
    );
    let slice = bytes.slice(1..2).unwrap();
    drop(bytes);
    assert_eq!(retained.used(), 3);
    second.append(b"w").unwrap();
    let bytes = second.finish_into(&retained).unwrap();
    assert_eq!(bytes.as_bytes(), b"xyzw");
    assert_eq!(receiving.used(), 0);
    assert_eq!(retained.used(), 7);
    drop(slice);
    drop(bytes);
    assert_eq!(retained.used(), 0);
}

#[test]
fn growing_payload_bounds_and_failed_transfer_release_only_their_owners() {
    let receiving = ByteBudget::new(8).unwrap();
    let retained = ByteBudget::new(4).unwrap();
    let held = retained.copy(b"old").unwrap();
    let mut incoming = ByteAccumulator::new(&receiving, 3).unwrap();
    incoming.append(b"ab").unwrap();
    assert!(matches!(incoming.append(b"cd"), Err(ApiError::Invalid(_))));
    assert_eq!(receiving.used(), 2);
    assert_eq!(
        incoming.finish_into(&retained).unwrap_err(),
        ApiError::Capacity
    );
    assert_eq!(receiving.used(), 0);
    assert_eq!(retained.used(), 3);
    assert_eq!(held.as_bytes(), b"old");
    drop(held);
    let mut incoming = ByteAccumulator::new(&receiving, 8).unwrap();
    incoming.append(b"partial").unwrap();
    drop(incoming);
    assert_eq!(receiving.used(), 0);
    assert_eq!(retained.used(), 0);
}

#[test]
fn native_allocations_transfer_without_copy_and_keep_unused_capacity() {
    let budget = ByteBudget::new(16).unwrap();
    let reservation = budget.reserve(16).unwrap();
    let mut source = Vec::with_capacity(16);
    source.extend_from_slice(b"private");
    let pointer = source.as_ptr();
    let retained = reservation.retain_vec(source).unwrap();
    assert_eq!(retained.as_bytes().as_ptr(), pointer);
    let slice = retained.into_bytes().slice(1..2);
    assert_eq!(budget.used(), 16);
    assert!(budget.reserve(1).is_err());
    drop(slice);
    assert_eq!(budget.used(), 0);
    let reservation = budget.reserve(1).unwrap();
    assert!(reservation.retain_vec(Vec::with_capacity(2)).is_err());
    assert_eq!(budget.used(), 0);
}

#[test]
fn slices_and_clones_retain_the_original_allocation_until_its_last_owner_drops() {
    let budget = ByteBudget::new(8).unwrap();
    let bytes = budget.copy(b"private!").unwrap();
    let slice = bytes.slice(2..4).unwrap();
    let clone = slice.clone();
    assert_eq!(slice.as_bytes(), b"iv");
    assert!(!format!("{bytes:?}").contains("private"));
    drop(bytes);
    drop(slice);
    assert_eq!(budget.used(), 8);
    assert!(matches!(budget.copy(b"x"), Err(ApiError::Capacity)));
    drop(clone);
    assert_eq!(budget.used(), 0);
    assert_eq!(budget.copy(b"next").unwrap().as_bytes(), b"next");
    assert_eq!(budget.used(), 0);
}

#[test]
fn reservation_failure_and_invalid_ranges_release_exactly_their_own_bytes() {
    let budget = ByteBudget::new(8).unwrap();
    let mut reserved = budget.reserve(8).unwrap();
    assert!(budget.reserve(1).is_err());
    assert!(reserved.shrink(9).is_err());
    assert_eq!(budget.used(), 8);
    reserved.shrink(3).unwrap();
    assert_eq!(budget.used(), 3);
    assert!(reserved.copy(b"oversize").is_err());
    assert_eq!(budget.used(), 0);
    let bytes = budget.copy(b"abc").unwrap();
    for (start, end) in [(4, 5), (2, 1), (0, usize::MAX)] {
        let range = start..end;
        assert!(bytes.slice(range).is_err());
    }
    assert_eq!(budget.used(), 3);
    drop(bytes);
    assert_eq!(budget.used(), 0);
}

struct ChangingValue(AtomicUsize);
impl Serialize for ChangingValue {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(if self.0.fetch_add(1, Ordering::SeqCst) == 0 {
            "a"
        } else {
            "grows between count and encoding"
        })
    }
}

#[test]
fn encoding_rechecks_the_reserved_limit_and_preserves_exact_json_numbers() {
    let budget = ByteBudget::new(128).unwrap();
    assert!(
        budget
            .encode(&ChangingValue(AtomicUsize::new(0)), 128)
            .is_err()
    );
    assert_eq!(budget.used(), 0);
    let value: serde_json::Value = serde_json::from_str(
        r#"{"large":18446744073709551615,"exact":123456789012345678901234567890}"#,
    )
    .unwrap();
    let bytes = budget.encode(&value, 128).unwrap();
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(bytes.as_bytes()).unwrap(),
        value
    );
    assert_eq!(budget.used(), bytes.len());
    assert!(budget.encode(&"too large", 2).is_err());
    assert_eq!(budget.used(), bytes.len());
    drop(bytes);
    assert_eq!(budget.used(), 0);
}

#[test]
fn capacity_rejection_does_not_begin_a_second_serialization_pass() {
    let budget = ByteBudget::new(2).unwrap();
    let value = ChangingValue(AtomicUsize::new(0));
    assert_eq!(budget.encode(&value, 128).unwrap_err(), ApiError::Capacity);
    assert_eq!(value.0.load(Ordering::SeqCst), 1);
    assert_eq!(budget.used(), 0);
}

#[test]
fn operation_identity_is_closed_and_has_no_transport_or_build_identity() {
    let valid = OperationId::new("workspace", "list", 1).unwrap();
    assert_eq!(
        serde_json::from_value::<OperationId>(serde_json::to_value(&valid).unwrap()).unwrap(),
        valid
    );
    for value in [
        serde_json::json!({"domain":"session","name":"create","version":0}),
        serde_json::json!({"domain":"../session","name":"create","version":1}),
        serde_json::json!({"domain":"session","name":"CREATE","version":1}),
        serde_json::json!({"domain":"session","name":"create","version":1,"build":"foreign"}),
    ] {
        assert!(serde_json::from_value::<OperationId>(value).is_err());
    }
}

#[test]
fn receiving_and_splitting_keep_capacity_owned_through_transport_clones() {
    let budget = ByteBudget::new(12).unwrap();
    let mut all = budget.reserve(12).unwrap();
    let metadata = all.split(4).unwrap().encode(&true).unwrap();
    let mut receiver = all.receive();
    receiver.append(b"ab").unwrap();
    assert!(receiver.append(b"too large").is_err());
    receiver.append(b"cd").unwrap();
    assert_eq!(budget.used(), 12);
    let binary = receiver.finish().into_bytes();
    assert_eq!(binary.as_ref(), b"abcd");
    let slice = binary.slice(1..2);
    drop(metadata);
    drop(binary);
    assert_eq!(
        budget.used(),
        8,
        "short body retains its admitted allocation"
    );
    drop(slice);
    assert_eq!(budget.used(), 0);
}

#[test]
fn completed_receiver_transfers_compacted_capacity_until_last_transport_slice_drops() {
    let receiving = ByteBudget::new(1024).unwrap();
    let retained = ByteBudget::new(4).unwrap();
    let mut receiver = receiving.reserve(1024).unwrap().receive();
    receiver.append(b"data").unwrap();
    assert_eq!(receiving.used(), 1024);
    let bytes = receiver.finish_into(&retained).unwrap().into_bytes();
    assert_eq!(receiving.used(), 0);
    assert_eq!(retained.used(), 4);
    let next_receive = receiving.reserve(1024).unwrap();
    let slice = bytes.slice(1..2);
    drop(bytes);
    assert_eq!(slice.as_ref(), b"a");
    assert_eq!(retained.used(), 4);
    drop(slice);
    drop(next_receive);
    assert_eq!(retained.used(), 0);
    assert_eq!(receiving.used(), 0);
}

#[test]
fn failed_transfer_releases_source_but_preserves_existing_destination_owners() {
    let receiving = ByteBudget::new(8).unwrap();
    let retained = ByteBudget::new(4).unwrap();
    let previous = retained.copy(b"old").unwrap();
    let mut receiver = receiving.reserve(8).unwrap().receive();
    receiver.append(b"new").unwrap();
    assert_eq!(
        receiver.finish_into(&retained).unwrap_err(),
        ApiError::Capacity
    );
    assert_eq!(receiving.used(), 0);
    assert_eq!(retained.used(), 3);
    assert_eq!(previous.as_bytes(), b"old");
    drop(previous);
    assert_eq!(retained.used(), 0);

    let mut receiver = receiving.reserve(8).unwrap().receive();
    receiver.append(b"x").unwrap();
    // A shared destination cannot reuse the source lease before acquiring its own.
    assert_eq!(
        receiver.finish_into(&receiving).unwrap_err(),
        ApiError::Capacity
    );
    assert_eq!(receiving.used(), 0);
}

#[test]
fn pre_reserved_encoding_releases_unused_upper_bound_before_allocation() {
    let budget = ByteBudget::new(128).unwrap();
    let reserved = budget.reserve(128).unwrap();
    let encoded = reserved.encode(&true).unwrap();
    assert_eq!(encoded.as_bytes(), b"true");
    assert_eq!(budget.used(), 4);
    drop(encoded);
    assert_eq!(budget.used(), 0);
}

#[test]
fn deployment_generation_and_device_identities_reject_noncanonical_wire_values() {
    use rsi_api_protocol::{DeviceId, EndpointId, HostEpoch, LocalCompatibilityKey};
    let endpoint = EndpointId::from_bytes([0xab; 16]);
    let epoch = HostEpoch::from_bytes([0xcd; 16]);
    let device = DeviceId::from_bytes([0xef; 16]);
    assert_eq!(endpoint.as_str(), "ab".repeat(16));
    assert_eq!(epoch.as_str(), "cd".repeat(16));
    assert_eq!(device.as_str(), "ef".repeat(16));
    let local = LocalCompatibilityKey::from_bytes([0xab; 32]);
    assert_eq!(local.as_str(), "ab".repeat(32));
    assert!(serde_json::from_str::<EndpointId>(&serde_json::to_string(&local).unwrap()).is_err());
    for value in [
        "a".repeat(32),
        "A".repeat(64),
        "g".repeat(64),
        "0".repeat(65),
    ] {
        assert!(
            serde_json::from_value::<LocalCompatibilityKey>(serde_json::Value::String(value))
                .is_err()
        );
    }
    for value in ["", "a", &"A".repeat(32), &"g".repeat(32), &"0".repeat(33)] {
        let json = serde_json::to_string(value).unwrap();
        assert!(serde_json::from_str::<EndpointId>(&json).is_err());
        assert!(serde_json::from_str::<HostEpoch>(&json).is_err());
        assert!(serde_json::from_str::<DeviceId>(&json).is_err());
    }
}
