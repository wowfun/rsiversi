use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

use async_stream::stream;
use bytes::Bytes;
use rsi_ai_transport::{ByteStream, JsonProjectionLimits, project_json_body};
use serde::Deserialize;

#[derive(Debug, Deserialize, Eq, PartialEq)]
struct ControlProjection {
    id: Option<String>,
    status: Option<String>,
}

fn streamed(chunks: Vec<Vec<u8>>, observed: Arc<AtomicUsize>) -> ByteStream {
    Box::pin(stream! {
        for chunk in chunks {
            observed.fetch_add(1, Ordering::SeqCst);
            yield Ok(Bytes::from(chunk));
        }
    })
}

fn limits(id_bytes: usize) -> JsonProjectionLimits {
    JsonProjectionLimits::new(1024 * 1024)
        .and_then(|limits| limits.with_top_level_string("id", id_bytes))
        .and_then(|limits| limits.with_top_level_string("status", 16))
        .expect("projection limits")
}

#[tokio::test]
async fn selected_string_bound_stops_the_stream_before_an_ignored_tail() {
    let observed = Arc::new(AtomicUsize::new(0));
    let mut chunks = vec![br#"{"id":"123456789"#.to_vec()];
    chunks.extend((0..128).map(|_| vec![b'x'; 1024]));
    chunks.push(br#"","status":"queued"}"#.to_vec());

    let error =
        project_json_body::<ControlProjection>(streamed(chunks, Arc::clone(&observed)), limits(8))
            .await
            .expect_err("the selected id must be rejected at its own bound");

    assert_eq!(error.code(), "json.project_limit");
    assert!(
        observed.load(Ordering::SeqCst) < 128,
        "the parser must disconnect before consuming the ignored tail"
    );
}

#[tokio::test]
async fn selected_string_bound_counts_decoded_escapes_and_escaped_keys() {
    let observed = Arc::new(AtomicUsize::new(0));
    let projection = project_json_body::<ControlProjection>(
        streamed(
            vec![br#"{"\u0069d":"1234\u0035","padding":[1,2,3],"status":"queued"}"#.to_vec()],
            observed,
        ),
        limits(5),
    )
    .await
    .expect("five decoded id bytes fit exactly");

    assert_eq!(projection.id.as_deref(), Some("12345"));
    assert_eq!(projection.status.as_deref(), Some("queued"));
}

#[tokio::test]
async fn decoded_escape_cannot_bypass_the_selected_string_bound() {
    let observed = Arc::new(AtomicUsize::new(0));
    let error = project_json_body::<ControlProjection>(
        streamed(
            vec![br#"{"id":"1234\u0035","status":"queued"}"#.to_vec()],
            observed,
        ),
        limits(4),
    )
    .await
    .expect_err("the decoded fifth byte exceeds the selected-field bound");

    assert_eq!(error.code(), "json.project_limit");
}

#[tokio::test]
async fn known_parser_error_is_returned_while_the_provider_body_stalls() {
    let body: ByteStream = Box::pin(stream! {
        yield Ok(Bytes::from_static(b"not-json"));
        std::future::pending::<()>().await;
    });
    let result = tokio::time::timeout(
        std::time::Duration::from_secs(1),
        project_json_body::<ControlProjection>(body, limits(8)),
    )
    .await
    .expect("parser failure must not wait for the stalled provider body");
    assert_eq!(result.unwrap_err().code(), "json.project");
}
