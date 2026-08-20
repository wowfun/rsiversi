use rsi_ai_transport::ChatCompletionsChunk;

#[test]
fn nullable_compatible_arrays_decode_as_empty_arrays() {
    let content_chunk: ChatCompletionsChunk = serde_json::from_str(
        r#"{"choices":[{"delta":{"content":"hello","tool_calls":null},"finish_reason":null}]}"#,
    )
    .expect("nullable tool_calls");
    assert!(content_chunk.choices[0].delta.tool_calls.is_empty());

    let usage_chunk: ChatCompletionsChunk =
        serde_json::from_str(r#"{"choices":null,"usage":null}"#).expect("nullable choices");
    assert!(usage_chunk.choices.is_empty());
}
