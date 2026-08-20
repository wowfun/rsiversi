//! Deterministic adapters for exercising the public `rsi-ai` seams.

#![deny(unsafe_code)]
#![allow(clippy::missing_panics_doc)] // Poisoned fixture locks indicate a test harness panic.

use std::{
    collections::BTreeMap,
    collections::VecDeque,
    fmt,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
};

use async_trait::async_trait;
use futures_util::stream;
use rsi_ai_protocol::{
    AiError, DispatchStatus, ErrorKind, ErrorPhase, ImageEvent, ImageRequest, LanguageEvent,
    LanguageRequest, MediaDescriptor, RealtimeCommand, RealtimeEvent, RealtimeRequest, SpeechEvent,
    SpeechRequest, TranscriptionEvent, TranscriptionRequest,
};
use rsi_ai_provider::{
    AbortSignal, AdapterFuture, ImageAdapter, ImageAdapterStream, LanguageAdapter,
    LanguageAdapterStream, MediaResolver, PrepareContext, Prepared, RealtimeAdapter,
    RealtimeAdapterTransport, RealtimeConnection, SpeechAdapter, SpeechAdapterStream,
    TranscriptionAdapter, TranscriptionAdapterStream,
};

/// In-memory content-addressed media source for deterministic adapter tests.
#[derive(Clone, Debug, Default)]
pub struct InMemoryMediaResolver {
    bodies: Arc<BTreeMap<String, Vec<u8>>>,
}

impl InMemoryMediaResolver {
    #[must_use]
    pub fn new(bodies: BTreeMap<String, Vec<u8>>) -> Self {
        Self {
            bodies: Arc::new(bodies),
        }
    }
}

impl MediaResolver for InMemoryMediaResolver {
    fn read(
        &self,
        descriptor: MediaDescriptor,
        _abort: AbortSignal,
    ) -> AdapterFuture<Result<Vec<u8>, AiError>> {
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

/// Repeatable language script whose counters change only at public adapter phases.
#[derive(Clone)]
pub struct ScriptedLanguageAdapter {
    inner: Arc<ScriptedLanguageInner>,
}

macro_rules! scripted_stream_adapter {
    (
        $adapter:ident,
        $inner:ident,
        $event:ty,
        $request:ty,
        $trait:ident,
        $stream:ident
    ) => {
        #[derive(Clone)]
        pub struct $adapter {
            inner: Arc<$inner>,
        }

        struct $inner {
            events: Vec<$event>,
            prepare_count: AtomicUsize,
            start_count: AtomicUsize,
        }

        impl $adapter {
            #[must_use]
            pub fn new(events: Vec<$event>) -> Self {
                Self {
                    inner: Arc::new($inner {
                        events,
                        prepare_count: AtomicUsize::new(0),
                        start_count: AtomicUsize::new(0),
                    }),
                }
            }

            pub fn prepare_count(&self) -> usize {
                self.inner.prepare_count.load(Ordering::SeqCst)
            }

            pub fn start_count(&self) -> usize {
                self.inner.start_count.load(Ordering::SeqCst)
            }
        }

        impl fmt::Debug for $adapter {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter
                    .debug_struct(stringify!($adapter))
                    .field("events", &self.inner.events.len())
                    .field("prepare_count", &self.prepare_count())
                    .field("start_count", &self.start_count())
                    .finish()
            }
        }

        impl $trait for $adapter {
            fn prepare(
                &self,
                context: PrepareContext,
                _model: String,
                _request: $request,
            ) -> AdapterFuture<Result<Prepared<$stream>, AiError>> {
                self.inner.prepare_count.fetch_add(1, Ordering::SeqCst);
                let snapshot = context.snapshot().clone();
                let inner = Arc::clone(&self.inner);
                Box::pin(async move {
                    Ok(Prepared::new(snapshot, move |_abort| {
                        let events = inner.events.clone();
                        inner.start_count.fetch_add(1, Ordering::SeqCst);
                        Box::pin(async move {
                            let stream: $stream =
                                Box::pin(stream::iter(events.into_iter().map(Ok)));
                            Ok(stream)
                        })
                    }))
                })
            }
        }
    };
}

scripted_stream_adapter!(
    ScriptedImageAdapter,
    ScriptedImageInner,
    ImageEvent,
    ImageRequest,
    ImageAdapter,
    ImageAdapterStream
);
scripted_stream_adapter!(
    ScriptedTranscriptionAdapter,
    ScriptedTranscriptionInner,
    TranscriptionEvent,
    TranscriptionRequest,
    TranscriptionAdapter,
    TranscriptionAdapterStream
);
scripted_stream_adapter!(
    ScriptedSpeechAdapter,
    ScriptedSpeechInner,
    SpeechEvent,
    SpeechRequest,
    SpeechAdapter,
    SpeechAdapterStream
);

/// Repeatable live-session script with an observable command sink.
#[derive(Clone)]
pub struct ScriptedRealtimeAdapter {
    inner: Arc<ScriptedRealtimeInner>,
}

#[derive(Debug)]
struct ScriptedRealtimeInner {
    events: Vec<RealtimeEvent>,
    wait_for_request: bool,
    commands: Mutex<Vec<RealtimeCommand>>,
    prepare_count: AtomicUsize,
    start_count: AtomicUsize,
}

impl ScriptedRealtimeAdapter {
    #[must_use]
    pub fn new(events: Vec<RealtimeEvent>) -> Self {
        Self {
            inner: Arc::new(ScriptedRealtimeInner {
                events,
                wait_for_request: false,
                commands: Mutex::new(Vec::new()),
                prepare_count: AtomicUsize::new(0),
                start_count: AtomicUsize::new(0),
            }),
        }
    }

    /// Creates a script whose first event is immediately available and whose
    /// remaining events wait until the caller sends `RequestResponse`.
    #[must_use]
    pub fn new_after_request(events: Vec<RealtimeEvent>) -> Self {
        Self {
            inner: Arc::new(ScriptedRealtimeInner {
                events,
                wait_for_request: true,
                commands: Mutex::new(Vec::new()),
                prepare_count: AtomicUsize::new(0),
                start_count: AtomicUsize::new(0),
            }),
        }
    }

    pub fn commands(&self) -> Vec<RealtimeCommand> {
        self.inner
            .commands
            .lock()
            .expect("scripted realtime command lock is not poisoned")
            .clone()
    }

    pub fn prepare_count(&self) -> usize {
        self.inner.prepare_count.load(Ordering::SeqCst)
    }

    pub fn start_count(&self) -> usize {
        self.inner.start_count.load(Ordering::SeqCst)
    }
}

impl fmt::Debug for ScriptedRealtimeAdapter {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ScriptedRealtimeAdapter")
            .field("events", &self.inner.events.len())
            .field("prepare_count", &self.prepare_count())
            .field("start_count", &self.start_count())
            .finish()
    }
}

impl RealtimeAdapter for ScriptedRealtimeAdapter {
    fn prepare(
        &self,
        context: PrepareContext,
        _model: String,
        _request: RealtimeRequest,
    ) -> AdapterFuture<Result<Prepared<RealtimeAdapterTransport>, AiError>> {
        self.inner.prepare_count.fetch_add(1, Ordering::SeqCst);
        let snapshot = context.snapshot().clone();
        let inner = Arc::clone(&self.inner);
        Box::pin(async move {
            Ok(Prepared::new(snapshot, move |_abort| {
                inner.start_count.fetch_add(1, Ordering::SeqCst);
                let transport: RealtimeAdapterTransport = Box::new(ScriptedRealtimeConnection {
                    events: inner.events.clone().into(),
                    wait_for_request: inner.wait_for_request,
                    first_event_emitted: false,
                    response_requested: false,
                    inner,
                });
                Box::pin(async move { Ok(transport) })
            }))
        })
    }
}

#[derive(Debug)]
struct ScriptedRealtimeConnection {
    events: VecDeque<RealtimeEvent>,
    wait_for_request: bool,
    first_event_emitted: bool,
    response_requested: bool,
    inner: Arc<ScriptedRealtimeInner>,
}

#[async_trait]
impl RealtimeConnection for ScriptedRealtimeConnection {
    async fn send(&mut self, command: RealtimeCommand) -> Result<(), AiError> {
        if matches!(command, RealtimeCommand::RequestResponse) {
            self.response_requested = true;
        }
        self.inner
            .commands
            .lock()
            .expect("scripted realtime command lock is not poisoned")
            .push(command);
        Ok(())
    }

    async fn next_event(&mut self) -> Result<Option<RealtimeEvent>, AiError> {
        if !self.first_event_emitted {
            self.first_event_emitted = true;
            return Ok(self.events.pop_front());
        }
        if self.wait_for_request && !self.response_requested {
            std::future::pending::<()>().await;
        }
        Ok(self.events.pop_front())
    }

    async fn close(&mut self) -> Result<(), AiError> {
        Ok(())
    }
}

struct ScriptedLanguageInner {
    events: Vec<LanguageEvent>,
    prepare_count: AtomicUsize,
    start_count: AtomicUsize,
}

impl ScriptedLanguageAdapter {
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

    pub fn prepare_count(&self) -> usize {
        self.inner.prepare_count.load(Ordering::SeqCst)
    }

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
    fn prepare(
        &self,
        context: PrepareContext,
        _model: String,
        _request: LanguageRequest,
    ) -> AdapterFuture<Result<Prepared<LanguageAdapterStream>, rsi_ai_protocol::AiError>> {
        self.inner.prepare_count.fetch_add(1, Ordering::SeqCst);
        let snapshot = context.snapshot().clone();
        let inner = Arc::clone(&self.inner);
        Box::pin(async move {
            Ok(Prepared::new(snapshot, move |_abort| {
                let events = inner.events.clone();
                inner.start_count.fetch_add(1, Ordering::SeqCst);
                Box::pin(async move {
                    let stream: LanguageAdapterStream =
                        Box::pin(stream::iter(events.into_iter().map(Ok)));
                    Ok(stream)
                })
            }))
        })
    }
}

type LanguageHandler =
    dyn Fn(LanguageRequest) -> Result<Vec<LanguageEvent>, AiError> + Send + Sync + 'static;

/// Request-aware deterministic language adapter for black-box fixtures.
#[derive(Clone)]
pub struct FunctionalLanguageAdapter {
    handler: Arc<LanguageHandler>,
    prepare_count: Arc<AtomicUsize>,
    start_count: Arc<AtomicUsize>,
}

impl FunctionalLanguageAdapter {
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

    pub fn prepare_count(&self) -> usize {
        self.prepare_count.load(Ordering::SeqCst)
    }

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
    fn prepare(
        &self,
        context: PrepareContext,
        _model: String,
        request: LanguageRequest,
    ) -> AdapterFuture<Result<Prepared<LanguageAdapterStream>, AiError>> {
        self.prepare_count.fetch_add(1, Ordering::SeqCst);
        let events = (self.handler)(request);
        let snapshot = context.snapshot().clone();
        let start_count = Arc::clone(&self.start_count);
        Box::pin(async move {
            let events = events?;
            Ok(Prepared::new(snapshot, move |_abort| {
                start_count.fetch_add(1, Ordering::SeqCst);
                Box::pin(async move {
                    let stream: LanguageAdapterStream =
                        Box::pin(stream::iter(events.into_iter().map(Ok)));
                    Ok(stream)
                })
            }))
        })
    }
}
