use rsi_api_client::{FiniteDecoder, FiniteEncoding, SseDecoder, SseEvent, decode_error};
use rsi_api_protocol::{ApiError, ByteBudget};

fn binary(json: &[u8], data: &[u8]) -> Vec<u8> {
    let mut bytes = Vec::new();
    bytes.extend_from_slice(&(json.len() as u64).to_be_bytes());
    bytes.extend_from_slice(&(data.len() as u64).to_be_bytes());
    bytes.extend_from_slice(json);
    bytes.extend_from_slice(data);
    bytes
}

#[test]
fn finite_every_split_preserves_exact_json_and_shared_binary_ownership() {
    let json = br#"{"large":18446744073709551615,"fraction":0.12345678901234567890123456789}"#;
    let data = [0, 1, 128, 255];
    let wire = binary(json, &data);
    for split in 0..=wire.len() {
        let budget = ByteBudget::new(1024).unwrap();
        let mut decoder = FiniteDecoder::new(
            FiniteEncoding::Binary,
            budget.reserve(1024).unwrap(),
            Some(wire.len()),
        )
        .unwrap();
        decoder.push(&wire[..split]).unwrap();
        decoder.push(&wire[split..]).unwrap();
        let reply = decoder.finish().unwrap();
        assert_eq!(reply.json.as_bytes(), json);
        assert_eq!(reply.binary.as_ref().unwrap().as_bytes(), data);
        assert_eq!(budget.used(), json.len() + data.len());
        let escaped = reply.binary.as_ref().unwrap().slice(1..2).unwrap();
        drop(reply);
        assert_eq!(budget.used(), json.len() + data.len());
        drop(escaped);
        assert_eq!(budget.used(), 0);
    }
}

#[test]
fn finite_unknown_json_length_keeps_reserved_capacity_and_rejects_truncation_or_suffix() {
    let budget = ByteBudget::new(128).unwrap();
    let mut decoder =
        FiniteDecoder::new(FiniteEncoding::Json, budget.reserve(128).unwrap(), None).unwrap();
    for byte in b"true" {
        decoder.push(&[*byte]).unwrap();
    }
    let reply = decoder.finish().unwrap();
    assert_eq!(budget.used(), 128);
    drop(reply);
    for input in [b"".as_slice(), b"{", b"true false", b"\xff", b"[1,]", b"01"] {
        let mut decoder =
            FiniteDecoder::new(FiniteEncoding::Json, budget.reserve(128).unwrap(), None).unwrap();
        decoder.push(input).unwrap();
        assert!(decoder.finish().is_err(), "accepted {input:?}");
        assert_eq!(budget.used(), 0);
    }
    let mut decoder =
        FiniteDecoder::new(FiniteEncoding::Json, budget.reserve(128).unwrap(), Some(5)).unwrap();
    decoder.push(b"true").unwrap();
    assert!(decoder.finish().is_err());
    let mut decoder =
        FiniteDecoder::new(FiniteEncoding::Json, budget.reserve(128).unwrap(), Some(4)).unwrap();
    assert!(decoder.push(b"true ").is_err());
    assert_eq!(budget.used(), 0);
    assert!(decoder.push(b"true").is_err());
    assert!(decoder.finish().is_err());
}

#[test]
fn finite_retention_is_independent_and_keeps_binary_slices_charged() {
    let receiving = ByteBudget::new(128).unwrap();
    let retained = ByteBudget::new(16).unwrap();
    let wire = binary(b"{}", b"image");
    let mut decoder = FiniteDecoder::new(
        FiniteEncoding::Binary,
        receiving.reserve(128).unwrap(),
        None,
    )
    .unwrap();
    decoder.push(&wire).unwrap();
    let first = decoder.finish_into(&retained).unwrap();
    let mut decoder =
        FiniteDecoder::new(FiniteEncoding::Json, receiving.reserve(128).unwrap(), None).unwrap();
    decoder.push(b"true").unwrap();
    let second = decoder.finish_into(&retained).unwrap();
    assert_eq!(receiving.used(), 0);
    assert_eq!(retained.used(), 11);
    assert_eq!(second.json.as_bytes(), b"true");
    let slice = first.binary.unwrap().slice(1..2).unwrap();
    drop(first.json);
    drop(second);
    assert_eq!(retained.used(), 7);
    drop(slice);
    assert_eq!(retained.used(), 0);
}

#[test]
fn exhausted_completed_retention_reports_unknown_for_a_delivered_mutation() {
    use futures_util::FutureExt as _;
    for (status, body) in [(200, b"true".as_slice()), (422, b"{}".as_slice())] {
        for mutation in [false, true] {
            let receiving = ByteBudget::new(128).unwrap();
            let retained = ByteBudget::new(1).unwrap();
            let previous = retained.copy(b"1").unwrap();
            let mut headers = http::HeaderMap::new();
            headers.insert(
                http::header::CONTENT_TYPE,
                if status == 422 {
                    "application/vnd.rsi.domain-error+json"
                } else {
                    "application/json"
                }
                .parse()
                .unwrap(),
            );
            let source = Box::pin(futures_util::stream::iter([Ok(bytes::Bytes::from_static(
                body,
            ))]));
            let result = rsi_api_client::decode_response(
                status,
                &headers,
                source,
                Some(receiving.reserve(128).unwrap()),
                mutation,
                &retained,
            )
            .now_or_never()
            .expect("in-memory body completes immediately");
            assert_eq!(
                result.unwrap_err(),
                if mutation {
                    ApiError::OutcomeUnknown
                } else {
                    ApiError::Capacity
                }
            );
            assert_eq!(receiving.used(), 0);
            assert_eq!(retained.used(), 1);
            drop(previous);
            assert_eq!(retained.used(), 0);
        }
    }
}

#[test]
fn binary_rejects_every_truncated_prefix_length_overflow_and_disagreement() {
    let budget = ByteBudget::new(64).unwrap();
    let wire = binary(b"{}", b"123");
    for end in 0..wire.len() {
        let mut decoder =
            FiniteDecoder::new(FiniteEncoding::Binary, budget.reserve(64).unwrap(), None).unwrap();
        decoder.push(&wire[..end]).unwrap();
        assert!(decoder.finish().is_err(), "accepted truncation at {end}");
        assert_eq!(budget.used(), 0);
    }
    for prefix in [(u64::MAX, 1), (1, u64::MAX), (63, 2), (0, 0)] {
        let mut wire = prefix.0.to_be_bytes().to_vec();
        wire.extend_from_slice(&prefix.1.to_be_bytes());
        let mut decoder =
            FiniteDecoder::new(FiniteEncoding::Binary, budget.reserve(64).unwrap(), None).unwrap();
        let failed = decoder.push(&wire).is_err();
        assert!(failed || decoder.finish().is_err());
        assert_eq!(budget.used(), 0);
    }
    for length in [0, 15, wire.len() - 1, wire.len() + 1, 100] {
        let result = FiniteDecoder::new(
            FiniteEncoding::Binary,
            budget.reserve(64).unwrap(),
            Some(length),
        )
        .and_then(|mut decoder| {
            decoder.push(&wire)?;
            decoder.finish()
        });
        assert!(result.is_err());
        assert_eq!(budget.used(), 0);
    }
    let mut decoder =
        FiniteDecoder::new(FiniteEncoding::Binary, budget.reserve(64).unwrap(), None).unwrap();
    decoder.push(&wire).unwrap();
    assert!(decoder.push(b"extra").is_err());
    assert_eq!(budget.used(), 0);
}

fn feed(
    decoder: &mut SseDecoder,
    mut bytes: &[u8],
    events: &mut Vec<SseEvent>,
) -> rsi_api_protocol::Result<()> {
    while !bytes.is_empty() {
        let (consumed, event) = decoder.push(bytes)?;
        assert_ne!(consumed, 0);
        bytes = &bytes[consumed..];
        if let Some(event) = event {
            events.push(event);
        }
    }
    Ok(())
}

#[test]
fn interleaved_small_sse_frames_do_not_reserve_both_operation_maxima() {
    let budget = ByteBudget::new(64).unwrap();
    let receiving = ByteBudget::new(64).unwrap();
    let mut first = SseDecoder::new(receiving.clone(), budget.clone(), 40).unwrap();
    let mut second = SseDecoder::new(receiving.clone(), budget.clone(), 40).unwrap();
    let mut events = Vec::new();
    feed(&mut first, b"event: item\ndata: tr", &mut events).unwrap();
    feed(&mut second, b"event: item\ndata: true\n\n", &mut events).unwrap();
    feed(&mut first, b"ue\n\n", &mut events).unwrap();
    assert_eq!(events.len(), 2);
    assert_eq!(receiving.used(), 0);
    assert_eq!(budget.used(), 8);
    drop(events);
    assert_eq!(budget.used(), 0);
}

#[test]
fn small_sse_items_do_not_each_retain_the_maximum_fact_allocation() {
    let budget = ByteBudget::default();
    let mut decoder = SseDecoder::new(
        ByteBudget::default(),
        budget.clone(),
        36 * 1024 * 1024 + 64 * 1024,
    )
    .unwrap();
    let mut events = Vec::new();
    feed(&mut decoder, b"event: item\ndata: 1\n\n", &mut events).unwrap();
    feed(&mut decoder, b"event: item\ndata: 2\n\n", &mut events).unwrap();
    assert_eq!(events.len(), 2);
    assert_eq!(budget.used(), 2);
    let SseEvent::Item(item) = events.pop().unwrap() else {
        panic!("item")
    };
    let retained = item.json.slice(..).unwrap();
    drop(item);
    drop(events);
    assert_eq!(budget.used(), 1);
    drop(retained);
    assert_eq!(budget.used(), 0);
}

#[test]
fn opening_comment_is_bounded_not_an_event_and_only_legal_before_events() {
    let wire = b": ready\n\nevent: item\ndata: 1\n\nevent: end\ndata: {}\n\n";
    for split in 0..=wire.len() {
        let budget = ByteBudget::new(1).unwrap();
        let mut decoder = SseDecoder::new(ByteBudget::default(), budget.clone(), 1).unwrap();
        let mut events = Vec::new();
        feed(&mut decoder, &wire[..split], &mut events).unwrap();
        feed(&mut decoder, &wire[split..], &mut events).unwrap();
        decoder.finish().unwrap();
        assert_eq!(events.len(), 2);
        assert_eq!(budget.used(), 1);
    }
    for bad in [
        b": ready\n\n: ready\n\n".as_slice(),
        b": other\n\n",
        b": ready\nx",
        b"event: item\ndata: 1\n\n: ready\n\n",
        b"event: end\ndata: {}\n\n: ready\n\n",
    ] {
        let mut decoder =
            SseDecoder::new(ByteBudget::default(), ByteBudget::default(), 128).unwrap();
        assert!(feed(&mut decoder, bad, &mut Vec::new()).is_err());
    }
    for length in 0..=9 {
        let mut decoder =
            SseDecoder::new(ByteBudget::default(), ByteBudget::new(0).unwrap(), 0).unwrap();
        feed(&mut decoder, &b": ready\n\n"[..length], &mut Vec::new()).unwrap();
        assert!(
            decoder.finish().is_err(),
            "opening alone is never clean end"
        );
    }
}

#[test]
fn sse_every_split_and_single_bytes_preserve_multiple_frames_and_unicode() {
    let wire = "event: item\ndata: 18446744073709551615\n\nevent: item\ndata: {\"文\":\"🙂\\n\"}\n\nevent: end\ndata: {}\n\n";
    for split in 0..=wire.len() {
        let budget = ByteBudget::new(256).unwrap();
        let mut decoder = SseDecoder::new(ByteBudget::default(), budget.clone(), 128).unwrap();
        let mut events = Vec::new();
        feed(&mut decoder, &wire.as_bytes()[..split], &mut events).unwrap();
        feed(&mut decoder, &wire.as_bytes()[split..], &mut events).unwrap();
        decoder.finish().unwrap();
        assert_eq!(events.len(), 3);
        let SseEvent::Item(first) = &events[0] else {
            panic!("item")
        };
        assert_eq!(first.json.as_bytes(), b"18446744073709551615");
        let SseEvent::Item(second) = &events[1] else {
            panic!("item")
        };
        assert_eq!(second.json.as_bytes(), "{\"文\":\"🙂\\n\"}".as_bytes());
        assert!(matches!(&events[2], SseEvent::End(None)));
        assert_eq!(budget.used(), first.json.len() + second.json.len());
        drop(events);
        assert_eq!(budget.used(), 0);
    }
    let mut decoder = SseDecoder::new(ByteBudget::default(), ByteBudget::default(), 128).unwrap();
    let mut events = Vec::new();
    for byte in wire.as_bytes() {
        feed(&mut decoder, &[*byte], &mut events).unwrap();
    }
    decoder.finish().unwrap();
    assert_eq!(events.len(), 3);
}

#[test]
fn sse_requires_end_and_rejects_every_truncation_and_trailing_frame() {
    let wire = b"event: item\ndata: true\n\nevent: end\ndata: {}\n\n";
    for end in 0..wire.len() {
        let budget = ByteBudget::new(64).unwrap();
        let mut decoder = SseDecoder::new(ByteBudget::default(), budget.clone(), 64).unwrap();
        let mut events = Vec::new();
        feed(&mut decoder, &wire[..end], &mut events).unwrap();
        assert!(decoder.finish().is_err(), "accepted truncation at {end}");
        drop(events);
        assert_eq!(budget.used(), 0);
    }
    for suffix in [
        b"x".as_slice(),
        b"\n",
        b"event: end\ndata: {}\n\n",
        b"event: item\ndata: 1\n\n",
    ] {
        let mut decoder =
            SseDecoder::new(ByteBudget::default(), ByteBudget::default(), 64).unwrap();
        feed(&mut decoder, wire, &mut Vec::new()).unwrap();
        assert!(decoder.push(suffix).is_err());
        assert!(decoder.finish().is_err());
    }
}

#[test]
fn sse_closed_frames_release_partial_reservations_on_failure() {
    for wire in [
        "event: unknown\ndata: {}\n\n",
        "event: item\nx: {}\n\n",
        "event: item\ndata: {\n\n",
        "event: item\ndata: 123456789\n\n",
        "event: item\ndata: true\nx",
        "event: end\ndata: []\n\n",
        "event: error\ndata: {\"code\":\"unknown\"}\n\n",
        "event: error\ndata: {\"code\":\"capacity\",\"extra\":1}\n\n",
        "event: error\ndata: {\"code\":\"capacity\"}\n\nevent: item\ndata: 1\n\n",
        "event: domain-error\ndata: {}\n\nevent: error\ndata: {\"code\":\"capacity\"}\n\n",
    ] {
        let budget = ByteBudget::new(8).unwrap();
        let mut decoder = SseDecoder::new(ByteBudget::default(), budget.clone(), 8).unwrap();
        assert!(
            feed(&mut decoder, wire.as_bytes(), &mut Vec::new()).is_err(),
            "accepted {wire}"
        );
        assert_eq!(budget.used(), 0);
        assert!(decoder.push(b"event: end\ndata: {}\n\n").is_err());
        assert!(decoder.finish().is_err());
    }
}

#[test]
fn sse_error_requires_explicit_end_and_last_item_clones_can_exhaust_admission() {
    let budget = ByteBudget::new(64).unwrap();
    let mut decoder = SseDecoder::new(ByteBudget::default(), budget.clone(), 64).unwrap();
    let mut events = Vec::new();
    feed(
        &mut decoder,
        b"event: domain-error\ndata: {\"code\":\"domain\"}\n\n",
        &mut events,
    )
    .unwrap();
    assert!(events.is_empty());
    assert_eq!(budget.used(), br#"{"code":"domain"}"#.len());
    feed(&mut decoder, b"event: end\ndata: {}\n\n", &mut events).unwrap();
    assert!(matches!(
        &events[0],
        SseEvent::End(Some(ApiError::Domain(_)))
    ));
    decoder.finish().unwrap();
    drop(events);
    assert_eq!(budget.used(), 0);
    let budget = ByteBudget::new(4).unwrap();
    let mut decoder = SseDecoder::new(ByteBudget::default(), budget.clone(), 64).unwrap();
    let (_, Some(SseEvent::Item(item))) = decoder.push(b"event: item\ndata: true\n\n").unwrap()
    else {
        panic!("item")
    };
    let held = item.json.clone();
    drop(item);
    assert!(matches!(
        decoder.push(b"event: item\ndata: true\n\n"),
        Err(ApiError::Capacity)
    ));
    assert_eq!(budget.used(), b"true".len());
    drop(held);
    assert_eq!(budget.used(), 0);
    assert!(decoder.finish().is_err());
}

#[test]
fn common_error_codes_are_closed_and_never_accept_remote_diagnostics() {
    assert_eq!(
        decode_error(br#"{"code":"outcome_unknown"}"#).unwrap(),
        ApiError::OutcomeUnknown
    );
    assert_eq!(
        decode_error(br#"{"code":"generation_retired"}"#).unwrap(),
        ApiError::ShuttingDown
    );
    for bad in [
        br#"{"code":"capacity","message":"secret"}"#.as_slice(),
        br#"{"code":"capacity","code":"capacity"}"#,
        br#"{"code":"new_code"}"#,
        b"null",
    ] {
        assert!(decode_error(bad).is_err());
    }
}
