use bytes::Bytes;
use futures_util::{StreamExt, stream};
use rsi_ai_transport::{
    ByteStream, DEFAULT_SSE_FRAME_BYTES, MAX_PROVIDER_SSE_FRAME_BYTES, SseTermination, decode_sse,
};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

static OVERLAPPING_ADMISSION_TEST_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

fn chunks(parts: &[&str]) -> ByteStream {
    let chunks = parts
        .iter()
        .map(|part| Ok(Bytes::copy_from_slice(part.as_bytes())))
        .collect::<Vec<_>>();
    Box::pin(stream::iter(chunks))
}

fn observed_chunk(part: &'static str, observed: Arc<AtomicBool>) -> ByteStream {
    Box::pin(stream::once(async move {
        observed.store(true, Ordering::SeqCst);
        Ok(Bytes::from_static(part.as_bytes()))
    }))
}

fn bounded_owned_chunks(part: &str) -> ByteStream {
    let chunks = part
        .as_bytes()
        .chunks(DEFAULT_SSE_FRAME_BYTES)
        .map(|chunk| Ok(Bytes::copy_from_slice(chunk)))
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
        DEFAULT_SSE_FRAME_BYTES,
    );

    assert_eq!(
        values.next().await.expect("event").expect("valid").as_str(),
        "{\"text\":\"你\"}\nsecond"
    );
    assert!(values.next().await.is_none());
}

#[tokio::test]
async fn done_terminated_sse_rejects_clean_eof_without_done() {
    let mut values = decode_sse(
        chunks(&["data: {\"ok\":true}\n\n"]),
        SseTermination::DoneSentinel,
        DEFAULT_SSE_FRAME_BYTES,
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
        DEFAULT_SSE_FRAME_BYTES,
    );
    assert_eq!(
        values.next().await.expect("event").expect("valid").as_str(),
        "{\"type\":\"response.completed\"}"
    );
    assert!(values.next().await.is_none());
}

#[tokio::test]
async fn eof_terminated_sse_preserves_a_literal_done_event() {
    let mut values = decode_sse(
        chunks(&["data: [DONE]\n\n"]),
        SseTermination::Eof,
        DEFAULT_SSE_FRAME_BYTES,
    );

    assert_eq!(
        values.next().await.expect("event").expect("valid").as_str(),
        "[DONE]"
    );
    assert!(values.next().await.is_none());
}

#[tokio::test]
async fn sse_rejects_a_frame_over_the_bound_before_allocating_unboundedly() {
    let oversized = format!("data: {}\n\n", "x".repeat(256 * 1024 + 1));
    let mut values = decode_sse(
        bounded_owned_chunks(&oversized),
        SseTermination::Eof,
        DEFAULT_SSE_FRAME_BYTES,
    );
    let error = values.next().await.expect("error").expect_err("oversized");
    assert_eq!(error.code(), "sse.frame_too_large");
}

#[tokio::test]
async fn sse_rejects_one_oversized_transport_item_before_yielding_from_it() {
    let item = "data: x\n\n".repeat(DEFAULT_SSE_FRAME_BYTES / "data: x\n\n".len() + 1);
    let body: ByteStream = Box::pin(stream::once(async move { Ok(Bytes::from(item)) }));
    let mut values = decode_sse(body, SseTermination::Eof, MAX_PROVIDER_SSE_FRAME_BYTES);

    let error = values
        .next()
        .await
        .expect("transport item error")
        .expect_err("one oversized transport item must not survive across decoder yields");
    assert_eq!(error.code(), "sse.transport_item_too_large");
    assert!(values.next().await.is_none());
}

#[tokio::test]
async fn sse_rejects_an_invalid_provider_frame_limit() {
    for invalid in [0, MAX_PROVIDER_SSE_FRAME_BYTES + 1] {
        let mut values = decode_sse(chunks(&[]), SseTermination::Eof, invalid);
        let error = values
            .next()
            .await
            .expect("error")
            .expect_err("invalid limit");
        assert_eq!(error.code(), "sse.invalid_frame_limit");
        assert!(values.next().await.is_none());
    }
}

#[tokio::test]
async fn sse_accepts_lone_cr_line_and_frame_terminators_across_chunks() {
    let mut values = decode_sse(
        chunks(&["data: first\rdata: sec", "ond\r", "\rdata: [DONE]\r\r"]),
        SseTermination::DoneSentinel,
        DEFAULT_SSE_FRAME_BYTES,
    );

    assert_eq!(
        values.next().await.expect("event").expect("valid").as_str(),
        "first\nsecond"
    );
    assert!(values.next().await.is_none());
}

#[tokio::test]
async fn empty_transport_item_preserves_pending_crlf_state() {
    let mut values = decode_sse(
        chunks(&["data: a\r", "", "\ndata: b\n\n"]),
        SseTermination::Eof,
        DEFAULT_SSE_FRAME_BYTES,
    );

    assert_eq!(
        values.next().await.expect("event").expect("valid").as_str(),
        "a\nb"
    );
    assert!(values.next().await.is_none());
}

#[tokio::test]
async fn overlapping_large_claims_do_not_serialize_small_frames() {
    let _exclusive = OVERLAPPING_ADMISSION_TEST_LOCK.lock().await;
    let overlapping_limit = MAX_PROVIDER_SSE_FRAME_BYTES / 2 + 1;
    let mut first = decode_sse(
        chunks(&["data: first\n\n"]),
        SseTermination::Eof,
        overlapping_limit,
    );
    let first_value = first.next().await.expect("first event").expect("valid");
    assert_eq!(first_value.as_str(), "first");

    let observed = Arc::new(AtomicBool::new(false));
    let second_observed = Arc::clone(&observed);
    let second = tokio::spawn(async move {
        let mut stream = decode_sse(
            observed_chunk("data: second\n\n", second_observed),
            SseTermination::Eof,
            overlapping_limit,
        );
        stream.next().await
    });
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        while !observed.load(Ordering::SeqCst) {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("declared maxima must not prevent the second stream from polling its body");
    let second_value = second
        .await
        .expect("second task")
        .expect("second event")
        .expect("valid");
    assert_eq!(second_value.as_str(), "second");
}

#[tokio::test]
async fn overlapping_claims_can_both_grow_past_the_initial_unit() {
    let _exclusive = OVERLAPPING_ADMISSION_TEST_LOCK.lock().await;
    let overlapping_limit = MAX_PROVIDER_SSE_FRAME_BYTES / 2 + 1;
    let payload = "x".repeat(DEFAULT_SSE_FRAME_BYTES + 1);
    let frame = format!("data: {payload}\n\n");
    let mut first = decode_sse(
        bounded_owned_chunks(&frame),
        SseTermination::Eof,
        overlapping_limit,
    );
    let first_value = first.next().await.expect("first event").expect("valid");
    assert_eq!(first_value.as_str().len(), payload.len());

    let second_frame = frame.clone();
    let second = tokio::spawn(async move {
        let mut stream = decode_sse(
            bounded_owned_chunks(&second_frame),
            SseTermination::Eof,
            overlapping_limit,
        );
        stream.next().await
    });
    let second_value = tokio::time::timeout(std::time::Duration::from_secs(2), second)
        .await
        .expect("second growing frame was serialized behind the first value")
        .expect("second task")
        .expect("second event")
        .expect("valid");
    assert_eq!(second_value.as_str().len(), payload.len());
}
