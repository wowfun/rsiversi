use jsonschema::Validator;
use rsi_ai_protocol::{
    ImageRequest, LanguageRequest, LanguageSettings, MAX_AUDIO_BYTES, MAX_BLOCKS_PER_MESSAGE,
    MAX_DESCRIPTION_BYTES, MAX_ID_BYTES, MAX_IMAGE_BYTES, MAX_IMAGE_DIMENSION, MAX_IMAGE_OUTPUTS,
    MAX_MESSAGES, MAX_REQUEST_BYTES, MAX_STOP_SEQUENCE_BYTES, MAX_STOP_SEQUENCES, MAX_TOOLS,
    MediaDescriptor, MediaKind, Message, RealtimeRequest, SpeechFormat, SpeechRequest, ToolChoice,
    ToolDefinition, TranscriptionRequest,
};
use serde::Serialize;
use serde_json::{Value, json};

const LANGUAGE: &str = include_str!("../../../../schemas/rsi-ai/language-request.schema.json");
const IMAGE: &str = include_str!("../../../../schemas/rsi-ai/image-request.schema.json");
const TRANSCRIPTION: &str =
    include_str!("../../../../schemas/rsi-ai/transcription-request.schema.json");
const SPEECH: &str = include_str!("../../../../schemas/rsi-ai/speech-request.schema.json");
const REALTIME: &str = include_str!("../../../../schemas/rsi-ai/realtime-request.schema.json");

fn image() -> MediaDescriptor {
    MediaDescriptor::new(MediaKind::Image, "image/png", 4, "1".repeat(64)).expect("image")
}

fn audio() -> MediaDescriptor {
    MediaDescriptor::new(MediaKind::Audio, "audio/wav", 4, "2".repeat(64)).expect("audio")
}

fn assert_contract<T: Serialize>(schema: &str, value: &T) {
    let schema: Value = serde_json::from_str(schema).expect("schema JSON");
    let validator = Validator::new(&schema).expect("compile schema");
    let mut value = serde_json::to_value(value).expect("serialize DTO");
    assert!(validator.is_valid(&value), "schema rejected {value}");
    value
        .as_object_mut()
        .expect("request object")
        .insert("unknown".to_owned(), Value::Bool(true));
    assert!(!validator.is_valid(&value), "schema accepted unknown field");
}

#[test]
fn every_capability_request_matches_its_closed_schema() {
    let language = LanguageRequest::new(vec![Message::user_text("hello").expect("message")])
        .expect("language")
        .with_settings(
            LanguageSettings::default()
                .with_max_output_tokens(128)
                .expect("settings"),
        )
        .expect("settings");
    assert_contract(LANGUAGE, &language);
    assert_contract(
        IMAGE,
        &ImageRequest::new("a dot", 1)
            .expect("image request")
            .with_inputs(vec![image()], None)
            .expect("image input"),
    );
    assert_contract(
        TRANSCRIPTION,
        &TranscriptionRequest::new(audio())
            .expect("transcription")
            .with_language("en")
            .expect("language"),
    );
    assert_contract(
        SPEECH,
        &SpeechRequest::new("hello", "alloy", SpeechFormat::Wav).expect("speech"),
    );
    assert_contract(
        REALTIME,
        &RealtimeRequest::new("alloy")
            .expect("Realtime")
            .with_instructions("Be concise")
            .expect("instructions"),
    );
}

#[test]
fn language_schema_accepts_the_full_rust_tool_name_contract() {
    let name = format!("a.{}", "b".repeat(rsi_ai_protocol::MAX_ID_BYTES - 2));
    let request = LanguageRequest::new(vec![Message::user_text("hello").expect("message")])
        .expect("request")
        .with_tools(
            vec![ToolDefinition::new(&name, "", json!(true)).expect("tool")],
            ToolChoice::Specific(name),
        )
        .expect("tools");

    assert_contract(LANGUAGE, &request);
}

fn schema_integer(schema: &Value, pointer: &str) -> u64 {
    schema
        .pointer(pointer)
        .and_then(Value::as_u64)
        .unwrap_or_else(|| panic!("missing integer schema bound at {pointer}"))
}

#[test]
fn schema_boundaries_equal_the_rust_contract_constants() {
    let language: Value = serde_json::from_str(LANGUAGE).expect("language schema");
    for (pointer, expected) in [
        ("/properties/messages/maxItems", MAX_MESSAGES as u64),
        ("/properties/tools/maxItems", MAX_TOOLS as u64),
        ("/$defs/id/maxLength", MAX_ID_BYTES as u64),
        ("/$defs/tool_name/maxLength", MAX_ID_BYTES as u64),
        ("/$defs/image/properties/byte_len/maximum", MAX_IMAGE_BYTES),
        ("/$defs/audio/properties/byte_len/maximum", MAX_AUDIO_BYTES),
        (
            "/$defs/image/properties/width/maximum",
            u64::from(MAX_IMAGE_DIMENSION),
        ),
        (
            "/$defs/image/properties/height/maximum",
            u64::from(MAX_IMAGE_DIMENSION),
        ),
        (
            "/$defs/message/properties/content/maxItems",
            MAX_BLOCKS_PER_MESSAGE as u64,
        ),
        (
            "/$defs/tool/properties/description/maxLength",
            MAX_DESCRIPTION_BYTES as u64,
        ),
        (
            "/$defs/settings/properties/stop/maxItems",
            MAX_STOP_SEQUENCES as u64,
        ),
        (
            "/$defs/settings/properties/stop/items/maxLength",
            MAX_STOP_SEQUENCE_BYTES as u64,
        ),
    ] {
        assert_eq!(schema_integer(&language, pointer), expected, "{pointer}");
    }

    for (schema, pointers) in [
        (
            IMAGE,
            vec![
                ("/properties/prompt/maxLength", MAX_REQUEST_BYTES as u64),
                ("/properties/count/maximum", u64::from(MAX_IMAGE_OUTPUTS)),
                ("/properties/inputs/maxItems", u64::from(MAX_IMAGE_OUTPUTS)),
                ("/$defs/image/properties/byte_len/maximum", MAX_IMAGE_BYTES),
            ],
        ),
        (
            TRANSCRIPTION,
            vec![
                ("/properties/prompt/maxLength", MAX_REQUEST_BYTES as u64),
                ("/$defs/audio/properties/byte_len/maximum", MAX_AUDIO_BYTES),
            ],
        ),
        (
            SPEECH,
            vec![("/properties/text/maxLength", MAX_REQUEST_BYTES as u64)],
        ),
        (
            REALTIME,
            vec![(
                "/properties/instructions/maxLength",
                MAX_REQUEST_BYTES as u64,
            )],
        ),
    ] {
        let schema: Value = serde_json::from_str(schema).expect("request schema");
        for (pointer, expected) in pointers {
            assert_eq!(schema_integer(&schema, pointer), expected, "{pointer}");
        }
    }
}
