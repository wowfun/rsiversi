#![cfg(unix)]
use async_trait::async_trait;
use rsi_agent_session_protocol::{
    AgentPresetId, FrozenAgentSettings, SessionHeader, SessionId, WorkspaceTrust,
};
use rsi_api::ApiRegistry;
use rsi_api_http::{HttpConfig, HttpServer, HttpServices};
use rsi_api_http_client::{HttpClient, HttpClientConfig};
use rsi_api_protocol::{
    ApiError, AuthenticatedDevice, DeviceAuthentication, DeviceId, EndpointId, HostEpoch,
};
use rsi_credentials_protocol::{CredentialRef, SecretValue};
use rsi_files::LocalFiles;
use rsi_files_protocol::*;
use rsi_session_files::{
    SessionFiles as _, SessionFilesApi, SessionFilesClient, SessionFilesError,
};
use rsi_session_protocol::{SessionError, SessionReadLease, SessionReads, SessionTarget};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, AtomicUsize, Ordering},
};
use tokio::sync::Semaphore;
use tokio_util::sync::CancellationToken;

static SERIAL: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

const TOKEN: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
#[derive(Debug)]
struct Authentication(CancellationToken);
impl DeviceAuthentication for Authentication {
    fn authenticate(&self, token: &SecretValue) -> rsi_api_protocol::Result<AuthenticatedDevice> {
        if token.expose_secret() != TOKEN || self.0.is_cancelled() {
            return Err(ApiError::Unauthorized);
        }
        Ok(AuthenticatedDevice {
            id: DeviceId::from_bytes([1; 16]),
            revoked: self.0.clone(),
        })
    }
}
#[derive(Debug)]
struct Reads {
    header: SessionHeader,
    available: AtomicBool,
    calls: AtomicUsize,
    active: Arc<AtomicUsize>,
    retiring: CancellationToken,
}
struct Activity(Arc<AtomicUsize>);
impl Drop for Activity {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}
#[async_trait]
impl SessionReads for Reads {
    async fn acquire(
        &self,
        target: &SessionTarget,
    ) -> rsi_session_protocol::Result<SessionReadLease> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        target.validate()?;
        if !self.available.load(Ordering::SeqCst)
            || target.session_id != *self.header.session_id()
            || target.header_key != self.header.fingerprint().unwrap()
        {
            return Err(SessionError::NotFound("test binding".into()));
        }
        self.active.fetch_add(1, Ordering::SeqCst);
        Ok(SessionReadLease::new(
            self.header.clone(),
            self.retiring.clone(),
            Activity(self.active.clone()),
        ))
    }
}
#[derive(Debug)]
struct Reader {
    native: LocalFiles,
    gate: AtomicBool,
    entered: Semaphore,
    cancellation: Mutex<Option<CancellationToken>>,
}
#[async_trait]
impl Files for Reader {
    fn release_caller(&self, caller: &FilesCaller) {
        self.native.release_caller(caller);
    }
    fn describe(&self, binding: &FilesBinding, token: &FileToken) -> Result<OpenedFile> {
        self.native.describe(binding, token)
    }
    async fn open(
        &self,
        binding: FilesBinding,
        path: RelativePath,
        kind: FileKind,
        cancellation: CancellationToken,
    ) -> Result<OpenedFile> {
        self.native.open(binding, path, kind, cancellation).await
    }
    async fn read(
        &self,
        binding: FilesBinding,
        token: FileToken,
        offset: u64,
        maximum: usize,
        cancellation: CancellationToken,
    ) -> Result<FilePage> {
        let page = self
            .native
            .read(binding, token, offset, maximum, cancellation.clone())
            .await?;
        if self.gate.load(Ordering::SeqCst) {
            *self.cancellation.lock().unwrap() = Some(cancellation);
            self.entered.add_permits(1);
            std::future::pending::<()>().await;
        }
        Ok(page)
    }
    async fn list(
        &self,
        binding: FilesBinding,
        token: FileToken,
        offset: usize,
        maximum: usize,
        cancellation: CancellationToken,
    ) -> Result<DirectoryPage> {
        self.native
            .list(binding, token, offset, maximum, cancellation)
            .await
    }
    fn release(&self, binding: &FilesBinding, token: &FileToken) -> Result<()> {
        self.native.release(binding, token)
    }
}
struct Harness {
    _temporary: tempfile::TempDir,
    origin: String,
    epoch: HostEpoch,
    auth: Arc<Authentication>,
    reads: Arc<Reads>,
    reader: Arc<Reader>,
    api: Option<SessionFilesApi>,
    registry: Arc<ApiRegistry>,
    connection: rsi_api::ConnectionApi,
    transport: Arc<HttpClient>,
    client: SessionFilesClient,
    stop: CancellationToken,
    server: tokio::task::JoinHandle<rsi_api_protocol::Result<()>>,
}
impl Harness {
    #[allow(clippy::too_many_lines)] // One isolated transport binds its actual reader and lifetimes.
    async fn start(trust: WorkspaceTrust) -> Self {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().canonicalize().unwrap();
        std::fs::write(root.join("file"), b"hello\0\xffworld").unwrap();
        std::fs::create_dir(root.join("sub")).unwrap();
        let header = SessionHeader::new(
            SessionId::new("files-http").unwrap(),
            1,
            root.to_str().unwrap(),
            AgentPresetId::new("test").unwrap(),
            FrozenAgentSettings::new(
                "settings",
                "system",
                rsi_ai_protocol::ModelRef::new("test", "model").unwrap(),
                rsi_sandbox::SandboxMode::ReadOnly,
                false,
            )
            .unwrap(),
        )
        .unwrap()
        .with_workspace_trust(trust)
        .unwrap();
        let reads = Arc::new(Reads {
            header,
            available: AtomicBool::new(true),
            calls: AtomicUsize::new(0),
            active: Arc::new(AtomicUsize::new(0)),
            retiring: CancellationToken::new(),
        });
        let reader = Arc::new(Reader {
            native: LocalFiles::new().unwrap(),
            gate: AtomicBool::new(false),
            entered: Semaphore::new(0),
            cancellation: Mutex::new(None),
        });
        let execution = rsi_meta::Execution::native(tokio::runtime::Handle::current());
        let registry = Arc::new(ApiRegistry::new(execution.clone()));
        let api =
            SessionFilesApi::register(registry.as_ref(), reads.clone(), reader.clone()).unwrap();
        let endpoint = EndpointId::from_bytes([2; 16]);
        let epoch = HostEpoch::from_bytes([3; 16]);
        let connection = rsi_api::ConnectionApi::register(
            registry.clone(),
            registry.as_ref(),
            endpoint.clone(),
            epoch.clone(),
        )
        .unwrap();
        let auth = Arc::new(Authentication(CancellationToken::new()));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let origin = format!("http://{address}");
        let server = HttpServer::from_listener(
            execution.clone(),
            listener,
            HttpConfig {
                bind: address,
                public_origin: origin.clone(),
                tls: None,
                allow_loopback_http: true,
            },
            HttpServices {
                dispatch: registry.clone(),
                authentication: auth.clone(),
                endpoint: endpoint.clone(),
                epoch: epoch.clone(),
            },
        )
        .await
        .unwrap();
        let stop = CancellationToken::new();
        let task = tokio::spawn(server.serve(stop.clone()));
        let transport = Arc::new(
            HttpClient::connect(
                execution,
                HttpClientConfig {
                    origin: origin.clone(),
                    endpoint_id: endpoint,
                    credential: CredentialRef::new("test.files", "device").unwrap(),
                    tls_ca: None,
                    allow_loopback_http: true,
                },
                SecretValue::new(TOKEN).unwrap(),
            )
            .await
            .unwrap(),
        );
        let client = SessionFilesClient::new(transport.clone()).unwrap();
        Self {
            _temporary: temporary,
            origin,
            epoch,
            auth,
            reads,
            reader,
            api: Some(api),
            registry,
            connection,
            transport,
            client,
            stop,
            server: task,
        }
    }
    fn target(&self) -> SessionTarget {
        SessionTarget {
            session_id: self.reads.header.session_id().clone(),
            header_key: self.reads.header.fingerprint().unwrap(),
        }
    }
    async fn raw(
        &self,
        operation: &str,
        body: serde_json::Value,
        authenticated: bool,
    ) -> reqwest::Response {
        let mut request = reqwest::Client::builder()
            .no_proxy()
            .timeout(std::time::Duration::from_secs(5))
            .build()
            .unwrap()
            .post(format!("{}/api/v1/files/{operation}/1", self.origin))
            .header("content-type", "application/json")
            .header("x-rsi-wire-version", "1")
            .header("x-rsi-host-epoch", self.epoch.as_str())
            .body(serde_json::to_vec(&body).unwrap());
        if authenticated {
            request = request.bearer_auth(TOKEN);
        }
        request.send().await.unwrap()
    }
    async fn close(self) {
        self.transport.close().await;
        self.stop.cancel();
        self.server.await.unwrap().unwrap();
        self.api.unwrap().close().await;
        self.connection.close().await;
        self.registry.close().await;
        self.reader.native.close().await;
        assert_eq!(self.reads.active.load(Ordering::SeqCst), 0);
    }
}

#[tokio::test]
async fn real_http_requires_authentication_before_session_binding_and_ignores_trust_as_file_authority()
 {
    let _serial = SERIAL.lock().await;
    for trust in [WorkspaceTrust::Untrusted, WorkspaceTrust::Trusted] {
        let harness = Harness::start(trust).await;
        let input = serde_json::json!({"target": harness.target(), "input": {"path": "66696c65", "kind": "file"}});
        let response = harness.raw("open", input.clone(), false).await;
        assert_eq!(response.status(), 401);
        assert_eq!(harness.reads.calls.load(Ordering::SeqCst), 0);
        let mut forged = input;
        forged["input"]["workspace"] = serde_json::json!("/");
        assert_eq!(harness.raw("open", forged, true).await.status(), 400);
        assert_eq!(harness.reads.calls.load(Ordering::SeqCst), 0);
        let file = harness
            .client
            .open(
                harness.target(),
                RelativePath::new(b"file").unwrap(),
                FileKind::File,
            )
            .await
            .unwrap();
        let page = harness
            .client
            .read(harness.target(), file.clone(), 4, 6)
            .await
            .unwrap();
        assert_eq!(page.bytes_hex, hex::encode(b"o\0\xffwor"));
        let mut wrong = file.clone();
        wrong.path = RelativePath::new(b"sub").unwrap();
        assert_eq!(
            harness.client.read(harness.target(), wrong, 0, 1).await,
            Err(SessionFilesError::Files(FilesError::Binding))
        );
        let directory = harness
            .client
            .open(
                harness.target(),
                RelativePath::default(),
                FileKind::Directory,
            )
            .await
            .unwrap();
        let page = harness
            .client
            .list(harness.target(), directory.clone(), 0, 1)
            .await
            .unwrap();
        assert_eq!(page.total, 2);
        assert_eq!(page.entries[0].name, "file");
        harness
            .client
            .release(harness.target(), directory.token)
            .await
            .unwrap();
        harness.reads.available.store(false, Ordering::SeqCst);
        assert_eq!(
            harness
                .client
                .read(harness.target(), file.clone(), 0, 1)
                .await,
            Err(SessionFilesError::Files(FilesError::Unavailable))
        );
        harness.reads.available.store(true, Ordering::SeqCst);
        let mut wrong_target = harness.target();
        wrong_target.header_key = "0".repeat(64);
        assert_eq!(
            harness.client.read(wrong_target, file.clone(), 0, 1).await,
            Err(SessionFilesError::Files(FilesError::Unavailable))
        );
        harness
            .client
            .release(harness.target(), file.token)
            .await
            .unwrap();
        harness.close().await;
    }
}

#[tokio::test]
async fn credential_revocation_cancels_an_in_flight_real_page_and_releases_session_activity() {
    let _serial = SERIAL.lock().await;
    let harness = Harness::start(WorkspaceTrust::Untrusted).await;
    let file = harness
        .client
        .open(
            harness.target(),
            RelativePath::new(b"file").unwrap(),
            FileKind::File,
        )
        .await
        .unwrap();
    harness.reader.gate.store(true, Ordering::SeqCst);
    let client = harness.client.clone();
    let target = harness.target();
    let read = tokio::spawn(async move { client.read(target, file, 0, 10).await });
    harness.reader.entered.acquire().await.unwrap().forget();
    assert_eq!(harness.reads.active.load(Ordering::SeqCst), 1);
    let cancellation = harness.reader.cancellation.lock().unwrap().clone().unwrap();
    harness.auth.0.cancel();
    assert_eq!(
        read.await.unwrap(),
        Err(SessionFilesError::Api(ApiError::Unauthorized))
    );
    tokio::time::timeout(std::time::Duration::from_secs(2), cancellation.cancelled())
        .await
        .unwrap();
    assert_eq!(harness.reads.active.load(Ordering::SeqCst), 0);
    harness.close().await;
}

#[tokio::test]
async fn session_retirement_cancels_active_files_response_without_changing_api_authentication() {
    let _serial = SERIAL.lock().await;
    let harness = Harness::start(WorkspaceTrust::Trusted).await;
    let file = harness
        .client
        .open(
            harness.target(),
            RelativePath::new(b"file").unwrap(),
            FileKind::File,
        )
        .await
        .unwrap();
    harness.reader.gate.store(true, Ordering::SeqCst);
    let client = harness.client.clone();
    let target = harness.target();
    let read = tokio::spawn(async move { client.read(target, file, 0, 10).await });
    harness.reader.entered.acquire().await.unwrap().forget();
    let cancellation = harness.reader.cancellation.lock().unwrap().clone().unwrap();
    harness.reads.retiring.cancel();
    assert_eq!(
        read.await.unwrap(),
        Err(SessionFilesError::Files(FilesError::Cancelled))
    );
    assert!(cancellation.is_cancelled());
    assert!(!harness.auth.0.is_cancelled());
    harness.close().await;
}

#[tokio::test]
async fn endpoint_replacement_releases_old_generation_tokens_before_new_reader_admission() {
    let _serial = SERIAL.lock().await;
    let mut harness = Harness::start(WorkspaceTrust::Untrusted).await;
    let mut old = None;
    for _ in 0..MAXIMUM_FILE_TOKENS {
        old = Some(
            harness
                .client
                .open(
                    harness.target(),
                    RelativePath::new(b"file").unwrap(),
                    FileKind::File,
                )
                .await
                .unwrap(),
        );
    }
    assert_eq!(
        harness
            .client
            .open(
                harness.target(),
                RelativePath::new(b"file").unwrap(),
                FileKind::File
            )
            .await,
        Err(SessionFilesError::Files(FilesError::Capacity))
    );
    harness.api.take().unwrap().close().await;
    harness.api = Some(
        SessionFilesApi::register(
            harness.registry.as_ref(),
            harness.reads.clone(),
            harness.reader.clone(),
        )
        .unwrap(),
    );
    assert_eq!(
        harness
            .client
            .read(harness.target(), old.unwrap(), 0, 1)
            .await,
        Err(SessionFilesError::Files(FilesError::Unavailable))
    );
    for _ in 0..MAXIMUM_FILE_TOKENS {
        harness
            .client
            .open(
                harness.target(),
                RelativePath::new(b"file").unwrap(),
                FileKind::File,
            )
            .await
            .unwrap();
    }
    harness.close().await;
}
