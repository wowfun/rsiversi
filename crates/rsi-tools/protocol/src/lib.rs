//! Runtime-independent process-local tool contracts.

#![deny(unsafe_code)]
#![warn(missing_docs)]
#![allow(clippy::missing_errors_doc)]

use async_trait::async_trait;
use rsi_media_protocol::MediaRef;
use rsi_meta_contract::LocalContract;
use rsi_sandbox::{ConfinedProcess, EnforcementStamp, ProcessRequest, Sandbox, SandboxMode};
use serde::de::{DeserializeSeed, MapAccess, SeqAccess, Visitor};
use serde::{Deserialize, Serialize};
use serde_json::{Number, Value};
use std::collections::BTreeSet;
use std::fmt;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::Mutex;
use thiserror::Error;
use tokio_util::sync::CancellationToken;

/// Maximum tool name or call identity bytes.
pub const MAXIMUM_TOOL_IDENTIFIER_BYTES: usize = 256;
/// Maximum encoded schema, arguments, or canonical result bytes.
pub const MAXIMUM_TOOL_JSON_BYTES: usize = 4 * 1024 * 1024;
/// Maximum model-facing text bytes in one result.
pub const MAXIMUM_TOOL_TEXT_BYTES: usize = 4 * 1024 * 1024;
/// Maximum ordered content items.
pub const MAXIMUM_TOOL_CONTENT_ITEMS: usize = 256;
/// Maximum model-visible description bytes.
pub const MAXIMUM_TOOL_DESCRIPTION_BYTES: usize = 4 * 1024;
/// Maximum provider-neutral freeform grammar bytes.
pub const MAXIMUM_FREEFORM_GRAMMAR_BYTES: usize = 64 * 1024;
/// Maximum active definitions in one Tool Runtime generation.
pub const MAXIMUM_REGISTERED_TOOLS: usize = 64;
/// Maximum cooperative timeout for one Tool call in milliseconds.
pub const MAXIMUM_TOOL_TIMEOUT_MS: u64 = 600_000;
/// Maximum active-or-retained invocations across one Tool catalog provider.
pub const MAXIMUM_ADMITTED_TOOL_INVOCATIONS: usize = 1_024;
/// Maximum unpublished and sealed catalogs retained by one provider.
pub const MAXIMUM_TOOL_CATALOGS: usize = 1_024;
/// Maximum truthful process-enforcement records in one Tool result.
pub const MAXIMUM_TOOL_ENFORCEMENT_STAMPS: usize = 256;
/// Maximum nested containers in model-produced Tool arguments.
pub const MAXIMUM_TOOL_JSON_DEPTH: usize = 64;
/// Maximum values and containers in model-produced Tool arguments.
pub const MAXIMUM_TOOL_JSON_NODES: usize = 100_000;

/// Provider-neutral tool declaration shared by Tools, AI, and Agent.
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
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct WireDefinition {
            name: String,
            description: String,
            input_schema: Value,
            #[serde(default)]
            freeform: Option<FreeformToolDefinition>,
        }

        let wire = WireDefinition::deserialize(deserializer)?;
        let definition = Self {
            name: wire.name,
            description: wire.description,
            input_schema: wire.input_schema,
            freeform: wire.freeform,
        };
        definition
            .validate()
            .map(|()| definition)
            .map_err(serde::de::Error::custom)
    }
}

impl ToolDefinition {
    /// Creates a bounded function-tool definition.
    pub fn new(
        name: impl Into<String>,
        description: impl Into<String>,
        input_schema: Value,
    ) -> Result<Self> {
        let definition = Self {
            name: name.into(),
            description: description.into(),
            input_schema,
            freeform: None,
        };
        definition.validate()?;
        Ok(definition)
    }

    /// Returns the exact model-visible name.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Returns the model-visible description.
    pub fn description(&self) -> &str {
        &self.description
    }

    /// Returns the bounded object or boolean JSON Schema.
    pub const fn input_schema(&self) -> &Value {
        &self.input_schema
    }

    /// Adds a provider-neutral freeform grammar projection.
    pub fn with_freeform(mut self, freeform: FreeformToolDefinition) -> Result<Self> {
        self.freeform = Some(freeform);
        self.validate()?;
        Ok(self)
    }

    /// Returns the optional freeform projection.
    pub const fn freeform(&self) -> Option<&FreeformToolDefinition> {
        self.freeform.as_ref()
    }

    /// Revalidates a definition decoded from a durable or external boundary.
    pub fn validate(&self) -> Result<()> {
        validate_model_tool_name(&self.name)?;
        validate_safe_text(
            "tool description",
            &self.description,
            MAXIMUM_TOOL_DESCRIPTION_BYTES,
            true,
        )?;
        if !matches!(self.input_schema, Value::Object(_) | Value::Bool(_)) {
            return Err(ToolError::InvalidInput(
                "tool input schema must be an object or boolean JSON Schema".into(),
            ));
        }
        validate_json("tool input schema", &self.input_schema)?;
        if let Some(freeform) = &self.freeform {
            freeform.validate()?;
        }
        Ok(())
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
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
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

/// Closed freeform grammar families understood by provider adapters.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FreeformFormat {
    /// Lark grammar used by Responses custom tools.
    Lark,
}

impl FreeformToolDefinition {
    /// Creates a bounded freeform grammar.
    pub fn new(format: FreeformFormat, grammar: impl Into<String>) -> Result<Self> {
        let definition = Self {
            format,
            grammar: grammar.into(),
        };
        definition.validate()?;
        Ok(definition)
    }

    /// Returns the grammar family.
    pub const fn format(&self) -> FreeformFormat {
        self.format
    }

    /// Returns the exact grammar text.
    pub fn grammar(&self) -> &str {
        &self.grammar
    }

    /// Revalidates a decoded freeform definition.
    pub fn validate(&self) -> Result<()> {
        validate_safe_text(
            "tool freeform grammar",
            &self.grammar,
            MAXIMUM_FREEFORM_GRAMMAR_BYTES,
            false,
        )
    }
}

/// Exact sandbox inputs pinned by the orchestrator before Tool execution.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ToolExecutionPolicy {
    /// Exact resolved sandbox mode.
    pub mode: SandboxMode,
    /// Canonical working directory.
    pub cwd: PathBuf,
    /// Canonical writable-workspace boundary.
    pub workspace: PathBuf,
}

impl<'de> Deserialize<'de> for ToolExecutionPolicy {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct WirePolicy {
            mode: SandboxMode,
            cwd: PathBuf,
            workspace: PathBuf,
        }

        let wire = WirePolicy::deserialize(deserializer)?;
        let policy = Self {
            mode: wire.mode,
            cwd: wire.cwd,
            workspace: wire.workspace,
        };
        policy
            .validate()
            .map(|()| policy)
            .map_err(serde::de::Error::custom)
    }
}

impl ToolExecutionPolicy {
    /// Validates process-plan paths before Tool preparation is started.
    pub fn validate(&self) -> Result<()> {
        if !self.cwd.is_absolute() || !self.workspace.is_absolute() {
            return Err(ToolError::InvalidInput(
                "Tool sandbox paths must be absolute".into(),
            ));
        }
        Ok(())
    }
}

/// Start-time authority supplied to one prepared Tool call.
#[derive(Clone, Debug)]
pub struct ToolStart {
    /// Cooperative turn cancellation.
    pub cancellation: CancellationToken,
    /// Exact durable execution policy.
    pub policy: ToolExecutionPolicy,
    /// Active sandbox planner generation.
    pub sandbox: Arc<dyn Sandbox>,
    /// Exact optional Jobs scope authority for this Agent invocation.
    pub job_scope: Option<rsi_jobs::JobScopeAuthority>,
}

/// Execution supplied to one trusted tool body.
#[derive(Clone, Debug)]
pub struct ToolExecution {
    /// Exact call identity.
    pub call_id: String,
    /// Cooperative cancellation token.
    pub cancellation: CancellationToken,
    policy: ToolExecutionPolicy,
    sandbox: Arc<dyn Sandbox>,
    job_scope: Option<rsi_jobs::JobScopeAuthority>,
    enforcement: Arc<Mutex<Vec<EnforcementStamp>>>,
}

impl ToolExecution {
    /// Returns the exact orchestrator-pinned process policy.
    pub const fn policy(&self) -> &ToolExecutionPolicy {
        &self.policy
    }

    /// Returns the exact optional Jobs scope authority pinned at Tool start.
    pub const fn job_scope(&self) -> Option<&rsi_jobs::JobScopeAuthority> {
        self.job_scope.as_ref()
    }

    /// Confines one process using the pinned mode and workspace authority.
    pub async fn confine(
        &self,
        program: PathBuf,
        arguments: Vec<String>,
    ) -> Result<ConfinedProcess> {
        let confined = self
            .sandbox
            .confine(ProcessRequest {
                mode: self.policy.mode,
                program,
                arguments,
                cwd: self.policy.cwd.clone(),
                workspace: self.policy.workspace.clone(),
            })
            .await
            .map_err(ToolError::Sandbox)?;
        let mut enforcement = self
            .enforcement
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if enforcement.len() >= MAXIMUM_TOOL_ENFORCEMENT_STAMPS {
            return Err(ToolError::Execution(
                "Tool enforcement record capacity is exhausted".into(),
            ));
        }
        enforcement.push(confined.stamp.clone());
        Ok(confined)
    }

    /// Creates one execution and its caller-owned enforcement collector.
    pub fn from_start(call_id: String, start: ToolStart) -> Result<(Self, ToolEnforcement)> {
        start.policy.validate()?;
        let enforcement = Arc::new(Mutex::new(Vec::new()));
        Ok((
            Self {
                call_id,
                cancellation: start.cancellation,
                policy: start.policy,
                sandbox: start.sandbox,
                job_scope: start.job_scope,
                enforcement: Arc::clone(&enforcement),
            },
            ToolEnforcement { enforcement },
        ))
    }
}

/// Opaque caller-owned collector for truthful confinement stamps.
#[derive(Debug)]
pub struct ToolEnforcement {
    enforcement: Arc<Mutex<Vec<EnforcementStamp>>>,
}

impl ToolEnforcement {
    /// Moves every recorded stamp into the settled Tool result.
    pub fn attach(self, mut result: ToolResult) -> Result<ToolResult> {
        result.enforcement.clone_from(
            &self
                .enforcement
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
        );
        result.validate()?;
        Ok(result)
    }
}

/// Model-facing ordered content.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ToolContent {
    /// UTF-8 text.
    Text {
        /// Exact model-facing text.
        text: String,
    },
    /// Durable image reference.
    Image {
        /// Immutable Media reference.
        media: MediaRef,
    },
}

/// Complete bounded tool outcome suitable for durable Agent Facts.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ToolResult {
    /// Canonical JSON value for programmatic consumers.
    pub value: Value,
    /// Ordered model-facing content.
    pub content: Vec<ToolContent>,
    /// Whether this is a tool-owned error result.
    pub is_error: bool,
    /// Truthful process confinement selected through [`ToolExecution::confine`].
    pub enforcement: Vec<EnforcementStamp>,
}

impl<'de> Deserialize<'de> for ToolResult {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct WireResult {
            value: Value,
            content: Vec<ToolContent>,
            is_error: bool,
            enforcement: Vec<EnforcementStamp>,
        }

        let wire = WireResult::deserialize(deserializer)?;
        let result = Self {
            value: wire.value,
            content: wire.content,
            is_error: wire.is_error,
            enforcement: wire.enforcement,
        };
        result
            .validate()
            .map(|()| result)
            .map_err(serde::de::Error::custom)
    }
}

impl ToolResult {
    /// Creates and validates a complete Tool result.
    pub fn new(value: Value, content: Vec<ToolContent>, is_error: bool) -> Result<Self> {
        let result = Self {
            value,
            content,
            is_error,
            enforcement: Vec::new(),
        };
        result.validate()?;
        Ok(result)
    }

    /// Revalidates a result decoded from a durable boundary.
    pub fn validate(&self) -> Result<()> {
        validate_json("tool canonical result", &self.value)?;
        if self.content.len() > MAXIMUM_TOOL_CONTENT_ITEMS {
            return Err(ToolError::InvalidInput(
                "tool result contains too many content items".into(),
            ));
        }
        let mut text_bytes = 0_usize;
        for content in &self.content {
            match content {
                ToolContent::Text { text } => {
                    validate_safe_text("tool result text", text, MAXIMUM_TOOL_TEXT_BYTES, true)?;
                    text_bytes = text_bytes.checked_add(text.len()).ok_or_else(|| {
                        ToolError::InvalidInput("tool result text length overflow".into())
                    })?;
                    if text_bytes > MAXIMUM_TOOL_TEXT_BYTES {
                        return Err(ToolError::InvalidInput(
                            "tool result text is too large".into(),
                        ));
                    }
                }
                ToolContent::Image { media } => media
                    .validate()
                    .map_err(|error| ToolError::InvalidInput(error.to_string()))?,
            }
        }
        if self.enforcement.len() > MAXIMUM_TOOL_ENFORCEMENT_STAMPS {
            return Err(ToolError::InvalidInput(
                "tool result contains too many enforcement stamps".into(),
            ));
        }
        for stamp in &self.enforcement {
            stamp
                .validate()
                .map_err(|error| ToolError::InvalidInput(error.to_string()))?;
        }
        Ok(())
    }
}

/// Trusted process-local tool body.
#[async_trait]
pub trait ToolExecutor: fmt::Debug + Send + Sync + 'static {
    /// Executes one validated bounded argument value.
    async fn execute(&self, arguments: Value, execution: ToolExecution) -> Result<ToolResult>;
}

/// One process-local implementation registered for a model-visible definition.
#[derive(Clone)]
pub struct ToolRegistration {
    /// Model-visible definition.
    pub definition: ToolDefinition,
    /// Cooperative timeout within `1..=MAXIMUM_TOOL_TIMEOUT_MS` milliseconds.
    pub timeout_ms: u64,
    /// Trusted body.
    pub executor: Arc<dyn ToolExecutor>,
}

impl fmt::Debug for ToolRegistration {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ToolRegistration")
            .field("definition", &self.definition)
            .field("timeout_ms", &self.timeout_ms)
            .field("executor", &"<tool executor>")
            .finish()
    }
}

/// One model-produced tool call.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ToolCall {
    /// Exact call identity.
    pub id: String,
    /// Exact registered tool name.
    pub name: String,
    /// Canonical JSON arguments.
    pub arguments: Value,
}

impl<'de> Deserialize<'de> for ToolCall {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct WireCall {
            id: String,
            name: String,
            arguments: Value,
        }

        let wire = WireCall::deserialize(deserializer)?;
        let call = Self {
            id: wire.id,
            name: wire.name,
            arguments: wire.arguments,
        };
        call.validate()
            .map(|()| call)
            .map_err(serde::de::Error::custom)
    }
}

impl ToolCall {
    /// Revalidates one model-produced call.
    pub fn validate(&self) -> Result<()> {
        validate_identifier("tool call id", &self.id)?;
        validate_model_tool_name(&self.name)?;
        validate_json("tool arguments", &self.arguments)
    }
}

/// Exact retained-result identity frozen before Tool execution starts.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ToolResultIdentity {
    owner_id: String,
    invocation_id: String,
    call_id: String,
    request_sha256: String,
}

impl<'de> Deserialize<'de> for ToolResultIdentity {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct WireIdentity {
            owner_id: String,
            invocation_id: String,
            call_id: String,
            request_sha256: String,
        }

        let wire = WireIdentity::deserialize(deserializer)?;
        Self::new(
            wire.owner_id,
            wire.invocation_id,
            wire.call_id,
            wire.request_sha256,
        )
        .map_err(serde::de::Error::custom)
    }
}

impl ToolResultIdentity {
    /// Creates a validated exact retained-result identity.
    pub fn new(
        owner_id: impl Into<String>,
        invocation_id: impl Into<String>,
        call_id: impl Into<String>,
        request_sha256: impl Into<String>,
    ) -> Result<Self> {
        let identity = Self {
            owner_id: owner_id.into(),
            invocation_id: invocation_id.into(),
            call_id: call_id.into(),
            request_sha256: request_sha256.into(),
        };
        validate_identifier("tool result owner", &identity.owner_id)?;
        validate_identifier("tool invocation", &identity.invocation_id)?;
        validate_identifier("tool call id", &identity.call_id)?;
        if identity.request_sha256.len() != 64
            || !identity
                .request_sha256
                .bytes()
                .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
        {
            return Err(ToolError::InvalidInput(
                "tool request digest must be lowercase SHA-256 hex".into(),
            ));
        }
        Ok(identity)
    }

    /// Returns the exact Tool registration generation.
    pub fn owner_id(&self) -> &str {
        &self.owner_id
    }

    /// Returns the orchestrator-owned invocation identity.
    pub fn invocation_id(&self) -> &str {
        &self.invocation_id
    }

    /// Returns the model-produced call identity.
    pub fn call_id(&self) -> &str {
        &self.call_id
    }

    /// Returns the canonical request digest.
    pub fn request_sha256(&self) -> &str {
        &self.request_sha256
    }
}

/// Bounded failure retained after an invocation settled without a Tool result.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RetainedToolFailure {
    /// Stable failure category.
    pub kind: RetainedToolFailureKind,
    /// Bounded diagnostic without secrets or provider values.
    pub summary: String,
}

/// Stable retained Tool failure categories.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RetainedToolFailureKind {
    /// Caller cancellation won and the body settled.
    Cancelled,
    /// Registration timeout won and the body settled.
    Timeout,
    /// Tool body or result validation failed.
    Execution,
}

/// Point-in-time state for an exact retained Tool invocation.
#[derive(Clone, Debug, PartialEq)]
pub enum RetainedToolResult {
    /// No invocation with that exact identity exists in this runtime generation.
    Absent,
    /// Invocation started but has not settled.
    Pending,
    /// Invocation produced a complete bounded result.
    Returned(ToolResult),
    /// Invocation settled without a Tool result.
    Failed(RetainedToolFailure),
}

/// One Tool call pinned to the registration generation resolved at prepare.
#[async_trait]
pub trait PreparedToolCall: fmt::Debug + Send + 'static {
    /// Returns the immutable exact retained-result identity.
    fn identity(&self) -> &ToolResultIdentity;
    /// Starts the Tool at most once and retains its eventual result.
    async fn start(self: Box<Self>, start: ToolStart) -> Result<ToolResult>;
}

/// Closed tool failure taxonomy.
#[derive(Clone, Debug, Error, Eq, PartialEq)]
pub enum ToolError {
    /// Malformed or out-of-bounds input/output.
    #[error("invalid tool value: {0}")]
    InvalidInput(String),
    /// Duplicate exact-name registration.
    #[error("tool `{0}` is already registered")]
    Duplicate(String),
    /// No active exact-name definition.
    #[error("tool `{0}` is not registered")]
    Unknown(String),
    /// The exact registration generation pinned at prepare was withdrawn before start.
    #[error("tool `{0}` registration was withdrawn")]
    Withdrawn(String),
    /// The unpublished catalog stage was already sealed or abandoned.
    #[error("Tool catalog stage is sealed")]
    Sealed,
    /// A provider-wide catalog or active-or-retained invocation bound is exhausted.
    #[error("Tool capacity is exhausted")]
    Capacity,
    /// The Tool catalog provider has begun shutdown.
    #[error("Tool catalog provider is shutting down")]
    ShuttingDown,
    /// Sandbox planning rejected the process before spawn.
    #[error(transparent)]
    Sandbox(rsi_sandbox::SandboxError),
    /// Call cancellation won and the body has settled.
    #[error("tool call was cancelled")]
    Cancelled,
    /// Tool timeout won and the body has settled.
    #[error("tool call timed out")]
    Timeout,
    /// Tool body returned a failure.
    #[error("tool execution failed: {0}")]
    Execution(String),
}

/// Tool result.
pub type Result<T> = std::result::Result<T, ToolError>;

/// Write-only exact-name registration interface for one unpublished catalog.
pub trait ToolRegistrar: fmt::Debug + Send + Sync + 'static {
    /// Registers one definition that its lease can withdraw while the stage is open.
    fn register(&self, registration: ToolRegistration) -> Result<ToolLease>;
    /// Atomically registers one nonempty batch, withdrawable only before sealing.
    fn register_batch(&self, registrations: Vec<ToolRegistration>) -> Result<ToolBatchLease>;
}

/// One unpublished Tool catalog stage.
pub trait ToolCatalogStage: fmt::Debug + Send + 'static {
    /// Returns the write-only registrar for this exact stage.
    fn registrar(&self) -> Arc<dyn ToolRegistrar>;
    /// Consumes and seals the complete catalog into one immutable runtime.
    fn seal(self: Box<Self>) -> Result<Arc<dyn ToolRuntime>>;
}

/// Process-wide owner of bounded Tool catalogs and retained results.
pub trait ToolCatalogProvider: fmt::Debug + Send + Sync + 'static {
    /// Begins one unpublished bounded catalog stage.
    fn begin_stage(&self) -> Result<Box<dyn ToolCatalogStage>>;
}

/// Immutable exact-name Tool execution authority.
#[async_trait]
pub trait ToolRuntime: fmt::Debug + Send + Sync + 'static {
    /// Returns ordered model-visible definitions of the active tools.
    fn definitions(&self) -> Vec<ToolDefinition>;
    /// Resolves and pins one call without starting external Tool code.
    fn prepare(&self, invocation_id: &str, call: ToolCall) -> Result<Box<dyn PreparedToolCall>>;
    /// Queries one exact invocation retained by this runtime generation.
    fn query(&self, identity: &ToolResultIdentity) -> Result<RetainedToolResult>;
    /// Waits until one exact invocation is absent or settled, or cancellation fires.
    async fn wait(
        &self,
        identity: &ToolResultIdentity,
        cancellation: CancellationToken,
    ) -> Result<RetainedToolResult>;
    /// Retires one exact settled invocation after its outcome is durable.
    fn commit(&self, identity: &ToolResultIdentity) -> Result<()>;
}

/// Nominal Local contract for [`ToolRegistrar`].
#[derive(Debug)]
pub struct ToolRegistrarContract;

impl LocalContract for ToolRegistrarContract {
    const KEY: &'static str = "rsi.tools.registrar";
    type Service = dyn ToolRegistrar;
}

/// Nominal Local contract for [`ToolCatalogProvider`].
#[derive(Debug)]
pub struct ToolCatalogProviderContract;

impl LocalContract for ToolCatalogProviderContract {
    const KEY: &'static str = "rsi.tools.catalog";
    type Service = dyn ToolCatalogProvider;
}

/// Opaque atomic registration-batch lease.
///
/// Dropping or retiring withdraws the exact batch only while its catalog stage
/// remains open. Once sealed, the immutable catalog owns the registered
/// executors and calls, so releasing this contributor lease has no effect.
pub struct ToolBatchLease {
    withdraw: Option<Box<dyn FnOnce() + Send + Sync + 'static>>,
}

impl ToolBatchLease {
    /// Creates a lease from one exact open-stage withdrawal action.
    pub fn new<F>(withdraw: F) -> Self
    where
        F: FnOnce() + Send + Sync + 'static,
    {
        Self {
            withdraw: Some(Box::new(withdraw)),
        }
    }

    /// Withdraws the complete batch if its catalog stage is still open.
    pub fn retire(mut self) -> Result<()> {
        if let Some(withdraw) = self.withdraw.take() {
            withdraw();
        }
        Ok(())
    }
}

impl fmt::Debug for ToolBatchLease {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("ToolBatchLease(..)")
    }
}

impl Drop for ToolBatchLease {
    fn drop(&mut self) {
        if let Some(withdraw) = self.withdraw.take() {
            withdraw();
        }
    }
}

/// Opaque registration lease.
pub struct ToolLease {
    batch: Option<ToolBatchLease>,
}

impl ToolLease {
    /// Creates a single-definition lease with batch-equivalent withdrawal semantics.
    pub fn new(batch: ToolBatchLease) -> Self {
        Self { batch: Some(batch) }
    }

    /// Withdraws the definition if its catalog stage is still open.
    pub fn retire(mut self) -> Result<()> {
        match self.batch.take() {
            Some(batch) => batch.retire(),
            None => Ok(()),
        }
    }
}

impl fmt::Debug for ToolLease {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("ToolLease(..)")
    }
}

impl Drop for ToolLease {
    fn drop(&mut self) {
        drop(self.batch.take());
    }
}

/// Validates one exact tool or call identifier.
pub fn validate_identifier(kind: &str, value: &str) -> Result<()> {
    if value.is_empty()
        || value.len() > MAXIMUM_TOOL_IDENTIFIER_BYTES
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-' | b':'))
    {
        return Err(ToolError::InvalidInput(format!(
            "{kind} must be a nonempty bounded ASCII identifier"
        )));
    }
    Ok(())
}

fn validate_model_tool_name(value: &str) -> Result<()> {
    if value.is_empty()
        || value.len() > MAXIMUM_TOOL_IDENTIFIER_BYTES
        || !value.bytes().enumerate().all(|(index, byte)| {
            byte.is_ascii_alphanumeric() || (index > 0 && matches!(byte, b'.' | b'_' | b'-'))
        })
    {
        return Err(ToolError::InvalidInput(
            "tool name must start with an ASCII alphanumeric and contain only ASCII alphanumerics, '.', '_', or '-'".into(),
        ));
    }
    Ok(())
}

fn validate_safe_text(
    kind: &str,
    value: &str,
    maximum_bytes: usize,
    allow_empty: bool,
) -> Result<()> {
    if (!allow_empty && value.is_empty())
        || value.len() > maximum_bytes
        || value.chars().any(|character| {
            (character.is_ascii_control() && !matches!(character, '\t' | '\n' | '\r'))
                || character == '\u{7f}'
        })
    {
        return Err(ToolError::InvalidInput(format!(
            "{kind} must contain {}..={maximum_bytes} safe UTF-8 bytes",
            usize::from(!allow_empty)
        )));
    }
    Ok(())
}

/// Validates one bounded JSON value.
pub fn validate_json(kind: &str, value: &Value) -> Result<()> {
    let mut nodes = 0usize;
    let mut pending = vec![(value, 0usize)];
    while let Some((value, depth)) = pending.pop() {
        if depth > MAXIMUM_TOOL_JSON_DEPTH {
            return Err(ToolError::InvalidInput(format!(
                "{kind} exceeds the JSON depth bound of {MAXIMUM_TOOL_JSON_DEPTH}"
            )));
        }
        nodes = nodes
            .checked_add(1)
            .ok_or_else(|| ToolError::InvalidInput(format!("{kind} node count overflowed")))?;
        if nodes > MAXIMUM_TOOL_JSON_NODES {
            return Err(ToolError::InvalidInput(format!(
                "{kind} exceeds the JSON node bound of {MAXIMUM_TOOL_JSON_NODES}"
            )));
        }
        match value {
            Value::Array(values) => {
                pending.extend(values.iter().map(|value| (value, depth + 1)));
            }
            Value::Object(values) => {
                pending.extend(values.values().map(|value| (value, depth + 1)));
            }
            Value::Number(number) => validate_json_number(kind, number)?,
            Value::Null | Value::Bool(_) | Value::String(_) => {}
        }
    }

    let bytes =
        serde_json::to_vec(value).map_err(|error| ToolError::InvalidInput(error.to_string()))?;
    if bytes.len() > MAXIMUM_TOOL_JSON_BYTES {
        return Err(ToolError::InvalidInput(format!(
            "{kind} exceeds {MAXIMUM_TOOL_JSON_BYTES} bytes"
        )));
    }
    Ok(())
}

/// Parses one exact JSON value while preserving duplicate-key evidence.
pub fn parse_tool_arguments(text: &str) -> Result<Value> {
    if text.is_empty() || text.len() > MAXIMUM_TOOL_JSON_BYTES {
        return Err(ToolError::InvalidInput(format!(
            "tool arguments must contain 1..={MAXIMUM_TOOL_JSON_BYTES} JSON bytes"
        )));
    }
    let mut deserializer = serde_json::Deserializer::from_str(text);
    let mut budget = JsonBudget { nodes: 0 };
    // This first pass preserves duplicate-key evidence that `Value` drops. Its
    // result is deliberately not authoritative: with `arbitrary_precision`,
    // serde_json presents non-i64/u64 numbers to custom visitors through a
    // private tagged map. Reparse through serde_json's own `Value` visitor to
    // preserve the exact Number, then validate that canonical representation.
    let _duplicate_checked = StrictValueSeed {
        depth: 0,
        budget: &mut budget,
    }
    .deserialize(&mut deserializer)
    .map_err(|error| ToolError::InvalidInput(error.to_string()))?;
    deserializer
        .end()
        .map_err(|error| ToolError::InvalidInput(error.to_string()))?;
    let value =
        serde_json::from_str(text).map_err(|error| ToolError::InvalidInput(error.to_string()))?;
    validate_json("tool arguments", &value)?;
    Ok(value)
}

fn validate_json_number(kind: &str, number: &Number) -> Result<()> {
    if number.is_i64() || number.is_u64() {
        return Ok(());
    }
    let raw = number.to_string();
    let represented = machine_number(&raw).ok_or_else(|| {
        ToolError::InvalidInput(format!(
            "{kind} contains a number that cannot be represented"
        ))
    })?;
    if !decimal_values_equal(&raw, &represented.to_string()) {
        return Err(ToolError::InvalidInput(format!(
            "{kind} contains a number that cannot round-trip exactly"
        )));
    }
    Ok(())
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
    magnitude
        .parse::<i128>()
        .ok()?
        .checked_mul(if negative { -1 } else { 1 })
}

struct JsonBudget {
    nodes: usize,
}

struct StrictValueSeed<'budget> {
    depth: usize,
    budget: &'budget mut JsonBudget,
}

impl<'de> DeserializeSeed<'de> for StrictValueSeed<'_> {
    type Value = Value;

    fn deserialize<D>(self, deserializer: D) -> std::result::Result<Self::Value, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        if self.depth > MAXIMUM_TOOL_JSON_DEPTH {
            return Err(serde::de::Error::custom(
                "tool arguments exceed the JSON depth bound",
            ));
        }
        self.budget.nodes = self
            .budget
            .nodes
            .checked_add(1)
            .ok_or_else(|| serde::de::Error::custom("tool JSON node count overflowed"))?;
        if self.budget.nodes > MAXIMUM_TOOL_JSON_NODES {
            return Err(serde::de::Error::custom(
                "tool arguments exceed the JSON node bound",
            ));
        }
        deserializer.deserialize_any(StrictValueVisitor {
            depth: self.depth,
            budget: self.budget,
        })
    }
}

struct StrictValueVisitor<'budget> {
    depth: usize,
    budget: &'budget mut JsonBudget,
}

impl<'de> Visitor<'de> for StrictValueVisitor<'_> {
    type Value = Value;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("one bounded JSON value")
    }

    fn visit_unit<E>(self) -> std::result::Result<Self::Value, E> {
        Ok(Value::Null)
    }

    fn visit_none<E>(self) -> std::result::Result<Self::Value, E> {
        Ok(Value::Null)
    }

    fn visit_bool<E>(self, value: bool) -> std::result::Result<Self::Value, E> {
        Ok(Value::Bool(value))
    }

    fn visit_i64<E>(self, value: i64) -> std::result::Result<Self::Value, E> {
        Ok(Value::Number(value.into()))
    }

    fn visit_u64<E>(self, value: u64) -> std::result::Result<Self::Value, E> {
        Ok(Value::Number(value.into()))
    }

    fn visit_f64<E>(self, value: f64) -> std::result::Result<Self::Value, E>
    where
        E: serde::de::Error,
    {
        serde_json::Number::from_f64(value)
            .map(Value::Number)
            .ok_or_else(|| E::custom("tool arguments contain a non-finite number"))
    }

    fn visit_str<E>(self, value: &str) -> std::result::Result<Self::Value, E> {
        Ok(Value::String(value.into()))
    }

    fn visit_string<E>(self, value: String) -> std::result::Result<Self::Value, E> {
        Ok(Value::String(value))
    }

    fn visit_seq<A>(self, mut sequence: A) -> std::result::Result<Self::Value, A::Error>
    where
        A: SeqAccess<'de>,
    {
        let mut values = Vec::new();
        while let Some(value) = sequence.next_element_seed(StrictValueSeed {
            depth: self.depth + 1,
            budget: self.budget,
        })? {
            values.push(value);
        }
        Ok(Value::Array(values))
    }

    fn visit_map<A>(self, mut map: A) -> std::result::Result<Self::Value, A::Error>
    where
        A: MapAccess<'de>,
    {
        let mut keys = BTreeSet::new();
        let mut values = serde_json::Map::new();
        while let Some(key) = map.next_key::<String>()? {
            if !keys.insert(key.clone()) {
                return Err(serde::de::Error::custom(format!(
                    "tool arguments contain duplicate object key `{key}`"
                )));
            }
            let value = map.next_value_seed(StrictValueSeed {
                depth: self.depth + 1,
                budget: self.budget,
            })?;
            values.insert(key, value);
        }
        Ok(Value::Object(values))
    }
}
