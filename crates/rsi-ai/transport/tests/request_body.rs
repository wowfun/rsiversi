use std::sync::Arc;

use rsi_ai_transport::{JsonBase64Replacement, json_base64_body};
use serde_json::json;

#[test]
fn text_only_json_is_buffered_while_media_json_is_streamed() {
    let buffered = json_base64_body(json!({"text":"hello"}), Vec::new()).expect("buffered");
    assert!(format!("{buffered:?}").contains("streaming: false"));

    let media: Arc<[u8]> = Arc::from([1, 2, 3]);
    let streamed = json_base64_body(
        json!({"media":null}),
        vec![JsonBase64Replacement::new(
            "/media",
            "data:application/octet-stream;base64,",
            Arc::clone(&media),
        )],
    )
    .expect("streamed");
    assert!(format!("{streamed:?}").contains("streaming: true"));
    drop(streamed);
    assert_eq!(Arc::strong_count(&media), 1);
}

#[test]
fn replacement_markers_cannot_match_inside_caller_strings() {
    let colliding_candidates = ["\0rsi-media-0-0\0", "\0rsi-media-0-1\0"];
    json_base64_body(
        json!({"media":null, "caller":colliding_candidates}),
        vec![JsonBase64Replacement::new(
            "/media",
            "",
            Arc::from([1, 2, 3]),
        )],
    )
    .expect("exact candidate collisions select a fresh marker");
}

#[test]
fn replacement_slots_must_exist_and_be_null() {
    let bytes: Arc<[u8]> = Arc::from([1, 2, 3]);
    for result in [
        json_base64_body(
            json!({"media":null}),
            vec![JsonBase64Replacement::new(
                "/missing",
                "",
                Arc::clone(&bytes),
            )],
        ),
        json_base64_body(
            json!({"media":"occupied"}),
            vec![JsonBase64Replacement::new("/media", "", Arc::clone(&bytes))],
        ),
    ] {
        let error = result.expect_err("invalid replacement slot");
        assert_eq!(error.code(), "http.invalid_body_template");
    }
}

#[test]
fn request_body_debug_never_contains_binary_media() {
    let secret = b"sensitive-media-bytes";
    let replacement = JsonBase64Replacement::new(
        "/media",
        "data:audio/wav;base64,",
        Arc::from(secret.as_slice()),
    );
    let replacement_debug = format!("{replacement:?}");
    assert!(!replacement_debug.contains("sensitive-media-bytes"));
    assert!(replacement_debug.contains(&secret.len().to_string()));

    let body = json_base64_body(json!({"media":null}), vec![replacement]).expect("body");
    assert!(!format!("{body:?}").contains("sensitive-media-bytes"));
}
