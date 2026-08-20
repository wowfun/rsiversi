use serde::{Deserialize, Serialize};

use crate::{AiError, MAX_BINARY_CHUNK_BYTES, MAX_REQUEST_BYTES, StreamError, validation};

const MAX_REALTIME_TEXT_BYTES: usize = 64 * 1024;

/// Audio encoding used on a live Realtime session.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RealtimeAudioFormat {
    Pcm16,
}

/// Configuration frozen when opening one Realtime session.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RealtimeRequest {
    voice: String,
    instructions: Option<String>,
    input_format: RealtimeAudioFormat,
    output_format: RealtimeAudioFormat,
}

impl RealtimeRequest {
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
    AppendAudio { sequence: u32, bytes: Vec<u8> },
    AppendText { text: String },
    CommitInput { item_id: String },
    RequestResponse,
    CancelResponse { response_id: String },
    Close,
}

/// Why a live session reached its one terminal event.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RealtimeCloseReason {
    Client,
    Provider,
    Timeout,
    Aborted,
    Protocol,
}

/// Provider-to-caller events on one live session.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RealtimeEvent {
    SessionStarted {
        session_id: String,
    },
    InputSpeechStarted {
        item_id: String,
    },
    InputTranscriptDelta {
        item_id: String,
        text: String,
    },
    InputTranscriptFinished {
        item_id: String,
        text: String,
    },
    OutputTextDelta {
        response_id: String,
        text: String,
    },
    OutputAudioChunk {
        response_id: String,
        sequence: u32,
        bytes: Vec<u8>,
    },
    HandoffRequested {
        item_id: String,
        text: String,
    },
    RecoverableError {
        error: AiError,
    },
    Closed {
        reason: RealtimeCloseReason,
    },
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
    #[must_use]
    pub const fn new() -> Self {
        Self {
            started: false,
            closing: false,
            closed: false,
            expected_audio_sequence: 1,
        }
    }

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
