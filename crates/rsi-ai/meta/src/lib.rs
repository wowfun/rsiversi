//! `rsi-ai` semantic protocol carried over generation-pinned `rsi-meta` streams.

#![deny(unsafe_code)]
#![allow(clippy::missing_errors_doc)] // Public failures use closed, typed wire/stream errors.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;

use rsi_ai_protocol::{
    AiError, ImageRequest, LanguageEvent, LanguageRequest, MAX_CONTROL_FRAME_BYTES,
    MediaDescriptor, MediaKind, RealtimeCloseReason, RealtimeRequest, SpeechRequest, TokenUsage,
    TranscriptionEvent, TranscriptionRequest, WireFrame, decode_wire_frame, encode_wire_frame,
};
pub use rsi_ai_provider::{Capability, PreparedCallSnapshot, RetryPolicy};
use rsi_meta::{
    CompositionHost, InstanceId, STREAM_BYTE_BUDGET, ServiceKey, ServiceOpenRequest, ServiceStream,
    StreamKind,
};

mod plugin_host;
pub use plugin_host::{
    PluginError, PluginMediaResolver, PluginProvider, PluginProviderFactory, ProviderPlugin,
    build_plugin_provider,
};

/// One statically declared rsi-meta service contract.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AiService {
    Language,
    Image,
    Transcription,
    Speech,
    Realtime,
}

impl AiService {
    pub const fn key(self) -> &'static str {
        match self {
            Self::Language => "rsi.ai.language",
            Self::Image => "rsi.ai.image",
            Self::Transcription => "rsi.ai.transcription",
            Self::Speech => "rsi.ai.speech",
            Self::Realtime => "rsi.ai.realtime",
        }
    }

    pub const fn version(self) -> u32 {
        0
    }

    pub fn from_key(value: &str) -> Option<Self> {
        match value {
            "rsi.ai.language" => Some(Self::Language),
            "rsi.ai.image" => Some(Self::Image),
            "rsi.ai.transcription" => Some(Self::Transcription),
            "rsi.ai.speech" => Some(Self::Speech),
            "rsi.ai.realtime" => Some(Self::Realtime),
            _ => None,
        }
    }
}

/// Typed consumer wrapper over one generation-pinned rsi-meta service stream.
pub struct MetaServiceStream {
    service: AiService,
    provider: InstanceId,
    stream: ServiceStream,
}

impl MetaServiceStream {
    pub async fn open(
        host: &CompositionHost,
        consumer: InstanceId,
        service: AiService,
    ) -> Result<Self, MetaStreamError> {
        let mut stream = host.open_service(ServiceOpenRequest {
            consumer,
            service: ServiceKey::new(service.key()),
        })?;
        stream.grant_credit(STREAM_BYTE_BUDGET).await?;
        let provider = stream.provider().clone();
        Ok(Self {
            service,
            provider,
            stream,
        })
    }

    pub const fn service(&self) -> AiService {
        self.service
    }

    pub const fn provider(&self) -> &InstanceId {
        &self.provider
    }

    pub async fn send_control(&mut self, control: &ClientControl) -> Result<(), MetaStreamError> {
        let call_id = control.call_id();
        let payload = encode_client_control(control)?;
        let bytes = encode_wire_frame(&WireFrame::Control {
            call_id: call_id.to_owned(),
            payload,
        })?;
        self.stream.send(&bytes).await?;
        Ok(())
    }

    pub async fn send_blob_chunk(
        &mut self,
        call_id: impl Into<String>,
        blob_id: impl Into<String>,
        sequence: u32,
        final_chunk: bool,
        bytes: Vec<u8>,
    ) -> Result<(), MetaStreamError> {
        let bytes = encode_wire_frame(&WireFrame::BlobChunk {
            call_id: call_id.into(),
            blob_id: blob_id.into(),
            sequence,
            final_chunk,
            bytes,
        })?;
        self.stream.send(&bytes).await?;
        Ok(())
    }

    pub async fn recv(&mut self) -> Result<Option<MetaIncoming>, MetaStreamError> {
        loop {
            let Some(frame) = self.stream.recv().await else {
                return Err(MetaStreamError::ClosedWithoutTerminal);
            };
            let frame = frame?;
            match frame.kind {
                StreamKind::Credit => {}
                StreamKind::Data => {
                    let data = frame.data.ok_or(MetaStreamError::MissingData)?;
                    let charge =
                        u64::try_from(data.len()).map_err(|_| MetaStreamError::InvalidLength)?;
                    let decoded = decode_wire_frame(&data)?;
                    self.stream.grant_credit(charge).await?;
                    return match decoded {
                        WireFrame::Control { call_id, payload } => {
                            let control = decode_server_control(&payload)?;
                            if control.call_id() == call_id {
                                Ok(Some(MetaIncoming::Control(control)))
                            } else {
                                Err(MetaStreamError::CallIdMismatch)
                            }
                        }
                        WireFrame::BlobChunk {
                            call_id,
                            blob_id,
                            sequence,
                            final_chunk,
                            bytes,
                        } => Ok(Some(MetaIncoming::BlobChunk {
                            call_id,
                            blob_id,
                            sequence,
                            final_chunk,
                            bytes,
                        })),
                    };
                }
                StreamKind::End => return Ok(Some(MetaIncoming::End)),
                StreamKind::Cancel => {
                    let reason = frame
                        .payload
                        .and_then(|payload| {
                            payload
                                .get("reason")
                                .and_then(Value::as_str)
                                .map(str::to_owned)
                        })
                        .unwrap_or_else(|| "provider_cancelled".to_owned());
                    return Ok(Some(MetaIncoming::Cancel { reason }));
                }
                StreamKind::Open | StreamKind::HalfClose => {
                    return Err(MetaStreamError::UnexpectedFrame);
                }
            }
        }
    }

    pub async fn half_close(&mut self) -> Result<(), MetaStreamError> {
        self.stream.half_close().await?;
        Ok(())
    }

    pub async fn cancel(&mut self, reason: impl Into<String>) -> Result<(), MetaStreamError> {
        self.stream.cancel(reason).await?;
        Ok(())
    }
}

impl std::fmt::Debug for MetaServiceStream {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("MetaServiceStream")
            .field("service", &self.service)
            .field("provider", &self.provider)
            .field("stream", &self.stream)
            .finish()
    }
}

/// One decoded provider frame.
#[derive(Clone, Debug, PartialEq)]
#[allow(clippy::large_enum_variant)] // Control is the hot path; boxing every frame is unnecessary.
pub enum MetaIncoming {
    Control(ServerControl),
    BlobChunk {
        call_id: String,
        blob_id: String,
        sequence: u32,
        final_chunk: bool,
        bytes: Vec<u8>,
    },
    End,
    Cancel {
        reason: String,
    },
}

/// Caller-to-provider control messages. Media bytes use `WireFrame::BlobChunk`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum ClientControl {
    PrepareLanguage {
        call_id: String,
        model: String,
        request: LanguageRequest,
    },
    PrepareImage {
        call_id: String,
        model: String,
        request: ImageRequest,
    },
    PrepareTranscription {
        call_id: String,
        model: String,
        request: TranscriptionRequest,
    },
    PrepareSpeech {
        call_id: String,
        model: String,
        request: SpeechRequest,
    },
    PrepareRealtime {
        call_id: String,
        model: String,
        request: RealtimeRequest,
    },
    DeclareInputBlob {
        call_id: String,
        blob_id: String,
        descriptor: MediaDescriptor,
    },
    Start {
        call_id: String,
    },
    Abort {
        call_id: String,
    },
    RealtimeAppendText {
        call_id: String,
        text: String,
    },
    RealtimeAppendAudio {
        call_id: String,
        blob_id: String,
        sequence: u32,
        descriptor: MediaDescriptor,
    },
    RealtimeCommitInput {
        call_id: String,
        item_id: String,
    },
    RealtimeRequestResponse {
        call_id: String,
    },
    RealtimeCancelResponse {
        call_id: String,
        response_id: String,
    },
    RealtimeClose {
        call_id: String,
    },
}

/// Provider-to-caller control messages. Large media stays on raw blob frames.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum ServerControl {
    Prepared {
        call_id: String,
        snapshot: PreparedCallSnapshot,
    },
    LanguageEvent {
        call_id: String,
        event: LanguageEvent,
    },
    ImageOutputStarted {
        call_id: String,
        index: u32,
        blob_id: String,
        mime_type: String,
    },
    ImageOutputFinished {
        call_id: String,
        index: u32,
        blob_id: String,
        descriptor: MediaDescriptor,
    },
    ImageFinished {
        call_id: String,
        usage: Option<TokenUsage>,
    },
    TranscriptionEvent {
        call_id: String,
        event: TranscriptionEvent,
    },
    SpeechOutputStarted {
        call_id: String,
        blob_id: String,
        mime_type: String,
    },
    SpeechOutputFinished {
        call_id: String,
        blob_id: String,
        descriptor: MediaDescriptor,
    },
    SpeechFinished {
        call_id: String,
        usage: Option<TokenUsage>,
    },
    RealtimeSessionStarted {
        call_id: String,
        session_id: String,
    },
    RealtimeSpeechStarted {
        call_id: String,
        item_id: String,
    },
    RealtimeTextDelta {
        call_id: String,
        response_id: String,
        text: String,
    },
    RealtimeTranscriptDelta {
        call_id: String,
        item_id: String,
        text: String,
        finished: bool,
    },
    RealtimeAudio {
        call_id: String,
        response_id: String,
        sequence: u32,
        blob_id: String,
        descriptor: MediaDescriptor,
    },
    RealtimeHandoffRequested {
        call_id: String,
        item_id: String,
        text: String,
    },
    RealtimeRecoverableError {
        call_id: String,
        error: AiError,
    },
    RealtimeClosed {
        call_id: String,
        reason: RealtimeCloseReason,
    },
    Failed {
        call_id: String,
        error: AiError,
    },
}

pub fn encode_client_control(value: &ClientControl) -> Result<Vec<u8>, MetaWireError> {
    value.validate()?;
    encode(value)
}

pub fn decode_client_control(bytes: &[u8]) -> Result<ClientControl, MetaWireError> {
    check_size(bytes)?;
    let value =
        serde_json::from_slice::<ClientControl>(bytes).map_err(MetaWireError::InvalidJson)?;
    value.validate()?;
    Ok(value)
}

pub fn encode_server_control(value: &ServerControl) -> Result<Vec<u8>, MetaWireError> {
    value.validate()?;
    encode(value)
}

pub fn decode_server_control(bytes: &[u8]) -> Result<ServerControl, MetaWireError> {
    check_size(bytes)?;
    let value =
        serde_json::from_slice::<ServerControl>(bytes).map_err(MetaWireError::InvalidJson)?;
    value.validate()?;
    Ok(value)
}

fn encode<T: Serialize>(value: &T) -> Result<Vec<u8>, MetaWireError> {
    let bytes = serde_json::to_vec(value).map_err(MetaWireError::InvalidJson)?;
    check_size(&bytes)?;
    Ok(bytes)
}

impl ClientControl {
    /// Correlates one caller command with its prepared or running call.
    pub fn call_id(&self) -> &str {
        match self {
            Self::PrepareLanguage { call_id, .. }
            | Self::PrepareImage { call_id, .. }
            | Self::PrepareTranscription { call_id, .. }
            | Self::PrepareSpeech { call_id, .. }
            | Self::PrepareRealtime { call_id, .. }
            | Self::DeclareInputBlob { call_id, .. }
            | Self::Start { call_id }
            | Self::Abort { call_id }
            | Self::RealtimeAppendText { call_id, .. }
            | Self::RealtimeAppendAudio { call_id, .. }
            | Self::RealtimeCommitInput { call_id, .. }
            | Self::RealtimeRequestResponse { call_id }
            | Self::RealtimeCancelResponse { call_id, .. }
            | Self::RealtimeClose { call_id } => call_id,
        }
    }

    #[allow(clippy::too_many_lines)] // One exhaustive validator owns the closed wire grammar.
    fn validate(&self) -> Result<(), MetaWireError> {
        match self {
            Self::PrepareLanguage {
                call_id,
                model,
                request,
            } => {
                ids(call_id, Some(model))?;
                request
                    .validate()
                    .map_err(|error| MetaWireError::InvalidValue(error.to_string()))
            }
            Self::PrepareImage {
                call_id,
                model,
                request,
            } => {
                ids(call_id, Some(model))?;
                request
                    .validate()
                    .map_err(|error| MetaWireError::InvalidValue(error.to_string()))
            }
            Self::PrepareTranscription {
                call_id,
                model,
                request,
            } => {
                ids(call_id, Some(model))?;
                request
                    .validate()
                    .map_err(|error| MetaWireError::InvalidValue(error.to_string()))
            }
            Self::PrepareSpeech {
                call_id,
                model,
                request,
            } => {
                ids(call_id, Some(model))?;
                request
                    .validate()
                    .map_err(|error| MetaWireError::InvalidValue(error.to_string()))
            }
            Self::PrepareRealtime {
                call_id,
                model,
                request,
            } => {
                ids(call_id, Some(model))?;
                request
                    .validate()
                    .map_err(|error| MetaWireError::InvalidValue(error.to_string()))
            }
            Self::DeclareInputBlob {
                call_id,
                blob_id,
                descriptor,
            } => {
                ids(call_id, Some(blob_id))?;
                descriptor
                    .validate()
                    .map_err(|error| MetaWireError::InvalidValue(error.to_string()))
            }
            Self::Start { call_id }
            | Self::Abort { call_id }
            | Self::RealtimeRequestResponse { call_id }
            | Self::RealtimeClose { call_id } => ids(call_id, None),
            Self::RealtimeAppendText { call_id, text } => {
                ids(call_id, None)?;
                if text.is_empty() || text.len() > 64 * 1024 || text.contains(['\0', '\u{7f}']) {
                    return Err(MetaWireError::InvalidValue(
                        "Realtime text is invalid".to_owned(),
                    ));
                }
                Ok(())
            }
            Self::RealtimeAppendAudio {
                call_id,
                blob_id,
                sequence,
                descriptor,
            } => {
                ids(call_id, Some(blob_id))?;
                if *sequence == 0 {
                    return Err(MetaWireError::InvalidValue(
                        "Realtime audio sequence must begin at one".to_owned(),
                    ));
                }
                descriptor
                    .validate()
                    .map_err(|error| MetaWireError::InvalidValue(error.to_string()))?;
                if descriptor.kind() != MediaKind::Audio {
                    return Err(MetaWireError::InvalidValue(
                        "Realtime input must be an audio descriptor".to_owned(),
                    ));
                }
                Ok(())
            }
            Self::RealtimeCommitInput { call_id, item_id } => ids(call_id, Some(item_id)),
            Self::RealtimeCancelResponse {
                call_id,
                response_id,
            } => ids(call_id, Some(response_id)),
        }
    }
}

impl ServerControl {
    /// Correlates one semantic response frame with its active prepared call.
    pub fn call_id(&self) -> &str {
        match self {
            Self::Prepared { call_id, .. }
            | Self::LanguageEvent { call_id, .. }
            | Self::ImageOutputStarted { call_id, .. }
            | Self::ImageOutputFinished { call_id, .. }
            | Self::ImageFinished { call_id, .. }
            | Self::TranscriptionEvent { call_id, .. }
            | Self::SpeechOutputStarted { call_id, .. }
            | Self::SpeechOutputFinished { call_id, .. }
            | Self::SpeechFinished { call_id, .. }
            | Self::RealtimeSessionStarted { call_id, .. }
            | Self::RealtimeSpeechStarted { call_id, .. }
            | Self::RealtimeTextDelta { call_id, .. }
            | Self::RealtimeTranscriptDelta { call_id, .. }
            | Self::RealtimeAudio { call_id, .. }
            | Self::RealtimeHandoffRequested { call_id, .. }
            | Self::RealtimeRecoverableError { call_id, .. }
            | Self::RealtimeClosed { call_id, .. }
            | Self::Failed { call_id, .. } => call_id,
        }
    }

    fn validate(&self) -> Result<(), MetaWireError> {
        let call_id = self.call_id();
        ids(call_id, None)?;
        if let Self::Prepared { snapshot, .. } = self {
            snapshot
                .validate()
                .map_err(|error| MetaWireError::InvalidValue(error.to_string()))?;
        }
        Ok(())
    }
}

fn ids(first: &str, second: Option<&String>) -> Result<(), MetaWireError> {
    for value in std::iter::once(first).chain(second.map(String::as_str)) {
        rsi_ai_protocol::validate_identifier("identifier", value)
            .map_err(MetaWireError::InvalidValue)?;
    }
    Ok(())
}

fn check_size(bytes: &[u8]) -> Result<(), MetaWireError> {
    if bytes.len() > MAX_CONTROL_FRAME_BYTES {
        Err(MetaWireError::ControlTooLarge)
    } else {
        Ok(())
    }
}

#[derive(Debug, Error)]
pub enum MetaWireError {
    #[error("control message exceeds its byte bound")]
    ControlTooLarge,
    #[error("invalid control JSON: {0}")]
    InvalidJson(serde_json::Error),
    #[error("invalid control value: {0}")]
    InvalidValue(String),
}

impl MetaWireError {
    pub const fn code(&self) -> &'static str {
        match self {
            Self::ControlTooLarge => "meta.control_too_large",
            Self::InvalidJson(_) => "meta.invalid_json",
            Self::InvalidValue(_) => "meta.invalid_value",
        }
    }
}

/// rsi-meta transport or nested rsi-ai frame failure.
#[derive(Debug, Error)]
pub enum MetaStreamError {
    #[error(transparent)]
    Host(#[from] rsi_meta::HostError),
    #[error(transparent)]
    Wire(#[from] rsi_ai_protocol::WireError),
    #[error(transparent)]
    Control(#[from] MetaWireError),
    #[error("service stream closed without END or CANCEL")]
    ClosedWithoutTerminal,
    #[error("DATA envelope did not carry raw bytes")]
    MissingData,
    #[error("DATA length cannot be represented as credit")]
    InvalidLength,
    #[error("nested control and wire call identifiers differ")]
    CallIdMismatch,
    #[error("provider emitted a caller-only stream frame")]
    UnexpectedFrame,
}
