use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;

use crate::{
    MAX_REQUEST_BYTES, MAX_TOOL_SCHEMA_BYTES, MAX_TOOLS, ProviderExtension, ToolCall, validation,
};

pub const MAX_MESSAGES: usize = 256;
pub const MAX_BLOCKS_PER_MESSAGE: usize = 256;
pub const MAX_DESCRIPTION_BYTES: usize = 4 * 1024;
pub const MAX_STOP_SEQUENCES: usize = 8;
pub const MAX_STOP_SEQUENCE_BYTES: usize = 1_024;
pub const MAX_IMAGE_BYTES: u64 = 32 * 1024 * 1024;
pub const MAX_AUDIO_BYTES: u64 = 128 * 1024 * 1024;
pub const MAX_IMAGE_DIMENSION: u32 = 65_535;
const MAX_IMAGE_PIXELS: u64 = 100_000_000;

/// Media class whose bytes travel separately from semantic JSON.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MediaKind {
    Image,
    Audio,
}

/// Locator-free identity and validated metadata for one media body.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
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

impl MediaDescriptor {
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

    pub const fn kind(&self) -> MediaKind {
        self.kind
    }

    pub fn mime_type(&self) -> &str {
        &self.mime_type
    }

    pub const fn byte_len(&self) -> u64 {
        self.byte_len
    }

    pub fn sha256(&self) -> &str {
        &self.sha256
    }

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
        if self.mime_type.len() > 127
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
    System,
    Developer,
    User,
    Assistant,
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
    Text {
        text: String,
    },
    Image(MediaDescriptor),
    Audio(MediaDescriptor),
    Reasoning {
        text: String,
        evidence: Option<ProviderExtension>,
    },
    ToolCall(ToolCall),
    ToolResult {
        call_id: String,
        content: Vec<MessageContent>,
        is_error: bool,
    },
}

/// A complete provider-neutral message.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Message {
    role: MessageRole,
    content: Vec<MessageContent>,
}

impl Message {
    pub fn system_text(text: impl Into<String>) -> Result<Self, SemanticError> {
        Self::new_text(MessageRole::System, text)
    }

    pub fn developer_text(text: impl Into<String>) -> Result<Self, SemanticError> {
        Self::new_text(MessageRole::Developer, text)
    }

    pub fn user_text(text: impl Into<String>) -> Result<Self, SemanticError> {
        Self::new_text(MessageRole::User, text)
    }

    pub fn user(content: Vec<MessageContent>) -> Result<Self, SemanticError> {
        Self::new(MessageRole::User, content)
    }

    pub fn assistant(content: Vec<MessageContent>) -> Result<Self, SemanticError> {
        Self::new(MessageRole::Assistant, content)
    }

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

    pub fn role(&self) -> MessageRole {
        self.role
    }

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

    pub fn validate(&self) -> Result<(), SemanticError> {
        if self.content.is_empty() || self.content.len() > MAX_BLOCKS_PER_MESSAGE {
            return Err(SemanticError::new(
                "message.invalid_content",
                "message.content",
                format!("must contain 1..={MAX_BLOCKS_PER_MESSAGE} blocks"),
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
        MessageContent::Text { text } | MessageContent::Reasoning { text, .. } => {
            validation::safe_text(field, text, MAX_REQUEST_BYTES, false)
                .map_err(|reason| SemanticError::new("message.invalid_content", field, reason))?;
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

/// Provider-neutral function tool declaration.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ToolDefinition {
    name: String,
    description: String,
    input_schema: Value,
}

impl ToolDefinition {
    pub fn new(
        name: impl Into<String>,
        description: impl Into<String>,
        input_schema: Value,
    ) -> Result<Self, SemanticError> {
        let tool = Self {
            name: name.into(),
            description: description.into(),
            input_schema,
        };
        tool.validate()?;
        Ok(tool)
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn description(&self) -> &str {
        &self.description
    }

    pub fn input_schema(&self) -> &Value {
        &self.input_schema
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
        validate_json("tool.input_schema", &self.input_schema)
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
    Auto,
    None,
    Required,
    Specific(String),
}

/// A provider-executed tool with shared cross-provider semantics.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum HostedTool {
    WebSearch { max_uses: Option<u8> },
}

/// Requested language output representation.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum ResponseFormat {
    Text,
    JsonSchema {
        name: String,
        description: Option<String>,
        schema: Value,
        strict: bool,
    },
}

/// Provider-neutral reasoning effort requested by a language call.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReasoningEffort {
    Minimal,
    Low,
    Medium,
    High,
    Xhigh,
}

/// Optional generation controls. Unsupported controls must be rejected by the
/// selected adapter during Prepare, never silently omitted.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LanguageSettings {
    max_output_tokens: Option<u32>,
    temperature: Option<f64>,
    top_p: Option<f64>,
    seed: Option<i64>,
    stop: Vec<String>,
    reasoning_effort: Option<ReasoningEffort>,
}

impl LanguageSettings {
    pub fn with_max_output_tokens(mut self, value: u32) -> Result<Self, SemanticError> {
        self.max_output_tokens = Some(value);
        self.validate()?;
        Ok(self)
    }

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
    pub const fn with_seed(mut self, seed: i64) -> Self {
        self.seed = Some(seed);
        self
    }

    pub fn with_stop(mut self, stop: Vec<String>) -> Result<Self, SemanticError> {
        self.stop = stop;
        self.validate()?;
        Ok(self)
    }

    #[must_use]
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
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
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

impl LanguageRequest {
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

    pub fn with_tools(
        mut self,
        tools: Vec<ToolDefinition>,
        tool_choice: ToolChoice,
    ) -> Result<Self, SemanticError> {
        validate_tools(&tools, &tool_choice)?;
        self.tools = tools;
        self.tool_choice = tool_choice;
        Ok(self)
    }

    pub fn with_hosted_tools(
        mut self,
        hosted_tools: Vec<HostedTool>,
    ) -> Result<Self, SemanticError> {
        validate_hosted_tools(&hosted_tools)?;
        self.hosted_tools = hosted_tools;
        Ok(self)
    }

    pub fn with_response_format(
        mut self,
        response_format: ResponseFormat,
    ) -> Result<Self, SemanticError> {
        response_format.validate()?;
        self.response_format = response_format;
        Ok(self)
    }

    pub fn with_extensions(
        mut self,
        extensions: Vec<ProviderExtension>,
    ) -> Result<Self, SemanticError> {
        validate_extensions(&extensions)?;
        self.extensions = extensions;
        Ok(self)
    }

    pub fn with_settings(mut self, settings: LanguageSettings) -> Result<Self, SemanticError> {
        settings.validate()?;
        self.settings = settings;
        Ok(self)
    }

    pub fn messages(&self) -> &[Message] {
        &self.messages
    }

    pub fn tools(&self) -> &[ToolDefinition] {
        &self.tools
    }

    pub fn tool_choice(&self) -> &ToolChoice {
        &self.tool_choice
    }

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

    pub fn canonical_bytes(&self) -> Result<Vec<u8>, SemanticError> {
        self.validate()?;
        self.canonical_bytes_unchecked()
    }

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
        self.settings.validate()?;
        validate_tools(&self.tools, &self.tool_choice)?;
        validate_hosted_tools(&self.hosted_tools)?;
        self.response_format.validate()?;
        validate_extensions(&self.extensions)
    }

    fn canonical_bytes_unchecked(&self) -> Result<Vec<u8>, SemanticError> {
        let value = serde_json::to_value(self).map_err(|error| {
            SemanticError::new("request.encoding", "request", error.to_string())
        })?;
        let canonical =
            validation::canonical_json(&value).map_err(|reason| json_error("request", reason))?;
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

fn json_error(field: &str, reason: String) -> SemanticError {
    let code = if reason.contains("nesting") {
        "json.too_deep"
    } else if reason.contains("node count") {
        "json.too_many_nodes"
    } else {
        "json.invalid"
    };
    SemanticError::new(code, field, reason)
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

    pub const fn code(&self) -> &'static str {
        self.code
    }

    pub fn field(&self) -> &str {
        &self.field
    }
}
