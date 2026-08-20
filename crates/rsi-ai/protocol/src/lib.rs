//! Provider-neutral, closed, and bounded semantic contracts for `rsi-ai`.

#![deny(unsafe_code)]
#![allow(clippy::missing_errors_doc)] // Closed error types carry the machine-readable conditions.

mod error;
mod language;
mod media;
mod realtime;
mod semantic;
mod validation;
mod wire;

pub use error::{AiError, DispatchStatus, ErrorKind, ErrorPhase, sanitize_error_summary};
pub use language::{
    ContentBlock, ContentDelta, ContentStart, FinishReason, LanguageAssembler,
    LanguageAssemblyError, LanguageEvent, LanguageOutput, LanguagePartialOutput, ProviderExtension,
    Source, StreamError, TokenUsage, ToolCall, Warning,
};
pub use media::{
    ImageAssembler, ImageEvent, ImageOutput, ImageRequest, MAX_IMAGE_OUTPUTS, MediaOutput,
    SpeechAssembler, SpeechEvent, SpeechFormat, SpeechOutput, SpeechRequest,
    TranscriptionAssembler, TranscriptionEvent, TranscriptionOutput, TranscriptionRequest,
    TranscriptionSegment,
};
pub use realtime::{
    RealtimeAudioFormat, RealtimeCloseReason, RealtimeCommand, RealtimeEvent, RealtimeRequest,
    RealtimeValidator,
};
pub use semantic::{
    HostedTool, LanguageRequest, LanguageSettings, MAX_AUDIO_BYTES, MAX_BLOCKS_PER_MESSAGE,
    MAX_DESCRIPTION_BYTES, MAX_IMAGE_BYTES, MAX_IMAGE_DIMENSION, MAX_MESSAGES,
    MAX_STOP_SEQUENCE_BYTES, MAX_STOP_SEQUENCES, MediaDescriptor, MediaKind, Message,
    MessageContent, MessageRole, ReasoningEffort, ResponseFormat, SemanticError, ToolChoice,
    ToolDefinition,
};
pub use validation::identifier as validate_identifier;
pub use wire::{BlobAssembler, WireError, WireFrame, decode_wire_frame, encode_wire_frame};

/// Maximum UTF-8 bytes retained while assembling one language response.
pub const MAX_LANGUAGE_OUTPUT_BYTES: usize = 32 * 1024 * 1024;
/// Maximum content blocks retained in one language response.
pub const MAX_CONTENT_BLOCKS: usize = 256;
/// Maximum citeable sources retained in one language response.
pub const MAX_SOURCES: usize = 256;
/// Maximum warnings retained in one language response.
pub const MAX_WARNINGS: usize = 256;
/// Maximum bytes in a provider-neutral identifier.
pub const MAX_ID_BYTES: usize = 255;
/// Maximum bytes in a provider extension or replay envelope.
pub const MAX_EXTENSION_BYTES: usize = 256 * 1024;
/// Maximum UTF-8 bytes retained in one safe error summary.
pub const MAX_ERROR_SUMMARY_BYTES: usize = 4 * 1024;
/// Maximum nesting accepted in arbitrary provider-neutral JSON.
pub const MAX_JSON_DEPTH: usize = 64;
/// Maximum scalar, array, and object nodes accepted in arbitrary JSON.
pub const MAX_JSON_NODES: usize = 65_536;
/// Maximum canonical bytes in one language request.
pub const MAX_REQUEST_BYTES: usize = 16 * 1024 * 1024;
/// Maximum tools in one language request.
pub const MAX_TOOLS: usize = 128;
/// Maximum aggregate canonical bytes in tool schemas.
pub const MAX_TOOL_SCHEMA_BYTES: usize = 2 * 1024 * 1024;
/// Maximum payload bytes in one normalized JSON control frame.
pub const MAX_CONTROL_FRAME_BYTES: usize = 256 * 1024;
/// Maximum raw bytes in one normalized binary chunk.
pub const MAX_BINARY_CHUNK_BYTES: usize = 256 * 1024;
