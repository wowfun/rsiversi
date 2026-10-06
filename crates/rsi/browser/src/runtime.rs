use crate::{BrowserPolicy, CheckOutcome, CheckResult, CheckSpec};
use base64::{Engine, engine::general_purpose::STANDARD};
use rsi_process::{DuplexProcess, DuplexProcessSpec, ManagedDuplexProcess};
use rsi_sandbox::{IsolatedProcessRequest, ReadOnlyMount, Sandbox};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{collections::BTreeMap, fmt, path::PathBuf, sync::Arc, time::Duration};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    sync::{Mutex, mpsc},
};
use tokio_util::{sync::CancellationToken, task::TaskTracker};

const FRAME: usize = 8 * 1024 * 1024;
const WORKER: &str = include_str!("../runtime/worker.mjs");
const FRAME_READER: &str = include_str!("../runtime/frame-reader.mjs");
const DEADLINE: &str = include_str!("../runtime/deadline.mjs");

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RuntimeConfig {
    pub node: PathBuf,
    pub chromium_directory: PathBuf,
    pub package_directory: PathBuf,
    pub systemd_run: PathBuf,
    pub user_runtime_directory: PathBuf,
    pub artifact_digest: String,
}
impl RuntimeConfig {
    /// # Errors
    /// Rejects malformed values or values exceeding the owning protocol bounds.
    pub fn validate(&self) -> Result<(), String> {
        for path in [
            &self.node,
            &self.chromium_directory,
            &self.package_directory,
            &self.systemd_run,
            &self.user_runtime_directory,
        ] {
            if !path.is_absolute()
                || path.components().any(|c| {
                    !matches!(
                        c,
                        std::path::Component::RootDir | std::path::Component::Normal(_)
                    )
                })
                || path == &PathBuf::from("/")
            {
                return Err("runtime paths must be explicit normalized absolute paths".into());
            }
        }
        if self.artifact_digest.len() != 64
            || !self
                .artifact_digest
                .bytes()
                .all(|b| b.is_ascii_digit() || matches!(b, b'a'..=b'f'))
        {
            return Err("runtime requires exact artifact SHA-256".into());
        }
        Ok(())
    }
    /// Measures all installed runtime bytes and contained npm command-link targets.
    /// # Errors
    /// Fails when runtime resources cannot be read or violate the immutable artifact layout.
    pub fn digest(&self) -> Result<String, String> {
        let mut hash = Sha256::new();
        let mut count = 0usize;
        let mut bytes = 0u64;
        for root in [
            &self.node,
            &self.chromium_directory,
            &self.package_directory,
        ] {
            fingerprint(root, root, &mut hash, &mut count, &mut bytes)?;
        }
        Ok(hex::encode(hash.finalize()))
    }
}
fn fingerprint(
    root: &std::path::Path,
    path: &std::path::Path,
    hash: &mut Sha256,
    count: &mut usize,
    bytes: &mut u64,
) -> Result<(), String> {
    let metadata = std::fs::symlink_metadata(path).map_err(|e| e.to_string())?;
    if metadata.file_type().is_symlink() {
        let command_link = path.parent().is_some_and(|parent| {
            parent.file_name().is_some_and(|n| n == ".bin")
                && parent
                    .parent()
                    .is_some_and(|p| p.file_name().is_some_and(|n| n == "node_modules"))
        });
        let target = std::fs::read_link(path).map_err(|e| e.to_string())?;
        let resolved = std::fs::canonicalize(path).map_err(|e| e.to_string())?;
        if !command_link
            || target.is_absolute()
            || target.as_os_str().len() > 4096
            || !resolved.starts_with(root)
            || !resolved.is_file()
        {
            return Err("runtime artifact contains an unexpected or escaping symlink".into());
        }
        *count += 1;
        *bytes = bytes.saturating_add(target.as_os_str().len() as u64);
        if *count > 32768 || *bytes > 1024 * 1024 * 1024 {
            return Err("runtime artifact exceeds validation budget".into());
        }
        hash.update(b"npm-command-link\0");
        let relative = path
            .strip_prefix(root)
            .map_err(|e| e.to_string())?
            .as_os_str()
            .as_encoded_bytes();
        hash.update((relative.len() as u64).to_le_bytes());
        hash.update(relative);
        let literal = target.as_os_str().as_encoded_bytes();
        hash.update((literal.len() as u64).to_le_bytes());
        hash.update(literal);
        return Ok(());
    }
    if metadata.is_dir() {
        let mut paths = std::fs::read_dir(path)
            .map_err(|e| e.to_string())?
            .map(|e| e.map(|e| e.path()))
            .collect::<std::io::Result<Vec<_>>>()
            .map_err(|e| e.to_string())?;
        paths.sort();
        for child in paths {
            fingerprint(root, &child, hash, count, bytes)?;
        }
    } else if metadata.is_file() {
        *count += 1;
        *bytes = bytes.saturating_add(metadata.len());
        if *count > 32768 || *bytes > 1024 * 1024 * 1024 {
            return Err("runtime artifact exceeds validation budget".into());
        }
        let relative = path
            .strip_prefix(root)
            .map_err(|e| e.to_string())?
            .as_os_str()
            .as_encoded_bytes();
        hash.update(
            u64::try_from(relative.len())
                .map_err(|e| e.to_string())?
                .to_le_bytes(),
        );
        hash.update(relative);
        hash.update(metadata.len().to_le_bytes());
        let mut file = std::fs::File::open(path).map_err(|e| e.to_string())?;
        let mut buffer = vec![0u8; 65_536].into_boxed_slice();
        loop {
            let length = std::io::Read::read(&mut file, &mut buffer).map_err(|e| e.to_string())?;
            if length == 0 {
                break;
            }
            hash.update(&buffer[..length]);
        }
    } else {
        return Err("runtime artifact is not regular storage".into());
    }
    Ok(())
}

/// Existing providers retain OS ownership; this owner holds only browser policy and leases.
pub struct NativeRuntime {
    config: RuntimeConfig,
    processes: Arc<dyn DuplexProcess>,
    sandbox: Arc<dyn Sandbox>,
    slots: Arc<tokio::sync::Semaphore>,
    verified: Arc<std::sync::atomic::AtomicBool>,
    diagnostic: Arc<std::sync::Mutex<Option<String>>>,
    preparation: Mutex<()>,
    #[cfg(feature = "test-support")]
    fixture: Option<u16>,
}
impl fmt::Debug for NativeRuntime {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("NativeRuntime").finish_non_exhaustive()
    }
}
static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
impl NativeRuntime {
    /// # Errors
    /// Rejects invalid resources or unavailable exact operation versions.
    pub fn new(
        config: RuntimeConfig,
        processes: Arc<dyn DuplexProcess>,
        sandbox: Arc<dyn Sandbox>,
    ) -> Result<Self, String> {
        config.validate()?;
        Ok(Self {
            config,
            processes,
            sandbox,
            slots: Arc::new(tokio::sync::Semaphore::new(2)),
            verified: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            diagnostic: Arc::new(std::sync::Mutex::new(None)),
            preparation: Mutex::new(()),
            #[cfg(feature = "test-support")]
            fixture: None,
        })
    }
    #[cfg(feature = "test-support")]
    #[must_use]
    pub fn with_fixture_network(mut self, port: u16) -> Self {
        self.fixture = Some(port);
        self
    }
    /// # Errors
    /// Fails when the installed runtime identity or pinned dependency versions differ.
    pub async fn verify(&self) -> Result<(), String> {
        let config = self.config.clone();
        tokio::task::spawn_blocking(move || {
            if config.digest()? != config.artifact_digest {
                return Err("browser runtime artifact digest mismatch".into());
            }
            for (path, expected) in [
                ("worker.mjs", WORKER),
                ("frame-reader.mjs", FRAME_READER),
                ("deadline.mjs", DEADLINE),
            ] {
                if std::fs::read_to_string(config.package_directory.join(path))
                    .map_err(|e| e.to_string())?
                    != expected
                {
                    return Err("browser helper differs from compiled source".into());
                }
            }
            for (path, expected) in [
                ("node_modules/playwright/package.json", "1.63.0"),
                ("node_modules/@playwright/mcp/package.json", "0.0.80"),
            ] {
                let value: Value = serde_json::from_slice(
                    &std::fs::read(config.package_directory.join(path))
                        .map_err(|e| e.to_string())?,
                )
                .map_err(|e| e.to_string())?;
                if value["version"] != expected {
                    return Err("browser dependency version mismatch".into());
                }
            }
            Ok(())
        })
        .await
        .map_err(|_| "browser verification worker failed".to_owned())?
    }
    /// Proves the fixed runtime can start and retire without any page request.
    /// # Errors
    /// Fails when verification, isolated startup or process retirement cannot be proved.
    pub async fn prepare(&self) -> Result<(), String> {
        let _preparation = self.preparation.lock().await;
        if self.is_verified() {
            return self.require_ready();
        }
        self.verified
            .store(false, std::sync::atomic::Ordering::Release);
        let identity = format!(
            "ready-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        );
        let result = async {
            self.verify().await?;
            let scope = self
                .open_kind(
                    BrowserPolicy {
                        entry_url: "https://readiness.invalid/".into(),
                        path_prefix: "/".into(),
                        dependency_hosts: std::collections::BTreeSet::default(),
                    },
                    &identity,
                    "checker",
                )
                .await?;
            scope.close().await
        }
        .await;
        *self
            .diagnostic
            .lock()
            .map_err(|_| "browser diagnostic unavailable")? = result
            .as_ref()
            .err()
            .map(|e| e.chars().take(4096).collect());
        self.verified
            .store(result.is_ok(), std::sync::atomic::Ordering::Release);
        result
    }
    pub fn diagnostic(&self) -> Option<String> {
        self.diagnostic.lock().ok().and_then(|d| d.clone())
    }
    pub fn is_verified(&self) -> bool {
        self.verified.load(std::sync::atomic::Ordering::Acquire)
    }
    fn require_ready(&self) -> Result<(), String> {
        if self.slots.is_closed() {
            return Err(
                "prior browser process settlement failed; replace the runtime generation".into(),
            );
        }
        if !self.is_verified() {
            return Err(self
                .diagnostic()
                .unwrap_or_else(|| "browser runtime has not completed preparation".into()));
        }
        Ok(())
    }
    async fn spawn(&self, mode: &str, unit: &str) -> Result<ManagedDuplexProcess, String> {
        let mut mounts = vec![
            ReadOnlyMount {
                source: PathBuf::from("/usr"),
                destination: PathBuf::from("/usr"),
            },
            ReadOnlyMount {
                source: std::fs::canonicalize("/lib").map_err(|e| e.to_string())?,
                destination: PathBuf::from("/lib"),
            },
            ReadOnlyMount {
                source: std::fs::canonicalize("/lib64").map_err(|e| e.to_string())?,
                destination: PathBuf::from("/lib64"),
            },
            ReadOnlyMount {
                source: self.config.node.clone(),
                destination: PathBuf::from("/runtime/node"),
            },
            ReadOnlyMount {
                source: self.config.package_directory.clone(),
                destination: PathBuf::from("/runtime/package"),
            },
            ReadOnlyMount {
                source: self.config.chromium_directory.clone(),
                destination: PathBuf::from("/runtime/chrome"),
            },
        ];
        if std::path::Path::new("/etc/fonts").is_dir() {
            mounts.push(ReadOnlyMount {
                source: PathBuf::from("/etc/fonts"),
                destination: PathBuf::from("/etc/fonts"),
            });
        }
        let request = IsolatedProcessRequest {
            supervisor: self.config.systemd_run.clone(),
            unit: unit.into(),
            program: PathBuf::from("/runtime/node"),
            arguments: vec!["/runtime/package/worker.mjs".into(), mode.into()],
            workspace: self.config.package_directory.clone(),
            mounts,
        };
        let plan = self
            .sandbox
            .confine_isolated(request.clone())
            .await
            .map_err(|e| e.to_string())?;
        let process = self
            .processes
            .spawn(DuplexProcessSpec {
                process: plan,
                environment: vec![
                    (
                        "XDG_RUNTIME_DIR".into(),
                        self.config.user_runtime_directory.as_os_str().to_owned(),
                    ),
                    (
                        "DBUS_SESSION_BUS_ADDRESS".into(),
                        format!(
                            "unix:path={}/bus",
                            self.config.user_runtime_directory.display()
                        )
                        .into(),
                    ),
                ],
                stdout_buffer_bytes: 65536,
                stderr_max_bytes: 16384,
                termination_grace_ms: 1000,
            })
            .await
            .map_err(|e| e.to_string())?;
        if let Err(error) = self
            .sandbox
            .verify_isolated_limits(&request, &self.config.user_runtime_directory)
            .await
        {
            process.terminate();
            if process.wait_settlement().await.is_err() {
                self.slots.close();
            }
            return Err(error.to_string());
        }
        Ok(process)
    }
    /// # Errors
    /// Fails when the selected resources cannot be validated, exclusively owned or made ready.
    pub async fn open(
        &self,
        policy: BrowserPolicy,
        identity: &str,
    ) -> Result<Arc<BrowserSession>, String> {
        self.require_ready()?;
        self.open_kind(policy, identity, "checker").await
    }
    /// # Errors
    /// Rejects invalid policy, unavailable isolation, exhausted capacity or a failed Browser launch.
    pub async fn open_exploration(
        &self,
        policy: BrowserPolicy,
        identity: &str,
    ) -> Result<ExplorationBrowser, String> {
        self.require_ready()?;
        let session = self.open_kind(policy, identity, "mcp").await?;
        let input = Arc::new(McpInput {
            session: session.clone(),
            buffer: Mutex::new(vec![]),
        });
        let output = Arc::new(McpOutput {
            session: session.clone(),
            buffer: Mutex::new(std::collections::VecDeque::new()),
        });
        let process = ManagedDuplexProcess::new(Arc::new(McpControl {
            session: session.clone(),
            input,
            output,
        }));
        let mcp = match rsi_mcp::PrivateMcp::attach(
            process,
            "preview",
            vec!["browser_navigate".into(), "browser_snapshot".into()],
        )
        .await
        {
            Ok(mcp) => mcp,
            Err(e) => {
                let _ = session.close().await;
                return Err(e.to_string());
            }
        };
        Ok(ExplorationBrowser {
            session,
            mcp,
            operations: Mutex::new(()),
        })
    }
    #[expect(
        clippy::too_many_lines,
        reason = "Keep one complete ownership operation or acceptance scenario together"
    )]
    async fn open_kind(
        &self,
        policy: BrowserPolicy,
        identity: &str,
        mode: &str,
    ) -> Result<Arc<BrowserSession>, String> {
        policy.validate()?;
        if identity.len() > 48
            || identity.is_empty()
            || !identity
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-')
        {
            return Err("invalid browser scope identity".into());
        }
        let permit = Arc::new(self.slots.clone().try_acquire_owned().map_err(
            |error| match error {
                tokio::sync::TryAcquireError::Closed => {
                    "prior browser process settlement failed; replace the runtime generation"
                }
                tokio::sync::TryAcquireError::NoPermits => "browser capacity reached",
            },
        )?);
        let browser = self
            .spawn("browser", &format!("rsi-browser-{identity}-browser"))
            .await?;
        let client = match self
            .spawn(mode, &format!("rsi-browser-{identity}-client"))
            .await
        {
            Ok(p) => p,
            Err(e) => {
                browser.terminate();
                if let Err(error) = browser.wait_settlement().await {
                    self.slots.close();
                    self.verified
                        .store(false, std::sync::atomic::Ordering::Release);
                    *self
                        .diagnostic
                        .lock()
                        .map_err(|_| "browser diagnostic unavailable")? = Some(
                        "prior browser process settlement failed; replace the runtime generation"
                            .into(),
                    );
                    return Err(error.to_string());
                }
                return Err(e);
            }
        };
        let (send, receive) = mpsc::channel(16);
        let (mcp_output, mcp_receive) = mpsc::channel(16);
        let tasks = TaskTracker::new();
        let stop = CancellationToken::new();
        let session = Arc::new(BrowserSession {
            browser: browser.clone(),
            client: client.clone(),
            policy,
            commands: Mutex::new(receive),
            mcp_responses: Mutex::new(mcp_receive),
            writes: Arc::new(Mutex::new(())),
            tasks,
            stop: stop.clone(),
            permit: permit.clone(),
        });
        let retired_stop = stop.clone();
        let retired_browser = browser.clone();
        let retired_client = client.clone();
        let slots = self.slots.clone();
        let verified = self.verified.clone();
        let diagnostic = self.diagnostic.clone();
        tokio::spawn(async move {
            let _capacity = permit;
            tokio::select! {()=retired_stop.cancelled()=>{},()=tokio::time::sleep(Duration::from_mins(10))=>retired_stop.cancel()};
            retired_browser.terminate();
            retired_client.terminate();
            let (b, c) = tokio::join!(
                retired_browser.wait_settlement(),
                retired_client.wait_settlement()
            );
            if b.is_err() || c.is_err() {
                slots.close();
                verified.store(false, std::sync::atomic::Ordering::Release);
                if let Ok(mut cause) = diagnostic.lock() {
                    *cause = Some(
                        "prior browser process settlement failed; replace the runtime generation"
                            .into(),
                    );
                }
            }
        });
        let packets = send.clone();
        let child = Arc::new(BridgeScope::from_session(&session));
        #[cfg(feature = "test-support")]
        let fixture = self.fixture;
        session.tasks.spawn(async move {
            if let Err(e) = bridge(
                child,
                packets,
                mcp_output,
                #[cfg(feature = "test-support")]
                fixture,
            )
            .await
            {
                let _ = send.try_send(Err(e));
                stop.cancel();
            }
        });
        // Initialization is explicit. A failed command never establishes replay safety.
        #[cfg(feature = "test-support")]
        let fixture_tls = self.fixture.is_some();
        #[cfg(not(feature = "test-support"))]
        let fixture_tls = false;
        let initialized = async {
            session
                .write(&browser, json!({"kind":"init","fixture_tls":fixture_tls}))
                .await?;
            session.wait_ready(1).await?;
            session
                .write(&client, {
                    let mut entropy = [0u8; 32];
                    getrandom::fill(&mut entropy)
                        .map_err(|_| "private bridge entropy unavailable")?;
                    json!({"kind":"init","token":hex::encode(entropy)})
                })
                .await
        }
        .await;
        if let Err(error) = initialized {
            let _ = session.close().await;
            return Err(error);
        }
        if let Err(error) = session.wait_ready(1).await {
            let _ = session.close().await;
            return Err(error);
        }
        Ok(session)
    }
}

/// Two fixed text-only operations over an attempt-private verified MCP epoch.
#[derive(Debug)]
pub struct ExplorationBrowser {
    session: Arc<BrowserSession>,
    mcp: rsi_mcp::PrivateMcp,
    operations: Mutex<()>,
}
impl ExplorationBrowser {
    /// # Errors
    /// Rejects destinations outside the frozen policy or failed and retired browser exchanges.
    pub async fn navigate(&self, url: &str) -> Result<String, String> {
        self.session.policy.navigate(url)?;
        self.text("browser_navigate", json!({"url":url})).await?;
        self.observe().await
    }
    /// # Errors
    /// Fails on retired transport or malformed preview evidence.
    pub async fn observe(&self) -> Result<String, String> {
        self.text("browser_snapshot", json!({})).await
    }
    async fn text(&self, name: &str, args: Value) -> Result<String, String> {
        let _operation = self.operations.lock().await;
        let value = self.mcp.call(name, args).await.map_err(|e| e.to_string())?;
        if value["isError"].as_bool() == Some(true) {
            return Err("private browser operation failed".into());
        }
        let parts = value["content"].as_array().ok_or("missing browser text")?;
        let mut text = String::new();
        for part in parts {
            if part["type"] != "text" {
                return Err("private browser returned non-text evidence".into());
            }
            text.push_str(part["text"].as_str().ok_or("invalid browser text")?);
            text.push('\n');
        }
        if text.len() > 65536 {
            return Err("browser text exceeds 64 KiB".into());
        }
        self.session.current_url().await?;
        Ok(text)
    }
    /// # Errors
    /// Fails when the owned confined processes cannot be proven settled.
    pub async fn close(&self) -> Result<(), String> {
        let result = self.mcp.close().await.map_err(|e| e.to_string());
        let cleanup = self.session.close().await;
        cleanup?;
        result
    }
}
#[derive(Debug)]
struct McpInput {
    session: Arc<BrowserSession>,
    buffer: Mutex<Vec<u8>>,
}
#[async_trait::async_trait]
impl rsi_process::DuplexInput for McpInput {
    async fn write(&self, bytes: &[u8]) -> rsi_process::Result<usize> {
        if bytes.len() > 65536 || self.session.stop.is_cancelled() {
            return Err(rsi_process::ProcessError::OutcomeUnknown);
        }
        let mut buffer = self.buffer.lock().await;
        buffer.extend_from_slice(bytes);
        if buffer.len() > 1024 * 1024 {
            return Err(rsi_process::ProcessError::Capacity);
        }
        while let Some(end) = buffer.iter().position(|b| *b == b'\n') {
            let value: Value = serde_json::from_slice(&buffer[..end])
                .map_err(|_| rsi_process::ProcessError::OutcomeUnknown)?;
            self.session
                .write(&self.session.client, json!({"kind":"mcp","value":value}))
                .await
                .map_err(|_| rsi_process::ProcessError::OutcomeUnknown)?;
            buffer.drain(..=end);
        }
        Ok(bytes.len())
    }
    async fn close(&self) -> rsi_process::Result<()> {
        self.session.stop.cancel();
        Ok(())
    }
}
#[derive(Debug)]
struct McpOutput {
    session: Arc<BrowserSession>,
    buffer: Mutex<std::collections::VecDeque<u8>>,
}
#[async_trait::async_trait]
impl rsi_process::DuplexOutput for McpOutput {
    async fn read(&self, maximum: usize) -> rsi_process::Result<rsi_process::DuplexRead> {
        if maximum == 0 || maximum > 65536 {
            return Err(rsi_process::ProcessError::Capacity);
        }
        let mut buffer = self.buffer.lock().await;
        while buffer.is_empty() {
            let mut reader = self.session.mcp_responses.lock().await;
            let packet = tokio::select! {()=self.session.stop.cancelled()=>return Ok(rsi_process::DuplexRead{bytes:vec![],eof:true}),packet=reader.recv()=>packet.ok_or(rsi_process::ProcessError::OutcomeUnknown)?.map_err(|_|rsi_process::ProcessError::OutcomeUnknown)?};
            if packet["kind"] != "mcp" {
                return Err(rsi_process::ProcessError::OutcomeUnknown);
            }
            let mut bytes = serde_json::to_vec(&packet["value"])
                .map_err(|_| rsi_process::ProcessError::OutcomeUnknown)?;
            if bytes.len() > 1024 * 1024 {
                return Err(rsi_process::ProcessError::Capacity);
            }
            bytes.push(b'\n');
            buffer.extend(bytes);
        }
        let length = maximum.min(buffer.len());
        Ok(rsi_process::DuplexRead {
            bytes: buffer.drain(..length).collect(),
            eof: false,
        })
    }
}
#[derive(Debug)]
struct McpControl {
    session: Arc<BrowserSession>,
    input: Arc<McpInput>,
    output: Arc<McpOutput>,
}
#[async_trait::async_trait]
impl rsi_process::DuplexControl for McpControl {
    fn pid(&self) -> u32 {
        self.session.client.pid()
    }
    fn stdin(&self) -> Arc<dyn rsi_process::DuplexInput> {
        self.input.clone()
    }
    fn stdout(&self) -> Arc<dyn rsi_process::DuplexOutput> {
        self.output.clone()
    }
    fn stderr(&self) -> Arc<dyn rsi_process::ProcessOutput> {
        self.session.client.stderr()
    }
    fn terminate(&self) {
        self.session.stop.cancel();
        self.session.client.terminate();
        self.session.browser.terminate();
    }
    async fn wait(&self) -> rsi_process::Result<rsi_process::ProcessOutcome> {
        self.session.client.wait().await
    }
    async fn wait_settlement(&self) -> rsi_process::Result<()> {
        self.session
            .close()
            .await
            .map_err(|_| rsi_process::ProcessError::OutcomeUnknown)
    }
}

pub struct BrowserSession {
    browser: ManagedDuplexProcess,
    client: ManagedDuplexProcess,
    policy: BrowserPolicy,
    commands: Mutex<mpsc::Receiver<Result<Value, String>>>,
    mcp_responses: Mutex<mpsc::Receiver<Result<Value, String>>>,
    writes: Arc<Mutex<()>>,
    tasks: TaskTracker,
    stop: CancellationToken,
    permit: Arc<tokio::sync::OwnedSemaphorePermit>,
}
impl fmt::Debug for BrowserSession {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("BrowserSession").finish_non_exhaustive()
    }
}
impl Drop for BrowserSession {
    fn drop(&mut self) {
        self.stop.cancel();
        self.browser.terminate();
        self.client.terminate();
    }
}
impl BrowserSession {
    async fn write(&self, process: &ManagedDuplexProcess, value: Value) -> Result<(), String> {
        write_packet(&self.writes, &self.stop, process, value).await
    }
    async fn wait_ready(&self, expected: usize) -> Result<(), String> {
        let mut reader = self.commands.lock().await;
        let mut ready = 0;
        scoped_command(
            &self.stop,
            Duration::from_secs(30),
            "browser startup timeout",
            async {
                while ready < expected {
                    let packet = reader.recv().await.ok_or("runtime disconnected")??;
                    if packet["kind"] == "ready" {
                        ready += 1;
                    } else if packet["kind"] == "error" {
                        return Err(packet["error"].as_str().unwrap_or("runtime failed").into());
                    }
                }
                Ok(())
            },
        )
        .await
    }
    /// # Errors
    /// Rejects invalid predicates or failed and retired browser exchanges.
    pub async fn check(&self, spec: CheckSpec) -> Result<(CheckResult, Vec<Vec<u8>>), String> {
        spec.validate()?;
        let mut reader = self.commands.lock().await;
        scoped_command(
            &self.stop,
            Duration::from_mins(2),
            "checker deadline elapsed",
            async {
                self.write(
                    &self.client,
                    json!({"kind":"check","url":self.policy.entry_url,"spec":spec}),
                )
                .await?;
                let mut artifacts = vec![];
                loop {
                    let packet = reader.recv().await.ok_or("runtime disconnected")??;
                    match packet["kind"].as_str() {
                        Some("artifact") => {
                            let raw = STANDARD
                                .decode(packet["png"].as_str().ok_or("missing screenshot")?)
                                .map_err(|_| "invalid screenshot encoding")?;
                            if artifacts.len() >= 4 {
                                return Err("screenshot count exceeds bound".into());
                            }
                            let capacity = self.permit.clone();
                            let png = self
                                .tasks
                                .spawn_blocking(move || {
                                    let _capacity = capacity;
                                    rsi_media::normalize_artifact_png(
                                        raw.into(),
                                        1280 * 720,
                                        512 * 1024,
                                    )
                                    .map_err(|e| e.to_string())
                                })
                                .await
                                .map_err(|_| "screenshot worker failed")??;
                            artifacts.push(png.to_vec());
                        }
                        Some("checked") => {
                            let mut result: CheckResult =
                                serde_json::from_value(packet["result"].clone())
                                    .map_err(|e| e.to_string())?;
                            filter_evidence(&self.policy, &mut result, &mut artifacts);
                            result.validate()?;
                            return Ok((result, artifacts));
                        }
                        Some("error" | "exit") => return Err("browser helper failed".into()),
                        _ => {}
                    }
                }
            },
        )
        .await
    }
    /// # Errors
    /// Rejects destinations outside the frozen policy or failed and retired browser exchanges.
    pub async fn navigate(&self, url: &str) -> Result<String, String> {
        self.policy.navigate(url)?;
        self.observe_command(json!({"kind":"navigate","url":url}))
            .await
    }
    /// # Errors
    /// Fails on retired transport or malformed preview evidence.
    pub async fn observe(&self) -> Result<String, String> {
        self.observe_command(json!({"kind":"observe"})).await
    }
    async fn observe_command(&self, packet: Value) -> Result<String, String> {
        let mut reader = self.commands.lock().await;
        scoped_command(
            &self.stop,
            Duration::from_secs(25),
            "observation deadline elapsed",
            async {
                self.write(&self.client, packet).await?;
                loop {
                    let packet = reader.recv().await.ok_or("runtime disconnected")??;
                    if packet["kind"] == "observation" {
                        self.policy
                            .navigate(packet["url"].as_str().ok_or("missing observation URL")?)?;
                        let text = packet["snapshot"].as_str().ok_or("missing text snapshot")?;
                        if text.len() > 65536 {
                            return Err("text snapshot exceeds bound".into());
                        }
                        return Ok(text.into());
                    }
                    if packet["kind"] == "error" {
                        return Err("observation failed".into());
                    }
                }
            },
        )
        .await
    }
    async fn current_url(&self) -> Result<(), String> {
        let mut reader = self.commands.lock().await;
        scoped_command(
            &self.stop,
            Duration::from_secs(25),
            "page state deadline elapsed",
            async {
                self.write(&self.browser, json!({"kind":"page_state"}))
                    .await?;
                loop {
                    let packet = reader.recv().await.ok_or("runtime disconnected")??;
                    match packet["kind"].as_str() {
                        Some("page_state") => {
                            self.policy.navigate(
                                packet["url"]
                                    .as_str()
                                    .ok_or("missing structured page URL")?,
                            )?;
                            return Ok(());
                        }
                        Some("error" | "exit") => {
                            return Err("structured page state unavailable".into());
                        }
                        _ => {}
                    }
                }
            },
        )
        .await
    }
    /// # Errors
    /// Fails when the owned confined processes cannot be proven settled.
    pub async fn close(&self) -> Result<(), String> {
        self.stop.cancel();
        self.browser.terminate();
        self.client.terminate();
        let (b, c) = tokio::join!(
            self.browser.wait_settlement(),
            self.client.wait_settlement()
        );
        self.tasks.close();
        self.tasks.wait().await;
        b.map_err(|e| e.to_string())?;
        c.map_err(|e| e.to_string())
    }
}

// An unfinished exchange cannot share its response channel with a successor.
fn filter_evidence(policy: &BrowserPolicy, result: &mut CheckResult, artifacts: &mut Vec<Vec<u8>>) {
    if result.outcome == CheckOutcome::PolicyBlocked || policy.navigate(&result.final_url).is_err()
    {
        result.outcome = CheckOutcome::PolicyBlocked;
        result.snapshot.clear();
        result.assertions.clear();
        artifacts.clear();
        result.evidence_error = Some("Evidence withheld outside deployment policy".into());
    }
}

struct CommandLease<'a> {
    stop: &'a CancellationToken,
    complete: bool,
}
impl Drop for CommandLease<'_> {
    fn drop(&mut self) {
        if !self.complete {
            self.stop.cancel();
        }
    }
}
async fn scoped_command<T>(
    stop: &CancellationToken,
    duration: Duration,
    timeout: &str,
    work: impl std::future::Future<Output = Result<T, String>>,
) -> Result<T, String> {
    let mut lease = CommandLease {
        stop,
        complete: false,
    };
    let result = tokio::select! { biased;
        () = stop.cancelled() => Err("browser scope retired".into()),
        result = tokio::time::timeout(duration, work) => result.unwrap_or_else(|_| Err(timeout.into())),
    };
    lease.complete = result.is_ok();
    result
}
// Internal pumps retain ports and cleanup admission, never the public owner.
#[derive(Clone)]
struct BridgeScope {
    browser: ManagedDuplexProcess,
    client: ManagedDuplexProcess,
    policy: BrowserPolicy,
    writes: Arc<Mutex<()>>,
    tasks: TaskTracker,
    stop: CancellationToken,
}
impl BridgeScope {
    fn from_session(session: &BrowserSession) -> Self {
        Self {
            browser: session.browser.clone(),
            client: session.client.clone(),
            policy: session.policy.clone(),
            writes: session.writes.clone(),
            tasks: session.tasks.clone(),
            stop: session.stop.clone(),
        }
    }
    async fn write(&self, process: &ManagedDuplexProcess, value: Value) -> Result<(), String> {
        write_packet(&self.writes, &self.stop, process, value).await
    }
}

async fn write_packet(
    writes: &Mutex<()>,
    stop: &CancellationToken,
    process: &ManagedDuplexProcess,
    value: Value,
) -> Result<(), String> {
    if stop.is_cancelled() {
        return Err("browser scope retired".into());
    }
    let mut bytes = serde_json::to_vec(&value).map_err(|e| e.to_string())?;
    if bytes.len() > FRAME {
        return Err("runtime packet exceeds frame bound".into());
    }
    bytes.push(b'\n');
    let _writer = tokio::select! { biased; () = stop.cancelled() => return Err("browser scope retired".into()), guard = writes.lock() => guard };
    for chunk in bytes.chunks(65536) {
        let mut offset = 0;
        while offset < chunk.len() {
            let input = process.stdin();
            let written = tokio::select! { biased; () = stop.cancelled() => return Err("browser scope retired".into()), written = input.write(&chunk[offset..]) => written.map_err(|e| e.to_string())? };
            if written == 0 {
                return Err("runtime input disconnected".into());
            }
            offset += written;
        }
    }
    Ok(())
}

async fn read_packets(
    process: ManagedDuplexProcess,
    browser: bool,
    send: mpsc::Sender<(bool, Value)>,
    stop: CancellationToken,
) -> Result<(), String> {
    let mut buffer = vec![];
    let stdout = process.stdout();
    loop {
        let part = tokio::select! {()=stop.cancelled()=>return Ok(()),part=stdout.read(65536)=>part.map_err(|e|e.to_string())?};
        for byte in part.bytes {
            if byte == b'\n' {
                let packet =
                    serde_json::from_slice(&buffer).map_err(|_| "invalid helper packet")?;
                buffer.clear();
                relay(&send, (browser, packet), &stop).await?;
            } else {
                if buffer.len() >= FRAME {
                    return Err("helper frame exceeds bound".into());
                }
                buffer.push(byte);
            }
        }
        if part.eof {
            return Err("helper closed its output".into());
        }
    }
}
#[expect(
    clippy::too_many_lines,
    reason = "Keep one complete ownership operation or acceptance scenario together"
)]
async fn bridge(
    session: Arc<BridgeScope>,
    output: mpsc::Sender<Result<Value, String>>,
    mcp_output: mpsc::Sender<Result<Value, String>>,
    #[cfg(feature = "test-support")] fixture: Option<u16>,
) -> Result<(), String> {
    let (send, mut packets) = mpsc::channel(16);
    for (browser, process) in [
        (true, session.browser.clone()),
        (false, session.client.clone()),
    ] {
        let send = send.clone();
        let errors = output.clone();
        let stop = session.stop.clone();
        session.tasks.spawn(async move {
            if let Err(e) = read_packets(process, browser, send, stop.clone()).await {
                let _ = errors.try_send(Err(e));
                stop.cancel();
            }
        });
    }
    let mut sockets: BTreeMap<u64, tokio::net::tcp::OwnedWriteHalf> = BTreeMap::new();
    let bytes = Arc::new(std::sync::atomic::AtomicU64::new(0));
    loop {
        let (browser, packet) = tokio::select! {()=session.stop.cancelled()=>return Ok(()),packet=packets.recv()=>packet.ok_or("browser bridge closed")?};
        match packet["kind"].as_str() {
            Some("cdp") => {
                session
                    .write(
                        if browser {
                            &session.client
                        } else {
                            &session.browser
                        },
                        packet,
                    )
                    .await?;
            }
            Some("proxy_open") if browser => {
                let id = packet["id"].as_u64().ok_or("invalid proxy identity")?;
                let host = packet["host"].as_str().ok_or("invalid proxy host")?;
                let port = packet["port"]
                    .as_u64()
                    .and_then(|p| u16::try_from(p).ok())
                    .ok_or("invalid proxy port")?;
                let addresses =
                    if session.policy.allows_destination(host, port) && sockets.len() < 8 {
                        #[cfg(feature = "test-support")]
                        if let Some(fixture_port) =
                            fixture.filter(|_| host.ends_with(".fixture.invalid"))
                        {
                            Ok(vec![std::net::SocketAddr::from((
                                [127, 0, 0, 1],
                                fixture_port,
                            ))])
                        } else {
                            rsi_retrieval::resolve_public_destination(host, port).await
                        }
                        #[cfg(not(feature = "test-support"))]
                        rsi_retrieval::resolve_public_destination(host, port).await
                    } else {
                        Err(rsi_retrieval::RetrievalError::BlockedUrl)
                    };
                let stream = match addresses {
                    Ok(addresses) => tokio::time::timeout(
                        Duration::from_secs(5),
                        tokio::net::TcpStream::connect(addresses.as_slice()),
                    )
                    .await
                    .ok()
                    .and_then(Result::ok),
                    Err(_) => None,
                };
                if let Some(stream) = stream {
                    let (mut reader, writer) = stream.into_split();
                    sockets.insert(id, writer);
                    session
                        .write(&session.browser, json!({"kind":"proxy_opened","id":id}))
                        .await?;
                    let child = session.clone();
                    let total = bytes.clone();
                    session.tasks.spawn(async move{let mut buffer=vec![0u8;32_768].into_boxed_slice();loop{let length=tokio::select!{()=child.stop.cancelled()=>break,length=reader.read(&mut buffer)=>match length{Ok(0)|Err(_)=>break,Ok(length)=>length}};if !reserve_proxy_bytes(&total, length){child.stop.cancel();break;}
if child.write(&child.browser,json!({"kind":"proxy_data","id":id,"data":STANDARD.encode(&buffer[..length])})).await.is_err(){break;}}let _=child.write(&child.browser,json!({"kind":"proxy_close","id":id})).await;});
                } else {
                    session
                        .write(&session.browser, json!({"kind":"proxy_close","id":id}))
                        .await?;
                }
            }
            Some("proxy_data") if browser => {
                let id = packet["id"].as_u64().ok_or("invalid proxy identity")?;
                let value = packet["data"].as_str().ok_or("invalid proxy bytes")?;
                if value.len() > 44000 {
                    return Err("proxy chunk exceeds bound".into());
                }
                let chunk = STANDARD
                    .decode(value)
                    .map_err(|_| "invalid proxy encoding")?;
                if chunk.len() > 32768 || !reserve_proxy_bytes(&bytes, chunk.len()) {
                    return Err("proxy byte budget exceeded".into());
                }
                if let Some(socket) = sockets.get_mut(&id) {
                    tokio::select! {
                        () = session.stop.cancelled() => return Ok(()),
                        result = tokio::time::timeout(Duration::from_secs(5), socket.write_all(&chunk)) =>
                            result.map_err(|_| "proxy write deadline elapsed")?.map_err(|e| e.to_string())?,
                    }
                }
            }
            Some("proxy_close") if browser => {
                if let Some(id) = packet["id"].as_u64() {
                    sockets.remove(&id);
                }
            }
            _ => {
                route_response(&output, &mcp_output, packet, &session.stop).await?;
            }
        }
    }
}

fn reserve_proxy_bytes(total: &std::sync::atomic::AtomicU64, length: usize) -> bool {
    total
        .fetch_update(
            std::sync::atomic::Ordering::AcqRel,
            std::sync::atomic::Ordering::Acquire,
            |used| {
                used.checked_add(length as u64)
                    .filter(|next| *next <= 32 * 1024 * 1024)
            },
        )
        .is_ok()
}

async fn route_response(
    commands: &mpsc::Sender<Result<Value, String>>,
    mcp: &mpsc::Sender<Result<Value, String>>,
    packet: Value,
    stop: &CancellationToken,
) -> Result<(), String> {
    let lane = if packet["kind"] == "mcp" {
        mcp
    } else {
        commands
    };
    relay(lane, Ok(packet), stop).await
}

async fn relay<T>(
    sender: &mpsc::Sender<T>,
    packet: T,
    stop: &CancellationToken,
) -> Result<(), String> {
    tokio::select! {
        biased;
        () = stop.cancelled() => Ok(()),
        result = sender.send(packet) => result.map_err(|_| "browser response owner closed".into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn idle_private_mcp_reader_cannot_block_or_consume_structured_policy_replies() {
        let (commands, mut commands_rx) = mpsc::channel(1);
        let (mcp, mut mcp_rx) = mpsc::channel::<Result<Value, String>>(1);
        let stop = CancellationToken::new();
        let reader = tokio::spawn(async move { mcp_rx.recv().await.unwrap().unwrap() });
        let state = json!({"kind":"page_state","url":"https://preview.example/"});
        route_response(&commands, &mcp, state.clone(), &stop)
            .await
            .unwrap();
        assert_eq!(commands_rx.recv().await.unwrap().unwrap(), state);
        let rpc = json!({"kind":"mcp","value":{"jsonrpc":"2.0","id":1,"result":{}}});
        route_response(&commands, &mcp, rpc.clone(), &stop)
            .await
            .unwrap();
        assert_eq!(reader.await.unwrap(), rpc);
    }
    #[test]
    fn blocked_destination_withholds_all_page_evidence() {
        let policy = BrowserPolicy {
            entry_url: "https://preview.example/scope/".into(),
            path_prefix: "/scope/".into(),
            dependency_hosts: std::collections::BTreeSet::default(),
        };
        let mut result = CheckResult {
            outcome: CheckOutcome::Pass,
            final_url: "https://preview.example/escaped/".into(),
            assertions: vec![crate::AssertionResult {
                assertion: crate::Assertion::TextVisible {
                    text: "Page URL: allowed".into(),
                },
                passed: true,
                detail: "off-policy evidence".into(),
            }],
            snapshot: "private page".into(),
            dialogs_dismissed: 0,
            evidence_error: None,
        };
        let mut images = vec![vec![1, 2, 3]];
        filter_evidence(&policy, &mut result, &mut images);
        assert_eq!(result.outcome, CheckOutcome::PolicyBlocked);
        assert_eq!(result.final_url, "https://preview.example/escaped/");
        assert!(result.snapshot.is_empty() && result.assertions.is_empty() && images.is_empty());
        result.validate().unwrap();
        result.snapshot = "off-policy page".into();
        assert!(result.validate().is_err());
    }
    #[tokio::test(start_paused = true)]
    async fn saturated_response_delivery_retires_when_its_owner_stops() {
        let (send, _receive) = mpsc::channel(16);
        for _ in 0..16 {
            send.try_send(json!({"kind":"event"})).unwrap();
        }
        let stop = CancellationToken::new();
        let delivery = relay(&send, json!({"kind":"last"}), &stop);
        tokio::pin!(delivery);
        assert!(
            std::future::poll_fn(|cx| std::task::Poll::Ready(
                delivery.as_mut().poll(cx).is_pending()
            ))
            .await
        );
        stop.cancel();
        tokio::time::timeout(Duration::from_secs(1), delivery)
            .await
            .expect("close must drain even when nobody reads the response queue")
            .unwrap();
    }
    #[tokio::test(start_paused = true)]
    async fn timed_out_exchange_rejects_late_reply_and_successor() {
        let stop = CancellationToken::new();
        let (send, mut receive) = mpsc::channel(1);
        let result = scoped_command(&stop, Duration::from_secs(25), "deadline", async {
            receive.recv().await.ok_or_else(|| "closed".into())
        })
        .await;
        assert_eq!(result, Err("deadline".into()));
        send.send("old observation").await.unwrap();
        let successor = scoped_command(&stop, Duration::from_secs(25), "deadline", async {
            receive.recv().await.ok_or_else(|| "closed".into())
        })
        .await;
        assert_eq!(successor, Err("browser scope retired".into()));
        assert_eq!(receive.try_recv().unwrap(), "old observation");
    }
    #[tokio::test]
    async fn abandoned_exchange_retires_the_scope() {
        let stop = CancellationToken::new();
        {
            let work = scoped_command::<()>(
                &stop,
                Duration::from_secs(25),
                "deadline",
                std::future::pending(),
            );
            tokio::pin!(work);
            assert!(
                std::future::poll_fn(|cx| std::task::Poll::Ready(
                    work.as_mut().poll(cx).is_pending()
                ))
                .await
            );
        }
        assert!(stop.is_cancelled());
    }
    fn test_digest(path: &std::path::Path) -> Result<String, String> {
        let mut hash = Sha256::new();
        fingerprint(path, path, &mut hash, &mut 0, &mut 0)?;
        Ok(hex::encode(hash.finalize()))
    }
    #[test]
    fn integrity_hash_includes_bin_directories() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir(tmp.path().join(".bin")).unwrap();
        let file = tmp.path().join(".bin/runtime");
        std::fs::write(&file, "one").unwrap();
        let first = test_digest(tmp.path()).unwrap();
        std::fs::write(&file, "two").unwrap();
        assert_ne!(first, test_digest(tmp.path()).unwrap());
        std::fs::create_dir_all(tmp.path().join("node_modules/.bin")).unwrap();
        std::fs::write(tmp.path().join("node_modules/.bin/unused"), "unused").unwrap();
        let before = test_digest(tmp.path()).unwrap();
        std::fs::write(tmp.path().join("node_modules/.bin/unused"), "changed").unwrap();
        assert_ne!(before, test_digest(tmp.path()).unwrap());
    }
    #[cfg(unix)]
    #[test]
    fn command_links_are_hashed_and_cannot_resolve_outside_the_runtime() {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(root.path().join("node_modules/.bin")).unwrap();
        std::fs::write(root.path().join("node_modules/cli"), "fixed").unwrap();
        let link = root.path().join("node_modules/.bin/cli");
        std::os::unix::fs::symlink("../cli", &link).unwrap();
        let before = test_digest(root.path()).unwrap();
        std::fs::write(root.path().join("node_modules/cli"), "replaced").unwrap();
        assert_ne!(before, test_digest(root.path()).unwrap());
        std::fs::remove_file(&link).unwrap();
        std::os::unix::fs::symlink("../../../outside", &link).unwrap();
        assert!(test_digest(root.path()).is_err());
    }
    #[test]
    fn proxy_budget_admits_only_the_remaining_bytes() {
        let bytes = std::sync::atomic::AtomicU64::new(32 * 1024 * 1024 - 1);
        assert!(reserve_proxy_bytes(&bytes, 1));
        assert!(!reserve_proxy_bytes(&bytes, 1));
        assert_eq!(
            bytes.load(std::sync::atomic::Ordering::Acquire),
            32 * 1024 * 1024
        );
    }
}
