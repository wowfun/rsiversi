use serde::{Deserialize, Serialize};

use crate::{AiError, MAX_BINARY_CHUNK_BYTES, MAX_REQUEST_BYTES, StreamError, validation};

const MAX_REALTIME_TEXT_BYTES: usize = 64 * 1024;

/// Audio encoding used on a live Realtime session.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RealtimeAudioFormat {
    /// Little-endian signed 16-bit PCM samples.
    Pcm16,
}

/// Configuration frozen when opening one Realtime session.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RealtimeRequest {
    voice: String,
    instructions: Option<String>,
    input_format: RealtimeAudioFormat,
    output_format: RealtimeAudioFormat,
}

impl<'de> Deserialize<'de> for RealtimeRequest {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct WireRequest {
            voice: String,
            instructions: Option<String>,
            input_format: RealtimeAudioFormat,
            output_format: RealtimeAudioFormat,
        }

        let wire = WireRequest::deserialize(deserializer)?;
        let request = Self {
            voice: wire.voice,
            instructions: wire.instructions,
            input_format: wire.input_format,
            output_format: wire.output_format,
        };
        request
            .validate()
            .map(|()| request)
            .map_err(serde::de::Error::custom)
    }
}

impl RealtimeRequest {
    /// Creates a session request for an exact provider voice using PCM16 audio.
    pub fn new(voice: impl Into<String>) -> Result<Self, StreamError> {
        let voice = voice.into();
        validation::identifier("realtime.voice", &voice)
            .map_err(|reason| StreamError::invalid("request.invalid_voice", reason))?;
        Ok(Self {
            voice,
            instructions: None,
            input_format: RealtimeAudioFormat::Pcm16,
            output_format: RealtimeAudioFormat::Pcm16,
        })
    }

    /// Adds bounded model instructions frozen with the session request.
    pub fn with_instructions(
        mut self,
        instructions: impl Into<String>,
    ) -> Result<Self, StreamError> {
        let instructions = instructions.into();
        validation::safe_text(
            "realtime.instructions",
            &instructions,
            MAX_REQUEST_BYTES,
            false,
        )
        .map_err(|reason| StreamError::invalid("request.invalid_instructions", reason))?;
        self.instructions = Some(instructions);
        Ok(self)
    }

    pub fn voice(&self) -> &str {
        &self.voice
    }

    pub fn instructions(&self) -> Option<&str> {
        self.instructions.as_deref()
    }

    pub const fn input_format(&self) -> RealtimeAudioFormat {
        self.input_format
    }

    pub const fn output_format(&self) -> RealtimeAudioFormat {
        self.output_format
    }

    /// Revalidates a deserialized session request.
    pub fn validate(&self) -> Result<(), StreamError> {
        validation::identifier("realtime.voice", &self.voice)
            .map_err(|reason| StreamError::invalid("request.invalid_voice", reason))?;
        if let Some(instructions) = &self.instructions {
            validation::safe_text(
                "realtime.instructions",
                instructions,
                MAX_REQUEST_BYTES,
                false,
            )
            .map_err(|reason| StreamError::invalid("request.invalid_instructions", reason))?;
        }
        Ok(())
    }
}

/// Caller-to-provider commands on one live session.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RealtimeCommand {
    /// Appends one ordered bounded chunk to the current audio input.
    AppendAudio {
        /// One-based contiguous input-audio sequence.
        sequence: u32,
        /// Raw PCM audio bytes.
        bytes: Vec<u8>,
    },
    /// Appends text to the current input item.
    AppendText {
        /// Bounded UTF-8 input text.
        text: String,
    },
    /// Commits the accumulated input as one provider item.
    CommitInput {
        /// Caller-chosen identifier for the committed item.
        item_id: String,
    },
    /// Requests a response from the committed conversation state.
    RequestResponse,
    /// Cancels one active provider response.
    CancelResponse { response_id: String },
    /// Begins orderly session closure; no later command is valid.
    Close,
}

/// Why a live session reached its one terminal event.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RealtimeCloseReason {
    /// The caller requested orderly closure.
    Client,
    /// The provider ended the session.
    Provider,
    /// A finite session I/O deadline elapsed.
    Timeout,
    /// The owning cancellation signal aborted the session.
    Aborted,
    /// Invalid provider or caller traffic made the session unusable.
    Protocol,
}

/// Provider-to-caller events on one live session.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RealtimeEvent {
    /// Opens the session and must be its first event.
    SessionStarted {
        /// Provider session identifier safe for correlation.
        session_id: String,
    },
    /// Reports provider-side speech detection for an input item.
    InputSpeechStarted { item_id: String },
    /// Appends an interim transcript fragment for an input item.
    InputTranscriptDelta {
        item_id: String,
        /// Bounded transcript fragment.
        text: String,
    },
    /// Supplies the final transcript for an input item.
    InputTranscriptFinished {
        item_id: String,
        /// Bounded final transcript.
        text: String,
    },
    /// Appends visible text to one provider response.
    OutputTextDelta {
        response_id: String,
        /// Bounded visible text fragment.
        text: String,
    },
    /// Emits one audio chunk for a provider response.
    OutputAudioChunk {
        response_id: String,
        /// Nonzero provider response chunk sequence.
        sequence: u32,
        /// Raw bounded audio bytes.
        bytes: Vec<u8>,
    },
    /// Requests that the caller take over an item outside the live model session.
    HandoffRequested {
        /// Identifier of the input item requiring handoff.
        item_id: String,
        /// Provider explanation safe to display to the caller.
        text: String,
    },
    /// Reports a non-terminal provider failure.
    RecoverableError { error: AiError },
    /// Terminates the session; no later event or command is valid.
    Closed { reason: RealtimeCloseReason },
}

/// Stateful grammar validation shared by Realtime adapters and consumers.
#[derive(Debug, Default)]
pub struct RealtimeValidator {
    started: bool,
    closing: bool,
    closed: bool,
    expected_audio_sequence: u32,
}

impl RealtimeValidator {
    /// Starts a validator before the mandatory session-start event.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            started: false,
            closing: false,
            closed: false,
            expected_audio_sequence: 1,
        }
    }

    /// Validates and records one caller command against current session state.
    pub fn push_command(&mut self, command: &RealtimeCommand) -> Result<(), StreamError> {
        self.require_live()?;
        if self.closing {
            return Err(StreamError::invalid(
                "realtime.closing",
                "Realtime session is already closing",
            ));
        }
        match command {
            RealtimeCommand::AppendAudio { sequence, bytes } => {
                if *sequence != self.expected_audio_sequence {
                    return Err(StreamError::invalid(
                        "realtime.audio_sequence",
                        format!(
                            "audio sequence {sequence} does not match expected {}",
                            self.expected_audio_sequence
                        ),
                    ));
                }
                if bytes.is_empty() || bytes.len() > MAX_BINARY_CHUNK_BYTES {
                    return Err(StreamError::invalid(
                        "realtime.invalid_audio",
                        "Realtime audio frame is empty or exceeds its bound",
                    ));
                }
                self.expected_audio_sequence = self.expected_audio_sequence.saturating_add(1);
            }
            RealtimeCommand::AppendText { text } => validate_text("command.text", text)?,
            RealtimeCommand::CommitInput { item_id } => validate_id("command.item_id", item_id)?,
            RealtimeCommand::CancelResponse { response_id } => {
                validate_id("command.response_id", response_id)?;
            }
            RealtimeCommand::RequestResponse => {}
            RealtimeCommand::Close => self.closing = true,
        }
        Ok(())
    }

    /// Validates and records one provider event against current session state.
    pub fn push_event(&mut self, event: &RealtimeEvent) -> Result<(), StreamError> {
        if self.closed {
            return Err(StreamError::invalid(
                "realtime.closed",
                "Realtime session emitted an event after close",
            ));
        }
        match event {
            RealtimeEvent::SessionStarted { session_id } => {
                if self.started {
                    return Err(StreamError::invalid(
                        "realtime.duplicate_start",
                        "Realtime session started more than once",
                    ));
                }
                validate_id("event.session_id", session_id)?;
                self.started = true;
            }
            _ if !self.started => {
                return Err(StreamError::invalid(
                    "realtime.not_started",
                    "Realtime event arrived before session start",
                ));
            }
            RealtimeEvent::InputSpeechStarted { item_id } => {
                validate_id("event.item_id", item_id)?;
            }
            RealtimeEvent::InputTranscriptDelta { item_id, text }
            | RealtimeEvent::InputTranscriptFinished { item_id, text }
            | RealtimeEvent::HandoffRequested { item_id, text } => {
                validate_id("event.item_id", item_id)?;
                validate_text("event.text", text)?;
            }
            RealtimeEvent::OutputTextDelta { response_id, text } => {
                validate_id("event.response_id", response_id)?;
                validate_text("event.text", text)?;
            }
            RealtimeEvent::OutputAudioChunk {
                response_id,
                sequence,
                bytes,
            } => {
                validate_id("event.response_id", response_id)?;
                if *sequence == 0 || bytes.is_empty() || bytes.len() > MAX_BINARY_CHUNK_BYTES {
                    return Err(StreamError::invalid(
                        "realtime.invalid_audio",
                        "Realtime output audio has an invalid sequence or size",
                    ));
                }
            }
            RealtimeEvent::RecoverableError { .. } => {}
            RealtimeEvent::Closed { .. } => {
                self.closed = true;
                self.closing = true;
            }
        }
        Ok(())
    }

    fn require_live(&self) -> Result<(), StreamError> {
        if self.closed {
            return Err(StreamError::invalid(
                "realtime.closed",
                "Realtime session is closed",
            ));
        }
        if !self.started {
            return Err(StreamError::invalid(
                "realtime.not_started",
                "Realtime command was sent before session start",
            ));
        }
        Ok(())
    }
}

fn validate_id(field: &str, value: &str) -> Result<(), StreamError> {
    validation::identifier(field, value)
        .map_err(|reason| StreamError::invalid("realtime.invalid_id", reason))
}

fn validate_text(field: &str, value: &str) -> Result<(), StreamError> {
    validation::safe_text(field, value, MAX_REALTIME_TEXT_BYTES, false)
        .map_err(|reason| StreamError::invalid("realtime.invalid_text", reason))
}
