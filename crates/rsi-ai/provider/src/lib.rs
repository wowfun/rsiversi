//! Capability-specific provider-author interfaces for `rsi-ai`.

#![deny(unsafe_code)]
#![allow(clippy::missing_errors_doc)] // Provider SDK errors expose stable codes.
#![allow(clippy::missing_panics_doc)] // Private construction invariants back static-error expects.

use std::{
    collections::HashMap,
    fmt,
    future::Future,
    pin::Pin,
    sync::{
        Arc, LazyLock, Mutex, Weak,
        atomic::{AtomicBool, Ordering},
    },
};

use async_trait::async_trait;
use futures_util::{FutureExt as _, Stream, future::Shared};
use rsi_ai_protocol::{
    AiError, DeferredLanguageBatch as CallerDeferredLanguageBatch,
    DeferredLanguageCheckpoint as CallerDeferredLanguageCheckpoint,
    DeferredStatus as CallerDeferredStatus, DispatchStatus, ErrorKind, ErrorPhase, ImageEvent,
    ImageRequest, LanguageEvent, LanguageProfile, LanguageRequest, MAX_EXTENSION_BYTES,
    PreparedCallSnapshot, ProviderExtension, RetryPolicy, sanitize_error_summary,
};
use rsi_credentials_protocol::{CredentialRef, ResolvedCredential};
use rsi_media_protocol::{MediaDescriptor, MediaRead};
use rsi_meta_contract::LocalContract;
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

impl From<DeferredStatus> for CallerDeferredStatus {
    fn from(status: DeferredStatus) -> Self {
        match status {
            DeferredStatus::Queued => Self::Queued,
            DeferredStatus::InProgress => Self::InProgress,
            DeferredStatus::Completed => Self::Completed,
            DeferredStatus::Failed => Self::Failed,
            DeferredStatus::Cancelled => Self::Cancelled,
        }
    }
}

impl From<CallerDeferredStatus> for DeferredStatus {
    fn from(status: CallerDeferredStatus) -> Self {
        match status {
            CallerDeferredStatus::Queued => Self::Queued,
            CallerDeferredStatus::InProgress => Self::InProgress,
            CallerDeferredStatus::Completed => Self::Completed,
            CallerDeferredStatus::Failed => Self::Failed,
            CallerDeferredStatus::Cancelled => Self::Cancelled,
        }
    }
}

/// Persistable cursor for a provider-managed background language response.
///
/// `provider_state` is bounded parser state, never response bytes, credentials,
/// or accumulated model output. A durable caller commits each emitted batch and
/// this checkpoint atomically before resuming after `sequence_number`.
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
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
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
            event_stream_terminal: false,
            sequence_number: None,
            provider_state,
        };
        checkpoint.validate()?;
        Ok(checkpoint)
    }

    /// Revalidates a checkpoint decoded from durable state.
    pub fn validate(&self) -> Result<(), ProviderSdkError> {
        self.call.validate().map_err(|error| {
            ProviderSdkError::new("provider.invalid_deferred_checkpoint", error.to_string())
        })?;
        validate_id("operation_id", &self.operation_id)?;
        if self.event_stream_terminal
            && (self.sequence_number.is_none() || !self.status.is_terminal())
        {
            return Err(ProviderSdkError::new(
                "provider.invalid_deferred_checkpoint",
                "a terminal deferred event stream requires a terminal status and sequence number",
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

    /// Returns whether a terminal output event has been durably consumed.
    pub const fn event_stream_terminal(&self) -> bool {
        self.event_stream_terminal
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
        event_stream_terminal: bool,
        sequence_number: u64,
        provider_state: Option<ProviderExtension>,
    ) -> Result<(), ProviderSdkError> {
        validate_status_transition(self.status, status)?;
        if self.event_stream_terminal && !event_stream_terminal {
            return Err(ProviderSdkError::new(
                "provider.invalid_deferred_checkpoint",
                "deferred event-stream terminal state cannot regress",
            ));
        }
        if event_stream_terminal && !status.is_terminal() {
            return Err(ProviderSdkError::new(
                "provider.invalid_deferred_checkpoint",
                "a terminal deferred event stream requires a terminal status",
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
        self.event_stream_terminal = event_stream_terminal;
        self.sequence_number = Some(sequence_number);
        self.provider_state = provider_state;
        Ok(())
    }

    /// Converts this provider cursor into the public durable caller contract.
    pub fn to_caller(&self) -> Result<CallerDeferredLanguageCheckpoint, ProviderSdkError> {
        let mut checkpoint = CallerDeferredLanguageCheckpoint::new(
            self.call.clone(),
            self.operation_id.clone(),
            self.status.into(),
            self.provider_state.clone(),
        )
        .map_err(|error| {
            ProviderSdkError::new("provider.invalid_deferred_checkpoint", error.to_string())
        })?;
        if let Some(sequence) = self.sequence_number {
            checkpoint
                .advance(
                    self.status.into(),
                    self.event_stream_terminal,
                    sequence,
                    self.provider_state.clone(),
                )
                .map_err(|error| {
                    ProviderSdkError::new("provider.invalid_deferred_checkpoint", error.to_string())
                })?;
        }
        Ok(checkpoint)
    }

    /// Converts one validated public cursor into the provider-author contract.
    pub fn from_caller(
        checkpoint: &CallerDeferredLanguageCheckpoint,
    ) -> Result<Self, ProviderSdkError> {
        checkpoint.validate().map_err(|error| {
            ProviderSdkError::new("provider.invalid_deferred_checkpoint", error.to_string())
        })?;
        let mut provider = Self::new(
            checkpoint.call().clone(),
            checkpoint.operation_id(),
            checkpoint.status().into(),
            checkpoint.provider_state().cloned(),
        )?;
        if let Some(sequence) = checkpoint.sequence_number() {
            provider.advance(
                checkpoint.status().into(),
                checkpoint.event_stream_terminal(),
                sequence,
                checkpoint.provider_state().cloned(),
            )?;
        }
        Ok(provider)
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

/// Atomic event/checkpoint batches returned by one deferred resume request.
pub type DeferredLanguageAdapterStream = AdapterStream<DeferredLanguageBatch>;

impl DeferredLanguageBatch {
    /// Couples one bounded event batch with the cursor immediately after it.
    pub fn new(
        events: Vec<LanguageEvent>,
        checkpoint: DeferredLanguageCheckpoint,
    ) -> Result<Self, ProviderSdkError> {
        const MAX_EVENTS: usize = rsi_ai_protocol::MAX_CONTENT_BLOCKS + 2;
        if events.len() > MAX_EVENTS {
            return Err(ProviderSdkError::new(
                "provider.invalid_deferred_batch",
                format!(
                    "one deferred provider event expanded to more than {MAX_EVENTS} normalized events"
                ),
            ));
        }
        for event in &events {
            event.validate().map_err(|error| {
                ProviderSdkError::new("provider.invalid_deferred_batch", error.to_string())
            })?;
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

    /// Converts an adapter batch into the public atomic caller contract.
    pub fn to_caller(&self) -> Result<CallerDeferredLanguageBatch, ProviderSdkError> {
        CallerDeferredLanguageBatch::new(self.events.clone(), self.checkpoint.to_caller()?).map_err(
            |error| ProviderSdkError::new("provider.invalid_deferred_batch", error.to_string()),
        )
    }
}

/// Provider-private context whose secret half is intentionally not serializable.
#[derive(Clone)]
pub struct PrepareContext {
    snapshot: PreparedCallSnapshot,
    credential: Option<ResolvedCredential>,
    media: Arc<dyn MediaResolver>,
    resolved_media: Arc<MediaResolutions>,
}

const MEDIA_ADMISSION_UNIT_BYTES: u64 = 1024 * 1024;
const MAXIMUM_RESIDENT_MEDIA_MIB: usize = 256;
/// Maximum unique declared media bytes retained by one process across prepared calls.
pub const MAXIMUM_RESIDENT_MEDIA_BYTES: u64 =
    MAXIMUM_RESIDENT_MEDIA_MIB as u64 * MEDIA_ADMISSION_UNIT_BYTES;
static MEDIA_RESIDENT_ADMISSION: LazyLock<Arc<tokio::sync::Semaphore>> =
    LazyLock::new(|| Arc::new(tokio::sync::Semaphore::new(MAXIMUM_RESIDENT_MEDIA_MIB)));

#[derive(Clone)]
struct ValidatedMedia {
    bytes: Arc<[u8]>,
    admission: Arc<tokio::sync::OwnedSemaphorePermit>,
}

type MediaResolutionFuture = Shared<AdapterFuture<Result<ValidatedMedia, AiError>>>;
type MediaAdmissionFuture =
    Shared<AdapterFuture<Result<Arc<tokio::sync::OwnedSemaphorePermit>, AiError>>>;

struct MediaResolutionFlight {
    future: MediaResolutionFuture,
    admission: Arc<MediaAdmissionFlight>,
}

struct MediaAdmissionFlight {
    future: MediaAdmissionFuture,
}

enum MediaAdmission {
    Empty,
    Pending(Weak<MediaAdmissionFlight>),
    Ready(Arc<tokio::sync::OwnedSemaphorePermit>),
}

enum MediaResolutionEntry {
    Pending {
        flight: Arc<MediaResolutionFlight>,
        waiters: usize,
    },
    Ready(ValidatedMedia),
}

struct MediaResolutionWaiter {
    resolutions: Arc<MediaResolutions>,
    descriptor: MediaDescriptor,
    flight: Arc<MediaResolutionFlight>,
}

impl Drop for MediaResolutionWaiter {
    fn drop(&mut self) {
        self.resolutions
            .release_waiter(&self.descriptor, &self.flight);
    }
}

enum MediaResolution {
    Pending(MediaResolutionWaiter),
    Ready(ValidatedMedia),
}

struct MediaResolutions {
    entries: Mutex<HashMap<MediaDescriptor, MediaResolutionEntry>>,
    admission: Mutex<MediaAdmission>,
    admission_units: u32,
    semaphore: Arc<tokio::sync::Semaphore>,
}

impl MediaResolutions {
    fn new(media_admission_bytes: u64) -> Result<Self, ProviderSdkError> {
        Self::new_with_semaphore(media_admission_bytes, Arc::clone(&MEDIA_RESIDENT_ADMISSION))
    }

    fn new_with_semaphore(
        media_admission_bytes: u64,
        semaphore: Arc<tokio::sync::Semaphore>,
    ) -> Result<Self, ProviderSdkError> {
        validate_media_admission_bytes(media_admission_bytes)?;
        let units = media_admission_bytes.saturating_add(MEDIA_ADMISSION_UNIT_BYTES - 1)
            / MEDIA_ADMISSION_UNIT_BYTES;
        let admission_units = u32::try_from(units).map_err(|_| {
            ProviderSdkError::new(
                "provider.media_admission_exceeded",
                "media admission weight exceeds its representation",
            )
        })?;
        Ok(Self {
            entries: Mutex::new(HashMap::new()),
            admission: Mutex::new(MediaAdmission::Empty),
            admission_units,
            semaphore,
        })
    }

    fn admission(&self) -> Arc<MediaAdmissionFlight> {
        let mut admission = self
            .admission
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        match &*admission {
            MediaAdmission::Pending(flight) => {
                if let Some(flight) = flight.upgrade() {
                    return flight;
                }
            }
            MediaAdmission::Ready(permit) => {
                let permit = Arc::clone(permit);
                let future: AdapterFuture<Result<Arc<tokio::sync::OwnedSemaphorePermit>, AiError>> =
                    Box::pin(async move { Ok(permit) });
                return Arc::new(MediaAdmissionFlight {
                    future: future.shared(),
                });
            }
            MediaAdmission::Empty => {}
        }
        let semaphore = Arc::clone(&self.semaphore);
        let units = self.admission_units;
        let future: AdapterFuture<Result<Arc<tokio::sync::OwnedSemaphorePermit>, AiError>> =
            Box::pin(async move {
                let admission = semaphore
                    .acquire_many_owned(units)
                    .await
                    .expect("static media admission remains open");
                Ok(Arc::new(admission))
            });
        let flight = Arc::new(MediaAdmissionFlight {
            future: future.shared(),
        });
        *admission = MediaAdmission::Pending(Arc::downgrade(&flight));
        flight
    }

    fn for_descriptor(
        self: &Arc<Self>,
        descriptor: &MediaDescriptor,
        media: Arc<dyn MediaResolver>,
    ) -> MediaResolution {
        let admission = self.admission();
        let mut entries = self
            .entries
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let entry = entries.entry(descriptor.clone()).or_insert_with(|| {
            let descriptor = descriptor.clone();
            let resolver_descriptor = descriptor.clone();
            let admitted = Arc::clone(&admission);
            let future: AdapterFuture<Result<ValidatedMedia, AiError>> = Box::pin(async move {
                let admission = admitted.future.clone().await?;
                let bytes = media.read(resolver_descriptor, AbortSignal::new()).await?;
                let bytes = validate_resolved_media_blocking(descriptor, bytes).await?;
                Ok(ValidatedMedia { bytes, admission })
            });
            MediaResolutionEntry::Pending {
                flight: Arc::new(MediaResolutionFlight {
                    future: future.shared(),
                    admission,
                }),
                waiters: 0,
            }
        });
        match entry {
            MediaResolutionEntry::Pending { flight, waiters } => {
                *waiters = waiters
                    .checked_add(1)
                    .expect("one process cannot retain usize::MAX media waiters");
                MediaResolution::Pending(MediaResolutionWaiter {
                    resolutions: Arc::clone(self),
                    descriptor: descriptor.clone(),
                    flight: Arc::clone(flight),
                })
            }
            MediaResolutionEntry::Ready(media) => MediaResolution::Ready(media.clone()),
        }
    }

    fn release_waiter(&self, descriptor: &MediaDescriptor, flight: &Arc<MediaResolutionFlight>) {
        let mut entries = self
            .entries
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let remove = match entries.get_mut(descriptor) {
            Some(MediaResolutionEntry::Pending {
                flight: current,
                waiters,
            }) if Arc::ptr_eq(current, flight) => {
                *waiters = waiters
                    .checked_sub(1)
                    .expect("a media waiter is released exactly once");
                *waiters == 0
            }
            Some(MediaResolutionEntry::Pending { .. } | MediaResolutionEntry::Ready(_)) | None => {
                false
            }
        };
        if remove {
            entries.remove(descriptor);
        }
    }

    fn retain_admission(
        &self,
        flight: &Arc<MediaAdmissionFlight>,
        permit: &Arc<tokio::sync::OwnedSemaphorePermit>,
    ) {
        let mut admission = self
            .admission
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if matches!(
            &*admission,
            MediaAdmission::Pending(current)
                if current.upgrade().is_some_and(|current| Arc::ptr_eq(&current, flight))
        ) {
            *admission = MediaAdmission::Ready(Arc::clone(permit));
        }
    }

    fn finish(
        &self,
        descriptor: &MediaDescriptor,
        flight: &Arc<MediaResolutionFlight>,
        result: &Result<ValidatedMedia, AiError>,
    ) {
        let mut entries = self
            .entries
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let is_current = matches!(
            entries.get(descriptor),
            Some(MediaResolutionEntry::Pending { flight: current, .. })
                if Arc::ptr_eq(current, flight)
        );
        if !is_current {
            return;
        }
        match result {
            Ok(media) => {
                self.retain_admission(&flight.admission, &media.admission);
                entries.insert(
                    descriptor.clone(),
                    MediaResolutionEntry::Ready(media.clone()),
                );
            }
            Err(_) => {
                entries.remove(descriptor);
            }
        }
    }
}

/// Validates one prepared call's unique declared media bytes against process admission.
pub fn validate_media_admission_bytes(media_admission_bytes: u64) -> Result<(), ProviderSdkError> {
    if media_admission_bytes > MAXIMUM_RESIDENT_MEDIA_BYTES {
        return Err(ProviderSdkError::new(
            "provider.media_admission_exceeded",
            format!(
                "prepared call declares {media_admission_bytes} unique media bytes, exceeding the {MAXIMUM_RESIDENT_MEDIA_BYTES}-byte process resident bound"
            ),
        ));
    }
    Ok(())
}

fn validate_resolved_media(descriptor: &MediaDescriptor, bytes: &[u8]) -> Result<(), AiError> {
    if u64::try_from(bytes.len()).ok() != Some(descriptor.byte_len()) {
        return Err(AiError::new(
            ErrorKind::Artifact,
            ErrorPhase::Send,
            DispatchStatus::NotDispatched,
            "resolved media length does not match its descriptor",
        )
        .expect("static media error is valid"));
    }
    let digest = hex::encode(Sha256::digest(bytes));
    if digest != descriptor.sha256() {
        return Err(AiError::new(
            ErrorKind::Artifact,
            ErrorPhase::Send,
            DispatchStatus::NotDispatched,
            "resolved media digest does not match its descriptor",
        )
        .expect("static media error is valid"));
    }
    Ok(())
}

async fn validate_resolved_media_blocking(
    descriptor: MediaDescriptor,
    bytes: Arc<[u8]>,
) -> Result<Arc<[u8]>, AiError> {
    tokio::task::spawn_blocking(move || {
        validate_resolved_media(&descriptor, &bytes)?;
        Ok(bytes)
    })
    .await
    .map_err(|_| {
        AiError::new(
            ErrorKind::Artifact,
            ErrorPhase::Send,
            DispatchStatus::NotDispatched,
            "media validation worker failed",
        )
        .expect("static media worker error is valid")
    })?
}

fn media_waiter_cancelled() -> AiError {
    AiError::new(
        ErrorKind::Cancelled,
        ErrorPhase::Send,
        DispatchStatus::NotDispatched,
        "media resolution waiter was cancelled",
    )
    .expect("static media cancellation is valid")
}

impl PrepareContext {
    /// Couples redacted snapshot facts with nonserializable secret and media access.
    pub fn new(
        snapshot: PreparedCallSnapshot,
        credential: Option<ResolvedCredential>,
        media: Arc<dyn MediaResolver>,
        media_admission_bytes: u64,
    ) -> Result<Self, ProviderSdkError> {
        snapshot.validate().map_err(|error| {
            ProviderSdkError::new("provider.invalid_prepare_context", error.to_string())
        })?;
        Ok(Self {
            snapshot,
            credential,
            media,
            resolved_media: Arc::new(MediaResolutions::new(media_admission_bytes)?),
        })
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

    /// Releases successful media bodies after the adapter's final resolution.
    pub fn release_resolved_media(&self) {
        self.resolved_media
            .entries
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clear();
        let mut admission = self
            .resolved_media
            .admission
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        *admission = MediaAdmission::Empty;
    }

    /// Reads and verifies one media body at Start-time.
    pub async fn resolve_media(
        &self,
        descriptor: &MediaDescriptor,
        abort: AbortSignal,
    ) -> Result<Arc<[u8]>, AiError> {
        let resolution = self
            .resolved_media
            .for_descriptor(descriptor, Arc::clone(&self.media));
        let waiter = match resolution {
            MediaResolution::Pending(waiter) => waiter,
            MediaResolution::Ready(media) => return Ok(media.bytes),
        };
        let result = tokio::select! {
            biased;
            () = abort.cancelled() => return Err(media_waiter_cancelled()),
            result = waiter.flight.future.clone() => result,
        };
        self.resolved_media
            .finish(descriptor, &waiter.flight, &result);
        result.map(|media| media.bytes)
    }
}

impl fmt::Debug for PrepareContext {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PrepareContext")
            .field("snapshot", &self.snapshot)
            .field(
                "credential",
                &self
                    .credential
                    .as_ref()
                    .map(|credential| &credential.source),
            )
            .field("media", &self.media)
            .finish_non_exhaustive()
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

/// Default resolver used when a router call has no durable Media reader.
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

/// Adapter from the durable Base Media read contract to provider-author media resolution.
#[derive(Clone, Debug)]
pub struct DurableMediaResolver {
    reader: Arc<dyn MediaRead>,
}

impl DurableMediaResolver {
    /// Pins one exact Media service generation for a prepared operation.
    #[must_use]
    pub fn new(reader: Arc<dyn MediaRead>) -> Self {
        Self { reader }
    }
}

impl MediaResolver for DurableMediaResolver {
    fn read(
        &self,
        descriptor: MediaDescriptor,
        abort: AbortSignal,
    ) -> AdapterFuture<Result<Arc<[u8]>, AiError>> {
        let reader = Arc::clone(&self.reader);
        Box::pin(async move {
            tokio::select! {
                biased;
                () = abort.cancelled() => Err(media_waiter_cancelled()),
                result = reader.read_descriptor(&descriptor) => result
                    .map(|body| body.bytes)
                    .map_err(|error| {
                        let summary = sanitize_error_summary(&error.to_string());
                        AiError::new(
                            ErrorKind::Artifact,
                            ErrorPhase::Send,
                            DispatchStatus::NotDispatched,
                            summary,
                        )
                        .expect("sanitized Media error is valid")
                    }),
            }
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

    /// Wraps an operation-owned cancellation token without a second watcher task.
    #[must_use]
    pub fn from_cancellation_token(token: CancellationToken) -> Self {
        Self(token)
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

    /// Checks model/request compatibility without resolving effect dependencies.
    fn validate_request(&self, model: &str, request: &LanguageRequest) -> Result<(), AiError>;

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
    /// Checks model/request compatibility without resolving effect dependencies.
    fn validate_request(&self, model: &str, request: &ImageRequest) -> Result<(), AiError>;

    /// Validates and freezes one image call without performing provider I/O.
    fn prepare(
        &self,
        context: PrepareContext,
        model: String,
        request: ImageRequest,
    ) -> AdapterFuture<Result<Prepared<ImageAdapterStream>, AiError>>;
}

/// Immutable deployment registration consumed by capability routers.
#[derive(Clone)]
pub struct ProviderRegistration {
    deployment_id: String,
    provider_family: String,
    protocol: String,
    image_protocol: Option<String>,
    transport: String,
    endpoint_fingerprint: String,
    config_generation: u64,
    credential: Option<CredentialRef>,
    retry_policy: RetryPolicy,
    language: Option<Arc<dyn LanguageAdapter>>,
    image: Option<Arc<dyn ImageAdapter>>,
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

    /// Returns the frozen Image protocol, falling back only for single-protocol providers.
    pub fn image_protocol(&self) -> &str {
        self.image_protocol.as_deref().unwrap_or(&self.protocol)
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
    pub const fn credential(&self) -> Option<&CredentialRef> {
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
}

impl fmt::Debug for ProviderRegistration {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ProviderRegistration")
            .field("deployment_id", &self.deployment_id)
            .field("provider_family", &self.provider_family)
            .field("protocol", &self.protocol)
            .field("image_protocol", &self.image_protocol)
            .field("transport", &self.transport)
            .field("endpoint_fingerprint", &self.endpoint_fingerprint)
            .field("config_generation", &self.config_generation)
            .field("credential", &self.credential)
            .field("retry_policy", &self.retry_policy)
            .field("language", &self.language.is_some())
            .field("image", &self.image.is_some())
            .finish()
    }
}

/// Builder that makes one deployment's capability claims explicit.
#[derive(Default)]
pub struct ProviderRegistrationBuilder {
    deployment_id: String,
    provider_family: String,
    protocol: String,
    image_protocol: Option<String>,
    transport: String,
    endpoint_fingerprint: String,
    config_generation: u64,
    credential: Option<CredentialRef>,
    retry_policy: RetryPolicy,
    language: Option<Arc<dyn LanguageAdapter>>,
    image: Option<Arc<dyn ImageAdapter>>,
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
    pub fn with_credential(mut self, credential: CredentialRef) -> Self {
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

    /// Replaces the protocol identity used only by the Image facet.
    pub fn with_image_protocol(
        mut self,
        protocol: impl Into<String>,
    ) -> Result<Self, ProviderSdkError> {
        let protocol = protocol.into();
        validate_id("image_protocol", &protocol)?;
        self.image_protocol = Some(protocol);
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

    /// Freezes a deployment that exposes at least one capability adapter.
    pub fn build(self) -> Result<ProviderRegistration, ProviderSdkError> {
        if self.language.is_none() && self.image.is_none() {
            return Err(ProviderSdkError::new(
                "provider.no_capabilities",
                "provider registration exposes no capability adapter",
            ));
        }
        if self.config_generation == 0 {
            return Err(ProviderSdkError::new(
                "provider.invalid_generation",
                "provider registration requires a nonzero Fiber generation",
            ));
        }
        Ok(ProviderRegistration {
            deployment_id: self.deployment_id,
            provider_family: self.provider_family,
            protocol: self.protocol,
            image_protocol: self.image_protocol,
            transport: self.transport,
            endpoint_fingerprint: self.endpoint_fingerprint,
            config_generation: self.config_generation,
            credential: self.credential,
            retry_policy: self.retry_policy,
            language: self.language,
            image: self.image,
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

/// Shared publication fence for all facets contributed by one provider generation.
#[derive(Clone, Debug)]
pub struct RegistrationGate(Arc<AtomicBool>);

impl RegistrationGate {
    /// Creates a hidden multi-facet registration transaction.
    #[must_use]
    pub fn new() -> Self {
        Self(Arc::new(AtomicBool::new(false)))
    }

    /// Makes every route carrying this gate visible in one release operation.
    pub fn commit(&self) {
        self.0.store(true, Ordering::Release);
    }

    /// Returns whether the provider completed all facet registrations.
    pub fn is_committed(&self) -> bool {
        self.0.load(Ordering::Acquire)
    }
}

impl Default for RegistrationGate {
    fn default() -> Self {
        Self::new()
    }
}

/// Generation-owned route lease returned by one router registrar.
pub struct ProviderLease {
    cleanup: Option<Box<dyn FnOnce() + Send + Sync + 'static>>,
}

/// Generation-owned set of provider facet leases published behind one gate.
#[derive(Debug)]
pub struct ProviderPublication {
    leases: Vec<ProviderLease>,
}

impl ProviderPublication {
    /// Atomically publishes every facet carried by one immutable registration.
    ///
    /// Registrars reserve hidden exact routes first. Any failure drops the
    /// already acquired leases while they are still invisible; only the final
    /// gate commit makes all facets observable.
    pub fn publish(
        registration: Arc<ProviderRegistration>,
        language: Option<Arc<dyn LanguageRegistrar>>,
        image: Option<Arc<dyn ImageRegistrar>>,
    ) -> Result<Self, ProviderSdkError> {
        let gate = RegistrationGate::new();
        let mut leases = Vec::with_capacity(2);
        if registration.language().is_some() {
            let registrar = language.ok_or_else(|| {
                ProviderSdkError::new(
                    "provider.language_router_missing",
                    "provider registration has a Language facet but no Language registrar",
                )
            })?;
            leases.push(registrar.register_language(Arc::clone(&registration), gate.clone())?);
        }
        if registration.image().is_some() {
            let registrar = image.ok_or_else(|| {
                ProviderSdkError::new(
                    "provider.image_router_missing",
                    "provider registration has an Image facet but no Image registrar",
                )
            })?;
            leases.push(registrar.register_image(registration, gate.clone())?);
        }
        gate.commit();
        Ok(Self { leases })
    }

    /// Returns the number of independently leased facets in this publication.
    pub fn facet_count(&self) -> usize {
        self.leases.len()
    }
}

impl ProviderLease {
    /// Creates a lease from an exact conditional withdrawal action.
    pub fn new(cleanup: impl FnOnce() + Send + Sync + 'static) -> Self {
        Self {
            cleanup: Some(Box::new(cleanup)),
        }
    }
}

impl fmt::Debug for ProviderLease {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("ProviderLease(..)")
    }
}

impl Drop for ProviderLease {
    fn drop(&mut self) {
        if let Some(cleanup) = self.cleanup.take() {
            cleanup();
        }
    }
}

/// Private Language route contribution seam.
pub trait LanguageRegistrar: fmt::Debug + Send + Sync + 'static {
    /// Reserves one exact route hidden behind the shared provider gate.
    fn register_language(
        &self,
        registration: Arc<ProviderRegistration>,
        gate: RegistrationGate,
    ) -> Result<ProviderLease, ProviderSdkError>;
}

/// Private Image route contribution seam.
pub trait ImageRegistrar: fmt::Debug + Send + Sync + 'static {
    /// Reserves one exact route hidden behind the shared provider gate.
    fn register_image(
        &self,
        registration: Arc<ProviderRegistration>,
        gate: RegistrationGate,
    ) -> Result<ProviderLease, ProviderSdkError>;
}

/// Nominal Local contract for Language provider contributions.
#[derive(Debug)]
pub struct LanguageRegistrarContract;

impl LocalContract for LanguageRegistrarContract {
    const KEY: &'static str = "rsi.ai.language.registrar";
    type Service = dyn LanguageRegistrar;
}

/// Nominal Local contract for Image provider contributions.
#[derive(Debug)]
pub struct ImageRegistrarContract;

impl LocalContract for ImageRegistrarContract {
    const KEY: &'static str = "rsi.ai.image.registrar";
    type Service = dyn ImageRegistrar;
}

/// Invalid provider-author registration.
#[derive(Clone, Debug, Eq, Error, PartialEq)]
#[error("{message}")]
pub struct ProviderSdkError {
    code: &'static str,
    message: String,
}

impl ProviderSdkError {
    /// Creates a bounded provider-registration failure at its owning seam.
    pub fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: sanitize_error_summary(&message.into()),
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

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn media_admission_waits_for_the_complete_request_weight() {
        let semaphore = Arc::new(tokio::sync::Semaphore::new(3));
        let first = MediaResolutions::new_with_semaphore(
            2 * MEDIA_ADMISSION_UNIT_BYTES,
            Arc::clone(&semaphore),
        )
        .expect("first admission");
        let second = MediaResolutions::new_with_semaphore(
            2 * MEDIA_ADMISSION_UNIT_BYTES,
            Arc::clone(&semaphore),
        )
        .expect("second admission");
        let first_permit = first
            .admission()
            .future
            .clone()
            .await
            .expect("first request acquires");
        let second_task = tokio::spawn(second.admission().future.clone());

        tokio::task::yield_now().await;
        assert!(
            !second_task.is_finished(),
            "a request must not proceed with only part of its declared weight"
        );

        drop(first_permit);
        drop(first);
        tokio::time::timeout(std::time::Duration::from_secs(1), second_task)
            .await
            .expect("second request acquires after complete capacity is released")
            .expect("admission task")
            .expect("admission remains open");
    }

    #[tokio::test]
    async fn dropping_the_last_admission_waiter_releases_its_semaphore_position() {
        let semaphore = Arc::new(tokio::sync::Semaphore::new(1));
        let held = Arc::clone(&semaphore)
            .acquire_owned()
            .await
            .expect("test capacity");
        let body = b"queued media";
        let descriptor = MediaDescriptor::new(
            rsi_ai_protocol::MediaKind::Image,
            "image/png",
            u64::try_from(body.len()).expect("test body length"),
            hex::encode(Sha256::digest(body)),
        )
        .expect("descriptor");
        let context = PrepareContext {
            snapshot: PreparedCallSnapshot {
                call_id: "1".to_owned(),
                deployment_id: "deployment".to_owned(),
                provider_family: "provider".to_owned(),
                capability: rsi_ai_protocol::AiCapability::Language,
                model: "model".to_owned(),
                protocol: "protocol".to_owned(),
                transport: "transport".to_owned(),
                endpoint_fingerprint: "endpoint".to_owned(),
                config_generation: 1,
                credential_source: None,
                retry_policy: RetryPolicy::default(),
                request_sha256: "0".repeat(64),
            },
            credential: None,
            media: Arc::new(MissingMediaResolver),
            resolved_media: Arc::new(
                MediaResolutions::new_with_semaphore(
                    MEDIA_ADMISSION_UNIT_BYTES,
                    Arc::clone(&semaphore),
                )
                .expect("abandoned admission"),
            ),
        };
        let mut waiter = Box::pin(context.resolve_media(&descriptor, AbortSignal::new()));
        assert!(futures_util::poll!(waiter.as_mut()).is_pending());
        drop(waiter);

        drop(held);
        let _permit = tokio::time::timeout(
            std::time::Duration::from_secs(1),
            Arc::clone(&semaphore).acquire_owned(),
        )
        .await
        .expect("an abandoned waiter must not retain released capacity")
        .expect("semaphore remains open");
    }
}
