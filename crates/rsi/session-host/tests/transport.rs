#![cfg(target_os = "linux")]

use async_trait::async_trait;
use base64::Engine as _;
use futures_util::StreamExt as _;
use rsi_agent_session_protocol::{
    AgentPresetId, FrozenAgentSettings, MAXIMUM_TURN_TEXT_BYTES, MessageId, SessionFact,
    SessionFactBody, SessionHeader, SessionId, TurnId, WorkspaceTrust,
};
use rsi_agent_turn_protocol::{
    CancelResult, CancelTarget, MessageReceipt, MessageState, ObservationCursor,
    SessionObservationStream,
};
use rsi_ai_protocol::{ImageRequest, ModelRef};
use rsi_approval_protocol::{ApprovalDecision, ApprovalRequest};
use rsi_host::HostPaths;
use rsi_sandbox::SandboxMode;
use rsi_session::{
    CreateSession, MAXIMUM_SESSION_INPUT_IMAGE_BYTES, RecentSessionCursor, RecentSessionPage,
    SessionApplication, SessionApplicationError, SessionHandle, SessionHistoryPage, SessionInput,
    SessionSummary, SubmitDirectImage, SubmitInput, TurnReceipt,
};
use rsi_session_host::{
    HostEpoch, SESSION_HOST_PROTOCOL_EPOCH, SessionHostDiagnostics, SessionHostError,
    SessionHostPaths, UdsSessionApplication, UdsSessionServer, session_host_product_build,
};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::os::unix::fs::{FileTypeExt as _, PermissionsExt as _, symlink};
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use tempfile::TempDir;
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio_util::sync::CancellationToken;

const KEY: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

#[derive(Debug, Default)]
#[allow(clippy::struct_excessive_bools)] // Independent malformed-adapter modes stay explicit.
struct FakeApplication {
    sessions: Mutex<BTreeMap<SessionId, SessionHeader>>,
    submitted_inputs: Arc<Mutex<Vec<SubmitInput>>>,
    pending: Arc<Mutex<Vec<ApprovalRequest>>>,
    history_fact_count: usize,
    history_ignores_limit: bool,
    mismatched_create_header: bool,
    mismatched_attach_header: bool,
    mismatched_receipt: bool,
    submit_conflict: bool,
    panic_create: bool,
    attach_attempts: AtomicUsize,
}

impl FakeApplication {
    fn insert(&self, header: SessionHeader) {
        self.sessions
            .lock()
            .unwrap()
            .insert(header.session_id().clone(), header);
    }
}

#[async_trait]
impl SessionApplication for FakeApplication {
    async fn create(&self, request: CreateSession) -> rsi_session::Result<Arc<dyn SessionHandle>> {
        assert!(!self.panic_create, "injected connection-task panic");
        let session_id = request
            .session_id
            .unwrap_or(SessionId::new("generated-session").unwrap());
        let session_id = if self.mismatched_create_header {
            SessionId::new("wrong-created-session").unwrap()
        } else {
            session_id
        };
        let header = header(session_id, &request.cwd)
            .with_workspace_trust(request.workspace_trust)
            .unwrap();
        self.insert(header.clone());
        Ok(Arc::new(FakeHandle {
            header,
            history_fact_count: self.history_fact_count,
            history_ignores_limit: self.history_ignores_limit,
            mismatched_receipt: self.mismatched_receipt,
            submit_conflict: self.submit_conflict,
            submitted_inputs: Arc::clone(&self.submitted_inputs),
            pending: Arc::clone(&self.pending),
        }))
    }

    async fn attach(&self, session_id: &SessionId) -> rsi_session::Result<Arc<dyn SessionHandle>> {
        self.attach_attempts.fetch_add(1, Ordering::Relaxed);
        let mut durable_header = self
            .sessions
            .lock()
            .unwrap()
            .get(session_id)
            .cloned()
            .ok_or_else(|| SessionApplicationError::NotFound(session_id.to_string()))?;
        if self.mismatched_attach_header {
            durable_header = header(
                SessionId::new("wrong-attached-session").unwrap(),
                Path::new(durable_header.canonical_cwd()),
            );
        }
        Ok(Arc::new(FakeHandle {
            header: durable_header,
            history_fact_count: self.history_fact_count,
            history_ignores_limit: self.history_ignores_limit,
            mismatched_receipt: self.mismatched_receipt,
            submit_conflict: self.submit_conflict,
            submitted_inputs: Arc::clone(&self.submitted_inputs),
            pending: Arc::clone(&self.pending),
        }))
    }

    async fn list_recent(
        &self,
        _after: Option<&RecentSessionCursor>,
        limit: usize,
    ) -> rsi_session::Result<RecentSessionPage> {
        if limit == 0 {
            return Err(SessionApplicationError::Invalid("zero limit".into()));
        }
        Ok(RecentSessionPage {
            sessions: self
                .sessions
                .lock()
                .unwrap()
                .values()
                .take(limit)
                .cloned()
                .map(|header| SessionSummary { header })
                .collect(),
            has_more: false,
        })
    }
}

#[derive(Debug)]
struct FakeHandle {
    header: SessionHeader,
    history_fact_count: usize,
    history_ignores_limit: bool,
    mismatched_receipt: bool,
    submit_conflict: bool,
    submitted_inputs: Arc<Mutex<Vec<SubmitInput>>>,
    pending: Arc<Mutex<Vec<ApprovalRequest>>>,
}

#[async_trait]
impl SessionHandle for FakeHandle {
    async fn header(&self) -> rsi_session::Result<SessionHeader> {
        Ok(self.header.clone())
    }

    async fn submit(&self, request: SubmitInput) -> rsi_session::Result<MessageReceipt> {
        if self.submit_conflict {
            return Err(SessionApplicationError::MessageConflict {
                session: self.header.session_id().to_string(),
                message: request.message_id.to_string(),
            });
        }
        self.submitted_inputs.lock().unwrap().push(request.clone());
        Ok(MessageReceipt {
            session_id: if self.mismatched_receipt {
                SessionId::new("wrong-receipt-session").unwrap()
            } else {
                self.header.session_id().clone()
            },
            message_id: if self.mismatched_receipt {
                MessageId::new("wrong-receipt-message").unwrap()
            } else {
                request.message_id
            },
            accepted_control_seq: 7,
            observed_fact_seq: 0,
            state: MessageState::Pending,
        })
    }

    async fn message_status(&self, message_id: &MessageId) -> rsi_session::Result<MessageReceipt> {
        Ok(MessageReceipt {
            session_id: self.header.session_id().clone(),
            message_id: message_id.clone(),
            accepted_control_seq: 7,
            observed_fact_seq: 0,
            state: MessageState::Pending,
        })
    }

    async fn generate_image(&self, request: SubmitDirectImage) -> rsi_session::Result<TurnReceipt> {
        Ok(TurnReceipt {
            session_id: self.header.session_id().clone(),
            turn_id: request.turn_id,
            accepted_seq: 8,
        })
    }

    async fn cancel(
        &self,
        _target: CancelTarget,
        _reason: Option<String>,
    ) -> rsi_session::Result<CancelResult> {
        Ok(CancelResult {
            accepted: true,
            already_terminal: false,
        })
    }

    async fn history_before(
        &self,
        _exclusive_before_seq: Option<u64>,
        limit: usize,
    ) -> rsi_session::Result<SessionHistoryPage> {
        if limit == 0 {
            return Err(SessionApplicationError::Invalid("zero limit".into()));
        }
        let fact_count = if self.history_ignores_limit {
            self.history_fact_count
        } else {
            self.history_fact_count.min(limit)
        };
        let facts = (1..=fact_count)
            .map(|seq| {
                SessionFact::new(
                    u64::try_from(seq).unwrap(),
                    u64::try_from(seq).unwrap(),
                    SessionFactBody::TurnAccepted {
                        turn_id: TurnId::new(format!("turn-{seq}")).unwrap(),
                        text: "x".repeat(MAXIMUM_TURN_TEXT_BYTES),
                        model: None,
                        sandbox: SandboxMode::WorkspaceWrite,
                        require_approval: false,
                    },
                )
                .unwrap()
            })
            .collect();
        Ok(SessionHistoryPage {
            before_seq: u64::try_from(fact_count).unwrap() + 1,
            facts,
            durable_seq: u64::try_from(fact_count).unwrap(),
            has_more: self.history_fact_count > fact_count,
        })
    }

    async fn observe(
        &self,
        _cursor: ObservationCursor,
    ) -> rsi_session::Result<SessionObservationStream> {
        Ok(Box::pin(futures_util::stream::empty()))
    }

    async fn pending_approvals(&self) -> rsi_session::Result<Vec<ApprovalRequest>> {
        Ok(self.pending.lock().unwrap().clone())
    }

    async fn answer_approval(
        &self,
        _approval_id: &str,
        _decision: ApprovalDecision,
    ) -> rsi_session::Result<bool> {
        Ok(false)
    }
}

fn header(session_id: SessionId, cwd: &Path) -> SessionHeader {
    SessionHeader::new(
        session_id,
        1,
        cwd.to_str().unwrap(),
        AgentPresetId::new("standard").unwrap(),
        FrozenAgentSettings::new(
            "default",
            "system",
            ModelRef::new("deployment", "model").unwrap(),
            SandboxMode::WorkspaceWrite,
            false,
        )
        .unwrap(),
    )
    .unwrap()
}

fn paths(root: &TempDir) -> SessionHostPaths {
    let paths = HostPaths::new(
        root.path().join("config"),
        root.path().join("state"),
        root.path().join("cache"),
    )
    .unwrap();
    SessionHostPaths::from_host_paths_with_runtime(&paths, Some(&root.path().join("runtime")))
        .unwrap()
}

fn start(
    paths: &SessionHostPaths,
) -> (
    Arc<FakeApplication>,
    HostEpoch,
    CancellationToken,
    tokio::task::JoinHandle<Result<(), SessionHostError>>,
) {
    let application = Arc::new(FakeApplication::default());
    let epoch = HostEpoch::generate().unwrap();
    let server = UdsSessionServer::bind(
        paths,
        application.clone() as Arc<dyn SessionApplication>,
        KEY,
        epoch.clone(),
    )
    .unwrap();
    let cancellation = CancellationToken::new();
    let task = tokio::spawn(server.serve(cancellation.clone()));
    (application, epoch, cancellation, task)
}

async fn write_json_frame(stream: &mut tokio::net::UnixStream, value: &Value) {
    let bytes = serde_json::to_vec(value).unwrap();
    stream
        .write_u32(u32::try_from(bytes.len()).unwrap())
        .await
        .unwrap();
    stream.write_all(&bytes).await.unwrap();
    stream.flush().await.unwrap();
}

async fn write_json_frame_if_open(
    stream: &mut tokio::net::UnixStream,
    value: &Value,
) -> std::io::Result<()> {
    let bytes = serde_json::to_vec(value).unwrap();
    stream
        .write_u32(u32::try_from(bytes.len()).unwrap())
        .await?;
    stream.write_all(&bytes).await?;
    stream.flush().await
}

async fn read_json_frame(stream: &mut tokio::net::UnixStream) -> Value {
    let length = stream.read_u32().await.unwrap();
    let mut bytes = vec![0; usize::try_from(length).unwrap()];
    stream.read_exact(&mut bytes).await.unwrap();
    serde_json::from_slice(&bytes).unwrap()
}

async fn raw_handshake(stream: &mut tokio::net::UnixStream, epoch: &HostEpoch) {
    write_json_frame(
        stream,
        &json!({
            "type": "hello",
            "protocol_epoch": SESSION_HOST_PROTOCOL_EPOCH,
            "product_build": session_host_product_build().unwrap(),
            "launch_key": KEY,
            "host_epoch": epoch,
        }),
    )
    .await;
    assert_eq!(
        read_json_frame(stream).await.get("type").unwrap(),
        "hello_ok"
    );
}

async fn accept_and_acknowledge_hello(
    listener: &tokio::net::UnixListener,
    epoch: &HostEpoch,
) -> tokio::net::UnixStream {
    let (mut stream, _) = listener.accept().await.unwrap();
    let hello = read_json_frame(&mut stream).await;
    assert_eq!(hello.get("type").unwrap(), "hello");
    write_json_frame(
        &mut stream,
        &json!({
            "type": "hello_ok",
            "protocol_epoch": SESSION_HOST_PROTOCOL_EPOCH,
            "product_build": session_host_product_build().unwrap(),
            "launch_key": KEY,
            "host_epoch": epoch,
        }),
    )
    .await;
    stream
}

async fn wait_for_diagnostics(
    diagnostics: &SessionHostDiagnostics,
    predicate: impl Fn(rsi_session_host::SessionHostDiagnosticsSnapshot) -> bool,
) {
    tokio::time::timeout(std::time::Duration::from_secs(1), async {
        loop {
            let snapshot = diagnostics.snapshot();
            if predicate(snapshot) {
                return;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("diagnostic counter did not become observable");
}

#[tokio::test]
async fn remote_adapter_preserves_the_public_session_interface() {
    let root = TempDir::new().unwrap();
    let paths = paths(&root);
    let (_fake, epoch, cancellation, task) = start(&paths);
    let remote = UdsSessionApplication::connect(paths.socket(), KEY, epoch)
        .await
        .unwrap();
    let handle = remote
        .create(CreateSession {
            cwd: root.path().to_owned(),
            session_id: Some(SessionId::new("remote-session").unwrap()),
            agent_preset_id: None,
            workspace_trust: WorkspaceTrust::Untrusted,
        })
        .await
        .unwrap();
    assert_eq!(
        handle.header().await.unwrap().session_id().as_str(),
        "remote-session"
    );
    assert!(
        handle
            .history_before(None, 8)
            .await
            .unwrap()
            .facts
            .is_empty()
    );
    assert!(handle.pending_approvals().await.unwrap().is_empty());
    assert!(
        !handle
            .answer_approval("missing", ApprovalDecision::Deny)
            .await
            .unwrap()
    );
    assert!(matches!(
        handle
            .submit(SubmitInput {
                message_id: MessageId::new("oversized-message").unwrap(),
                content: vec![SessionInput::Text {
                    text: "x".repeat(MAXIMUM_TURN_TEXT_BYTES + 1),
                }],
                model: None,
                sandbox: None,
            })
            .await,
        Err(SessionApplicationError::Invalid(message)) if message.contains("message text")
    ));
    let mut observation = handle.observe(ObservationCursor::default()).await.unwrap();
    assert!(observation.next().await.is_none());
    let receipt = handle
        .submit(SubmitInput {
            message_id: MessageId::new("message-1").unwrap(),
            content: vec![SessionInput::Text {
                text: "hello".into(),
            }],
            model: None,
            sandbox: None,
        })
        .await
        .unwrap();
    assert_eq!(receipt.accepted_control_seq, 7);
    assert_eq!(
        handle
            .message_status(&MessageId::new("message-1").unwrap())
            .await
            .unwrap(),
        receipt
    );
    let image_receipt = handle
        .generate_image(SubmitDirectImage {
            turn_id: TurnId::new("image-turn-1").unwrap(),
            model: ModelRef::new("deployment", "image-model").unwrap(),
            request: ImageRequest::new("draw one square", 1).unwrap(),
        })
        .await
        .unwrap();
    assert_eq!(image_receipt.accepted_seq, 8);
    assert_eq!(
        remote.list_recent(None, 10).await.unwrap().sessions.len(),
        1
    );
    let cancelled = handle
        .cancel(
            CancelTarget::Turn(TurnId::new("turn-1").unwrap()),
            Some("test".into()),
        )
        .await
        .unwrap();
    assert!(cancelled.accepted);

    cancellation.cancel();
    task.await.unwrap().unwrap();
    assert!(!paths.socket().exists());
}

#[tokio::test]
async fn multimodal_upload_reconstructs_ordered_exact_bodies_across_chunks() {
    let root = TempDir::new().unwrap();
    let paths = paths(&root);
    let (fake, epoch, cancellation, task) = start(&paths);
    let remote = UdsSessionApplication::connect(paths.socket(), KEY, epoch)
        .await
        .unwrap();
    let handle = remote
        .create(CreateSession {
            cwd: root.path().to_owned(),
            session_id: Some(SessionId::new("multimodal-upload").unwrap()),
            agent_preset_id: None,
            workspace_trust: WorkspaceTrust::Trusted,
        })
        .await
        .unwrap();
    assert_eq!(
        handle.header().await.unwrap().workspace_trust(),
        WorkspaceTrust::Trusted
    );
    let first = Arc::<[u8]>::from(vec![0x5a; 48 * 1024 + 17]);
    let second = Arc::<[u8]>::from(vec![0xa5; 97]);
    let receipt = handle
        .submit(SubmitInput {
            message_id: MessageId::new("multimodal-message").unwrap(),
            content: vec![
                SessionInput::Text {
                    text: "look".into(),
                },
                SessionInput::Image {
                    bytes: Arc::clone(&first),
                },
                SessionInput::Image {
                    bytes: Arc::clone(&second),
                },
            ],
            model: None,
            sandbox: None,
        })
        .await
        .unwrap();
    assert_eq!(receipt.accepted_control_seq, 7);

    {
        let submitted = fake.submitted_inputs.lock().unwrap();
        assert_eq!(submitted.len(), 1);
        assert!(matches!(
            submitted[0].content.as_slice(),
            [
                SessionInput::Text { text },
                SessionInput::Image { bytes: observed_first },
                SessionInput::Image { bytes: observed_second },
            ] if text == "look"
                && observed_first.as_ref() == first.as_ref()
                && observed_second.as_ref() == second.as_ref()
        ));
    }

    cancellation.cancel();
    task.await.unwrap().unwrap();
}

#[tokio::test]
#[allow(clippy::too_many_lines)] // Keep every malformed upload case beside the shared typed-response assertion.
async fn malformed_uploads_return_typed_errors_before_the_application_observes_input() {
    let root = TempDir::new().unwrap();
    let paths = paths(&root);
    let (fake, epoch, cancellation, task) = start(&paths);
    let remote = UdsSessionApplication::connect(paths.socket(), KEY, epoch.clone())
        .await
        .unwrap();
    remote
        .create(CreateSession {
            cwd: root.path().to_owned(),
            session_id: Some(SessionId::new("malformed-upload").unwrap()),
            agent_preset_id: None,
            workspace_trust: WorkspaceTrust::Untrusted,
        })
        .await
        .unwrap();

    let valid_chunk = base64::engine::general_purpose::STANDARD.encode(b"abc");
    let cases = [
        (
            "digest",
            3_u64,
            vec![
                json!({"type":"upload_chunk","request_id":"raw-digest","upload_id":0,"index":0,"data":valid_chunk.clone()}),
                json!({"type":"upload_end","request_id":"raw-digest"}),
            ],
        ),
        (
            "index",
            3,
            vec![
                json!({"type":"upload_chunk","request_id":"raw-index","upload_id":0,"index":1,"data":valid_chunk}),
                json!({"type":"upload_end","request_id":"raw-index"}),
            ],
        ),
        (
            "base64",
            3,
            vec![
                json!({"type":"upload_chunk","request_id":"raw-base64","upload_id":0,"index":0,"data":"!!!!"}),
                json!({"type":"upload_end","request_id":"raw-base64"}),
            ],
        ),
        (
            "chunk",
            48 * 1024 + 1,
            vec![
                json!({"type":"upload_chunk","request_id":"raw-chunk","upload_id":0,"index":0,"data":"A".repeat(64 * 1024 + 4)}),
                json!({"type":"upload_end","request_id":"raw-chunk"}),
            ],
        ),
        (
            "aggregate",
            u64::try_from(MAXIMUM_SESSION_INPUT_IMAGE_BYTES).unwrap() + 1,
            Vec::new(),
        ),
    ];

    for (case, declared_bytes, frames) in cases {
        let request_id = format!("raw-{case}");
        let mut stream = tokio::net::UnixStream::connect(paths.socket())
            .await
            .unwrap();
        raw_handshake(&mut stream, &epoch).await;
        write_json_frame(
            &mut stream,
            &json!({
                "type": "request",
                "request_id": request_id,
                "operation": {
                    "type": "submit_input",
                    "session_id": "malformed-upload",
                    "message_id": format!("message-{case}"),
                    "content": [{
                        "type": "image",
                        "upload_id": 0,
                        "bytes": declared_bytes,
                        "sha256": "0".repeat(64),
                    }],
                    "model": null,
                    "sandbox": null,
                },
            }),
        )
        .await;
        for frame in frames {
            if write_json_frame_if_open(&mut stream, &frame).await.is_err() {
                break;
            }
        }
        let response = tokio::time::timeout(
            std::time::Duration::from_secs(1),
            read_json_frame(&mut stream),
        )
        .await
        .expect("malformed upload must receive a typed response");
        assert_eq!(response["type"], "response", "case {case}");
        assert_eq!(response["request_id"], request_id, "case {case}");
        assert!(response["response"].is_null(), "case {case}");
        assert_eq!(response["error"]["kind"], "invalid", "case {case}");
    }
    assert!(fake.submitted_inputs.lock().unwrap().is_empty());

    cancellation.cancel();
    task.await.unwrap().unwrap();
}

#[tokio::test]
async fn readiness_probe_does_not_touch_the_session_application() {
    let root = TempDir::new().unwrap();
    let paths = paths(&root);
    let (fake, epoch, cancellation, task) = start(&paths);

    UdsSessionApplication::connect(paths.socket(), KEY, epoch)
        .await
        .unwrap();

    assert!(fake.sessions.lock().unwrap().is_empty());
    assert_eq!(fake.attach_attempts.load(Ordering::Relaxed), 0);
    cancellation.cancel();
    task.await.unwrap().unwrap();
}

#[tokio::test]
async fn readiness_requires_the_matching_ready_response() {
    let root = TempDir::new().unwrap();
    let socket = root.path().join("wrong-ready.sock");
    let listener = tokio::net::UnixListener::bind(&socket).unwrap();
    let epoch = HostEpoch::generate().unwrap();
    let server_epoch = epoch.clone();
    let server = tokio::spawn(async move {
        let mut stream = accept_and_acknowledge_hello(&listener, &server_epoch).await;
        let request = read_json_frame(&mut stream).await;
        assert_eq!(request["operation"]["type"], "probe");
        write_json_frame(
            &mut stream,
            &json!({
                "type": "response",
                "request_id": request["request_id"],
                "response": { "type": "subscribed" },
                "error": null,
            }),
        )
        .await;
    });

    let error = UdsSessionApplication::connect(&socket, KEY, epoch)
        .await
        .unwrap_err();
    assert!(error.to_string().contains("wrong readiness response"));
    server.await.unwrap();
}

#[tokio::test]
async fn readiness_rejects_a_missing_response_after_a_valid_handshake() {
    let root = TempDir::new().unwrap();
    let socket = root.path().join("missing-ready.sock");
    let listener = tokio::net::UnixListener::bind(&socket).unwrap();
    let epoch = HostEpoch::generate().unwrap();
    let server_epoch = epoch.clone();
    let server = tokio::spawn(async move {
        let mut stream = accept_and_acknowledge_hello(&listener, &server_epoch).await;
        let request = read_json_frame(&mut stream).await;
        assert_eq!(request["operation"]["type"], "probe");
    });

    assert!(
        UdsSessionApplication::connect(&socket, KEY, epoch)
            .await
            .is_err()
    );
    server.await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn readiness_probe_wait_is_bounded_after_a_valid_handshake() {
    let root = TempDir::new().unwrap();
    let socket = root.path().join("stalled-ready.sock");
    let listener = tokio::net::UnixListener::bind(&socket).unwrap();
    let epoch = HostEpoch::generate().unwrap();
    let server_epoch = epoch.clone();
    let server = tokio::spawn(async move {
        let mut stream = accept_and_acknowledge_hello(&listener, &server_epoch).await;
        let request = read_json_frame(&mut stream).await;
        assert_eq!(request["operation"]["type"], "probe");
        std::future::pending::<()>().await;
    });
    let error = UdsSessionApplication::connect(&socket, KEY, epoch)
        .await
        .unwrap_err();

    assert!(error.to_string().contains("readiness probe timed out"));
    server.abort();
}

#[tokio::test]
async fn diagnostics_classify_rejections_malformed_requests_and_task_panics() {
    let root = TempDir::new().unwrap();
    let paths = paths(&root);
    let application = Arc::new(FakeApplication {
        panic_create: true,
        ..FakeApplication::default()
    });
    let epoch = HostEpoch::generate().unwrap();
    let server = UdsSessionServer::bind(
        &paths,
        application as Arc<dyn SessionApplication>,
        KEY,
        epoch.clone(),
    )
    .unwrap();
    let diagnostics = server.diagnostics();
    let cancellation = CancellationToken::new();
    let task = tokio::spawn(server.serve(cancellation.clone()));

    let rejected = UdsSessionApplication::connect(paths.socket(), "b".repeat(64), epoch.clone())
        .await
        .unwrap_err();
    assert!(rejected.to_string().contains("handshake rejected"));

    let mut malformed = tokio::net::UnixStream::connect(paths.socket())
        .await
        .unwrap();
    raw_handshake(&mut malformed, &epoch).await;
    malformed.write_all(&[0, 0, 0, 0]).await.unwrap();
    malformed.shutdown().await.unwrap();

    let remote = UdsSessionApplication::connect(paths.socket(), KEY, epoch)
        .await
        .unwrap();
    let create = remote
        .create(CreateSession {
            cwd: root.path().to_owned(),
            session_id: Some(SessionId::new("panic-session").unwrap()),
            agent_preset_id: None,
            workspace_trust: WorkspaceTrust::Untrusted,
        })
        .await;
    assert!(create.is_err());

    wait_for_diagnostics(&diagnostics, |snapshot| {
        snapshot.handshake_rejections == 1
            && snapshot.request_failures == 1
            && snapshot.connection_task_panics == 1
    })
    .await;
    cancellation.cancel();
    task.await.unwrap().unwrap();
}

#[tokio::test]
async fn server_rejects_a_relative_wire_workspace_before_application_create() {
    let root = TempDir::new().unwrap();
    let paths = paths(&root);
    let (fake, epoch, cancellation, task) = start(&paths);
    let mut stream = tokio::net::UnixStream::connect(paths.socket())
        .await
        .unwrap();
    raw_handshake(&mut stream, &epoch).await;
    write_json_frame(
        &mut stream,
        &json!({
            "type": "request",
            "request_id": "relative-wire-path",
            "operation": {
                "type": "create", "cwd": ".", "session_id": "relative-wire-session",
                "agent_preset_id": null, "workspace_trust": "untrusted"
            }
        }),
    )
    .await;
    let response = read_json_frame(&mut stream).await;
    assert_eq!(response["error"]["kind"], "invalid");
    assert!(response["error"].to_string().contains("absolute"));
    assert!(fake.sessions.lock().unwrap().is_empty());
    cancellation.cancel();
    task.await.unwrap().unwrap();
}

#[tokio::test]
async fn remote_adapter_resolves_relative_workspace_before_transport() {
    let root = TempDir::new().unwrap();
    let paths = paths(&root);
    let (_fake, epoch, cancellation, task) = start(&paths);
    let remote = UdsSessionApplication::connect(paths.socket(), KEY, epoch)
        .await
        .unwrap();
    let current = std::env::current_dir().unwrap();
    let workspace = tempfile::tempdir_in(&current).unwrap();
    let relative = workspace.path().strip_prefix(&current).unwrap().to_owned();

    let handle = remote
        .create(CreateSession {
            cwd: relative,
            session_id: Some(SessionId::new("relative-workspace").unwrap()),
            agent_preset_id: None,
            workspace_trust: WorkspaceTrust::Untrusted,
        })
        .await
        .unwrap();

    assert_eq!(
        handle.header().await.unwrap().canonical_cwd(),
        workspace.path().canonicalize().unwrap().to_str().unwrap()
    );
    cancellation.cancel();
    task.await.unwrap().unwrap();
}

#[tokio::test]
async fn history_larger_than_one_wire_frame_is_delivered_one_fact_at_a_time() {
    const FACT_COUNT: usize = 37;

    let root = TempDir::new().unwrap();
    let paths = paths(&root);
    let application = Arc::new(FakeApplication {
        history_fact_count: FACT_COUNT,
        ..FakeApplication::default()
    });
    let epoch = HostEpoch::generate().unwrap();
    let server = UdsSessionServer::bind(
        &paths,
        application.clone() as Arc<dyn SessionApplication>,
        KEY,
        epoch.clone(),
    )
    .unwrap();
    let cancellation = CancellationToken::new();
    let task = tokio::spawn(server.serve(cancellation.clone()));
    let remote = UdsSessionApplication::connect(paths.socket(), KEY, epoch)
        .await
        .unwrap();
    let handle = remote
        .create(CreateSession {
            cwd: root.path().to_owned(),
            session_id: Some(SessionId::new("large-history").unwrap()),
            agent_preset_id: None,
            workspace_trust: WorkspaceTrust::Untrusted,
        })
        .await
        .unwrap();

    let page = handle.history_before(None, FACT_COUNT).await.unwrap();
    assert_eq!(page.facts.len(), FACT_COUNT);
    assert!(
        page.facts
            .iter()
            .map(SessionFact::encoded_len)
            .sum::<usize>()
            > 36 * 1024 * 1024 + 64 * 1024
    );

    cancellation.cancel();
    task.await.unwrap().unwrap();
}

#[tokio::test]
async fn handshake_rejects_a_different_launch_generation() {
    let root = TempDir::new().unwrap();
    let paths = paths(&root);
    let (_fake, epoch, cancellation, task) = start(&paths);
    let error = UdsSessionApplication::connect(paths.socket(), "b".repeat(64), epoch)
        .await
        .unwrap_err();
    assert!(error.to_string().contains("handshake rejected"));
    cancellation.cancel();
    task.await.unwrap().unwrap();
}

#[tokio::test]
async fn client_handshake_with_an_unresponsive_server_is_bounded() {
    let root = TempDir::new().unwrap();
    let socket = root.path().join("unresponsive.sock");
    let listener = tokio::net::UnixListener::bind(&socket).unwrap();
    let server = tokio::spawn(async move {
        let (_stream, _) = listener.accept().await.unwrap();
        std::future::pending::<()>().await;
    });
    let error = UdsSessionApplication::connect(&socket, KEY, HostEpoch::generate().unwrap())
        .await
        .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("server handshake read timed out")
    );
    server.abort();
}

#[tokio::test]
async fn oversized_first_frame_is_rejected_without_allocating_its_body() {
    let root = TempDir::new().unwrap();
    let paths = paths(&root);
    let (_fake, _epoch, cancellation, task) = start(&paths);
    let mut stream = tokio::net::UnixStream::connect(paths.socket())
        .await
        .unwrap();
    stream.write_u32(u32::MAX).await.unwrap();
    let mut byte = [0_u8; 1];
    let read = tokio::time::timeout(std::time::Duration::from_secs(1), stream.read(&mut byte))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(read, 0);
    cancellation.cancel();
    task.await.unwrap().unwrap();
}

#[tokio::test]
async fn handshake_uses_its_small_control_frame_bound() {
    let root = TempDir::new().unwrap();
    let paths = paths(&root);
    let (_fake, _epoch, cancellation, task) = start(&paths);
    let mut stream = tokio::net::UnixStream::connect(paths.socket())
        .await
        .unwrap();
    stream.write_u32(1024 * 1024).await.unwrap();
    let mut byte = [0_u8; 1];
    let read = tokio::time::timeout(std::time::Duration::from_secs(1), stream.read(&mut byte))
        .await
        .expect("handshake framing must reject before waiting for a large body")
        .unwrap();
    assert_eq!(read, 0);
    cancellation.cancel();
    task.await.unwrap().unwrap();
}

#[tokio::test]
async fn draft_capacity_is_reserved_before_application_create() {
    let root = TempDir::new().unwrap();
    let paths = paths(&root);
    let (fake, epoch, cancellation, task) = start(&paths);
    let remote = UdsSessionApplication::connect(paths.socket(), KEY, epoch)
        .await
        .unwrap();

    for index in 0..1024 {
        remote
            .create(CreateSession {
                cwd: root.path().to_owned(),
                session_id: Some(SessionId::new(format!("reserved-draft-{index}")).unwrap()),
                agent_preset_id: None,
                workspace_trust: WorkspaceTrust::Untrusted,
            })
            .await
            .unwrap();
    }
    let overflow = SessionId::new("draft-capacity-overflow").unwrap();
    assert!(matches!(
        remote
            .create(CreateSession {
                cwd: root.path().to_owned(),
                session_id: Some(overflow.clone()),
                agent_preset_id: None,
                workspace_trust: WorkspaceTrust::Untrusted,
            })
            .await,
        Err(SessionApplicationError::Capacity)
    ));
    assert!(!fake.sessions.lock().unwrap().contains_key(&overflow));

    cancellation.cancel();
    task.await.unwrap().unwrap();
}

#[tokio::test]
async fn client_rejects_create_and_attach_headers_for_another_session() {
    for attach in [false, true] {
        let root = TempDir::new().unwrap();
        let paths = paths(&root);
        let application = Arc::new(FakeApplication {
            mismatched_create_header: !attach,
            mismatched_attach_header: attach,
            ..FakeApplication::default()
        });
        let expected = SessionId::new(if attach {
            "expected-attached-session"
        } else {
            "expected-created-session"
        })
        .unwrap();
        if attach {
            application.insert(header(expected.clone(), root.path()));
        }
        let epoch = HostEpoch::generate().unwrap();
        let server = UdsSessionServer::bind(
            &paths,
            application as Arc<dyn SessionApplication>,
            KEY,
            epoch.clone(),
        )
        .unwrap();
        let cancellation = CancellationToken::new();
        let task = tokio::spawn(server.serve(cancellation.clone()));
        let remote = UdsSessionApplication::connect(paths.socket(), KEY, epoch)
            .await
            .unwrap();
        let result = if attach {
            remote.attach(&expected).await
        } else {
            remote
                .create(CreateSession {
                    cwd: root.path().to_owned(),
                    session_id: Some(expected),
                    agent_preset_id: None,
                    workspace_trust: WorkspaceTrust::Untrusted,
                })
                .await
        };
        assert!(
            matches!(result, Err(SessionApplicationError::Backend(message)) if message.contains("identity"))
        );
        cancellation.cancel();
        task.await.unwrap().unwrap();
    }
}

#[tokio::test]
async fn client_rejects_a_receipt_for_another_session_or_message() {
    let root = TempDir::new().unwrap();
    let paths = paths(&root);
    let application = Arc::new(FakeApplication {
        mismatched_receipt: true,
        ..FakeApplication::default()
    });
    let epoch = HostEpoch::generate().unwrap();
    let server = UdsSessionServer::bind(
        &paths,
        application as Arc<dyn SessionApplication>,
        KEY,
        epoch.clone(),
    )
    .unwrap();
    let cancellation = CancellationToken::new();
    let task = tokio::spawn(server.serve(cancellation.clone()));
    let remote = UdsSessionApplication::connect(paths.socket(), KEY, epoch)
        .await
        .unwrap();
    let handle = remote
        .create(CreateSession {
            cwd: root.path().to_owned(),
            session_id: Some(SessionId::new("receipt-session").unwrap()),
            agent_preset_id: None,
            workspace_trust: WorkspaceTrust::Untrusted,
        })
        .await
        .unwrap();
    assert!(matches!(
        handle
            .submit(SubmitInput {
                message_id: MessageId::new("receipt-message").unwrap(),
                content: vec![SessionInput::Text {
                    text: "hello".into(),
                }],
                model: None,
                sandbox: None,
            })
            .await,
        Err(SessionApplicationError::Backend(message)) if message.contains("identity")
    ));
    cancellation.cancel();
    task.await.unwrap().unwrap();
}

#[tokio::test(start_paused = true)]
async fn response_timeout_reports_the_exact_unknown_message_identity() {
    let root = TempDir::new().unwrap();
    let socket = root.path().join("unknown-outcome.sock");
    let listener = tokio::net::UnixListener::bind(&socket).unwrap();
    let epoch = HostEpoch::generate().unwrap();
    let server_epoch = epoch.clone();
    let session_id = SessionId::new("unknown-outcome-session").unwrap();
    let message_id = MessageId::new("unknown-outcome-message").unwrap();
    let response_header = header(session_id.clone(), root.path());
    let expected_message_id = message_id.clone();
    let server = tokio::spawn(async move {
        let mut probe = accept_and_acknowledge_hello(&listener, &server_epoch).await;
        let request = read_json_frame(&mut probe).await;
        write_json_frame(
            &mut probe,
            &json!({
                "type": "response",
                "request_id": request["request_id"],
                "response": { "type": "ready" },
                "error": null,
            }),
        )
        .await;

        let mut create = accept_and_acknowledge_hello(&listener, &server_epoch).await;
        let request = read_json_frame(&mut create).await;
        write_json_frame(
            &mut create,
            &json!({
                "type": "response",
                "request_id": request["request_id"],
                "response": { "type": "session", "header": response_header },
                "error": null,
            }),
        )
        .await;

        let mut submit = accept_and_acknowledge_hello(&listener, &server_epoch).await;
        let request = read_json_frame(&mut submit).await;
        assert_eq!(
            request["operation"]["message_id"],
            expected_message_id.as_str()
        );
        std::future::pending::<()>().await;
    });
    let remote = UdsSessionApplication::connect(&socket, KEY, epoch)
        .await
        .unwrap();
    let handle = remote
        .create(CreateSession {
            cwd: root.path().to_owned(),
            session_id: Some(session_id.clone()),
            agent_preset_id: None,
            workspace_trust: WorkspaceTrust::Untrusted,
        })
        .await
        .unwrap();
    let submit = tokio::spawn({
        let handle = Arc::clone(&handle);
        let message_id = message_id.clone();
        async move {
            handle
                .submit(SubmitInput {
                    message_id,
                    content: vec![SessionInput::Text {
                        text: "may commit after timeout".into(),
                    }],
                    model: None,
                    sandbox: None,
                })
                .await
        }
    });
    tokio::task::yield_now().await;
    tokio::time::advance(std::time::Duration::from_secs(31)).await;
    tokio::task::yield_now().await;

    assert!(matches!(
        submit.await.unwrap(),
        Err(SessionApplicationError::MessageOutcomeUnknown { session, message })
            if session == session_id.as_str() && message == message_id.as_str()
    ));
    server.abort();
}

#[tokio::test]
async fn invalid_matching_response_envelope_reports_the_exact_unknown_message_identity() {
    let root = TempDir::new().unwrap();
    let socket = root.path().join("invalid-message-envelope.sock");
    let listener = tokio::net::UnixListener::bind(&socket).unwrap();
    let epoch = HostEpoch::generate().unwrap();
    let server_epoch = epoch.clone();
    let session_id = SessionId::new("invalid-envelope-session").unwrap();
    let message_id = MessageId::new("invalid-envelope-message").unwrap();
    let response_header = header(session_id.clone(), root.path());
    let expected_message_id = message_id.clone();
    let server = tokio::spawn(async move {
        let mut probe = accept_and_acknowledge_hello(&listener, &server_epoch).await;
        let request = read_json_frame(&mut probe).await;
        write_json_frame(
            &mut probe,
            &json!({
                "type": "response",
                "request_id": request["request_id"],
                "response": { "type": "ready" },
                "error": null,
            }),
        )
        .await;

        let mut create = accept_and_acknowledge_hello(&listener, &server_epoch).await;
        let request = read_json_frame(&mut create).await;
        write_json_frame(
            &mut create,
            &json!({
                "type": "response",
                "request_id": request["request_id"],
                "response": { "type": "session", "header": response_header },
                "error": null,
            }),
        )
        .await;

        let mut submit = accept_and_acknowledge_hello(&listener, &server_epoch).await;
        let request = read_json_frame(&mut submit).await;
        assert_eq!(
            request["operation"]["message_id"],
            expected_message_id.as_str()
        );
        write_json_frame(
            &mut submit,
            &json!({
                "type": "response",
                "request_id": request["request_id"],
                "response": null,
                "error": null,
            }),
        )
        .await;
    });
    let remote = UdsSessionApplication::connect(&socket, KEY, epoch)
        .await
        .unwrap();
    let handle = remote
        .create(CreateSession {
            cwd: root.path().to_owned(),
            session_id: Some(session_id.clone()),
            agent_preset_id: None,
            workspace_trust: WorkspaceTrust::Untrusted,
        })
        .await
        .unwrap();

    assert!(matches!(
        handle
            .submit(SubmitInput {
                message_id: message_id.clone(),
                content: vec![SessionInput::Text {
                    text: "may already be committed".into(),
                }],
                model: None,
                sandbox: None,
            })
            .await,
        Err(SessionApplicationError::MessageOutcomeUnknown { session, message })
            if session == session_id.as_str() && message == message_id.as_str()
    ));
    server.await.unwrap();
}

#[tokio::test]
async fn connection_failure_before_message_transmission_remains_a_backend_error() {
    let root = TempDir::new().unwrap();
    let socket = root.path().join("pre-send-failure.sock");
    let listener = tokio::net::UnixListener::bind(&socket).unwrap();
    let epoch = HostEpoch::generate().unwrap();
    let server_epoch = epoch.clone();
    let session_id = SessionId::new("pre-send-failure-session").unwrap();
    let response_header = header(session_id.clone(), root.path());
    let server = tokio::spawn(async move {
        let mut probe = accept_and_acknowledge_hello(&listener, &server_epoch).await;
        let request = read_json_frame(&mut probe).await;
        write_json_frame(
            &mut probe,
            &json!({
                "type": "response",
                "request_id": request["request_id"],
                "response": { "type": "ready" },
                "error": null,
            }),
        )
        .await;

        let mut create = accept_and_acknowledge_hello(&listener, &server_epoch).await;
        let request = read_json_frame(&mut create).await;
        write_json_frame(
            &mut create,
            &json!({
                "type": "response",
                "request_id": request["request_id"],
                "response": { "type": "session", "header": response_header },
                "error": null,
            }),
        )
        .await;
    });
    let remote = UdsSessionApplication::connect(&socket, KEY, epoch)
        .await
        .unwrap();
    let handle = remote
        .create(CreateSession {
            cwd: root.path().to_owned(),
            session_id: Some(session_id),
            agent_preset_id: None,
            workspace_trust: WorkspaceTrust::Untrusted,
        })
        .await
        .unwrap();
    server.await.unwrap();

    assert!(matches!(
        handle
            .submit(SubmitInput {
                message_id: MessageId::new("pre-send-failure-message").unwrap(),
                content: vec![SessionInput::Text {
                    text: "never transmitted".into(),
                }],
                model: None,
                sandbox: None,
            })
            .await,
        Err(SessionApplicationError::Backend(_))
    ));
}

#[tokio::test]
async fn client_enforces_history_count_and_aggregate_byte_bounds() {
    for fact_count in [2, 65] {
        let root = TempDir::new().unwrap();
        let paths = paths(&root);
        let application = Arc::new(FakeApplication {
            history_fact_count: fact_count,
            history_ignores_limit: fact_count == 2,
            ..FakeApplication::default()
        });
        let epoch = HostEpoch::generate().unwrap();
        let server = UdsSessionServer::bind(
            &paths,
            application as Arc<dyn SessionApplication>,
            KEY,
            epoch.clone(),
        )
        .unwrap();
        let cancellation = CancellationToken::new();
        let task = tokio::spawn(server.serve(cancellation.clone()));
        let remote = UdsSessionApplication::connect(paths.socket(), KEY, epoch)
            .await
            .unwrap();
        let handle = remote
            .create(CreateSession {
                cwd: root.path().to_owned(),
                session_id: Some(SessionId::new(format!("bounded-history-{fact_count}")).unwrap()),
                agent_preset_id: None,
                workspace_trust: WorkspaceTrust::Untrusted,
            })
            .await
            .unwrap();
        let limit = if fact_count == 2 { 1 } else { 256 };
        assert!(matches!(
            handle.history_before(None, limit).await,
            Err(SessionApplicationError::Backend(message)) if message.contains("bound")
        ));
        cancellation.cancel();
        task.await.unwrap().unwrap();
    }
}

#[tokio::test]
async fn submission_conflict_releases_the_unpublished_draft_slot() {
    let root = TempDir::new().unwrap();
    let paths = paths(&root);
    let application = Arc::new(FakeApplication {
        submit_conflict: true,
        ..FakeApplication::default()
    });
    let epoch = HostEpoch::generate().unwrap();
    let server = UdsSessionServer::bind(
        &paths,
        application as Arc<dyn SessionApplication>,
        KEY,
        epoch.clone(),
    )
    .unwrap();
    let cancellation = CancellationToken::new();
    let task = tokio::spawn(server.serve(cancellation.clone()));
    let remote = UdsSessionApplication::connect(paths.socket(), KEY, epoch)
        .await
        .unwrap();

    for index in 0..1024 {
        let handle = remote
            .create(CreateSession {
                cwd: root.path().to_owned(),
                session_id: Some(SessionId::new(format!("conflicted-draft-{index}")).unwrap()),
                agent_preset_id: None,
                workspace_trust: WorkspaceTrust::Untrusted,
            })
            .await
            .unwrap();
        assert!(matches!(
            handle
                .submit(SubmitInput {
                    message_id: MessageId::new(format!("conflicted-message-{index}")).unwrap(),
                    content: vec![SessionInput::Text {
                        text: "conflict".into(),
                    }],
                    model: None,
                    sandbox: None,
                })
                .await,
            Err(SessionApplicationError::MessageConflict { .. })
        ));
    }
    remote
        .create(CreateSession {
            cwd: root.path().to_owned(),
            session_id: Some(SessionId::new("after-conflicts").unwrap()),
            agent_preset_id: None,
            workspace_trust: WorkspaceTrust::Untrusted,
        })
        .await
        .expect("conflicted drafts must not retain server capacity");

    cancellation.cancel();
    task.await.unwrap().unwrap();
}

#[tokio::test]
async fn client_that_never_sends_hello_cannot_hold_a_connection_slot_forever() {
    let root = TempDir::new().unwrap();
    let paths = paths(&root);
    let (_fake, _epoch, cancellation, task) = start(&paths);
    let mut stream = tokio::net::UnixStream::connect(paths.socket())
        .await
        .unwrap();
    let mut byte = [0_u8; 1];
    let read = tokio::time::timeout(std::time::Duration::from_secs(5), stream.read(&mut byte))
        .await
        .expect("the server must bound the initial handshake read")
        .unwrap();
    assert_eq!(read, 0);
    cancellation.cancel();
    task.await.unwrap().unwrap();
}

#[tokio::test(start_paused = true)]
async fn abandoned_unpublished_drafts_expire_and_release_capacity() {
    let root = TempDir::new().unwrap();
    let paths = paths(&root);
    let (_fake, epoch, cancellation, task) = start(&paths);
    let remote = UdsSessionApplication::connect(paths.socket(), KEY, epoch)
        .await
        .unwrap();

    for index in 0..1024 {
        let handle = remote
            .create(CreateSession {
                cwd: root.path().to_owned(),
                session_id: Some(SessionId::new(format!("abandoned-draft-{index}")).unwrap()),
                agent_preset_id: None,
                workspace_trust: WorkspaceTrust::Untrusted,
            })
            .await
            .unwrap();
        drop(handle);
    }

    tokio::time::advance(std::time::Duration::from_secs(60 * 60 + 1)).await;
    remote
        .create(CreateSession {
            cwd: root.path().to_owned(),
            session_id: Some(SessionId::new("draft-after-expiry").unwrap()),
            agent_preset_id: None,
            workspace_trust: WorkspaceTrust::Untrusted,
        })
        .await
        .expect("abandoned drafts must release their bounded server capacity");

    cancellation.cancel();
    task.await.unwrap().unwrap();
}

#[tokio::test(start_paused = true)]
async fn unpublished_draft_activity_renews_its_idle_lease() {
    let root = TempDir::new().unwrap();
    let paths = paths(&root);
    let (fake, epoch, cancellation, task) = start(&paths);
    let remote = UdsSessionApplication::connect(paths.socket(), KEY, epoch)
        .await
        .unwrap();
    let handle = remote
        .create(CreateSession {
            cwd: root.path().to_owned(),
            session_id: Some(SessionId::new("renewed-draft").unwrap()),
            agent_preset_id: None,
            workspace_trust: WorkspaceTrust::Untrusted,
        })
        .await
        .unwrap();

    tokio::time::advance(std::time::Duration::from_mins(59)).await;
    handle.history_before(None, 1).await.unwrap();
    tokio::time::advance(std::time::Duration::from_mins(2)).await;
    handle
        .submit(SubmitInput {
            message_id: MessageId::new("renewed-draft-message").unwrap(),
            content: vec![SessionInput::Text {
                text: "hello".into(),
            }],
            model: None,
            sandbox: None,
        })
        .await
        .unwrap();
    assert_eq!(fake.attach_attempts.load(Ordering::Relaxed), 0);

    cancellation.cancel();
    task.await.unwrap().unwrap();
}

#[tokio::test]
async fn staged_endpoint_is_private_and_refuses_to_replace_a_live_socket() {
    let root = TempDir::new().unwrap();
    let paths = paths(&root);
    let (_fake, _epoch, cancellation, task) = start(&paths);
    let metadata = std::fs::symlink_metadata(paths.socket()).unwrap();
    assert!(metadata.file_type().is_socket());
    assert_eq!(metadata.permissions().mode() & 0o777, 0o600);
    assert_eq!(
        std::fs::metadata(paths.runtime_directory())
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o700
    );
    let second = UdsSessionServer::bind(
        &paths,
        Arc::new(FakeApplication::default()),
        KEY,
        HostEpoch::generate().unwrap(),
    );
    assert!(matches!(second, Err(SessionHostError::OwnerActive)));
    cancellation.cancel();
    task.await.unwrap().unwrap();
}

#[tokio::test]
async fn runtime_parent_symlink_is_rejected_without_changing_its_target_permissions() {
    let root = TempDir::new().unwrap();
    let runtime_root = root.path().join("runtime");
    let borrowed = root.path().join("borrowed-runtime-parent");
    std::fs::create_dir(&runtime_root).unwrap();
    std::fs::create_dir(&borrowed).unwrap();
    std::fs::set_permissions(&borrowed, std::fs::Permissions::from_mode(0o755)).unwrap();
    symlink(&borrowed, runtime_root.join("rsi")).unwrap();
    let paths = paths(&root);

    let result = UdsSessionServer::bind(
        &paths,
        Arc::new(FakeApplication::default()),
        KEY,
        HostEpoch::generate().unwrap(),
    );
    let borrowed_mode = std::fs::metadata(&borrowed).unwrap().permissions().mode() & 0o777;
    std::fs::set_permissions(&borrowed, std::fs::Permissions::from_mode(0o700)).unwrap();

    assert!(matches!(result, Err(SessionHostError::Invalid(_))));
    assert_eq!(borrowed_mode, 0o755);
}

#[tokio::test]
async fn stale_socket_is_removed_only_after_a_failed_liveness_probe() {
    let root = TempDir::new().unwrap();
    let paths = paths(&root);
    std::fs::create_dir_all(paths.runtime_directory()).unwrap();
    let stale = std::os::unix::net::UnixListener::bind(paths.socket()).unwrap();
    drop(stale);
    let server = UdsSessionServer::bind(
        &paths,
        Arc::new(FakeApplication::default()),
        KEY,
        HostEpoch::generate().unwrap(),
    )
    .unwrap();
    let cancellation = CancellationToken::new();
    cancellation.cancel();
    server.serve(cancellation).await.unwrap();
    assert!(!paths.socket().exists());
}

#[tokio::test]
async fn approval_sequence_accepts_descendants_but_rejects_an_unrelated_session() {
    use rsi_agent_session_protocol::{
        AgentPath, EMPTY_FACT_PREFIX_DIGEST, ForkOrigin, ForkTurnSelection,
    };
    use rsi_approval_protocol::ApprovalSubject;
    let root = TempDir::new().unwrap();
    let paths = paths(&root);
    let (fake, epoch, cancellation, task) = start(&paths);
    let parent = header(SessionId::new("approval-parent").unwrap(), root.path());
    let child = parent
        .forked_child(
            SessionId::new("approval-child").unwrap(),
            2,
            ForkOrigin {
                parent_session_id: parent.session_id().clone(),
                root_session_id: parent.session_id().clone(),
                path: AgentPath::new(vec![1]).unwrap(),
                task_name: "child".into(),
                parent_header_fingerprint: parent.fingerprint().unwrap(),
                invoking_turn_id: TurnId::new("turn-parent").unwrap(),
                resolved_after_seq: 0,
                resolved_terminal_seq: 0,
                terminal_prefix_sha256: hex::encode(EMPTY_FACT_PREFIX_DIGEST),
                requested_turns: ForkTurnSelection::None,
                effective_turns: 0,
            },
        )
        .unwrap();
    let unrelated = header(SessionId::new("approval-unrelated").unwrap(), root.path());
    fake.insert(parent.clone());
    fake.insert(child.clone());
    fake.insert(unrelated.clone());
    let request = |session: &SessionHeader| ApprovalRequest {
        id: "approval-request".into(),
        subject: ApprovalSubject::new(session.session_id().as_str(), "turn", "effect").unwrap(),
        action: "write".into(),
        reason: "test routing".into(),
    };
    let remote = UdsSessionApplication::connect(paths.socket(), KEY, epoch)
        .await
        .unwrap();
    let handle = remote.attach(parent.session_id()).await.unwrap();
    *fake.pending.lock().unwrap() = vec![request(&child)];
    assert_eq!(handle.pending_approvals().await.unwrap(), [request(&child)]);
    *fake.pending.lock().unwrap() = vec![request(&unrelated)];
    assert!(
        matches!(handle.pending_approvals().await, Err(SessionApplicationError::Backend(message)) if message.contains("Agent tree"))
    );
    cancellation.cancel();
    task.await.unwrap().unwrap();
}
