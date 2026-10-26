use rsi_media_protocol::{
    MAXIMUM_IMAGE_DESCRIPTOR_BYTES, MAXIMUM_IMAGE_DIMENSION, MediaId, MediaRef,
};

#[test]
fn valid_descriptors_and_references_round_trip_at_inclusive_bounds() {
    use rsi_media_protocol::{MAXIMUM_AUDIO_DESCRIPTOR_BYTES, MediaDescriptor, MediaKind};
    for bytes in [1, MAXIMUM_IMAGE_DESCRIPTOR_BYTES] {
        let descriptor = MediaDescriptor::new(MediaKind::Image, "image/png", bytes, "a".repeat(64))
            .unwrap()
            .with_image_dimensions(MAXIMUM_IMAGE_DIMENSION, 1)
            .unwrap();
        assert_eq!(
            serde_json::from_slice::<MediaDescriptor>(&serde_json::to_vec(&descriptor).unwrap())
                .unwrap(),
            descriptor
        );
        let reference: MediaRef = serde_json::from_value(serde_json::json!({"id":"a".repeat(64),"mime":"image/png","bytes":bytes,"width":MAXIMUM_IMAGE_DIMENSION,"height":1})).unwrap();
        assert_eq!(
            serde_json::from_slice::<MediaRef>(&serde_json::to_vec(&reference).unwrap()).unwrap(),
            reference
        );
    }
    let audio = MediaDescriptor::new(
        MediaKind::Audio,
        "audio/ogg",
        MAXIMUM_AUDIO_DESCRIPTOR_BYTES,
        "b".repeat(64),
    )
    .unwrap()
    .with_audio_duration_ms(u64::MAX)
    .unwrap();
    assert_eq!(
        serde_json::from_slice::<MediaDescriptor>(&serde_json::to_vec(&audio).unwrap()).unwrap(),
        audio
    );
    assert!(MediaDescriptor::new(MediaKind::Image, "image/图像", 1, "a".repeat(64)).is_err());
}

#[test]
fn durable_media_descriptors_require_canonical_lowercase_mime_types() {
    let descriptor = format!(
        r#"{{"kind":"image","mime_type":"image/PNG","byte_len":1,"sha256":"{}","width":null,"height":null,"duration_ms":null}}"#,
        "a".repeat(64)
    );

    assert!(serde_json::from_str::<rsi_media_protocol::MediaDescriptor>(&descriptor).is_err());
}

#[test]
fn durable_media_identity_and_reference_revalidate_constructor_invariants() {
    assert!(serde_json::from_str::<MediaId>(r#""short""#).is_err());
    assert!(serde_json::from_str::<MediaId>(&format!(r#""{}""#, "A".repeat(64))).is_err());

    let invalid_reference = format!(
        r#"{{"id":"{}","mime":"image/jpeg","bytes":0,"width":0,"height":0}}"#,
        "a".repeat(64)
    );
    assert!(serde_json::from_str::<MediaRef>(&invalid_reference).is_err());
}

#[test]
fn durable_media_reference_uses_the_same_image_body_bound_as_descriptors() {
    let oversized = format!(
        r#"{{"id":"{}","mime":"image/png","bytes":{},"width":1,"height":1}}"#,
        "a".repeat(64),
        MAXIMUM_IMAGE_DESCRIPTOR_BYTES + 1
    );
    assert!(serde_json::from_str::<MediaRef>(&oversized).is_err());
}

#[test]
fn durable_media_reference_enforces_each_dimension_bound() {
    let oversized = format!(
        r#"{{"id":"{}","mime":"image/png","bytes":1,"width":{},"height":1}}"#,
        "a".repeat(64),
        MAXIMUM_IMAGE_DIMENSION + 1
    );
    assert!(serde_json::from_str::<MediaRef>(&oversized).is_err());
}
