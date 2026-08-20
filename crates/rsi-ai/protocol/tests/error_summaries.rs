use rsi_ai_protocol::{MAX_ERROR_SUMMARY_BYTES, sanitize_error_summary};

#[test]
fn provider_error_summaries_are_safe_utf8_and_never_empty() {
    let boundary = format!("{}é", "a".repeat(MAX_ERROR_SUMMARY_BYTES - 1));
    let sanitized = sanitize_error_summary(&boundary);
    assert!(sanitized.len() <= MAX_ERROR_SUMMARY_BYTES);
    assert!(sanitized.is_char_boundary(sanitized.len()));

    let unsafe_text = sanitize_error_summary("bad\0provider\u{7f}message");
    assert!(!unsafe_text.contains(['\0', '\u{7f}']));
    assert_eq!(sanitize_error_summary(""), "provider error");
}
