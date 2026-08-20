use std::{
    collections::{BTreeMap, BTreeSet},
    time::Duration,
};

use rsi_ai_meta::{
    AiService, ClientControl, MetaIncoming, MetaServiceStream, PreparedCallSnapshot, ServerControl,
};
use rsi_ai_protocol::{
    ImageRequest, MAX_BINARY_CHUNK_BYTES, MediaDescriptor, RealtimeCloseReason, RealtimeRequest,
    SpeechRequest, TokenUsage, TranscriptionAssembler, TranscriptionOutput, TranscriptionRequest,
};
use serde_json::json;
use tokio::sync::OwnedSemaphorePermit;

use crate::host::AiRuntime;
use crate::persistence::{WriterHandle, preflight_ai_terminal};
use crate::{AgentError, AiOperationId, ArtifactRef, ArtifactStore, Result};

/// Durable artifact references returned by one image operation.
#[derive(Clone, Debug, PartialEq)]
pub struct AgentImageOutput {
    pub images: Vec<ArtifactRef>,
    pub usage: Option<TokenUsage>,
    pub prepared: PreparedCallSnapshot,
}

/// Durable audio reference returned by one speech operation.
#[derive(Clone, Debug, PartialEq)]
pub struct AgentSpeechOutput {
    pub audio: ArtifactRef,
    pub usage: Option<TokenUsage>,
    pub prepared: PreparedCallSnapshot,
}

/// A transcription and the redacted provider snapshot that produced it.
#[derive(Clone, Debug, PartialEq)]
pub struct AgentTranscriptionOutput {
    pub transcription: TranscriptionOutput,
    pub prepared: PreparedCallSnapshot,
}

/// One normalized event from the non-replayable Realtime plane.
#[derive(Clone, Debug, PartialEq)]
pub enum AgentRealtimeEvent {
    InputSpeechStarted {
        item_id: String,
    },
    InputTranscriptDelta {
        item_id: String,
        text: String,
    },
    InputTranscriptFinished {
        item_id: String,
        text: String,
    },
    OutputTextDelta {
        response_id: String,
        text: String,
    },
    OutputAudio {
        response_id: String,
        sequence: u32,
        artifact: ArtifactRef,
    },
    HandoffRequested {
        item_id: String,
        text: String,
    },
    RecoverableError {
        error: rsi_ai_protocol::AiError,
    },
    Closed {
        reason: RealtimeCloseReason,
    },
}

#[derive(Debug, Default)]
struct IncomingBlob {
    next_sequence: u32,
    bytes: Vec<u8>,
    final_seen: bool,
}

impl IncomingBlob {
    fn push(&mut self, sequence: u32, bytes: &[u8], final_chunk: bool) -> Result<()> {
        if self.next_sequence == 0 {
            self.next_sequence = 1;
        }
        if self.final_seen || sequence != self.next_sequence || bytes.len() > MAX_BINARY_CHUNK_BYTES
        {
            return Err(ai_error("receive media", "invalid output blob sequence"));
        }
        self.bytes
            .len()
            .checked_add(bytes.len())
            .filter(|size| *size <= 128 * 1024 * 1024)
            .ok_or_else(|| ai_error("receive media", "output blob exceeds its bound"))?;
        self.bytes.extend_from_slice(bytes);
        self.next_sequence = self.next_sequence.saturating_add(1);
        self.final_seen = final_chunk;
        Ok(())
    }

    fn finish(self, descriptor: &MediaDescriptor) -> Result<Vec<u8>> {
        if !self.final_seen
            || u64::try_from(self.bytes.len()).ok() != Some(descriptor.byte_len())
            || crate::digest::sha256_hex(&self.bytes) != descriptor.sha256()
        {
            return Err(ai_error(
                "verify media",
                "output blob does not match its declared descriptor",
            ));
        }
        Ok(self.bytes)
    }
}

#[allow(clippy::too_many_lines)] // Blob framing and durable terminal assembly are one invariant.
pub(crate) async fn generate_image(
    runtime: &AiRuntime,
    writer: &WriterHandle,
    artifacts: &ArtifactStore,
    operation_id: AiOperationId,
    model: String,
    request: ImageRequest,
) -> Result<AgentImageOutput> {
    let mut stream = open(runtime, AiService::Image).await?;
    let call_id = operation_id.as_str();
    let mut uploaded = BTreeSet::new();
    for descriptor in request.inputs().iter().chain(request.mask()) {
        if uploaded.insert(descriptor.sha256().to_owned()) {
            upload(&mut stream, artifacts, call_id, descriptor).await?;
        }
    }
    stream
        .send_control(&ClientControl::PrepareImage {
            call_id: call_id.to_owned(),
            model,
            request,
        })
        .await
        .map_err(meta_error("prepare image"))?;
    let prepared = receive_prepared(&mut stream, call_id).await?;
    writer
        .ai_prepared(operation_id.clone(), prepared.clone())
        .await?;
    writer.ai_started(operation_id.clone()).await?;
    writer.check_health()?;
    let result = async {
        stream
            .send_control(&ClientControl::Start {
                call_id: call_id.to_owned(),
            })
            .await
            .map_err(meta_error("start image"))?;
        let mut open_blobs = BTreeMap::<String, IncomingBlob>::new();
        let mut images = BTreeMap::<u32, ArtifactRef>::new();
        let usage = loop {
            match receive_for_call(&mut stream, "receive image", call_id).await? {
                MetaIncoming::Control(ServerControl::ImageOutputStarted { blob_id, .. }) => {
                    if open_blobs
                        .insert(blob_id, IncomingBlob::default())
                        .is_some()
                    {
                        return Err(ai_error("receive image", "duplicate image blob"));
                    }
                }
                MetaIncoming::BlobChunk {
                    blob_id,
                    sequence,
                    final_chunk,
                    bytes,
                    ..
                } => {
                    open_blobs
                        .get_mut(&blob_id)
                        .ok_or_else(|| ai_error("receive image", "undeclared image blob"))?
                        .push(sequence, &bytes, final_chunk)?;
                }
                MetaIncoming::Control(ServerControl::ImageOutputFinished {
                    index,
                    blob_id,
                    descriptor,
                    ..
                }) => {
                    let bytes = open_blobs
                        .remove(&blob_id)
                        .ok_or_else(|| ai_error("receive image", "missing image blob"))?
                        .finish(&descriptor)?;
                    let artifact = artifacts
                        .ingest(descriptor.kind(), descriptor.mime_type(), bytes)
                        .await?;
                    if artifact.descriptor() != &descriptor
                        || images.insert(index, artifact).is_some()
                    {
                        return Err(ai_error(
                            "commit image",
                            "image descriptor or index mismatch",
                        ));
                    }
                }
                MetaIncoming::Control(ServerControl::ImageFinished { usage, .. }) => break usage,
                MetaIncoming::Control(ServerControl::Failed { error, .. }) => {
                    return Err(ai_error("generate image", error.to_string()));
                }
                _ => return Err(ai_error("receive image", "unexpected image service frame")),
            }
        };
        finish(&mut stream).await?;
        Ok(AgentImageOutput {
            images: images.into_values().collect(),
            usage,
            prepared,
        })
    }
    .await;
    match result {
        Ok(output) => {
            writer
                .ai_terminal(
                    operation_id,
                    json!({
                        "status": "succeeded",
                        "result": {"images": output.images, "usage": output.usage}
                    }),
                )
                .await?;
            Ok(output)
        }
        Err(error) => {
            record_failure(writer, operation_id, &error).await?;
            Err(error)
        }
    }
}

pub(crate) async fn transcribe(
    runtime: &AiRuntime,
    writer: &WriterHandle,
    artifacts: &ArtifactStore,
    operation_id: AiOperationId,
    model: String,
    request: TranscriptionRequest,
) -> Result<(TranscriptionOutput, PreparedCallSnapshot)> {
    let mut stream = open(runtime, AiService::Transcription).await?;
    let call_id = operation_id.as_str();
    upload(&mut stream, artifacts, call_id, request.audio()).await?;
    stream
        .send_control(&ClientControl::PrepareTranscription {
            call_id: call_id.to_owned(),
            model,
            request,
        })
        .await
        .map_err(meta_error("prepare transcription"))?;
    let prepared = receive_prepared(&mut stream, call_id).await?;
    writer
        .ai_prepared(operation_id.clone(), prepared.clone())
        .await?;
    writer.ai_started(operation_id.clone()).await?;
    writer.check_health()?;
    let result = async {
        stream
            .send_control(&ClientControl::Start {
                call_id: call_id.to_owned(),
            })
            .await
            .map_err(meta_error("start transcription"))?;
        let mut assembler = TranscriptionAssembler::new();
        loop {
            match receive_for_call(&mut stream, "receive transcription", call_id).await? {
                MetaIncoming::Control(ServerControl::TranscriptionEvent { event, .. }) => {
                    let terminal =
                        matches!(event, rsi_ai_protocol::TranscriptionEvent::Finished { .. });
                    assembler
                        .push(&event)
                        .map_err(|error| ai_error("assemble transcription", error.to_string()))?;
                    if terminal {
                        break;
                    }
                }
                MetaIncoming::Control(ServerControl::Failed { error, .. }) => {
                    return Err(ai_error("transcribe", error.to_string()));
                }
                _ => {
                    return Err(ai_error(
                        "receive transcription",
                        "unexpected transcription service frame",
                    ));
                }
            }
        }
        let output = assembler
            .finish()
            .map_err(|error| ai_error("assemble transcription", error.to_string()))?;
        finish(&mut stream).await?;
        Ok((output, prepared))
    }
    .await;
    match result {
        Ok((output, prepared)) => {
            let terminal = json!({"status":"succeeded", "result":{"transcription": output}});
            if let Err(error) = preflight_ai_terminal(&terminal) {
                let error = ai_error("commit transcription", error.to_string());
                record_failure(writer, operation_id, &error).await?;
                return Err(error);
            }
            writer.ai_terminal(operation_id, terminal).await?;
            Ok((output, prepared))
        }
        Err(error) => {
            record_failure(writer, operation_id, &error).await?;
            Err(error)
        }
    }
}

#[allow(clippy::too_many_lines)] // Blob framing and durable terminal assembly are one invariant.
pub(crate) async fn synthesize(
    runtime: &AiRuntime,
    writer: &WriterHandle,
    artifacts: &ArtifactStore,
    operation_id: AiOperationId,
    model: String,
    request: SpeechRequest,
) -> Result<AgentSpeechOutput> {
    let mut stream = open(runtime, AiService::Speech).await?;
    let call_id = operation_id.as_str();
    stream
        .send_control(&ClientControl::PrepareSpeech {
            call_id: call_id.to_owned(),
            model,
            request,
        })
        .await
        .map_err(meta_error("prepare speech"))?;
    let prepared = receive_prepared(&mut stream, call_id).await?;
    writer
        .ai_prepared(operation_id.clone(), prepared.clone())
        .await?;
    writer.ai_started(operation_id.clone()).await?;
    writer.check_health()?;
    let result = async {
        stream
            .send_control(&ClientControl::Start {
                call_id: call_id.to_owned(),
            })
            .await
            .map_err(meta_error("start speech"))?;
        let mut blob = None::<(String, IncomingBlob)>;
        let mut audio = None;
        let usage = loop {
            match receive_for_call(&mut stream, "receive speech", call_id).await? {
                MetaIncoming::Control(ServerControl::SpeechOutputStarted { blob_id, .. }) => {
                    if blob.replace((blob_id, IncomingBlob::default())).is_some() {
                        return Err(ai_error("receive speech", "duplicate speech blob"));
                    }
                }
                MetaIncoming::BlobChunk {
                    blob_id,
                    sequence,
                    final_chunk,
                    bytes,
                    ..
                } => {
                    let (expected, incoming) = blob
                        .as_mut()
                        .ok_or_else(|| ai_error("receive speech", "undeclared speech blob"))?;
                    if expected != &blob_id {
                        return Err(ai_error("receive speech", "speech blob identity mismatch"));
                    }
                    incoming.push(sequence, &bytes, final_chunk)?;
                }
                MetaIncoming::Control(ServerControl::SpeechOutputFinished {
                    blob_id,
                    descriptor,
                    ..
                }) => {
                    let (expected, incoming) = blob
                        .take()
                        .ok_or_else(|| ai_error("receive speech", "missing speech blob"))?;
                    if expected != blob_id {
                        return Err(ai_error("receive speech", "speech blob identity mismatch"));
                    }
                    let bytes = incoming.finish(&descriptor)?;
                    let artifact = artifacts
                        .ingest(descriptor.kind(), descriptor.mime_type(), bytes)
                        .await?;
                    if artifact.descriptor() != &descriptor {
                        return Err(ai_error("commit speech", "speech descriptor mismatch"));
                    }
                    audio = Some(artifact);
                }
                MetaIncoming::Control(ServerControl::SpeechFinished { usage, .. }) => break usage,
                MetaIncoming::Control(ServerControl::Failed { error, .. }) => {
                    return Err(ai_error("synthesize speech", error.to_string()));
                }
                _ => {
                    return Err(ai_error(
                        "receive speech",
                        "unexpected speech service frame",
                    ));
                }
            }
        };
        finish(&mut stream).await?;
        Ok(AgentSpeechOutput {
            audio: audio
                .ok_or_else(|| ai_error("synthesize speech", "speech completed without audio"))?,
            usage,
            prepared,
        })
    }
    .await;
    match result {
        Ok(output) => {
            writer
                .ai_terminal(
                    operation_id,
                    json!({
                        "status":"succeeded",
                        "result":{"audio": output.audio, "usage": output.usage}
                    }),
                )
                .await?;
            Ok(output)
        }
        Err(error) => {
            record_failure(writer, operation_id, &error).await?;
            Err(error)
        }
    }
}

/// One live Realtime lease. Raw frames are never persisted; returned audio is committed to CAS.
pub struct AgentRealtimeSession {
    stream: MetaServiceStream,
    artifacts: ArtifactStore,
    writer: WriterHandle,
    operation_id: AiOperationId,
    call_id: String,
    session_id: String,
    prepared: PreparedCallSnapshot,
    close_timeout: Duration,
    closed: bool,
    permit: Option<OwnedSemaphorePermit>,
}

impl AgentRealtimeSession {
    pub fn session_id(&self) -> &str {
        &self.session_id
    }
    pub const fn prepared(&self) -> &PreparedCallSnapshot {
        &self.prepared
    }

    /// Sends one previously committed audio frame to the live session.
    ///
    /// # Errors
    ///
    /// Returns an error when verification, framing, or stream delivery fails.
    pub async fn append_audio(&mut self, sequence: u32, artifact: &ArtifactRef) -> Result<()> {
        let result = async {
            let bytes = self.artifacts.read(artifact).await?;
            if bytes.len() > MAX_BINARY_CHUNK_BYTES {
                return Err(ai_error(
                    "append Realtime audio",
                    "one Realtime frame exceeds the binary chunk bound",
                ));
            }
            let blob_id = format!("{}.input.{sequence}", self.call_id);
            self.stream
                .send_control(&ClientControl::RealtimeAppendAudio {
                    call_id: self.call_id.clone(),
                    blob_id: blob_id.clone(),
                    sequence,
                    descriptor: artifact.descriptor().clone(),
                })
                .await
                .map_err(meta_error("declare Realtime audio"))?;
            self.stream
                .send_blob_chunk(self.call_id.clone(), blob_id, 1, true, bytes)
                .await
                .map_err(meta_error("send Realtime audio"))
        }
        .await;
        match result {
            Ok(()) => Ok(()),
            Err(error) => self.fail(error).await,
        }
    }

    /// Appends text to the live input buffer.
    ///
    /// # Errors
    ///
    /// Returns an error when the session is closed or stream delivery fails.
    pub async fn append_text(&mut self, text: impl Into<String>) -> Result<()> {
        self.send(ClientControl::RealtimeAppendText {
            call_id: self.call_id.clone(),
            text: text.into(),
        })
        .await
    }
    /// Commits the provider's current live input item.
    ///
    /// # Errors
    ///
    /// Returns an error when the session is closed or stream delivery fails.
    pub async fn commit_input(&mut self, item_id: impl Into<String>) -> Result<()> {
        self.send(ClientControl::RealtimeCommitInput {
            call_id: self.call_id.clone(),
            item_id: item_id.into(),
        })
        .await
    }
    /// Requests a response for the committed live input.
    ///
    /// # Errors
    ///
    /// Returns an error when the session is closed or stream delivery fails.
    pub async fn request_response(&mut self) -> Result<()> {
        self.send(ClientControl::RealtimeRequestResponse {
            call_id: self.call_id.clone(),
        })
        .await
    }
    /// Cancels an in-progress live response.
    ///
    /// # Errors
    ///
    /// Returns an error when the session is closed or stream delivery fails.
    pub async fn cancel_response(&mut self, response_id: impl Into<String>) -> Result<()> {
        self.send(ClientControl::RealtimeCancelResponse {
            call_id: self.call_id.clone(),
            response_id: response_id.into(),
        })
        .await
    }

    async fn send(&mut self, control: ClientControl) -> Result<()> {
        if self.closed {
            return Err(ai_error(
                "send Realtime command",
                "Realtime session is closed",
            ));
        }
        let result = self
            .stream
            .send_control(&control)
            .await
            .map_err(meta_error("send Realtime command"));
        match result {
            Ok(()) => Ok(()),
            Err(error) => self.fail(error).await,
        }
    }

    /// Receives and validates the next event, committing output audio to CAS.
    ///
    /// # Errors
    ///
    /// Returns an error for provider failure, invalid framing, or artifact failure.
    pub async fn next_event(&mut self) -> Result<Option<AgentRealtimeEvent>> {
        let result = self.next_event_inner().await;
        match result {
            Ok(event) => Ok(event),
            Err(error) => self.fail(error).await,
        }
    }

    async fn next_event_inner(&mut self) -> Result<Option<AgentRealtimeEvent>> {
        if self.closed {
            return Ok(None);
        }
        let event =
            match receive_for_call(&mut self.stream, "receive Realtime event", &self.call_id)
                .await?
            {
                MetaIncoming::Control(ServerControl::RealtimeSpeechStarted { item_id, .. }) => {
                    AgentRealtimeEvent::InputSpeechStarted { item_id }
                }
                MetaIncoming::Control(ServerControl::RealtimeTranscriptDelta {
                    item_id,
                    text,
                    finished,
                    ..
                }) => {
                    if finished {
                        AgentRealtimeEvent::InputTranscriptFinished { item_id, text }
                    } else {
                        AgentRealtimeEvent::InputTranscriptDelta { item_id, text }
                    }
                }
                MetaIncoming::Control(ServerControl::RealtimeTextDelta {
                    response_id,
                    text,
                    ..
                }) => AgentRealtimeEvent::OutputTextDelta { response_id, text },
                MetaIncoming::Control(ServerControl::RealtimeAudio {
                    response_id,
                    sequence,
                    blob_id,
                    descriptor,
                    ..
                }) => {
                    let MetaIncoming::BlobChunk {
                        blob_id: received,
                        sequence: wire_sequence,
                        final_chunk,
                        bytes,
                        ..
                    } = receive_for_call(&mut self.stream, "receive Realtime audio", &self.call_id)
                        .await?
                    else {
                        return Err(ai_error(
                            "receive Realtime audio",
                            "audio declaration was not followed by a blob",
                        ));
                    };
                    if received != blob_id {
                        return Err(ai_error(
                            "receive Realtime audio",
                            "audio blob identity mismatch",
                        ));
                    }
                    let mut incoming = IncomingBlob::default();
                    incoming.push(wire_sequence, &bytes, final_chunk)?;
                    let bytes = incoming.finish(&descriptor)?;
                    let artifact = self
                        .artifacts
                        .ingest(descriptor.kind(), descriptor.mime_type(), bytes)
                        .await?;
                    AgentRealtimeEvent::OutputAudio {
                        response_id,
                        sequence,
                        artifact,
                    }
                }
                MetaIncoming::Control(ServerControl::RealtimeHandoffRequested {
                    item_id,
                    text,
                    ..
                }) => AgentRealtimeEvent::HandoffRequested { item_id, text },
                MetaIncoming::Control(ServerControl::RealtimeRecoverableError {
                    error, ..
                }) => AgentRealtimeEvent::RecoverableError { error },
                MetaIncoming::Control(ServerControl::RealtimeClosed { reason, .. }) => {
                    self.writer
                        .ai_terminal(
                            self.operation_id.clone(),
                            json!({"status":"succeeded", "result":{"closed": reason}}),
                        )
                        .await?;
                    self.closed = true;
                    self.permit.take();
                    AgentRealtimeEvent::Closed { reason }
                }
                MetaIncoming::Control(ServerControl::Failed { error, .. }) => {
                    return Err(ai_error("Realtime session", error.to_string()));
                }
                _ => {
                    return Err(ai_error(
                        "receive Realtime event",
                        "unexpected Realtime service frame",
                    ));
                }
            };
        Ok(Some(event))
    }

    async fn fail<T>(&mut self, error: AgentError) -> Result<T> {
        if !self.closed {
            record_failure(&self.writer, self.operation_id.clone(), &error).await?;
            self.closed = true;
            self.permit.take();
        }
        Err(error)
    }

    /// Closes the live session and drains its semantic terminal event.
    ///
    /// # Errors
    ///
    /// Returns an error when close delivery, terminal draining, or shutdown fails.
    pub async fn close(&mut self) -> Result<()> {
        let result = tokio::time::timeout(self.close_timeout, async {
            if !self.closed {
                self.send(ClientControl::RealtimeClose {
                    call_id: self.call_id.clone(),
                })
                .await?;
                while !self.closed {
                    let _ = self.next_event().await?;
                }
            }
            finish(&mut self.stream).await
        })
        .await;
        match result {
            Ok(result) => result,
            Err(_) => {
                self.fail(ai_error(
                    "close Realtime session",
                    "Realtime close deadline elapsed",
                ))
                .await
            }
        }
    }
}

impl std::fmt::Debug for AgentRealtimeSession {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("AgentRealtimeSession")
            .field("session_id", &self.session_id)
            .field("closed", &self.closed)
            .finish_non_exhaustive()
    }
}

pub(crate) async fn open_realtime(
    runtime: &AiRuntime,
    writer: WriterHandle,
    artifacts: ArtifactStore,
    operation_id: AiOperationId,
    model: String,
    request: RealtimeRequest,
    permit: OwnedSemaphorePermit,
) -> Result<AgentRealtimeSession> {
    let mut stream = open(runtime, AiService::Realtime).await?;
    let call_id = operation_id.as_str().to_owned();
    stream
        .send_control(&ClientControl::PrepareRealtime {
            call_id: call_id.clone(),
            model,
            request,
        })
        .await
        .map_err(meta_error("prepare Realtime"))?;
    let prepared = receive_prepared(&mut stream, &call_id).await?;
    writer
        .ai_prepared(operation_id.clone(), prepared.clone())
        .await?;
    writer.ai_started(operation_id.clone()).await?;
    writer.check_health()?;
    let session_id = match async {
        stream
            .send_control(&ClientControl::Start {
                call_id: call_id.clone(),
            })
            .await
            .map_err(meta_error("start Realtime"))?;
        let MetaIncoming::Control(ServerControl::RealtimeSessionStarted { session_id, .. }) =
            receive_for_call(&mut stream, "start Realtime", &call_id).await?
        else {
            return Err(ai_error(
                "start Realtime",
                "Realtime provider did not start a session",
            ));
        };
        Ok(session_id)
    }
    .await
    {
        Ok(session_id) => session_id,
        Err(error) => {
            record_failure(&writer, operation_id, &error).await?;
            return Err(error);
        }
    };
    Ok(AgentRealtimeSession {
        stream,
        artifacts,
        writer,
        operation_id,
        call_id,
        session_id,
        prepared,
        close_timeout: runtime.execution_limits.handshake_timeout(),
        closed: false,
        permit: Some(permit),
    })
}

async fn open(runtime: &AiRuntime, service: AiService) -> Result<MetaServiceStream> {
    MetaServiceStream::open(&runtime.composition, runtime.consumer.clone(), service)
        .await
        .map_err(meta_error("open AI service"))
}

async fn upload(
    stream: &mut MetaServiceStream,
    artifacts: &ArtifactStore,
    call_id: &str,
    descriptor: &MediaDescriptor,
) -> Result<()> {
    let bytes = artifacts.read_descriptor(descriptor).await?;
    let blob_id = format!("{call_id}.input.{}", &descriptor.sha256()[..16]);
    stream
        .send_control(&ClientControl::DeclareInputBlob {
            call_id: call_id.to_owned(),
            blob_id: blob_id.clone(),
            descriptor: descriptor.clone(),
        })
        .await
        .map_err(meta_error("declare input artifact"))?;
    let chunks = bytes.chunks(MAX_BINARY_CHUNK_BYTES).collect::<Vec<_>>();
    for (index, chunk) in chunks.iter().enumerate() {
        stream
            .send_blob_chunk(
                call_id.to_owned(),
                blob_id.clone(),
                u32::try_from(index + 1)
                    .map_err(|_| ai_error("upload artifact", "too many artifact chunks"))?,
                index + 1 == chunks.len(),
                chunk.to_vec(),
            )
            .await
            .map_err(meta_error("upload artifact"))?;
    }
    Ok(())
}

async fn receive_prepared(
    stream: &mut MetaServiceStream,
    call_id: &str,
) -> Result<PreparedCallSnapshot> {
    match receive(stream, "receive prepared call").await? {
        MetaIncoming::Control(ServerControl::Prepared {
            call_id: received,
            snapshot,
        }) if received == call_id => Ok(snapshot),
        MetaIncoming::Control(ServerControl::Failed {
            call_id: received,
            error,
        }) if received == call_id => Err(ai_error("prepare AI call", error.to_string())),
        _ => Err(ai_error(
            "prepare AI call",
            "provider returned an unexpected prepare frame",
        )),
    }
}

async fn receive(stream: &mut MetaServiceStream, operation: &'static str) -> Result<MetaIncoming> {
    stream
        .recv()
        .await
        .map_err(meta_error(operation))?
        .ok_or_else(|| ai_error(operation, "AI service closed without a frame"))
}

async fn receive_for_call(
    stream: &mut MetaServiceStream,
    operation: &'static str,
    call_id: &str,
) -> Result<MetaIncoming> {
    let incoming = receive(stream, operation).await?;
    let received = match &incoming {
        MetaIncoming::Control(control) => Some(control.call_id()),
        MetaIncoming::BlobChunk { call_id, .. } => Some(call_id.as_str()),
        MetaIncoming::End | MetaIncoming::Cancel { .. } => None,
    };
    if received.is_some_and(|received| received != call_id) {
        return Err(ai_error(operation, "AI response call id mismatch"));
    }
    Ok(incoming)
}

async fn finish(stream: &mut MetaServiceStream) -> Result<()> {
    stream
        .half_close()
        .await
        .map_err(meta_error("half-close AI service"))?;
    match receive(stream, "finish AI service").await? {
        MetaIncoming::End => Ok(()),
        MetaIncoming::Cancel { reason } => Err(ai_error("finish AI service", reason)),
        _ => Err(ai_error(
            "finish AI service",
            "AI service emitted data while closing",
        )),
    }
}

fn ai_error(operation: &'static str, message: impl Into<String>) -> AgentError {
    AgentError::Ai {
        operation,
        message: message.into(),
    }
}

async fn record_failure(
    writer: &WriterHandle,
    operation_id: AiOperationId,
    error: &AgentError,
) -> Result<()> {
    writer
        .ai_terminal(
            operation_id,
            json!({"status":"failed", "error": error.to_string()}),
        )
        .await
}

fn meta_error(operation: &'static str) -> impl FnOnce(rsi_ai_meta::MetaStreamError) -> AgentError {
    move |error| ai_error(operation, error.to_string())
}
