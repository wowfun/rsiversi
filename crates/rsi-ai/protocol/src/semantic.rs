use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;

use crate::{
    MAX_REQUEST_BYTES, MAX_TOOL_SCHEMA_BYTES, MAX_TOOLS, ProviderExtension, ToolCall, validation,
};

pub const MAX_MESSAGES: usize = 256;
pub const MAX_BLOCKS_PER_MESSAGE: usize = 256;
pub const MAX_DESCRIPTION_BYTES: usize = 4 * 1024;
/// Maximum UTF-8 bytes in one provider-neutral freeform tool grammar.
pub const MAX_FREEFORM_GRAMMAR_BYTES: usize = 64 * 1024;
pub const MAX_STOP_SEQUENCES: usize = 8;
pub const MAX_STOP_SEQUENCE_BYTES: usize = 1_024;
pub const MAX_IMAGE_BYTES: u64 = 32 * 1024 * 1024;
pub const MAX_AUDIO_BYTES: u64 = 128 * 1024 * 1024;
pub const MAX_IMAGE_DIMENSION: u32 = 65_535;
/// Maximum provider-extension formats one language profile may accept.
pub const MAX_ACCEPTED_PROVIDER_EXTENSIONS: usize = 64;
/// Maximum exact model-capacity profiles retained by one adapter configuration.
pub const MAX_LANGUAGE_MODEL_PROFILES: usize = 256;
const MAX_IMAGE_PIXELS: u64 = 100_000_000;

/// Provider wire family used to project caller-owned tools and rich results.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolDialect {
    /// `OpenAI` Responses function and custom-tool items.
    Responses,
    /// OpenAI-compatible Chat Completions function calls.
    ChatCompletions,
}

/// Provider wire projection for an image-bearing tool result.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ImageToolResultMode {
    /// Text and image blocks remain in one Responses function-call output.
    FunctionOutput,
    /// Tool text is followed by an adjacent user multimodal image message.
    AdjacentUserMessage,
}

/// Whether a generation-pinned language route accepts image tool results.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(
    tag = "support",
    content = "mode",
    rename_all = "snake_case",
    deny_unknown_fields
)]
pub enum ImageToolResultCapability {
    /// The route has an explicit, tested image-result projection.
    Yes(ImageToolResultMode),
    /// The route is explicitly text-only.
    No,
    /// The adapter cannot prove support and callers must omit image-result tools.
    Unknown,
}

/// Exact token capacities configured for one language-model identifier.
#[allow(clippy::struct_field_names)] // The repeated suffix is part of the explicit wire unit.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct LanguageModelLimits {
    context_window_tokens: u32,
    default_output_reserve_tokens: u32,
    max_output_reserve_tokens: u32,
}

impl<'de> Deserialize<'de> for LanguageModelLimits {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[allow(clippy::struct_field_names)]
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct WireLimits {
            context_window_tokens: u32,
            default_output_reserve_tokens: u32,
            max_output_reserve_tokens: u32,
        }

        let limits = WireLimits::deserialize(deserializer)?;
        Self::new(
            limits.context_window_tokens,
            limits.default_output_reserve_tokens,
            limits.max_output_reserve_tokens,
        )
        .map_err(serde::de::Error::custom)
    }
}

impl LanguageModelLimits {
    /// Creates one internally consistent model-capacity description.
    pub fn new(
        context_window_tokens: u32,
        default_output_reserve_tokens: u32,
        max_output_reserve_tokens: u32,
    ) -> Result<Self, SemanticError> {
        validate_language_model_limits(
            "language_model_limits",
            context_window_tokens,
            default_output_reserve_tokens,
            max_output_reserve_tokens,
        )?;
        Ok(Self {
            context_window_tokens,
            default_output_reserve_tokens,
            max_output_reserve_tokens,
        })
    }

    /// Complete input and output context capacity.
    #[must_use]
    pub const fn context_window_tokens(self) -> u32 {
        self.context_window_tokens
    }

    /// Default output capacity reserved by an orchestrator.
    #[must_use]
    pub const fn default_output_reserve_tokens(self) -> u32 {
        self.default_output_reserve_tokens
    }

    /// Largest output reserve accepted for this model.
    #[must_use]
    pub const fn max_output_reserve_tokens(self) -> u32 {
        self.max_output_reserve_tokens
    }
}

/// Bounded exact model-to-capacity facts shared by language adapters.
///
/// Missing identifiers intentionally have no inferred fallback.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct LanguageModelProfiles {
    profiles: BTreeMap<String, LanguageModelLimits>,
}

impl LanguageModelProfiles {
    /// Adds one unique, bounded printable-ASCII model identifier.
    pub fn with_profile(
        mut self,
        model: impl Into<String>,
        limits: LanguageModelLimits,
    ) -> Result<Self, SemanticError> {
        self.insert(model, limits)?;
        Ok(self)
    }

    /// Inserts one exact model profile without replacing an existing fact.
    pub fn insert(
        &mut self,
        model: impl Into<String>,
        limits: LanguageModelLimits,
    ) -> Result<(), SemanticError> {
        let model = model.into();
        validation::identifier("language_model_profiles.id", &model).map_err(|reason| {
            SemanticError::new(
                "language_model_profiles.invalid_id",
                "language_model_profiles.id",
                reason,
            )
        })?;
        if self.profiles.contains_key(&model) {
            return Err(SemanticError::new(
                "language_model_profiles.duplicate",
                "language_model_profiles.id",
                "must be unique",
            ));
        }
        if self.profiles.len() >= MAX_LANGUAGE_MODEL_PROFILES {
            return Err(SemanticError::new(
                "language_model_profiles.too_many",
                "language_model_profiles",
                format!("must contain at most {MAX_LANGUAGE_MODEL_PROFILES} entries"),
            ));
        }
        self.profiles.insert(model, limits);
        Ok(())
    }

    /// Returns the exact configured limits, or `None` when no fact is known.
    #[must_use]
    pub fn get(&self, model: &str) -> Option<LanguageModelLimits> {
        self.profiles.get(model).copied()
    }
}

/// Exact namespace and version of provider-private state accepted on replay.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ProviderExtensionFormat {
    namespace: String,
    version: u32,
}

impl<'de> Deserialize<'de> for ProviderExtensionFormat {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct WireFormat {
            namespace: String,
            version: u32,
        }

        let format = WireFormat::deserialize(deserializer)?;
        Self::new(format.namespace, format.version).map_err(serde::de::Error::custom)
    }
}

impl ProviderExtensionFormat {
    /// Creates one bounded provider-extension identity.
    pub fn new(namespace: impl Into<String>, version: u32) -> Result<Self, SemanticError> {
        let format = Self {
            namespace: namespace.into(),
            version,
        };
        format.validate()?;
        Ok(format)
    }

    pub fn namespace(&self) -> &str {
        &self.namespace
    }

    pub const fn version(&self) -> u32 {
        self.version
    }

    fn validate(&self) -> Result<(), SemanticError> {
        validation::identifier(
            "language_profile.accepted_provider_extensions.namespace",
            &self.namespace,
        )
        .map_err(|reason| {
            SemanticError::new(
                "language_profile.invalid_extension",
                "language_profile.accepted_provider_extensions.namespace",
                reason,
            )
        })
    }
}

/// Provider-I/O-free capabilities captured before one language request is built.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct LanguageProfile {
    context_window_tokens: u32,
    default_output_reserve_tokens: u32,
    max_output_reserve_tokens: u32,
    tool_dialect: ToolDialect,
    supports_freeform_tools: bool,
    image_tool_result: ImageToolResultCapability,
    accepted_provider_extensions: Vec<ProviderExtensionFormat>,
}

impl<'de> Deserialize<'de> for LanguageProfile {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[allow(clippy::struct_field_names)]
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct WireProfile {
            context_window_tokens: u32,
            default_output_reserve_tokens: u32,
            max_output_reserve_tokens: u32,
            tool_dialect: ToolDialect,
            supports_freeform_tools: bool,
            image_tool_result: ImageToolResultCapability,
            accepted_provider_extensions: Vec<ProviderExtensionFormat>,
        }

        let profile = WireProfile::deserialize(deserializer)?;
        Self::new(
            profile.context_window_tokens,
            profile.default_output_reserve_tokens,
            profile.max_output_reserve_tokens,
            profile.tool_dialect,
            profile.supports_freeform_tools,
            profile.image_tool_result,
            profile.accepted_provider_extensions,
        )
        .map_err(serde::de::Error::custom)
    }
}

impl LanguageProfile {
    /// Creates a complete generation-pinned language capability profile.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        context_window_tokens: u32,
        default_output_reserve_tokens: u32,
        max_output_reserve_tokens: u32,
        tool_dialect: ToolDialect,
        supports_freeform_tools: bool,
        image_tool_result: ImageToolResultCapability,
        accepted_provider_extensions: Vec<ProviderExtensionFormat>,
    ) -> Result<Self, SemanticError> {
        validate_language_model_limits(
            "language_profile",
            context_window_tokens,
            default_output_reserve_tokens,
            max_output_reserve_tokens,
        )?;
        let profile = Self {
            context_window_tokens,
            default_output_reserve_tokens,
            max_output_reserve_tokens,
            tool_dialect,
            supports_freeform_tools,
            image_tool_result,
            accepted_provider_extensions,
        };
        profile.validate()?;
        Ok(profile)
    }

    pub const fn context_window_tokens(&self) -> u32 {
        self.context_window_tokens
    }

    pub const fn default_output_reserve_tokens(&self) -> u32 {
        self.default_output_reserve_tokens
    }

    pub const fn max_output_reserve_tokens(&self) -> u32 {
        self.max_output_reserve_tokens
    }

    pub const fn tool_dialect(&self) -> ToolDialect {
        self.tool_dialect
    }

    pub const fn supports_freeform_tools(&self) -> bool {
        self.supports_freeform_tools
    }

    pub const fn image_tool_result(&self) -> ImageToolResultCapability {
        self.image_tool_result
    }

    pub fn accepted_provider_extensions(&self) -> &[ProviderExtensionFormat] {
        &self.accepted_provider_extensions
    }

    /// Returns whether private state may be forwarded to this exact profile.
    pub fn accepts_extension(&self, extension: &ProviderExtension) -> bool {
        self.accepted_provider_extensions.iter().any(|accepted| {
            accepted.namespace == extension.namespace && accepted.version == extension.version
        })
    }

    /// Revalidates all numeric and aggregate profile bounds after decoding.
    pub fn validate(&self) -> Result<(), SemanticError> {
        validate_language_model_limits(
            "language_profile",
            self.context_window_tokens,
            self.default_output_reserve_tokens,
            self.max_output_reserve_tokens,
        )?;
        if self.accepted_provider_extensions.len() > MAX_ACCEPTED_PROVIDER_EXTENSIONS {
            return Err(SemanticError::new(
                "language_profile.too_many_extensions",
                "language_profile.accepted_provider_extensions",
                format!("must contain at most {MAX_ACCEPTED_PROVIDER_EXTENSIONS} entries"),
            ));
        }
        let mut seen = BTreeSet::new();
        for accepted in &self.accepted_provider_extensions {
            accepted.validate()?;
            if !seen.insert((accepted.namespace.as_str(), accepted.version)) {
                return Err(SemanticError::new(
                    "language_profile.duplicate_extension",
                    "language_profile.accepted_provider_extensions",
                    "namespace/version pairs must be unique",
                ));
            }
        }
        Ok(())
    }
}

fn validate_language_model_limits(
    field: &str,
    context_window_tokens: u32,
    default_output_reserve_tokens: u32,
    max_output_reserve_tokens: u32,
) -> Result<(), SemanticError> {
    if context_window_tokens == 0
        || default_output_reserve_tokens == 0
        || default_output_reserve_tokens > max_output_reserve_tokens
        || max_output_reserve_tokens >= context_window_tokens
    {
        return Err(SemanticError::new(
            "language_profile.invalid_token_limits",
            field,
            "token limits must satisfy 0 < default reserve <= max reserve < context window",
        ));
    }
    Ok(())
}

/// Media class whose bytes travel separately from semantic JSON.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MediaKind {
    /// Still-image bytes with an `image/` MIME type.
    Image,
    /// Audio bytes with an `audio/` MIME type.
    Audio,
}

/// Locator-free identity and validated metadata for one media body.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct MediaDescriptor {
    kind: MediaKind,
    mime_type: String,
    byte_len: u64,
    sha256: String,
    width: Option<u32>,
    height: Option<u32>,
    duration_ms: Option<u64>,
}

impl<'de> Deserialize<'de> for MediaDescriptor {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct WireDescriptor {
            kind: MediaKind,
            mime_type: String,
            byte_len: u64,
            sha256: String,
            width: Option<u32>,
            height: Option<u32>,
            duration_ms: Option<u64>,
        }

        let wire = WireDescriptor::deserialize(deserializer)?;
        let descriptor = Self {
            kind: wire.kind,
            mime_type: wire.mime_type,
            byte_len: wire.byte_len,
            sha256: wire.sha256,
            width: wire.width,
            height: wire.height,
            duration_ms: wire.duration_ms,
        };
        descriptor
            .validate()
            .map(|()| descriptor)
            .map_err(serde::de::Error::custom)
    }
}

impl MediaDescriptor {
    /// Creates locator-free media identity from kind, MIME type, length, and SHA-256.
    pub fn new(
        kind: MediaKind,
        mime_type: impl Into<String>,
        byte_len: u64,
        sha256: impl Into<String>,
    ) -> Result<Self, SemanticError> {
        let descriptor = Self {
            kind,
            mime_type: mime_type.into(),
            byte_len,
            sha256: sha256.into(),
            width: None,
            height: None,
            duration_ms: None,
        };
        descriptor.validate()?;
        Ok(descriptor)
    }

    /// Adds a bounded nonzero width and height to an image descriptor.
    pub fn with_image_dimensions(mut self, width: u32, height: u32) -> Result<Self, SemanticError> {
        if self.kind != MediaKind::Image {
            return Err(SemanticError::new(
                "media.invalid_metadata",
                "media.dimensions",
                "dimensions are valid only for images",
            ));
        }
        self.width = Some(width);
        self.height = Some(height);
        self.validate()?;
        Ok(self)
    }

    /// Adds a positive duration in milliseconds to an audio descriptor.
    pub fn with_audio_duration_ms(mut self, duration_ms: u64) -> Result<Self, SemanticError> {
        if self.kind != MediaKind::Audio {
            return Err(SemanticError::new(
                "media.invalid_metadata",
                "media.duration_ms",
                "positive duration is valid only for audio",
            ));
        }
        self.duration_ms = Some(duration_ms);
        self.validate()?;
        Ok(self)
    }

    /// Returns the media class that determines validation bounds.
    pub const fn kind(&self) -> MediaKind {
        self.kind
    }

    /// Returns the validated media MIME type.
    pub fn mime_type(&self) -> &str {
        &self.mime_type
    }

    /// Returns the exact byte length of the separately transported body.
    pub const fn byte_len(&self) -> u64 {
        self.byte_len
    }

    /// Returns the lowercase hexadecimal SHA-256 of the body.
    pub fn sha256(&self) -> &str {
        &self.sha256
    }

    /// Revalidates a deserialized descriptor and kind-specific metadata.
    pub fn validate(&self) -> Result<(), SemanticError> {
        let maximum = match self.kind {
            MediaKind::Image => MAX_IMAGE_BYTES,
            MediaKind::Audio => MAX_AUDIO_BYTES,
        };
        if self.byte_len == 0 || self.byte_len > maximum {
            return Err(SemanticError::new(
                "media.invalid_length",
                "media.byte_len",
                format!("must be 1..={maximum}"),
            ));
        }
        let expected_prefix = match self.kind {
            MediaKind::Image => "image/",
            MediaKind::Audio => "audio/",
        };
        if self.mime_type.len() <= expected_prefix.len()
            || self.mime_type.len() > 127
            || !self.mime_type.starts_with(expected_prefix)
            || !self.mime_type.bytes().all(|byte| {
                byte.is_ascii_alphanumeric() || matches!(byte, b'/' | b'.' | b'+' | b'-')
            })
        {
            return Err(SemanticError::new(
                "media.invalid_mime",
                "media.mime_type",
                format!("must be a bounded {expected_prefix} MIME type"),
            ));
        }
        if self.sha256.len() != 64
            || !self
                .sha256
                .bytes()
                .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
        {
            return Err(SemanticError::new(
                "media.invalid_digest",
                "media.sha256",
                "must be 64 lowercase hexadecimal characters",
            ));
        }
        match self.kind {
            MediaKind::Image => {
                if self.duration_ms.is_some()
                    || self.width.is_some() != self.height.is_some()
                    || self.width.zip(self.height).is_some_and(|(width, height)| {
                        width == 0
                            || height == 0
                            || width > MAX_IMAGE_DIMENSION
                            || height > MAX_IMAGE_DIMENSION
                            || u64::from(width).saturating_mul(u64::from(height)) > MAX_IMAGE_PIXELS
                    })
                {
                    return Err(SemanticError::new(
                        "media.invalid_metadata",
                        "media",
                        "image metadata must contain either no dimensions or one bounded width/height pair and no duration",
                    ));
                }
            }
            MediaKind::Audio => {
                if self.width.is_some() || self.height.is_some() || self.duration_ms == Some(0) {
                    return Err(SemanticError::new(
                        "media.invalid_metadata",
                        "media",
                        "audio metadata may contain only a positive optional duration",
                    ));
                }
            }
        }
        Ok(())
    }
}

/// Role attached to one provider-neutral message.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MessageRole {
    /// Product-level instruction with highest request precedence.
    System,
    /// Application-developer instruction distinct from end-user input.
    Developer,
    /// End-user input.
    User,
    /// Prior model output.
    Assistant,
    /// Result of a model-requested tool call.
    Tool,
}

/// One rich message block.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(
    tag = "type",
    content = "content",
    rename_all = "snake_case",
    deny_unknown_fields
)]
pub enum MessageContent {
    /// Plain UTF-8 message content.
    Text {
        /// Bounded nonempty text.
        text: String,
    },
    /// Locator-free image input or prior image output.
    Image(MediaDescriptor),
    /// Locator-free audio input or prior audio output.
    Audio(MediaDescriptor),
    /// Provider-exposed reasoning and optional replay evidence.
    Reasoning {
        /// Bounded reasoning text.
        text: String,
        /// Bounded provider-private evidence needed for a later request.
        evidence: Option<ProviderExtension>,
    },
    /// Tool call previously emitted by the assistant.
    ToolCall(ToolCall),
    /// Content returned for one earlier tool call.
    ToolResult {
        /// Identifier of the corresponding assistant tool call.
        call_id: String,
        /// Bounded text, image, or audio result blocks.
        content: Vec<MessageContent>,
        /// Whether the tool execution failed semantically.
        is_error: bool,
    },
}

/// A complete provider-neutral message.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Message {
    role: MessageRole,
    content: Vec<MessageContent>,
}

impl<'de> Deserialize<'de> for Message {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct WireMessage {
            role: MessageRole,
            content: Vec<MessageContent>,
        }

        let wire = WireMessage::deserialize(deserializer)?;
        Self::new(wire.role, wire.content).map_err(serde::de::Error::custom)
    }
}

impl Message {
    /// Creates a validated system text message.
    pub fn system_text(text: impl Into<String>) -> Result<Self, SemanticError> {
        Self::new_text(MessageRole::System, text)
    }

    /// Creates a validated developer text message.
    pub fn developer_text(text: impl Into<String>) -> Result<Self, SemanticError> {
        Self::new_text(MessageRole::Developer, text)
    }

    /// Creates a validated user text message.
    pub fn user_text(text: impl Into<String>) -> Result<Self, SemanticError> {
        Self::new_text(MessageRole::User, text)
    }

    /// Creates a validated multimodal user message.
    pub fn user(content: Vec<MessageContent>) -> Result<Self, SemanticError> {
        Self::new(MessageRole::User, content)
    }

    /// Creates a validated assistant history message.
    pub fn assistant(content: Vec<MessageContent>) -> Result<Self, SemanticError> {
        Self::new(MessageRole::Assistant, content)
    }

    /// Creates the sole result block for one previous tool call.
    pub fn tool_result(
        call_id: impl Into<String>,
        content: Vec<MessageContent>,
        is_error: bool,
    ) -> Result<Self, SemanticError> {
        Self::new(
            MessageRole::Tool,
            vec![MessageContent::ToolResult {
                call_id: call_id.into(),
                content,
                is_error,
            }],
        )
    }

    /// Returns the role governing which content kinds are valid.
    pub fn role(&self) -> MessageRole {
        self.role
    }

    /// Returns ordered validated message content.
    pub fn content(&self) -> &[MessageContent] {
        &self.content
    }

    fn new_text(role: MessageRole, text: impl Into<String>) -> Result<Self, SemanticError> {
        Self::new(role, vec![MessageContent::Text { text: text.into() }])
    }

    fn new(role: MessageRole, content: Vec<MessageContent>) -> Result<Self, SemanticError> {
        let message = Self { role, content };
        message.validate()?;
        Ok(message)
    }

    /// Revalidates message bounds and role-to-content relationships.
    pub fn validate(&self) -> Result<(), SemanticError> {
        if self.content.is_empty() || self.content.len() > MAX_BLOCKS_PER_MESSAGE {
            return Err(SemanticError::new(
                "message.invalid_content",
                "message.content",
                format!("must contain 1..={MAX_BLOCKS_PER_MESSAGE} blocks"),
            ));
        }
        if self.role == MessageRole::Tool && self.content.len() != 1 {
            return Err(SemanticError::new(
                "message.invalid_content",
                "message.content",
                "a tool message must contain exactly one tool result",
            ));
        }
        for (index, block) in self.content.iter().enumerate() {
            validate_message_content(self.role, block, &format!("message.content[{index}]"))?;
        }
        Ok(())
    }
}

fn validate_message_content(
    role: MessageRole,
    block: &MessageContent,
    field: &str,
) -> Result<(), SemanticError> {
    let allowed = matches!(
        (role, block),
        (
            MessageRole::System | MessageRole::Developer,
            MessageContent::Text { .. }
        ) | (
            MessageRole::User,
            MessageContent::Text { .. } | MessageContent::Image(_) | MessageContent::Audio(_),
        ) | (
            MessageRole::Assistant,
            MessageContent::Text { .. }
                | MessageContent::Image(_)
                | MessageContent::Audio(_)
                | MessageContent::Reasoning { .. }
                | MessageContent::ToolCall(_),
        ) | (MessageRole::Tool, MessageContent::ToolResult { .. })
    );
    if !allowed {
        return Err(SemanticError::new(
            "message.invalid_content",
            field,
            "block type is not valid for this message role",
        ));
    }

    match block {
        MessageContent::Text { text } => {
            validation::safe_text(field, text, MAX_REQUEST_BYTES, false)
                .map_err(|reason| SemanticError::new("message.invalid_content", field, reason))?;
        }
        MessageContent::Reasoning { text, evidence } => {
            validate_reasoning_content(text, evidence.as_ref(), field)?;
        }
        MessageContent::Image(media) => {
            if media.kind != MediaKind::Image {
                return Err(SemanticError::new(
                    "message.invalid_content",
                    field,
                    "image block must contain image media",
                ));
            }
            media.validate()?;
        }
        MessageContent::Audio(media) => {
            if media.kind != MediaKind::Audio {
                return Err(SemanticError::new(
                    "message.invalid_content",
                    field,
                    "audio block must contain audio media",
                ));
            }
            media.validate()?;
        }
        MessageContent::ToolCall(call) => {
            validation::identifier(&format!("{field}.id"), &call.id)
                .map_err(|reason| SemanticError::new("message.invalid_content", field, reason))?;
            validation::tool_name(&format!("{field}.name"), &call.name)
                .map_err(|reason| SemanticError::new("message.invalid_content", field, reason))?;
            validation::safe_text(
                &format!("{field}.arguments"),
                &call.arguments,
                MAX_REQUEST_BYTES,
                true,
            )
            .map_err(|reason| SemanticError::new("message.invalid_content", field, reason))?;
        }
        MessageContent::ToolResult {
            call_id, content, ..
        } => {
            validation::identifier(&format!("{field}.call_id"), call_id)
                .map_err(|reason| SemanticError::new("message.invalid_content", field, reason))?;
            if content.is_empty() || content.len() > MAX_BLOCKS_PER_MESSAGE {
                return Err(SemanticError::new(
                    "message.invalid_content",
                    field,
                    "tool result content is empty or too large",
                ));
            }
            for (index, nested) in content.iter().enumerate() {
                match nested {
                    MessageContent::Text { .. }
                    | MessageContent::Image(_)
                    | MessageContent::Audio(_) => validate_message_content(
                        MessageRole::User,
                        nested,
                        &format!("{field}.content[{index}]"),
                    )?,
                    _ => {
                        return Err(SemanticError::new(
                            "message.invalid_content",
                            field,
                            "tool results may contain only text, image, or audio blocks",
                        ));
                    }
                }
            }
        }
    }
    Ok(())
}

fn validate_reasoning_content(
    text: &str,
    evidence: Option<&ProviderExtension>,
    field: &str,
) -> Result<(), SemanticError> {
    validation::safe_text(field, text, MAX_REQUEST_BYTES, false)
        .map_err(|reason| SemanticError::new("message.invalid_content", field, reason))?;
    if let Some(evidence) = evidence {
        evidence
            .validate(&format!("{field}.evidence"))
            .map_err(|error| {
                SemanticError::new("message.invalid_content", field, error.to_string())
            })?;
    }
    Ok(())
}

/// Provider-neutral tool declaration with a JSON function schema and optional
/// freeform projection for providers that support custom grammar tools.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ToolDefinition {
    name: String,
    description: String,
    input_schema: Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    freeform: Option<FreeformToolDefinition>,
}

impl<'de> Deserialize<'de> for ToolDefinition {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct WireTool {
            name: String,
            description: String,
            input_schema: Value,
            #[serde(default)]
            freeform: Option<FreeformToolDefinition>,
        }

        let wire = WireTool::deserialize(deserializer)?;
        let tool = Self {
            name: wire.name,
            description: wire.description,
            input_schema: wire.input_schema,
            freeform: wire.freeform,
        };
        tool.validate()
            .map(|()| tool)
            .map_err(serde::de::Error::custom)
    }
}

/// Bounded provider-neutral freeform grammar attached to one tool.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct FreeformToolDefinition {
    format: FreeformFormat,
    grammar: String,
}

impl<'de> Deserialize<'de> for FreeformToolDefinition {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct WireFreeform {
            format: FreeformFormat,
            grammar: String,
        }

        let wire = WireFreeform::deserialize(deserializer)?;
        Self::new(wire.format, wire.grammar).map_err(serde::de::Error::custom)
    }
}

/// Closed freeform grammar families supported by semantic adapters.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FreeformFormat {
    /// Lark grammar used by `OpenAI` Responses custom tools.
    Lark,
}

impl FreeformToolDefinition {
    /// Creates a bounded freeform grammar definition.
    pub fn new(format: FreeformFormat, grammar: impl Into<String>) -> Result<Self, SemanticError> {
        let definition = Self {
            format,
            grammar: grammar.into(),
        };
        definition.validate()?;
        Ok(definition)
    }

    pub const fn format(&self) -> FreeformFormat {
        self.format
    }

    pub fn grammar(&self) -> &str {
        &self.grammar
    }

    fn validate(&self) -> Result<(), SemanticError> {
        validation::safe_text(
            "tool.freeform.grammar",
            &self.grammar,
            MAX_FREEFORM_GRAMMAR_BYTES,
            false,
        )
        .map_err(|reason| {
            SemanticError::new("tool.invalid_freeform", "tool.freeform.grammar", reason)
        })
    }
}

impl ToolDefinition {
    /// Creates a named function tool with a bounded JSON Schema input contract.
    pub fn new(
        name: impl Into<String>,
        description: impl Into<String>,
        input_schema: Value,
    ) -> Result<Self, SemanticError> {
        let tool = Self {
            name: name.into(),
            description: description.into(),
            input_schema,
            freeform: None,
        };
        tool.validate()?;
        Ok(tool)
    }

    /// Returns the exact tool name exposed to the model.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Returns the model-visible tool description.
    pub fn description(&self) -> &str {
        &self.description
    }

    /// Returns the bounded object or boolean JSON Schema for tool arguments.
    pub fn input_schema(&self) -> &Value {
        &self.input_schema
    }

    /// Selects a provider-neutral freeform projection for capable adapters.
    pub fn with_freeform(
        mut self,
        freeform: FreeformToolDefinition,
    ) -> Result<Self, SemanticError> {
        self.freeform = Some(freeform);
        self.validate()?;
        Ok(self)
    }

    pub const fn freeform(&self) -> Option<&FreeformToolDefinition> {
        self.freeform.as_ref()
    }

    fn validate(&self) -> Result<(), SemanticError> {
        validation::tool_name("tool.name", &self.name)
            .map_err(|reason| SemanticError::new("tool.invalid_name", "tool.name", reason))?;
        validation::safe_text(
            "tool.description",
            &self.description,
            MAX_DESCRIPTION_BYTES,
            true,
        )
        .map_err(|reason| {
            SemanticError::new("tool.invalid_description", "tool.description", reason)
        })?;
        if !matches!(self.input_schema, Value::Object(_) | Value::Bool(_)) {
            return Err(SemanticError::new(
                "tool.invalid_schema",
                "tool.input_schema",
                "must be an object or boolean JSON Schema",
            ));
        }
        validate_json("tool.input_schema", &self.input_schema)?;
        if let Some(freeform) = &self.freeform {
            freeform.validate()?;
        }
        Ok(())
    }
}

/// Function-tool selection policy.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(
    tag = "type",
    content = "name",
    rename_all = "snake_case",
    deny_unknown_fields
)]
pub enum ToolChoice {
    /// Lets the model decide whether and which tool to call.
    Auto,
    /// Forbids function-tool calls.
    None,
    /// Requires at least one function-tool call.
    Required,
    /// Requires the named declared function tool.
    Specific(String),
}

/// A provider-executed tool with shared cross-provider semantics.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum HostedTool {
    /// Requests provider-hosted web search.
    WebSearch {
        /// Optional provider-neutral maximum searches for the operation.
        max_uses: Option<u8>,
    },
}

/// Requested language output representation.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum ResponseFormat {
    /// Ordinary text or tool-call output.
    Text,
    /// Strict output constrained by a caller-provided JSON Schema.
    JsonSchema {
        name: String,
        description: Option<String>,
        schema: Value,
        /// Must remain true at every untrusted boundary.
        strict: bool,
    },
}

impl<'de> Deserialize<'de> for ResponseFormat {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
        enum WireFormat {
            Text,
            JsonSchema {
                name: String,
                description: Option<String>,
                schema: Value,
                strict: bool,
            },
        }

        let format = match WireFormat::deserialize(deserializer)? {
            WireFormat::Text => Self::Text,
            WireFormat::JsonSchema {
                name,
                description,
                schema,
                strict,
            } => Self::JsonSchema {
                name,
                description,
                schema,
                strict,
            },
        };
        format
            .validate()
            .map(|()| format)
            .map_err(serde::de::Error::custom)
    }
}

/// Provider-neutral reasoning effort requested by a language call.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReasoningEffort {
    /// Smallest provider-supported reasoning budget.
    Minimal,
    /// Low reasoning budget.
    Low,
    /// Provider-default medium reasoning budget.
    Medium,
    /// High reasoning budget.
    High,
    /// Highest extended reasoning budget.
    Xhigh,
}

/// Optional generation controls. Unsupported controls must be rejected by the
/// selected adapter during Prepare, never silently omitted.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct LanguageSettings {
    max_output_tokens: Option<u32>,
    temperature: Option<f64>,
    top_p: Option<f64>,
    seed: Option<i64>,
    stop: Vec<String>,
    reasoning_effort: Option<ReasoningEffort>,
}

impl<'de> Deserialize<'de> for LanguageSettings {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Default, Deserialize)]
        #[serde(default, deny_unknown_fields)]
        struct WireSettings {
            max_output_tokens: Option<u32>,
            temperature: Option<f64>,
            top_p: Option<f64>,
            seed: Option<i64>,
            stop: Vec<String>,
            reasoning_effort: Option<ReasoningEffort>,
        }

        let wire = WireSettings::deserialize(deserializer)?;
        let settings = Self {
            max_output_tokens: wire.max_output_tokens,
            temperature: wire.temperature,
            top_p: wire.top_p,
            seed: wire.seed,
            stop: wire.stop,
            reasoning_effort: wire.reasoning_effort,
        };
        settings
            .validate()
            .map(|()| settings)
            .map_err(serde::de::Error::custom)
    }
}

impl LanguageSettings {
    /// Sets a positive bounded output-token limit.
    pub fn with_max_output_tokens(mut self, value: u32) -> Result<Self, SemanticError> {
        self.max_output_tokens = Some(value);
        self.validate()?;
        Ok(self)
    }

    /// Sets finite temperature and nucleus-sampling controls.
    pub fn with_sampling(
        mut self,
        temperature: Option<f64>,
        top_p: Option<f64>,
    ) -> Result<Self, SemanticError> {
        self.temperature = temperature;
        self.top_p = top_p;
        self.validate()?;
        Ok(self)
    }

    #[must_use]
    /// Sets the deterministic sampling seed when supported by the adapter.
    pub const fn with_seed(mut self, seed: i64) -> Self {
        self.seed = Some(seed);
        self
    }

    /// Sets bounded nonempty stop sequences.
    pub fn with_stop(mut self, stop: Vec<String>) -> Result<Self, SemanticError> {
        self.stop = stop;
        self.validate()?;
        Ok(self)
    }

    #[must_use]
    /// Sets requested reasoning effort when supported by the adapter.
    pub const fn with_reasoning_effort(mut self, effort: ReasoningEffort) -> Self {
        self.reasoning_effort = Some(effort);
        self
    }

    pub const fn max_output_tokens(&self) -> Option<u32> {
        self.max_output_tokens
    }

    pub const fn temperature(&self) -> Option<f64> {
        self.temperature
    }

    pub const fn top_p(&self) -> Option<f64> {
        self.top_p
    }

    pub const fn seed(&self) -> Option<i64> {
        self.seed
    }

    pub fn stop(&self) -> &[String] {
        &self.stop
    }

    pub const fn reasoning_effort(&self) -> Option<ReasoningEffort> {
        self.reasoning_effort
    }

    /// Revalidates all optional generation controls and aggregate stop bounds.
    pub fn validate(&self) -> Result<(), SemanticError> {
        if self.max_output_tokens == Some(0)
            || self
                .max_output_tokens
                .is_some_and(|value| value > 1_000_000)
            || self
                .temperature
                .is_some_and(|value| !value.is_finite() || !(0.0..=2.0).contains(&value))
            || self
                .top_p
                .is_some_and(|value| !value.is_finite() || !(0.0..=1.0).contains(&value))
            || self.stop.len() > MAX_STOP_SEQUENCES
            || self.stop.iter().any(|value| {
                value.is_empty()
                    || value.len() > MAX_STOP_SEQUENCE_BYTES
                    || value.contains(['\0', '\u{7f}'])
            })
        {
            return Err(SemanticError::new(
                "request.invalid_settings",
                "settings",
                "generation settings exceed their finite token, sampling, or stop bounds",
            ));
        }
        Ok(())
    }
}

impl ResponseFormat {
    /// Creates a strict bounded JSON Schema response format.
    pub fn json_schema(
        name: impl Into<String>,
        description: Option<String>,
        schema: Value,
    ) -> Result<Self, SemanticError> {
        let name = name.into();
        validation::tool_name("response_format.name", &name).map_err(|reason| {
            SemanticError::new(
                "response_format.invalid_name",
                "response_format.name",
                reason,
            )
        })?;
        if let Some(description) = &description {
            validation::safe_text(
                "response_format.description",
                description,
                MAX_DESCRIPTION_BYTES,
                true,
            )
            .map_err(|reason| {
                SemanticError::new(
                    "response_format.invalid_description",
                    "response_format.description",
                    reason,
                )
            })?;
        }
        if !matches!(schema, Value::Object(_) | Value::Bool(_)) {
            return Err(SemanticError::new(
                "response_format.invalid_schema",
                "response_format.schema",
                "must be an object or boolean JSON Schema",
            ));
        }
        validate_json("response_format.schema", &schema)?;
        Ok(Self::JsonSchema {
            name,
            description,
            schema,
            strict: true,
        })
    }

    fn validate(&self) -> Result<(), SemanticError> {
        match self {
            Self::Text => Ok(()),
            Self::JsonSchema {
                name,
                description,
                schema,
                strict,
            } => {
                if !strict {
                    return Err(SemanticError::new(
                        "response_format.invalid_strict",
                        "response_format.strict",
                        "JSON Schema response format must be strict",
                    ));
                }
                validation::tool_name("response_format.name", name).map_err(|reason| {
                    SemanticError::new(
                        "response_format.invalid_name",
                        "response_format.name",
                        reason,
                    )
                })?;
                if let Some(description) = description {
                    validation::safe_text(
                        "response_format.description",
                        description,
                        MAX_DESCRIPTION_BYTES,
                        true,
                    )
                    .map_err(|reason| {
                        SemanticError::new(
                            "response_format.invalid_description",
                            "response_format.description",
                            reason,
                        )
                    })?;
                }
                if !matches!(schema, Value::Object(_) | Value::Bool(_)) {
                    return Err(SemanticError::new(
                        "response_format.invalid_schema",
                        "response_format.schema",
                        "must be an object or boolean JSON Schema",
                    ));
                }
                validate_json("response_format.schema", schema)
            }
        }
    }
}

/// One validated language request; model/provider selection lives outside it.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct LanguageRequest {
    messages: Vec<Message>,
    tools: Vec<ToolDefinition>,
    tool_choice: ToolChoice,
    hosted_tools: Vec<HostedTool>,
    response_format: ResponseFormat,
    settings: LanguageSettings,
    extensions: Vec<ProviderExtension>,
}

impl<'de> Deserialize<'de> for LanguageRequest {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct WireRequest {
            messages: Vec<Message>,
            tools: Vec<ToolDefinition>,
            tool_choice: ToolChoice,
            hosted_tools: Vec<HostedTool>,
            response_format: ResponseFormat,
            settings: LanguageSettings,
            extensions: Vec<ProviderExtension>,
        }

        let wire = WireRequest::deserialize(deserializer)?;
        let request = Self {
            messages: wire.messages,
            tools: wire.tools,
            tool_choice: wire.tool_choice,
            hosted_tools: wire.hosted_tools,
            response_format: wire.response_format,
            settings: wire.settings,
            extensions: wire.extensions,
        };
        request
            .validate()
            .map(|()| request)
            .map_err(serde::de::Error::custom)
    }
}

impl LanguageRequest {
    /// Creates a request from one or more validated messages with default controls.
    pub fn new(messages: Vec<Message>) -> Result<Self, SemanticError> {
        let request = Self {
            messages,
            tools: Vec::new(),
            tool_choice: ToolChoice::Auto,
            hosted_tools: Vec::new(),
            response_format: ResponseFormat::Text,
            settings: LanguageSettings::default(),
            extensions: Vec::new(),
        };
        request.validate()?;
        Ok(request)
    }

    /// Adds function tools and a selection policy that must reference them consistently.
    pub fn with_tools(
        mut self,
        tools: Vec<ToolDefinition>,
        tool_choice: ToolChoice,
    ) -> Result<Self, SemanticError> {
        self.tools = tools;
        self.tool_choice = tool_choice;
        self.validate()?;
        Ok(self)
    }

    /// Adds bounded provider-hosted tools with shared semantics.
    pub fn with_hosted_tools(
        mut self,
        hosted_tools: Vec<HostedTool>,
    ) -> Result<Self, SemanticError> {
        self.hosted_tools = hosted_tools;
        self.validate()?;
        Ok(self)
    }

    /// Selects text or strict structured output.
    pub fn with_response_format(
        mut self,
        response_format: ResponseFormat,
    ) -> Result<Self, SemanticError> {
        self.response_format = response_format;
        self.validate()?;
        Ok(self)
    }

    /// Adds bounded provider-private request extensions.
    pub fn with_extensions(
        mut self,
        extensions: Vec<ProviderExtension>,
    ) -> Result<Self, SemanticError> {
        self.extensions = extensions;
        self.validate()?;
        Ok(self)
    }

    /// Replaces the optional generation controls.
    pub fn with_settings(mut self, settings: LanguageSettings) -> Result<Self, SemanticError> {
        self.settings = settings;
        self.validate()?;
        Ok(self)
    }

    pub fn messages(&self) -> &[Message] {
        &self.messages
    }

    /// Returns caller-executed function tools exposed to the model.
    pub fn tools(&self) -> &[ToolDefinition] {
        &self.tools
    }

    pub fn tool_choice(&self) -> &ToolChoice {
        &self.tool_choice
    }

    /// Returns provider-executed tools requested for the operation.
    pub fn hosted_tools(&self) -> &[HostedTool] {
        &self.hosted_tools
    }

    pub fn response_format(&self) -> &ResponseFormat {
        &self.response_format
    }

    pub const fn settings(&self) -> &LanguageSettings {
        &self.settings
    }

    pub fn extensions(&self) -> &[ProviderExtension] {
        &self.extensions
    }

    /// Validates and returns deterministic canonical JSON bytes for identity and persistence.
    pub fn canonical_bytes(&self) -> Result<Vec<u8>, SemanticError> {
        self.validate()?;
        self.canonical_bytes_unchecked()
    }

    /// Revalidates deserialized request structure, relationships, and aggregate bounds.
    pub fn validate(&self) -> Result<(), SemanticError> {
        if self.messages.is_empty() || self.messages.len() > MAX_MESSAGES {
            return Err(SemanticError::new(
                "request.invalid_messages",
                "messages",
                format!("must contain 1..={MAX_MESSAGES} messages"),
            ));
        }
        for message in &self.messages {
            message.validate()?;
        }
        let mut calls = BTreeSet::new();
        let mut results = BTreeSet::new();
        for message in &self.messages {
            for block in &message.content {
                match block {
                    MessageContent::ToolCall(call) => {
                        if !calls.insert(call.id.as_str()) {
                            return Err(SemanticError::new(
                                "request.duplicate_tool_call",
                                "messages",
                                format!("tool call id {} is not conversation-wide unique", call.id),
                            ));
                        }
                    }
                    MessageContent::ToolResult { call_id, .. } => {
                        if !calls.contains(call_id.as_str()) {
                            return Err(SemanticError::new(
                                "request.orphan_tool_result",
                                "messages",
                                format!(
                                    "tool result {call_id} has no earlier retained assistant call"
                                ),
                            ));
                        }
                        if !results.insert(call_id.as_str()) {
                            return Err(SemanticError::new(
                                "request.duplicate_tool_result",
                                "messages",
                                format!("tool call id {call_id} has more than one result"),
                            ));
                        }
                    }
                    MessageContent::Text { .. }
                    | MessageContent::Image(_)
                    | MessageContent::Audio(_)
                    | MessageContent::Reasoning { .. } => {}
                }
            }
        }
        self.settings.validate()?;
        validate_tools(&self.tools, &self.tool_choice)?;
        validate_hosted_tools(&self.hosted_tools)?;
        self.response_format.validate()?;
        validate_extensions(&self.extensions)?;
        self.validate_aggregate_size()
    }

    fn validate_aggregate_size(&self) -> Result<(), SemanticError> {
        let encoded = validation::encoded_len(self)
            .map_err(|reason| SemanticError::new("request.encoding", "request", reason))?;
        if encoded > MAX_REQUEST_BYTES {
            return Err(SemanticError::new(
                "request.too_large",
                "request",
                format!("canonical encoding exceeds {MAX_REQUEST_BYTES} bytes"),
            ));
        }
        Ok(())
    }

    fn canonical_bytes_unchecked(&self) -> Result<Vec<u8>, SemanticError> {
        let value = serde_json::to_value(self).map_err(|error| {
            SemanticError::new("request.encoding", "request", error.to_string())
        })?;
        let canonical =
            validation::canonical_json(value).map_err(|reason| json_error("request", reason))?;
        let bytes = serde_json::to_vec(&canonical).map_err(|error| {
            SemanticError::new("request.encoding", "request", error.to_string())
        })?;
        if bytes.len() > MAX_REQUEST_BYTES {
            return Err(SemanticError::new(
                "request.too_large",
                "request",
                format!("canonical encoding exceeds {MAX_REQUEST_BYTES} bytes"),
            ));
        }
        Ok(bytes)
    }
}

fn validate_tools(tools: &[ToolDefinition], tool_choice: &ToolChoice) -> Result<(), SemanticError> {
    if tools.len() > MAX_TOOLS {
        return Err(SemanticError::new(
            "request.too_many_tools",
            "tools",
            format!("contains more than {MAX_TOOLS} tools"),
        ));
    }
    let mut names = BTreeSet::new();
    let mut schema_bytes = 0usize;
    for tool in tools {
        tool.validate()?;
        if !names.insert(tool.name.as_str()) {
            return Err(SemanticError::new(
                "request.duplicate_tool",
                "tools",
                "contains duplicate tool names",
            ));
        }
        schema_bytes = schema_bytes.saturating_add(
            validation::encoded_len(&tool.input_schema)
                .map_err(|reason| SemanticError::new("tool.invalid_schema", "tools", reason))?,
        );
        if let Some(freeform) = &tool.freeform {
            schema_bytes =
                schema_bytes.saturating_add(validation::encoded_len(&freeform.grammar).map_err(
                    |reason| SemanticError::new("tool.invalid_freeform", "tools", reason),
                )?);
        }
    }
    if schema_bytes > MAX_TOOL_SCHEMA_BYTES {
        return Err(SemanticError::new(
            "request.tool_schemas_too_large",
            "tools",
            format!("schemas exceed {MAX_TOOL_SCHEMA_BYTES} encoded bytes"),
        ));
    }
    match tool_choice {
        ToolChoice::Specific(name) => {
            validation::tool_name("tool_choice.name", name).map_err(|reason| {
                SemanticError::new("request.invalid_tool_choice", "tool_choice", reason)
            })?;
            if !names.contains(name.as_str()) {
                return Err(SemanticError::new(
                    "request.invalid_tool_choice",
                    "tool_choice",
                    "specific tool is not declared",
                ));
            }
        }
        ToolChoice::Required if tools.is_empty() => {
            return Err(SemanticError::new(
                "request.invalid_tool_choice",
                "tool_choice",
                "required tool choice needs at least one declared tool",
            ));
        }
        ToolChoice::Auto | ToolChoice::None | ToolChoice::Required => {}
    }
    Ok(())
}

fn validate_hosted_tools(hosted_tools: &[HostedTool]) -> Result<(), SemanticError> {
    for hosted in hosted_tools {
        match hosted {
            HostedTool::WebSearch {
                max_uses: Some(0 | 17..=u8::MAX),
            } => {
                return Err(SemanticError::new(
                    "request.invalid_hosted_tool",
                    "hosted_tools",
                    "web search max_uses must be 1..=16",
                ));
            }
            HostedTool::WebSearch { .. } => {}
        }
    }
    Ok(())
}

fn validate_extensions(extensions: &[ProviderExtension]) -> Result<(), SemanticError> {
    let mut namespaces = BTreeSet::new();
    for extension in extensions {
        extension.validate("request.extensions").map_err(|error| {
            SemanticError::new("request.invalid_extension", "extensions", error.to_string())
        })?;
        if !namespaces.insert(extension.namespace.as_str()) {
            return Err(SemanticError::new(
                "request.duplicate_extension",
                "extensions",
                "contains duplicate extension namespaces",
            ));
        }
    }
    Ok(())
}

fn validate_json(field: &str, value: &Value) -> Result<(), SemanticError> {
    validation::validate_json(value).map_err(|reason| json_error(field, reason))
}

fn json_error(field: &str, reason: validation::JsonValidationError) -> SemanticError {
    let code = match reason {
        validation::JsonValidationError::TooDeep => "json.too_deep",
        validation::JsonValidationError::TooManyNodes => "json.too_many_nodes",
    };
    SemanticError::new(code, field, reason.to_string())
}

/// Rejection at a provider-neutral semantic constructor.
#[derive(Clone, Debug, Eq, Error, PartialEq)]
#[error("field `{field}` {reason}")]
pub struct SemanticError {
    code: &'static str,
    field: String,
    reason: String,
}

impl SemanticError {
    fn new(code: &'static str, field: impl Into<String>, reason: impl Into<String>) -> Self {
        Self {
            code,
            field: field.into(),
            reason: reason.into(),
        }
    }

    /// Returns the stable machine-readable semantic failure code.
    pub const fn code(&self) -> &'static str {
        self.code
    }

    /// Returns the request field path responsible for rejection.
    pub fn field(&self) -> &str {
        &self.field
    }

    /// Returns the stable human-readable constraint that was violated.
    pub fn reason(&self) -> &str {
        &self.reason
    }
}
