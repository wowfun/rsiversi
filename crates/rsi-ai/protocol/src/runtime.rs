use std::{fmt, pin::Pin};

use async_trait::async_trait;
use futures_util::Stream;
use rsi_credentials_protocol::CredentialSource;
use rsi_meta_contract::LocalContract;
use serde::{Deserialize, Serialize};
use thiserror::Error;
use tokio_util::sync::CancellationToken;

use crate::{
    AiError, ErrorKind, ImageEvent, ImageRequest, LanguageEvent, LanguageProfile, LanguageRequest,
    MAX_CONTENT_BLOCKS, MAX_EXTENSION_BYTES, ProviderExtension, validate_identifier,
};

/// Exact live deployment and model selection.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ModelRef {
    deployment: String,
    model: String,
}

impl<'de> Deserialize<'de> for ModelRef {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct WireModelRef {
            deployment: String,
            model: String,
        }

        let wire = WireModelRef::deserialize(deserializer)?;
        Self::new(wire.deployment, wire.model).map_err(serde::de::Error::custom)
    }
}

impl ModelRef {
    /// Creates one exact route reference without provider guessing or aliases.
    pub fn new(
        deployment: impl Into<String>,
        model: impl Into<String>,
    ) -> Result<Self, AiContractError> {
        let reference = Self {
            deployment: deployment.into(),
            model: model.into(),
        };
        reference.validate()?;
        Ok(reference)
    }

    /// Revalidates a decoded route reference.
    pub fn validate(&self) -> Result<(), AiContractError> {
        validate_identifier("deployment", &self.deployment).map_err(AiContractError::invalid)?;
        validate_identifier("model", &self.model).map_err(AiContractError::invalid)
    }

    /// Returns the exact deployment route key.
    pub fn deployment(&self) -> &str {
        &self.deployment
    }

    /// Returns the exact provider model identifier.
    pub fn model(&self) -> &str {
        &self.model
    }
}

/// Public AI capability attached to a prepared snapshot.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AiCapability {
    /// Text, multimodal understanding, reasoning, and tool calls.
    Language,
    /// Image generation or editing.
    Image,
}

/// Finite retry facts interpreted only by a durable orchestrator.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RetryPolicy {
    max_retries: u8,
    retryable_kinds: Vec<ErrorKind>,
    initial_delay_ms: u64,
    max_delay_ms: u64,
    jitter_per_mille: u16,
}

impl<'de> Deserialize<'de> for RetryPolicy {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct WireRetryPolicy {
            max_retries: u8,
            retryable_kinds: Vec<ErrorKind>,
            initial_delay_ms: u64,
            max_delay_ms: u64,
            jitter_per_mille: u16,
        }

        let wire = WireRetryPolicy::deserialize(deserializer)?;
        Self::new(
            wire.max_retries,
            wire.retryable_kinds,
            wire.initial_delay_ms,
            wire.max_delay_ms,
            wire.jitter_per_mille,
        )
        .map_err(serde::de::Error::custom)
    }
}

impl RetryPolicy {
    /// Creates bounded retry facts; a provider attempt itself never retries.
    pub fn new(
        max_retries: u8,
        retryable_kinds: Vec<ErrorKind>,
        initial_delay_ms: u64,
        max_delay_ms: u64,
        jitter_per_mille: u16,
    ) -> Result<Self, AiContractError> {
        if max_retries > 16
            || retryable_kinds.is_empty()
            || retryable_kinds.len() > 16
            || retryable_kinds
                .iter()
                .enumerate()
                .any(|(index, kind)| retryable_kinds[..index].contains(kind))
            || initial_delay_ms == 0
            || initial_delay_ms > max_delay_ms
            || max_delay_ms > 60_000
            || jitter_per_mille > 1_000
        {
            return Err(AiContractError::invalid(
                "retry policy exceeds its attempt, kind, delay, or jitter bounds",
            ));
        }
        Ok(Self {
            max_retries,
            retryable_kinds,
            initial_delay_ms,
            max_delay_ms,
            jitter_per_mille,
        })
    }

    /// Returns the maximum retries after the initial attempt.
    pub const fn max_retries(&self) -> u8 {
        self.max_retries
    }

    /// Returns whether the policy admits one provider-neutral failure kind.
    pub fn retries(&self, kind: ErrorKind) -> bool {
        self.retryable_kinds.contains(&kind)
    }

    /// Returns the initial backoff in milliseconds.
    pub const fn initial_delay_ms(&self) -> u64 {
        self.initial_delay_ms
    }

    /// Returns the maximum backoff in milliseconds.
    pub const fn max_delay_ms(&self) -> u64 {
        self.max_delay_ms
    }

    /// Returns the bounded jitter fraction in per-mille units.
    pub const fn jitter_per_mille(&self) -> u16 {
        self.jitter_per_mille
    }

    /// Revalidates decoded retry facts.
    pub fn validate(&self) -> Result<(), AiContractError> {
        Self::new(
            self.max_retries,
            self.retryable_kinds.clone(),
            self.initial_delay_ms,
            self.max_delay_ms,
            self.jitter_per_mille,
        )
        .map(|_| ())
    }
}

impl Default for RetryPolicy {
    fn default() -> Self {
        Self {
            max_retries: 2,
            retryable_kinds: vec![
                ErrorKind::RateLimited,
                ErrorKind::Server,
                ErrorKind::Timeout,
                ErrorKind::Transport,
                ErrorKind::OutputValidation,
            ],
            initial_delay_ms: 500,
            max_delay_ms: 10_000,
            jitter_per_mille: 100,
        }
    }
}

/// Redacted, persistable facts frozen before external provider I/O.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PreparedCallSnapshot {
    /// Caller-visible call identity unique within the router generation.
    pub call_id: String,
    /// Exact configured provider deployment.
    pub deployment_id: String,
    /// Provider family that owns translation semantics.
    pub provider_family: String,
    /// Typed capability prepared for the call.
    pub capability: AiCapability,
    /// Exact model identifier supplied by the caller.
    pub model: String,
    /// Provider protocol family frozen during preparation.
    pub protocol: String,
    /// Transport kind frozen during preparation.
    pub transport: String,
    /// Redacted endpoint identity suitable for replay diagnostics.
    pub endpoint_fingerprint: String,
    /// Provider Fiber generation pinned by the call.
    pub config_generation: u64,
    /// Redacted source of the resolved credential, when required.
    pub credential_source: Option<CredentialSource>,
    /// Finite retry facts for a durable orchestration layer.
    pub retry_policy: RetryPolicy,
    /// Lowercase SHA-256 of canonical provider-neutral request bytes.
    pub request_sha256: String,
}

impl PreparedCallSnapshot {
    /// Revalidates redacted facts decoded from durable state.
    pub fn validate(&self) -> Result<(), AiContractError> {
        for (field, value) in [
            ("call_id", self.call_id.as_str()),
            ("deployment_id", self.deployment_id.as_str()),
            ("provider_family", self.provider_family.as_str()),
            ("model", self.model.as_str()),
            ("protocol", self.protocol.as_str()),
            ("transport", self.transport.as_str()),
            ("endpoint_fingerprint", self.endpoint_fingerprint.as_str()),
        ] {
            validate_identifier(field, value).map_err(AiContractError::invalid)?;
        }
        if self.config_generation == 0 {
            return Err(AiContractError::invalid(
                "prepared snapshot has a zero provider generation",
            ));
        }
        if let Some(source) = &self.credential_source {
            source.validate().map_err(|error| {
                AiContractError::invalid(format!("invalid credential source: {error}"))
            })?;
        }
        if self.request_sha256.len() != 64
            || !self
                .request_sha256
                .bytes()
                .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
        {
            return Err(AiContractError::invalid(
                "prepared snapshot contains an invalid request digest",
            ));
        }
        self.retry_policy.validate()
    }
}

impl<'de> Deserialize<'de> for PreparedCallSnapshot {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct WirePreparedCallSnapshot {
            call_id: String,
            deployment_id: String,
            provider_family: String,
            capability: AiCapability,
            model: String,
            protocol: String,
            transport: String,
            endpoint_fingerprint: String,
            config_generation: u64,
            credential_source: Option<CredentialSource>,
            retry_policy: RetryPolicy,
            request_sha256: String,
        }

        let wire = WirePreparedCallSnapshot::deserialize(deserializer)?;
        let snapshot = Self {
            call_id: wire.call_id,
            deployment_id: wire.deployment_id,
            provider_family: wire.provider_family,
            capability: wire.capability,
            model: wire.model,
            protocol: wire.protocol,
            transport: wire.transport,
            endpoint_fingerprint: wire.endpoint_fingerprint,
            config_generation: wire.config_generation,
            credential_source: wire.credential_source,
            retry_policy: wire.retry_policy,
            request_sha256: wire.request_sha256,
        };
        snapshot
            .validate()
            .map(|()| snapshot)
            .map_err(serde::de::Error::custom)
    }
}

/// Invalid route or durable prepared-call contract.
#[derive(Clone, Debug, Eq, Error, PartialEq)]
#[error("invalid AI contract: {message}")]
pub struct AiContractError {
    message: String,
}

impl AiContractError {
    pub(crate) fn invalid(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

/// Pull-based normalized language event stream.
pub type LanguageStream =
    Pin<Box<dyn Stream<Item = Result<LanguageEvent, AiError>> + Send + 'static>>;
/// Pull-based normalized image event stream.
pub type ImageStream = Pin<Box<dyn Stream<Item = Result<ImageEvent, AiError>> + Send + 'static>>;

/// Current provider-side state of one explicitly deferred language response.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeferredStatus {
    /// Accepted remotely but not yet executing.
    Queued,
    /// Executing or producing resumable output remotely.
    InProgress,
    /// Completed successfully.
    Completed,
    /// Reached a terminal provider failure.
    Failed,
    /// Reached a terminal cancelled state.
    Cancelled,
}

impl DeferredStatus {
    /// Returns whether no later distinct status is valid.
    pub const fn is_terminal(self) -> bool {
        matches!(self, Self::Completed | Self::Failed | Self::Cancelled)
    }
}

/// Persistable cursor for a provider-managed background language response.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct DeferredLanguageCheckpoint {
    call: PreparedCallSnapshot,
    operation_id: String,
    status: DeferredStatus,
    event_stream_terminal: bool,
    sequence_number: Option<u64>,
    provider_state: Option<ProviderExtension>,
}

impl<'de> Deserialize<'de> for DeferredLanguageCheckpoint {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct WireCheckpoint {
            call: PreparedCallSnapshot,
            operation_id: String,
            status: DeferredStatus,
            event_stream_terminal: bool,
            sequence_number: Option<u64>,
            provider_state: Option<ProviderExtension>,
        }

        let wire = WireCheckpoint::deserialize(deserializer)?;
        let checkpoint = Self {
            call: wire.call,
            operation_id: wire.operation_id,
            status: wire.status,
            event_stream_terminal: wire.event_stream_terminal,
            sequence_number: wire.sequence_number,
            provider_state: wire.provider_state,
        };
        checkpoint
            .validate()
            .map(|()| checkpoint)
            .map_err(serde::de::Error::custom)
    }
}

impl DeferredLanguageCheckpoint {
    /// Creates an initial cursor before a resumable stream has been opened.
    pub fn new(
        call: PreparedCallSnapshot,
        operation_id: impl Into<String>,
        status: DeferredStatus,
        provider_state: Option<ProviderExtension>,
    ) -> Result<Self, AiContractError> {
        let checkpoint = Self {
            call,
            operation_id: operation_id.into(),
            status,
            event_stream_terminal: false,
            sequence_number: None,
            provider_state,
        };
        checkpoint.validate()?;
        Ok(checkpoint)
    }

    /// Revalidates a decoded durable cursor.
    pub fn validate(&self) -> Result<(), AiContractError> {
        self.call.validate()?;
        validate_identifier("operation_id", &self.operation_id)
            .map_err(AiContractError::invalid)?;
        if self.event_stream_terminal
            && (self.sequence_number.is_none() || !self.status.is_terminal())
        {
            return Err(AiContractError::invalid(
                "terminal deferred output requires terminal status and a sequence number",
            ));
        }
        if let Some(state) = &self.provider_state {
            state
                .validate("provider_state")
                .map_err(|error| AiContractError::invalid(error.to_string()))?;
            let bytes = serde_json::to_vec(state)
                .map_err(|error| AiContractError::invalid(error.to_string()))?;
            if bytes.len() > MAX_EXTENSION_BYTES {
                return Err(AiContractError::invalid(
                    "deferred provider state exceeds its byte bound",
                ));
            }
        }
        Ok(())
    }

    /// Returns the original frozen call facts.
    pub const fn call(&self) -> &PreparedCallSnapshot {
        &self.call
    }

    /// Returns the remote operation identity.
    pub fn operation_id(&self) -> &str {
        &self.operation_id
    }

    /// Returns the latest status.
    pub const fn status(&self) -> DeferredStatus {
        self.status
    }

    /// Returns whether a terminal output event was consumed durably.
    pub const fn event_stream_terminal(&self) -> bool {
        self.event_stream_terminal
    }

    /// Returns the durable provider cursor.
    pub const fn sequence_number(&self) -> Option<u64> {
        self.sequence_number
    }

    /// Returns bounded provider parser state.
    pub const fn provider_state(&self) -> Option<&ProviderExtension> {
        self.provider_state.as_ref()
    }

    /// Advances this caller checkpoint after atomically committing one batch.
    pub fn advance(
        &mut self,
        status: DeferredStatus,
        event_stream_terminal: bool,
        sequence_number: u64,
        provider_state: Option<ProviderExtension>,
    ) -> Result<(), AiContractError> {
        if self.status.is_terminal() && self.status != status
            || matches!(
                (self.status, status),
                (DeferredStatus::InProgress, DeferredStatus::Queued)
            )
            || self.event_stream_terminal && !event_stream_terminal
            || event_stream_terminal && !status.is_terminal()
            || self
                .sequence_number
                .is_some_and(|previous| sequence_number <= previous)
        {
            return Err(AiContractError::invalid(
                "deferred checkpoint status or sequence regressed",
            ));
        }
        self.status = status;
        self.event_stream_terminal = event_stream_terminal;
        self.sequence_number = Some(sequence_number);
        self.provider_state = provider_state;
        self.validate()
    }
}

/// One atomic normalized event batch and the checkpoint immediately after it.
#[derive(Clone, Debug, PartialEq)]
pub struct DeferredLanguageBatch {
    events: Vec<LanguageEvent>,
    checkpoint: DeferredLanguageCheckpoint,
}

impl DeferredLanguageBatch {
    /// Couples one bounded event batch with its post-event durable cursor.
    pub fn new(
        events: Vec<LanguageEvent>,
        checkpoint: DeferredLanguageCheckpoint,
    ) -> Result<Self, AiContractError> {
        if events.len() > MAX_CONTENT_BLOCKS + 2 {
            return Err(AiContractError::invalid(
                "deferred Language batch contains too many events",
            ));
        }
        for event in &events {
            event
                .validate()
                .map_err(|error| AiContractError::invalid(error.to_string()))?;
        }
        checkpoint.validate()?;
        Ok(Self { events, checkpoint })
    }

    /// Returns events to commit atomically with the checkpoint.
    pub fn events(&self) -> &[LanguageEvent] {
        &self.events
    }

    /// Returns the post-event checkpoint.
    pub const fn checkpoint(&self) -> &DeferredLanguageCheckpoint {
        &self.checkpoint
    }
}

/// Pull-based atomic batches from one deferred resume request.
pub type DeferredLanguageStream =
    Pin<Box<dyn Stream<Item = Result<DeferredLanguageBatch, AiError>> + Send + 'static>>;

/// Provider-I/O-free deferred submission pinned to one provider generation.
#[async_trait]
pub trait PreparedDeferredLanguageCall: fmt::Debug + Send + 'static {
    /// Returns redacted facts that may be committed before Start.
    fn snapshot(&self) -> &PreparedCallSnapshot;
    /// Starts exactly one deferred submission and returns its owned controller.
    async fn start(
        self: Box<Self>,
        cancellation: CancellationToken,
    ) -> Result<Box<dyn DeferredLanguageCall>, AiError>;
}

/// Caller-owned controller for one explicitly deferred language response.
#[async_trait]
pub trait DeferredLanguageCall: fmt::Debug + Send + 'static {
    /// Returns the latest cursor for atomic persistence or rejects an invalid
    /// provider projection.
    fn checkpoint(&self) -> Result<DeferredLanguageCheckpoint, AiError>;
    /// Performs exactly one status request.
    async fn poll(&mut self, cancellation: CancellationToken) -> Result<DeferredStatus, AiError>;
    /// Opens exactly one stream request after the durable cursor.
    async fn resume(
        &mut self,
        cancellation: CancellationToken,
    ) -> Result<DeferredLanguageStream, AiError>;
    /// Performs exactly one explicit cancellation request.
    async fn cancel(&mut self, cancellation: CancellationToken) -> Result<DeferredStatus, AiError>;
}

/// Provider-I/O-free one-shot language call pinned to one provider generation.
#[async_trait]
pub trait PreparedLanguageCall: fmt::Debug + Send + 'static {
    /// Returns redacted facts that may be committed before Start.
    fn snapshot(&self) -> &PreparedCallSnapshot;
    /// Consumes this operation and starts exactly one provider attempt.
    async fn start(
        self: Box<Self>,
        cancellation: CancellationToken,
    ) -> Result<LanguageStream, AiError>;
}

/// Provider-I/O-free one-shot image call pinned to one provider generation.
#[async_trait]
pub trait PreparedImageCall: fmt::Debug + Send + 'static {
    /// Returns redacted facts that may be committed before Start.
    fn snapshot(&self) -> &PreparedCallSnapshot;
    /// Consumes this operation and starts exactly one provider attempt.
    async fn start(
        self: Box<Self>,
        cancellation: CancellationToken,
    ) -> Result<ImageStream, AiError>;
}

/// Exact-route Language router service.
#[async_trait]
pub trait LanguageCall: fmt::Debug + Send + Sync + 'static {
    /// Describes one route without credentials, media reads, or provider I/O.
    fn describe(&self, model: &ModelRef) -> Result<LanguageProfile, AiError>;
    /// Validates and freezes one call without provider I/O.
    async fn prepare(
        &self,
        model: ModelRef,
        request: LanguageRequest,
    ) -> Result<Box<dyn PreparedLanguageCall>, AiError>;
    /// Validates and freezes one explicitly deferred call without provider I/O.
    async fn prepare_deferred(
        &self,
        _model: ModelRef,
        _request: LanguageRequest,
    ) -> Result<Box<dyn PreparedDeferredLanguageCall>, AiError> {
        Err(deferred_unsupported())
    }
    /// Restores an exact durable deferred cursor without provider I/O.
    async fn restore_deferred(
        &self,
        _checkpoint: DeferredLanguageCheckpoint,
    ) -> Result<Box<dyn DeferredLanguageCall>, AiError> {
        Err(deferred_unsupported())
    }
}

/// Stable unsupported error for caller implementations without deferred support.
pub fn deferred_unsupported() -> AiError {
    AiError::deferred_unsupported()
}

/// Exact-route Image router service.
#[async_trait]
pub trait ImageCall: fmt::Debug + Send + Sync + 'static {
    /// Validates one exact Image route without credentials, media reads, or provider I/O.
    fn describe(&self, model: &ModelRef) -> Result<(), AiError>;
    /// Validates and freezes one call without provider I/O.
    async fn prepare(
        &self,
        model: ModelRef,
        request: ImageRequest,
    ) -> Result<Box<dyn PreparedImageCall>, AiError>;
}

/// Nominal Local contract for [`LanguageCall`].
#[derive(Debug)]
pub struct LanguageCallContract;

impl LocalContract for LanguageCallContract {
    const KEY: &'static str = "rsi.ai.language";
    type Service = dyn LanguageCall;
}

/// Nominal Local contract for [`ImageCall`].
#[derive(Debug)]
pub struct ImageCallContract;

impl LocalContract for ImageCallContract {
    const KEY: &'static str = "rsi.ai.image";
    type Service = dyn ImageCall;
}
