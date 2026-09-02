use rsi_ai_protocol::{
    FreeformFormat, FreeformToolDefinition, HostedTool, LanguageModelLimits, LanguageModelProfiles,
    LanguageProfile, LanguageRequest, LanguageSettings, MAX_EXTENSION_BYTES,
    MAX_LANGUAGE_MEDIA_BYTES, MAX_LANGUAGE_MEDIA_OCCURRENCES, MAX_REQUEST_BYTES, MediaDescriptor,
    MediaKind, Message, MessageContent, ProviderExtension, ProviderExtensionFormat,
    ReasoningEffort, ResponseFormat, ToolCall, ToolCallKind, ToolChoice, ToolDefinition,
    validate_json_structure,
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
    assert!(error.to_string().contains("freeform grammar"), "{error}");
}

#[test]
fn freeform_tool_grammar_counts_multibyte_text_as_utf8_bytes() {
    let error = FreeformToolDefinition::new(
        FreeformFormat::Lark,
        "é".repeat(rsi_ai_protocol::MAX_FREEFORM_GRAMMAR_BYTES / 2 + 1),
    )
    .expect_err("multibyte grammar exceeds the byte limit");
    assert!(error.to_string().contains("freeform grammar"), "{error}");
}

#[test]
fn aggregate_freeform_grammar_bound_counts_encoded_json_bytes() {
    let tools = (0..17)
        .map(|index| {
            ToolDefinition::new(format!("tool_{index}"), "", json!(true))
                .expect("tool")
                .with_freeform(
                    FreeformToolDefinition::new(
                        FreeformFormat::Lark,
                        "\n".repeat(rsi_ai_protocol::MAX_FREEFORM_GRAMMAR_BYTES),
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
fn provider_extensions_are_closed_at_construction_and_decode() {
    assert!(ProviderExtension::new("not valid", 1, json!({"proof": true})).is_err());
    let error = serde_json::from_value::<ProviderExtension>(json!({
        "namespace": "not valid",
        "version": 1,
        "value": {"proof": true}
    }))
    .expect_err("a direct extension decode must validate its namespace");
    assert!(error.to_string().contains("namespace"), "{error}");
}

#[test]
fn provider_extension_bound_includes_its_complete_wire_envelope() {
    let error = ProviderExtension::new("fixture", 1, json!("x".repeat(MAX_EXTENSION_BYTES - 2)))
        .expect_err("a maximum-size value leaves no room for the extension envelope");
    assert_eq!(error.code(), "stream.extension_too_large");
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

fn media(kind: MediaKind, byte_len: u64) -> MediaDescriptor {
    let mime = match kind {
        MediaKind::Image => "image/png",
        MediaKind::Audio => "audio/wav",
    };
    MediaDescriptor::new(kind, mime, byte_len, "b".repeat(64)).expect("media descriptor")
}

#[test]
fn language_request_bounds_media_occurrences_before_provider_io() {
    let exact = Message::user(
        (0..MAX_LANGUAGE_MEDIA_OCCURRENCES)
            .map(|_| MessageContent::Image(media(MediaKind::Image, 1)))
            .collect(),
    )
    .expect("per-message maximum equals aggregate media maximum");
    LanguageRequest::new(vec![exact]).expect("exact media occurrence limit");

    let overflow = LanguageRequest::new(vec![
        Message::user(
            (0..MAX_LANGUAGE_MEDIA_OCCURRENCES)
                .map(|_| MessageContent::Image(media(MediaKind::Image, 1)))
                .collect(),
        )
        .expect("full first message"),
        Message::user(vec![MessageContent::Image(media(MediaKind::Image, 1))])
            .expect("one extra media occurrence"),
    ])
    .expect_err("media occurrence overflow");
    assert_eq!(overflow.code(), "request.too_many_media");
}

#[test]
fn language_request_counts_media_nested_inside_tool_results() {
    let conversation = |direct_media| {
        vec![
            Message::user(
                (0..direct_media)
                    .map(|_| MessageContent::Image(media(MediaKind::Image, 1)))
                    .collect(),
            )
            .expect("direct media"),
            Message::assistant(vec![MessageContent::ToolCall(ToolCall {
                id: "call-1".to_owned(),
                name: "inspect".to_owned(),
                arguments: "{}".to_owned(),
                kind: ToolCallKind::Function,
            })])
            .expect("assistant tool call"),
            Message::tool_result(
                "call-1",
                vec![MessageContent::Image(media(MediaKind::Image, 1))],
                false,
            )
            .expect("image tool result"),
        ]
    };

    LanguageRequest::new(conversation(MAX_LANGUAGE_MEDIA_OCCURRENCES - 1))
        .expect("nested image at exact aggregate limit");
    let error = LanguageRequest::new(conversation(MAX_LANGUAGE_MEDIA_OCCURRENCES))
        .expect_err("nested image exceeds aggregate limit");
    assert_eq!(error.code(), "request.too_many_media");
}

#[test]
fn language_request_bounds_declared_media_bytes_with_checked_arithmetic() {
    let exact = LanguageRequest::new(vec![
        Message::user(vec![
            MessageContent::Audio(media(MediaKind::Audio, 128 * 1024 * 1024)),
            MessageContent::Audio(media(MediaKind::Audio, 128 * 1024 * 1024)),
        ])
        .expect("exact aggregate media bytes"),
    ])
    .expect("exact aggregate media bytes are admitted");
    assert_eq!(MAX_LANGUAGE_MEDIA_BYTES, 256 * 1024 * 1024);
    assert_eq!(exact.messages().len(), 1);

    let overflow = LanguageRequest::new(vec![
        Message::user(vec![
            MessageContent::Audio(media(MediaKind::Audio, 128 * 1024 * 1024)),
            MessageContent::Audio(media(MediaKind::Audio, 128 * 1024 * 1024)),
            MessageContent::Image(media(MediaKind::Image, 1)),
        ])
        .expect("individually valid media descriptors"),
    ])
    .expect_err("aggregate media byte overflow");
    assert_eq!(overflow.code(), "request.media_bytes_exceeded");
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
fn request_revalidates_imported_tool_definitions_at_the_ai_boundary() {
    let request =
        LanguageRequest::new(vec![Message::user_text("hello").expect("message")]).expect("request");

    let tools_valid_ai_rejects = ToolDefinition::new(
        "a".repeat(rsi_tools_protocol::MAXIMUM_TOOL_IDENTIFIER_BYTES),
        "",
        serde_json::Value::Bool(true),
    )
    .expect("valid Tools definition");
    let error = request
        .clone()
        .with_tools(vec![tools_valid_ai_rejects], ToolChoice::Auto)
        .expect_err("AI owns the narrower provider-neutral name bound");
    assert_eq!(error.code(), "tool.invalid_name");

    let tools_valid_ai_rejects = ToolDefinition::new(
        "wide",
        "",
        json!({
            "nodes": vec![serde_json::Value::Null; rsi_ai_protocol::MAX_JSON_NODES]
        }),
    )
    .expect("within the Tools JSON node bound");
    let error = request
        .with_tools(vec![tools_valid_ai_rejects], ToolChoice::Auto)
        .expect_err("AI owns its lower provider-neutral JSON node bound");
    assert_eq!(error.code(), "json.too_many_nodes");
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
fn public_json_structure_validator_enforces_exact_depth_and_node_bounds() {
    fn require_standard_error<T: std::error::Error>() {}
    require_standard_error::<rsi_ai_protocol::JsonStructureError>();

    let mut exact_depth = serde_json::Value::Null;
    for _ in 0..rsi_ai_protocol::MAX_JSON_DEPTH {
        exact_depth = json!([exact_depth]);
    }
    validate_json_structure(&exact_depth).expect("exact depth");
    assert_eq!(
        validate_json_structure(&json!([exact_depth])).expect_err("depth overflow"),
        rsi_ai_protocol::JsonStructureError::TooDeep
    );

    let exact_nodes = json!(vec![
        serde_json::Value::Null;
        rsi_ai_protocol::MAX_JSON_NODES - 1
    ]);
    validate_json_structure(&exact_nodes).expect("array plus children equals node limit");
    assert_eq!(
        validate_json_structure(&json!(vec![
            serde_json::Value::Null;
            rsi_ai_protocol::MAX_JSON_NODES
        ]))
        .expect_err("node overflow"),
        rsi_ai_protocol::JsonStructureError::TooManyNodes
    );
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
    assert!(error.to_string().contains("media byte length"), "{error}");
}

#[test]
fn tool_definition_revalidates_during_deserialization() {
    let error = serde_json::from_value::<ToolDefinition>(json!({
        "name": "not a tool name",
        "description": "",
        "input_schema": true
    }))
    .expect_err("invalid tool names must not enter a typed definition");
    assert!(error.to_string().contains("tool name"), "{error}");
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
        .with_extensions(vec![
            ProviderExtension::new("fixture", 1, json!("x".repeat(200_000))).unwrap(),
        ])
        .expect_err("builder must recheck the complete request size");
    assert_eq!(error.code(), "request.too_large");
}

#[test]
fn canonical_request_preserves_provider_extension_integers_above_two_to_the_53() {
    let extension: ProviderExtension = serde_json::from_str(
        r#"{"namespace":"fixture","version":1,"value":{"exact":9007199254740993}}"#,
    )
    .expect("exact extension");
    let request = LanguageRequest::new(vec![Message::user_text("hello").expect("message")])
        .expect("request")
        .with_extensions(vec![extension])
        .expect("extension");
    let encoded = request.canonical_bytes().expect("canonical request");
    let text = std::str::from_utf8(&encoded).expect("UTF-8 JSON");
    assert!(text.contains("9007199254740993"), "{text}");
    let restored: LanguageRequest = serde_json::from_slice(&encoded).expect("round trip");
    assert_eq!(restored.canonical_bytes().unwrap(), encoded);
}
