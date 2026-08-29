use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::{
    MAX_BINARY_CHUNK_BYTES, MAX_IMAGE_BYTES, MAX_REQUEST_BYTES, MediaDescriptor, MediaKind,
    StreamError, TokenUsage, validation,
};

pub const MAX_IMAGE_OUTPUTS: u8 = 10;

/// One bounded image generation request.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ImageRequest {
    prompt: String,
    count: u8,
    inputs: Vec<MediaDescriptor>,
    mask: Option<MediaDescriptor>,
}

impl<'de> Deserialize<'de> for ImageRequest {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct WireRequest {
            prompt: String,
            count: u8,
            inputs: Vec<MediaDescriptor>,
            mask: Option<MediaDescriptor>,
        }

        let wire = WireRequest::deserialize(deserializer)?;
        let request = Self {
            prompt: wire.prompt,
            count: wire.count,
            inputs: wire.inputs,
            mask: wire.mask,
        };
        request
            .validate()
            .map(|()| request)
            .map_err(serde::de::Error::custom)
    }
}

impl ImageRequest {
    /// Creates a text-only request for `count` generated images.
    pub fn new(prompt: impl Into<String>, count: u8) -> Result<Self, StreamError> {
        let request = Self {
            prompt: prompt.into(),
            count,
            inputs: Vec::new(),
            mask: None,
        };
        request.validate()?;
        Ok(request)
    }

    /// Adds bounded image inputs and an optional image mask for editing.
    pub fn with_inputs(
        mut self,
        inputs: Vec<MediaDescriptor>,
        mask: Option<MediaDescriptor>,
    ) -> Result<Self, StreamError> {
        self.inputs = inputs;
        self.mask = mask;
        self.validate()?;
        Ok(self)
    }

    pub fn prompt(&self) -> &str {
        &self.prompt
    }

    pub const fn count(&self) -> u8 {
        self.count
    }

    pub fn inputs(&self) -> &[MediaDescriptor] {
        &self.inputs
    }

    pub fn mask(&self) -> Option<&MediaDescriptor> {
        self.mask.as_ref()
    }

    /// Returns deterministic canonical JSON bytes for identity and persistence.
    pub fn canonical_bytes(&self) -> Result<Vec<u8>, StreamError> {
        self.encode_canonical()
    }

    fn encode_canonical(&self) -> Result<Vec<u8>, StreamError> {
        let value = serde_json::to_value(self)
            .map_err(|error| StreamError::invalid("request.encoding", error.to_string()))?;
        let canonical = validation::canonical_json(value)
            .map_err(|error| StreamError::invalid("request.encoding", error.to_string()))?;
        let bytes = serde_json::to_vec(&canonical)
            .map_err(|error| StreamError::invalid("request.encoding", error.to_string()))?;
        if bytes.len() > MAX_REQUEST_BYTES {
            return Err(StreamError::invalid(
                "request.too_large",
                format!("canonical encoding exceeds {MAX_REQUEST_BYTES} bytes"),
            ));
        }
        Ok(bytes)
    }

    /// Revalidates a deserialized request and all media-kind relationships.
    pub fn validate(&self) -> Result<(), StreamError> {
        validation::safe_text("image.prompt", &self.prompt, MAX_REQUEST_BYTES, false)
            .map_err(|reason| StreamError::invalid("request.invalid_prompt", reason))?;
        if self.count == 0 || self.count > MAX_IMAGE_OUTPUTS {
            return Err(StreamError::invalid(
                "request.invalid_count",
                format!("image count must be 1..={MAX_IMAGE_OUTPUTS}"),
            ));
        }
        if self.inputs.len() > usize::from(MAX_IMAGE_OUTPUTS)
            || (self.mask.is_some() && self.inputs.is_empty())
            || self
                .inputs
                .iter()
                .any(|input| input.kind() != MediaKind::Image)
            || self
                .mask
                .as_ref()
                .is_some_and(|mask| mask.kind() != MediaKind::Image)
        {
            return Err(StreamError::invalid(
                "request.invalid_media",
                "image inputs and mask must be bounded image descriptors",
            ));
        }
        self.encode_canonical()?;
        Ok(())
    }
}

/// Normalized image generation events.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ImageEvent {
    /// Opens the next image output.
    OutputStarted {
        /// Zero-based contiguous output index.
        index: u32,
        /// Declared MIME type for the following bytes.
        mime_type: String,
    },
    /// Appends one bounded binary chunk to an open output.
    OutputChunk {
        /// Index of the open output.
        index: u32,
        /// One-based contiguous chunk sequence.
        sequence: u32,
        bytes: Vec<u8>,
    },
    /// Closes one image output and computes its descriptor.
    OutputFinished {
        /// Index of the output to close.
        index: u32,
    },
    /// Supplies the operation's sole cumulative usage record.
    Usage { usage: TokenUsage },
    /// Terminates the stream after at least one output is closed.
    Finished,
}

/// One complete binary output and its computed descriptor.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MediaOutput {
    /// Descriptor computed from the assembled bytes.
    pub descriptor: MediaDescriptor,
    /// Complete validated media body.
    pub bytes: Vec<u8>,
}

/// Complete image generation output.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ImageOutput {
    /// Ordered generated images.
    pub images: Vec<MediaOutput>,
    /// Provider-reported usage, when available.
    pub usage: Option<TokenUsage>,
}

/// Strict image stream assembler.
#[derive(Debug, Default)]
pub struct ImageAssembler {
    next_index: u32,
    open: BTreeMap<u32, OpenMedia>,
    outputs: BTreeMap<u32, MediaOutput>,
    completed_outputs: usize,
    usage: Option<TokenUsage>,
    finished: bool,
}

impl ImageAssembler {
    #[must_use]
    pub const fn new() -> Self {
        Self {
            next_index: 0,
            open: BTreeMap::new(),
            outputs: BTreeMap::new(),
            completed_outputs: 0,
            usage: None,
            finished: false,
        }
    }

    /// Applies one event while enforcing output, chunk, terminal, and size invariants.
    pub fn push(&mut self, event: &ImageEvent) -> Result<(), StreamError> {
        if self.finished {
            return Err(StreamError::invalid(
                "stream.already_finished",
                "image stream emitted an event after its terminal event",
            ));
        }
        match event {
            ImageEvent::OutputStarted { index, mime_type } => {
                if *index != self.next_index || *index >= u32::from(MAX_IMAGE_OUTPUTS) {
                    return Err(StreamError::invalid(
                        "stream.non_contiguous_index",
                        "image output indexes must be contiguous and bounded",
                    ));
                }
                self.open
                    .insert(*index, OpenMedia::new(MediaKind::Image, mime_type.clone()));
                self.next_index = self.next_index.saturating_add(1);
            }
            ImageEvent::OutputChunk {
                index,
                sequence,
                bytes,
            } => self
                .open
                .get_mut(index)
                .ok_or_else(output_not_open)?
                .push(*sequence, bytes)?,
            ImageEvent::OutputFinished { index } => {
                let output = self
                    .open
                    .remove(index)
                    .ok_or_else(output_not_open)?
                    .finish()?;
                self.outputs.insert(*index, output);
                self.completed_outputs = self.completed_outputs.saturating_add(1);
            }
            ImageEvent::Usage { usage } => set_usage(&mut self.usage, *usage)?,
            ImageEvent::Finished => {
                if !self.open.is_empty() || self.completed_outputs == 0 {
                    return Err(StreamError::invalid(
                        "stream.output_still_open",
                        "image stream finished without at least one closed output",
                    ));
                }
                self.finished = true;
            }
        }
        Ok(())
    }

    /// Borrows one already-closed output before the complete stream terminates.
    ///
    /// This lets durable callers commit each image independently instead of
    /// retaining all successful outputs behind a later provider failure.
    pub fn completed(&self, index: u32) -> Option<&MediaOutput> {
        self.outputs.get(&index)
    }

    /// Moves one closed output out so durable consumers can release its body promptly.
    pub fn take_completed(&mut self, index: u32) -> Option<MediaOutput> {
        self.outputs.remove(&index)
    }

    /// Returns the number of outputs closed during this stream, including drained outputs.
    pub const fn completed_count(&self) -> usize {
        self.completed_outputs
    }

    /// Returns complete output only after a valid terminal event.
    pub fn finish(self) -> Result<ImageOutput, StreamError> {
        if !self.finished {
            return Err(StreamError::invalid(
                "stream.missing_finish",
                "image stream ended without a terminal event",
            ));
        }
        Ok(ImageOutput {
            images: self.outputs.into_values().collect(),
            usage: self.usage,
        })
    }
}

#[derive(Debug)]
struct OpenMedia {
    kind: MediaKind,
    mime_type: String,
    expected_sequence: u32,
    bytes: Vec<u8>,
}

impl OpenMedia {
    fn new(kind: MediaKind, mime_type: String) -> Self {
        debug_assert_eq!(kind, MediaKind::Image);
        Self {
            kind,
            mime_type,
            expected_sequence: 1,
            bytes: Vec::new(),
        }
    }

    fn push(&mut self, sequence: u32, bytes: &[u8]) -> Result<(), StreamError> {
        if sequence != self.expected_sequence {
            return Err(StreamError::invalid(
                "stream.chunk_sequence",
                format!(
                    "media chunk {sequence} does not match expected {}",
                    self.expected_sequence
                ),
            ));
        }
        if bytes.is_empty() || bytes.len() > MAX_BINARY_CHUNK_BYTES {
            return Err(StreamError::invalid(
                "stream.invalid_chunk",
                "media chunk is empty or exceeds its bound",
            ));
        }
        let assembled_length = u64::try_from(self.bytes.len().saturating_add(bytes.len()))
            .map_err(|_| {
                StreamError::invalid(
                    "stream.output_too_large",
                    "media output length exceeds the protocol representation",
                )
            })?;
        if assembled_length > MAX_IMAGE_BYTES {
            return Err(StreamError::invalid(
                "stream.output_too_large",
                "media output exceeds its assembled bound",
            ));
        }
        self.bytes.extend_from_slice(bytes);
        self.expected_sequence = self.expected_sequence.saturating_add(1);
        Ok(())
    }

    fn finish(self) -> Result<MediaOutput, StreamError> {
        if self.bytes.is_empty() {
            return Err(StreamError::invalid(
                "stream.empty_output",
                "media output is empty",
            ));
        }
        let sha256 = hex::encode(Sha256::digest(&self.bytes));
        let descriptor = MediaDescriptor::new(
            self.kind,
            self.mime_type,
            u64::try_from(self.bytes.len()).map_err(|_| {
                StreamError::invalid("stream.output_too_large", "media length exceeds u64")
            })?,
            sha256,
        )
        .map_err(|error| StreamError::invalid("stream.invalid_media", error.to_string()))?;
        Ok(MediaOutput {
            descriptor,
            bytes: self.bytes,
        })
    }
}

fn set_usage(slot: &mut Option<TokenUsage>, usage: TokenUsage) -> Result<(), StreamError> {
    if slot.replace(usage).is_some() {
        return Err(StreamError::invalid(
            "stream.duplicate_usage",
            "stream emitted usage more than once",
        ));
    }
    Ok(())
}

fn output_not_open() -> StreamError {
    StreamError::invalid(
        "stream.output_not_open",
        "media output is not currently open",
    )
}
