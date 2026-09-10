use super::{FramingError, Result};
use crate::{
    DispatchStatus, ErrorKind, ImageRequest, LanguageEvent, LanguageProfile, LanguageRequest,
    MediaDescriptor, PreparedCallSnapshot, ProviderExtensionFormat, TokenUsage,
};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::Value;
use std::fmt;

/// Native provider business service contract.
pub const PROVIDER_CONTRACT: &str = "rsi.ai.portable";
/// Native provider business protocol version.
pub const PROVIDER_VERSION: u32 = 1;
/// Maximum encoded provider description or prepared response.
pub const MAXIMUM_METADATA_BYTES: usize = 256 * 1024;
/// Maximum transient native state retained with a prepared call.
pub const MAXIMUM_PREPARED_STATE_BYTES: usize = 64 * 1024;
/// Maximum declared models in either native facet.
pub const MAXIMUM_MODELS_PER_FACET: usize = 256;
/// Maximum Start-time dependency requests per native provider attempt.
pub const MAXIMUM_DEPENDENCY_REQUESTS: usize = 2048;
/// Maximum aggregate credential and Media reply bytes in one Start attempt.
pub const MAXIMUM_DEPENDENCY_BYTES: u64 = 256 * 1024 * 1024;
/// Maximum normalized stream messages admitted by one bridge attempt.
pub const MAXIMUM_STREAM_EVENTS: usize = 65_536;

/// Additional normalized Language features explicitly supported by one model.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LanguageFeature {
    /// Image inputs outside Tool results.
    InputImages,
    /// Audio inputs outside Tool results.
    InputAudio,
    /// Provider-hosted tools.
    HostedTools,
    /// Strict structured response formatting.
    StructuredOutput,
    /// Non-default generation settings.
    GenerationSettings,
}

/// Exact model declaration used before resolving effect dependencies.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LanguageModel {
    /// Exact model identifier, without aliases.
    pub model: String,
    /// Context limits, Tool dialect and history replay capabilities.
    pub profile: LanguageProfile,
    /// Unique additional supported request features.
    pub features: Vec<LanguageFeature>,
    /// Unique accepted request extension namespace/version pairs.
    pub request_extensions: Vec<ProviderExtensionFormat>,
}

/// Optional Image request features supported by one model.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ImageFeature {
    /// Existing images may be supplied as editing inputs.
    Inputs,
    /// An input mask may be supplied.
    Mask,
}

/// Exact Image model declaration with bounded count and input support.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ImageModel {
    /// Exact model identifier.
    pub model: String,
    /// Maximum requested output count, within the Image protocol bound.
    pub maximum_count: u8,
    /// Unique supported editing features.
    pub features: Vec<ImageFeature>,
}

/// Complete immutable native provider declarations, read before route publication.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Description {
    /// Exact Language model declarations.
    pub language: Vec<LanguageModel>,
    /// Exact Image model declarations.
    pub image: Vec<ImageModel>,
}

/// Frozen Language input shared unchanged by Prepare and Start.
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LanguageInput {
    /// Exact model name within the selected deployment.
    pub model: String,
    /// Complete normalized request.
    pub request: LanguageRequest,
    /// Caller-owned, redacted route and request identity.
    pub snapshot: PreparedCallSnapshot,
}

/// Frozen Image input shared unchanged by Prepare and Start.
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ImageInput {
    /// Exact model name within the selected deployment.
    pub model: String,
    /// Complete normalized request.
    pub request: ImageRequest,
    /// Caller-owned, redacted route and request identity.
    pub snapshot: PreparedCallSnapshot,
}

/// Initial request for a Portable provider call. Binary dependency replies have no JSON form.
#[derive(Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case", deny_unknown_fields)]
pub enum ControlRequest {
    /// Reads capabilities without credentials, media or provider I/O.
    Describe {},
    /// Purely prepares a Language attempt.
    PrepareLanguage { input: Box<LanguageInput> },
    /// Purely prepares an Image attempt.
    PrepareImage { input: Box<ImageInput> },
    /// Consumes one prepared Language attempt.
    StartLanguage {
        input: Box<LanguageInput>,
        state: Value,
    },
    /// Consumes one prepared Image attempt.
    StartImage {
        input: Box<ImageInput>,
        state: Value,
    },
}

/// Image stream metadata; chunk bytes are the immediately following binary packet.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum ImageHeader {
    /// Opens an output at the exact index.
    OutputStarted { index: u32, mime_type: String },
    /// Introduces one nonempty binary output chunk.
    OutputChunk { index: u32, sequence: u32 },
    /// Closes an output.
    OutputFinished { index: u32 },
    /// Sole cumulative usage value.
    Usage { usage: TokenUsage },
    /// Sole terminal Image event, followed by clean Portable terminal.
    Finished {},
}

/// Native-to-host control and normalized stream messages.
#[derive(Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case", deny_unknown_fields)]
pub enum ControlResponse {
    /// Complete capabilities returned only to Describe.
    Description { description: Box<Description> },
    /// Pure Prepare result. Snapshot changes are forbidden.
    Prepared {
        snapshot: Box<PreparedCallSnapshot>,
        state: Value,
    },
    /// Requests the already-resolved credential only during Start.
    Credential {},
    /// Requests a bounded page of one descriptor from the frozen input.
    Media {
        descriptor: MediaDescriptor,
        offset: u64,
        length: u32,
    },
    /// One normalized Language stream event.
    Language { event: Box<LanguageEvent> },
    /// One normalized Image event header.
    Image { header: ImageHeader },
    /// A bounded semantic failure; native diagnostic text never crosses this frame.
    Failed {
        kind: ErrorKind,
        dispatch: DispatchStatus,
    },
}

impl fmt::Debug for ControlRequest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Describe { .. } => "Describe",
            Self::PrepareLanguage { .. } => "PrepareLanguage(<redacted>)",
            Self::PrepareImage { .. } => "PrepareImage(<redacted>)",
            Self::StartLanguage { .. } => "StartLanguage(<redacted>)",
            Self::StartImage { .. } => "StartImage(<redacted>)",
        })
    }
}
impl fmt::Debug for ControlResponse {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Description { .. } => "Description(<redacted>)",
            Self::Prepared { .. } => "Prepared(<redacted>)",
            Self::Credential { .. } => "Credential",
            Self::Media { .. } => "Media(<redacted>)",
            Self::Language { .. } => "Language(<redacted>)",
            Self::Image { .. } => "Image(<redacted>)",
            Self::Failed { .. } => "Failed",
        })
    }
}

/// Decodes a bounded control packet while rejecting ignored nested fields.
pub fn decode_control<T: DeserializeOwned + Serialize>(bytes: &[u8]) -> Result<T> {
    if bytes.len() > super::MAXIMUM_CONTROL_BYTES {
        return Err(FramingError::Invalid);
    }
    let value: Value = serde_json::from_slice(bytes).map_err(|_| FramingError::Invalid)?;
    crate::validate_json_structure(&value).map_err(|_| FramingError::Invalid)?;
    let decoded: T = serde_json::from_slice(bytes).map_err(|_| FramingError::Invalid)?;
    let roundtrip = serde_json::to_value(&decoded).map_err(|_| FramingError::Invalid)?;
    if value != roundtrip {
        return Err(FramingError::Invalid);
    }
    Ok(decoded)
}

/// Checks a native provider's opaque transient state before keeping it in Prepared.
pub fn validate_prepared_state(state: &Value) -> Result<()> {
    crate::validate_json_structure(state).map_err(|_| FramingError::Invalid)?;
    rsi_api_protocol::measure_json(state, MAXIMUM_PREPARED_STATE_BYTES)
        .map(|_| ())
        .map_err(|_| FramingError::Invalid)
}

impl fmt::Debug for LanguageInput {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("LanguageInput(<redacted>)")
    }
}
impl fmt::Debug for ImageInput {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("ImageInput(<redacted>)")
    }
}

impl Description {
    /// Validates complete declarations before publishing any provider facet.
    pub fn validate(&self) -> Result<()> {
        use std::collections::BTreeSet;
        if self.language.len() > MAXIMUM_MODELS_PER_FACET
            || self.image.len() > MAXIMUM_MODELS_PER_FACET
        {
            return Err(FramingError::Invalid);
        }
        let mut names = BTreeSet::new();
        for model in &self.language {
            crate::validate_identifier("model", &model.model).map_err(|_| FramingError::Invalid)?;
            model
                .profile
                .validate()
                .map_err(|_| FramingError::Invalid)?;
            if !names.insert(&model.model)
                || model.features.len() > 5
                || model.features.iter().collect::<BTreeSet<_>>().len() != model.features.len()
                || model.request_extensions.len() > crate::MAX_ACCEPTED_PROVIDER_EXTENSIONS
                || model
                    .request_extensions
                    .iter()
                    .map(|e| (e.namespace(), e.version()))
                    .collect::<BTreeSet<_>>()
                    .len()
                    != model.request_extensions.len()
            {
                return Err(FramingError::Invalid);
            }
        }
        names.clear();
        for model in &self.image {
            crate::validate_identifier("model", &model.model).map_err(|_| FramingError::Invalid)?;
            if !names.insert(&model.model)
                || model.maximum_count == 0
                || model.maximum_count > crate::MAX_IMAGE_OUTPUTS
                || model.features.len() > 2
                || model.features.iter().collect::<BTreeSet<_>>().len() != model.features.len()
                || (model.features.contains(&ImageFeature::Mask)
                    && !model.features.contains(&ImageFeature::Inputs))
            {
                return Err(FramingError::Invalid);
            }
        }
        Ok(())
    }
}
