use jsonschema::Validator;
use rsi_ai_protocol::{
    FreeformToolDefinition, HostedTool, ImageRequest, LanguageRequest, LanguageSettings,
    MAX_BLOCKS_PER_MESSAGE, MAX_DESCRIPTION_BYTES, MAX_FREEFORM_GRAMMAR_BYTES, MAX_ID_BYTES,
    MAX_IMAGE_BYTES, MAX_IMAGE_DIMENSION, MAX_IMAGE_OUTPUTS, MAX_MESSAGES, MAX_REQUEST_BYTES,
    MAX_STOP_SEQUENCE_BYTES, MAX_STOP_SEQUENCES, MAX_TOOLS, MediaDescriptor, MediaKind, Message,
    MessageContent, ResponseFormat, ToolCall, ToolCallKind, ToolChoice, ToolDefinition,
};
use serde::Serialize;
use serde_json::{Value, json};

const LANGUAGE: &str = include_str!("../../../../schemas/rsi-ai/language-request.schema.json");
const IMAGE: &str = include_str!("../../../../schemas/rsi-ai/image-request.schema.json");

fn image() -> MediaDescriptor {
    MediaDescriptor::new(MediaKind::Image, "image/png", 4, "1".repeat(64)).expect("image")
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
fn language_and_image_requests_match_their_closed_schemas() {
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
}

#[test]
fn request_schemas_reject_mime_case_that_the_typed_boundary_rejects() {
    for schema in [LANGUAGE, IMAGE] {
        let schema: Value = serde_json::from_str(schema).expect("schema JSON");
        let validator = Validator::new(&schema).expect("compile schema");
        let mut request = if schema["$id"].as_str().unwrap().contains("language") {
            serde_json::to_value(
                LanguageRequest::new(vec![Message::user_text("hello").unwrap()]).unwrap(),
            )
            .unwrap()
        } else {
            serde_json::to_value(ImageRequest::new("hello", 1).unwrap()).unwrap()
        };
        let descriptor = json!({
            "kind":"image",
            "mime_type":"image/PNG",
            "byte_len":1,
            "sha256":"a".repeat(64),
            "width":null,
            "height":null,
            "duration_ms":null
        });
        if request.get("messages").is_some() {
            request["messages"][0]["content"] = json!([{"type":"image","content":descriptor}]);
        } else {
            request["inputs"] = json!([descriptor]);
        }
        assert!(
            !validator.is_valid(&request),
            "schema accepted uppercase MIME: {request}"
        );
    }
}

#[test]
fn image_request_revalidates_during_deserialization() {
    let error = serde_json::from_value::<ImageRequest>(json!({
        "prompt": "draw",
        "count": 0,
        "inputs": [],
        "mask": null
    }))
    .expect_err("zero image outputs must not enter a typed request");
    assert!(error.to_string().contains("image count"), "{error}");
}

#[test]
fn freeform_tool_definition_revalidates_during_deserialization() {
    let error = serde_json::from_value::<FreeformToolDefinition>(json!({
        "format": "lark",
        "grammar": "x".repeat(MAX_FREEFORM_GRAMMAR_BYTES + 1)
    }))
    .expect_err("an oversized grammar must not enter the typed protocol");
    assert!(error.to_string().contains("freeform grammar"), "{error}");
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

#[test]
fn language_schema_matches_optional_rust_request_fields() {
    let request = LanguageRequest::new(vec![Message::user_text("hello").expect("message")])
        .expect("request")
        .with_hosted_tools(vec![HostedTool::WebSearch { max_uses: None }])
        .expect("hosted tool")
        .with_response_format(
            ResponseFormat::json_schema("answer", None, json!(true)).expect("format"),
        )
        .expect("response format");
    let mut value = serde_json::to_value(request).expect("request JSON");
    value["hosted_tools"][0]
        .as_object_mut()
        .expect("hosted tool object")
        .remove("max_uses");
    value["response_format"]
        .as_object_mut()
        .expect("response format object")
        .remove("description");
    value["settings"] = json!({});

    serde_json::from_value::<LanguageRequest>(value.clone())
        .expect("the authoritative Rust decoder accepts omitted optional settings");
    let schema: Value = serde_json::from_str(LANGUAGE).expect("language schema");
    let validator = Validator::new(&schema).expect("compile schema");
    assert!(
        validator.is_valid(&value),
        "schema rejected a request accepted by the Rust decoder: {value}"
    );
}

#[test]
fn language_schema_rejects_multiple_results_in_one_tool_message() {
    let request = LanguageRequest::new(vec![
        Message::assistant(vec![MessageContent::ToolCall(ToolCall {
            id: "call-1".to_owned(),
            name: "lookup".to_owned(),
            arguments: "{}".to_owned(),
            kind: ToolCallKind::Function,
        })])
        .unwrap(),
        Message::tool_result(
            "call-1",
            vec![MessageContent::Text {
                text: "result".to_owned(),
            }],
            false,
        )
        .unwrap(),
    ])
    .unwrap();
    let mut value = serde_json::to_value(request).unwrap();
    let duplicate = value["messages"][1]["content"][0].clone();
    value["messages"][1]["content"]
        .as_array_mut()
        .unwrap()
        .push(duplicate);
    let schema: Value = serde_json::from_str(LANGUAGE).unwrap();
    let validator = Validator::new(&schema).unwrap();

    assert!(!validator.is_valid(&value));
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
            "/$defs/tool/properties/freeform/properties/grammar/maxLength",
            MAX_FREEFORM_GRAMMAR_BYTES as u64,
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

    let image: Value = serde_json::from_str(IMAGE).expect("request schema");
    for (pointer, expected) in [
        ("/properties/prompt/maxLength", MAX_REQUEST_BYTES as u64),
        ("/properties/count/maximum", u64::from(MAX_IMAGE_OUTPUTS)),
        ("/properties/inputs/maxItems", u64::from(MAX_IMAGE_OUTPUTS)),
        ("/$defs/image/properties/byte_len/maximum", MAX_IMAGE_BYTES),
    ] {
        assert_eq!(schema_integer(&image, pointer), expected, "{pointer}");
    }
}
