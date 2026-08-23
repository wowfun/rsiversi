//! Closed, bounded semantic DATA protocols for `rsi-agent` providers.
//!
//! This crate owns the JSON payload and RAT1 binary chunk contracts; no active
//! Runtime or transport currently maps them to `rsi-meta`. Callers must use [`ToolsEnvelope::decode`]
//! at an untrusted boundary. AI model semantics are owned by `rsi-ai-protocol`.

use std::{
    collections::{BTreeMap, BTreeSet},
    io,
};

use serde::{Deserialize, Deserializer, Serialize, de};
use serde_json::{Map, Number, Value, value::RawValue};
use thiserror::Error;

use rsi_ai_protocol::{MediaDescriptor, MediaKind, SemanticError};

pub use rsi_ai_protocol::{
    FreeformFormat, FreeformToolDefinition, MAX_FREEFORM_GRAMMAR_BYTES, MAX_IMAGE_BYTES,
    MAX_IMAGE_DIMENSION,
};

pub const TOOLS_SERVICE_KEY: &str = "rsi.agent.tools";
pub const TOOLS_PROTOCOL: &str = TOOLS_SERVICE_KEY;
pub const WIRE_VERSION: u32 = 1;
pub const MAX_DATA_BYTES: usize = 768 * 1024;
pub const MAX_ID_BYTES: usize = 255;

/// Returns whether a value satisfies the shared durable and service identifier grammar.
#[must_use]
pub fn is_wire_identifier(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= MAX_ID_BYTES
        && value.bytes().all(|byte| byte.is_ascii_graphic())
}
pub const MAX_TOOLS: usize = 64;
pub const MAX_CATALOG_BYTES: usize = 256 * 1024;
pub const MAX_CONTENT_CHARS: usize = 64 * 1024;
pub const MAX_TOOL_CONTENT_BLOCKS: usize = 64;
pub const MAX_RESULT_TEXT_BYTES: usize = 512 * 1024;
pub const MAX_PATH_CHARS: usize = 4 * 1024;
pub const MAX_TERMINAL_TYPES: usize = 16;
pub const MAX_TIME_BUDGET_MS: u64 = 600_000;
pub const MAX_BLOB_CHUNK_BYTES: usize = rsi_ai_protocol::MAX_BINARY_CHUNK_BYTES;
pub const RAT1_MAGIC: [u8; 4] = *b"RAT1";
pub const MAX_JSON_DEPTH: usize = 64;
/// Maximum JSON nodes including the root value.
pub const MAX_JSON_NODES: usize = 65_536;

pub const MAX_DESCRIPTION_CHARS: usize = 4 * 1024;
pub const MAX_ERROR_CODE_BYTES: usize = 64;
pub const MAX_ERROR_MESSAGE_CHARS: usize = 4 * 1024;

fn deserialize_optional_nonnull<'de, D, T>(deserializer: D) -> Result<Option<T>, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de>,
{
    T::deserialize(deserializer).map(Some)
}

/// Provider-neutral declaration of one callable tool.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ToolDefinition {
    /// Stable callable name within the catalog.
    pub name: String,
    /// Model-facing explanation of the operation.
    pub description: String,
    /// JSON Schema for the tool's argument object.
    pub input_schema: Value,
    /// Optional alternate freeform projection of the same semantic tool.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "deserialize_optional_nonnull"
    )]
    pub freeform: Option<FreeformToolDefinition>,
}

impl ToolDefinition {
    fn validate(&self, field: &str) -> Result<(), ProtocolError> {
        require_tool_name(&format!("{field}.name"), &self.name)?;
        require_text(
            &format!("{field}.description"),
            &self.description,
            MAX_DESCRIPTION_CHARS,
            true,
        )?;
        if !matches!(self.input_schema, Value::Object(_) | Value::Bool(_)) {
            return invalid(
                format!("{field}.input_schema"),
                "must be an object or boolean JSON Schema",
            );
        }
        validate_json(&self.input_schema)?;
        Ok(())
    }
}

/// Image bytes transferred on the owner stream before a rich tool result references them.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ToolImage {
    pub blob_id: String,
    pub mime_type: String,
    pub byte_len: u64,
    pub sha256: String,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "deserialize_optional_nonnull"
    )]
    pub width: Option<u32>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "deserialize_optional_nonnull"
    )]
    pub height: Option<u32>,
}

impl ToolImage {
    fn validate(&self, field: &str) -> Result<(), ProtocolError> {
        require_identifier(&format!("{field}.blob_id"), &self.blob_id)?;
        let descriptor = MediaDescriptor::new(
            MediaKind::Image,
            self.mime_type.clone(),
            self.byte_len,
            self.sha256.clone(),
        )
        .map_err(|error| map_media_error(field, &error))?;
        match (self.width, self.height) {
            (None, None) => Ok(()),
            (Some(width), Some(height)) => descriptor
                .with_image_dimensions(width, height)
                .map(|_| ())
                .map_err(|error| map_media_error(field, &error)),
            _ => invalid(
                field,
                "image dimensions must be absent together or form a bounded positive pair",
            ),
        }
    }
}

fn map_media_error(field: &str, error: &SemanticError) -> ProtocolError {
    let suffix = error.field().strip_prefix("media").unwrap_or_default();
    ProtocolError::InvalidField {
        field: format!("{field}{suffix}"),
        reason: error.reason().to_owned(),
    }
}

/// One native model-visible block returned by a tool.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum ToolContent {
    Text { text: String },
    Image { image: ToolImage },
}

fn validate_tool_content(content: &[ToolContent], field: &str) -> Result<(), ProtocolError> {
    if content.is_empty() || content.len() > MAX_TOOL_CONTENT_BLOCKS {
        return invalid(
            field,
            format!("must contain 1..={MAX_TOOL_CONTENT_BLOCKS} blocks"),
        );
    }
    let mut text_bytes = 0_usize;
    for (index, block) in content.iter().enumerate() {
        match block {
            ToolContent::Text { text } => {
                require_text(
                    &format!("{field}[{index}].text"),
                    text,
                    MAX_RESULT_TEXT_BYTES,
                    true,
                )?;
                text_bytes = text_bytes.checked_add(text.len()).ok_or_else(|| {
                    ProtocolError::InvalidField {
                        field: field.to_owned(),
                        reason: "aggregate text byte count overflowed".to_owned(),
                    }
                })?;
            }
            ToolContent::Image { image } => {
                image.validate(&format!("{field}[{index}].image"))?;
            }
        }
    }
    if text_bytes > MAX_RESULT_TEXT_BYTES {
        return invalid(
            field,
            format!("aggregate text exceeds {MAX_RESULT_TEXT_BYTES} UTF-8 bytes"),
        );
    }
    Ok(())
}

/// Stable semantic result presented to the model and transcript.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case", deny_unknown_fields)]
pub enum ToolResult {
    /// Successful native text/image result.
    Ok {
        /// Ordered content returned by the tool.
        content: Vec<ToolContent>,
    },
    /// Stable tool-level failure presented to the model and transcript.
    Error {
        /// Machine-readable failure code.
        code: String,
        /// Bounded human-readable failure summary.
        message: String,
    },
}

/// Filesystem change kind retained for non-model-visible patch provenance.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AppliedPatchChangeKind {
    Add,
    Update,
    Delete,
    Move,
}

/// Digest-only description of one applied patch change.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AppliedPatchChange {
    /// Kind of committed filesystem mutation.
    pub kind: AppliedPatchChangeKind,
    /// Patch-relative source path, or destination path for a partial move write.
    pub path: String,
    /// Patch-relative destination for a fully committed move.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "deserialize_optional_nonnull"
    )]
    pub move_to: Option<String>,
    /// Digest of the source content observed before the change.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "deserialize_optional_nonnull"
    )]
    pub old_sha256: Option<String>,
    /// Digest of the content committed by the change.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "deserialize_optional_nonnull"
    )]
    pub new_sha256: Option<String>,
    /// Digest of destination content overwritten by an add or move.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "deserialize_optional_nonnull"
    )]
    pub overwritten_sha256: Option<String>,
}

/// Bounded non-model-visible provenance for one sequential patch execution.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AppliedPatchDelta {
    /// Whether the ordered digest observations exactly describe all known mutations.
    pub exact: bool,
    /// Changes known to have committed, in publication order.
    pub changes: Vec<AppliedPatchChange>,
}

/// Private result metadata persisted by the agent but never projected to a model.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum ToolPrivateResult {
    /// Ordered filesystem provenance from the patch runtime.
    AppliedPatchDelta { delta: AppliedPatchDelta },
}

impl ToolPrivateResult {
    /// Validates bounded paths, digests, change count, and kind-specific shape.
    ///
    /// # Errors
    ///
    /// Returns [`ProtocolError::InvalidField`] when the private result is malformed.
    pub fn validate(&self) -> Result<(), ProtocolError> {
        let Self::AppliedPatchDelta { delta } = self;
        if delta.changes.len() > 128 {
            return invalid("private_result.changes", "must contain at most 128 changes");
        }
        for (index, change) in delta.changes.iter().enumerate() {
            require_relative_path(
                &format!("private_result.changes[{index}].path"),
                &change.path,
            )?;
            if let Some(path) = &change.move_to {
                require_relative_path(&format!("private_result.changes[{index}].move_to"), path)?;
            }
            for (field, digest) in [
                ("old_sha256", &change.old_sha256),
                ("new_sha256", &change.new_sha256),
                ("overwritten_sha256", &change.overwritten_sha256),
            ] {
                if digest.as_deref().is_some_and(|value| {
                    value.len() != 64
                        || !value
                            .bytes()
                            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
                }) {
                    return invalid(
                        format!("private_result.changes[{index}].{field}"),
                        "must be a lowercase SHA-256 digest",
                    );
                }
            }
            let shape_valid = match change.kind {
                AppliedPatchChangeKind::Add => {
                    change.move_to.is_none()
                        && change.old_sha256.is_none()
                        && change.new_sha256.is_some()
                }
                AppliedPatchChangeKind::Update => {
                    change.move_to.is_none()
                        && change.old_sha256.is_some()
                        && change.new_sha256.is_some()
                        && change.overwritten_sha256.is_none()
                }
                AppliedPatchChangeKind::Delete => {
                    change.move_to.is_none()
                        && change.old_sha256.is_some()
                        && change.new_sha256.is_none()
                        && change.overwritten_sha256.is_none()
                }
                AppliedPatchChangeKind::Move => {
                    change.move_to.is_some()
                        && change.old_sha256.is_some()
                        && change.new_sha256.is_some()
                }
            };
            if !shape_valid {
                return invalid(
                    format!("private_result.changes[{index}]"),
                    "digest and move fields do not match the change kind",
                );
            }
        }
        Ok(())
    }
}

fn require_relative_path(field: &str, path: &str) -> Result<(), ProtocolError> {
    require_path_text(field, path)?;
    if path.starts_with('/')
        || path.starts_with('\\')
        || path.contains('\\')
        || path.contains(':')
        || path.split('/').any(|component| {
            component.is_empty()
                || matches!(component, "." | "..")
                || component.ends_with(['.', ' '])
                || is_windows_device_component(component)
        })
    {
        return invalid(
            field,
            "must be a normalized relative path using nonempty forward-slash components",
        );
    }
    Ok(())
}

fn is_windows_device_component(component: &str) -> bool {
    let basename = component.split('.').next().unwrap_or(component);
    if ["CON", "PRN", "AUX", "NUL"]
        .iter()
        .any(|reserved| basename.eq_ignore_ascii_case(reserved))
    {
        return true;
    }

    let basename = basename.to_ascii_uppercase();
    ["COM", "LPT"].iter().any(|prefix| {
        basename.strip_prefix(prefix).is_some_and(|suffix| {
            matches!(
                suffix,
                "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9" | "¹" | "²" | "³"
            )
        })
    })
}

impl ToolResult {
    /// Validates the aggregate result without requiring a surrounding envelope.
    ///
    /// This is the durable-transcript validation seam: callers that already
    /// have a typed result should not manufacture unrelated request fields just
    /// to enforce result bounds.
    ///
    /// # Errors
    ///
    /// Returns [`ProtocolError::InvalidField`], [`ProtocolError::JsonLimit`], or
    /// [`ProtocolError::LossyJsonNumber`] when the result is outside the wire
    /// contract.
    pub fn validate(&self, field: &str) -> Result<(), ProtocolError> {
        match self {
            Self::Ok { content } => validate_tool_content(content, &format!("{field}.content")),
            Self::Error { code, message } => {
                require_code(&format!("{field}.code"), code)?;
                require_text(
                    &format!("{field}.message"),
                    message,
                    MAX_ERROR_MESSAGE_CHARS,
                    true,
                )
            }
        }
    }
}

/// Opens one live tools owner epoch on a generation-pinned stream.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ToolsOwnerOpenRequest {
    pub owner_id: String,
    pub owner_epoch: String,
    pub execution_cwd: String,
    pub tool_policy_sha256: String,
}

impl ToolsOwnerOpenRequest {
    fn validate(&self) -> Result<(), ProtocolError> {
        require_identifier("owner_id", &self.owner_id)?;
        require_identifier("owner_epoch", &self.owner_epoch)?;
        require_path_text("execution_cwd", &self.execution_cwd)?;
        require_sha256("tool_policy_sha256", &self.tool_policy_sha256)
    }
}

/// Confirms the exact owner epoch and advertises registered terminal kinds.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ToolsOwnerOpenResponse {
    pub owner_id: String,
    pub owner_epoch: String,
    pub terminal_types: Vec<String>,
}

impl ToolsOwnerOpenResponse {
    fn validate(&self) -> Result<(), ProtocolError> {
        require_identifier("owner_id", &self.owner_id)?;
        require_identifier("owner_epoch", &self.owner_epoch)?;
        if self.terminal_types.len() > MAX_TERMINAL_TYPES {
            return invalid(
                "terminal_types",
                format!("contains more than {MAX_TERMINAL_TYPES} entries"),
            );
        }
        let mut types = BTreeSet::new();
        for (index, terminal_type) in self.terminal_types.iter().enumerate() {
            require_tool_name(&format!("terminal_types[{index}]"), terminal_type)?;
            if !types.insert(terminal_type) {
                return invalid("terminal_types", "contains duplicates");
            }
        }
        Ok(())
    }
}

/// Stable service-level error payload.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WireError {
    /// Machine-readable service failure code.
    pub code: String,
    /// Bounded human-readable failure summary.
    pub message: String,
}

impl WireError {
    fn validate(&self, field: &str) -> Result<(), ProtocolError> {
        require_code(&format!("{field}.code"), &self.code)?;
        require_text(
            &format!("{field}.message"),
            &self.message,
            MAX_ERROR_MESSAGE_CHARS,
            true,
        )
    }
}

/// Payload of a successful tools catalog response.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ToolsCatalogResponse {
    /// Canonically ordered callable tool declarations.
    pub tools: Vec<ToolDefinition>,
}

impl ToolsCatalogResponse {
    /// Validates the aggregate catalog without requiring a surrounding envelope.
    ///
    /// # Errors
    ///
    /// Returns a field, JSON-complexity, number, or canonical-size error when
    /// the catalog is outside the wire contract.
    pub fn validate(&self, field: &str) -> Result<(), ProtocolError> {
        validate_catalog(&self.tools, field)
    }
}

/// Payload of one tool invocation request.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ToolsInvokeRequest {
    /// Call identifier correlated with the model's tool request.
    pub call_id: String,
    /// Exact tool name from the current catalog.
    pub name: String,
    /// Raw JSON argument text supplied by the model.
    pub arguments: String,
    /// Trusted execution budget remaining after the core reserves settle time.
    pub time_budget_ms: u64,
}

impl ToolsInvokeRequest {
    fn validate(&self) -> Result<(), ProtocolError> {
        require_identifier("call_id", &self.call_id)?;
        require_tool_name("name", &self.name)?;
        require_text("arguments", &self.arguments, MAX_CONTENT_CHARS, false)?;
        if self.time_budget_ms == 0 || self.time_budget_ms > MAX_TIME_BUDGET_MS {
            return invalid(
                "time_budget_ms",
                format!("must be 1..={MAX_TIME_BUDGET_MS}"),
            );
        }
        Ok(())
    }
}

/// Requests cancellation of one invocation without creating a second terminal outcome.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ToolsCancelInvoke {
    pub target_request_id: String,
    pub reason: String,
}

impl ToolsCancelInvoke {
    fn validate(&self) -> Result<(), ProtocolError> {
        require_identifier("target_request_id", &self.target_request_id)?;
        require_text("reason", &self.reason, MAX_ERROR_MESSAGE_CHARS, false)
    }
}

/// Whether an idle owner should immediately open a completion turn.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NotificationDelivery {
    Quiet,
    Wakeup,
}

/// Owner-scoped background completion delivered independently of invoke responses.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ToolsNotification {
    pub notification_id: String,
    pub delivery: NotificationDelivery,
    pub content: Vec<ToolContent>,
}

impl ToolsNotification {
    fn validate(&self) -> Result<(), ProtocolError> {
        require_identifier("notification_id", &self.notification_id)?;
        validate_tool_content(&self.content, "content")
    }
}

/// Acknowledges that the invoke result for one call is durable in the transcript.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ToolsCommitResult {
    pub call_id: String,
}

impl ToolsCommitResult {
    fn validate(&self) -> Result<(), ProtocolError> {
        require_identifier("call_id", &self.call_id)
    }
}

/// Opens a bounded image blob whose bytes follow as RAT1 chunks.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ToolsBlobStart {
    pub blob: ToolImage,
}

impl ToolsBlobStart {
    fn validate(&self) -> Result<(), ProtocolError> {
        self.blob.validate("blob")
    }
}

/// Closes a previously opened and completely transferred blob.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ToolsBlobEnd {
    pub blob_id: String,
}

impl ToolsBlobEnd {
    fn validate(&self) -> Result<(), ProtocolError> {
        require_identifier("blob_id", &self.blob_id)
    }
}

/// Payload of one tool invocation response.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ToolsInvokeResponse {
    /// Call identifier from the corresponding invocation request.
    pub call_id: String,
    /// Durable semantic tool result.
    pub result: ToolResult,
    /// Optional bounded provenance excluded from all model projections.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "deserialize_optional_nonnull"
    )]
    pub private_result: Option<ToolPrivateResult>,
}

impl ToolsInvokeResponse {
    fn validate(&self) -> Result<(), ProtocolError> {
        require_identifier("call_id", &self.call_id)?;
        self.result.validate("result")?;
        if let Some(private) = &self.private_result {
            private.validate()?;
        }
        Ok(())
    }
}

/// Kind-specific body of a tools envelope.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum ToolsBody {
    OwnerOpenRequest(ToolsOwnerOpenRequest),
    OwnerOpenResponse(ToolsOwnerOpenResponse),
    CatalogRequest {},
    CatalogResponse(ToolsCatalogResponse),
    InvokeRequest(ToolsInvokeRequest),
    InvokeResponse(ToolsInvokeResponse),
    CancelInvoke(ToolsCancelInvoke),
    Notification(ToolsNotification),
    CommitResult(ToolsCommitResult),
    BlobStart(ToolsBlobStart),
    BlobEnd(ToolsBlobEnd),
    Error { error: WireError },
}

/// A closed, versioned tools-service envelope.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ToolsEnvelope {
    /// Exact protocol identity; must equal [`TOOLS_PROTOCOL`].
    pub protocol: String,
    /// Exact semantic wire version; only [`WIRE_VERSION`] is accepted.
    #[serde(deserialize_with = "deserialize_wire_version")]
    pub version: u32,
    /// Caller-assigned envelope correlation identifier.
    pub request_id: String,
    /// Kind-specific request or response payload.
    #[serde(flatten)]
    pub body: ToolsBody,
}

impl ToolsEnvelope {
    /// Constructs an owner-open request.
    pub fn owner_open_request(
        request_id: impl Into<String>,
        request: ToolsOwnerOpenRequest,
    ) -> Self {
        Self::new(request_id, ToolsBody::OwnerOpenRequest(request))
    }

    /// Constructs an owner-open response.
    pub fn owner_open_response(
        request_id: impl Into<String>,
        response: ToolsOwnerOpenResponse,
    ) -> Self {
        Self::new(request_id, ToolsBody::OwnerOpenResponse(response))
    }

    /// Constructs a catalog request with the current protocol header.
    pub fn catalog_request(request_id: impl Into<String>) -> Self {
        Self::new(request_id, ToolsBody::CatalogRequest {})
    }

    /// Constructs a catalog response with the current protocol header.
    pub fn catalog_response(request_id: impl Into<String>, response: ToolsCatalogResponse) -> Self {
        Self::new(request_id, ToolsBody::CatalogResponse(response))
    }

    /// Constructs an invocation request with the current protocol header.
    pub fn invoke_request(request_id: impl Into<String>, request: ToolsInvokeRequest) -> Self {
        Self::new(request_id, ToolsBody::InvokeRequest(request))
    }

    /// Constructs an invocation response with the current protocol header.
    pub fn invoke_response(request_id: impl Into<String>, response: ToolsInvokeResponse) -> Self {
        Self::new(request_id, ToolsBody::InvokeResponse(response))
    }

    pub fn cancel_invoke(request_id: impl Into<String>, cancel: ToolsCancelInvoke) -> Self {
        Self::new(request_id, ToolsBody::CancelInvoke(cancel))
    }

    pub fn notification(request_id: impl Into<String>, notification: ToolsNotification) -> Self {
        Self::new(request_id, ToolsBody::Notification(notification))
    }

    pub fn commit_result(request_id: impl Into<String>, commit: ToolsCommitResult) -> Self {
        Self::new(request_id, ToolsBody::CommitResult(commit))
    }

    pub fn blob_start(request_id: impl Into<String>, start: ToolsBlobStart) -> Self {
        Self::new(request_id, ToolsBody::BlobStart(start))
    }

    pub fn blob_end(request_id: impl Into<String>, end: ToolsBlobEnd) -> Self {
        Self::new(request_id, ToolsBody::BlobEnd(end))
    }

    /// Constructs a service error response with the current protocol header.
    pub fn error(request_id: impl Into<String>, error: WireError) -> Self {
        Self::new(request_id, ToolsBody::Error { error })
    }

    fn new(request_id: impl Into<String>, body: ToolsBody) -> Self {
        Self {
            protocol: TOOLS_PROTOCOL.to_owned(),
            version: WIRE_VERSION,
            request_id: request_id.into(),
            body,
        }
    }

    /// Validates the protocol header and every kind-specific field.
    ///
    /// # Errors
    ///
    /// Returns a protocol, version, identifier, or payload-bound error.
    pub fn validate(&self) -> Result<(), ProtocolError> {
        validate_header(
            &self.protocol,
            TOOLS_PROTOCOL,
            self.version,
            &self.request_id,
        )?;
        match &self.body {
            ToolsBody::OwnerOpenRequest(request) => request.validate(),
            ToolsBody::OwnerOpenResponse(response) => response.validate(),
            ToolsBody::CatalogRequest {} => Ok(()),
            ToolsBody::CatalogResponse(response) => response.validate("tools"),
            ToolsBody::InvokeRequest(request) => request.validate(),
            ToolsBody::InvokeResponse(response) => response.validate(),
            ToolsBody::CancelInvoke(cancel) => cancel.validate(),
            ToolsBody::Notification(notification) => notification.validate(),
            ToolsBody::CommitResult(commit) => commit.validate(),
            ToolsBody::BlobStart(start) => start.validate(),
            ToolsBody::BlobEnd(end) => end.validate(),
            ToolsBody::Error { error } => error.validate("error"),
        }
    }

    /// Encodes one validated, recursively canonical JSON DATA payload.
    ///
    /// # Errors
    ///
    /// Returns a validation, serialization, or frame-size error.
    pub fn encode(&self) -> Result<Vec<u8>, ProtocolError> {
        self.validate()?;
        encode_canonical(self)
    }

    /// Decodes and validates one untrusted tools DATA payload.
    ///
    /// # Errors
    ///
    /// Returns a JSON, lossy-number, validation, or frame-size error.
    pub fn decode(bytes: &[u8]) -> Result<Self, ProtocolError> {
        decode_canonical(bytes, Self::validate)
    }
}

/// One bounded binary chunk belonging to an opened tool blob.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ToolBlobChunk {
    pub blob_id: String,
    pub offset: u64,
    pub data: Vec<u8>,
}

impl ToolBlobChunk {
    /// Encodes a RAT1 chunk without copying its payload more than once.
    ///
    /// # Errors
    ///
    /// Rejects an invalid blob id, an empty or oversized chunk, offset overflow,
    /// or a chunk whose range exceeds the maximum image size.
    pub fn encode(&self) -> Result<Vec<u8>, ProtocolError> {
        self.validate()?;
        let id_len =
            u16::try_from(self.blob_id.len()).map_err(|_| ProtocolError::InvalidBinaryFrame {
                reason: "blob id length is unsupported".to_owned(),
            })?;
        let data_len =
            u32::try_from(self.data.len()).map_err(|_| ProtocolError::InvalidBinaryFrame {
                reason: "chunk length is unsupported".to_owned(),
            })?;
        let mut encoded = Vec::with_capacity(19 + self.blob_id.len() + self.data.len());
        encoded.extend_from_slice(&RAT1_MAGIC);
        encoded.push(1);
        encoded.extend_from_slice(&id_len.to_be_bytes());
        encoded.extend_from_slice(&self.offset.to_be_bytes());
        encoded.extend_from_slice(&data_len.to_be_bytes());
        encoded.extend_from_slice(self.blob_id.as_bytes());
        encoded.extend_from_slice(&self.data);
        Ok(encoded)
    }

    fn validate(&self) -> Result<(), ProtocolError> {
        require_identifier("blob_id", &self.blob_id)?;
        if self.data.is_empty() || self.data.len() > MAX_BLOB_CHUNK_BYTES {
            return invalid(
                "data",
                format!("must contain 1..={MAX_BLOB_CHUNK_BYTES} bytes"),
            );
        }
        let chunk_len =
            u64::try_from(self.data.len()).map_err(|_| ProtocolError::InvalidBinaryFrame {
                reason: "chunk length is unsupported on this platform".to_owned(),
            })?;
        let end = self.offset.checked_add(chunk_len).ok_or_else(|| {
            ProtocolError::InvalidBinaryFrame {
                reason: "chunk range overflowed".to_owned(),
            }
        })?;
        if end > MAX_IMAGE_BYTES {
            return Err(ProtocolError::InvalidBinaryFrame {
                reason: format!("chunk range exceeds {MAX_IMAGE_BYTES} bytes"),
            });
        }
        Ok(())
    }

    /// Decodes and validates exactly one RAT1 chunk.
    ///
    /// # Errors
    ///
    /// Rejects truncated, trailing, unsupported, or semantically invalid data.
    pub fn decode(bytes: &[u8]) -> Result<Self, ProtocolError> {
        const HEADER_BYTES: usize = 19;
        if bytes.len() < HEADER_BYTES || bytes[..4] != RAT1_MAGIC || bytes[4] != 1 {
            return Err(ProtocolError::InvalidBinaryFrame {
                reason: "missing RAT1 chunk header".to_owned(),
            });
        }
        let id_len = usize::from(u16::from_be_bytes([bytes[5], bytes[6]]));
        let offset = u64::from_be_bytes([
            bytes[7], bytes[8], bytes[9], bytes[10], bytes[11], bytes[12], bytes[13], bytes[14],
        ]);
        let data_len = usize::try_from(u32::from_be_bytes([
            bytes[15], bytes[16], bytes[17], bytes[18],
        ]))
        .map_err(|_| ProtocolError::InvalidBinaryFrame {
            reason: "chunk length is unsupported on this platform".to_owned(),
        })?;
        if id_len == 0 || id_len > MAX_ID_BYTES {
            return Err(ProtocolError::InvalidBinaryFrame {
                reason: format!("blob id length must be 1..={MAX_ID_BYTES} bytes"),
            });
        }
        if data_len == 0 || data_len > MAX_BLOB_CHUNK_BYTES {
            return Err(ProtocolError::InvalidBinaryFrame {
                reason: format!("chunk data length must be 1..={MAX_BLOB_CHUNK_BYTES} bytes"),
            });
        }
        let expected = HEADER_BYTES
            .checked_add(id_len)
            .and_then(|length| length.checked_add(data_len))
            .ok_or_else(|| ProtocolError::InvalidBinaryFrame {
                reason: "encoded chunk length overflowed".to_owned(),
            })?;
        if bytes.len() != expected {
            return Err(ProtocolError::InvalidBinaryFrame {
                reason: format!(
                    "encoded chunk length is {}, header declares {expected}",
                    bytes.len()
                ),
            });
        }
        let blob_id = std::str::from_utf8(&bytes[HEADER_BYTES..HEADER_BYTES + id_len])
            .map_err(|_| ProtocolError::InvalidBinaryFrame {
                reason: "blob id is not UTF-8".to_owned(),
            })?
            .to_owned();
        let chunk = Self {
            blob_id,
            offset,
            data: bytes[HEADER_BYTES + id_len..].to_vec(),
        };
        chunk.validate()?;
        Ok(chunk)
    }
}

/// Recursively sorts object keys while enforcing bounded JSON complexity.
///
/// # Errors
///
/// Returns [`ProtocolError::JsonLimit`] when nesting or node count exceeds the
/// version-zero contract, or [`ProtocolError::LossyJsonNumber`] when a number
/// exists only through an enabled extended-precision representation.
pub fn canonicalize_json(value: &Value) -> Result<Value, ProtocolError> {
    let mut nodes = 0;
    canonicalize_json_at(value, 0, &mut nodes)
}

fn validate_json(value: &Value) -> Result<(), ProtocolError> {
    let mut nodes = 0;
    validate_json_at(value, 0, &mut nodes)
}

/// Encodes a recursively key-sorted JSON value.
///
/// # Errors
///
/// Returns a JSON-complexity, lossy-number, or serialization error.
pub fn canonical_json_bytes(value: &Value) -> Result<Vec<u8>, ProtocolError> {
    serde_json::to_vec(&canonicalize_json(value)?).map_err(ProtocolError::Json)
}

/// Parses exactly one JSON text without losing duplicate-key information.
///
/// The returned value is recursively key-sorted. Fractions, exponents, and
/// integers outside the native `i64`/`u64` domain use the finite machine number
/// represented by [`serde_json::Value`]. Unlike `serde_json::from_str::<Value>`,
/// this parser rejects duplicate object keys at every depth while the original
/// member stream is still observable. It also enforces [`MAX_JSON_DEPTH`] and
/// [`MAX_JSON_NODES`]. The input string is only borrowed, so a caller can retain
/// the original text independently.
///
/// # Errors
///
/// Returns [`ProtocolError::DuplicateJsonKey`] for a repeated object key,
/// [`ProtocolError::JsonLimit`] for a depth or node limit,
/// [`ProtocolError::LossyJsonNumber`] for a syntactically valid number outside
/// the finite native representation, or [`ProtocolError::Json`] for malformed
/// JSON or trailing data.
pub fn parse_json_strict(text: &str) -> Result<Value, ProtocolError> {
    StrictJsonParser::new(text.as_bytes()).parse(NumberPolicy::Normalize)
}

/// Parses strict JSON for a downstream consumer that evaluates every number as `f64`.
///
/// Every accepted number must have an exact finite `f64` representation.
/// Mathematically integral values are canonicalized as JSON integers and
/// nonintegral values retain their finite `f64` representation. A number whose
/// decimal value would change is rejected instead of being silently rounded
/// before schema validation or dispatch.
///
/// # Errors
///
/// Returns the same syntax, duplicate-key, and complexity failures as
/// [`parse_json_strict`], plus [`ProtocolError::LossyJsonNumber`] when a number
/// has no exact finite `f64` representation.
pub fn parse_json_strict_f64(text: &str) -> Result<Value, ProtocolError> {
    StrictJsonParser::new(text.as_bytes()).parse(NumberPolicy::RequireF64Exact)
}

fn parse_json_exact(bytes: &[u8]) -> Result<Value, ProtocolError> {
    StrictJsonParser::new(bytes).parse(NumberPolicy::RequireExact)
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum NumberPolicy {
    Normalize,
    RequireExact,
    RequireF64Exact,
}

// This parser deliberately owns the small amount of JSON grammar needed at the
// trust boundary. Deserializing straight into `Value` would erase duplicate
// members before they can be rejected, while deserializing every token through
// `RawValue` would duplicate a substantially larger, feature-sensitive grammar.
// The differential corpus in `tests/contracts.rs` keeps this implementation
// aligned with serde_json for ordinary JSON syntax.
struct StrictJsonParser<'input> {
    input: &'input [u8],
    cursor: usize,
    nodes: usize,
}

impl<'input> StrictJsonParser<'input> {
    fn new(input: &'input [u8]) -> Self {
        Self {
            input,
            cursor: 0,
            nodes: 0,
        }
    }

    fn parse(mut self, number_policy: NumberPolicy) -> Result<Value, ProtocolError> {
        self.skip_whitespace();
        let value = self.parse_value(0, number_policy)?;
        self.skip_whitespace();
        if self.cursor != self.input.len() {
            return Err(json_syntax("trailing data after the JSON value"));
        }
        Ok(value)
    }

    fn parse_value(
        &mut self,
        depth: usize,
        number_policy: NumberPolicy,
    ) -> Result<Value, ProtocolError> {
        self.record_node(depth)?;
        match self.peek() {
            Some(b'n') => {
                self.consume_literal(b"null")?;
                Ok(Value::Null)
            }
            Some(b't') => {
                self.consume_literal(b"true")?;
                Ok(Value::Bool(true))
            }
            Some(b'f') => {
                self.consume_literal(b"false")?;
                Ok(Value::Bool(false))
            }
            Some(b'"') => self.parse_string().map(Value::String),
            Some(b'[') => self.parse_array(depth, number_policy),
            Some(b'{') => self.parse_object(depth, number_policy),
            Some(b'-' | b'0'..=b'9') => self.parse_number(number_policy).map(Value::Number),
            Some(_) => Err(json_syntax("unexpected character in JSON value")),
            None => Err(json_syntax("expected a JSON value")),
        }
    }

    fn parse_array(
        &mut self,
        depth: usize,
        number_policy: NumberPolicy,
    ) -> Result<Value, ProtocolError> {
        self.cursor += 1;
        self.skip_whitespace();
        let mut values = Vec::new();
        if self.consume_if(b']') {
            return Ok(Value::Array(values));
        }
        loop {
            values.push(self.parse_value(depth + 1, number_policy)?);
            self.skip_whitespace();
            if self.consume_if(b']') {
                return Ok(Value::Array(values));
            }
            self.expect(b',', "expected `,` or `]` after array element")?;
            self.skip_whitespace();
        }
    }

    fn parse_object(
        &mut self,
        depth: usize,
        number_policy: NumberPolicy,
    ) -> Result<Value, ProtocolError> {
        self.cursor += 1;
        self.skip_whitespace();
        let mut members = BTreeMap::new();
        if self.consume_if(b'}') {
            return Ok(Value::Object(Map::new()));
        }
        loop {
            if self.peek() != Some(b'"') {
                return Err(json_syntax("expected a JSON object key"));
            }
            let key = self.parse_string()?;
            if members.contains_key(&key) {
                return Err(ProtocolError::DuplicateJsonKey { key });
            }
            self.skip_whitespace();
            self.expect(b':', "expected `:` after JSON object key")?;
            self.skip_whitespace();
            let value = self.parse_value(depth + 1, number_policy)?;
            members.insert(key, value);
            self.skip_whitespace();
            if self.consume_if(b'}') {
                let object = members.into_iter().collect();
                return Ok(Value::Object(object));
            }
            self.expect(b',', "expected `,` or `}` after object member")?;
            self.skip_whitespace();
        }
    }

    fn parse_string(&mut self) -> Result<String, ProtocolError> {
        let start = self.cursor;
        self.cursor += 1;
        while let Some(byte) = self.peek() {
            match byte {
                b'"' => {
                    self.cursor += 1;
                    return serde_json::from_slice(&self.input[start..self.cursor])
                        .map_err(ProtocolError::Json);
                }
                b'\\' => {
                    self.cursor = self.cursor.saturating_add(2);
                    if self.cursor > self.input.len() {
                        return Err(json_syntax("unterminated JSON string escape"));
                    }
                }
                _ => self.cursor += 1,
            }
        }
        Err(json_syntax("unterminated JSON string"))
    }

    fn parse_number(&mut self, number_policy: NumberPolicy) -> Result<Number, ProtocolError> {
        let start = self.cursor;
        self.scan_number()?;
        let raw = std::str::from_utf8(&self.input[start..self.cursor])
            .expect("a JSON number token contains only ASCII");
        let represented = match number_policy {
            NumberPolicy::RequireF64Exact => f64_exact_number(raw),
            NumberPolicy::Normalize | NumberPolicy::RequireExact => machine_number(raw),
        };
        let Some(represented) = represented else {
            return Err(ProtocolError::LossyJsonNumber);
        };
        if number_policy == NumberPolicy::RequireExact
            && !decimal_values_equal(raw, &represented.to_string())
        {
            return Err(ProtocolError::LossyJsonNumber);
        }
        Ok(represented)
    }

    fn scan_number(&mut self) -> Result<(), ProtocolError> {
        self.consume_if(b'-');
        match self.peek() {
            Some(b'0') => {
                self.cursor += 1;
                if self.peek().is_some_and(|byte| byte.is_ascii_digit()) {
                    return Err(json_syntax("leading zero in JSON number"));
                }
            }
            Some(b'1'..=b'9') => {
                self.cursor += 1;
                self.consume_digits();
            }
            _ => return Err(json_syntax("expected digit in JSON number")),
        }
        if self.consume_if(b'.') {
            self.require_digit("expected digit after decimal point")?;
            self.consume_digits();
        }
        if self.peek().is_some_and(|byte| matches!(byte, b'e' | b'E')) {
            self.cursor += 1;
            if self.peek().is_some_and(|byte| matches!(byte, b'+' | b'-')) {
                self.cursor += 1;
            }
            self.require_digit("expected digit in number exponent")?;
            self.consume_digits();
        }
        Ok(())
    }

    fn require_digit(&mut self, reason: &'static str) -> Result<(), ProtocolError> {
        if self.peek().is_none_or(|byte| !byte.is_ascii_digit()) {
            return Err(json_syntax(reason));
        }
        self.cursor += 1;
        Ok(())
    }

    fn consume_digits(&mut self) {
        while self.peek().is_some_and(|byte| byte.is_ascii_digit()) {
            self.cursor += 1;
        }
    }

    fn consume_literal(&mut self, literal: &[u8]) -> Result<(), ProtocolError> {
        if self.input[self.cursor..].starts_with(literal) {
            self.cursor += literal.len();
            Ok(())
        } else {
            Err(json_syntax("invalid JSON literal"))
        }
    }

    fn record_node(&mut self, depth: usize) -> Result<(), ProtocolError> {
        if depth > MAX_JSON_DEPTH {
            return Err(ProtocolError::JsonLimit {
                reason: format!("nesting exceeds {MAX_JSON_DEPTH}"),
            });
        }
        self.nodes = self.nodes.saturating_add(1);
        if self.nodes > MAX_JSON_NODES {
            return Err(ProtocolError::JsonLimit {
                reason: format!("node count exceeds {MAX_JSON_NODES}"),
            });
        }
        Ok(())
    }

    fn expect(&mut self, byte: u8, reason: &'static str) -> Result<(), ProtocolError> {
        if self.consume_if(byte) {
            Ok(())
        } else {
            Err(json_syntax(reason))
        }
    }

    fn consume_if(&mut self, byte: u8) -> bool {
        if self.peek() == Some(byte) {
            self.cursor += 1;
            true
        } else {
            false
        }
    }

    fn skip_whitespace(&mut self) {
        while self
            .peek()
            .is_some_and(|byte| matches!(byte, b' ' | b'\n' | b'\r' | b'\t'))
        {
            self.cursor += 1;
        }
    }

    fn peek(&self) -> Option<u8> {
        self.input.get(self.cursor).copied()
    }
}

fn machine_number(raw: &str) -> Option<Number> {
    if !raw.bytes().any(|byte| matches!(byte, b'.' | b'e' | b'E')) {
        if raw.starts_with('-') {
            if let Ok(value) = raw.parse::<i64>() {
                return Some(value.into());
            }
        } else if let Ok(value) = raw.parse::<u64>() {
            return Some(value.into());
        }
    }
    raw.parse::<f64>().ok().and_then(Number::from_f64)
}

fn f64_exact_number(raw: &str) -> Option<Number> {
    let value = raw.parse::<f64>().ok()?;
    if !value.is_finite() {
        return None;
    }
    let exact = exact_f64_decimal(value)?;
    if canonical_decimal(raw)? != exact {
        return None;
    }
    if value.fract() == 0.0
        && let Some(integer) = native_integer_from_decimal(&exact)
    {
        return Some(integer);
    }
    Number::from_f64(value)
}

fn exact_f64_decimal(value: f64) -> Option<CanonicalDecimal> {
    if !value.is_finite() {
        return None;
    }
    let bits = value.to_bits();
    let negative = bits >> 63 != 0;
    let exponent_bits = i32::try_from((bits >> 52) & 0x7ff).ok()?;
    let fraction = bits & ((1_u64 << 52) - 1);
    let (significand, binary_exponent) = if exponent_bits == 0 {
        (fraction, -1_074)
    } else {
        (
            fraction | (1_u64 << 52),
            exponent_bits.checked_sub(1_023)?.checked_sub(52)?,
        )
    };
    if significand == 0 {
        return Some(CanonicalDecimal {
            negative: false,
            digits: "0".to_owned(),
            exponent: 0,
        });
    }

    // Least-significant digit first keeps repeated small multiplication linear.
    let mut digits = significand
        .to_string()
        .bytes()
        .rev()
        .map(|digit| digit - b'0')
        .collect::<Vec<_>>();
    let mut decimal_exponent = 0_i128;
    if binary_exponent >= 0 {
        for _ in 0..binary_exponent {
            multiply_decimal_digits(&mut digits, 2);
        }
    } else {
        let denominator_power = binary_exponent.checked_neg()?;
        for _ in 0..denominator_power {
            multiply_decimal_digits(&mut digits, 5);
        }
        decimal_exponent = -i128::from(denominator_power);
    }
    let trailing_zeros = digits.iter().take_while(|digit| **digit == 0).count();
    digits.drain(..trailing_zeros);
    decimal_exponent = decimal_exponent.checked_add(i128::try_from(trailing_zeros).ok()?)?;
    Some(CanonicalDecimal {
        negative,
        digits: digits
            .iter()
            .rev()
            .map(|digit| char::from(b'0' + *digit))
            .collect(),
        exponent: decimal_exponent,
    })
}

fn multiply_decimal_digits(digits: &mut Vec<u8>, factor: u8) {
    let mut carry = 0_u16;
    for digit in digits.iter_mut() {
        let product = u16::from(*digit) * u16::from(factor) + carry;
        *digit = u8::try_from(product % 10).expect("one decimal digit");
        carry = product / 10;
    }
    while carry != 0 {
        digits.push(u8::try_from(carry % 10).expect("one decimal digit"));
        carry /= 10;
    }
}

fn native_integer_from_decimal(decimal: &CanonicalDecimal) -> Option<Number> {
    let zero_count = usize::try_from(decimal.exponent).ok()?;
    let mut integer = String::with_capacity(
        decimal
            .digits
            .len()
            .checked_add(zero_count)?
            .checked_add(usize::from(decimal.negative))?,
    );
    if decimal.negative {
        integer.push('-');
    }
    integer.push_str(&decimal.digits);
    integer.extend(std::iter::repeat_n('0', zero_count));
    if decimal.negative {
        integer.parse::<i64>().ok().map(Number::from)
    } else {
        integer.parse::<u64>().ok().map(Number::from)
    }
}

fn json_syntax(reason: &'static str) -> ProtocolError {
    ProtocolError::Json(serde_json::Error::io(io::Error::new(
        io::ErrorKind::InvalidData,
        reason,
    )))
}

fn validate_json_at(value: &Value, depth: usize, nodes: &mut usize) -> Result<(), ProtocolError> {
    record_json_node(depth, nodes)?;
    match value {
        Value::Object(object) => {
            for value in object.values() {
                validate_json_at(value, depth + 1, nodes)?;
            }
        }
        Value::Array(array) => {
            for value in array {
                validate_json_at(value, depth + 1, nodes)?;
            }
        }
        Value::Number(number) => validate_json_number(number)?,
        Value::Null | Value::Bool(_) | Value::String(_) => {}
    }
    Ok(())
}

fn canonicalize_json_at(
    value: &Value,
    depth: usize,
    nodes: &mut usize,
) -> Result<Value, ProtocolError> {
    record_json_node(depth, nodes)?;
    match value {
        Value::Object(object) => {
            let mut keys: Vec<_> = object.keys().collect();
            keys.sort_unstable();
            let mut canonical = Map::new();
            for key in keys {
                canonical.insert(
                    key.clone(),
                    canonicalize_json_at(&object[key], depth + 1, nodes)?,
                );
            }
            Ok(Value::Object(canonical))
        }
        Value::Array(array) => array
            .iter()
            .map(|item| canonicalize_json_at(item, depth + 1, nodes))
            .collect::<Result<Vec<_>, _>>()
            .map(Value::Array),
        Value::Number(number) => {
            let raw = number.to_string();
            let represented = machine_number(&raw).ok_or(ProtocolError::LossyJsonNumber)?;
            if !decimal_values_equal(&raw, &represented.to_string()) {
                return Err(ProtocolError::LossyJsonNumber);
            }
            Ok(Value::Number(represented))
        }
        scalar => Ok(scalar.clone()),
    }
}

fn record_json_node(depth: usize, nodes: &mut usize) -> Result<(), ProtocolError> {
    if depth > MAX_JSON_DEPTH {
        return Err(ProtocolError::JsonLimit {
            reason: format!("nesting exceeds {MAX_JSON_DEPTH}"),
        });
    }
    *nodes = nodes.saturating_add(1);
    if *nodes > MAX_JSON_NODES {
        return Err(ProtocolError::JsonLimit {
            reason: format!("node count exceeds {MAX_JSON_NODES}"),
        });
    }
    Ok(())
}

fn encode_canonical<T: Serialize>(value: &T) -> Result<Vec<u8>, ProtocolError> {
    let serialized = serde_json::to_vec(value).map_err(ProtocolError::Json)?;
    require_frame_size(serialized.len())?;
    let canonical = parse_json_exact(&serialized)?;
    let bytes = serde_json::to_vec(&canonical).map_err(ProtocolError::Json)?;
    require_frame_size(bytes.len())?;
    Ok(bytes)
}

#[derive(Default)]
struct ByteCounter {
    bytes: usize,
}

impl io::Write for ByteCounter {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        self.bytes = self
            .bytes
            .checked_add(buffer.len())
            .ok_or_else(|| io::Error::other("serialized JSON length overflow"))?;
        Ok(buffer.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// Counts the exact compact JSON bytes emitted for a serializable value without
/// allocating an intermediate buffer.
///
/// # Errors
///
/// Returns [`ProtocolError::Json`] when serialization fails.
pub fn encoded_json_len(value: &impl Serialize) -> Result<usize, ProtocolError> {
    let mut counter = ByteCounter::default();
    serde_json::to_writer(&mut counter, value).map_err(ProtocolError::Json)?;
    Ok(counter.bytes)
}

fn decode_canonical<T>(
    bytes: &[u8],
    validate: impl FnOnce(&T) -> Result<(), ProtocolError>,
) -> Result<T, ProtocolError>
where
    T: StableEnvelope,
{
    require_frame_size(bytes.len())?;
    let mut canonical = parse_json_exact(bytes)?;
    let stable_values = T::take_stable_values(&mut canonical);
    let canonical = serde_json::to_vec(&canonical).map_err(ProtocolError::Json)?;
    let mut envelope: T = serde_json::from_slice(&canonical).map_err(ProtocolError::Json)?;
    envelope.restore_stable_values(stable_values)?;
    validate(&envelope)?;
    Ok(envelope)
}

// `serde_json` uses private marker objects to transport arbitrary-precision
// numbers and raw values through its data model. Those markers are an internal
// implementation detail and can collide with valid provider-owned object keys
// when another workspace crate enables the feature. Lift only the protocol's
// arbitrary JSON subtrees out before typed deserialization and restore them
// afterward; closed envelope fields still go through the derived DTO parser.
trait StableEnvelope: for<'de> Deserialize<'de> {
    fn take_stable_values(value: &mut Value) -> Vec<Value>;
    fn restore_stable_values(&mut self, values: Vec<Value>) -> Result<(), ProtocolError>;
}

impl StableEnvelope for ToolsEnvelope {
    fn take_stable_values(value: &mut Value) -> Vec<Value> {
        let mut values = Vec::new();
        let Some(envelope) = value.as_object_mut() else {
            return values;
        };
        if let Some("catalog_response") = envelope.get("kind").and_then(Value::as_str)
            && let Some(tools) = envelope.get_mut("tools")
        {
            take_catalog_values(tools, &mut values);
        }
        values
    }

    fn restore_stable_values(&mut self, values: Vec<Value>) -> Result<(), ProtocolError> {
        let mut values = values.into_iter();
        if let ToolsBody::CatalogResponse(response) = &mut self.body {
            restore_catalog_values(&mut response.tools, &mut values)?;
        }
        require_no_stable_values(values)
    }
}

fn take_catalog_values(catalog: &mut Value, values: &mut Vec<Value>) {
    let Some(tools) = catalog.as_array_mut() else {
        return;
    };
    for tool in tools {
        let Some(tool) = tool.as_object_mut() else {
            continue;
        };
        if let Some(schema) = tool.get_mut("input_schema") {
            values.push(std::mem::replace(schema, Value::Bool(true)));
        }
    }
}

fn restore_catalog_values(
    tools: &mut [ToolDefinition],
    values: &mut impl Iterator<Item = Value>,
) -> Result<(), ProtocolError> {
    for tool in tools {
        tool.input_schema = next_stable_value(values)?;
    }
    Ok(())
}

fn next_stable_value(values: &mut impl Iterator<Item = Value>) -> Result<Value, ProtocolError> {
    values
        .next()
        .ok_or_else(|| json_syntax("missing preserved JSON value during envelope decoding"))
}

fn require_no_stable_values(mut values: impl Iterator<Item = Value>) -> Result<(), ProtocolError> {
    if values.next().is_some() {
        return Err(json_syntax(
            "unexpected preserved JSON value during envelope decoding",
        ));
    }
    Ok(())
}

fn validate_json_number(number: &Number) -> Result<(), ProtocolError> {
    if number.is_i64() || number.is_u64() {
        return Ok(());
    }
    let raw = number.to_string();
    let represented = machine_number(&raw).ok_or(ProtocolError::LossyJsonNumber)?;
    if !decimal_values_equal(&raw, &represented.to_string()) {
        return Err(ProtocolError::LossyJsonNumber);
    }
    Ok(())
}

#[derive(Debug, Eq, PartialEq)]
struct CanonicalDecimal {
    negative: bool,
    digits: String,
    exponent: i128,
}

fn decimal_values_equal(left: &str, right: &str) -> bool {
    canonical_decimal(left).is_some_and(|left| canonical_decimal(right) == Some(left))
}

fn canonical_decimal(text: &str) -> Option<CanonicalDecimal> {
    let (negative, unsigned) = text
        .strip_prefix('-')
        .map_or((false, text), |unsigned| (true, unsigned));
    let exponent_start = unsigned.find(['e', 'E']);
    let (mantissa, explicit_exponent) = exponent_start.map_or((unsigned, "0"), |index| {
        (&unsigned[..index], &unsigned[index + 1..])
    });

    let fraction_digits = mantissa
        .split_once('.')
        .map_or(0, |(_, fraction)| fraction.len());
    let digits = mantissa
        .bytes()
        .filter(u8::is_ascii_digit)
        .map(char::from)
        .collect::<String>();
    let without_leading = digits.trim_start_matches('0');
    if without_leading.is_empty() {
        return Some(CanonicalDecimal {
            negative: false,
            digits: "0".to_owned(),
            exponent: 0,
        });
    }
    let coefficient = without_leading.trim_end_matches('0');
    let trailing_zeros = without_leading.len().checked_sub(coefficient.len())?;
    let exponent = parse_decimal_exponent(explicit_exponent)?
        .checked_sub(i128::try_from(fraction_digits).ok()?)?
        .checked_add(i128::try_from(trailing_zeros).ok()?)?;
    Some(CanonicalDecimal {
        negative,
        digits: coefficient.to_owned(),
        exponent,
    })
}

fn parse_decimal_exponent(text: &str) -> Option<i128> {
    let (negative, magnitude) = text.strip_prefix('-').map_or_else(
        || (false, text.strip_prefix('+').unwrap_or(text)),
        |magnitude| (true, magnitude),
    );
    let magnitude = magnitude.trim_start_matches('0');
    if magnitude.is_empty() {
        return Some(0);
    }
    let magnitude = magnitude
        .parse::<i128>()
        .ok()?
        .checked_mul(if negative { -1 } else { 1 })?;
    Some(magnitude)
}

fn parse_wire_version(text: &str) -> Result<u32, ProtocolError> {
    text.parse::<u32>()
        .map_err(|_| json_syntax("wire version must be an unsigned JSON integer"))
}

fn validate_header(
    protocol: &str,
    expected: &'static str,
    version: u32,
    request_id: &str,
) -> Result<(), ProtocolError> {
    if protocol != expected {
        return Err(ProtocolError::UnsupportedProtocol {
            expected,
            found: protocol.to_owned(),
        });
    }
    if version != WIRE_VERSION {
        return Err(ProtocolError::UnsupportedVersion { found: version });
    }
    require_identifier("request_id", request_id)
}

fn deserialize_wire_version<'de, D>(deserializer: D) -> Result<u32, D::Error>
where
    D: Deserializer<'de>,
{
    let raw = Box::<RawValue>::deserialize(deserializer)?;
    parse_wire_version(raw.get()).map_err(de::Error::custom)
}

fn validate_catalog(tools: &[ToolDefinition], field: &str) -> Result<(), ProtocolError> {
    if tools.len() > MAX_TOOLS {
        return invalid(field, format!("contains more than {MAX_TOOLS} tools"));
    }
    let mut names = BTreeSet::new();
    for (index, tool) in tools.iter().enumerate() {
        tool.validate(&format!("{field}[{index}]"))?;
        if !names.insert(tool.name.as_str()) {
            return invalid(field, "contains duplicate tool names");
        }
    }
    // Canonical key ordering changes only member order, never compact JSON
    // length, so a counting writer enforces the aggregate bound without
    // allocating and reparsing a temporary catalog encoding.
    let encoded_bytes = encoded_json_len(&tools)?;
    if encoded_bytes > MAX_CATALOG_BYTES {
        return invalid(
            field,
            format!("canonical encoding exceeds {MAX_CATALOG_BYTES} bytes"),
        );
    }
    Ok(())
}

fn require_frame_size(actual: usize) -> Result<(), ProtocolError> {
    if actual > MAX_DATA_BYTES {
        return Err(ProtocolError::PayloadTooLarge {
            actual,
            maximum: MAX_DATA_BYTES,
        });
    }
    Ok(())
}

fn require_identifier(field: &str, value: &str) -> Result<(), ProtocolError> {
    if !is_wire_identifier(value) {
        return invalid(
            field,
            format!("must be 1..={MAX_ID_BYTES} non-whitespace printable ASCII bytes"),
        );
    }
    Ok(())
}

fn require_tool_name(field: &str, value: &str) -> Result<(), ProtocolError> {
    if value.is_empty()
        || value.len() > MAX_ID_BYTES
        || !value.bytes().enumerate().all(|(index, byte)| {
            byte.is_ascii_alphanumeric() || (index > 0 && matches!(byte, b'.' | b'_' | b'-'))
        })
    {
        return invalid(
            field,
            "must start with an ASCII alphanumeric and contain only ASCII alphanumerics, '.', '_', or '-'",
        );
    }
    Ok(())
}

fn require_code(field: &str, value: &str) -> Result<(), ProtocolError> {
    if value.is_empty()
        || value.len() > MAX_ERROR_CODE_BYTES
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
    {
        return invalid(field, "is outside the bounded error-code syntax");
    }
    Ok(())
}

fn require_sha256(field: &str, value: &str) -> Result<(), ProtocolError> {
    if value.len() != 64
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
    {
        return invalid(field, "must contain exactly 64 lowercase hexadecimal bytes");
    }
    Ok(())
}

fn require_text(
    field: &str,
    value: &str,
    maximum: usize,
    allow_empty: bool,
) -> Result<(), ProtocolError> {
    let mut characters = 0_usize;
    let mut contains_forbidden = false;
    for character in value.chars() {
        characters = characters.saturating_add(1);
        contains_forbidden |= character == '\0' || character == '\u{007f}';
    }
    if !allow_empty && characters == 0 {
        return invalid(field, "must not be empty");
    }
    if characters > maximum {
        return invalid(
            field,
            format!("must contain at most {maximum} Unicode scalar values"),
        );
    }
    if contains_forbidden {
        return invalid(field, "must not contain NUL or DEL");
    }
    Ok(())
}

fn require_path_text(field: &str, value: &str) -> Result<(), ProtocolError> {
    require_text(field, value, MAX_PATH_CHARS, false)?;
    if value.chars().any(char::is_control) {
        return invalid(field, "must not contain Unicode control characters");
    }
    Ok(())
}

fn invalid<T>(field: impl Into<String>, reason: impl Into<String>) -> Result<T, ProtocolError> {
    Err(ProtocolError::InvalidField {
        field: field.into(),
        reason: reason.into(),
    })
}

/// Rejection returned at the semantic protocol boundary.
#[derive(Debug, Error)]
pub enum ProtocolError {
    #[error("invalid protocol JSON: {0}")]
    Json(#[source] serde_json::Error),
    #[error("unsupported protocol `{found}`; expected `{expected}`")]
    UnsupportedProtocol {
        expected: &'static str,
        found: String,
    },
    #[error("unsupported protocol version {found}")]
    UnsupportedVersion { found: u32 },
    #[error("field `{field}` {reason}")]
    InvalidField { field: String, reason: String },
    #[error("JSON object contains duplicate key `{key}`")]
    DuplicateJsonKey { key: String },
    #[error("JSON value exceeds the version-one complexity bound: {reason}")]
    JsonLimit { reason: String },
    #[error("JSON number cannot be represented without changing its decimal value")]
    LossyJsonNumber,
    #[error("semantic DATA payload is {actual} bytes; maximum is {maximum}")]
    PayloadTooLarge { actual: usize, maximum: usize },
    #[error("invalid RAT1 binary frame: {reason}")]
    InvalidBinaryFrame { reason: String },
}
