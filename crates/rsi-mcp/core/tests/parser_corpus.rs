#![cfg(feature = "test-support")]

use rsi_mcp::{McpError, exercise_frame_parser};
use serde_json::json;

#[test]
fn fuzz_seeds_exercise_json_rpc_and_every_sse_byte_boundary() {
    let response = include_bytes!("corpus/response.json");
    let event = include_bytes!("corpus/event.txt");
    for chunk in 1..=response.len().max(event.len()) {
        let parsed = exercise_frame_parser(response, chunk);
        assert_eq!(
            parsed.json,
            Ok(json!({"jsonrpc":"2.0","id":"一","result":{"text":"🌱"}}))
        );
        let parsed = exercise_frame_parser(event, chunk);
        assert_eq!(
            parsed.messages,
            vec![json!({"jsonrpc":"2.0","id":"一","result":{}})]
        );
        assert_eq!(parsed.event_error, None);
    }
    for source in [
        b"{\"jsonrpc\":\"2.0\",\"method\":\"ping\",\"id\":1}".as_slice(),
        b"{\"jsonrpc\":\"2.0\",\"method\":\"notifications/tools/list_changed\"}",
    ] {
        assert!(exercise_frame_parser(source, 13).json.is_ok());
    }
    assert_eq!(
        exercise_frame_parser(&[255], 0).json,
        Err(McpError::Protocol)
    );
    let invalid = exercise_frame_parser(b"data: invalid\n\n", 1);
    assert!(invalid.messages.is_empty());
    assert_eq!(invalid.event_error, Some(McpError::Protocol));
    let oversized = exercise_frame_parser(&vec![b'x'; rsi_mcp::MAXIMUM_FRAME_BYTES + 1], 1);
    assert_eq!(oversized.json, Err(McpError::Capacity));
    assert!(oversized.messages.is_empty());
    assert_eq!(oversized.event_error, Some(McpError::Capacity));
}

#[test]
fn oversized_multiline_sse_still_reaches_the_transport_parser() {
    let half = "x".repeat(rsi_mcp::MAXIMUM_FRAME_BYTES / 2);
    let source = format!("data: {half}\ndata: {half}\n");
    let parsed = exercise_frame_parser(source.as_bytes(), 17);
    assert!(parsed.messages.is_empty());
    assert_eq!(parsed.event_error, Some(McpError::Capacity));
}

#[test]
fn parser_exercise_bounds_work_without_changing_transport_stream_limits() {
    let source = b"data: {\"jsonrpc\":\"2.0\",\"method\":\"a\"}\n\n".repeat(129);
    let parsed = exercise_frame_parser(&source, 7);
    assert_eq!(parsed.messages.len(), 128);
    assert_eq!(parsed.event_error, Some(McpError::Capacity));
    let parsed = exercise_frame_parser(&vec![b'x'; rsi_mcp::MAXIMUM_FRAME_BYTES * 2 + 1], 1);
    assert!(parsed.messages.is_empty());
    assert_eq!(parsed.event_error, Some(McpError::Capacity));
}
