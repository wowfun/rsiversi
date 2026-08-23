use jsonschema::Validator;
use rsi_agent_protocol::{
    AppliedPatchChange, AppliedPatchChangeKind, AppliedPatchDelta, FreeformFormat,
    FreeformToolDefinition, MAX_BLOB_CHUNK_BYTES, MAX_CONTENT_CHARS, MAX_DATA_BYTES,
    MAX_FREEFORM_GRAMMAR_BYTES, MAX_JSON_DEPTH, MAX_RESULT_TEXT_BYTES, NotificationDelivery,
    ProtocolError, ToolBlobChunk, ToolContent, ToolDefinition, ToolImage, ToolPrivateResult,
    ToolResult, ToolsBlobEnd, ToolsBlobStart, ToolsCancelInvoke, ToolsCatalogResponse,
    ToolsCommitResult, ToolsEnvelope, ToolsInvokeRequest, ToolsInvokeResponse, ToolsNotification,
    ToolsOwnerOpenRequest, ToolsOwnerOpenResponse, WireError, canonical_json_bytes,
    canonicalize_json, parse_json_strict, parse_json_strict_f64,
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
        freeform: None,
    }
}

fn text(value: &str) -> ToolContent {
    ToolContent::Text {
        text: value.to_owned(),
    }
}

#[allow(clippy::too_many_lines)] // Keep every protocol kind in one exhaustive sample table.
fn samples() -> Vec<ToolsEnvelope> {
    vec![
        ToolsEnvelope::owner_open_request(
            "owner-open-1",
            ToolsOwnerOpenRequest {
                owner_id: "session-1".to_owned(),
                owner_epoch: "epoch-1".to_owned(),
                execution_cwd: "/workspace".to_owned(),
                tool_policy_sha256: "a".repeat(64),
            },
        ),
        ToolsEnvelope::owner_open_response(
            "owner-open-1",
            ToolsOwnerOpenResponse {
                owner_id: "session-1".to_owned(),
                owner_epoch: "epoch-1".to_owned(),
                terminal_types: vec!["shell".to_owned()],
            },
        ),
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
                time_budget_ms: 600_000,
            },
        ),
        ToolsEnvelope::invoke_response(
            "invoke-2",
            ToolsInvokeResponse {
                call_id: "call-1".to_owned(),
                result: ToolResult::Ok {
                    content: vec![text("hello")],
                },
                private_result: Some(ToolPrivateResult::AppliedPatchDelta {
                    delta: AppliedPatchDelta {
                        exact: true,
                        changes: vec![AppliedPatchChange {
                            kind: AppliedPatchChangeKind::Add,
                            path: "hello.txt".to_owned(),
                            move_to: None,
                            old_sha256: None,
                            new_sha256: Some("1".repeat(64)),
                            overwritten_sha256: None,
                        }],
                    },
                }),
            },
        ),
        ToolsEnvelope::cancel_invoke(
            "cancel-1",
            ToolsCancelInvoke {
                target_request_id: "invoke-1".to_owned(),
                reason: "turn cancelled".to_owned(),
            },
        ),
        ToolsEnvelope::notification(
            "notice-frame-1",
            ToolsNotification {
                notification_id: "notice-1".to_owned(),
                delivery: NotificationDelivery::Wakeup,
                content: vec![text("background job completed")],
            },
        ),
        ToolsEnvelope::commit_result(
            "commit-1",
            ToolsCommitResult {
                call_id: "call-1".to_owned(),
            },
        ),
        ToolsEnvelope::blob_start(
            "blob-start-1",
            ToolsBlobStart {
                blob: ToolImage {
                    blob_id: "blob-1".to_owned(),
                    mime_type: "image/png".to_owned(),
                    byte_len: 4,
                    sha256: "0".repeat(64),
                    width: Some(1),
                    height: Some(1),
                },
            },
        ),
        ToolsEnvelope::blob_end(
            "blob-end-1",
            ToolsBlobEnd {
                blob_id: "blob-1".to_owned(),
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
fn explicit_null_is_rejected_for_optional_nonnull_schema_fields() {
    let mut catalog = serde_json::to_value(&samples()[3]).unwrap();
    catalog["tools"][0]["freeform"] = Value::Null;
    let mut response = serde_json::to_value(&samples()[5]).unwrap();
    response["private_result"] = Value::Null;
    let mut change = serde_json::to_value(&samples()[5]).unwrap();
    change["private_result"]["delta"]["changes"][0]["move_to"] = Value::Null;
    let mut blob = serde_json::to_value(&samples()[9]).unwrap();
    blob["blob"]["width"] = Value::Null;

    for value in [catalog, response, change, blob] {
        assert!(
            ToolsEnvelope::decode(&serde_json::to_vec(&value).unwrap()).is_err(),
            "accepted explicit null: {value}"
        );
    }
}

#[test]
fn applied_patch_delta_round_trips_on_failed_results_and_rejects_invalid_shapes() {
    let response = ToolsEnvelope::invoke_response(
        "patch-response",
        ToolsInvokeResponse {
            call_id: "patch-call".to_owned(),
            result: ToolResult::Error {
                code: "patch_publish_failed".to_owned(),
                message: "later publication failed".to_owned(),
            },
            private_result: Some(ToolPrivateResult::AppliedPatchDelta {
                delta: AppliedPatchDelta {
                    exact: true,
                    changes: vec![AppliedPatchChange {
                        kind: AppliedPatchChangeKind::Add,
                        path: "committed.txt".to_owned(),
                        move_to: None,
                        old_sha256: None,
                        new_sha256: Some("a".repeat(64)),
                        overwritten_sha256: None,
                    }],
                },
            }),
        },
    );
    let bytes = response.encode().expect("private failure delta");
    assert_eq!(ToolsEnvelope::decode(&bytes).expect("round trip"), response);
    let schema: Value = serde_json::from_str(TOOLS_SCHEMA).expect("tools schema JSON");
    let validator = Validator::new(&schema).expect("compile tools schema");
    let value: Value = serde_json::from_slice(&bytes).expect("wire JSON");
    assert!(validator.is_valid(&value));

    let invalid = ToolsEnvelope::invoke_response(
        "patch-invalid",
        ToolsInvokeResponse {
            call_id: "patch-call".to_owned(),
            result: ToolResult::Ok {
                content: vec![text("applied")],
            },
            private_result: Some(ToolPrivateResult::AppliedPatchDelta {
                delta: AppliedPatchDelta {
                    exact: true,
                    changes: vec![AppliedPatchChange {
                        kind: AppliedPatchChangeKind::Delete,
                        path: "missing-digest.txt".to_owned(),
                        move_to: None,
                        old_sha256: None,
                        new_sha256: None,
                        overwritten_sha256: None,
                    }],
                },
            }),
        },
    );
    assert!(invalid.encode().is_err());

    let update_with_overwritten = ToolsEnvelope::invoke_response(
        "patch-update-overwritten",
        ToolsInvokeResponse {
            call_id: "patch-call".to_owned(),
            result: ToolResult::Ok {
                content: vec![text("applied")],
            },
            private_result: Some(ToolPrivateResult::AppliedPatchDelta {
                delta: AppliedPatchDelta {
                    exact: true,
                    changes: vec![AppliedPatchChange {
                        kind: AppliedPatchChangeKind::Update,
                        path: "updated.txt".to_owned(),
                        move_to: None,
                        old_sha256: Some("a".repeat(64)),
                        new_sha256: Some("b".repeat(64)),
                        overwritten_sha256: Some("c".repeat(64)),
                    }],
                },
            }),
        },
    );
    assert!(
        update_with_overwritten.encode().is_err(),
        "update provenance accepted a redundant overwritten digest"
    );
    let schema: Value = serde_json::from_str(TOOLS_SCHEMA).expect("tools schema JSON");
    let validator = Validator::new(&schema).expect("compile tools schema");
    assert!(!validator.is_valid(
        &serde_json::to_value(update_with_overwritten).expect("unvalidated update JSON")
    ));
}

#[test]
fn applied_patch_provenance_rejects_non_relative_or_non_normalized_paths() {
    for path in [
        "/absolute",
        "../escape",
        "nested/../escape",
        "nested//file",
        "nested/./file",
        "nested/.. /escape",
        "nested./file",
        "nested /file",
        "nested\\file",
        "C:/windows",
        "con",
        "nested/CON.txt",
        "aux",
        "com1.log",
        "LPT9",
        "nested/com¹.txt",
        "lpt³.log",
        "nested/line\nfeed",
        "nested/tab\tfile",
        "nested/next\u{0085}line",
        "trailing/",
    ] {
        let private = ToolPrivateResult::AppliedPatchDelta {
            delta: AppliedPatchDelta {
                exact: true,
                changes: vec![AppliedPatchChange {
                    kind: AppliedPatchChangeKind::Add,
                    path: path.to_owned(),
                    move_to: None,
                    old_sha256: None,
                    new_sha256: Some("a".repeat(64)),
                    overwritten_sha256: None,
                }],
            },
        };
        assert!(
            private.validate().is_err(),
            "accepted invalid path {path:?}"
        );
    }

    let ordinary_unicode = ToolPrivateResult::AppliedPatchDelta {
        delta: AppliedPatchDelta {
            exact: true,
            changes: vec![AppliedPatchChange {
                kind: AppliedPatchChangeKind::Add,
                path: "nested/ＣＯＮ.txt".to_owned(),
                move_to: None,
                old_sha256: None,
                new_sha256: Some("a".repeat(64)),
                overwritten_sha256: None,
            }],
        },
    };
    ordinary_unicode
        .validate()
        .expect("Unicode lookalikes remain ordinary portable path components");
}

#[test]
fn filesystem_paths_reject_controls_in_rust_and_schema() {
    let envelope = ToolsEnvelope::owner_open_request(
        "owner-control-path",
        ToolsOwnerOpenRequest {
            owner_id: "session-1".to_owned(),
            owner_epoch: "epoch-1".to_owned(),
            execution_cwd: "/workspace/escape\u{001b}[31m".to_owned(),
            tool_policy_sha256: "a".repeat(64),
        },
    );
    assert!(envelope.encode().is_err());

    let schema: Value = serde_json::from_str(TOOLS_SCHEMA).expect("schema JSON");
    let validator = Validator::new(&schema).expect("compiled schema");
    let value = serde_json::to_value(envelope).expect("unvalidated envelope JSON");
    assert!(!validator.is_valid(&value));
}

#[test]
fn tools_schema_boundaries_equal_the_rust_contract_constants() {
    let schema: Value = serde_json::from_str(TOOLS_SCHEMA).expect("schema JSON");
    let cases = [
        ("/oneOf/4/properties/arguments/maxLength", MAX_CONTENT_CHARS),
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
        (
            "/$defs/toolContent/oneOf/0/properties/text/maxLength",
            MAX_RESULT_TEXT_BYTES,
        ),
        (
            "/$defs/freeform/properties/grammar/maxLength",
            rsi_agent_protocol::MAX_FREEFORM_GRAMMAR_BYTES,
        ),
        (
            "/$defs/image/properties/byte_len/maximum",
            usize::try_from(rsi_agent_protocol::MAX_IMAGE_BYTES).expect("image limit fits usize"),
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
        r#"{"protocol":"wrong","version":1,"request_id":"x","kind":"catalog_request"}"#,
        r#"{"protocol":"rsi.agent.tools","version":0,"request_id":"x","kind":"catalog_request"}"#,
        r#"{"protocol":"rsi.agent.tools","version":1,"request_id":"bad id","kind":"catalog_request"}"#,
        r#"{"protocol":"rsi.agent.tools","version":1,"request_id":"x","kind":"catalog_request","extra":true}"#,
        r#"{"protocol":"rsi.agent.tools","version":1.0,"request_id":"x","kind":"catalog_request"}"#,
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
            time_budget_ms: 1,
        },
    );
    assert!(empty_arguments.encode().is_err());

    let oversized = ToolsEnvelope::invoke_request(
        "invoke",
        ToolsInvokeRequest {
            call_id: "call".to_owned(),
            name: "echo".to_owned(),
            arguments: "x".repeat(MAX_CONTENT_CHARS + 1),
            time_budget_ms: 1,
        },
    );
    assert!(oversized.encode().is_err());
}

#[test]
fn freeform_grammar_bound_is_measured_in_utf8_bytes() {
    assert!(
        FreeformToolDefinition::new(
            FreeformFormat::Lark,
            "é".repeat(MAX_FREEFORM_GRAMMAR_BYTES / 2 + 1),
        )
        .is_err()
    );
}

#[test]
fn agent_freeform_is_the_shared_ai_semantic_type() {
    let shared = rsi_ai_protocol::FreeformToolDefinition::new(
        rsi_ai_protocol::FreeformFormat::Lark,
        "start: /.+/",
    )
    .expect("shared freeform definition");
    let agent: FreeformToolDefinition = shared;

    assert_eq!(agent.format(), FreeformFormat::Lark);
    assert_eq!(agent.grammar(), "start: /.+/");
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
    for exact in [
        r#"{"n":18446744073709551616}"#,
        r#"{"n":100000000000000000000}"#,
        r#"{"n":-18446744073709551616}"#,
    ] {
        assert!(
            parse_json_strict_f64(exact).is_ok(),
            "rejected an exact integral f64: {exact}"
        );
    }
    for lossy in [
        r#"{"n":18446744073709551617}"#,
        r#"{"n":100000000000000000001}"#,
    ] {
        assert!(matches!(
            parse_json_strict_f64(lossy),
            Err(ProtocolError::LossyJsonNumber)
        ));
    }
    assert_eq!(
        parse_json_strict_f64(r#"{"n":9007199254740992}"#).expect("exact f64 integer"),
        json!({"n": 9_007_199_254_740_992_u64})
    );
    assert_eq!(
        parse_json_strict_f64(r#"{"n":1e3}"#).expect("integral exponent"),
        json!({"n": 1000_u64})
    );
}

#[test]
fn rat1_rejects_declared_chunk_bounds_before_materializing_fields() {
    const HEADER_BYTES: usize = 19;
    let id_len = rsi_agent_protocol::MAX_ID_BYTES + 1;
    let mut bytes = vec![0_u8; HEADER_BYTES + id_len + 1];
    bytes[..4].copy_from_slice(b"RAT1");
    bytes[4] = 1;
    bytes[5..7].copy_from_slice(&u16::try_from(id_len).unwrap().to_be_bytes());
    bytes[15..19].copy_from_slice(&1_u32.to_be_bytes());
    bytes[HEADER_BYTES..HEADER_BYTES + id_len].fill(b'a');
    bytes[HEADER_BYTES + id_len] = b'x';

    assert!(matches!(
        ToolBlobChunk::decode(&bytes),
        Err(ProtocolError::InvalidBinaryFrame { reason })
            if reason.contains("blob id length")
    ));
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
fn rich_result_text_is_bounded_by_aggregate_utf8_bytes() {
    let result = ToolResult::Ok {
        content: vec![
            ToolContent::Text {
                text: "é".repeat(MAX_RESULT_TEXT_BYTES / 2),
            },
            text("x"),
        ],
    };
    assert!(result.validate("result").is_err());
}

#[test]
fn image_mime_requires_a_nonempty_subtype_like_the_schema() {
    let result = ToolResult::Ok {
        content: vec![ToolContent::Image {
            image: ToolImage {
                blob_id: "blob-1".to_owned(),
                mime_type: "image/".to_owned(),
                byte_len: 1,
                sha256: "a".repeat(64),
                width: Some(1),
                height: Some(1),
            },
        }],
    };
    assert!(result.validate("result").is_err());
}

#[test]
fn rat1_chunks_round_trip_and_reject_truncation_trailing_data_and_oversize() {
    let chunk = ToolBlobChunk {
        blob_id: "blob-1".to_owned(),
        offset: 7,
        data: vec![1, 2, 3, 4],
    };
    let encoded = chunk.encode().expect("encode RAT1");
    assert_eq!(ToolBlobChunk::decode(&encoded).expect("decode RAT1"), chunk);

    assert!(ToolBlobChunk::decode(&encoded[..encoded.len() - 1]).is_err());
    let mut trailing = encoded.clone();
    trailing.push(0);
    assert!(ToolBlobChunk::decode(&trailing).is_err());
    assert!(
        ToolBlobChunk {
            blob_id: "blob-1".to_owned(),
            offset: 0,
            data: vec![0; MAX_BLOB_CHUNK_BYTES + 1],
        }
        .encode()
        .is_err()
    );
}
