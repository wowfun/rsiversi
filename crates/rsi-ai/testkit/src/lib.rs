//! Deterministic provider adapters for keyless tests.

#![deny(unsafe_code)]
#![warn(missing_docs)]
#![allow(clippy::missing_errors_doc)]

use futures_util::{StreamExt as _, stream};
use rsi_ai_protocol::{
    AiCapability, AiError, DispatchStatus, ErrorKind, ErrorPhase, ImageEvent, ImageRequest,
    LanguageAssembler, LanguageAssemblyError, LanguageEvent, LanguageOutput, LanguageRequest,
    PreparedCallSnapshot, RetryPolicy,
};
use rsi_ai_provider::{
    AbortSignal, AdapterFuture, ImageAdapter, ImageAdapterStream, LanguageAdapter,
    LanguageAdapterStream, MediaResolver, PrepareContext, Prepared,
};
use rsi_credentials_protocol::ResolvedCredential;
use rsi_media_protocol::MediaDescriptor;
use std::collections::BTreeMap;
use std::fmt;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

/// In-memory content-addressed media source for deterministic adapter tests.
#[derive(Clone, Debug, Default)]
pub struct InMemoryMediaResolver {
    bodies: Arc<BTreeMap<String, Arc<[u8]>>>,
}

impl InMemoryMediaResolver {
    /// Creates a resolver keyed by each descriptor's lowercase SHA-256 digest.
    #[must_use]
    pub fn new(bodies: BTreeMap<String, Vec<u8>>) -> Self {
        Self {
            bodies: Arc::new(
                bodies
                    .into_iter()
                    .map(|(digest, bytes)| (digest, Arc::from(bytes)))
                    .collect(),
            ),
        }
    }
}

impl MediaResolver for InMemoryMediaResolver {
    fn read(
        &self,
        descriptor: MediaDescriptor,
        _abort: AbortSignal,
    ) -> AdapterFuture<Result<Arc<[u8]>, AiError>> {
        let body = self.bodies.get(descriptor.sha256()).cloned();
        Box::pin(async move {
            body.ok_or_else(|| {
                AiError::new(
                    ErrorKind::Artifact,
                    ErrorPhase::Send,
                    DispatchStatus::NotDispatched,
                    "media body is absent from the scripted resolver",
                )
                .expect("static scripted resolver error is valid")
            })
        })
    }
}

/// Creates bounded provider-author context facts for one deterministic Language adapter call.
///
/// # Panics
///
/// Panics when a supplied identity cannot form a valid prepared-call snapshot.
pub fn language_context(
    deployment: impl Into<String>,
    provider_family: impl Into<String>,
    model: impl Into<String>,
    credential: Option<ResolvedCredential>,
    media: Arc<dyn MediaResolver>,
    media_admission_bytes: u64,
) -> PrepareContext {
    PrepareContext::new(
        PreparedCallSnapshot {
            call_id: "test-call".into(),
            deployment_id: deployment.into(),
            provider_family: provider_family.into(),
            capability: AiCapability::Language,
            model: model.into(),
            protocol: "test-protocol".into(),
            transport: "test-transport".into(),
            endpoint_fingerprint: "test-endpoint".into(),
            config_generation: 1,
            credential_source: credential.as_ref().map(|value| value.source.clone()),
            retry_policy: RetryPolicy::default(),
            request_sha256: "0".repeat(64),
        },
        credential,
        media,
        media_admission_bytes,
    )
    .expect("static scripted provider context is valid")
}

/// Failure observed while driving one adapter through normalized assembly.
#[derive(Debug)]
pub enum LanguageRunError {
    /// Provider preparation, start, or transport failure.
    Provider(AiError),
    /// Normalized stream grammar or semantic provider failure.
    Assembly(LanguageAssemblyError),
}

impl LanguageRunError {
    /// Returns structured provider facts when the failure came from the adapter.
    pub fn provider_error(&self) -> Option<&AiError> {
        match self {
            Self::Provider(error)
            | Self::Assembly(LanguageAssemblyError::Provider { error, .. }) => Some(error),
            Self::Assembly(LanguageAssemblyError::Protocol(_)) => None,
        }
    }
}

/// Drives one provider adapter through prepare, start, stream, and assembly.
pub async fn complete_language(
    adapter: &dyn LanguageAdapter,
    context: PrepareContext,
    model: impl Into<String>,
    request: LanguageRequest,
) -> Result<LanguageOutput, LanguageRunError> {
    let prepared = adapter
        .prepare(context, model.into(), request)
        .await
        .map_err(LanguageRunError::Provider)?;
    let mut stream = prepared
        .start(AbortSignal::new())
        .await
        .map_err(LanguageRunError::Provider)?;
    let mut assembler = LanguageAssembler::new();
    while let Some(event) = stream.next().await {
        let event = event.map_err(LanguageRunError::Provider)?;
        assembler
            .push(&event)
            .map_err(|error| LanguageRunError::Assembly(error.into()))?;
    }
    assembler.finish().map_err(LanguageRunError::Assembly)
}

struct ScriptedLanguageInner {
    events: Vec<LanguageEvent>,
    prepare_count: AtomicUsize,
    start_count: AtomicUsize,
}

/// Repeatable Language adapter whose counters change only at public phases.
#[derive(Clone)]
pub struct ScriptedLanguageAdapter {
    inner: Arc<ScriptedLanguageInner>,
}

impl ScriptedLanguageAdapter {
    /// Creates an adapter that replays the supplied events on every start.
    #[must_use]
    pub fn new(events: Vec<LanguageEvent>) -> Self {
        Self {
            inner: Arc::new(ScriptedLanguageInner {
                events,
                prepare_count: AtomicUsize::new(0),
                start_count: AtomicUsize::new(0),
            }),
        }
    }

    /// Returns completed prepare calls.
    pub fn prepare_count(&self) -> usize {
        self.inner.prepare_count.load(Ordering::SeqCst)
    }

    /// Returns started provider attempts.
    pub fn start_count(&self) -> usize {
        self.inner.start_count.load(Ordering::SeqCst)
    }
}

impl fmt::Debug for ScriptedLanguageAdapter {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ScriptedLanguageAdapter")
            .field("events", &self.inner.events.len())
            .field("prepare_count", &self.prepare_count())
            .field("start_count", &self.start_count())
            .finish()
    }
}

impl LanguageAdapter for ScriptedLanguageAdapter {
    fn describe(&self, _model: &str) -> Result<rsi_ai_protocol::LanguageProfile, AiError> {
        Ok(test_language_profile())
    }

    fn validate_request(&self, _model: &str, _request: &LanguageRequest) -> Result<(), AiError> {
        Ok(())
    }

    fn prepare(
        &self,
        context: PrepareContext,
        _model: String,
        _request: LanguageRequest,
    ) -> AdapterFuture<Result<Prepared<LanguageAdapterStream>, AiError>> {
        self.inner.prepare_count.fetch_add(1, Ordering::SeqCst);
        let snapshot = context.snapshot().clone();
        let inner = Arc::clone(&self.inner);
        Box::pin(async move {
            Ok(Prepared::new(snapshot, move |_abort| {
                let events = inner.events.clone();
                inner.start_count.fetch_add(1, Ordering::SeqCst);
                Box::pin(async move {
                    Ok(Box::pin(stream::iter(events.into_iter().map(Ok))) as LanguageAdapterStream)
                })
            }))
        })
    }
}

type LanguageHandler =
    dyn Fn(LanguageRequest) -> Result<Vec<LanguageEvent>, AiError> + Send + Sync + 'static;

/// Request-aware deterministic Language adapter.
#[derive(Clone)]
pub struct FunctionalLanguageAdapter {
    handler: Arc<LanguageHandler>,
    prepare_count: Arc<AtomicUsize>,
    start_count: Arc<AtomicUsize>,
}

impl FunctionalLanguageAdapter {
    /// Creates an adapter whose handler derives a deterministic script per request.
    pub fn new<F>(handler: F) -> Self
    where
        F: Fn(LanguageRequest) -> Result<Vec<LanguageEvent>, AiError> + Send + Sync + 'static,
    {
        Self {
            handler: Arc::new(handler),
            prepare_count: Arc::new(AtomicUsize::new(0)),
            start_count: Arc::new(AtomicUsize::new(0)),
        }
    }

    /// Returns completed prepare calls.
    pub fn prepare_count(&self) -> usize {
        self.prepare_count.load(Ordering::SeqCst)
    }

    /// Returns started provider attempts.
    pub fn start_count(&self) -> usize {
        self.start_count.load(Ordering::SeqCst)
    }
}

impl fmt::Debug for FunctionalLanguageAdapter {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("FunctionalLanguageAdapter")
            .field("prepare_count", &self.prepare_count())
            .field("start_count", &self.start_count())
            .finish_non_exhaustive()
    }
}

impl LanguageAdapter for FunctionalLanguageAdapter {
    fn describe(&self, _model: &str) -> Result<rsi_ai_protocol::LanguageProfile, AiError> {
        Ok(test_language_profile())
    }

    fn validate_request(&self, _model: &str, _request: &LanguageRequest) -> Result<(), AiError> {
        Ok(())
    }

    fn prepare(
        &self,
        context: PrepareContext,
        _model: String,
        request: LanguageRequest,
    ) -> AdapterFuture<Result<Prepared<LanguageAdapterStream>, AiError>> {
        self.prepare_count.fetch_add(1, Ordering::SeqCst);
        let events = (self.handler)(request);
        let snapshot = context.snapshot().clone();
        let starts = Arc::clone(&self.start_count);
        Box::pin(async move {
            let events = events?;
            Ok(Prepared::new(snapshot, move |_abort| {
                starts.fetch_add(1, Ordering::SeqCst);
                Box::pin(async move {
                    Ok(Box::pin(stream::iter(events.into_iter().map(Ok))) as LanguageAdapterStream)
                })
            }))
        })
    }
}

struct ScriptedImageInner {
    events: Vec<ImageEvent>,
    prepare_count: AtomicUsize,
    start_count: AtomicUsize,
}

/// Repeatable Image adapter for keyless router tests.
#[derive(Clone)]
pub struct ScriptedImageAdapter {
    inner: Arc<ScriptedImageInner>,
}

impl ScriptedImageAdapter {
    /// Creates an adapter that replays the supplied events on every start.
    #[must_use]
    pub fn new(events: Vec<ImageEvent>) -> Self {
        Self {
            inner: Arc::new(ScriptedImageInner {
                events,
                prepare_count: AtomicUsize::new(0),
                start_count: AtomicUsize::new(0),
            }),
        }
    }
}

impl fmt::Debug for ScriptedImageAdapter {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ScriptedImageAdapter")
            .field("events", &self.inner.events.len())
            .finish_non_exhaustive()
    }
}

impl ImageAdapter for ScriptedImageAdapter {
    fn validate_request(&self, _model: &str, _request: &ImageRequest) -> Result<(), AiError> {
        Ok(())
    }

    fn prepare(
        &self,
        context: PrepareContext,
        _model: String,
        _request: ImageRequest,
    ) -> AdapterFuture<Result<Prepared<ImageAdapterStream>, AiError>> {
        self.inner.prepare_count.fetch_add(1, Ordering::SeqCst);
        let snapshot = context.snapshot().clone();
        let inner = Arc::clone(&self.inner);
        Box::pin(async move {
            Ok(Prepared::new(snapshot, move |_abort| {
                let events = inner.events.clone();
                inner.start_count.fetch_add(1, Ordering::SeqCst);
                Box::pin(async move {
                    Ok(Box::pin(stream::iter(events.into_iter().map(Ok))) as ImageAdapterStream)
                })
            }))
        })
    }
}

fn test_language_profile() -> rsi_ai_protocol::LanguageProfile {
    rsi_ai_protocol::LanguageProfile::new(
        128_000,
        4_096,
        32_768,
        rsi_ai_protocol::ToolDialect::Responses,
        true,
        rsi_ai_protocol::ImageToolResultCapability::Yes(
            rsi_ai_protocol::ImageToolResultMode::FunctionOutput,
        ),
        Vec::new(),
    )
    .expect("static scripted Language profile is valid")
}
