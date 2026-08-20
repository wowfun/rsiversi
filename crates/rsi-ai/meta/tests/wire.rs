use rsi_ai_meta::{
    AiService, ClientControl, MetaWireError, ServerControl, decode_client_control,
    decode_server_control, encode_client_control, encode_server_control,
};
use rsi_ai_meta::{Capability, PreparedCallSnapshot, RetryPolicy};
use rsi_ai_protocol::{LanguageRequest, MediaDescriptor, MediaKind, Message};

fn snapshot() -> PreparedCallSnapshot {
    PreparedCallSnapshot {
        call_id: "call-1".to_owned(),
        deployment_id: "provider".to_owned(),
        provider_family: "test".to_owned(),
        capability: Capability::Language,
        model: "model".to_owned(),
        protocol: "fixture".to_owned(),
        transport: "memory".to_owned(),
        endpoint_fingerprint: "fixture".to_owned(),
        config_generation: 7,
        credential_source: None,
        retry_policy: RetryPolicy::default(),
        request_sha256: "00".repeat(32),
    }
}

#[test]
fn five_service_contracts_are_fixed_and_distinct() {
    assert_eq!(AiService::Language.key(), "rsi.ai.language");
    assert_eq!(AiService::Image.key(), "rsi.ai.image");
    assert_eq!(AiService::Transcription.key(), "rsi.ai.transcription");
    assert_eq!(AiService::Speech.key(), "rsi.ai.speech");
    assert_eq!(AiService::Realtime.key(), "rsi.ai.realtime");
    assert_eq!(AiService::Language.version(), 0);
}

#[test]
fn prepare_and_prepared_are_strict_bounded_control_messages() {
    let request = ClientControl::PrepareLanguage {
        call_id: "call-1".to_owned(),
        model: "model".to_owned(),
        request: LanguageRequest::new(vec![Message::user_text("hello").expect("message")])
            .expect("request"),
    };
    let bytes = encode_client_control(&request).expect("encode");
    assert_eq!(decode_client_control(&bytes).expect("decode"), request);

    let prepared = ServerControl::Prepared {
        call_id: "call-1".to_owned(),
        snapshot: snapshot(),
    };
    let bytes = encode_server_control(&prepared).expect("encode");
    assert_eq!(decode_server_control(&bytes).expect("decode"), prepared);

    let unknown = br#"{"type":"start","call_id":"call-1","unknown":true}"#;
    assert!(matches!(
        decode_client_control(unknown),
        Err(MetaWireError::InvalidJson(_))
    ));
    let oversized = vec![b'x'; rsi_ai_protocol::MAX_CONTROL_FRAME_BYTES + 1];
    assert_eq!(
        decode_client_control(&oversized)
            .expect_err("bounded")
            .code(),
        "meta.control_too_large"
    );
}

#[test]
fn realtime_audio_control_rejects_non_audio_descriptors() {
    let image = MediaDescriptor::new(MediaKind::Image, "image/png", 1, "00".repeat(32))
        .expect("image descriptor");
    let control = ClientControl::RealtimeAppendAudio {
        call_id: "call-1".to_owned(),
        blob_id: "blob-1".to_owned(),
        sequence: 1,
        descriptor: image,
    };
    assert!(matches!(
        encode_client_control(&control),
        Err(MetaWireError::InvalidValue(_))
    ));
}
