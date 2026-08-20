use rsi_ai_protocol::{
    ImageAssembler, ImageEvent, ImageRequest, MAX_REQUEST_BYTES, MediaDescriptor, MediaKind,
    SpeechAssembler, SpeechEvent, SpeechFormat, SpeechRequest, TokenUsage, TranscriptionAssembler,
    TranscriptionEvent, TranscriptionRequest, TranscriptionSegment,
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

#[test]
fn transcription_stream_bounds_text_and_segments_together() {
    let mut assembler = TranscriptionAssembler::new();
    assembler
        .push(&TranscriptionEvent::TextDelta {
            text: "x".to_owned(),
        })
        .expect("small transcript delta");
    let error = assembler
        .push(&TranscriptionEvent::Segment {
            segment: TranscriptionSegment {
                id: 0,
                start_ms: 0,
                end_ms: 1,
                text: "a".repeat(MAX_REQUEST_BYTES),
            },
        })
        .expect_err("aggregate transcription output is bounded");
    assert_eq!(error.code(), "stream.output_too_large");
}

fn audio() -> MediaDescriptor {
    MediaDescriptor::new(
        MediaKind::Audio,
        "audio/wav",
        11,
        "b94d27b9934d3e08a52e52d7da7dabfac484efe37a5380ee9088f7ace2efcde9",
    )
    .expect("audio descriptor")
}

fn image() -> MediaDescriptor {
    MediaDescriptor::new(MediaKind::Image, "image/png", 4, "a".repeat(64))
        .expect("image descriptor")
}

#[test]
fn deserialized_media_metadata_cannot_cross_kind_boundaries() {
    let mut invalid_audio = serde_json::to_value(audio()).expect("audio JSON");
    invalid_audio["width"] = json!(10);
    invalid_audio["height"] = json!(10);
    let invalid_audio: MediaDescriptor =
        serde_json::from_value(invalid_audio).expect("shape decodes");
    assert!(invalid_audio.validate().is_err());

    let mut invalid_image = serde_json::to_value(image()).expect("image JSON");
    invalid_image["duration_ms"] = json!(100);
    let invalid_image: MediaDescriptor =
        serde_json::from_value(invalid_image).expect("shape decodes");
    assert!(invalid_image.validate().is_err());

    let mut half_dimensions = serde_json::to_value(image()).expect("image JSON");
    half_dimensions["width"] = json!(10);
    half_dimensions["height"] = json!(null);
    let half_dimensions: MediaDescriptor =
        serde_json::from_value(half_dimensions).expect("shape decodes");
    assert!(half_dimensions.validate().is_err());
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
fn transcription_stream_preserves_deltas_and_timestamped_segments() {
    let request = TranscriptionRequest::new(audio()).expect("request");
    assert_eq!(request.audio().kind(), MediaKind::Audio);

    let mut assembler = TranscriptionAssembler::new();
    for event in [
        TranscriptionEvent::TextDelta {
            text: "hello ".to_owned(),
        },
        TranscriptionEvent::TextDelta {
            text: "world".to_owned(),
        },
        TranscriptionEvent::Segment {
            segment: TranscriptionSegment {
                id: 0,
                start_ms: 0,
                end_ms: 900,
                text: "hello world".to_owned(),
            },
        },
        TranscriptionEvent::Usage { usage: usage() },
        TranscriptionEvent::Finished {
            language: Some("en".to_owned()),
        },
    ] {
        assembler.push(&event).expect("transcription event");
    }
    let output = assembler.finish().expect("transcription output");
    assert_eq!(output.text, "hello world");
    assert_eq!(output.segments.len(), 1);
    assert_eq!(output.language.as_deref(), Some("en"));
    assert_eq!(output.usage, Some(usage()));
}

#[test]
fn unary_speech_is_the_same_stream_with_one_final_chunk() {
    let request =
        SpeechRequest::new("hello", "alloy", SpeechFormat::Pcm16).expect("speech request");
    assert_eq!(request.voice(), "alloy");

    let mut assembler = SpeechAssembler::new();
    for event in [
        SpeechEvent::OutputStarted {
            mime_type: "audio/pcm".to_owned(),
        },
        SpeechEvent::AudioChunk {
            sequence: 1,
            bytes: vec![0, 1, 2, 3],
        },
        SpeechEvent::OutputFinished,
        SpeechEvent::Usage { usage: usage() },
        SpeechEvent::Finished,
    ] {
        assembler.push(&event).expect("speech event");
    }
    let output = assembler.finish().expect("speech output");
    assert_eq!(output.audio.bytes, vec![0, 1, 2, 3]);
    assert_eq!(output.audio.descriptor.kind(), MediaKind::Audio);
    assert_eq!(output.audio.descriptor.mime_type(), "audio/pcm");
    assert_eq!(output.usage, Some(usage()));
}

#[test]
fn media_streams_reject_chunks_after_output_finished() {
    let mut assembler = SpeechAssembler::new();
    assembler
        .push(&SpeechEvent::OutputStarted {
            mime_type: "audio/pcm".to_owned(),
        })
        .expect("start");
    assembler
        .push(&SpeechEvent::AudioChunk {
            sequence: 1,
            bytes: vec![0, 1],
        })
        .expect("chunk");
    assembler
        .push(&SpeechEvent::OutputFinished)
        .expect("output finish");
    let error = assembler
        .push(&SpeechEvent::AudioChunk {
            sequence: 2,
            bytes: vec![2, 3],
        })
        .expect_err("post-finish chunk");
    assert_eq!(error.code(), "stream.output_not_open");
}
