use std::{
    collections::{BTreeMap, BTreeSet},
    fmt,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
};

use futures_util::StreamExt as _;
use rsi_ai::{
    ModelRef, PreparedImageCall, PreparedLanguageCall, PreparedRealtimeSession, PreparedSpeechCall,
    PreparedTranscriptionCall, Registry, RegistryError,
};
use rsi_ai_auth::{CredentialManager, CredentialRequirement};
use rsi_ai_protocol::{
    AiError, BlobAssembler, DispatchStatus, ErrorKind, ErrorPhase, ImageEvent, MediaDescriptor,
    MediaKind, RealtimeCommand, RealtimeEvent, SpeechEvent, WireFrame, decode_wire_frame,
    encode_wire_frame, sanitize_error_summary,
};
use rsi_ai_provider::{AbortSignal, AdapterFuture, MediaResolver, ProviderRegistration};
use rsi_meta_plugin::{
    EVENT_CANCEL, EVENT_CREDIT, EVENT_END, Frame, FrameBody, Lane, LifecyclePhase, OP_CANCEL,
    OP_CREDIT, OP_HALF_CLOSE, OP_OPEN, PostFrameOutcome, RUNTIME_TICK_EVENT, RUNTIME_TICK_SERVICE,
    STREAM_BYTE_BUDGET,
    sdk::{Host, Plugin},
};
use serde_json::{Value, json};
use sha2::{Digest as _, Sha256};
use tokio::runtime::Runtime;
use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;

use crate::{
    AiService, ClientControl, ServerControl, decode_client_control, encode_server_control,
};

const MAX_MEDIA_BLOBS: usize = 4_096;
const MAX_MEDIA_BYTES: u64 = 512 * 1024 * 1024;

/// Content-addressed live-call media supplied over rsi-meta blob frames.
#[derive(Clone, Debug, Default)]
pub struct PluginMediaResolver {
    inner: Arc<Mutex<MediaStore>>,
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
struct MediaOwner {
    stream_id: String,
    call_id: String,
}

impl MediaOwner {
    fn new(stream_id: impl Into<String>, call_id: impl Into<String>) -> Self {
        Self {
            stream_id: stream_id.into(),
            call_id: call_id.into(),
        }
    }
}

#[derive(Debug, Default)]
struct MediaStore {
    bodies: BTreeMap<String, MediaBody>,
    bytes: u64,
}

struct MediaBody {
    bytes: Arc<[u8]>,
    owners: BTreeSet<MediaOwner>,
}

impl fmt::Debug for MediaBody {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("MediaBody")
            .field("byte_len", &self.bytes.len())
            .field("owners", &self.owners)
            .finish()
    }
}

impl PluginMediaResolver {
    fn usage(&self) -> Result<(usize, u64), PluginError> {
        let store = self
            .inner
            .lock()
            .map_err(|_| PluginError::new("media store poisoned"))?;
        Ok((store.bodies.len(), store.bytes))
    }

    fn insert(
        &self,
        owner: &MediaOwner,
        descriptor: &MediaDescriptor,
        bytes: Vec<u8>,
    ) -> Result<(), PluginError> {
        let mut store = self
            .inner
            .lock()
            .map_err(|_| PluginError::new("media store poisoned"))?;
        if let Some(existing) = store.bodies.get(descriptor.sha256()) {
            if existing.bytes.as_ref() != bytes.as_slice() {
                return Err(PluginError::new("media digest collision"));
            }
            store
                .bodies
                .get_mut(descriptor.sha256())
                .expect("entry checked above")
                .owners
                .insert(owner.clone());
            return Ok(());
        }
        let length =
            u64::try_from(bytes.len()).map_err(|_| PluginError::new("media length overflow"))?;
        if store.bodies.len() >= MAX_MEDIA_BLOBS
            || store
                .bytes
                .checked_add(length)
                .is_none_or(|total| total > MAX_MEDIA_BYTES)
        {
            return Err(PluginError::new("generation media quota exceeded"));
        }
        store.bytes += length;
        store.bodies.insert(
            descriptor.sha256().to_owned(),
            MediaBody {
                bytes: Arc::from(bytes),
                owners: BTreeSet::from([owner.clone()]),
            },
        );
        Ok(())
    }

    fn release_call(&self, owner: &MediaOwner) -> Result<(), PluginError> {
        let mut store = self
            .inner
            .lock()
            .map_err(|_| PluginError::new("media store poisoned"))?;
        let released = store
            .bodies
            .extract_if(.., |_, body| {
                body.owners.remove(owner);
                body.owners.is_empty()
            })
            .map(|(_, body)| u64::try_from(body.bytes.len()).unwrap_or(u64::MAX))
            .try_fold(0_u64, u64::checked_add)
            .ok_or_else(|| PluginError::new("media length overflow"))?;
        store.bytes = store
            .bytes
            .checked_sub(released)
            .ok_or_else(|| PluginError::new("media accounting underflow"))?;
        Ok(())
    }

    fn release_stream(&self, stream_id: &str) -> Result<(), PluginError> {
        let mut store = self
            .inner
            .lock()
            .map_err(|_| PluginError::new("media store poisoned"))?;
        let released = store
            .bodies
            .extract_if(.., |_, body| {
                body.owners.retain(|owner| owner.stream_id != stream_id);
                body.owners.is_empty()
            })
            .map(|(_, body)| u64::try_from(body.bytes.len()).unwrap_or(u64::MAX))
            .try_fold(0_u64, u64::checked_add)
            .ok_or_else(|| PluginError::new("media length overflow"))?;
        store.bytes = store
            .bytes
            .checked_sub(released)
            .ok_or_else(|| PluginError::new("media accounting underflow"))?;
        Ok(())
    }
}

impl MediaResolver for PluginMediaResolver {
    fn read(
        &self,
        descriptor: MediaDescriptor,
        abort: AbortSignal,
    ) -> AdapterFuture<Result<Arc<[u8]>, AiError>> {
        let value = self.inner.lock().map_or_else(
            |_| {
                Err(ai_error(
                    ErrorKind::Artifact,
                    ErrorPhase::Send,
                    DispatchStatus::NotDispatched,
                    "media store lock is poisoned",
                ))
            },
            |store| {
                Ok(store
                    .bodies
                    .get(descriptor.sha256())
                    .map(|body| Arc::clone(&body.bytes)))
            },
        );
        Box::pin(async move {
            let value = value?.ok_or_else(|| {
                ai_error(
                    ErrorKind::Artifact,
                    ErrorPhase::Send,
                    DispatchStatus::NotDispatched,
                    "input media was not uploaded before Start",
                )
            })?;
            if abort.is_aborted() {
                return Err(ai_error(
                    ErrorKind::Cancelled,
                    ErrorPhase::Send,
                    DispatchStatus::NotDispatched,
                    "input media read was cancelled",
                ));
            }
            Ok(value)
        })
    }
}

/// Registry and exact deployment identity produced from one plugin generation config.
#[derive(Clone, Debug)]
pub struct PluginProvider {
    /// Exact capability registry exposed by this generation.
    pub registry: Registry,
    /// Stable deployment identity included in prepared snapshots.
    pub deployment_id: String,
}

/// Concrete dylib seam. Building a generation must perform no provider I/O.
pub trait PluginProviderFactory: Default + fmt::Debug + Send + 'static {
    /// Builds one generation from validated configuration without provider I/O.
    fn build(
        &self,
        generation: u64,
        config: Value,
        media: PluginMediaResolver,
    ) -> Result<PluginProvider, PluginError>;
}

/// Finishes the credential and registry wiring shared by concrete plugin factories.
pub fn build_plugin_provider(
    deployment_id: String,
    credential_id: &'static str,
    api_key: String,
    media: PluginMediaResolver,
    registration: impl FnOnce(CredentialRequirement) -> Result<ProviderRegistration, PluginError>,
) -> Result<PluginProvider, PluginError> {
    let credentials = CredentialManager::builder()
        .with_explicit(credential_id, api_key)
        .map_err(|error| PluginError::context("invalid provider API key", &error))?
        .build();
    let requirement = CredentialRequirement::new(credential_id, std::iter::empty::<String>())
        .map_err(|error| PluginError::context("invalid provider credential requirement", &error))?;
    let registration = registration(requirement)?;
    let registry = Registry::builder(credentials)
        .with_media_resolver(media)
        .register(registration)
        .map_err(|error| PluginError::context("cannot register provider deployment", &error))?
        .build()
        .map_err(|error| PluginError::context("cannot build provider registry", &error))?;
    Ok(PluginProvider {
        registry,
        deployment_id,
    })
}

#[derive(Debug)]
struct Candidate {
    generation: u64,
    provider: PluginProvider,
}

#[derive(Debug)]
struct Active {
    generation: u64,
    provider: PluginProvider,
}

/// Shared implementation used by each concrete provider dylib wrapper.
pub struct ProviderPlugin<F: PluginProviderFactory> {
    host: Host,
    factory: F,
    runtime: Runtime,
    media: PluginMediaResolver,
    candidate: Option<Candidate>,
    active: Option<Active>,
    streams: BTreeMap<String, Arc<PluginStream>>,
}

impl<F: PluginProviderFactory> fmt::Debug for ProviderPlugin<F> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ProviderPlugin")
            .field("factory", &self.factory)
            .field(
                "candidate_generation",
                &self.candidate.as_ref().map(|value| value.generation),
            )
            .field(
                "active_generation",
                &self.active.as_ref().map(|value| value.generation),
            )
            .field("streams", &self.streams.keys().collect::<Vec<_>>())
            .finish_non_exhaustive()
    }
}

#[derive(Debug)]
struct PluginStream {
    service: AiService,
    output: StreamOutput,
    media: PluginMediaResolver,
    calls: Mutex<BTreeMap<String, CallSlot>>,
    uploads: Mutex<BTreeMap<String, Upload>>,
    input_closed: AtomicBool,
    cancelled: AtomicBool,
}

#[derive(Debug)]
enum CallSlot {
    Preparing(CancellationToken),
    LanguagePrepared(PreparedLanguageCall),
    ImagePrepared(PreparedImageCall),
    TranscriptionPrepared(PreparedTranscriptionCall),
    SpeechPrepared(PreparedSpeechCall),
    RealtimePrepared(PreparedRealtimeSession),
    Running {
        cancel: CancellationToken,
        realtime: Option<RealtimeCommandQueue>,
    },
}

#[derive(Clone, Debug)]
struct RealtimeCommandQueue {
    sender: tokio::sync::mpsc::Sender<QueuedRealtimeCommand>,
}

#[derive(Debug)]
struct QueuedRealtimeCommand {
    command: RealtimeCommand,
    input_credit: Option<DeferredInputCredit>,
}

#[derive(Debug)]
struct DeferredInputCredit {
    stream: Arc<PluginStream>,
    charge: u64,
}

impl DeferredInputCredit {
    const fn new(stream: Arc<PluginStream>, charge: u64) -> Self {
        Self { stream, charge }
    }

    fn return_to_caller(self) {
        tokio::spawn(async move {
            let credit = Frame::service_event(
                Some(self.stream.output.request_id.clone()),
                self.stream.service.key(),
                EVENT_CREDIT,
                json!({"bytes":self.charge}),
            );
            if self.stream.output.post(credit, 0).await.is_err() {
                fail_output_stream(&self.stream);
            }
        });
    }
}

impl QueuedRealtimeCommand {
    fn into_command(self) -> RealtimeCommand {
        let Self {
            command,
            input_credit,
        } = self;
        if let Some(input_credit) = input_credit {
            input_credit.return_to_caller();
        }
        command
    }
}

impl RealtimeCommandQueue {
    fn new() -> (Self, tokio::sync::mpsc::Receiver<QueuedRealtimeCommand>) {
        // Each queued command retains at least one byte of the caller's
        // STREAM_BYTE_BUDGET until dequeue. Matching the entry capacity to
        // that byte window makes Full unreachable for a credit-honoring host.
        Self::with_capacity(
            usize::try_from(STREAM_BYTE_BUDGET).expect("stream byte budget fits this platform"),
        )
    }

    fn with_capacity(
        capacity: usize,
    ) -> (Self, tokio::sync::mpsc::Receiver<QueuedRealtimeCommand>) {
        let (sender, receiver) = tokio::sync::mpsc::channel(capacity);
        (Self { sender }, receiver)
    }

    fn try_send(
        &self,
        command: RealtimeCommand,
        input_credit: Option<DeferredInputCredit>,
    ) -> Result<(), PluginError> {
        self.sender
            .try_send(QueuedRealtimeCommand {
                command,
                input_credit,
            })
            .map_err(|error| match error {
                tokio::sync::mpsc::error::TrySendError::Full(_) => {
                    PluginError::new("Realtime command queue is full")
                }
                tokio::sync::mpsc::error::TrySendError::Closed(_) => {
                    PluginError::new("Realtime command queue is closed")
                }
            })
    }
}

#[derive(Debug)]
struct Upload {
    call_id: String,
    assembler: Option<BlobAssembler>,
    realtime_sequence: Option<u32>,
    deferred_input_credit: u64,
}

#[derive(Debug)]
struct StreamOutput {
    host: Host,
    request_id: String,
    service: AiService,
    credit: OutputCredit,
    serial: tokio::sync::Mutex<()>,
    closed: AtomicBool,
}

#[derive(Debug, Default)]
struct OutputCredit {
    available: Mutex<u64>,
    notify: Notify,
}

impl OutputCredit {
    fn grant(&self, bytes: u64) -> Result<(), PluginError> {
        let mut available = self
            .available
            .lock()
            .map_err(|_| PluginError::new("credit lock poisoned"))?;
        *available = available
            .checked_add(bytes)
            .filter(|value| *value <= STREAM_BYTE_BUDGET)
            .ok_or(PluginError::new("output credit exceeds stream window"))?;
        drop(available);
        // `StreamOutput::post` is serialized, so there can be only one credit
        // waiter. A stored permit closes the check-then-wait race when credit
        // arrives immediately before that waiter registers.
        self.notify.notify_one();
        Ok(())
    }

    fn has(&self, charge: u64) -> Result<bool, PluginError> {
        Ok(*self
            .available
            .lock()
            .map_err(|_| PluginError::new("credit lock poisoned"))?
            >= charge)
    }

    fn consume(&self, charge: u64) -> Result<(), PluginError> {
        let mut available = self
            .available
            .lock()
            .map_err(|_| PluginError::new("credit lock poisoned"))?;
        *available = available
            .checked_sub(charge)
            .ok_or(PluginError::new("output credit was consumed twice"))?;
        Ok(())
    }

    async fn changed(&self) {
        self.notify.notified().await;
    }

    fn wake(&self) {
        self.notify.notify_one();
    }
}

impl StreamOutput {
    fn new(host: Host, request_id: String, service: AiService) -> Self {
        Self {
            host,
            request_id,
            service,
            credit: OutputCredit::default(),
            serial: tokio::sync::Mutex::new(()),
            closed: AtomicBool::new(false),
        }
    }

    fn grant(&self, bytes: u64) -> Result<(), PluginError> {
        self.credit.grant(bytes)
    }

    fn wake(&self) {
        self.credit.wake();
    }

    async fn control(&self, control: &ServerControl) -> Result<(), PluginError> {
        let payload =
            encode_server_control(control).map_err(|_| PluginError::new("encode control"))?;
        let bytes = encode_wire_frame(&WireFrame::Control {
            call_id: control.call_id().to_owned(),
            payload,
        })
        .map_err(|_| PluginError::new("encode wire control"))?;
        self.data(bytes).await
    }

    async fn data(&self, payload: Vec<u8>) -> Result<(), PluginError> {
        let charge =
            u64::try_from(payload.len()).map_err(|_| PluginError::new("output length overflow"))?;
        let frame = Frame::service_data_event(&self.request_id, self.service.key(), payload);
        self.post(frame, charge).await
    }

    async fn blob(
        &self,
        call_id: &str,
        blob_id: &str,
        sequence: u32,
        final_chunk: bool,
        bytes: Vec<u8>,
    ) -> Result<(), PluginError> {
        let payload = encode_wire_frame(&WireFrame::BlobChunk {
            call_id: call_id.to_owned(),
            blob_id: blob_id.to_owned(),
            sequence,
            final_chunk,
            bytes,
        })
        .map_err(|_| PluginError::new("encode wire blob"))?;
        self.data(payload).await
    }

    async fn terminal(&self, event: &'static str, reason: Value) -> Result<(), PluginError> {
        if self.closed.swap(true, Ordering::AcqRel) {
            return Ok(());
        }
        let frame = Frame::service_event(
            Some(self.request_id.clone()),
            self.service.key(),
            event,
            reason,
        );
        self.post(frame, 0).await
    }

    async fn post(&self, frame: Frame, charge: u64) -> Result<(), PluginError> {
        let bytes = frame
            .encode()
            .map_err(|_| PluginError::new("encode plugin frame"))?;
        let _serial = self.serial.lock().await;
        loop {
            if self.closed.load(Ordering::Acquire) && charge > 0 {
                return Err(PluginError::new("stream output is closed"));
            }
            let has_credit = self.credit.has(charge)?;
            if !has_credit {
                self.credit.changed().await;
                continue;
            }
            match self
                .host
                .post_frame(Lane::Data, &bytes)
                .map_err(|_| PluginError::new("host unavailable"))?
            {
                PostFrameOutcome::Accepted => {
                    if charge > 0 {
                        self.credit.consume(charge)?;
                    }
                    return Ok(());
                }
                PostFrameOutcome::WouldBlock => self.credit.changed().await,
                PostFrameOutcome::Closed | PostFrameOutcome::Unknown(_) => {
                    return Err(PluginError::new("host closed"));
                }
            }
        }
    }
}

impl<F: PluginProviderFactory> ProviderPlugin<F> {
    #[allow(clippy::needless_pass_by_value)] // Callers construct one-shot lifecycle frames.
    fn post_control(&self, frame: Frame) -> Result<(), PluginError> {
        let bytes = frame
            .encode()
            .map_err(|_| PluginError::new("encode control frame"))?;
        match self
            .host
            .post_frame(Lane::Control, &bytes)
            .map_err(|_| PluginError::new("host unavailable"))?
        {
            PostFrameOutcome::Accepted => Ok(()),
            PostFrameOutcome::WouldBlock => Err(PluginError::new("host control backpressure")),
            PostFrameOutcome::Closed | PostFrameOutcome::Unknown(_) => {
                Err(PluginError::new("host closed"))
            }
        }
    }

    fn prepare_generation(
        &mut self,
        generation: u64,
        config: Option<Value>,
    ) -> Result<(), PluginError> {
        if self.candidate.is_some() {
            return Err(PluginError::new("generation prepare already in progress"));
        }
        let config = config.ok_or(PluginError::new("generation config is missing"))?;
        match self.factory.build(generation, config, self.media.clone()) {
            Ok(provider) => {
                self.candidate = Some(Candidate {
                    generation,
                    provider,
                });
                self.post_control(Frame::lifecycle(LifecyclePhase::Prepared, generation, None))
            }
            Err(error) => self.post_control(Frame::lifecycle(
                LifecyclePhase::PrepareFailed,
                generation,
                Some(json!({"code":"provider_config_invalid", "message":error.to_string()})),
            )),
        }
    }

    fn open_stream(
        &mut self,
        request_id: &str,
        service: &str,
        payload: &Value,
    ) -> Result<(), PluginError> {
        if payload.get("consumer").and_then(Value::as_str).is_none()
            || payload.get("sequence").and_then(Value::as_u64) != Some(0)
            || self.streams.contains_key(request_id)
        {
            return Err(PluginError::new("invalid stream open"));
        }
        let service =
            AiService::from_key(service).ok_or(PluginError::new("unsupported AI service"))?;
        let stream = Arc::new(PluginStream {
            service,
            output: StreamOutput::new(self.host, request_id.to_owned(), service),
            media: self.media.clone(),
            calls: Mutex::new(BTreeMap::new()),
            uploads: Mutex::new(BTreeMap::new()),
            input_closed: AtomicBool::new(false),
            cancelled: AtomicBool::new(false),
        });
        self.streams.insert(request_id.to_owned(), stream);
        let stream = self
            .streams
            .get(request_id)
            .cloned()
            .expect("stream was inserted above");
        self.post_input_credit(stream, STREAM_BYTE_BUDGET);
        Ok(())
    }

    fn post_input_credit(&self, stream: Arc<PluginStream>, charge: u64) {
        let credit = Frame::service_event(
            Some(stream.output.request_id.clone()),
            stream.service.key(),
            EVENT_CREDIT,
            json!({"bytes":charge}),
        );
        self.runtime.spawn(async move {
            if stream.output.post(credit, 0).await.is_err() {
                fail_output_stream(&stream);
            }
        });
    }

    fn handle_data(
        &self,
        request_id: &str,
        service: &str,
        payload: &[u8],
    ) -> Result<(), PluginError> {
        let stream = self
            .streams
            .get(request_id)
            .cloned()
            .ok_or(PluginError::new("unknown stream"))?;
        if service != stream.service.key() || stream.cancelled.load(Ordering::Acquire) {
            return Err(PluginError::new("stream service mismatch or cancelled"));
        }
        let charge =
            u64::try_from(payload.len()).map_err(|_| PluginError::new("input length overflow"))?;
        let provider = self
            .active
            .as_ref()
            .ok_or(PluginError::new("provider is not committed"))?
            .provider
            .clone();
        let input_credit_deferred = match decode_wire_frame(payload)
            .map_err(|error| PluginError::context("invalid nested AI frame", &error))?
        {
            WireFrame::Control { call_id, payload } => {
                let control = decode_client_control(&payload)
                    .map_err(|error| PluginError::context("invalid AI control", &error))?;
                if control.call_id() != call_id {
                    return Err(PluginError::new("nested call id mismatch"));
                }
                self.handle_control(Arc::clone(&stream), provider, control, charge)?
            }
            WireFrame::BlobChunk {
                call_id,
                blob_id,
                sequence,
                final_chunk,
                bytes,
            } => Self::handle_blob(
                &stream,
                &call_id,
                &blob_id,
                sequence,
                final_chunk,
                &bytes,
                charge,
            )?,
        };
        if !input_credit_deferred {
            self.post_input_credit(stream, charge);
        }
        Ok(())
    }

    #[allow(clippy::too_many_lines)] // Closed control grammar keeps state transitions co-located.
    fn handle_control(
        &self,
        stream: Arc<PluginStream>,
        provider: PluginProvider,
        control: ClientControl,
        input_charge: u64,
    ) -> Result<bool, PluginError> {
        match control {
            ClientControl::PrepareLanguage {
                call_id,
                model,
                request,
            } => {
                if stream.service != AiService::Language {
                    return Err(PluginError::new("language request on a different service"));
                }
                let cancel = CancellationToken::new();
                {
                    let mut calls = stream
                        .calls
                        .lock()
                        .map_err(|_| PluginError::new("call lock poisoned"))?;
                    if !calls.is_empty()
                        || calls
                            .insert(call_id.clone(), CallSlot::Preparing(cancel.clone()))
                            .is_some()
                    {
                        return Err(PluginError::new("stream already has an in-flight call"));
                    }
                }
                self.runtime.spawn(prepare_language(
                    stream, provider, call_id, model, request, cancel,
                ));
                Ok(false)
            }
            ClientControl::PrepareImage {
                call_id,
                model,
                request,
            } => {
                ensure_service_and_begin_prepare(&stream, AiService::Image, &call_id)?;
                let cancel = preparing_cancel(&stream, &call_id)?;
                self.runtime.spawn(prepare_image(
                    stream, provider, call_id, model, request, cancel,
                ));
                Ok(false)
            }
            ClientControl::PrepareTranscription {
                call_id,
                model,
                request,
            } => {
                ensure_service_and_begin_prepare(&stream, AiService::Transcription, &call_id)?;
                let cancel = preparing_cancel(&stream, &call_id)?;
                self.runtime.spawn(prepare_transcription(
                    stream, provider, call_id, model, request, cancel,
                ));
                Ok(false)
            }
            ClientControl::PrepareSpeech {
                call_id,
                model,
                request,
            } => {
                ensure_service_and_begin_prepare(&stream, AiService::Speech, &call_id)?;
                let cancel = preparing_cancel(&stream, &call_id)?;
                self.runtime.spawn(prepare_speech(
                    stream, provider, call_id, model, request, cancel,
                ));
                Ok(false)
            }
            ClientControl::PrepareRealtime {
                call_id,
                model,
                request,
            } => {
                ensure_service_and_begin_prepare(&stream, AiService::Realtime, &call_id)?;
                let cancel = preparing_cancel(&stream, &call_id)?;
                self.runtime.spawn(prepare_realtime(
                    stream, provider, call_id, model, request, cancel,
                ));
                Ok(false)
            }
            ClientControl::Start { call_id } => {
                let prepared = {
                    let mut calls = stream
                        .calls
                        .lock()
                        .map_err(|_| PluginError::new("call lock poisoned"))?;
                    match calls.remove(&call_id) {
                        Some(
                            prepared @ (CallSlot::LanguagePrepared(_)
                            | CallSlot::ImagePrepared(_)
                            | CallSlot::TranscriptionPrepared(_)
                            | CallSlot::SpeechPrepared(_)
                            | CallSlot::RealtimePrepared(_)),
                        ) => prepared,
                        other => {
                            if let Some(other) = other {
                                calls.insert(call_id, other);
                            }
                            return Err(PluginError::new("call is not prepared"));
                        }
                    }
                };
                let cancel = CancellationToken::new();
                match prepared {
                    CallSlot::LanguagePrepared(prepared) => {
                        insert_running(&stream, &call_id, cancel.clone(), None)?;
                        self.runtime
                            .spawn(run_language(stream, call_id, prepared, cancel));
                    }
                    CallSlot::ImagePrepared(prepared) => {
                        insert_running(&stream, &call_id, cancel.clone(), None)?;
                        self.runtime
                            .spawn(run_image(stream, call_id, prepared, cancel));
                    }
                    CallSlot::TranscriptionPrepared(prepared) => {
                        insert_running(&stream, &call_id, cancel.clone(), None)?;
                        self.runtime
                            .spawn(run_transcription(stream, call_id, prepared, cancel));
                    }
                    CallSlot::SpeechPrepared(prepared) => {
                        insert_running(&stream, &call_id, cancel.clone(), None)?;
                        self.runtime
                            .spawn(run_speech(stream, call_id, prepared, cancel));
                    }
                    CallSlot::RealtimePrepared(prepared) => {
                        let (commands, receiver) = RealtimeCommandQueue::new();
                        insert_running(&stream, &call_id, cancel.clone(), Some(commands))?;
                        self.runtime
                            .spawn(run_realtime(stream, call_id, prepared, cancel, receiver));
                    }
                    CallSlot::Preparing(_) | CallSlot::Running { .. } => {
                        unreachable!("filtered above")
                    }
                }
                Ok(false)
            }
            ClientControl::Abort { call_id } => {
                if let Some(slot) = stream
                    .calls
                    .lock()
                    .map_err(|_| PluginError::new("call lock poisoned"))?
                    .remove(&call_id)
                {
                    match slot {
                        CallSlot::Preparing(cancel) | CallSlot::Running { cancel, .. } => {
                            cancel.cancel();
                        }
                        CallSlot::LanguagePrepared(_)
                        | CallSlot::ImagePrepared(_)
                        | CallSlot::TranscriptionPrepared(_)
                        | CallSlot::SpeechPrepared(_)
                        | CallSlot::RealtimePrepared(_) => {
                            stream.media.release_call(&MediaOwner::new(
                                &stream.output.request_id,
                                &call_id,
                            ))?;
                        }
                    }
                }
                Ok(false)
            }
            ClientControl::DeclareInputBlob {
                call_id,
                blob_id,
                descriptor,
            } => {
                let mut uploads = stream
                    .uploads
                    .lock()
                    .map_err(|_| PluginError::new("upload lock poisoned"))?;
                if uploads.contains_key(&blob_id) {
                    return Err(PluginError::new("duplicate input blob"));
                }
                validate_upload_quota(&uploads, &self.media, &descriptor)?;
                uploads.insert(
                    blob_id,
                    Upload {
                        call_id,
                        assembler: Some(BlobAssembler::new(descriptor)),
                        realtime_sequence: None,
                        deferred_input_credit: 0,
                    },
                );
                Ok(false)
            }
            ClientControl::RealtimeAppendAudio {
                call_id,
                blob_id,
                sequence,
                descriptor,
            } => {
                let mut uploads = stream
                    .uploads
                    .lock()
                    .map_err(|_| PluginError::new("upload lock poisoned"))?;
                if stream.service != AiService::Realtime || uploads.contains_key(&blob_id) {
                    return Err(PluginError::new("invalid Realtime audio declaration"));
                }
                validate_upload_quota(&uploads, &self.media, &descriptor)?;
                uploads.insert(
                    blob_id,
                    Upload {
                        call_id,
                        assembler: Some(BlobAssembler::new(descriptor)),
                        realtime_sequence: Some(sequence),
                        deferred_input_credit: input_charge,
                    },
                );
                Ok(true)
            }
            ClientControl::RealtimeAppendText { call_id, text } => {
                send_realtime_command(
                    &stream,
                    &call_id,
                    RealtimeCommand::AppendText { text },
                    input_charge,
                )?;
                Ok(true)
            }
            ClientControl::RealtimeCommitInput { call_id, item_id } => {
                send_realtime_command(
                    &stream,
                    &call_id,
                    RealtimeCommand::CommitInput { item_id },
                    input_charge,
                )?;
                Ok(true)
            }
            ClientControl::RealtimeRequestResponse { call_id } => {
                send_realtime_command(
                    &stream,
                    &call_id,
                    RealtimeCommand::RequestResponse,
                    input_charge,
                )?;
                Ok(true)
            }
            ClientControl::RealtimeCancelResponse {
                call_id,
                response_id,
            } => send_realtime_command(
                &stream,
                &call_id,
                RealtimeCommand::CancelResponse { response_id },
                input_charge,
            )
            .map(|()| true),
            ClientControl::RealtimeClose { call_id } => {
                send_realtime_command(&stream, &call_id, RealtimeCommand::Close, input_charge)?;
                Ok(true)
            }
        }
    }

    fn handle_blob(
        stream: &Arc<PluginStream>,
        call_id: &str,
        blob_id: &str,
        sequence: u32,
        final_chunk: bool,
        bytes: &[u8],
        input_charge: u64,
    ) -> Result<bool, PluginError> {
        let (complete, input_credit_deferred) = {
            let mut uploads = stream
                .uploads
                .lock()
                .map_err(|_| PluginError::new("upload lock poisoned"))?;
            let upload = uploads
                .get_mut(blob_id)
                .ok_or(PluginError::new("undeclared input blob"))?;
            if upload.call_id != call_id {
                return Err(PluginError::new("input blob call id mismatch"));
            }
            let input_credit_deferred = upload.realtime_sequence.is_some();
            if input_credit_deferred {
                upload.deferred_input_credit = upload
                    .deferred_input_credit
                    .checked_add(input_charge)
                    .filter(|charge| *charge <= STREAM_BYTE_BUDGET)
                    .ok_or(PluginError::new("Realtime input credit exceeds its window"))?;
            }
            let assembler = upload
                .assembler
                .as_mut()
                .ok_or(PluginError::new("input blob already complete"))?;
            assembler
                .push(sequence, bytes, final_chunk)
                .map_err(|_| PluginError::new("invalid input blob chunk"))?;
            let complete = if final_chunk {
                let upload = uploads
                    .remove(blob_id)
                    .ok_or(PluginError::new("input blob disappeared"))?;
                let assembler = upload
                    .assembler
                    .ok_or(PluginError::new("input blob already complete"))?;
                let descriptor = assembler.descriptor().clone();
                let bytes = assembler
                    .finish()
                    .map_err(|_| PluginError::new("invalid complete input blob"))?;
                Some((
                    descriptor,
                    bytes,
                    upload.realtime_sequence,
                    upload.deferred_input_credit,
                ))
            } else {
                None
            };
            (complete, input_credit_deferred)
        };
        if let Some((descriptor, bytes, realtime_sequence, deferred_input_credit)) = complete {
            if let Some(sequence) = realtime_sequence {
                send_realtime_command(
                    stream,
                    call_id,
                    RealtimeCommand::AppendAudio { sequence, bytes },
                    deferred_input_credit,
                )?;
            } else {
                stream.media.insert(
                    &MediaOwner::new(&stream.output.request_id, call_id),
                    &descriptor,
                    bytes,
                )?;
            }
        }
        Ok(input_credit_deferred)
    }

    fn half_close(&self, request_id: &str, service: &str) -> Result<(), PluginError> {
        let stream = self
            .streams
            .get(request_id)
            .cloned()
            .ok_or(PluginError::new("unknown stream"))?;
        if stream.service.key() != service || stream.input_closed.swap(true, Ordering::AcqRel) {
            return Err(PluginError::new("invalid half close"));
        }
        maybe_end(stream, &self.runtime);
        Ok(())
    }

    fn cancel_stream(
        &mut self,
        request_id: &str,
        service: &str,
        payload: Value,
    ) -> Result<(), PluginError> {
        let stream = self
            .streams
            .remove(request_id)
            .ok_or(PluginError::new("unknown stream"))?;
        if stream.service.key() != service
            || payload.get("reason").and_then(Value::as_str).is_none()
        {
            return Err(PluginError::new("invalid stream cancellation"));
        }
        stream.cancelled.store(true, Ordering::Release);
        for call in stream
            .calls
            .lock()
            .map_err(|_| PluginError::new("call lock poisoned"))?
            .values()
        {
            if let CallSlot::Preparing(cancel) | CallSlot::Running { cancel, .. } = call {
                cancel.cancel();
            }
        }
        self.media.release_stream(request_id)?;
        let output = Arc::clone(&stream);
        self.runtime.spawn(async move {
            let _ = output.output.terminal(EVENT_CANCEL, payload).await;
        });
        Ok(())
    }
}

fn validate_upload_quota(
    uploads: &BTreeMap<String, Upload>,
    media: &PluginMediaResolver,
    descriptor: &MediaDescriptor,
) -> Result<(), PluginError> {
    let (stored_count, stored_bytes) = media.usage()?;
    let pending_bytes = uploads.values().try_fold(0_u64, |total, upload| {
        let length = upload
            .assembler
            .as_ref()
            .map_or(0, |assembler| assembler.descriptor().byte_len());
        total
            .checked_add(length)
            .ok_or(PluginError::new("media length overflow"))
    })?;
    let total_count = stored_count
        .checked_add(uploads.len())
        .and_then(|value| value.checked_add(1))
        .ok_or(PluginError::new("generation media quota exceeded"))?;
    let total_bytes = stored_bytes
        .checked_add(pending_bytes)
        .and_then(|value| value.checked_add(descriptor.byte_len()))
        .ok_or(PluginError::new("generation media quota exceeded"))?;
    if total_count > MAX_MEDIA_BLOBS || total_bytes > MAX_MEDIA_BYTES {
        return Err(PluginError::new("generation media quota exceeded"));
    }
    Ok(())
}

impl<F: PluginProviderFactory> Plugin for ProviderPlugin<F> {
    type Error = PluginError;

    fn create(host: Host) -> Result<Self, Self::Error> {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .enable_all()
            .thread_name("rsi-ai-provider")
            .build()
            .map_err(|_| PluginError::new("create provider runtime"))?;
        Ok(Self {
            host,
            factory: F::default(),
            runtime,
            media: PluginMediaResolver::default(),
            candidate: None,
            active: None,
            streams: BTreeMap::new(),
        })
    }

    #[allow(clippy::too_many_lines)] // Lifecycle and stream frames form one closed state machine.
    fn on_frame(&mut self, lane: Lane, payload: &[u8]) -> Result<(), Self::Error> {
        let frame = Frame::decode(payload)
            .map_err(|error| PluginError::context("invalid plugin frame", &error))?;
        match frame.body {
            FrameBody::Lifecycle {
                phase: LifecyclePhase::Prepare,
                generation,
                config,
            } if lane == Lane::Control => self.prepare_generation(generation, config),
            FrameBody::Lifecycle {
                phase: LifecyclePhase::Abort,
                generation,
                ..
            } if lane == Lane::Control => {
                if self
                    .candidate
                    .as_ref()
                    .is_some_and(|candidate| candidate.generation == generation)
                {
                    self.candidate = None;
                }
                Ok(())
            }
            FrameBody::Lifecycle {
                phase: LifecyclePhase::Committed,
                generation,
                ..
            } if lane == Lane::Control => {
                let candidate = self
                    .candidate
                    .take()
                    .ok_or(PluginError::new("no prepared generation"))?;
                if candidate.generation != generation {
                    return Err(PluginError::new("committed generation mismatch"));
                }
                self.active = Some(Active {
                    generation,
                    provider: candidate.provider,
                });
                Ok(())
            }
            FrameBody::Lifecycle {
                phase: LifecyclePhase::Retire,
                generation,
                ..
            } if lane == Lane::Control => {
                if self
                    .active
                    .as_ref()
                    .is_none_or(|active| active.generation != generation)
                {
                    return Err(PluginError::new("retire arrived for the wrong generation"));
                }
                if self
                    .streams
                    .values()
                    .any(|stream| !stream.output.closed.load(Ordering::Acquire))
                {
                    return Err(PluginError::new(
                        "retire arrived with active streams or wrong generation",
                    ));
                }
                self.streams.clear();
                self.active = None;
                self.post_control(Frame::lifecycle(LifecyclePhase::Retired, generation, None))
            }
            FrameBody::ServiceRequest {
                request_id,
                service,
                operation,
                payload,
            } if lane == Lane::Data && self.active.is_some() => match operation.as_str() {
                OP_OPEN => self.open_stream(&request_id, &service, &payload),
                OP_CREDIT => {
                    let bytes = payload
                        .get("bytes")
                        .and_then(Value::as_u64)
                        .ok_or(PluginError::new("credit bytes missing"))?;
                    self.streams
                        .get(&request_id)
                        .ok_or(PluginError::new("unknown stream"))?
                        .output
                        .grant(bytes)
                }
                OP_HALF_CLOSE => self.half_close(&request_id, &service),
                OP_CANCEL => self.cancel_stream(&request_id, &service, payload),
                _ => Err(PluginError::new("unknown stream operation")),
            },
            FrameBody::ServiceDataRequest {
                request_id,
                service,
                payload,
            } if lane == Lane::Data && self.active.is_some() => {
                self.handle_data(&request_id, &service, &payload)
            }
            FrameBody::ServiceEvent { service, event, .. }
                if service == RUNTIME_TICK_SERVICE && event == RUNTIME_TICK_EVENT =>
            {
                for stream in self.streams.values() {
                    stream.output.wake();
                }
                Ok(())
            }
            _ => Err(PluginError::new(
                "frame rejected in current lifecycle state",
            )),
        }
    }

    fn shutdown(&mut self) -> Result<(), Self::Error> {
        for stream in self.streams.values() {
            fail_output_stream(stream);
        }
        self.streams.clear();
        Ok(())
    }
}

fn ensure_service_and_begin_prepare(
    stream: &PluginStream,
    service: AiService,
    call_id: &str,
) -> Result<(), PluginError> {
    if stream.service != service {
        return Err(PluginError::new(
            "capability request arrived on a different service",
        ));
    }
    let cancel = CancellationToken::new();
    let mut calls = stream
        .calls
        .lock()
        .map_err(|_| PluginError::new("call lock poisoned"))?;
    if !calls.is_empty()
        || calls
            .insert(call_id.to_owned(), CallSlot::Preparing(cancel))
            .is_some()
    {
        return Err(PluginError::new("stream already has an in-flight call"));
    }
    Ok(())
}

fn preparing_cancel(
    stream: &PluginStream,
    call_id: &str,
) -> Result<CancellationToken, PluginError> {
    let calls = stream
        .calls
        .lock()
        .map_err(|_| PluginError::new("call lock poisoned"))?;
    match calls.get(call_id) {
        Some(CallSlot::Preparing(cancel)) => Ok(cancel.clone()),
        _ => Err(PluginError::new("call is not preparing")),
    }
}

fn insert_running(
    stream: &PluginStream,
    call_id: &str,
    cancel: CancellationToken,
    realtime: Option<RealtimeCommandQueue>,
) -> Result<(), PluginError> {
    let previous = stream
        .calls
        .lock()
        .map_err(|_| PluginError::new("call lock poisoned"))?
        .insert(call_id.to_owned(), CallSlot::Running { cancel, realtime });
    if previous.is_some() {
        return Err(PluginError::new("call unexpectedly remained occupied"));
    }
    Ok(())
}

fn send_realtime_command(
    stream: &Arc<PluginStream>,
    call_id: &str,
    command: RealtimeCommand,
    input_charge: u64,
) -> Result<(), PluginError> {
    if input_charge == 0 {
        return Err(PluginError::new(
            "Realtime input frame has no credit charge",
        ));
    }
    if stream.service != AiService::Realtime {
        return Err(PluginError::new(
            "Realtime command arrived on a different service",
        ));
    }
    let sender = {
        let calls = stream
            .calls
            .lock()
            .map_err(|_| PluginError::new("call lock poisoned"))?;
        match calls.get(call_id) {
            Some(CallSlot::Running {
                realtime: Some(sender),
                ..
            }) => sender.clone(),
            _ => return Err(PluginError::new("Realtime call is not running")),
        }
    };
    sender.try_send(
        command,
        Some(DeferredInputCredit::new(Arc::clone(stream), input_charge)),
    )
}

macro_rules! prepare_capability {
    ($function:ident, $request:ty, $lookup:ident, $slot:ident) => {
        async fn $function(
            stream: Arc<PluginStream>,
            provider: PluginProvider,
            call_id: String,
            model: String,
            request: $request,
            cancel: CancellationToken,
        ) {
            let result = async {
                let model = provider
                    .registry
                    .$lookup(ModelRef::new(&provider.deployment_id, model).map_err(registry_error)?)
                    .map_err(registry_error)?;
                model.prepare(request).await.map_err(registry_error)
            };
            let result = tokio::select! {
                () = cancel.cancelled() => Err(ai_error(
                    ErrorKind::Cancelled,
                    ErrorPhase::Prepare,
                    DispatchStatus::NotStarted,
                    "call was aborted during prepare",
                )),
                result = result => result,
            };
            match result {
                Ok(prepared) => {
                    let snapshot = prepared.snapshot().clone();
                    let inserted = match stream.calls.lock() {
                        Ok(mut calls) => {
                            if matches!(calls.get(&call_id), Some(CallSlot::Preparing(_))) {
                                calls.insert(call_id.clone(), CallSlot::$slot(prepared));
                                true
                            } else {
                                false
                            }
                        }
                        Err(error) => {
                            drop(error);
                            fail_output_stream(&stream);
                            return;
                        }
                    };
                    if inserted {
                        if stream
                            .output
                            .control(&ServerControl::Prepared { call_id, snapshot })
                            .await
                            .is_err()
                        {
                            fail_output_stream(&stream);
                        }
                    } else {
                        let _ = stream
                            .media
                            .release_call(&MediaOwner::new(&stream.output.request_id, &call_id));
                    }
                }
                Err(error) => {
                    if !remove_call(&stream, &call_id) {
                        return;
                    }
                    if stream
                        .media
                        .release_call(&MediaOwner::new(&stream.output.request_id, &call_id))
                        .is_err()
                    {
                        fail_output_stream(&stream);
                        return;
                    }
                    let _ = stream
                        .output
                        .control(&ServerControl::Failed { call_id, error })
                        .await;
                }
            }
        }
    };
}

prepare_capability!(
    prepare_language,
    rsi_ai_protocol::LanguageRequest,
    language,
    LanguagePrepared
);
prepare_capability!(
    prepare_image,
    rsi_ai_protocol::ImageRequest,
    image,
    ImagePrepared
);
prepare_capability!(
    prepare_transcription,
    rsi_ai_protocol::TranscriptionRequest,
    transcription,
    TranscriptionPrepared
);
prepare_capability!(
    prepare_speech,
    rsi_ai_protocol::SpeechRequest,
    speech,
    SpeechPrepared
);
prepare_capability!(
    prepare_realtime,
    rsi_ai_protocol::RealtimeRequest,
    realtime,
    RealtimePrepared
);

async fn run_language(
    stream: Arc<PluginStream>,
    call_id: String,
    prepared: PreparedLanguageCall,
    cancel: CancellationToken,
) {
    match prepared.start().await {
        Ok(mut generation) => loop {
            let next = tokio::select! {
                () = cancel.cancelled() => {
                    generation.abort();
                    let error = ai_error(ErrorKind::Cancelled, ErrorPhase::Stream, DispatchStatus::Unknown, "call was aborted");
                    let _ = stream.output.control(&ServerControl::Failed { call_id: call_id.clone(), error }).await;
                    break;
                }
                next = generation.next() => next,
            };
            let Some(event) = next else { break };
            let terminal = matches!(
                event,
                rsi_ai_protocol::LanguageEvent::Finished { .. }
                    | rsi_ai_protocol::LanguageEvent::Failed { .. }
            );
            if stream
                .output
                .control(&ServerControl::LanguageEvent {
                    call_id: call_id.clone(),
                    event,
                })
                .await
                .is_err()
            {
                break;
            }
            if terminal {
                break;
            }
        },
        Err(error) => {
            let _ = stream
                .output
                .control(&ServerControl::Failed {
                    call_id: call_id.clone(),
                    error: registry_error(error),
                })
                .await;
        }
    }
    finish_running_call(stream, call_id, None).await;
}

#[derive(Debug)]
struct OutgoingBlob {
    blob_id: String,
    mime_type: String,
    hasher: Sha256,
    byte_len: u64,
    next_wire_sequence: u32,
}

impl OutgoingBlob {
    fn new(blob_id: String, mime_type: String) -> Self {
        Self {
            blob_id,
            mime_type,
            hasher: Sha256::new(),
            byte_len: 0,
            next_wire_sequence: 1,
        }
    }

    fn record(&mut self, bytes: &[u8]) -> Result<u32, PluginError> {
        self.byte_len = self
            .byte_len
            .checked_add(
                u64::try_from(bytes.len())
                    .map_err(|_| PluginError::new("media length overflow"))?,
            )
            .ok_or(PluginError::new("media length overflow"))?;
        self.hasher.update(bytes);
        let sequence = self.next_wire_sequence;
        self.next_wire_sequence = self.next_wire_sequence.saturating_add(1);
        Ok(sequence)
    }

    fn finish(self, kind: MediaKind) -> Result<MediaDescriptor, PluginError> {
        let sha256 = hex::encode(self.hasher.finalize());
        MediaDescriptor::new(kind, self.mime_type, self.byte_len, sha256)
            .map_err(|_| PluginError::new("provider emitted invalid media"))
    }
}

#[allow(clippy::too_many_lines)] // Streaming media validation stays beside terminal assembly.
async fn run_image(
    stream: Arc<PluginStream>,
    call_id: String,
    prepared: PreparedImageCall,
    cancel: CancellationToken,
) {
    let mut failure = None;
    match prepared.start().await {
        Ok(mut generation) => {
            let mut outputs = BTreeMap::<u32, OutgoingBlob>::new();
            loop {
                let next = tokio::select! {
                    () = cancel.cancelled() => {
                        generation.abort();
                        failure = Some(cancelled_stream_error());
                        break;
                    }
                    next = generation.next() => next,
                };
                let Some(event) = next else { break };
                let result = match event {
                    ImageEvent::OutputStarted { index, mime_type } => {
                        let blob_id = format!("{call_id}.image.{index}");
                        outputs
                            .insert(index, OutgoingBlob::new(blob_id.clone(), mime_type.clone()));
                        stream
                            .output
                            .control(&ServerControl::ImageOutputStarted {
                                call_id: call_id.clone(),
                                index,
                                blob_id,
                                mime_type,
                            })
                            .await
                    }
                    ImageEvent::OutputChunk {
                        index,
                        sequence: _,
                        bytes,
                    } => {
                        let Some(output) = outputs.get_mut(&index) else {
                            failure =
                                Some(protocol_ai_error("image chunk arrived before output start"));
                            break;
                        };
                        let Ok(sequence) = output.record(&bytes) else {
                            failure = Some(protocol_ai_error("image output length overflowed"));
                            break;
                        };
                        stream
                            .output
                            .blob(&call_id, &output.blob_id, sequence, false, bytes)
                            .await
                    }
                    ImageEvent::OutputFinished { index } => {
                        let Some(output) = outputs.remove(&index) else {
                            failure = Some(protocol_ai_error("image output finished before start"));
                            break;
                        };
                        let final_sequence = output.next_wire_sequence;
                        let blob_id = output.blob_id.clone();
                        let Ok(descriptor) = output.finish(MediaKind::Image) else {
                            failure = Some(protocol_ai_error("image output descriptor is invalid"));
                            break;
                        };
                        if let Err(error) = stream
                            .output
                            .blob(&call_id, &blob_id, final_sequence, true, Vec::new())
                            .await
                        {
                            Err(error)
                        } else {
                            stream
                                .output
                                .control(&ServerControl::ImageOutputFinished {
                                    call_id: call_id.clone(),
                                    index,
                                    blob_id,
                                    descriptor,
                                })
                                .await
                        }
                    }
                    ImageEvent::Usage { .. } | ImageEvent::Finished => Ok(()),
                };
                if result.is_err() {
                    break;
                }
            }
            if failure.is_none() {
                match generation.finish() {
                    Ok(output) => {
                        let _ = stream
                            .output
                            .control(&ServerControl::ImageFinished {
                                call_id: call_id.clone(),
                                usage: output.usage,
                            })
                            .await;
                    }
                    Err(error) => failure = Some(registry_error(error)),
                }
            }
        }
        Err(error) => failure = Some(registry_error(error)),
    }
    finish_running_call(stream, call_id, failure).await;
}

async fn run_transcription(
    stream: Arc<PluginStream>,
    call_id: String,
    prepared: PreparedTranscriptionCall,
    cancel: CancellationToken,
) {
    let mut failure = None;
    match prepared.start().await {
        Ok(mut generation) => {
            loop {
                let next = tokio::select! {
                    () = cancel.cancelled() => {
                        generation.abort();
                        failure = Some(cancelled_stream_error());
                        break;
                    }
                    next = generation.next() => next,
                };
                let Some(event) = next else { break };
                if stream
                    .output
                    .control(&ServerControl::TranscriptionEvent {
                        call_id: call_id.clone(),
                        event,
                    })
                    .await
                    .is_err()
                {
                    break;
                }
            }
            if failure.is_none()
                && let Err(error) = generation.finish()
            {
                failure = Some(registry_error(error));
            }
        }
        Err(error) => failure = Some(registry_error(error)),
    }
    finish_running_call(stream, call_id, failure).await;
}

#[allow(clippy::too_many_lines)] // Streaming media validation stays beside terminal assembly.
async fn run_speech(
    stream: Arc<PluginStream>,
    call_id: String,
    prepared: PreparedSpeechCall,
    cancel: CancellationToken,
) {
    let mut failure = None;
    let mut outgoing = None::<OutgoingBlob>;
    match prepared.start().await {
        Ok(mut generation) => {
            loop {
                let next = tokio::select! {
                    () = cancel.cancelled() => {
                        generation.abort();
                        failure = Some(cancelled_stream_error());
                        break;
                    }
                    next = generation.next() => next,
                };
                let Some(event) = next else { break };
                let result = match event {
                    SpeechEvent::OutputStarted { mime_type } => {
                        let blob_id = format!("{call_id}.speech");
                        outgoing = Some(OutgoingBlob::new(blob_id.clone(), mime_type.clone()));
                        stream
                            .output
                            .control(&ServerControl::SpeechOutputStarted {
                                call_id: call_id.clone(),
                                blob_id,
                                mime_type,
                            })
                            .await
                    }
                    SpeechEvent::AudioChunk { sequence: _, bytes } => {
                        let Some(output) = outgoing.as_mut() else {
                            failure = Some(protocol_ai_error(
                                "speech chunk arrived before output start",
                            ));
                            break;
                        };
                        let Ok(sequence) = output.record(&bytes) else {
                            failure = Some(protocol_ai_error("speech output length overflowed"));
                            break;
                        };
                        stream
                            .output
                            .blob(&call_id, &output.blob_id, sequence, false, bytes)
                            .await
                    }
                    SpeechEvent::OutputFinished => {
                        let Some(output) = outgoing.take() else {
                            failure =
                                Some(protocol_ai_error("speech output finished before start"));
                            break;
                        };
                        let final_sequence = output.next_wire_sequence;
                        let blob_id = output.blob_id.clone();
                        let Ok(descriptor) = output.finish(MediaKind::Audio) else {
                            failure =
                                Some(protocol_ai_error("speech output descriptor is invalid"));
                            break;
                        };
                        if let Err(error) = stream
                            .output
                            .blob(&call_id, &blob_id, final_sequence, true, Vec::new())
                            .await
                        {
                            Err(error)
                        } else {
                            stream
                                .output
                                .control(&ServerControl::SpeechOutputFinished {
                                    call_id: call_id.clone(),
                                    blob_id,
                                    descriptor,
                                })
                                .await
                        }
                    }
                    SpeechEvent::Usage { .. } | SpeechEvent::Finished => Ok(()),
                };
                if result.is_err() {
                    break;
                }
            }
            if failure.is_none() {
                match generation.finish() {
                    Ok(output) => {
                        let _ = stream
                            .output
                            .control(&ServerControl::SpeechFinished {
                                call_id: call_id.clone(),
                                usage: output.usage,
                            })
                            .await;
                    }
                    Err(error) => failure = Some(registry_error(error)),
                }
            }
        }
        Err(error) => failure = Some(registry_error(error)),
    }
    finish_running_call(stream, call_id, failure).await;
}

async fn run_realtime(
    stream: Arc<PluginStream>,
    call_id: String,
    prepared: PreparedRealtimeSession,
    cancel: CancellationToken,
    mut commands: tokio::sync::mpsc::Receiver<QueuedRealtimeCommand>,
) {
    let mut failure = None;
    match prepared.start().await {
        Ok(mut session) => loop {
            tokio::select! {
                () = cancel.cancelled() => {
                    session.abort();
                    failure = Some(cancelled_stream_error());
                    break;
                }
                command = commands.recv() => {
                    let Some(command) = command else {
                        failure = Some(protocol_ai_error("Realtime command channel closed"));
                        break;
                    };
                    let command = command.into_command();
                    let closes = matches!(command, RealtimeCommand::Close);
                    let result = if closes { session.close().await } else { session.send(command).await };
                    if let Err(error) = result {
                        failure = Some(registry_error(error));
                        break;
                    }
                    if closes {
                        let _ = stream.output.control(&ServerControl::RealtimeClosed {
                            call_id: call_id.clone(), reason: rsi_ai_protocol::RealtimeCloseReason::Client,
                        }).await;
                        break;
                    }
                }
                event = session.next_event() => {
                    match event {
                        Ok(Some(event)) => {
                            let terminal = matches!(event, RealtimeEvent::Closed { .. });
                            if emit_realtime_event(&stream, &call_id, event).await.is_err() {
                                break;
                            }
                            if terminal { break; }
                        }
                        Ok(None) => {
                            failure = Some(protocol_ai_error("Realtime session ended without close"));
                            break;
                        }
                        Err(error) => {
                            failure = Some(registry_error(error));
                            break;
                        }
                    }
                }
            }
        },
        Err(error) => failure = Some(registry_error(error)),
    }
    finish_running_call(stream, call_id, failure).await;
}

async fn emit_realtime_event(
    stream: &PluginStream,
    call_id: &str,
    event: RealtimeEvent,
) -> Result<(), PluginError> {
    let control = match event {
        RealtimeEvent::SessionStarted { session_id } => ServerControl::RealtimeSessionStarted {
            call_id: call_id.to_owned(),
            session_id,
        },
        RealtimeEvent::InputSpeechStarted { item_id } => ServerControl::RealtimeSpeechStarted {
            call_id: call_id.to_owned(),
            item_id,
        },
        RealtimeEvent::InputTranscriptDelta { item_id, text } => {
            ServerControl::RealtimeTranscriptDelta {
                call_id: call_id.to_owned(),
                item_id,
                text,
                finished: false,
            }
        }
        RealtimeEvent::InputTranscriptFinished { item_id, text } => {
            ServerControl::RealtimeTranscriptDelta {
                call_id: call_id.to_owned(),
                item_id,
                text,
                finished: true,
            }
        }
        RealtimeEvent::OutputTextDelta { response_id, text } => ServerControl::RealtimeTextDelta {
            call_id: call_id.to_owned(),
            response_id,
            text,
        },
        RealtimeEvent::OutputAudioChunk {
            response_id,
            sequence,
            bytes,
        } => {
            let blob_id = format!("{call_id}.realtime.{response_id}.{sequence}");
            let sha256 = hex::encode(Sha256::digest(&bytes));
            let descriptor = MediaDescriptor::new(
                MediaKind::Audio,
                "audio/pcm".to_owned(),
                u64::try_from(bytes.len())
                    .map_err(|_| PluginError::new("Realtime audio length overflow"))?,
                sha256,
            )
            .map_err(|_| PluginError::new("invalid Realtime audio"))?;
            stream
                .output
                .control(&ServerControl::RealtimeAudio {
                    call_id: call_id.to_owned(),
                    response_id,
                    sequence,
                    blob_id: blob_id.clone(),
                    descriptor,
                })
                .await?;
            return stream.output.blob(call_id, &blob_id, 1, true, bytes).await;
        }
        RealtimeEvent::HandoffRequested { item_id, text } => {
            ServerControl::RealtimeHandoffRequested {
                call_id: call_id.to_owned(),
                item_id,
                text,
            }
        }
        RealtimeEvent::RecoverableError { error } => ServerControl::RealtimeRecoverableError {
            call_id: call_id.to_owned(),
            error,
        },
        RealtimeEvent::Closed { reason } => ServerControl::RealtimeClosed {
            call_id: call_id.to_owned(),
            reason,
        },
    };
    stream.output.control(&control).await
}

async fn finish_running_call(stream: Arc<PluginStream>, call_id: String, failure: Option<AiError>) {
    if let Some(error) = failure {
        let _ = stream
            .output
            .control(&ServerControl::Failed {
                call_id: call_id.clone(),
                error,
            })
            .await;
    }
    if stream
        .media
        .release_call(&MediaOwner::new(&stream.output.request_id, &call_id))
        .is_err()
    {
        fail_output_stream(&stream);
        return;
    }
    if !remove_call(&stream, &call_id) {
        return;
    }
    maybe_end(stream, &tokio::runtime::Handle::current());
}

fn cancelled_stream_error() -> AiError {
    ai_error(
        ErrorKind::Cancelled,
        ErrorPhase::Stream,
        DispatchStatus::Unknown,
        "call was aborted",
    )
}

fn protocol_ai_error(summary: &'static str) -> AiError {
    ai_error(
        ErrorKind::Protocol,
        ErrorPhase::Stream,
        DispatchStatus::Unknown,
        summary,
    )
}

fn fail_output_stream(stream: &PluginStream) {
    stream.cancelled.store(true, Ordering::Release);
    stream.output.wake();
    if let Ok(mut calls) = stream.calls.lock() {
        for call in calls.values() {
            if let CallSlot::Preparing(cancel) | CallSlot::Running { cancel, .. } = call {
                cancel.cancel();
            }
        }
        calls.clear();
    }
    let _ = stream.media.release_stream(&stream.output.request_id);
}

fn remove_call(stream: &PluginStream, call_id: &str) -> bool {
    match stream.calls.lock() {
        Ok(mut calls) => {
            calls.remove(call_id);
            true
        }
        Err(error) => {
            drop(error);
            fail_output_stream(stream);
            false
        }
    }
}

fn maybe_end(stream: Arc<PluginStream>, runtime: &impl SpawnHandle) {
    let idle = match stream.calls.lock() {
        Ok(calls) => calls.is_empty(),
        Err(error) => {
            drop(error);
            fail_output_stream(&stream);
            return;
        }
    };
    if stream.input_closed.load(Ordering::Acquire) && idle {
        runtime.spawn_end(async move {
            let _ = stream.output.terminal(EVENT_END, json!({})).await;
        });
    }
}

trait SpawnHandle {
    fn spawn_end(&self, future: impl std::future::Future<Output = ()> + Send + 'static);
}

impl SpawnHandle for Runtime {
    fn spawn_end(&self, future: impl std::future::Future<Output = ()> + Send + 'static) {
        self.spawn(future);
    }
}

impl SpawnHandle for tokio::runtime::Handle {
    fn spawn_end(&self, future: impl std::future::Future<Output = ()> + Send + 'static) {
        self.spawn(future);
    }
}

#[allow(clippy::needless_pass_by_value)] // Intended for direct use with Result::map_err.
fn registry_error(error: RegistryError) -> AiError {
    if let Some(provider_error) = error.provider_error() {
        return provider_error.clone();
    }
    let kind = ErrorKind::from_code(error.code()).unwrap_or_else(|| match error.code() {
        "credential.missing" => ErrorKind::Authentication,
        "registry.capability_unavailable" => ErrorKind::Unsupported,
        "request.invalid" | "registry.invalid_model_ref" => ErrorKind::InvalidRequest,
        _ => ErrorKind::Transport,
    });
    ai_error(
        kind,
        ErrorPhase::Prepare,
        DispatchStatus::Unknown,
        error.to_string(),
    )
}

fn ai_error(
    kind: ErrorKind,
    phase: ErrorPhase,
    dispatch: DispatchStatus,
    summary: impl Into<String>,
) -> AiError {
    let summary = sanitize_error_summary(&summary.into());
    AiError::new(kind, phase, dispatch, summary).expect("plugin errors are bounded")
}

/// Safe configuration or runtime rejection from the shared provider wrapper.
#[derive(Clone, Debug)]
pub struct PluginError(String);

impl PluginError {
    /// Creates a plugin failure with a sanitized, bounded summary.
    pub fn new(summary: impl AsRef<str>) -> Self {
        Self(sanitize_error_summary(summary.as_ref()))
    }

    /// Adds a static operation context to a displayable underlying failure.
    pub fn context(context: &'static str, error: &impl fmt::Display) -> Self {
        Self::new(format!("{context}: {error}"))
    }
}

impl fmt::Display for PluginError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl std::error::Error for PluginError {}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::time::Duration;

    use rsi_ai_protocol::{ErrorKind, MediaDescriptor, MediaKind};
    use rsi_ai_provider::{AbortSignal, MediaResolver as _};

    use super::{MediaOwner, OutputCredit, PluginMediaResolver, RealtimeCommandQueue};

    #[tokio::test]
    async fn output_credit_grant_before_wait_registration_is_not_lost() {
        let credit = OutputCredit::default();
        assert!(!credit.has(1).expect("credit state"));
        credit.grant(1).expect("grant");

        tokio::time::timeout(Duration::from_millis(50), credit.changed())
            .await
            .expect("stored wake permit");
        assert!(credit.has(1).expect("credit state"));
        credit.consume(1).expect("consume");
        assert!(!credit.has(1).expect("credit state"));
    }

    #[tokio::test]
    async fn poisoned_media_store_is_an_explicit_artifact_error() {
        let resolver = PluginMediaResolver::default();
        let inner = resolver.inner.clone();
        let _ = std::thread::spawn(move || {
            let _guard = inner.lock().expect("initial media lock");
            panic!("poison media store for the regression test");
        })
        .join();
        let descriptor = MediaDescriptor::new(
            MediaKind::Image,
            "image/png",
            1,
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        )
        .expect("descriptor");

        let error = resolver
            .read(descriptor, AbortSignal::new())
            .await
            .expect_err("poisoning is not reported as a missing artifact");
        assert_eq!(error.kind(), ErrorKind::Artifact);
        assert_eq!(error.safe_summary(), "media store lock is poisoned");
    }

    #[tokio::test]
    async fn completed_call_reclaims_owned_media() {
        let resolver = PluginMediaResolver::default();
        let owner = MediaOwner::new("stream-1", "call-1");
        let descriptor = MediaDescriptor::new(
            MediaKind::Image,
            "image/png",
            1,
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        )
        .expect("descriptor");
        resolver
            .insert(&owner, &descriptor, vec![1])
            .expect("upload");
        assert_eq!(resolver.usage().expect("usage"), (1, 1));
        let first = resolver
            .read(descriptor.clone(), AbortSignal::new())
            .await
            .expect("first read");
        let second = resolver
            .read(descriptor.clone(), AbortSignal::new())
            .await
            .expect("second read");
        assert!(Arc::ptr_eq(&first, &second));

        resolver.release_call(&owner).expect("release");

        assert_eq!(resolver.usage().expect("usage"), (0, 0));
        let error = resolver
            .read(descriptor, AbortSignal::new())
            .await
            .expect_err("released media is no longer resolvable");
        assert_eq!(error.kind(), ErrorKind::Artifact);
    }

    #[test]
    fn media_resolver_debug_redacts_retained_binary_bytes() {
        let resolver = PluginMediaResolver::default();
        let owner = MediaOwner::new("stream-1", "call-1");
        let descriptor = MediaDescriptor::new(
            MediaKind::Audio,
            "audio/wav",
            4,
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        )
        .expect("descriptor");
        resolver
            .insert(&owner, &descriptor, vec![115, 101, 99, 114])
            .expect("upload");

        let debug = format!("{resolver:?}");
        assert!(!debug.contains("115, 101, 99, 114"));
        assert!(debug.contains("byte_len"));
    }

    #[test]
    fn sequential_calls_can_exceed_generation_media_budget_cumulatively() {
        let resolver = PluginMediaResolver::default();
        let bytes_per_call = 32 * 1024 * 1024;
        let descriptor = MediaDescriptor::new(
            MediaKind::Image,
            "image/png",
            bytes_per_call,
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        )
        .expect("descriptor");

        for call in 0..17 {
            let owner = MediaOwner::new("stream-1", format!("call-{call}"));
            resolver
                .insert(
                    &owner,
                    &descriptor,
                    vec![1; usize::try_from(bytes_per_call).expect("test call size fits usize")],
                )
                .expect("only live bytes count against quota");
            resolver.release_call(&owner).expect("release");
        }

        assert_eq!(resolver.usage().expect("usage"), (0, 0));
    }

    #[test]
    fn realtime_command_queue_reports_backpressure_without_blocking() {
        let (queue, _receiver) = RealtimeCommandQueue::with_capacity(1);
        queue
            .try_send(rsi_ai_protocol::RealtimeCommand::RequestResponse, None)
            .expect("first command fits");

        let error = queue
            .try_send(rsi_ai_protocol::RealtimeCommand::RequestResponse, None)
            .expect_err("a full queue rejects work synchronously");
        assert_eq!(error.to_string(), "Realtime command queue is full");
    }

    #[test]
    fn production_realtime_queue_does_not_fail_inside_one_input_credit_window() {
        let (queue, _receiver) = RealtimeCommandQueue::new();
        for _ in 0..65 {
            queue
                .try_send(rsi_ai_protocol::RealtimeCommand::RequestResponse, None)
                .expect("the protocol credit window, not a 64-item queue, owns pacing");
        }
    }
}
