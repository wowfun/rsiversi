//! Capability-specific provider-author interfaces for `rsi-ai`.

#![deny(unsafe_code)]
#![allow(clippy::missing_errors_doc)] // Provider SDK errors expose stable codes.
#![allow(clippy::missing_panics_doc)] // Private construction invariants back static-error expects.

use std::{fmt, future::Future, pin::Pin, sync::Arc};

use async_trait::async_trait;
use futures_util::Stream;
use rsi_ai_auth::{CredentialRequirement, CredentialSourceSnapshot, ResolvedCredential};
use rsi_ai_protocol::{
    AiError, DispatchStatus, ErrorKind, ErrorPhase, ImageEvent, ImageRequest, LanguageEvent,
    LanguageProfile, LanguageRequest, MAX_EXTENSION_BYTES, MediaDescriptor, ProviderExtension,
    RealtimeCommand, RealtimeEvent, RealtimeRequest, SpeechEvent, SpeechRequest,
    TranscriptionEvent, TranscriptionRequest,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;
use tokio_util::sync::CancellationToken;

/// Heap-owned future returned by a provider-author interface.
pub type AdapterFuture<T> = Pin<Box<dyn Future<Output = T> + Send + 'static>>;
/// Pull-based event stream returned by a provider-author interface.
pub type AdapterStream<E> = Pin<Box<dyn Stream<Item = Result<E, AiError>> + Send + 'static>>;

/// Normalized language stream returned after a prepared call starts.
pub type LanguageAdapterStream = AdapterStream<LanguageEvent>;
/// Normalized image stream returned after a prepared call starts.
pub type ImageAdapterStream = AdapterStream<ImageEvent>;
/// Normalized transcription stream returned after a prepared call starts.
pub type TranscriptionAdapterStream = AdapterStream<TranscriptionEvent>;
/// Normalized speech stream returned after a prepared call starts.
pub type SpeechAdapterStream = AdapterStream<SpeechEvent>;

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
///
/// `provider_state` is bounded parser state, never response bytes, credentials,
/// or accumulated model output. A durable caller commits each emitted batch and
/// this checkpoint atomically before resuming after `sequence_number`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeferredLanguageCheckpoint {
    call: PreparedCallSnapshot,
    operation_id: String,
    status: DeferredStatus,
    stream_created: bool,
    sequence_number: Option<u64>,
    provider_state: Option<ProviderExtension>,
}

impl DeferredLanguageCheckpoint {
    /// Creates an initial checkpoint before a resumable stream has been opened.
    pub fn new(
        call: PreparedCallSnapshot,
        operation_id: impl Into<String>,
        status: DeferredStatus,
        provider_state: Option<ProviderExtension>,
    ) -> Result<Self, ProviderSdkError> {
        let checkpoint = Self {
            call,
            operation_id: operation_id.into(),
            status,
            stream_created: false,
            sequence_number: None,
            provider_state,
        };
        checkpoint.validate()?;
        Ok(checkpoint)
    }

    /// Revalidates a checkpoint decoded from durable state.
    pub fn validate(&self) -> Result<(), ProviderSdkError> {
        self.call.validate()?;
        validate_id("operation_id", &self.operation_id)?;
        if self.stream_created && self.sequence_number.is_none() {
            return Err(ProviderSdkError::new(
                "provider.invalid_deferred_checkpoint",
                "a created deferred stream must have a sequence number",
            ));
        }
        validate_provider_state(self.provider_state.as_ref())
    }

    /// Returns the original frozen call facts.
    pub const fn call(&self) -> &PreparedCallSnapshot {
        &self.call
    }

    /// Returns the remote provider operation identifier.
    pub fn operation_id(&self) -> &str {
        &self.operation_id
    }

    /// Returns the latest observed provider status.
    pub const fn status(&self) -> DeferredStatus {
        self.status
    }

    /// Returns whether any resumable provider stream has been created.
    pub const fn stream_created(&self) -> bool {
        self.stream_created
    }

    /// Returns the cursor after the last durably paired normalized event batch.
    pub const fn sequence_number(&self) -> Option<u64> {
        self.sequence_number
    }

    /// Returns bounded parser state required to resume after the cursor.
    pub const fn provider_state(&self) -> Option<&ProviderExtension> {
        self.provider_state.as_ref()
    }

    /// Applies one provider status observation without permitting regression
    /// from an in-progress or terminal state.
    pub fn observe_status(&mut self, status: DeferredStatus) -> Result<(), ProviderSdkError> {
        validate_status_transition(self.status, status)?;
        self.status = status;
        Ok(())
    }

    /// Advances the resumable stream cursor and its bounded parser state.
    pub fn advance(
        &mut self,
        status: DeferredStatus,
        stream_created: bool,
        sequence_number: u64,
        provider_state: Option<ProviderExtension>,
    ) -> Result<(), ProviderSdkError> {
        validate_status_transition(self.status, status)?;
        if self.stream_created && !stream_created {
            return Err(ProviderSdkError::new(
                "provider.invalid_deferred_checkpoint",
                "deferred stream-created state cannot regress",
            ));
        }
        if self
            .sequence_number
            .is_some_and(|previous| sequence_number <= previous)
        {
            return Err(ProviderSdkError::new(
                "provider.invalid_deferred_checkpoint",
                "deferred sequence numbers must increase monotonically",
            ));
        }
        validate_provider_state(provider_state.as_ref())?;
        self.status = status;
        self.stream_created = stream_created;
        self.sequence_number = Some(sequence_number);
        self.provider_state = provider_state;
        Ok(())
    }
}

fn validate_status_transition(
    current: DeferredStatus,
    next: DeferredStatus,
) -> Result<(), ProviderSdkError> {
    let valid = if current.is_terminal() {
        current == next
    } else {
        !matches!(
            (current, next),
            (DeferredStatus::InProgress, DeferredStatus::Queued)
        )
    };
    if valid {
        Ok(())
    } else {
        Err(ProviderSdkError::new(
            "provider.invalid_deferred_status",
            "deferred response status regressed or changed after terminal",
        ))
    }
}

fn validate_provider_state(state: Option<&ProviderExtension>) -> Result<(), ProviderSdkError> {
    let Some(state) = state else {
        return Ok(());
    };
    validate_id("provider_state.namespace", &state.namespace)?;
    let encoded = serde_json::to_vec(state).map_err(|_| {
        ProviderSdkError::new(
            "provider.invalid_deferred_checkpoint",
            "deferred provider state is not serializable",
        )
    })?;
    if encoded.len() > MAX_EXTENSION_BYTES {
        return Err(ProviderSdkError::new(
            "provider.invalid_deferred_checkpoint",
            format!("deferred provider state exceeds {MAX_EXTENSION_BYTES} bytes"),
        ));
    }
    Ok(())
}

/// One atomic normalized output batch and the cursor immediately after it.
#[derive(Clone, Debug, PartialEq)]
pub struct DeferredLanguageBatch {
    events: Vec<LanguageEvent>,
    checkpoint: DeferredLanguageCheckpoint,
}

impl DeferredLanguageBatch {
    /// Couples one bounded event batch with the cursor immediately after it.
    pub fn new(
        events: Vec<LanguageEvent>,
        checkpoint: DeferredLanguageCheckpoint,
    ) -> Result<Self, ProviderSdkError> {
        if events.len() > 64 {
            return Err(ProviderSdkError::new(
                "provider.invalid_deferred_batch",
                "one deferred provider event expanded to more than 64 normalized events",
            ));
        }
        checkpoint.validate()?;
        Ok(Self { events, checkpoint })
    }

    /// Returns normalized events that must be committed with the checkpoint.
    pub fn events(&self) -> &[LanguageEvent] {
        &self.events
    }

    /// Returns the post-event durable checkpoint.
    pub const fn checkpoint(&self) -> &DeferredLanguageCheckpoint {
        &self.checkpoint
    }
}

/// Atomic event/checkpoint batches returned by one deferred resume request.
pub type DeferredLanguageAdapterStream = AdapterStream<DeferredLanguageBatch>;

/// AI capability selected before a provider call is prepared.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Capability {
    /// Text, multimodal understanding, reasoning, and tool calls.
    Language,
    /// Image generation or editing.
    Image,
    /// Audio-to-text transcription.
    Transcription,
    /// Text-to-audio synthesis.
    Speech,
    /// Live bidirectional Realtime interaction.
    Realtime,
}

/// Finite provider-owned retry policy. Standalone calls remain single-attempt;
/// an orchestration layer may execute this policy at a durable call boundary.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RetryPolicy {
    max_retries: u8,
    retryable_kinds: Vec<ErrorKind>,
    initial_delay_ms: u64,
    max_delay_ms: u64,
    jitter_per_mille: u16,
}

impl RetryPolicy {
    /// Creates bounded retry facts for an orchestration layer to interpret durably.
    pub fn new(
        max_retries: u8,
        retryable_kinds: Vec<ErrorKind>,
        initial_delay_ms: u64,
        max_delay_ms: u64,
        jitter_per_mille: u16,
    ) -> Result<Self, ProviderSdkError> {
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
            return Err(ProviderSdkError::new(
                "provider.invalid_retry_policy",
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

    /// Returns whether the policy admits the provider-neutral failure category.
    pub fn retries(&self, kind: ErrorKind) -> bool {
        self.retryable_kinds.contains(&kind)
    }

    pub const fn initial_delay_ms(&self) -> u64 {
        self.initial_delay_ms
    }

    pub const fn max_delay_ms(&self) -> u64 {
        self.max_delay_ms
    }

    pub const fn jitter_per_mille(&self) -> u16 {
        self.jitter_per_mille
    }

    /// Revalidates retry facts decoded from a prepared snapshot.
    pub fn validate(&self) -> Result<(), ProviderSdkError> {
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
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PreparedCallSnapshot {
    /// Caller-visible call identity unique within the owning runtime.
    pub call_id: String,
    /// Exact configured provider deployment.
    pub deployment_id: String,
    /// Provider family that owns translation semantics.
    pub provider_family: String,
    /// Typed capability prepared for the call.
    pub capability: Capability,
    /// Exact model identifier supplied by the caller.
    pub model: String,
    /// Provider protocol family frozen during preparation.
    pub protocol: String,
    /// Transport kind frozen during preparation.
    pub transport: String,
    /// Redacted endpoint identity suitable for replay diagnostics.
    pub endpoint_fingerprint: String,
    /// Provider configuration generation pinned by the call.
    pub config_generation: u64,
    /// Redacted source of the resolved credential, when required.
    pub credential_source: Option<CredentialSourceSnapshot>,
    /// Finite retry facts for a durable orchestration layer.
    pub retry_policy: RetryPolicy,
    /// Lowercase SHA-256 of canonical provider-neutral request bytes.
    pub request_sha256: String,
}

impl PreparedCallSnapshot {
    /// Revalidates redacted facts decoded from durable or wire state.
    pub fn validate(&self) -> Result<(), ProviderSdkError> {
        for (field, value) in [
            ("call_id", self.call_id.as_str()),
            ("deployment_id", self.deployment_id.as_str()),
            ("provider_family", self.provider_family.as_str()),
            ("model", self.model.as_str()),
            ("protocol", self.protocol.as_str()),
            ("transport", self.transport.as_str()),
            ("endpoint_fingerprint", self.endpoint_fingerprint.as_str()),
        ] {
            validate_id(field, value)?;
        }
        if self.request_sha256.len() != 64
            || !self
                .request_sha256
                .bytes()
                .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
        {
            return Err(ProviderSdkError::new(
                "provider.invalid_snapshot",
                "prepared snapshot contains an invalid request digest",
            ));
        }
        if let Some(credential) = &self.credential_source {
            credential.validate().map_err(|error| {
                ProviderSdkError::new("provider.invalid_snapshot", error.to_string())
            })?;
        }
        self.retry_policy.validate()
    }
}

/// Provider-private context whose secret half is intentionally not serializable.
#[derive(Clone)]
pub struct PrepareContext {
    snapshot: PreparedCallSnapshot,
    credential: Option<ResolvedCredential>,
    media: Arc<dyn MediaResolver>,
}

impl PrepareContext {
    /// Couples redacted snapshot facts with nonserializable secret and media access.
    #[must_use]
    pub fn new(
        snapshot: PreparedCallSnapshot,
        credential: Option<ResolvedCredential>,
        media: Arc<dyn MediaResolver>,
    ) -> Self {
        Self {
            snapshot,
            credential,
            media,
        }
    }

    /// Returns persistable redacted call facts.
    pub const fn snapshot(&self) -> &PreparedCallSnapshot {
        &self.snapshot
    }

    /// Returns the resolved secret and source facts when the provider requires one.
    pub const fn credential(&self) -> Option<&ResolvedCredential> {
        self.credential.as_ref()
    }

    /// Returns the Start-time media resolver.
    pub fn media(&self) -> &Arc<dyn MediaResolver> {
        &self.media
    }

    /// Reads and verifies one media body at Start-time.
    pub async fn resolve_media(
        &self,
        descriptor: &MediaDescriptor,
        abort: AbortSignal,
    ) -> Result<Arc<[u8]>, AiError> {
        let bytes = self.media.read(descriptor.clone(), abort).await?;
        if u64::try_from(bytes.len()).ok() != Some(descriptor.byte_len()) {
            return Err(AiError::new(
                ErrorKind::Artifact,
                ErrorPhase::Send,
                DispatchStatus::NotDispatched,
                "resolved media length does not match its descriptor",
            )
            .expect("static media error is valid"));
        }
        let digest = hex::encode(Sha256::digest(&bytes));
        if digest != descriptor.sha256() {
            return Err(AiError::new(
                ErrorKind::Artifact,
                ErrorPhase::Send,
                DispatchStatus::NotDispatched,
                "resolved media digest does not match its descriptor",
            )
            .expect("static media error is valid"));
        }
        Ok(bytes)
    }
}

impl fmt::Debug for PrepareContext {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PrepareContext")
            .field("snapshot", &self.snapshot)
            .field(
                "credential",
                &self.credential.as_ref().map(ResolvedCredential::source),
            )
            .field("media", &self.media)
            .finish()
    }
}

/// Resolves locator-free media only after a prepared call crosses its Start barrier.
pub trait MediaResolver: fmt::Debug + Send + Sync {
    /// Reads one body cooperatively, leaving descriptor verification to the caller context.
    fn read(
        &self,
        descriptor: MediaDescriptor,
        abort: AbortSignal,
    ) -> AdapterFuture<Result<Arc<[u8]>, AiError>>;
}

/// Default resolver used when a Registry was not connected to an artifact store.
#[derive(Clone, Copy, Debug, Default)]
pub struct MissingMediaResolver;

impl MediaResolver for MissingMediaResolver {
    fn read(
        &self,
        _descriptor: MediaDescriptor,
        _abort: AbortSignal,
    ) -> AdapterFuture<Result<Arc<[u8]>, AiError>> {
        Box::pin(async {
            Err(AiError::new(
                ErrorKind::Artifact,
                ErrorPhase::Send,
                DispatchStatus::NotDispatched,
                "no media resolver is configured",
            )
            .expect("static media resolver failure is valid"))
        })
    }
}

/// Cooperative cancellation shared by direct calls, plugins, and tests.
#[derive(Clone, Debug, Default)]
pub struct AbortSignal(CancellationToken);

impl AbortSignal {
    /// Creates an independent cancellation signal.
    #[must_use]
    pub fn new() -> Self {
        Self(CancellationToken::new())
    }

    /// Signals cancellation to all clones.
    pub fn abort(&self) {
        self.0.cancel();
    }

    pub fn is_aborted(&self) -> bool {
        self.0.is_cancelled()
    }

    /// Waits until this signal is cancelled.
    pub async fn cancelled(&self) {
        self.0.cancelled().await;
    }

    /// Returns a clone of the underlying Tokio cancellation token for adapters.
    pub fn cancellation_token(&self) -> CancellationToken {
        self.0.clone()
    }
}

type StartExecutor<T> =
    Box<dyn FnOnce(AbortSignal) -> AdapterFuture<Result<T, AiError>> + Send + 'static>;

/// One-shot prepared adapter call. Construction performs no provider I/O.
pub struct Prepared<T> {
    snapshot: PreparedCallSnapshot,
    executor: Option<StartExecutor<T>>,
}

impl<T> Prepared<T> {
    /// Creates a provider-I/O-free one-shot call around its Start executor.
    pub fn new<F>(snapshot: PreparedCallSnapshot, executor: F) -> Self
    where
        F: FnOnce(AbortSignal) -> AdapterFuture<Result<T, AiError>> + Send + 'static,
    {
        Self {
            snapshot,
            executor: Some(Box::new(executor)),
        }
    }

    /// Returns redacted facts that may be committed before Start.
    pub const fn snapshot(&self) -> &PreparedCallSnapshot {
        &self.snapshot
    }

    /// Consumes the prepared call and permits exactly one external provider attempt.
    pub async fn start(mut self, abort: AbortSignal) -> Result<T, AiError> {
        let executor = self
            .executor
            .take()
            .expect("Prepared executor exists until the consuming start call");
        executor(abort).await
    }
}

impl<T> fmt::Debug for Prepared<T> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Prepared")
            .field("snapshot", &self.snapshot)
            .field("ready", &self.executor.is_some())
            .finish()
    }
}

/// Language provider seam.
pub trait LanguageAdapter: fmt::Debug + Send + Sync {
    /// Describes one model without credentials, network, filesystem, or other provider I/O.
    fn describe(&self, model: &str) -> Result<LanguageProfile, AiError>;

    /// Validates and freezes one language call without performing provider I/O.
    fn prepare(
        &self,
        context: PrepareContext,
        model: String,
        request: LanguageRequest,
    ) -> AdapterFuture<Result<Prepared<LanguageAdapterStream>, AiError>>;

    /// Prepares one explicitly backgrounded response without provider I/O.
    fn prepare_deferred(
        &self,
        _context: PrepareContext,
        _model: String,
        _request: LanguageRequest,
    ) -> AdapterFuture<Result<Prepared<DeferredLanguageAdapterHandle>, AiError>> {
        Box::pin(async { Err(deferred_unsupported()) })
    }

    /// Restores a durable background-response cursor without provider I/O.
    fn restore_deferred(
        &self,
        _context: PrepareContext,
        _checkpoint: DeferredLanguageCheckpoint,
    ) -> AdapterFuture<Result<DeferredLanguageAdapterHandle, AiError>> {
        Box::pin(async { Err(deferred_unsupported()) })
    }
}

/// Provider-owned controller for one explicitly deferred language response.
#[async_trait]
pub trait DeferredLanguageOperation: fmt::Debug + Send {
    /// Returns the latest cursor for atomic persistence with observed events.
    fn checkpoint(&self) -> DeferredLanguageCheckpoint;

    /// Performs exactly one status request.
    async fn poll(&mut self, abort: AbortSignal) -> Result<DeferredStatus, AiError>;

    /// Opens exactly one stream request after the current durable cursor.
    async fn resume(
        &mut self,
        abort: AbortSignal,
    ) -> Result<DeferredLanguageAdapterStream, AiError>;

    /// Performs exactly one explicit cancellation request.
    async fn cancel(&mut self, abort: AbortSignal) -> Result<DeferredStatus, AiError>;
}

/// Owned provider controller for one deferred response.
pub type DeferredLanguageAdapterHandle = Box<dyn DeferredLanguageOperation>;

fn deferred_unsupported() -> AiError {
    AiError::new(
        ErrorKind::Unsupported,
        ErrorPhase::Prepare,
        DispatchStatus::NotStarted,
        "provider does not support deferred language responses",
    )
    .expect("static deferred error is valid")
}

/// Image provider seam.
pub trait ImageAdapter: fmt::Debug + Send + Sync {
    /// Validates and freezes one image call without performing provider I/O.
    fn prepare(
        &self,
        context: PrepareContext,
        model: String,
        request: ImageRequest,
    ) -> AdapterFuture<Result<Prepared<ImageAdapterStream>, AiError>>;
}

/// Transcription provider seam.
pub trait TranscriptionAdapter: fmt::Debug + Send + Sync {
    /// Validates and freezes one transcription call without performing provider I/O.
    fn prepare(
        &self,
        context: PrepareContext,
        model: String,
        request: TranscriptionRequest,
    ) -> AdapterFuture<Result<Prepared<TranscriptionAdapterStream>, AiError>>;
}

/// Speech provider seam.
pub trait SpeechAdapter: fmt::Debug + Send + Sync {
    /// Validates and freezes one speech call without performing provider I/O.
    fn prepare(
        &self,
        context: PrepareContext,
        model: String,
        request: SpeechRequest,
    ) -> AdapterFuture<Result<Prepared<SpeechAdapterStream>, AiError>>;
}

/// Live provider transport hidden by the standalone Realtime façade.
#[async_trait]
pub trait RealtimeConnection: fmt::Debug + Send {
    /// Sends one validated live command.
    async fn send(&mut self, command: RealtimeCommand) -> Result<(), AiError>;
    /// Receives the next provider event, or clean EOF after closure.
    async fn next_event(&mut self) -> Result<Option<RealtimeEvent>, AiError>;
    /// Performs bounded orderly transport closure.
    async fn close(&mut self) -> Result<(), AiError>;
}

/// Owned live transport created by a prepared Realtime call.
pub type RealtimeAdapterTransport = Box<dyn RealtimeConnection>;

/// Realtime provider seam.
pub trait RealtimeAdapter: fmt::Debug + Send + Sync {
    /// Validates and freezes one live session without opening its transport.
    fn prepare(
        &self,
        context: PrepareContext,
        model: String,
        request: RealtimeRequest,
    ) -> AdapterFuture<Result<Prepared<RealtimeAdapterTransport>, AiError>>;
}

/// Immutable deployment registration consumed by a Registry or rsi-meta wrapper.
#[derive(Clone)]
pub struct ProviderRegistration {
    deployment_id: String,
    provider_family: String,
    protocol: String,
    transport: String,
    endpoint_fingerprint: String,
    config_generation: u64,
    credential: Option<CredentialRequirement>,
    retry_policy: RetryPolicy,
    language: Option<Arc<dyn LanguageAdapter>>,
    image: Option<Arc<dyn ImageAdapter>>,
    transcription: Option<Arc<dyn TranscriptionAdapter>>,
    speech: Option<Arc<dyn SpeechAdapter>>,
    realtime: Option<Arc<dyn RealtimeAdapter>>,
}

impl ProviderRegistration {
    /// Starts a registration builder for one exact deployment and provider family.
    pub fn builder(
        deployment_id: impl Into<String>,
        provider_family: impl Into<String>,
    ) -> Result<ProviderRegistrationBuilder, ProviderSdkError> {
        ProviderRegistrationBuilder::new(deployment_id.into(), provider_family.into())
    }

    /// Returns the exact routing key for this deployment.
    pub fn deployment_id(&self) -> &str {
        &self.deployment_id
    }

    /// Returns the provider family that owns translation policy.
    pub fn provider_family(&self) -> &str {
        &self.provider_family
    }

    /// Returns the frozen provider protocol family.
    pub fn protocol(&self) -> &str {
        &self.protocol
    }

    /// Returns the frozen transport kind.
    pub fn transport(&self) -> &str {
        &self.transport
    }

    /// Returns redacted endpoint identity used in prepared snapshots.
    pub fn endpoint_fingerprint(&self) -> &str {
        &self.endpoint_fingerprint
    }

    /// Returns the immutable provider configuration generation.
    pub const fn config_generation(&self) -> u64 {
        self.config_generation
    }

    /// Returns the provider credential requirement, when any.
    pub const fn credential(&self) -> Option<&CredentialRequirement> {
        self.credential.as_ref()
    }

    /// Returns the finite orchestration retry facts.
    pub const fn retry_policy(&self) -> &RetryPolicy {
        &self.retry_policy
    }

    pub fn language(&self) -> Option<&Arc<dyn LanguageAdapter>> {
        self.language.as_ref()
    }

    pub fn image(&self) -> Option<&Arc<dyn ImageAdapter>> {
        self.image.as_ref()
    }

    pub fn transcription(&self) -> Option<&Arc<dyn TranscriptionAdapter>> {
        self.transcription.as_ref()
    }

    pub fn speech(&self) -> Option<&Arc<dyn SpeechAdapter>> {
        self.speech.as_ref()
    }

    pub fn realtime(&self) -> Option<&Arc<dyn RealtimeAdapter>> {
        self.realtime.as_ref()
    }
}

impl fmt::Debug for ProviderRegistration {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ProviderRegistration")
            .field("deployment_id", &self.deployment_id)
            .field("provider_family", &self.provider_family)
            .field("protocol", &self.protocol)
            .field("transport", &self.transport)
            .field("endpoint_fingerprint", &self.endpoint_fingerprint)
            .field("config_generation", &self.config_generation)
            .field("credential", &self.credential)
            .field("retry_policy", &self.retry_policy)
            .field("language", &self.language.is_some())
            .field("image", &self.image.is_some())
            .field("transcription", &self.transcription.is_some())
            .field("speech", &self.speech.is_some())
            .field("realtime", &self.realtime.is_some())
            .finish()
    }
}

/// Builder that makes one deployment's capability claims explicit.
#[derive(Default)]
pub struct ProviderRegistrationBuilder {
    deployment_id: String,
    provider_family: String,
    protocol: String,
    transport: String,
    endpoint_fingerprint: String,
    config_generation: u64,
    credential: Option<CredentialRequirement>,
    retry_policy: RetryPolicy,
    language: Option<Arc<dyn LanguageAdapter>>,
    image: Option<Arc<dyn ImageAdapter>>,
    transcription: Option<Arc<dyn TranscriptionAdapter>>,
    speech: Option<Arc<dyn SpeechAdapter>>,
    realtime: Option<Arc<dyn RealtimeAdapter>>,
}

impl ProviderRegistrationBuilder {
    fn new(deployment_id: String, provider_family: String) -> Result<Self, ProviderSdkError> {
        validate_id("deployment_id", &deployment_id)?;
        validate_id("provider_family", &provider_family)?;
        Ok(Self {
            protocol: provider_family.clone(),
            transport: "adapter".to_owned(),
            endpoint_fingerprint: deployment_id.clone(),
            deployment_id,
            provider_family,
            ..Self::default()
        })
    }

    #[must_use]
    /// Sets the credential requirement resolved during preparation.
    pub fn with_credential(mut self, credential: CredentialRequirement) -> Self {
        self.credential = Some(credential);
        self
    }

    #[must_use]
    /// Replaces the finite retry facts attached to prepared snapshots.
    pub fn with_retry_policy(mut self, policy: RetryPolicy) -> Self {
        self.retry_policy = policy;
        self
    }

    /// Sets protocol, transport, and redacted endpoint identity together.
    pub fn with_protocol(
        mut self,
        protocol: impl Into<String>,
        transport: impl Into<String>,
        endpoint_fingerprint: impl Into<String>,
    ) -> Result<Self, ProviderSdkError> {
        let protocol = protocol.into();
        let transport = transport.into();
        let endpoint_fingerprint = endpoint_fingerprint.into();
        validate_id("protocol", &protocol)?;
        validate_id("transport", &transport)?;
        validate_id("endpoint_fingerprint", &endpoint_fingerprint)?;
        self.protocol = protocol;
        self.transport = transport;
        self.endpoint_fingerprint = endpoint_fingerprint;
        Ok(self)
    }

    #[must_use]
    /// Sets the provider configuration generation frozen by later calls.
    pub const fn with_config_generation(mut self, generation: u64) -> Self {
        self.config_generation = generation;
        self
    }

    #[must_use]
    pub fn with_language<A>(mut self, adapter: A) -> Self
    where
        A: LanguageAdapter + 'static,
    {
        self.language = Some(Arc::new(adapter));
        self
    }

    #[must_use]
    pub fn with_image<A>(mut self, adapter: A) -> Self
    where
        A: ImageAdapter + 'static,
    {
        self.image = Some(Arc::new(adapter));
        self
    }

    #[must_use]
    pub fn with_transcription<A>(mut self, adapter: A) -> Self
    where
        A: TranscriptionAdapter + 'static,
    {
        self.transcription = Some(Arc::new(adapter));
        self
    }

    #[must_use]
    pub fn with_speech<A>(mut self, adapter: A) -> Self
    where
        A: SpeechAdapter + 'static,
    {
        self.speech = Some(Arc::new(adapter));
        self
    }

    #[must_use]
    pub fn with_realtime<A>(mut self, adapter: A) -> Self
    where
        A: RealtimeAdapter + 'static,
    {
        self.realtime = Some(Arc::new(adapter));
        self
    }

    /// Freezes a deployment that exposes at least one capability adapter.
    pub fn build(self) -> Result<ProviderRegistration, ProviderSdkError> {
        if self.language.is_none()
            && self.image.is_none()
            && self.transcription.is_none()
            && self.speech.is_none()
            && self.realtime.is_none()
        {
            return Err(ProviderSdkError::new(
                "provider.no_capabilities",
                "provider registration exposes no capability adapter",
            ));
        }
        Ok(ProviderRegistration {
            deployment_id: self.deployment_id,
            provider_family: self.provider_family,
            protocol: self.protocol,
            transport: self.transport,
            endpoint_fingerprint: self.endpoint_fingerprint,
            config_generation: self.config_generation,
            credential: self.credential,
            retry_policy: self.retry_policy,
            language: self.language,
            image: self.image,
            transcription: self.transcription,
            speech: self.speech,
            realtime: self.realtime,
        })
    }
}

impl fmt::Debug for ProviderRegistrationBuilder {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ProviderRegistrationBuilder")
            .field("deployment_id", &self.deployment_id)
            .field("provider_family", &self.provider_family)
            .finish_non_exhaustive()
    }
}

/// Invalid provider-author registration.
#[derive(Clone, Debug, Eq, Error, PartialEq)]
#[error("{message}")]
pub struct ProviderSdkError {
    code: &'static str,
    message: String,
}

impl ProviderSdkError {
    fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }

    /// Returns the stable machine-readable provider SDK failure code.
    pub const fn code(&self) -> &'static str {
        self.code
    }
}

fn validate_id(field: &str, value: &str) -> Result<(), ProviderSdkError> {
    rsi_ai_protocol::validate_identifier(field, value)
        .map_err(|message| ProviderSdkError::new("provider.invalid_id", message))
}
