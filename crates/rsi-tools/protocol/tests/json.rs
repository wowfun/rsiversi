use rsi_tools_protocol::{
    MAXIMUM_TOOL_IDENTIFIER_BYTES, MAXIMUM_TOOL_JSON_DEPTH, MAXIMUM_TOOL_JSON_NODES, ToolCall,
    ToolDefinition, ToolExecutionPolicy, ToolResult, ToolScheduling, parse_tool_arguments,
};
use serde_json::json;

#[test]
fn tool_scheduling_is_process_local_and_defaults_exclusive_on_decode() {
    for scheduling in [ToolScheduling::ParallelSafe, ToolScheduling::ExclusiveFinal] {
        let definition = ToolDefinition::new("read", "read", true.into())
            .unwrap()
            .with_scheduling(scheduling);
        let value = serde_json::to_value(&definition).unwrap();
        assert!(value.get("scheduling").is_none());
        let decoded: ToolDefinition = serde_json::from_value(value).unwrap();
        assert_eq!(decoded.scheduling(), ToolScheduling::Exclusive);
    }
}

#[test]
fn strict_tool_arguments_reject_duplicate_keys_at_every_depth() {
    for text in [
        r#"{"a":1,"a":2}"#,
        r#"{"outer":{"a":1,"a":2}}"#,
        r#"[{"a":1,"a":2}]"#,
    ] {
        assert!(parse_tool_arguments(text).is_err(), "accepted {text}");
    }
    assert_eq!(
        parse_tool_arguments(r#"{"b":2,"a":[true,null]}"#).unwrap(),
        json!({"b": 2, "a": [true, null]})
    );
}

#[test]
fn strict_tool_arguments_enforce_depth_and_trailing_data() {
    let too_deep = format!(
        "{}0{}",
        "[".repeat(MAXIMUM_TOOL_JSON_DEPTH + 1),
        "]".repeat(MAXIMUM_TOOL_JSON_DEPTH + 1)
    );
    assert!(parse_tool_arguments(&too_deep).is_err());
    assert!(parse_tool_arguments("{} true").is_err());
}

#[test]
fn strict_tool_arguments_reject_out_of_range_integers_and_preserve_large_u64() {
    for text in ["18446744073709551616", "-9223372036854775809"] {
        assert!(
            parse_tool_arguments(text).is_err(),
            "accepted out-of-range integer {text}"
        );
    }
    assert_eq!(
        parse_tool_arguments("18446744073709551615").unwrap(),
        json!(u64::MAX)
    );
}

#[test]
fn strict_tool_arguments_reject_numbers_that_cannot_round_trip_exactly() {
    for text in ["1.0000000000000001", "18446744073709551617"] {
        assert!(
            parse_tool_arguments(text).is_err(),
            "accepted lossy number {text}"
        );
    }
    assert_eq!(
        parse_tool_arguments("9007199254740993").unwrap(),
        json!(9_007_199_254_740_993_u64)
    );
}

#[test]
fn durable_tool_result_deserialization_revalidates_text() {
    let value = json!({
        "value": null,
        "content": [{"type": "text", "text": "bad\u{0}"}],
        "is_error": false
    });
    assert!(serde_json::from_value::<ToolResult>(value).is_err());
}

#[test]
fn durable_tool_result_rejects_noncanonical_enforcement_workspaces() {
    for workspace in [
        "relative/workspace",
        "/workspace/../escape",
        "/workspace/./child",
    ] {
        let value = json!({
            "value": null,
            "content": [],
            "is_error": false,
            "enforcement": [{
                "requested": "danger-full-access",
                "backend": {"kind": "unconfined"},
                "workspace": workspace,
                "filesystem": "unconfined",
                "scratch": "host",
                "network": "host"
            }]
        });
        assert!(
            serde_json::from_value::<ToolResult>(value).is_err(),
            "accepted noncanonical durable workspace {workspace:?}"
        );
    }
}

#[test]
fn model_facing_tool_text_rejects_terminal_escape_controls() {
    let value = json!({
        "value": null,
        "content": [{"type": "text", "text": "safe\u{1b}[31munsafe"}],
        "is_error": false
    });
    assert!(serde_json::from_value::<ToolResult>(value).is_err());
}

#[test]
fn typed_tool_definitions_enforce_json_structure_bounds() {
    let mut too_deep = serde_json::Value::Bool(true);
    for _ in 0..=MAXIMUM_TOOL_JSON_DEPTH {
        too_deep = json!({"nested": too_deep});
    }
    assert!(ToolDefinition::new("deep", "", too_deep).is_err());

    let too_many_nodes = json!({
        "nodes": vec![serde_json::Value::Null; MAXIMUM_TOOL_JSON_NODES]
    });
    assert!(ToolDefinition::new("wide", "", too_many_nodes).is_err());
}

#[test]
fn tool_deserialization_revalidates_calls_and_execution_policy() {
    assert!(
        serde_json::from_value::<ToolCall>(json!({
            "id": "x".repeat(MAXIMUM_TOOL_IDENTIFIER_BYTES + 1),
            "name": "tool",
            "arguments": null
        }))
        .is_err()
    );
    assert!(
        serde_json::from_value::<ToolExecutionPolicy>(json!({
            "mode": "workspace-write",
            "cwd": "relative",
            "workspace": "/workspace"
        }))
        .is_err()
    );
}
