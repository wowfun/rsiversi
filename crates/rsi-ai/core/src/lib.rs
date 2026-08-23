//! Standalone provider-neutral AI runtime.

#![deny(unsafe_code)]
#![allow(clippy::missing_errors_doc)] // RegistryError and stream errors carry stable codes.
#![allow(clippy::missing_panics_doc)] // Resolved handles preserve internal adapter invariants.

use std::{
    collections::BTreeMap,
    fmt,
    marker::PhantomData,
    pin::Pin,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    task::{Context, Poll},
};

use futures_util::Stream;
use rsi_ai_auth::CredentialManager;
use rsi_ai_protocol::{
    AiError, DispatchStatus, ErrorKind, ErrorPhase, ImageAssembler, ImageEvent, ImageOutput,
    ImageRequest, LanguageAssembler, LanguageAssemblyError, LanguageEvent, LanguageOutput,
    LanguageRequest, RealtimeCommand, RealtimeEvent, RealtimeRequest, RealtimeValidator,
    SpeechAssembler, SpeechEvent, SpeechOutput, SpeechRequest, StreamError, TranscriptionAssembler,
    TranscriptionEvent, TranscriptionOutput, TranscriptionRequest,
};
use rsi_ai_provider::{
    AbortSignal, Capability, DeferredLanguageAdapterHandle, DeferredLanguageAdapterStream,
    ImageAdapterStream, LanguageAdapterStream, MediaResolver, MissingMediaResolver, PrepareContext,
    Prepared, PreparedCallSnapshot, ProviderRegistration, RealtimeAdapterTransport,
    SpeechAdapterStream, TranscriptionAdapterStream,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;

pub use rsi_ai_protocol;
pub use rsi_ai_provider::{
    DeferredLanguageBatch as DeferredLanguageChunk, DeferredLanguageCheckpoint, DeferredStatus,
};

/// Exact standalone provider deployment and model selection.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModelRef {
    provider: String,
    model: String,
}

impl ModelRef {
    pub fn new(
        provider: impl Into<String>,
        model: impl Into<String>,
    ) -> Result<Self, RegistryError> {
        let provider = provider.into();
        let model = model.into();
        validate_id("provider", &provider)?;
        validate_id("model", &model)?;
        Ok(Self { provider, model })
    }

    pub fn provider(&self) -> &str {
        &self.provider
    }

    pub fn model(&self) -> &str {
        &self.model
    }
}

/// Facts known for one resolved model handle.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModelDescriptor {
    pub provider: String,
    pub model: String,
    pub capability: Capability,
}

/// Immutable exact-routing registry.
#[derive(Clone)]
pub struct Registry {
    inner: Arc<RegistryInner>,
}

struct RegistryInner {
    credentials: CredentialManager,
    media: Arc<dyn MediaResolver>,
    registrations: BTreeMap<String, Arc<ProviderRegistration>>,
    next_call_id: AtomicU64,
}

impl Registry {
    /// Starts a builder with immutable credential resolution and no media resolver.
    #[must_use]
    pub fn builder(credentials: CredentialManager) -> RegistryBuilder {
        RegistryBuilder {
            credentials,
            media: Arc::new(MissingMediaResolver),
            registrations: BTreeMap::new(),
        }
    }

    #[allow(clippy::needless_pass_by_value)] // All capability selectors intentionally share ownership semantics.
    /// Resolves an exact language handle without provider I/O.
    pub fn language(&self, model: ModelRef) -> Result<LanguageModel, RegistryError> {
        let registration = self.registration(&model)?;
        if registration.language().is_none() {
            return Err(RegistryError::new(
                "registry.capability_unavailable",
                format!("provider `{}` has no language adapter", model.provider()),
            ));
        }
        Ok(LanguageModel {
            registry: Arc::clone(&self.inner),
            registration: Arc::clone(registration),
            descriptor: ModelDescriptor {
                provider: model.provider.clone(),
                model: model.model.clone(),
                capability: Capability::Language,
            },
        })
    }

    /// Resolves an exact image handle without provider I/O.
    pub fn image(&self, model: ModelRef) -> Result<ImageModel, RegistryError> {
        let registration = self.registration(&model)?;
        require_capability(registration.image().is_some(), &model, "image")?;
        Ok(ImageModel::new(
            Arc::clone(&self.inner),
            Arc::clone(registration),
            model,
        ))
    }

    /// Resolves an exact transcription handle without provider I/O.
    pub fn transcription(&self, model: ModelRef) -> Result<TranscriptionModel, RegistryError> {
        let registration = self.registration(&model)?;
        require_capability(
            registration.transcription().is_some(),
            &model,
            "transcription",
        )?;
        Ok(TranscriptionModel::new(
            Arc::clone(&self.inner),
            Arc::clone(registration),
            model,
        ))
    }

    /// Resolves an exact speech handle without provider I/O.
    pub fn speech(&self, model: ModelRef) -> Result<SpeechModel, RegistryError> {
        let registration = self.registration(&model)?;
        require_capability(registration.speech().is_some(), &model, "speech")?;
        Ok(SpeechModel::new(
            Arc::clone(&self.inner),
            Arc::clone(registration),
            model,
        ))
    }

    /// Resolves an exact live Realtime handle without provider I/O.
    pub fn realtime(&self, model: ModelRef) -> Result<RealtimeModel, RegistryError> {
        let registration = self.registration(&model)?;
        require_capability(registration.realtime().is_some(), &model, "realtime")?;
        Ok(RealtimeModel::new(
            Arc::clone(&self.inner),
            Arc::clone(registration),
            model,
        ))
    }

    fn registration(&self, model: &ModelRef) -> Result<&Arc<ProviderRegistration>, RegistryError> {
        self.inner
            .registrations
            .get(model.provider())
            .ok_or_else(|| {
                RegistryError::new(
                    "registry.provider_not_found",
                    format!(
                        "provider deployment `{}` is not registered",
                        model.provider()
                    ),
                )
            })
    }
}

impl fmt::Debug for Registry {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Registry")
            .field(
                "providers",
                &self.inner.registrations.keys().collect::<Vec<_>>(),
            )
            .finish()
    }
}

/// Builder for one immutable Registry.
#[derive(Debug)]
pub struct RegistryBuilder {
    credentials: CredentialManager,
    media: Arc<dyn MediaResolver>,
    registrations: BTreeMap<String, Arc<ProviderRegistration>>,
}

impl RegistryBuilder {
    /// Installs the Start-time media resolver shared by all prepared calls.
    #[must_use]
    pub fn with_media_resolver<R>(mut self, resolver: R) -> Self
    where
        R: MediaResolver + 'static,
    {
        self.media = Arc::new(resolver);
        self
    }

    /// Adds one uniquely named immutable provider deployment.
    pub fn register(mut self, registration: ProviderRegistration) -> Result<Self, RegistryError> {
        let id = registration.deployment_id().to_owned();
        if self.registrations.contains_key(&id) {
            return Err(RegistryError::new(
                "registry.duplicate_provider",
                format!("provider deployment `{id}` is registered more than once"),
            ));
        }
        self.registrations.insert(id, Arc::new(registration));
        Ok(self)
    }

    /// Freezes exact routing and all local dependency handles.
    pub fn build(self) -> Result<Registry, RegistryError> {
        Ok(Registry {
            inner: Arc::new(RegistryInner {
                credentials: self.credentials,
                media: self.media,
                registrations: self.registrations,
                next_call_id: AtomicU64::new(1),
            }),
        })
    }
}

impl RegistryInner {
    async fn resolve_credential(
        &self,
        registration: &ProviderRegistration,
    ) -> Result<Option<rsi_ai_auth::ResolvedCredential>, RegistryError> {
        let Some(requirement) = registration.credential().cloned() else {
            return Ok(None);
        };
        if let Some(resolved) = self.credentials.try_resolve_in_memory(&requirement) {
            return resolved
                .map(Some)
                .map_err(|error| RegistryError::new(error.code(), error.to_string()));
        }
        let credentials = self.credentials.clone();
        tokio::task::spawn_blocking(move || credentials.resolve(&requirement))
            .await
            .map_err(|error| {
                RegistryError::new(
                    "credential.worker_failed",
                    format!("credential worker failed: {error}"),
                )
            })?
            .map(Some)
            .map_err(|error| RegistryError::new(error.code(), error.to_string()))
    }

    async fn prepare_context(
        &self,
        registration: &ProviderRegistration,
        capability: Capability,
        model: &str,
        request_bytes: &[u8],
    ) -> Result<PrepareContext, RegistryError> {
        let credential = self.resolve_credential(registration).await?;
        let call_number = self.next_call_id.fetch_add(1, Ordering::Relaxed);
        let snapshot = PreparedCallSnapshot {
            call_id: format!("{}:{call_number}", registration.deployment_id()),
            deployment_id: registration.deployment_id().to_owned(),
            provider_family: registration.provider_family().to_owned(),
            capability,
            model: model.to_owned(),
            protocol: registration.protocol().to_owned(),
            transport: registration.transport().to_owned(),
            endpoint_fingerprint: registration.endpoint_fingerprint().to_owned(),
            config_generation: registration.config_generation(),
            credential_source: credential
                .as_ref()
                .map(|resolved| resolved.source().clone()),
            retry_policy: registration.retry_policy().clone(),
            request_sha256: sha256_hex(request_bytes),
        };
        Ok(PrepareContext::new(
            snapshot,
            credential,
            Arc::clone(&self.media),
        ))
    }

    async fn restore_context(
        &self,
        registration: &ProviderRegistration,
        snapshot: &PreparedCallSnapshot,
    ) -> Result<PrepareContext, RegistryError> {
        snapshot
            .validate()
            .map_err(|error| RegistryError::new(error.code(), error.to_string()))?;
        if snapshot.deployment_id != registration.deployment_id()
            || snapshot.provider_family != registration.provider_family()
            || snapshot.capability != Capability::Language
            || snapshot.protocol != registration.protocol()
            || snapshot.transport != registration.transport()
            || snapshot.endpoint_fingerprint != registration.endpoint_fingerprint()
            || snapshot.config_generation != registration.config_generation()
            || snapshot.retry_policy != *registration.retry_policy()
        {
            return Err(RegistryError::new(
                "registry.deferred_route_changed",
                "deferred checkpoint does not match the frozen provider route",
            ));
        }
        let credential = self.resolve_credential(registration).await?;
        if credential
            .as_ref()
            .map(rsi_ai_auth::ResolvedCredential::source)
            != snapshot.credential_source.as_ref()
        {
            return Err(RegistryError::new(
                "registry.deferred_credential_changed",
                "deferred checkpoint credential source no longer matches",
            ));
        }
        Ok(PrepareContext::new(
            snapshot.clone(),
            credential,
            Arc::clone(&self.media),
        ))
    }
}

fn require_capability(
    available: bool,
    model: &ModelRef,
    capability: &str,
) -> Result<(), RegistryError> {
    if available {
        Ok(())
    } else {
        Err(RegistryError::new(
            "registry.capability_unavailable",
            format!(
                "provider `{}` has no {capability} adapter",
                model.provider()
            ),
        ))
    }
}

/// Resolved exact language model.
#[derive(Clone)]
pub struct LanguageModel {
    registry: Arc<RegistryInner>,
    registration: Arc<ProviderRegistration>,
    descriptor: ModelDescriptor,
}

impl LanguageModel {
    pub const fn descriptor(&self) -> &ModelDescriptor {
        &self.descriptor
    }

    /// Describes the model through the generation-pinned adapter without provider I/O.
    pub fn describe(&self) -> Result<rsi_ai_protocol::LanguageProfile, RegistryError> {
        self.registration
            .language()
            .expect("language handle proves adapter exists")
            .describe(&self.descriptor.model)
            .map_err(RegistryError::provider)
    }

    /// Validates and freezes one one-shot language call without provider I/O.
    pub async fn prepare(
        &self,
        request: LanguageRequest,
    ) -> Result<PreparedLanguageCall, RegistryError> {
        let request_bytes = request
            .canonical_bytes()
            .map_err(|error| RegistryError::new("request.invalid", error.to_string()))?;
        let context = self
            .registry
            .prepare_context(
                &self.registration,
                Capability::Language,
                &self.descriptor.model,
                &request_bytes,
            )
            .await?;
        let adapter = self
            .registration
            .language()
            .expect("language handle proves adapter exists");
        let prepared = adapter
            .prepare(context, self.descriptor.model.clone(), request)
            .await
            .map_err(RegistryError::provider)?;
        Ok(PreparedLanguageCall { prepared })
    }

    /// Prepares, starts, drains, and validates one language response.
    pub async fn complete(
        &self,
        request: LanguageRequest,
    ) -> Result<LanguageOutput, RegistryError> {
        use futures_util::StreamExt as _;

        let mut generation = self.prepare(request).await?.start().await?;
        while generation.next().await.is_some() {}
        generation.finish().map_err(|error| match error {
            LanguageAssemblyError::Protocol(error) => {
                RegistryError::new(error.code(), error.to_string())
            }
            LanguageAssemblyError::Provider { error, .. } => RegistryError::provider(error),
        })
    }

    /// Prepares an explicitly provider-managed background response.
    pub async fn prepare_deferred(
        &self,
        request: LanguageRequest,
    ) -> Result<PreparedDeferredLanguageCall, RegistryError> {
        let request_bytes = request
            .canonical_bytes()
            .map_err(|error| RegistryError::new("request.invalid", error.to_string()))?;
        let context = self
            .registry
            .prepare_context(
                &self.registration,
                Capability::Language,
                &self.descriptor.model,
                &request_bytes,
            )
            .await?;
        let adapter = self
            .registration
            .language()
            .expect("language handle proves adapter exists");
        let prepared = adapter
            .prepare_deferred(context, self.descriptor.model.clone(), request)
            .await
            .map_err(RegistryError::provider)?;
        Ok(PreparedDeferredLanguageCall { prepared })
    }

    /// Restores a persisted deferred cursor after verifying the exact route.
    pub async fn restore_deferred(
        &self,
        checkpoint: DeferredLanguageCheckpoint,
    ) -> Result<DeferredLanguageHandle, RegistryError> {
        checkpoint
            .validate()
            .map_err(|error| RegistryError::new(error.code(), error.to_string()))?;
        if checkpoint.call().model != self.descriptor.model {
            return Err(RegistryError::new(
                "registry.deferred_model_changed",
                "deferred checkpoint model does not match this handle",
            ));
        }
        let context = self
            .registry
            .restore_context(&self.registration, checkpoint.call())
            .await?;
        let adapter = self
            .registration
            .language()
            .expect("language handle proves adapter exists");
        let operation = adapter
            .restore_deferred(context, checkpoint)
            .await
            .map_err(RegistryError::provider)?;
        Ok(DeferredLanguageHandle { operation })
    }
}

impl fmt::Debug for LanguageModel {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("LanguageModel")
            .field("descriptor", &self.descriptor)
            .finish_non_exhaustive()
    }
}

macro_rules! streaming_capability {
    (
        $model:ident,
        $prepared:ident,
        $generation:ident,
        $request:ty,
        $event:ty,
        $output:ty,
        $assembler:ty,
        $stream:ty,
        $capability:expr,
        $adapter:ident,
        $complete:ident,
        $terminal:pat
    ) => {
        /// Resolved exact capability handle.
        #[derive(Clone)]
        pub struct $model {
            registry: Arc<RegistryInner>,
            registration: Arc<ProviderRegistration>,
            descriptor: ModelDescriptor,
        }

        impl $model {
            fn new(
                registry: Arc<RegistryInner>,
                registration: Arc<ProviderRegistration>,
                model: ModelRef,
            ) -> Self {
                Self {
                    registry,
                    registration,
                    descriptor: ModelDescriptor {
                        provider: model.provider,
                        model: model.model,
                        capability: $capability,
                    },
                }
            }

            pub const fn descriptor(&self) -> &ModelDescriptor {
                &self.descriptor
            }

            /// Validates and freezes one one-shot call without provider I/O.
            pub async fn prepare(&self, request: $request) -> Result<$prepared, RegistryError> {
                request
                    .validate()
                    .map_err(|error| RegistryError::new("request.invalid", error.to_string()))?;
                let request_bytes = serde_json::to_vec(&request)
                    .map_err(|error| RegistryError::new("request.invalid", error.to_string()))?;
                let context = self
                    .registry
                    .prepare_context(
                        &self.registration,
                        $capability,
                        &self.descriptor.model,
                        &request_bytes,
                    )
                    .await?;
                let adapter = self
                    .registration
                    .$adapter()
                    .expect("resolved capability handle proves adapter exists");
                let prepared = adapter
                    .prepare(context, self.descriptor.model.clone(), request)
                    .await
                    .map_err(RegistryError::provider)?;
                Ok($prepared { prepared })
            }

            /// Prepares, starts, drains, and validates one complete result.
            pub async fn $complete(&self, request: $request) -> Result<$output, RegistryError> {
                use futures_util::StreamExt as _;

                let mut generation = self.prepare(request).await?.start().await?;
                while generation.next().await.is_some() {}
                generation.finish()
            }
        }

        impl fmt::Debug for $model {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter
                    .debug_struct(stringify!($model))
                    .field("descriptor", &self.descriptor)
                    .finish_non_exhaustive()
            }
        }

        /// Provider-I/O-free one-shot prepared call.
        #[derive(Debug)]
        pub struct $prepared {
            prepared: Prepared<$stream>,
        }

        impl $prepared {
            /// Returns redacted facts that may be committed before Start.
            pub const fn snapshot(&self) -> &PreparedCallSnapshot {
                self.prepared.snapshot()
            }

            /// Consumes the prepared value and performs one provider attempt.
            pub async fn start(self) -> Result<$generation, RegistryError> {
                let abort = AbortSignal::new();
                let stream = self
                    .prepared
                    .start(abort.clone())
                    .await
                    .map_err(RegistryError::provider)?;
                Ok($generation {
                    stream,
                    assembler: Some(<$assembler>::new()),
                    abort,
                    terminal_emitted: false,
                    error: None,
                })
            }
        }

        /// Pull-based validated stream.
        pub struct $generation {
            stream: $stream,
            assembler: Option<$assembler>,
            abort: AbortSignal,
            terminal_emitted: bool,
            error: Option<RegistryError>,
        }

        impl $generation {
            /// Signals cooperative cancellation to the active provider attempt.
            pub fn abort(&self) {
                self.abort.abort();
            }

            /// Returns assembled output only after a valid terminal event.
            pub fn finish(mut self) -> Result<$output, RegistryError> {
                if let Some(error) = self.error.take() {
                    return Err(error);
                }
                self.assembler
                    .take()
                    .expect("assembler exists until finish")
                    .finish()
                    .map_err(|error| RegistryError::new(error.code(), error.to_string()))
            }
        }

        impl fmt::Debug for $generation {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter
                    .debug_struct(stringify!($generation))
                    .field("terminal_emitted", &self.terminal_emitted)
                    .field("aborted", &self.abort.is_aborted())
                    .finish_non_exhaustive()
            }
        }

        impl Stream for $generation {
            type Item = $event;

            fn poll_next(
                mut self: Pin<&mut Self>,
                context: &mut Context<'_>,
            ) -> Poll<Option<Self::Item>> {
                let this = self.as_mut().get_mut();
                if this.terminal_emitted {
                    return Poll::Ready(None);
                }
                match this.stream.as_mut().poll_next(context) {
                    Poll::Pending => Poll::Pending,
                    Poll::Ready(Some(Ok(event))) => {
                        if let Err(error) = this
                            .assembler
                            .as_mut()
                            .expect("assembler exists while streaming")
                            .push(&event)
                        {
                            this.error = Some(RegistryError::new(error.code(), error.to_string()));
                            this.terminal_emitted = true;
                            return Poll::Ready(None);
                        }
                        this.terminal_emitted = matches!(event, $terminal);
                        Poll::Ready(Some(event))
                    }
                    Poll::Ready(Some(Err(error))) => {
                        this.error = Some(RegistryError::provider(error));
                        this.terminal_emitted = true;
                        Poll::Ready(None)
                    }
                    Poll::Ready(None) => {
                        this.terminal_emitted = true;
                        Poll::Ready(None)
                    }
                }
            }
        }

        impl Drop for $generation {
            fn drop(&mut self) {
                self.abort.abort();
            }
        }
    };
}

streaming_capability!(
    ImageModel,
    PreparedImageCall,
    ImageGeneration,
    ImageRequest,
    ImageEvent,
    ImageOutput,
    ImageAssembler,
    ImageAdapterStream,
    Capability::Image,
    image,
    generate,
    ImageEvent::Finished
);

streaming_capability!(
    TranscriptionModel,
    PreparedTranscriptionCall,
    TranscriptionGeneration,
    TranscriptionRequest,
    TranscriptionEvent,
    TranscriptionOutput,
    TranscriptionAssembler,
    TranscriptionAdapterStream,
    Capability::Transcription,
    transcription,
    transcribe,
    TranscriptionEvent::Finished { .. }
);

streaming_capability!(
    SpeechModel,
    PreparedSpeechCall,
    SpeechGeneration,
    SpeechRequest,
    SpeechEvent,
    SpeechOutput,
    SpeechAssembler,
    SpeechAdapterStream,
    Capability::Speech,
    speech,
    synthesize,
    SpeechEvent::Finished
);

/// Resolved live Realtime model.
#[derive(Clone)]
pub struct RealtimeModel {
    registry: Arc<RegistryInner>,
    registration: Arc<ProviderRegistration>,
    descriptor: ModelDescriptor,
}

impl RealtimeModel {
    fn new(
        registry: Arc<RegistryInner>,
        registration: Arc<ProviderRegistration>,
        model: ModelRef,
    ) -> Self {
        Self {
            registry,
            registration,
            descriptor: ModelDescriptor {
                provider: model.provider,
                model: model.model,
                capability: Capability::Realtime,
            },
        }
    }

    pub const fn descriptor(&self) -> &ModelDescriptor {
        &self.descriptor
    }

    /// Validates and freezes a live session without opening its transport.
    pub async fn prepare(
        &self,
        request: RealtimeRequest,
    ) -> Result<PreparedRealtimeSession, RegistryError> {
        request
            .validate()
            .map_err(|error| RegistryError::new("request.invalid", error.to_string()))?;
        let request_bytes = serde_json::to_vec(&request)
            .map_err(|error| RegistryError::new("request.invalid", error.to_string()))?;
        let context = self
            .registry
            .prepare_context(
                &self.registration,
                Capability::Realtime,
                &self.descriptor.model,
                &request_bytes,
            )
            .await?;
        let adapter = self
            .registration
            .realtime()
            .expect("resolved realtime handle proves adapter exists");
        let prepared = adapter
            .prepare(context, self.descriptor.model.clone(), request)
            .await
            .map_err(RegistryError::provider)?;
        Ok(PreparedRealtimeSession { prepared })
    }

    /// Prepares and starts one live non-replayable session.
    pub async fn connect(
        &self,
        request: RealtimeRequest,
    ) -> Result<RealtimeSession, RegistryError> {
        self.prepare(request).await?.start().await
    }
}

impl fmt::Debug for RealtimeModel {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RealtimeModel")
            .field("descriptor", &self.descriptor)
            .finish_non_exhaustive()
    }
}

/// Provider-I/O-free prepared Realtime session.
#[derive(Debug)]
pub struct PreparedRealtimeSession {
    prepared: Prepared<RealtimeAdapterTransport>,
}

impl PreparedRealtimeSession {
    /// Returns redacted facts that may be committed before opening the transport.
    pub const fn snapshot(&self) -> &PreparedCallSnapshot {
        self.prepared.snapshot()
    }

    /// Consumes the prepared value and opens one live provider transport.
    pub async fn start(self) -> Result<RealtimeSession, RegistryError> {
        let abort = AbortSignal::new();
        let transport = self
            .prepared
            .start(abort.clone())
            .await
            .map_err(RegistryError::provider)?;
        Ok(RealtimeSession {
            transport,
            validator: RealtimeValidator::new(),
            abort,
            closed: false,
        })
    }
}

/// A non-replayable, independently closed live Realtime session.
pub struct RealtimeSession {
    transport: RealtimeAdapterTransport,
    validator: RealtimeValidator,
    abort: AbortSignal,
    closed: bool,
}

impl RealtimeSession {
    /// Validates and sends one live command in session order.
    pub async fn send(&mut self, command: RealtimeCommand) -> Result<(), RegistryError> {
        self.validator
            .push_command(&command)
            .map_err(|error| RegistryError::new(error.code(), error.to_string()))?;
        self.transport
            .send(command)
            .await
            .map_err(RegistryError::provider)
    }

    /// Receives and validates the next event; returns `None` only after `Closed`.
    pub async fn next_event(&mut self) -> Result<Option<RealtimeEvent>, RegistryError> {
        if self.closed {
            return Ok(None);
        }
        let event = self
            .transport
            .next_event()
            .await
            .map_err(RegistryError::provider)?
            .ok_or_else(|| {
                RegistryError::new(
                    "realtime.missing_close",
                    "Realtime transport ended without a Closed event",
                )
            })?;
        self.validator
            .push_event(&event)
            .map_err(|error| RegistryError::new(error.code(), error.to_string()))?;
        self.closed = matches!(event, RealtimeEvent::Closed { .. });
        Ok(Some(event))
    }

    /// Requests orderly closure and closes the underlying transport once.
    pub async fn close(&mut self) -> Result<(), RegistryError> {
        if self.closed {
            return Ok(());
        }
        self.validator
            .push_command(&RealtimeCommand::Close)
            .map_err(|error| RegistryError::new(error.code(), error.to_string()))?;
        self.transport
            .send(RealtimeCommand::Close)
            .await
            .map_err(RegistryError::provider)?;
        self.transport
            .close()
            .await
            .map_err(RegistryError::provider)
    }

    /// Signals immediate cooperative cancellation without waiting for closure.
    pub fn abort(&self) {
        self.abort.abort();
    }
}

impl fmt::Debug for RealtimeSession {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RealtimeSession")
            .field("closed", &self.closed)
            .field("aborted", &self.abort.is_aborted())
            .finish_non_exhaustive()
    }
}

impl Drop for RealtimeSession {
    fn drop(&mut self) {
        self.abort.abort();
    }
}

/// Prepared provider-I/O-free submission of one background language response.
#[derive(Debug)]
pub struct PreparedDeferredLanguageCall {
    prepared: Prepared<DeferredLanguageAdapterHandle>,
}

impl PreparedDeferredLanguageCall {
    /// Returns redacted facts that may be committed before submission.
    pub const fn snapshot(&self) -> &PreparedCallSnapshot {
        self.prepared.snapshot()
    }

    /// Submits exactly one background response request.
    pub async fn submit(self) -> Result<DeferredLanguageHandle, RegistryError> {
        let abort = AbortSignal::new();
        let operation = self
            .prepared
            .start(abort)
            .await
            .map_err(RegistryError::provider)?;
        Ok(DeferredLanguageHandle { operation })
    }
}

/// Explicit controller for a provider-managed background response.
pub struct DeferredLanguageHandle {
    operation: DeferredLanguageAdapterHandle,
}

impl DeferredLanguageHandle {
    /// Returns the latest provider-owned cursor for durable persistence.
    pub fn checkpoint(&self) -> DeferredLanguageCheckpoint {
        self.operation.checkpoint()
    }

    /// Performs one status request; this method never polls in a loop.
    pub async fn poll(&mut self) -> Result<DeferredStatus, RegistryError> {
        self.operation
            .poll(AbortSignal::new())
            .await
            .map_err(RegistryError::provider)
    }

    /// Opens one stream request after the last committed sequence number.
    pub async fn resume(&mut self) -> Result<DeferredLanguageGeneration<'_>, RegistryError> {
        let initial = self.operation.checkpoint();
        initial
            .validate()
            .map_err(|error| RegistryError::new(error.code(), error.to_string()))?;
        let abort = AbortSignal::new();
        let stream = self
            .operation
            .resume(abort.clone())
            .await
            .map_err(RegistryError::provider)?;
        Ok(DeferredLanguageGeneration {
            stream,
            abort,
            checkpoint: initial,
            error: None,
            _handle: PhantomData,
        })
    }

    /// Sends one explicit cancellation request.
    pub async fn cancel(&mut self) -> Result<DeferredStatus, RegistryError> {
        self.operation
            .cancel(AbortSignal::new())
            .await
            .map_err(RegistryError::provider)
    }
}

impl fmt::Debug for DeferredLanguageHandle {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("DeferredLanguageHandle")
            .field("checkpoint", &self.operation.checkpoint())
            .finish()
    }
}

/// Atomic normalized batches from one resumable background stream request.
///
/// A durable caller commits `events()` and `checkpoint()` from each item in one
/// transaction. Clean EOF without a terminal event is resumable, not failure.
pub struct DeferredLanguageGeneration<'handle> {
    stream: DeferredLanguageAdapterStream,
    abort: AbortSignal,
    checkpoint: DeferredLanguageCheckpoint,
    error: Option<RegistryError>,
    _handle: PhantomData<&'handle mut DeferredLanguageHandle>,
}

impl DeferredLanguageGeneration<'_> {
    /// Signals cooperative cancellation of this single resume request.
    pub fn abort(&self) {
        self.abort.abort();
    }

    /// Finishes this transport segment and returns its last validated cursor.
    pub fn finish_segment(mut self) -> Result<DeferredLanguageCheckpoint, RegistryError> {
        if let Some(error) = self.error.take() {
            return Err(error);
        }
        Ok(self.checkpoint.clone())
    }
}

impl fmt::Debug for DeferredLanguageGeneration<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("DeferredLanguageGeneration")
            .field("checkpoint", &self.checkpoint)
            .field("aborted", &self.abort.is_aborted())
            .finish_non_exhaustive()
    }
}

impl Stream for DeferredLanguageGeneration<'_> {
    type Item = DeferredLanguageChunk;

    fn poll_next(mut self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = self.as_mut().get_mut();
        if this.error.is_some() {
            return Poll::Ready(None);
        }
        match this.stream.as_mut().poll_next(context) {
            Poll::Pending => Poll::Pending,
            Poll::Ready(Some(Ok(batch))) => {
                if let Err(error) = validate_deferred_advance(&this.checkpoint, batch.checkpoint())
                {
                    this.error = Some(error);
                    return Poll::Ready(None);
                }
                this.checkpoint = batch.checkpoint().clone();
                Poll::Ready(Some(batch))
            }
            Poll::Ready(Some(Err(error))) => {
                this.error = Some(RegistryError::provider(error));
                Poll::Ready(None)
            }
            Poll::Ready(None) => Poll::Ready(None),
        }
    }
}

impl Drop for DeferredLanguageGeneration<'_> {
    fn drop(&mut self) {
        self.abort.abort();
    }
}

fn validate_deferred_advance(
    current: &DeferredLanguageCheckpoint,
    next: &DeferredLanguageCheckpoint,
) -> Result<(), RegistryError> {
    if next.call() != current.call() || next.operation_id() != current.operation_id() {
        return Err(RegistryError::new(
            "provider.invalid_deferred_checkpoint",
            "deferred adapter changed its operation identity or frozen route",
        ));
    }
    let sequence = next.sequence_number().ok_or_else(|| {
        RegistryError::new(
            "provider.invalid_deferred_checkpoint",
            "deferred stream batch has no sequence number",
        )
    })?;
    let mut expected = current.clone();
    expected
        .advance(
            next.status(),
            next.stream_created(),
            sequence,
            next.provider_state().cloned(),
        )
        .map_err(|error| RegistryError::new(error.code(), error.to_string()))?;
    if expected != *next {
        return Err(RegistryError::new(
            "provider.invalid_deferred_checkpoint",
            "deferred adapter checkpoint skipped validated state",
        ));
    }
    Ok(())
}

/// One-shot prepared language call.
#[derive(Debug)]
pub struct PreparedLanguageCall {
    prepared: Prepared<LanguageAdapterStream>,
}

impl PreparedLanguageCall {
    /// Returns redacted facts that may be committed before Start.
    pub const fn snapshot(&self) -> &PreparedCallSnapshot {
        self.prepared.snapshot()
    }

    /// Consumes the prepared value and starts one provider stream.
    pub async fn start(self) -> Result<LanguageGeneration, RegistryError> {
        let abort = AbortSignal::new();
        let stream = self
            .prepared
            .start(abort.clone())
            .await
            .map_err(RegistryError::provider)?;
        Ok(LanguageGeneration {
            stream,
            assembler: Some(LanguageAssembler::new()),
            abort,
            terminal_emitted: false,
            protocol_error: None,
        })
    }
}

/// Pull-based validated language generation.
pub struct LanguageGeneration {
    stream: LanguageAdapterStream,
    assembler: Option<LanguageAssembler>,
    abort: AbortSignal,
    terminal_emitted: bool,
    protocol_error: Option<StreamError>,
}

impl LanguageGeneration {
    /// Signals cooperative cancellation to the active provider attempt.
    pub fn abort(&self) {
        self.abort.abort();
    }

    /// Returns complete output, or a terminal error carrying diagnostic partial
    /// output that must not be treated as a successful response.
    pub fn finish(mut self) -> Result<LanguageOutput, LanguageAssemblyError> {
        if let Some(error) = self.protocol_error.take() {
            return Err(LanguageAssemblyError::Protocol(error));
        }
        self.assembler
            .take()
            .expect("assembler exists until finish")
            .finish()
    }
}

impl fmt::Debug for LanguageGeneration {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("LanguageGeneration")
            .field("terminal_emitted", &self.terminal_emitted)
            .field("aborted", &self.abort.is_aborted())
            .finish_non_exhaustive()
    }
}

impl Stream for LanguageGeneration {
    type Item = LanguageEvent;

    fn poll_next(mut self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = self.as_mut().get_mut();
        if this.terminal_emitted {
            return Poll::Ready(None);
        }
        match this.stream.as_mut().poll_next(context) {
            Poll::Pending => Poll::Pending,
            Poll::Ready(Some(Ok(event))) => {
                if let Err(error) = this
                    .assembler
                    .as_mut()
                    .expect("assembler exists while streaming")
                    .push(&event)
                {
                    this.protocol_error = Some(error);
                    this.terminal_emitted = true;
                    return Poll::Ready(Some(protocol_failure_event(
                        "adapter emitted an invalid language event",
                    )));
                }
                this.terminal_emitted = is_language_terminal(&event);
                Poll::Ready(Some(event))
            }
            Poll::Ready(Some(Err(error))) => {
                let event = LanguageEvent::Failed {
                    error,
                    replay: None,
                };
                if let Err(protocol_error) = this
                    .assembler
                    .as_mut()
                    .expect("assembler exists while streaming")
                    .push(&event)
                {
                    this.protocol_error = Some(protocol_error);
                }
                this.terminal_emitted = true;
                Poll::Ready(Some(event))
            }
            Poll::Ready(None) => {
                let event = protocol_failure_event(
                    "adapter stream ended without a terminal language event",
                );
                if let Err(protocol_error) = this
                    .assembler
                    .as_mut()
                    .expect("assembler exists while streaming")
                    .push(&event)
                {
                    this.protocol_error = Some(protocol_error);
                }
                this.terminal_emitted = true;
                Poll::Ready(Some(event))
            }
        }
    }
}

impl Drop for LanguageGeneration {
    fn drop(&mut self) {
        self.abort.abort();
    }
}

fn is_language_terminal(event: &LanguageEvent) -> bool {
    matches!(
        event,
        LanguageEvent::Finished { .. } | LanguageEvent::Failed { .. }
    )
}

fn protocol_failure_event(summary: &str) -> LanguageEvent {
    LanguageEvent::Failed {
        error: AiError::new(
            ErrorKind::Protocol,
            ErrorPhase::Stream,
            DispatchStatus::Unknown,
            summary,
        )
        .expect("static protocol summary is bounded"),
        replay: None,
    }
}

fn sha256_hex(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

/// Registry or prepared-call failure.
#[derive(Clone, Debug, Eq, Error, PartialEq)]
#[error("{message}")]
pub struct RegistryError {
    code: &'static str,
    message: String,
    provider_error: Option<Box<AiError>>,
}

impl RegistryError {
    fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            provider_error: None,
        }
    }

    #[allow(clippy::needless_pass_by_value)] // Suitable for direct Result::map_err use.
    fn provider(error: AiError) -> Self {
        let code = error.kind().code();
        Self {
            code,
            message: error.safe_summary().to_owned(),
            provider_error: Some(Box::new(error)),
        }
    }

    /// Returns the stable registry or nested provider failure code.
    pub const fn code(&self) -> &'static str {
        self.code
    }

    /// Original structured provider facts, when this failure crossed a provider seam.
    pub fn provider_error(&self) -> Option<&AiError> {
        self.provider_error.as_deref()
    }
}

fn validate_id(field: &str, value: &str) -> Result<(), RegistryError> {
    rsi_ai_protocol::validate_identifier(field, value)
        .map_err(|message| RegistryError::new("registry.invalid_model_ref", message))
}
