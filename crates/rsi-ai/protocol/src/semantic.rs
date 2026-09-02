use std::collections::{BTreeMap, BTreeSet};

use rsi_tools_protocol::ToolDefinition;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;

use crate::{
    MAX_REQUEST_BYTES, MAX_TOOL_SCHEMA_BYTES, MAX_TOOLS, MediaDescriptor, MediaKind,
    ProviderExtension, ToolCall, validation,
};

pub const MAX_MESSAGES: usize = 256;
pub const MAX_BLOCKS_PER_MESSAGE: usize = 256;
pub const MAX_DESCRIPTION_BYTES: usize = 4 * 1024;
pub const MAX_STOP_SEQUENCES: usize = 8;
pub const MAX_STOP_SEQUENCE_BYTES: usize = 1_024;
/// Maximum media descriptor occurrences retained by one language request.
pub const MAX_LANGUAGE_MEDIA_OCCURRENCES: usize = 256;
/// Maximum declared raw bytes across all media occurrences in one language request.
pub const MAX_LANGUAGE_MEDIA_BYTES: u64 = 256 * 1024 * 1024;
/// Maximum provider-extension formats one language profile may accept.
pub const MAX_ACCEPTED_PROVIDER_EXTENSIONS: usize = 64;
/// Maximum exact model-capacity profiles retained by one adapter configuration.
pub const MAX_LANGUAGE_MODEL_PROFILES: usize = 256;

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
            accepted.namespace == extension.namespace() && accepted.version == extension.version()
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
    if !message_content_allowed(role, block) {
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
        MessageContent::Reasoning { text, .. } => {
            validate_reasoning_content(text, field)?;
        }
        MessageContent::Image(media) => {
            if media.kind() != MediaKind::Image {
                return Err(SemanticError::new(
                    "message.invalid_content",
                    field,
                    "image block must contain image media",
                ));
            }
            media.validate().map_err(|error| {
                SemanticError::new("message.invalid_content", field, error.to_string())
            })?;
        }
        MessageContent::Audio(media) => {
            if media.kind() != MediaKind::Audio {
                return Err(SemanticError::new(
                    "message.invalid_content",
                    field,
                    "audio block must contain audio media",
                ));
            }
            media.validate().map_err(|error| {
                SemanticError::new("message.invalid_content", field, error.to_string())
            })?;
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

fn message_content_allowed(role: MessageRole, block: &MessageContent) -> bool {
    matches!(
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
    )
}

fn validate_reasoning_content(text: &str, field: &str) -> Result<(), SemanticError> {
    validation::safe_text(field, text, MAX_REQUEST_BYTES, false)
        .map_err(|reason| SemanticError::new("message.invalid_content", field, reason))
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

    /// Returns deterministic canonical JSON bytes for identity and persistence.
    ///
    /// Construction and deserialization establish this closed type's invariant;
    /// callers may invoke [`Self::validate`] explicitly when they need a separate check.
    pub fn canonical_bytes(&self) -> Result<Vec<u8>, SemanticError> {
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
        validate_language_media(&self.messages)?;
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

fn validate_language_media(messages: &[Message]) -> Result<(), SemanticError> {
    fn add_block(
        block: &MessageContent,
        occurrences: &mut usize,
        declared_bytes: &mut u64,
    ) -> Result<(), SemanticError> {
        match block {
            MessageContent::Image(media) | MessageContent::Audio(media) => {
                *occurrences = occurrences.checked_add(1).ok_or_else(|| {
                    SemanticError::new(
                        "request.too_many_media",
                        "messages",
                        "media occurrence count overflowed",
                    )
                })?;
                if *occurrences > MAX_LANGUAGE_MEDIA_OCCURRENCES {
                    return Err(SemanticError::new(
                        "request.too_many_media",
                        "messages",
                        format!(
                            "must contain at most {MAX_LANGUAGE_MEDIA_OCCURRENCES} media occurrences"
                        ),
                    ));
                }
                *declared_bytes =
                    declared_bytes
                        .checked_add(media.byte_len())
                        .ok_or_else(|| {
                            SemanticError::new(
                                "request.media_bytes_exceeded",
                                "messages",
                                "declared media byte total overflowed",
                            )
                        })?;
                if *declared_bytes > MAX_LANGUAGE_MEDIA_BYTES {
                    return Err(SemanticError::new(
                        "request.media_bytes_exceeded",
                        "messages",
                        format!(
                            "declared media exceeds {MAX_LANGUAGE_MEDIA_BYTES} aggregate bytes"
                        ),
                    ));
                }
            }
            MessageContent::ToolResult { content, .. } => {
                for nested in content {
                    add_block(nested, occurrences, declared_bytes)?;
                }
            }
            MessageContent::Text { .. }
            | MessageContent::Reasoning { .. }
            | MessageContent::ToolCall(_) => {}
        }
        Ok(())
    }

    let mut occurrences = 0usize;
    let mut declared_bytes = 0u64;
    for message in messages {
        for block in &message.content {
            add_block(block, &mut occurrences, &mut declared_bytes)?;
        }
    }
    Ok(())
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
        tool.validate().map_err(|error| {
            SemanticError::new("tool.invalid_definition", "tools", error.to_string())
        })?;
        validation::tool_name("tools.name", tool.name())
            .map_err(|reason| SemanticError::new("tool.invalid_name", "tools", reason))?;
        validate_json("tools.input_schema", tool.input_schema())?;
        if !names.insert(tool.name()) {
            return Err(SemanticError::new(
                "request.duplicate_tool",
                "tools",
                "contains duplicate tool names",
            ));
        }
        schema_bytes =
            schema_bytes
                .saturating_add(validation::encoded_len(tool.input_schema()).map_err(
                    |reason| SemanticError::new("tool.invalid_schema", "tools", reason),
                )?);
        if let Some(freeform) = tool.freeform() {
            schema_bytes =
                schema_bytes.saturating_add(validation::encoded_len(freeform.grammar()).map_err(
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
        if !namespaces.insert(extension.namespace()) {
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
    validation::validate_json_structure(value).map_err(|reason| json_error(field, reason))
}

fn json_error(field: &str, reason: validation::JsonStructureError) -> SemanticError {
    let code = match reason {
        validation::JsonStructureError::TooDeep => "json.too_deep",
        validation::JsonStructureError::TooManyNodes => "json.too_many_nodes",
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
