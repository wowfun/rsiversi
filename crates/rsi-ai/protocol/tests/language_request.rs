use rsi_ai_protocol::{
    HostedTool, LanguageRequest, LanguageSettings, MediaDescriptor, MediaKind, Message,
    MessageContent, ReasoningEffort, ResponseFormat, ToolChoice, ToolDefinition,
};
use serde_json::json;

fn image() -> MediaDescriptor {
    MediaDescriptor::new(
        MediaKind::Image,
        "image/png",
        68,
        "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
    )
    .expect("valid descriptor")
    .with_image_dimensions(1, 1)
    .expect("valid dimensions")
}

fn lookup() -> ToolDefinition {
    ToolDefinition::new(
        "lookup",
        "Read one value.",
        json!({
            "type": "object",
            "required": ["key"],
            "properties": {"key": {"type": "string"}},
            "additionalProperties": false
        }),
    )
    .expect("valid tool")
}

#[test]
fn rich_language_request_is_bounded_and_contains_only_media_descriptors() {
    let request = LanguageRequest::new(vec![
        Message::developer_text("Answer with one object.").expect("developer message"),
        Message::user(vec![
            MessageContent::Text {
                text: "Inspect this image.".to_owned(),
            },
            MessageContent::Image(image()),
        ])
        .expect("user message"),
    ])
    .expect("request")
    .with_tools(vec![lookup()], ToolChoice::Auto)
    .expect("tools")
    .with_hosted_tools(vec![HostedTool::WebSearch { max_uses: Some(3) }])
    .expect("hosted tools")
    .with_response_format(
        ResponseFormat::json_schema(
            "answer",
            Some("One answer object.".to_owned()),
            json!({
                "type": "object",
                "required": ["answer"],
                "properties": {"answer": {"type": "string"}},
                "additionalProperties": false
            }),
        )
        .expect("response format"),
    )
    .expect("structured request");

    let encoded = request.canonical_bytes().expect("canonical request");
    let text = String::from_utf8(encoded).expect("JSON UTF-8");
    assert!(text.contains("\"sha256\":\"aaaaaaaa"));
    assert!(!text.contains("base64"));
    assert!(!text.contains("data:"));
    assert!(!text.contains("file://"));
    assert!(!text.contains("/tmp/"));
}

#[test]
fn request_rejects_duplicate_tool_names_before_provider_io() {
    let request =
        LanguageRequest::new(vec![Message::user_text("hello").expect("message")]).expect("request");
    let error = request
        .with_tools(vec![lookup(), lookup()], ToolChoice::Auto)
        .expect_err("duplicate tool names");
    assert_eq!(error.code(), "request.duplicate_tool");
    assert_eq!(error.field(), "tools");
}

#[test]
fn role_validation_rejects_reasoning_in_a_user_message() {
    let error = Message::user(vec![MessageContent::Reasoning {
        text: "private chain".to_owned(),
        evidence: None,
    }])
    .expect_err("reasoning is assistant-only");
    assert_eq!(error.code(), "message.invalid_content");
}

#[test]
fn response_schema_rejects_excessive_json_depth() {
    let mut schema = json!({"type": "string"});
    for _ in 0..=rsi_ai_protocol::MAX_JSON_DEPTH {
        schema = json!({"nested": schema});
    }
    let error =
        ResponseFormat::json_schema("too_deep", None, schema).expect_err("bounded JSON depth");
    assert_eq!(error.code(), "json.too_deep");
}

#[test]
fn language_settings_round_trip_in_the_canonical_request() {
    let settings = LanguageSettings::default()
        .with_max_output_tokens(4_096)
        .expect("token limit")
        .with_sampling(Some(0.25), Some(0.9))
        .expect("sampling")
        .with_seed(42)
        .with_stop(vec!["END".to_owned(), "STOP".to_owned()])
        .expect("stop sequences")
        .with_reasoning_effort(ReasoningEffort::High);
    let request = LanguageRequest::new(vec![Message::user_text("hello").expect("message")])
        .expect("request")
        .with_settings(settings.clone())
        .expect("settings");

    let encoded = request.canonical_bytes().expect("canonical request");
    let decoded: LanguageRequest = serde_json::from_slice(&encoded).expect("decode request");
    assert_eq!(decoded.settings(), &settings);
    assert_eq!(decoded.settings().max_output_tokens(), Some(4_096));
    assert_eq!(
        decoded.settings().reasoning_effort(),
        Some(ReasoningEffort::High)
    );
}

#[test]
fn language_settings_are_bounded_before_provider_io() {
    assert!(
        LanguageSettings::default()
            .with_max_output_tokens(0)
            .is_err()
    );
    assert!(
        LanguageSettings::default()
            .with_sampling(Some(f64::NAN), None)
            .is_err()
    );
    assert!(
        LanguageSettings::default()
            .with_sampling(None, Some(1.01))
            .is_err()
    );
    assert!(
        LanguageSettings::default()
            .with_stop(vec![String::new()])
            .is_err()
    );
    assert!(
        LanguageSettings::default()
            .with_stop((0..9).map(|index| format!("stop-{index}")).collect())
            .is_err()
    );
}

#[test]
fn nested_language_dtos_reject_unknown_fields() {
    let request =
        LanguageRequest::new(vec![Message::user_text("hello").expect("message")]).expect("request");
    let mut value = serde_json::to_value(request).expect("request JSON");
    value["messages"][0]["content"][0]["content"]["unknown"] = json!(true);
    assert!(serde_json::from_value::<LanguageRequest>(value).is_err());

    let request =
        LanguageRequest::new(vec![Message::user_text("hello").expect("message")]).expect("request");
    let mut value = serde_json::to_value(request).expect("request JSON");
    value["tool_choice"]["unknown"] = json!(true);
    assert!(serde_json::from_value::<LanguageRequest>(value).is_err());
}

#[test]
fn deserialized_non_strict_response_schema_fails_semantic_validation() {
    let request = LanguageRequest::new(vec![Message::user_text("hello").expect("message")])
        .expect("request")
        .with_response_format(
            ResponseFormat::json_schema("answer", None, json!({"type":"string"}))
                .expect("response format"),
        )
        .expect("request");
    let mut value = serde_json::to_value(request).expect("request JSON");
    value["response_format"]["strict"] = json!(false);
    let decoded: LanguageRequest = serde_json::from_value(value).expect("closed DTO");

    let error = decoded
        .canonical_bytes()
        .expect_err("non-strict response format");
    assert_eq!(error.code(), "response_format.invalid_strict");
}

#[test]
fn aggregate_request_size_is_enforced_by_the_single_canonicalization_boundary() {
    let messages = (0..256)
        .map(|_| Message::user_text("x".repeat(65_536)).expect("bounded message"))
        .collect();
    let request = LanguageRequest::new(messages).expect("structurally valid request");

    let error = request
        .canonical_bytes()
        .expect_err("aggregate canonical request bound");
    assert_eq!(error.code(), "request.too_large");
}
