#![cfg(target_os = "linux")]

use async_trait::async_trait;
use futures_util::StreamExt as _;
use rsi_agent_session_protocol::{
    AgentPresetId, FrozenAgentSettings, MAXIMUM_TURN_TEXT_BYTES, SessionFact, SessionFactBody,
    SessionHeader, SessionId, TurnId,
};
use rsi_agent_turn_protocol::{CancelResult, TurnObservation};
use rsi_ai_protocol::ModelRef;
use rsi_approval_protocol::{ApprovalDecision, ApprovalRequest};
use rsi_host::HostPaths;
use rsi_sandbox::SandboxMode;
use rsi_session::{
    CreateSession, RecentSessionCursor, RecentSessionPage, SessionApplication,
    SessionApplicationError, SessionHandle, SessionHistoryPage, SessionSummary, SubmitDirectImage,
    SubmitText, TurnReceipt,
};
use rsi_session_host::{
    HostEpoch, SessionHostError, SessionHostPaths, UdsSessionApplication, UdsSessionServer,
};
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
    history_fact_count: usize,
    history_ignores_limit: bool,
    mismatched_create_header: bool,
    mismatched_attach_header: bool,
    mismatched_receipt: bool,
    submit_conflict: bool,
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
        let session_id = request
            .session_id
            .unwrap_or(SessionId::new("generated-session").unwrap());
        let session_id = if self.mismatched_create_header {
            SessionId::new("wrong-created-session").unwrap()
        } else {
            session_id
        };
        let header = header(session_id, &request.cwd);
        self.insert(header.clone());
        Ok(Arc::new(FakeHandle {
            header,
            history_fact_count: self.history_fact_count,
            history_ignores_limit: self.history_ignores_limit,
            mismatched_receipt: self.mismatched_receipt,
            submit_conflict: self.submit_conflict,
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
}

#[async_trait]
impl SessionHandle for FakeHandle {
    async fn header(&self) -> rsi_session::Result<SessionHeader> {
        Ok(self.header.clone())
    }

    async fn submit_text(&self, request: SubmitText) -> rsi_session::Result<TurnReceipt> {
        if self.submit_conflict {
            return Err(SessionApplicationError::Conflict {
                session: self.header.session_id().to_string(),
                turn: request.turn_id.to_string(),
            });
        }
        Ok(TurnReceipt {
            session_id: if self.mismatched_receipt {
                SessionId::new("wrong-receipt-session").unwrap()
            } else {
                self.header.session_id().clone()
            },
            turn_id: if self.mismatched_receipt {
                TurnId::new("wrong-receipt-turn").unwrap()
            } else {
                request.turn_id
            },
            accepted_seq: 7,
        })
    }

    async fn submit_image(&self, request: SubmitDirectImage) -> rsi_session::Result<TurnReceipt> {
        Ok(TurnReceipt {
            session_id: self.header.session_id().clone(),
            turn_id: request.turn_id,
            accepted_seq: 8,
        })
    }

    async fn cancel(
        &self,
        _turn_id: &TurnId,
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

    async fn subscribe(&self, _after_seq: u64) -> rsi_session::Result<TurnObservation> {
        Ok(Box::pin(futures_util::stream::empty()))
    }

    async fn pending_approvals(&self) -> rsi_session::Result<Vec<ApprovalRequest>> {
        Ok(Vec::new())
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
            .submit_text(SubmitText {
                turn_id: TurnId::new("oversized-turn").unwrap(),
                text: "x".repeat(MAXIMUM_TURN_TEXT_BYTES + 1),
                model: None,
                sandbox: None,
            })
            .await,
        Err(SessionApplicationError::Invalid(message)) if message.contains("turn text")
    ));
    let mut observation = handle.subscribe(0).await.unwrap();
    assert!(observation.next().await.is_none());
    let receipt = handle
        .submit_text(SubmitText {
            turn_id: TurnId::new("turn-1").unwrap(),
            text: "hello".into(),
            model: None,
            sandbox: None,
        })
        .await
        .unwrap();
    assert_eq!(receipt.accepted_seq, 7);
    assert_eq!(
        remote.list_recent(None, 10).await.unwrap().sessions.len(),
        1
    );
    let cancelled = handle
        .cancel(&TurnId::new("turn-1").unwrap(), Some("test".into()))
        .await
        .unwrap();
    assert!(cancelled.accepted);

    cancellation.cancel();
    task.await.unwrap().unwrap();
    assert!(!paths.socket().exists());
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
async fn client_rejects_a_receipt_for_another_session_or_turn() {
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
        })
        .await
        .unwrap();
    assert!(matches!(
        handle
            .submit_text(SubmitText {
                turn_id: TurnId::new("receipt-turn").unwrap(),
                text: "hello".into(),
                model: None,
                sandbox: None,
            })
            .await,
        Err(SessionApplicationError::Backend(message)) if message.contains("identity")
    ));
    cancellation.cancel();
    task.await.unwrap().unwrap();
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
            })
            .await
            .unwrap();
        assert!(matches!(
            handle
                .submit_text(SubmitText {
                    turn_id: TurnId::new(format!("conflicted-turn-{index}")).unwrap(),
                    text: "conflict".into(),
                    model: None,
                    sandbox: None,
                })
                .await,
            Err(SessionApplicationError::Conflict { .. })
        ));
    }
    remote
        .create(CreateSession {
            cwd: root.path().to_owned(),
            session_id: Some(SessionId::new("after-conflicts").unwrap()),
            agent_preset_id: None,
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
        })
        .await
        .unwrap();

    tokio::time::advance(std::time::Duration::from_mins(59)).await;
    handle.history_before(None, 1).await.unwrap();
    tokio::time::advance(std::time::Duration::from_mins(2)).await;
    handle
        .submit_text(SubmitText {
            turn_id: TurnId::new("renewed-draft-turn").unwrap(),
            text: "hello".into(),
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
