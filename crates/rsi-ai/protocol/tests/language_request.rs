use rsi_ai_protocol::{
    FreeformFormat, FreeformToolDefinition, HostedTool, LanguageModelLimits, LanguageModelProfiles,
    LanguageProfile, LanguageRequest, LanguageSettings, MAX_REQUEST_BYTES, MediaDescriptor,
    MediaKind, Message, MessageContent, ProviderExtension, ProviderExtensionFormat,
    ReasoningEffort, ResponseFormat, ToolCall, ToolCallKind, ToolChoice, ToolDefinition,
};

#[test]
fn model_profiles_own_identifier_uniqueness_and_capacity_bounds() {
    let limits = LanguageModelLimits::new(128_000, 4_096, 16_384).expect("model limits");
    let profiles = LanguageModelProfiles::default()
        .with_profile("model-a", limits)
        .expect("first profile");

    assert_eq!(profiles.get("model-a"), Some(limits));
    assert_eq!(
        profiles
            .clone()
            .with_profile("model-a", limits)
            .expect_err("duplicate profile")
            .code(),
        "language_model_profiles.duplicate"
    );
    assert_eq!(
        profiles
            .with_profile("line\nbreak", limits)
            .expect_err("invalid model identifier")
            .code(),
        "language_model_profiles.invalid_id"
    );

    let mut full = LanguageModelProfiles::default();
    for index in 0..rsi_ai_protocol::MAX_LANGUAGE_MODEL_PROFILES {
        full = full
            .with_profile(format!("model-{index}"), limits)
            .expect("profile within capacity");
    }
    assert_eq!(
        full.with_profile("overflow", limits)
            .expect_err("profile capacity")
            .code(),
        "language_model_profiles.too_many"
    );
}
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

#[test]
fn freeform_tool_grammar_round_trips_without_removing_function_schema() {
    let tool = lookup()
        .with_freeform(
            FreeformToolDefinition::new(FreeformFormat::Lark, "start: /.+/")
                .expect("freeform grammar"),
        )
        .expect("freeform tool");
    let request = LanguageRequest::new(vec![Message::user_text("edit").expect("message")])
        .expect("request")
        .with_tools(vec![tool], ToolChoice::Auto)
        .expect("tools");
    let bytes = request.canonical_bytes().expect("canonical request");
    let decoded: LanguageRequest = serde_json::from_slice(&bytes).expect("decode request");
    let tool = &decoded.tools()[0];
    assert_eq!(tool.input_schema()["type"], "object");
    assert_eq!(
        tool.freeform().expect("freeform").format(),
        FreeformFormat::Lark
    );
    assert_eq!(tool.freeform().expect("freeform").grammar(), "start: /.+/");
}

#[test]
fn freeform_tool_grammar_is_bounded_at_construction() {
    let error = FreeformToolDefinition::new(
        FreeformFormat::Lark,
        "x".repeat(rsi_ai_protocol::MAX_FREEFORM_GRAMMAR_BYTES + 1),
    )
    .expect_err("oversized grammar");
    assert_eq!(error.code(), "tool.invalid_freeform");
}

#[test]
fn freeform_tool_grammar_counts_multibyte_text_as_utf8_bytes() {
    let error = FreeformToolDefinition::new(
        FreeformFormat::Lark,
        "é".repeat(rsi_ai_protocol::MAX_FREEFORM_GRAMMAR_BYTES / 2 + 1),
    )
    .expect_err("multibyte grammar exceeds the byte limit");
    assert_eq!(error.code(), "tool.invalid_freeform");
}

#[test]
fn aggregate_freeform_grammar_bound_counts_encoded_json_bytes() {
    let tools = (0..6)
        .map(|index| {
            ToolDefinition::new(format!("tool_{index}"), "", json!(true))
                .expect("tool")
                .with_freeform(
                    FreeformToolDefinition::new(
                        FreeformFormat::Lark,
                        "\u{1}".repeat(rsi_ai_protocol::MAX_FREEFORM_GRAMMAR_BYTES),
                    )
                    .expect("individually bounded grammar"),
                )
                .expect("freeform tool")
        })
        .collect();
    let error = LanguageRequest::new(vec![Message::user_text("edit").expect("message")])
        .expect("request")
        .with_tools(tools, ToolChoice::Auto)
        .expect_err("encoded aggregate must remain bounded");
    assert_eq!(error.code(), "request.tool_schemas_too_large");
}

#[test]
fn language_model_limits_revalidate_relations_during_deserialization() {
    let error = serde_json::from_value::<LanguageModelLimits>(json!({
        "context_window_tokens": 1,
        "default_output_reserve_tokens": 10,
        "max_output_reserve_tokens": 20
    }))
    .expect_err("invalid relational limits must not deserialize");
    assert!(error.to_string().contains("token limits"), "{error}");
}

#[test]
fn language_profile_and_extension_formats_revalidate_during_deserialization() {
    let error = serde_json::from_value::<LanguageProfile>(json!({
        "context_window_tokens": 1,
        "default_output_reserve_tokens": 10,
        "max_output_reserve_tokens": 20,
        "tool_dialect": "responses",
        "supports_freeform_tools": true,
        "image_tool_result": {"support": "unknown"},
        "accepted_provider_extensions": []
    }))
    .expect_err("invalid profile limits must not deserialize");
    assert!(error.to_string().contains("token limits"), "{error}");

    let error = serde_json::from_value::<ProviderExtensionFormat>(json!({
        "namespace": "not valid",
        "version": 1
    }))
    .expect_err("invalid extension namespace must not deserialize");
    assert!(error.to_string().contains("namespace"), "{error}");
}

#[test]
fn provider_extensions_and_reasoning_evidence_revalidate_at_every_public_boundary() {
    let invalid = ProviderExtension {
        namespace: "not valid".to_owned(),
        version: 1,
        value: json!({"proof": true}),
    };
    let error = Message::assistant(vec![MessageContent::Reasoning {
        text: "reasoning".to_owned(),
        evidence: Some(invalid.clone()),
    }])
    .expect_err("reasoning evidence must be validated");
    assert_eq!(error.code(), "message.invalid_content");

    let error = serde_json::from_value::<ProviderExtension>(
        serde_json::to_value(invalid).expect("extension JSON"),
    )
    .expect_err("a direct extension decode must validate its namespace");
    assert!(error.to_string().contains("namespace"), "{error}");
}

fn call(id: &str) -> MessageContent {
    MessageContent::ToolCall(ToolCall {
        id: id.to_owned(),
        name: "lookup".to_owned(),
        arguments: "{}".to_owned(),
        kind: ToolCallKind::Function,
    })
}

#[test]
fn tool_call_result_correlation_is_conversation_wide_and_unambiguous() {
    let error = LanguageRequest::new(vec![
        Message::assistant(vec![call("same")]).unwrap(),
        Message::assistant(vec![call("same")]).unwrap(),
    ])
    .expect_err("duplicate call ids");
    assert_eq!(error.code(), "request.duplicate_tool_call");

    let error = LanguageRequest::new(vec![
        Message::tool_result(
            "orphan",
            vec![MessageContent::Text {
                text: "result".to_owned(),
            }],
            false,
        )
        .unwrap(),
    ])
    .expect_err("orphan result");
    assert_eq!(error.code(), "request.orphan_tool_result");

    let result = || {
        Message::tool_result(
            "one",
            vec![MessageContent::Text {
                text: "result".to_owned(),
            }],
            false,
        )
        .unwrap()
    };
    let error = LanguageRequest::new(vec![
        Message::assistant(vec![call("one")]).unwrap(),
        result(),
        result(),
    ])
    .expect_err("duplicate results");
    assert_eq!(error.code(), "request.duplicate_tool_result");

    let error = LanguageRequest::new(vec![
        Message::tool_result(
            "later",
            vec![MessageContent::Text {
                text: "result".to_owned(),
            }],
            false,
        )
        .unwrap(),
        Message::assistant(vec![call("later")]).unwrap(),
    ])
    .expect_err("a result cannot precede its call");
    assert_eq!(error.code(), "request.orphan_tool_result");
}

#[test]
fn deserialization_enforces_tool_message_and_request_semantics() {
    let result = Message::tool_result(
        "one",
        vec![MessageContent::Text {
            text: "result".to_owned(),
        }],
        false,
    )
    .unwrap();
    let mut invalid_message = serde_json::to_value(&result).unwrap();
    let duplicate = invalid_message["content"][0].clone();
    invalid_message["content"]
        .as_array_mut()
        .unwrap()
        .push(duplicate);
    assert!(serde_json::from_value::<Message>(invalid_message).is_err());

    let request =
        LanguageRequest::new(vec![Message::assistant(vec![call("one")]).unwrap(), result]).unwrap();
    let mut invalid_request = serde_json::to_value(request).unwrap();
    invalid_request["messages"]
        .as_array_mut()
        .unwrap()
        .swap(0, 1);
    assert!(serde_json::from_value::<LanguageRequest>(invalid_request).is_err());
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
fn response_schema_reports_the_typed_json_node_limit() {
    let schema = json!({
        "nodes": vec![serde_json::Value::Null; rsi_ai_protocol::MAX_JSON_NODES]
    });
    let error = ResponseFormat::json_schema("too_many_nodes", None, schema)
        .expect_err("bounded JSON nodes");
    assert_eq!(error.code(), "json.too_many_nodes");
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
fn language_settings_revalidate_during_deserialization() {
    let error = serde_json::from_value::<LanguageSettings>(json!({
        "max_output_tokens": 0,
        "temperature": null,
        "top_p": null,
        "seed": null,
        "stop": [],
        "reasoning_effort": null
    }))
    .expect_err("zero output tokens must not enter a typed setting");
    assert!(error.to_string().contains("generation settings"), "{error}");
}

#[test]
fn media_descriptor_revalidates_during_deserialization() {
    let error = serde_json::from_value::<MediaDescriptor>(json!({
        "kind": "image",
        "mime_type": "image/png",
        "byte_len": 0,
        "sha256": "a".repeat(64),
        "width": null,
        "height": null,
        "duration_ms": null
    }))
    .expect_err("empty media must not enter a typed descriptor");
    assert!(error.to_string().contains("media.byte_len"), "{error}");
}

#[test]
fn tool_definition_revalidates_during_deserialization() {
    let error = serde_json::from_value::<ToolDefinition>(json!({
        "name": "not a tool name",
        "description": "",
        "input_schema": true
    }))
    .expect_err("invalid tool names must not enter a typed definition");
    assert!(error.to_string().contains("tool.name"), "{error}");
}

#[test]
fn response_format_revalidates_during_deserialization() {
    let error = serde_json::from_value::<ResponseFormat>(json!({
        "type": "json_schema",
        "name": "answer",
        "description": null,
        "schema": {"type": "string"},
        "strict": false
    }))
    .expect_err("non-strict schemas must not enter a typed response format");
    assert!(error.to_string().contains("must be strict"), "{error}");
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
fn deserialization_rejects_a_non_strict_response_schema() {
    let request = LanguageRequest::new(vec![Message::user_text("hello").expect("message")])
        .expect("request")
        .with_response_format(
            ResponseFormat::json_schema("answer", None, json!({"type":"string"}))
                .expect("response format"),
        )
        .expect("request");
    let mut value = serde_json::to_value(request).expect("request JSON");
    value["response_format"]["strict"] = json!(false);
    let error = serde_json::from_value::<LanguageRequest>(value)
        .expect_err("non-strict response format must fail at ingress");
    assert!(error.to_string().contains("must be strict"), "{error}");
}

#[test]
fn aggregate_request_size_is_enforced_at_construction() {
    let messages = (0..256)
        .map(|_| Message::user_text("x".repeat(65_536)).expect("bounded message"))
        .collect();
    let Err(error) = LanguageRequest::new(messages) else {
        panic!("aggregate request bound was not enforced");
    };
    assert_eq!(error.code(), "request.too_large");
}

#[test]
fn aggregate_request_size_is_rechecked_by_builders() {
    let request = LanguageRequest::new(vec![
        Message::user_text("x".repeat(MAX_REQUEST_BYTES - 200_000)).expect("bounded message"),
    ])
    .expect("base request remains below the aggregate limit");
    let error = request
        .with_extensions(vec![ProviderExtension {
            namespace: "fixture".to_owned(),
            version: 1,
            value: json!("x".repeat(200_000)),
        }])
        .expect_err("builder must recheck the complete request size");
    assert_eq!(error.code(), "request.too_large");
}
