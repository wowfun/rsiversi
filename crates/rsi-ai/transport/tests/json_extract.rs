use rsi_ai_transport::{BoundedJsonExtractor, JsonExtractEvent, JsonExtractionLimits};

fn limits(extracted: usize) -> JsonExtractionLimits {
    JsonExtractionLimits::new(4096, 1024, extracted).expect("limits")
}

#[test]
fn escaped_string_field_is_extracted_and_normalized_across_byte_boundaries() {
    let mut extractor = BoundedJsonExtractor::string("/choices/0/message/audio/data", limits(4))
        .expect("extractor");
    let input = br#"{"choices":[{"message":{"audio":{"data":"QU\u004aDRA=="}}}],"usage":{"n":1}}"#;
    let mut started = 0;
    let mut extracted = Vec::new();
    for byte in input {
        match extractor.push(*byte).expect("valid byte") {
            Some(JsonExtractEvent::TargetStarted) => started += 1,
            Some(JsonExtractEvent::StringChunk(bytes)) => extracted.extend(bytes),
            Some(JsonExtractEvent::ArrayItem(_)) | None => {}
        }
    }
    extractor.finish().expect("finish");

    assert_eq!(started, 1);
    assert_eq!(extracted, b"QUJDRA==");
    assert_eq!(
        extractor_normalized(input, "/choices/0/message/audio/data", limits(4)),
        serde_json::json!({
            "choices":[{"message":{"audio":{"data":""}}}],
            "usage":{"n":1}
        })
    );
}

#[test]
fn escaped_pointer_tokens_select_decoded_object_keys() {
    let input = br#"{"a/b":{"~key":[{"data":"QUJD"}]}}"#;
    assert_eq!(
        extractor_normalized(input, "/a~1b/~0key/0/data", limits(4)),
        serde_json::json!({"a/b":{"~key":[{"data":""}]}})
    );
}

#[test]
fn object_array_items_are_extracted_without_retaining_them_in_the_envelope() {
    let input = br#"{"created":1,"data":[{"b64_json":"e30=","meta":{"x":"}"}},{"b64_json":"W10="}],"usage":2}"#;
    let mut extractor =
        BoundedJsonExtractor::object_array("/data", limits(256)).expect("extractor");
    let mut items = Vec::new();
    for chunk in input.chunks(3) {
        for byte in chunk {
            if let Some(JsonExtractEvent::ArrayItem(item)) =
                extractor.push(*byte).expect("valid byte")
            {
                items.push(serde_json::from_slice::<serde_json::Value>(&item).expect("item"));
            }
        }
    }
    let finished = extractor.finish().expect("finish");

    assert_eq!(items.len(), 2);
    assert_eq!(items[0]["meta"]["x"], "}");
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&finished.envelope).expect("envelope"),
        serde_json::json!({"created":1,"data":[null,null],"usage":2})
    );
}

#[test]
fn slice_push_reports_exact_progress_without_buffering_later_items() {
    let input = br#"{"data":[{"value":1},{"value":2}]}"#;
    let mut extractor =
        BoundedJsonExtractor::object_array("/data", limits(256)).expect("extractor");
    let mut offset = 0;
    let mut items = Vec::new();
    while offset < input.len() {
        let progress = extractor.push_bytes(&input[offset..]).expect("valid slice");
        assert!(progress.consumed > 0);
        offset += progress.consumed;
        if let Some(JsonExtractEvent::ArrayItem(item)) = progress.event {
            items.push(serde_json::from_slice::<serde_json::Value>(&item).expect("item"));
        }
    }
    extractor.finish().expect("finish");

    assert_eq!(
        items,
        [
            serde_json::json!({"value":1}),
            serde_json::json!({"value":2})
        ]
    );
}

#[test]
fn object_array_never_emits_malformed_json_items() {
    for input in [
        br#"{"data":[{"x":[1,]}]}"#.as_slice(),
        br#"{"data":[{"x":"\q"}]}"#.as_slice(),
        br#"{"data":[{"x":[}]}"#.as_slice(),
    ] {
        let mut extractor =
            BoundedJsonExtractor::object_array("/data", limits(256)).expect("extractor");
        let mut emitted = false;
        let result = input.iter().try_for_each(|byte| {
            emitted |= matches!(extractor.push(*byte)?, Some(JsonExtractEvent::ArrayItem(_)));
            Ok(())
        });
        let result = result.and_then(|()| extractor.finish().map(|_| ()));

        assert!(result.is_err(), "malformed input was accepted: {input:?}");
        assert!(!emitted, "malformed item escaped the extractor: {input:?}");
    }
}

#[test]
fn final_array_index_pointer_selects_the_indexed_value() {
    let input = br#"{"values":["QUJD","ignored"]}"#;
    let mut extractor = BoundedJsonExtractor::string("/values/0", limits(8)).expect("extractor");
    let mut extracted = Vec::new();
    for byte in input {
        if let Some(JsonExtractEvent::StringChunk(chunk)) =
            extractor.push(*byte).expect("valid byte")
        {
            extracted.extend(chunk);
        }
    }
    let finished = extractor.finish().expect("finish");

    assert_eq!(extracted, b"QUJD");
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&finished.envelope).expect("envelope"),
        serde_json::json!({"values":["", "ignored"]})
    );
}

#[test]
fn extractor_rejects_missing_targets_wrong_types_and_bounds() {
    for (mut extractor, input) in [
        (
            BoundedJsonExtractor::string("/data", limits(8)).expect("extractor"),
            br#"{"other":"QQ=="}"#.as_slice(),
        ),
        (
            BoundedJsonExtractor::string("/data", limits(8)).expect("extractor"),
            br#"{"data":[]}"#.as_slice(),
        ),
        (
            BoundedJsonExtractor::object_array("/data", limits(4)).expect("extractor"),
            br#"{"data":[{"too":"large"}]}"#.as_slice(),
        ),
        (
            BoundedJsonExtractor::string("/data", limits(8)).expect("extractor"),
            br#"{"data":"QQ==","data":"QQ=="}"#.as_slice(),
        ),
    ] {
        let pushed = input
            .iter()
            .try_for_each(|byte| extractor.push(*byte).map(|_| ()));
        assert!(
            pushed
                .and_then(|()| extractor.finish().map(|_| ()))
                .is_err()
        );
    }

    for limits in [
        JsonExtractionLimits::new(12, 12, 8).expect("total bound"),
        JsonExtractionLimits::new(64, 8, 8).expect("envelope bound"),
    ] {
        let mut extractor = BoundedJsonExtractor::string("/data", limits).expect("extractor");
        assert!(
            br#"{"prefix":"long","data":"QQ=="}"#
                .iter()
                .try_for_each(|byte| extractor.push(*byte).map(|_| ()))
                .and_then(|()| extractor.finish().map(|_| ()))
                .is_err()
        );
    }

    assert!(JsonExtractionLimits::new(8, 4, 9).is_err());
}

#[test]
fn debug_output_never_exposes_provider_bytes() {
    const SECRET: &str = "provider-secret-material";

    let event = JsonExtractEvent::StringChunk(SECRET.as_bytes().to_vec());
    let extraction = rsi_ai_transport::JsonExtraction {
        envelope: SECRET.as_bytes().to_vec(),
    };
    let mut extractor = BoundedJsonExtractor::string("/data", limits(64)).expect("extractor");
    for byte in format!(r#"{{"data":"{SECRET}"}}"#).bytes().take(16) {
        let _ = extractor.push(byte);
    }

    for debug in [
        format!("{event:?}"),
        format!("{extraction:?}"),
        format!("{extractor:?}"),
    ] {
        assert!(
            !debug.contains(SECRET),
            "Debug exposed raw provider bytes: {debug}"
        );
        assert!(
            !debug.contains(&format!("{:?}", SECRET.as_bytes())),
            "Debug exposed raw provider bytes numerically: {debug}"
        );
    }
    assert!(format!("{event:?}").contains("byte_len"));
    assert!(format!("{extraction:?}").contains("envelope_bytes"));
}

#[test]
fn extracted_strings_reject_non_ascii_bytes_and_unicode_surrogates() {
    for input in [
        b"{\"data\":\"\xc3\xa9\"}".as_slice(),
        br#"{"data":"\uD800"}"#.as_slice(),
        br#"{"data":"\u00E9"}"#.as_slice(),
    ] {
        let mut extractor = BoundedJsonExtractor::string("/data", limits(8)).expect("extractor");
        let result = input
            .iter()
            .try_for_each(|byte| extractor.push(*byte).map(|_| ()))
            .and_then(|()| extractor.finish().map(|_| ()));
        assert!(result.is_err(), "non-ASCII target was accepted: {input:?}");
    }
}

fn extractor_normalized(
    input: &[u8],
    pointer: &str,
    limits: JsonExtractionLimits,
) -> serde_json::Value {
    let mut extractor = BoundedJsonExtractor::string(pointer, limits).expect("extractor");
    for byte in input {
        extractor.push(*byte).expect("valid byte");
    }
    let finished = extractor.finish().expect("finish");
    serde_json::from_slice(&finished.envelope).expect("normalized envelope")
}
