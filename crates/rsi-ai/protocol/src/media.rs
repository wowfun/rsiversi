use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::{
    MAX_AUDIO_BYTES, MAX_BINARY_CHUNK_BYTES, MAX_IMAGE_BYTES, MAX_REQUEST_BYTES, MediaDescriptor,
    MediaKind, StreamError, TokenUsage, validation,
};

pub const MAX_IMAGE_OUTPUTS: u8 = 10;
const MAX_TRANSCRIPTION_SEGMENTS: usize = 4_096;

/// One bounded image generation request.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ImageRequest {
    prompt: String,
    count: u8,
    inputs: Vec<MediaDescriptor>,
    mask: Option<MediaDescriptor>,
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
            }
            ImageEvent::Usage { usage } => set_usage(&mut self.usage, *usage)?,
            ImageEvent::Finished => {
                if !self.open.is_empty() || self.outputs.is_empty() {
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

/// Transcription of one bounded audio object.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TranscriptionRequest {
    audio: MediaDescriptor,
    language: Option<String>,
    prompt: Option<String>,
    timestamps: bool,
}

impl TranscriptionRequest {
    /// Creates a request for one validated audio descriptor.
    pub fn new(audio: MediaDescriptor) -> Result<Self, StreamError> {
        if audio.kind() != MediaKind::Audio {
            return Err(StreamError::invalid(
                "request.invalid_media",
                "transcription input must be audio",
            ));
        }
        Ok(Self {
            audio,
            language: None,
            prompt: None,
            timestamps: false,
        })
    }

    pub fn audio(&self) -> &MediaDescriptor {
        &self.audio
    }

    pub fn language(&self) -> Option<&str> {
        self.language.as_deref()
    }

    pub fn prompt(&self) -> Option<&str> {
        self.prompt.as_deref()
    }

    pub const fn timestamps(&self) -> bool {
        self.timestamps
    }

    /// Adds a bounded language identifier hint.
    pub fn with_language(mut self, language: impl Into<String>) -> Result<Self, StreamError> {
        let language = language.into();
        validation::identifier("transcription.language", &language)
            .map_err(|reason| StreamError::invalid("request.invalid_language", reason))?;
        self.language = Some(language);
        Ok(self)
    }

    /// Adds bounded provider context for transcription.
    pub fn with_prompt(mut self, prompt: impl Into<String>) -> Result<Self, StreamError> {
        let prompt = prompt.into();
        validation::safe_text("transcription.prompt", &prompt, MAX_REQUEST_BYTES, false)
            .map_err(|reason| StreamError::invalid("request.invalid_prompt", reason))?;
        self.prompt = Some(prompt);
        Ok(self)
    }

    #[must_use]
    /// Selects whether the provider should return timestamped segments.
    pub const fn with_timestamps(mut self, timestamps: bool) -> Self {
        self.timestamps = timestamps;
        self
    }

    /// Revalidates a deserialized request and its audio-kind constraint.
    pub fn validate(&self) -> Result<(), StreamError> {
        if self.audio.kind() != MediaKind::Audio {
            return Err(StreamError::invalid(
                "request.invalid_media",
                "transcription input must be audio",
            ));
        }
        self.audio
            .validate()
            .map_err(|error| StreamError::invalid("request.invalid_media", error.to_string()))?;
        if let Some(language) = &self.language {
            validation::identifier("transcription.language", language)
                .map_err(|reason| StreamError::invalid("request.invalid_language", reason))?;
        }
        if let Some(prompt) = &self.prompt {
            validation::safe_text("transcription.prompt", prompt, MAX_REQUEST_BYTES, false)
                .map_err(|reason| StreamError::invalid("request.invalid_prompt", reason))?;
        }
        Ok(())
    }
}

/// One final timestamped transcription segment.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TranscriptionSegment {
    /// Zero-based contiguous segment identifier.
    pub id: u32,
    /// Inclusive segment start offset in milliseconds.
    pub start_ms: u64,
    /// Segment end offset in milliseconds, not earlier than `start_ms`.
    pub end_ms: u64,
    pub text: String,
}

/// Normalized streaming transcription events.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum TranscriptionEvent {
    /// Appends text to the complete transcript.
    TextDelta {
        /// Bounded UTF-8 transcript fragment.
        text: String,
    },
    /// Adds one final ordered timestamped segment.
    Segment { segment: TranscriptionSegment },
    /// Supplies the operation's sole cumulative usage record.
    Usage { usage: TokenUsage },
    /// Terminates a nonempty transcript.
    Finished {
        /// Provider-detected language, when reported.
        language: Option<String>,
    },
}

/// Complete assembled transcription.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TranscriptionOutput {
    /// Complete assembled transcript text.
    pub text: String,
    /// Ordered final timestamped segments.
    pub segments: Vec<TranscriptionSegment>,
    /// Provider-detected language, when reported.
    pub language: Option<String>,
    /// Provider-reported usage, when available.
    pub usage: Option<TokenUsage>,
}

/// Strict transcription assembler.
#[derive(Debug, Default)]
pub struct TranscriptionAssembler {
    text: String,
    segments: Vec<TranscriptionSegment>,
    retained_text_bytes: usize,
    language: Option<String>,
    usage: Option<TokenUsage>,
    finished: bool,
}

impl TranscriptionAssembler {
    #[must_use]
    pub const fn new() -> Self {
        Self {
            text: String::new(),
            segments: Vec::new(),
            retained_text_bytes: 0,
            language: None,
            usage: None,
            finished: false,
        }
    }

    /// Applies one event while enforcing text, segment, usage, and terminal invariants.
    pub fn push(&mut self, event: &TranscriptionEvent) -> Result<(), StreamError> {
        if self.finished {
            return Err(StreamError::invalid(
                "stream.already_finished",
                "transcription stream emitted an event after terminal",
            ));
        }
        match event {
            TranscriptionEvent::TextDelta { text } => {
                validation::safe_text("transcription.delta", text, MAX_REQUEST_BYTES, false)
                    .map_err(|reason| StreamError::invalid("stream.invalid_text", reason))?;
                if self.retained_text_bytes.saturating_add(text.len()) > MAX_REQUEST_BYTES {
                    return Err(StreamError::invalid(
                        "stream.output_too_large",
                        "transcription text exceeds its assembled bound",
                    ));
                }
                self.retained_text_bytes = self.retained_text_bytes.saturating_add(text.len());
                self.text.push_str(text);
            }
            TranscriptionEvent::Segment { segment } => {
                if self.segments.len() >= MAX_TRANSCRIPTION_SEGMENTS
                    || usize::try_from(segment.id).ok() != Some(self.segments.len())
                    || segment.start_ms > segment.end_ms
                {
                    return Err(StreamError::invalid(
                        "stream.invalid_segment",
                        "transcription segments must be contiguous, ordered, and bounded",
                    ));
                }
                validation::safe_text(
                    "transcription.segment.text",
                    &segment.text,
                    MAX_REQUEST_BYTES,
                    false,
                )
                .map_err(|reason| StreamError::invalid("stream.invalid_segment", reason))?;
                if self.retained_text_bytes.saturating_add(segment.text.len()) > MAX_REQUEST_BYTES {
                    return Err(StreamError::invalid(
                        "stream.output_too_large",
                        "transcription text and segments exceed their assembled bound",
                    ));
                }
                self.retained_text_bytes =
                    self.retained_text_bytes.saturating_add(segment.text.len());
                self.segments.push(segment.clone());
            }
            TranscriptionEvent::Usage { usage } => set_usage(&mut self.usage, *usage)?,
            TranscriptionEvent::Finished { language } => {
                if self.text.is_empty() {
                    return Err(StreamError::invalid(
                        "stream.empty_output",
                        "transcription completed without text",
                    ));
                }
                if let Some(language) = &language {
                    validation::identifier("transcription.language", language).map_err(
                        |reason| StreamError::invalid("stream.invalid_language", reason),
                    )?;
                }
                self.language.clone_from(language);
                self.finished = true;
            }
        }
        Ok(())
    }

    /// Returns complete output only after a valid terminal event.
    pub fn finish(self) -> Result<TranscriptionOutput, StreamError> {
        if !self.finished {
            return Err(StreamError::invalid(
                "stream.missing_finish",
                "transcription stream ended without a terminal event",
            ));
        }
        Ok(TranscriptionOutput {
            text: self.text,
            segments: self.segments,
            language: self.language,
            usage: self.usage,
        })
    }
}

/// Speech output format requested by a caller.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SpeechFormat {
    /// Headerless little-endian signed 16-bit PCM samples.
    Pcm16,
    /// RIFF/WAVE audio.
    Wav,
    /// MPEG Layer III audio.
    Mp3,
}

/// One bounded text-to-speech request.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SpeechRequest {
    text: String,
    voice: String,
    format: SpeechFormat,
    speed: Option<f32>,
}

impl SpeechRequest {
    /// Creates a bounded speech request using an exact voice and output format.
    pub fn new(
        text: impl Into<String>,
        voice: impl Into<String>,
        format: SpeechFormat,
    ) -> Result<Self, StreamError> {
        let request = Self {
            text: text.into(),
            voice: voice.into(),
            format,
            speed: None,
        };
        request.validate()?;
        Ok(request)
    }

    /// Adds a finite provider-neutral speaking-rate multiplier.
    pub fn with_speed(mut self, speed: f32) -> Result<Self, StreamError> {
        self.speed = Some(speed);
        self.validate()?;
        Ok(self)
    }

    pub fn text(&self) -> &str {
        &self.text
    }

    pub fn voice(&self) -> &str {
        &self.voice
    }

    pub const fn format(&self) -> SpeechFormat {
        self.format
    }

    pub const fn speed(&self) -> Option<f32> {
        self.speed
    }

    /// Revalidates a deserialized request and its speaking-rate bound.
    pub fn validate(&self) -> Result<(), StreamError> {
        validation::safe_text("speech.text", &self.text, MAX_REQUEST_BYTES, false)
            .map_err(|reason| StreamError::invalid("request.invalid_text", reason))?;
        validation::identifier("speech.voice", &self.voice)
            .map_err(|reason| StreamError::invalid("request.invalid_voice", reason))?;
        if self
            .speed
            .is_some_and(|speed| !speed.is_finite() || !(0.25..=4.0).contains(&speed))
        {
            return Err(StreamError::invalid(
                "request.invalid_speed",
                "speech speed must be finite and within 0.25..=4.0",
            ));
        }
        Ok(())
    }
}

/// Normalized streaming speech events.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SpeechEvent {
    /// Opens the operation's sole audio output.
    OutputStarted {
        /// Declared MIME type for the following bytes.
        mime_type: String,
    },
    /// Appends one bounded audio chunk.
    AudioChunk {
        /// One-based contiguous chunk sequence.
        sequence: u32,
        bytes: Vec<u8>,
    },
    /// Closes the audio output and computes its descriptor.
    OutputFinished,
    /// Supplies the operation's sole cumulative usage record.
    Usage { usage: TokenUsage },
    /// Terminates the stream after the output is closed.
    Finished,
}

/// Complete speech output.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SpeechOutput {
    /// Complete validated synthesized audio.
    pub audio: MediaOutput,
    /// Provider-reported usage, when available.
    pub usage: Option<TokenUsage>,
}

/// Strict speech stream assembler.
#[derive(Debug, Default)]
pub struct SpeechAssembler {
    open: Option<OpenMedia>,
    output: Option<MediaOutput>,
    usage: Option<TokenUsage>,
    finished: bool,
}

impl SpeechAssembler {
    #[must_use]
    pub const fn new() -> Self {
        Self {
            open: None,
            output: None,
            usage: None,
            finished: false,
        }
    }

    /// Applies one event while enforcing the single-output stream grammar.
    pub fn push(&mut self, event: &SpeechEvent) -> Result<(), StreamError> {
        if self.finished {
            return Err(StreamError::invalid(
                "stream.already_finished",
                "speech stream emitted an event after terminal",
            ));
        }
        match event {
            SpeechEvent::OutputStarted { mime_type } => {
                if self.open.is_some() || self.output.is_some() {
                    return Err(StreamError::invalid(
                        "stream.duplicate_output",
                        "speech stream opened more than one output",
                    ));
                }
                self.open = Some(OpenMedia::new(MediaKind::Audio, mime_type.clone()));
            }
            SpeechEvent::AudioChunk { sequence, bytes } => self
                .open
                .as_mut()
                .ok_or_else(output_not_open)?
                .push(*sequence, bytes)?,
            SpeechEvent::OutputFinished => {
                self.output = Some(self.open.take().ok_or_else(output_not_open)?.finish()?);
            }
            SpeechEvent::Usage { usage } => set_usage(&mut self.usage, *usage)?,
            SpeechEvent::Finished => {
                if self.open.is_some() || self.output.is_none() {
                    return Err(StreamError::invalid(
                        "stream.output_still_open",
                        "speech stream finished without one closed output",
                    ));
                }
                self.finished = true;
            }
        }
        Ok(())
    }

    /// Returns complete output only after a valid terminal event.
    pub fn finish(self) -> Result<SpeechOutput, StreamError> {
        if !self.finished {
            return Err(StreamError::invalid(
                "stream.missing_finish",
                "speech stream ended without a terminal event",
            ));
        }
        let audio = self.output.ok_or_else(|| {
            StreamError::invalid(
                "stream.missing_output",
                "speech stream finished without an output descriptor",
            )
        })?;
        Ok(SpeechOutput {
            audio,
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
        let maximum = match self.kind {
            MediaKind::Image => MAX_IMAGE_BYTES,
            MediaKind::Audio => MAX_AUDIO_BYTES,
        };
        let assembled_length = u64::try_from(self.bytes.len().saturating_add(bytes.len()))
            .map_err(|_| {
                StreamError::invalid(
                    "stream.output_too_large",
                    "media output length exceeds the protocol representation",
                )
            })?;
        if assembled_length > maximum {
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
        let digest = Sha256::digest(&self.bytes);
        let sha256 = hex::encode(digest);
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
