use rsi_ai_protocol::{
    ImageAssembler, ImageEvent, ImageRequest, MediaDescriptor, MediaKind, TokenUsage,
};
use serde_json::json;

fn usage() -> TokenUsage {
    TokenUsage {
        input_tokens: 4,
        output_tokens: 6,
        cache_read_tokens: None,
        cache_write_tokens: None,
        reasoning_tokens: None,
    }
}

fn image() -> MediaDescriptor {
    MediaDescriptor::new(MediaKind::Image, "image/png", 4, "a".repeat(64))
        .expect("image descriptor")
}

#[test]
fn image_mask_requires_an_edit_input() {
    let error = ImageRequest::new("edit the image", 1)
        .expect("base request")
        .with_inputs(Vec::new(), Some(image()))
        .expect_err("a mask without an input must not become generation");
    assert_eq!(error.code(), "request.invalid_media");
}

#[test]
fn media_mime_requires_a_nonempty_subtype() {
    let image = MediaDescriptor::new(MediaKind::Image, "image/", 1, "a".repeat(64))
        .expect_err("an image MIME type needs a subtype");
    assert!(image.to_string().contains("MIME"));

    let nested = MediaDescriptor::new(MediaKind::Image, "image/a/b", 1, "a".repeat(64))
        .expect_err("a MIME type has exactly one slash");
    assert!(nested.to_string().contains("MIME"));
}

#[test]
fn deserialized_image_metadata_cannot_claim_audio_duration() {
    let mut invalid_image = serde_json::to_value(image()).expect("image JSON");
    invalid_image["duration_ms"] = json!(100);
    serde_json::from_value::<MediaDescriptor>(invalid_image)
        .expect_err("image duration must fail at the typed boundary");

    let mut half_dimensions = serde_json::to_value(image()).expect("image JSON");
    half_dimensions["width"] = json!(10);
    half_dimensions["height"] = json!(null);
    serde_json::from_value::<MediaDescriptor>(half_dimensions)
        .expect_err("half-specified image dimensions must fail at the typed boundary");
}

#[test]
fn image_stream_collects_raw_chunks_into_verified_descriptors() {
    let request = ImageRequest::new("draw one dot", 1).expect("request");
    assert_eq!(request.count(), 1);

    let mut assembler = ImageAssembler::new();
    for event in [
        ImageEvent::OutputStarted {
            index: 0,
            mime_type: "image/png".to_owned(),
        },
        ImageEvent::OutputChunk {
            index: 0,
            sequence: 1,
            bytes: vec![137, 80],
        },
        ImageEvent::OutputChunk {
            index: 0,
            sequence: 2,
            bytes: vec![78, 71],
        },
        ImageEvent::OutputFinished { index: 0 },
        ImageEvent::Usage { usage: usage() },
        ImageEvent::Finished,
    ] {
        assembler.push(&event).expect("image event");
    }
    let output = assembler.finish().expect("image output");
    assert_eq!(output.images.len(), 1);
    assert_eq!(output.images[0].bytes, vec![137, 80, 78, 71]);
    assert_eq!(output.images[0].descriptor.kind(), MediaKind::Image);
    assert_eq!(output.images[0].descriptor.byte_len(), 4);
    assert_eq!(output.usage, Some(usage()));
}

#[test]
fn image_stream_rejects_each_terminal_and_ordering_violation() {
    let mut noncontiguous = ImageAssembler::new();
    assert_eq!(
        noncontiguous
            .push(&ImageEvent::OutputStarted {
                index: 1,
                mime_type: "image/png".into(),
            })
            .unwrap_err()
            .code(),
        "stream.non_contiguous_index"
    );

    let mut sequence = ImageAssembler::new();
    sequence
        .push(&ImageEvent::OutputStarted {
            index: 0,
            mime_type: "image/png".into(),
        })
        .unwrap();
    assert_eq!(
        sequence
            .push(&ImageEvent::OutputChunk {
                index: 0,
                sequence: 2,
                bytes: vec![1],
            })
            .unwrap_err()
            .code(),
        "stream.chunk_sequence"
    );
    assert_eq!(
        sequence.push(&ImageEvent::Finished).unwrap_err().code(),
        "stream.output_still_open"
    );

    let mut terminal = ImageAssembler::new();
    for event in [
        ImageEvent::OutputStarted {
            index: 0,
            mime_type: "image/png".into(),
        },
        ImageEvent::OutputChunk {
            index: 0,
            sequence: 1,
            bytes: vec![1],
        },
        ImageEvent::OutputFinished { index: 0 },
        ImageEvent::Finished,
    ] {
        terminal.push(&event).unwrap();
    }
    assert_eq!(
        terminal
            .push(&ImageEvent::Usage { usage: usage() })
            .unwrap_err()
            .code(),
        "stream.already_finished"
    );

    assert_eq!(
        ImageAssembler::new().finish().unwrap_err().code(),
        "stream.missing_finish"
    );
}
