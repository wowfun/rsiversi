use rsi_ai_protocol::{
    BlobAssembler, MediaDescriptor, MediaKind, WireFrame, decode_wire_frame, encode_wire_frame,
};

const HELLO_SHA256: &str = "b94d27b9934d3e08a52e52d7da7dabfac484efe37a5380ee9088f7ace2efcde9";

#[test]
fn binary_wire_round_trips_control_and_raw_blob_bytes() {
    let control = WireFrame::Control {
        call_id: "call-1".to_owned(),
        payload: br#"{"type":"prepare"}"#.to_vec(),
    };
    let chunk = WireFrame::BlobChunk {
        call_id: "call-1".to_owned(),
        blob_id: "input-1".to_owned(),
        sequence: 1,
        final_chunk: true,
        bytes: vec![0, 255, 10, 13, 42],
    };

    for frame in [control, chunk] {
        let encoded = encode_wire_frame(&frame).expect("encode");
        assert_eq!(&encoded[..4], b"RAI0");
        assert_eq!(decode_wire_frame(&encoded).expect("decode"), frame);
    }
}

#[test]
fn blob_assembler_verifies_sequence_length_and_digest() {
    let descriptor =
        MediaDescriptor::new(MediaKind::Audio, "audio/wav", 11, HELLO_SHA256).expect("descriptor");
    let mut assembler = BlobAssembler::new(descriptor);
    assembler.push(1, b"hello ", false).expect("first chunk");
    assembler.push(2, b"world", true).expect("final chunk");
    assert_eq!(assembler.finish().expect("verified bytes"), b"hello world");
}

#[test]
fn blob_assembler_rejects_gaps_and_digest_mismatch() {
    let descriptor =
        MediaDescriptor::new(MediaKind::Audio, "audio/wav", 11, HELLO_SHA256).expect("descriptor");
    let mut gap = BlobAssembler::new(descriptor.clone());
    let error = gap.push(2, b"hello world", true).expect_err("sequence gap");
    assert_eq!(error.code(), "blob.sequence");

    let mut mismatch = BlobAssembler::new(descriptor);
    mismatch
        .push(1, b"hello worle", true)
        .expect("bounded chunk");
    let error = mismatch.finish().expect_err("digest mismatch");
    assert_eq!(error.code(), "blob.digest");
}

#[test]
fn wire_rejects_oversized_chunks_before_allocation_or_send() {
    let frame = WireFrame::BlobChunk {
        call_id: "call-1".to_owned(),
        blob_id: "input-1".to_owned(),
        sequence: 1,
        final_chunk: true,
        bytes: vec![0; rsi_ai_protocol::MAX_BINARY_CHUNK_BYTES + 1],
    };
    let error = encode_wire_frame(&frame).expect_err("oversized chunk");
    assert_eq!(error.code(), "wire.payload_too_large");
}
