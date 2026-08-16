use rsi_meta_frame_contract::{
    DurableCommand, EVENT_CANCEL, EVENT_CREDIT, EVENT_DATA, EVENT_END, Frame, FrameBody,
    FrameError, LifecyclePhase, OP_CANCEL, OP_CREDIT, OP_DATA, OP_HALF_CLOSE, OP_OPEN, PROTOCOL,
    STATE_EVENT_APPLIED, STATE_EVENT_CONFLICT, STATE_EVENT_DELETED, STATE_EVENT_VALUE,
    STATE_OP_COMPARE_AND_SWAP, STATE_OP_DELETE, STATE_OP_GET, VERSION,
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
fn service_stream_vocabulary_is_frozen_for_both_directions() {
    assert_eq!(
        [OP_OPEN, OP_DATA, OP_CREDIT, OP_HALF_CLOSE, OP_CANCEL],
        ["open", "data", "credit", "half_close", "cancel"]
    );
    assert_eq!(
        [EVENT_DATA, EVENT_CREDIT, EVENT_END, EVENT_CANCEL],
        ["data", "credit", "end", "cancel"]
    );
}

#[test]
fn host_state_service_vocabulary_is_frozen() {
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
fn decode_rejects_unknown_protocol_or_version_before_dispatch() {
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
    assert_eq!(PROTOCOL, "rsi-meta.plugin");
    assert_eq!(VERSION, 0);
}

#[test]
fn decode_enforces_the_published_kind_and_field_constraints() {
    for invalid in [
        br#"{"protocol":"rsi-meta.plugin","version":0,"kind":"lifecycle","phase":"prepared","generation":1,"service":"fixture.echo"}"#.as_slice(),
        br#"{"protocol":"rsi-meta.plugin","version":0,"kind":"lifecycle","phase":"prepared","generation":0}"#.as_slice(),
        br#"{"protocol":"rsi-meta.plugin","version":0,"kind":"lifecycle","phase":"prepared","generation":1,"config":{}}"#.as_slice(),
        br#"{"protocol":"rsi-meta.plugin","version":0,"kind":"lifecycle","phase":"prepare_failed","generation":1,"config":{}}"#.as_slice(),
        br#"{"protocol":"rsi-meta.plugin","version":0,"kind":"service_request","request_id":"","service":"fixture.echo","operation":"open","payload":{}}"#.as_slice(),
        br#"{"protocol":"rsi-meta.plugin","version":0,"kind":"service_event","service":"","event":"data","payload":[]}"#.as_slice(),
        br#"{"protocol":"rsi-meta.plugin","version":0,"kind":"durable_command","command_id":"","command":{"type":"apply_manifest_path","manifest_path":"a","lock_path":"b"}}"#.as_slice(),
    ] {
        assert!(Frame::decode(invalid).is_err(), "accepted invalid frame: {}", String::from_utf8_lossy(invalid));
    }
}
