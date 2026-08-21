use bytes::Bytes;
use futures_util::{StreamExt, stream};
use rsi_ai_transport::{ByteStream, SseTermination, decode_sse};

fn chunks(parts: &[&str]) -> ByteStream {
    let chunks = parts
        .iter()
        .map(|part| Ok(Bytes::copy_from_slice(part.as_bytes())))
        .collect::<Vec<_>>();
    Box::pin(stream::iter(chunks))
}

#[tokio::test]
async fn sse_handles_fragmented_utf8_crlf_comments_and_multiline_data() {
    let mut values = decode_sse(
        chunks(&[
            ": keep-alive\r\nda",
            "ta: {\"text\":\"你\"}\r\ndata: second\r\n\r\ndata: [DO",
            "NE]\r\n\r\n",
        ]),
        SseTermination::DoneSentinel,
    );

    assert_eq!(
        values.next().await.expect("event").expect("valid"),
        "{\"text\":\"你\"}\nsecond"
    );
    assert!(values.next().await.is_none());
}

#[tokio::test]
async fn done_terminated_sse_rejects_clean_eof_without_done() {
    let mut values = decode_sse(
        chunks(&["data: {\"ok\":true}\n\n"]),
        SseTermination::DoneSentinel,
    );
    assert!(values.next().await.expect("data").is_ok());
    let error = values
        .next()
        .await
        .expect("error")
        .expect_err("missing done");
    assert_eq!(error.code(), "sse.missing_done");
    assert!(values.next().await.is_none());
}

#[tokio::test]
async fn eof_terminated_sse_accepts_a_final_complete_frame() {
    let mut values = decode_sse(
        chunks(&["event: response.completed\ndata: {\"type\":\"response.completed\"}\n\n"]),
        SseTermination::Eof,
    );
    assert_eq!(
        values.next().await.expect("event").expect("valid"),
        "{\"type\":\"response.completed\"}"
    );
    assert!(values.next().await.is_none());
}

#[tokio::test]
async fn eof_terminated_sse_preserves_a_literal_done_event() {
    let mut values = decode_sse(chunks(&["data: [DONE]\n\n"]), SseTermination::Eof);

    assert_eq!(
        values.next().await.expect("event").expect("valid"),
        "[DONE]"
    );
    assert!(values.next().await.is_none());
}

#[tokio::test]
async fn sse_rejects_a_frame_over_the_bound_before_allocating_unboundedly() {
    let oversized = format!("data: {}\n\n", "x".repeat(256 * 1024 + 1));
    let mut values = decode_sse(chunks(&[&oversized]), SseTermination::Eof);
    let error = values.next().await.expect("error").expect_err("oversized");
    assert_eq!(error.code(), "sse.frame_too_large");
}

#[tokio::test]
async fn sse_accepts_lone_cr_line_and_frame_terminators_across_chunks() {
    let mut values = decode_sse(
        chunks(&["data: first\rdata: sec", "ond\r", "\rdata: [DONE]\r\r"]),
        SseTermination::DoneSentinel,
    );

    assert_eq!(
        values.next().await.expect("event").expect("valid"),
        "first\nsecond"
    );
    assert!(values.next().await.is_none());
}
