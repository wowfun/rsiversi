use rsi_meta_plugin::{
    DurableCommand, EVENT_CANCEL, EVENT_CREDIT, EVENT_DATA, EVENT_END, FRAME_PROTOCOL,
    FRAME_VERSION, Frame, FrameBody, FrameError, LifecyclePhase, MAX_FRAME_ID_CHARACTERS,
    OP_CANCEL, OP_CREDIT, OP_DATA, OP_HALF_CLOSE, OP_OPEN, STATE_EVENT_APPLIED,
    STATE_EVENT_CONFLICT, STATE_EVENT_DELETED, STATE_EVENT_VALUE, STATE_OP_COMPARE_AND_SWAP,
    STATE_OP_DELETE, STATE_OP_GET,
};
use serde_json::json;

#[test]
fn lifecycle_frame_has_one_canonical_versioned_json_shape() {
    let frame = Frame::lifecycle(
        LifecyclePhase::Prepare,
        42,
        Some(json!({"path": "/workspace"})),
    );

    assert_eq!(
        String::from_utf8(frame.encode().unwrap()).unwrap(),
        r#"{"protocol":"rsi-meta.plugin","version":0,"kind":"lifecycle","phase":"prepare","generation":42,"config":{"path":"/workspace"}}"#
    );
    assert_eq!(Frame::decode(&frame.encode().unwrap()).unwrap(), frame);
}

#[test]
fn lifecycle_prepare_acknowledgements_have_canonical_shapes() {
    let prepared = Frame::lifecycle(LifecyclePhase::Prepared, 42, None);
    assert_eq!(
        String::from_utf8(prepared.encode().unwrap()).unwrap(),
        r#"{"protocol":"rsi-meta.plugin","version":0,"kind":"lifecycle","phase":"prepared","generation":42}"#
    );

    let failed = Frame::lifecycle(
        LifecyclePhase::PrepareFailed,
        42,
        Some(json!({
            "code": "state_read_failed",
            "message": "state.cas returned conflict",
        })),
    );
    assert_eq!(
        String::from_utf8(failed.encode().unwrap()).unwrap(),
        r#"{"protocol":"rsi-meta.plugin","version":0,"kind":"lifecycle","phase":"prepare_failed","generation":42,"config":{"code":"state_read_failed","message":"state.cas returned conflict"}}"#
    );
}

#[test]
fn service_and_state_vocabulary_is_canonical() {
    assert_eq!(
        [OP_OPEN, OP_DATA, OP_CREDIT, OP_HALF_CLOSE, OP_CANCEL],
        ["open", "data", "credit", "half_close", "cancel"]
    );
    assert_eq!(
        [EVENT_DATA, EVENT_CREDIT, EVENT_END, EVENT_CANCEL],
        ["data", "credit", "end", "cancel"]
    );
    assert_eq!(
        [STATE_OP_GET, STATE_OP_COMPARE_AND_SWAP, STATE_OP_DELETE],
        ["get", "compare_and_swap", "delete"]
    );
    assert_eq!(
        [
            STATE_EVENT_VALUE,
            STATE_EVENT_APPLIED,
            STATE_EVENT_CONFLICT,
            STATE_EVENT_DELETED,
        ],
        ["value", "applied", "conflict", "deleted"]
    );
}

#[test]
fn contract_covers_service_and_durable_command_frames() {
    let request = Frame::service_request(
        "request-7",
        "fs.watch",
        "watch",
        json!({"path": "/workspace/rsi-meta.toml"}),
    );
    assert!(matches!(
        request.body,
        FrameBody::ServiceRequest { ref request_id, ref service, .. }
            if request_id == "request-7" && service == "fs.watch"
    ));

    let command = Frame::durable_command(
        "hmr-7",
        DurableCommand::ApplyManifestPath {
            manifest_path: "/workspace/rsi-meta.toml".into(),
            lock_path: "/workspace/rsi-meta.lock".into(),
        },
    );
    assert_eq!(Frame::decode(&command.encode().unwrap()).unwrap(), command);
}

#[test]
fn data_frames_are_binary_and_charge_raw_payload_bytes() {
    let payload: Vec<u8> = (0..=u8::MAX).collect();
    let request = Frame::service_data_request("stream-7", "fixture.echo", payload.clone());
    let encoded = request.encode().unwrap();

    assert_eq!(
        encoded.len(),
        13 + "stream-7".len() + "fixture.echo".len() + payload.len()
    );
    assert_ne!(encoded.first(), Some(&b'{'));
    assert_eq!(Frame::decode(&encoded).unwrap(), request);

    let event = Frame::service_data_event("stream-7", "fixture.echo", payload);
    assert_eq!(Frame::decode(&event.encode().unwrap()).unwrap(), event);
    assert!(Frame::decode(&encoded[..encoded.len() - 1]).is_err());

    let json_request =
        Frame::service_request("stream-7", "fixture.echo", OP_DATA, json!([1, 2, 3]));
    assert!(json_request.encode().is_err());
    assert!(
        Frame::decode(
            br#"{"protocol":"rsi-meta.plugin","version":0,"kind":"service_event","request_id":"stream-7","service":"fixture.echo","event":"data","payload":[1,2,3]}"#
        )
        .is_err()
    );
}

#[test]
fn binary_data_decoder_rejects_adversarial_headers_and_identifiers() {
    let encoded = Frame::service_data_request("id", "svc", [7_u8])
        .encode()
        .unwrap();
    let mut invalid = Vec::new();

    let mut unknown_kind = encoded.clone();
    unknown_kind[4] = u8::MAX;
    invalid.push(unknown_kind);

    let mut non_utf8_request = encoded.clone();
    non_utf8_request[13] = u8::MAX;
    invalid.push(non_utf8_request);

    let mut non_utf8_service = encoded.clone();
    non_utf8_service[15] = u8::MAX;
    invalid.push(non_utf8_service);

    let mut inconsistent_payload_length = encoded.clone();
    inconsistent_payload_length[9..13].copy_from_slice(&2_u32.to_be_bytes());
    invalid.push(inconsistent_payload_length);

    let mut trailing_byte = encoded;
    trailing_byte.push(0);
    invalid.push(trailing_byte);

    let mut empty_request = b"RMD0".to_vec();
    empty_request.push(0);
    empty_request.extend_from_slice(&0_u16.to_be_bytes());
    empty_request.extend_from_slice(&3_u16.to_be_bytes());
    empty_request.extend_from_slice(&0_u32.to_be_bytes());
    empty_request.extend_from_slice(b"svc");
    invalid.push(empty_request);

    let mut empty_service = b"RMD0".to_vec();
    empty_service.push(1);
    empty_service.extend_from_slice(&2_u16.to_be_bytes());
    empty_service.extend_from_slice(&0_u16.to_be_bytes());
    empty_service.extend_from_slice(&0_u32.to_be_bytes());
    empty_service.extend_from_slice(b"id");
    invalid.push(empty_service);

    for bytes in invalid {
        assert!(Frame::decode(&bytes).is_err(), "accepted {bytes:?}");
    }
}

#[test]
fn decode_rejects_unknown_protocol_version_and_unbounded_fields() {
    let wrong_protocol =
        br#"{"protocol":"other","version":0,"kind":"lifecycle","phase":"abort","generation":1}"#;
    assert!(matches!(
        Frame::decode(wrong_protocol),
        Err(FrameError::UnsupportedProtocol { .. })
    ));
    let wrong_version = br#"{"protocol":"rsi-meta.plugin","version":9,"kind":"lifecycle","phase":"abort","generation":1}"#;
    assert!(matches!(
        Frame::decode(wrong_version),
        Err(FrameError::UnsupportedVersion { .. })
    ));

    for field in ["request_id", "service", "operation"] {
        let mut request = json!({
            "protocol": FRAME_PROTOCOL,
            "version": FRAME_VERSION,
            "kind": "service_request",
            "request_id": "request",
            "service": "fixture.echo",
            "operation": "open",
            "payload": {}
        });
        request[field] = json!("x".repeat(MAX_FRAME_ID_CHARACTERS + 1));
        assert!(Frame::decode(request.to_string().as_bytes()).is_err());
    }
    let oversized_command = json!({
        "protocol": FRAME_PROTOCOL,
        "version": FRAME_VERSION,
        "kind": "durable_command",
        "command_id": "x".repeat(MAX_FRAME_ID_CHARACTERS + 1),
        "command": {
            "type": "apply_manifest_path",
            "manifest_path": "a",
            "lock_path": "b"
        }
    });
    assert!(Frame::decode(oversized_command.to_string().as_bytes()).is_err());
}

#[test]
fn decode_enforces_kind_specific_constraints() {
    for invalid in [
        br#"{"protocol":"rsi-meta.plugin","version":0,"kind":"lifecycle","phase":"prepared","generation":1,"service":"fixture.echo"}"#.as_slice(),
        br#"{"protocol":"rsi-meta.plugin","version":0,"kind":"lifecycle","phase":"prepared","generation":0}"#.as_slice(),
        br#"{"protocol":"rsi-meta.plugin","version":0,"kind":"lifecycle","phase":"prepared","generation":1,"config":{}}"#.as_slice(),
        br#"{"protocol":"rsi-meta.plugin","version":0,"kind":"lifecycle","phase":"prepare_failed","generation":1,"config":{}}"#.as_slice(),
        br#"{"protocol":"rsi-meta.plugin","version":0,"kind":"service_request","request_id":"","service":"fixture.echo","operation":"open","payload":{}}"#.as_slice(),
        br#"{"protocol":"rsi-meta.plugin","version":0,"kind":"service_event","service":"","event":"data","payload":[]}"#.as_slice(),
        br#"{"protocol":"rsi-meta.plugin","version":0,"kind":"durable_command","command_id":"","command":{"type":"apply_manifest_path","manifest_path":"a","lock_path":"b"}}"#.as_slice(),
        br#"{"protocol":"rsi-meta.plugin","version":0,"kind":"durable_command","command_id":"apply","command":{"type":"apply_manifest_path","manifest_path":"a","lock_path":"b"},"surprise":true}"#.as_slice(),
    ] {
        assert!(
            Frame::decode(invalid).is_err(),
            "accepted invalid frame: {}",
            String::from_utf8_lossy(invalid)
        );
    }
}
