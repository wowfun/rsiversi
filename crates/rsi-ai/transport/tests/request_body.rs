use std::sync::Arc;

use rsi_ai_transport::{JsonBase64Replacement, MAX_JSON_BASE64_REPLACEMENTS, json_base64_body};
use serde_json::json;

const TEST_BODY_LIMIT: usize = 1024 * 1024;

#[test]
fn text_only_json_is_buffered_while_media_json_is_streamed() {
    let buffered =
        json_base64_body(json!({"text":"hello"}), Vec::new(), TEST_BODY_LIMIT).expect("buffered");
    assert!(format!("{buffered:?}").contains("streaming: false"));

    let media: Arc<[u8]> = Arc::from([1, 2, 3]);
    let streamed = json_base64_body(
        json!({"media":null}),
        vec![JsonBase64Replacement::new(
            "/media",
            "data:application/octet-stream;base64,",
            Arc::clone(&media),
        )],
        TEST_BODY_LIMIT,
    )
    .expect("streamed");
    assert!(format!("{streamed:?}").contains("streaming: true"));
    drop(streamed);
    assert_eq!(Arc::strong_count(&media), 1);
}

#[test]
fn replacement_markers_cannot_match_inside_caller_strings() {
    let colliding_candidates = ["\0rsi-media-0-0\0", "\0rsi-media-0-1\0"];
    let expected = serde_json::to_vec(&json!({
        "media":"AQID",
        "caller":colliding_candidates,
    }))
    .unwrap();
    json_base64_body(
        json!({"media":null, "caller":colliding_candidates}),
        vec![JsonBase64Replacement::new(
            "/media",
            "",
            Arc::from([1, 2, 3]),
        )],
        expected.len(),
    )
    .expect("exact candidate collisions select a fresh marker and preserve exact size");
    assert_eq!(
        json_base64_body(
            json!({"media":null, "caller":colliding_candidates}),
            vec![JsonBase64Replacement::new(
                "/media",
                "",
                Arc::from([1, 2, 3]),
            )],
            expected.len() - 1,
        )
        .expect_err("marker collision must not undercount the final body")
        .code(),
        "http.request_body_too_large"
    );
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
            TEST_BODY_LIMIT,
        ),
        json_base64_body(
            json!({"media":"occupied"}),
            vec![JsonBase64Replacement::new("/media", "", Arc::clone(&bytes))],
            TEST_BODY_LIMIT,
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

    let body =
        json_base64_body(json!({"media":null}), vec![replacement], TEST_BODY_LIMIT).expect("body");
    assert!(!format!("{body:?}").contains("sensitive-media-bytes"));
}

#[test]
fn projected_json_body_limit_is_exact_before_streaming() {
    let replacement = || JsonBase64Replacement::new("/media", "", Arc::from([1_u8, 2, 3]));
    json_base64_body(json!({"media":null}), vec![replacement()], 16)
        .expect("exact projected length");
    let error = json_base64_body(json!({"media":null}), vec![replacement()], 15)
        .expect_err("one byte over projected body limit");
    assert_eq!(error.code(), "http.request_body_too_large");

    let buffered = serde_json::to_vec(&json!({"text":"hello"})).expect("template");
    json_base64_body(json!({"text":"hello"}), Vec::new(), buffered.len())
        .expect("exact buffered length");
    assert_eq!(
        json_base64_body(json!({"text":"hello"}), Vec::new(), buffered.len() - 1,)
            .expect_err("buffered body over limit")
            .code(),
        "http.request_body_too_large"
    );
}

#[test]
fn replacement_count_is_bounded_at_the_transport_seam() {
    let template = json!({
        "media": vec![serde_json::Value::Null; MAX_JSON_BASE64_REPLACEMENTS + 1]
    });
    let replacements = (0..=MAX_JSON_BASE64_REPLACEMENTS)
        .map(|index| JsonBase64Replacement::new(format!("/media/{index}"), "", Arc::from([0_u8])))
        .collect();
    let error = json_base64_body(template, replacements, TEST_BODY_LIMIT)
        .expect_err("replacement count overflow");
    assert_eq!(error.code(), "http.too_many_media_replacements");
}
