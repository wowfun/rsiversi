use jsonschema::Validator;
use rsi_agent_protocol::{
    MAX_CONTENT_CHARS, MAX_DATA_BYTES, MAX_JSON_DEPTH, ProtocolError, ToolDefinition, ToolResult,
    ToolsBody, ToolsCatalogResponse, ToolsEnvelope, ToolsInvokeRequest, ToolsInvokeResponse,
    WireError, canonical_json_bytes, canonicalize_json, parse_json_strict, parse_json_strict_f64,
};
use serde_json::{Value, json};

const TOOLS_SCHEMA: &str = include_str!("../../../../schemas/rsi-agent/tools-envelope.schema.json");

fn definition(name: &str) -> ToolDefinition {
    ToolDefinition {
        name: name.to_owned(),
        description: "A deterministic test tool.".to_owned(),
        input_schema: json!({
            "type":"object",
            "properties":{"text":{"type":"string"}},
            "required":["text"],
            "additionalProperties":false
        }),
    }
}

fn samples() -> Vec<ToolsEnvelope> {
    vec![
        ToolsEnvelope::catalog_request("catalog-1"),
        ToolsEnvelope::catalog_response(
            "catalog-2",
            ToolsCatalogResponse {
                tools: vec![definition("echo")],
            },
        ),
        ToolsEnvelope::invoke_request(
            "invoke-1",
            ToolsInvokeRequest {
                call_id: "call-1".to_owned(),
                name: "echo".to_owned(),
                arguments: r#"{"text":"hello"}"#.to_owned(),
            },
        ),
        ToolsEnvelope::invoke_response(
            "invoke-2",
            ToolsInvokeResponse {
                call_id: "call-1".to_owned(),
                result: ToolResult::Ok {
                    value: json!({"nested":{"b":2,"a":1}}),
                },
            },
        ),
        ToolsEnvelope::error(
            "error-1",
            WireError {
                code: "fixture_failure".to_owned(),
                message: "rejected".to_owned(),
            },
        ),
    ]
}

#[test]
fn every_tools_kind_is_canonical_roundtrippable_and_schema_valid() {
    let schema: Value = serde_json::from_str(TOOLS_SCHEMA).expect("schema JSON");
    let validator = Validator::new(&schema).expect("compiled schema");
    for envelope in samples() {
        let bytes = envelope.encode().expect("encode");
        assert!(bytes.len() <= MAX_DATA_BYTES);
        assert_eq!(ToolsEnvelope::decode(&bytes).expect("decode"), envelope);
        let value: Value = serde_json::from_slice(&bytes).expect("JSON");
        assert!(validator.is_valid(&value), "schema rejected {value}");
        assert_eq!(bytes, canonical_json_bytes(&value).expect("canonical"));
    }
}

#[test]
fn tools_schema_boundaries_equal_the_rust_contract_constants() {
    let schema: Value = serde_json::from_str(TOOLS_SCHEMA).expect("schema JSON");
    let cases = [
        ("/oneOf/2/properties/arguments/maxLength", MAX_CONTENT_CHARS),
        (
            "/$defs/identifier/maxLength",
            rsi_agent_protocol::MAX_ID_BYTES,
        ),
        (
            "/$defs/toolName/maxLength",
            rsi_agent_protocol::MAX_ID_BYTES,
        ),
        (
            "/$defs/errorCode/maxLength",
            rsi_agent_protocol::MAX_ERROR_CODE_BYTES,
        ),
        (
            "/$defs/error/properties/message/maxLength",
            rsi_agent_protocol::MAX_ERROR_MESSAGE_CHARS,
        ),
        (
            "/$defs/toolResult/oneOf/1/properties/message/maxLength",
            rsi_agent_protocol::MAX_ERROR_MESSAGE_CHARS,
        ),
        (
            "/$defs/toolDefinition/properties/description/maxLength",
            rsi_agent_protocol::MAX_DESCRIPTION_CHARS,
        ),
        ("/$defs/catalog/maxItems", rsi_agent_protocol::MAX_TOOLS),
    ];
    for (pointer, expected) in cases {
        assert_eq!(
            schema.pointer(pointer).and_then(Value::as_u64),
            Some(expected as u64),
            "{pointer}"
        );
    }
}

#[test]
fn wire_identifier_predicate_owns_the_shared_ascii_grammar() {
    assert!(rsi_agent_protocol::is_wire_identifier("!model~"));
    assert!(rsi_agent_protocol::is_wire_identifier(
        &"x".repeat(rsi_agent_protocol::MAX_ID_BYTES)
    ));
    assert!(!rsi_agent_protocol::is_wire_identifier(""));
    assert!(!rsi_agent_protocol::is_wire_identifier("has space"));
    assert!(!rsi_agent_protocol::is_wire_identifier("line\nfeed"));
    assert!(!rsi_agent_protocol::is_wire_identifier("é"));
    assert!(!rsi_agent_protocol::is_wire_identifier(
        &"x".repeat(rsi_agent_protocol::MAX_ID_BYTES + 1)
    ));
}

#[test]
fn closed_headers_unknown_fields_and_nonzero_versions_fail_closed() {
    for text in [
        r#"{"protocol":"wrong","version":0,"request_id":"x","kind":"catalog_request"}"#,
        r#"{"protocol":"rsi.agent.tools","version":1,"request_id":"x","kind":"catalog_request"}"#,
        r#"{"protocol":"rsi.agent.tools","version":0,"request_id":"bad id","kind":"catalog_request"}"#,
        r#"{"protocol":"rsi.agent.tools","version":0,"request_id":"x","kind":"catalog_request","extra":true}"#,
        r#"{"protocol":"rsi.agent.tools","version":0.0,"request_id":"x","kind":"catalog_request"}"#,
    ] {
        assert!(
            ToolsEnvelope::decode(text.as_bytes()).is_err(),
            "accepted {text}"
        );
    }
}

#[test]
fn catalogs_and_results_enforce_semantic_bounds() {
    let duplicate = ToolsEnvelope::catalog_response(
        "catalog",
        ToolsCatalogResponse {
            tools: vec![definition("echo"), definition("echo")],
        },
    );
    assert!(duplicate.encode().is_err());

    let empty_arguments = ToolsEnvelope::invoke_request(
        "invoke",
        ToolsInvokeRequest {
            call_id: "call".to_owned(),
            name: "echo".to_owned(),
            arguments: String::new(),
        },
    );
    assert!(empty_arguments.encode().is_err());

    let oversized = ToolsEnvelope::invoke_request(
        "invoke",
        ToolsInvokeRequest {
            call_id: "call".to_owned(),
            name: "echo".to_owned(),
            arguments: "x".repeat(MAX_CONTENT_CHARS + 1),
        },
    );
    assert!(oversized.encode().is_err());
}

#[test]
fn strict_json_rejects_duplicate_keys_depth_and_trailing_data() {
    assert!(matches!(
        parse_json_strict(r#"{"a":1,"a":2}"#),
        Err(ProtocolError::DuplicateJsonKey { .. })
    ));
    let nested = format!(
        "{}0{}",
        "[".repeat(MAX_JSON_DEPTH + 1),
        "]".repeat(MAX_JSON_DEPTH + 1)
    );
    assert!(matches!(
        parse_json_strict(&nested),
        Err(ProtocolError::JsonLimit { .. })
    ));
    assert!(parse_json_strict("true false").is_err());
}

#[test]
fn strict_json_parser_matches_serde_json_on_ordinary_syntax_corpus() {
    let valid = [
        "null",
        " true ",
        r"[false,0,1.25,1e2,-3.5E-2]",
        r#"{"z":1,"a":[null,{"escaped":"line\\nfeed","unicode":"\\u4f60\\u597d","pair":"\\ud83d\\ude80"}]}"#,
        r#"{"empty_object":{},"empty_array":[],"solidus":"\\/","controls":"\\b\\f\\n\\r\\t"}"#,
    ];
    for text in valid {
        let serde_value: Value = serde_json::from_str(text).expect("serde accepts corpus value");
        let expected = canonicalize_json(&serde_value).expect("canonical serde value");
        assert_eq!(parse_json_strict(text).expect(text), expected, "{text}");
    }

    let invalid = [
        "",
        "nul",
        "[1,]",
        r#"{"a":1,}"#,
        r#"{"a" 1}"#,
        r#""unterminated"#,
        r#""bad\xescape""#,
        "\"raw\ncontrol\"",
        "01",
        "1.",
        "1e",
        "--1",
        "true false",
    ];
    for text in invalid {
        assert!(
            serde_json::from_str::<Value>(text).is_err(),
            "serde accepted {text:?}"
        );
        assert!(
            parse_json_strict(text).is_err(),
            "strict parser accepted {text:?}"
        );
    }
}

#[test]
fn strict_json_normalizes_integer_negative_zero_independently_of_serde_features() {
    assert_eq!(parse_json_strict("-0").expect("negative zero"), json!(0));
}

#[test]
fn strict_json_reports_out_of_range_numbers_as_lossy_not_syntax() {
    assert!(matches!(
        parse_json_strict("1e9999"),
        Err(ProtocolError::LossyJsonNumber)
    ));
}

#[test]
fn f64_strict_json_rejects_numbers_that_would_change_before_dispatch() {
    assert!(matches!(
        parse_json_strict_f64(r#"{"n":9007199254740993}"#),
        Err(ProtocolError::LossyJsonNumber)
    ));
    assert!(matches!(
        parse_json_strict_f64(r#"{"n":1e-999}"#),
        Err(ProtocolError::LossyJsonNumber)
    ));
    assert_eq!(
        parse_json_strict_f64(r#"{"n":9007199254740992}"#).expect("exact f64 integer"),
        json!({"n": 9_007_199_254_740_992.0_f64})
    );
}

#[test]
fn arbitrary_json_is_sorted_without_changing_array_order_or_numbers() {
    let input = json!({"z":[3,2,1],"a":{"y":2,"x":1},"n":1.25});
    let canonical = canonicalize_json(&input).expect("canonical value");
    assert_eq!(
        canonical_json_bytes(&canonical).expect("bytes"),
        br#"{"a":{"x":1,"y":2},"n":1.25,"z":[3,2,1]}"#
    );
}

#[test]
fn arbitrary_precision_result_values_survive_the_typed_envelope() {
    let text = br#"{"protocol":"rsi.agent.tools","version":0,"request_id":"x","kind":"invoke_response","call_id":"call","result":{"status":"ok","value":{"n":9007199254740993}}}"#;
    let envelope = ToolsEnvelope::decode(text).expect("exact integer");
    let ToolsBody::InvokeResponse(response) = envelope.body else {
        panic!("response kind")
    };
    assert_eq!(
        response.result,
        ToolResult::Ok {
            value: json!({"n": 9_007_199_254_740_993_u64})
        }
    );
}
