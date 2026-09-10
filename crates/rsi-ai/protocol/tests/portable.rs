use rsi_ai_protocol::portable::{self, Decoder, Kind};
use rsi_api_protocol::ByteBudget;

#[test]
fn fragmented_packets_reserve_before_allocation_and_retain_through_last_clone() {
    let payload = vec![b'x'; portable::MAXIMUM_FRAGMENT_BYTES + 7];
    let budget = ByteBudget::new(payload.len()).unwrap();
    let mut decoder = Decoder::new(budget.clone());
    let mut frames = portable::frames(Kind::Json, &payload).unwrap();
    assert!(decoder.push(&frames.next().unwrap()).unwrap().is_none());
    assert_eq!(budget.used(), payload.len());
    let packet = decoder.push(&frames.next().unwrap()).unwrap().unwrap();
    assert_eq!(packet.kind, Kind::Json);
    assert_eq!(packet.bytes.as_bytes(), payload);
    assert!(frames.next().is_none());
    let retained = packet.clone();
    drop(packet);
    drop(decoder);
    assert_eq!(budget.used(), payload.len());
    drop(retained);
    assert_eq!(budget.used(), 0);
}

#[test]
fn bad_fragments_close_the_decoder_and_release_partial_storage() {
    let payload = vec![0; portable::MAXIMUM_FRAGMENT_BYTES + 1];
    let valid: Vec<_> = portable::frames(Kind::Binary, &payload).unwrap().collect();
    for mutation in 0..5 {
        let budget = ByteBudget::new(payload.len()).unwrap();
        let mut decoder = Decoder::new(budget.clone());
        assert!(decoder.push(&valid[0]).unwrap().is_none());
        let mut bad = valid[1].clone();
        match mutation {
            0 => bad[0] = 0,
            1 => bad[1..5].copy_from_slice(&1u32.to_le_bytes()),
            2 => bad[5..9].copy_from_slice(&0u32.to_le_bytes()),
            3 => bad.truncate(9),
            _ => bad.push(0),
        }
        assert!(decoder.push(&bad).is_err());
        assert_eq!(budget.used(), 0);
        assert!(decoder.push(&valid[0]).is_err());
    }
}

#[test]
fn decoder_rejects_oversize_and_capacity_before_receiving_a_body() {
    let budget = ByteBudget::new(0).unwrap();
    let mut decoder = Decoder::new(budget.clone());
    let frame = portable::frames(Kind::Json, b"{}").unwrap().next().unwrap();
    assert!(decoder.push(&frame).is_err());
    assert_eq!(budget.used(), 0);
    let budget = ByteBudget::default();
    let mut metadata = Decoder::with_control_limit(budget.clone(), 1).unwrap();
    assert!(metadata.push(&frame).is_err());
    assert_eq!(budget.used(), 0);
    assert!(Decoder::with_control_limit(budget, portable::MAXIMUM_CONTROL_BYTES + 1).is_err());
    assert!(portable::frames(Kind::Binary, &vec![0; portable::MAXIMUM_BINARY_BYTES + 1]).is_err());
    for frame in [
        vec![],
        vec![2; 9],
        vec![0; portable::MAXIMUM_FRAGMENT_BYTES + 10],
    ] {
        assert!(Decoder::new(ByteBudget::default()).push(&frame).is_err());
    }
    let mut frame = vec![0; 9];
    frame[1..5].copy_from_slice(&u32::MAX.to_le_bytes());
    assert!(Decoder::new(ByteBudget::default()).push(&frame).is_err());
}

#[test]
fn complete_empty_packet_and_incomplete_eof_are_distinct() {
    let budget = ByteBudget::default();
    let mut decoder = Decoder::new(budget.clone());
    let frame = portable::frames(Kind::Binary, &[]).unwrap().next().unwrap();
    let packet = decoder.push(&frame).unwrap().unwrap();
    assert!(packet.bytes.is_empty());
    assert!(decoder.finish().is_ok());
    let frames: Vec<_> =
        portable::frames(Kind::Json, &vec![0; portable::MAXIMUM_FRAGMENT_BYTES + 1])
            .unwrap()
            .collect();
    let mut decoder = Decoder::new(budget.clone());
    decoder.push(&frames[0]).unwrap();
    assert!(decoder.finish().is_err());
    assert_eq!(budget.used(), 0);
}

#[test]
fn packet_iterator_partial_decoder_and_completed_packet_redact_binary_bytes() {
    let secret = "credential-value".repeat(5_000);
    let mut frames = portable::frames(Kind::Binary, secret.as_bytes()).unwrap();
    assert!(!format!("{frames:?}").contains("credential-value"));
    let mut decoder = Decoder::new(ByteBudget::default());
    assert!(decoder.push(&frames.next().unwrap()).unwrap().is_none());
    assert!(!format!("{decoder:?}").contains("credential-value"));
    let packet = decoder.push(&frames.next().unwrap()).unwrap().unwrap();
    assert_eq!(packet.bytes.as_bytes(), secret.as_bytes());
    assert!(!format!("{packet:?}").contains("credential-value"));
}

#[test]
fn control_rejects_unknown_duplicate_and_ignored_nested_fields() {
    use portable::{ControlRequest, ControlResponse, decode_control};
    assert!(decode_control::<ControlRequest>(br#"{"op":"describe"}"#).is_ok());
    for bad in [
        br#"{"op":"describe","extra":1}"#.as_slice(),
        br#"{"op":"describe","op":"describe"}"#.as_slice(),
    ] {
        assert!(decode_control::<ControlRequest>(bad).is_err());
    }
    let valid = serde_json::to_value(ControlResponse::Language {
        event: Box::new(rsi_ai_protocol::LanguageEvent::ContentStarted {
            index: 0,
            content: rsi_ai_protocol::ContentStart::Text,
        }),
    })
    .unwrap();
    assert!(decode_control::<ControlResponse>(&serde_json::to_vec(&valid).unwrap()).is_ok());
    let mut bad = valid;
    bad["event"]["content"]["secret"] = serde_json::json!("credential-marker");
    assert!(decode_control::<ControlResponse>(&serde_json::to_vec(&bad).unwrap()).is_err());
    assert!(
        portable::validate_prepared_state(&serde_json::json!(
            "x".repeat(portable::MAXIMUM_PREPARED_STATE_BYTES)
        ))
        .is_err()
    );
    assert!(portable::validate_prepared_state(&serde_json::json!({"cursor":1})).is_ok());
}

#[test]
fn descriptions_reject_duplicate_models_features_and_unsupported_image_counts() {
    use portable::{Description, ImageFeature, ImageModel};
    let mut description = Description {
        language: vec![],
        image: vec![ImageModel {
            model: "image".into(),
            maximum_count: 1,
            features: vec![],
        }],
    };
    description.validate().unwrap();
    description.image.push(description.image[0].clone());
    assert!(description.validate().is_err());
    description.image.pop();
    description.image[0].maximum_count = 0;
    assert!(description.validate().is_err());
    description.image[0].maximum_count = 1;
    description.image[0].features = vec![ImageFeature::Mask];
    assert!(description.validate().is_err());
    description.image[0].features = vec![ImageFeature::Inputs, ImageFeature::Inputs];
    assert!(description.validate().is_err());
}
