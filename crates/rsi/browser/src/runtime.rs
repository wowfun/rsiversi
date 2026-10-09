use crate::session_policy::RuntimePolicy;
use crate::{BrowserPolicy, CheckOutcome, CheckResult, CheckSpec, SessionPolicy};
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
const PROXY_FLOW: &str = include_str!("../runtime/proxy-flow.mjs");
const SESSION_HELPER: &str = include_str!("../runtime/session-helper.mjs");
const HTTP_PROXY: &str = include_str!("../runtime/http-proxy.mjs");
const CDP: &str = include_str!("../runtime/cdp.mjs");
const SETTLEMENT_FAILURE: &str =
    "prior browser process settlement failed; replace the runtime generation";

/// Runtime open failure; capacity refusals precede launch admission.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum OpenError {
    /// The shared runtime has no free slot; no launch was admitted.
    Capacity,
    /// Validation, readiness, launch or settlement failed.
    Unavailable(String),
}
impl fmt::Display for OpenError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Capacity => formatter.write_str("browser capacity reached"),
            Self::Unavailable(detail) => formatter.write_str(detail),
        }
    }
}
impl std::error::Error for OpenError {}
impl From<String> for OpenError {
    fn from(detail: String) -> Self {
        Self::Unavailable(detail)
    }
}
impl From<&str> for OpenError {
    fn from(detail: &str) -> Self {
        Self::Unavailable(detail.into())
    }
}

#[cfg(test)]
#[path = "runtime_tests.rs"]
pub(super) mod ownership_tests;

#[cfg(test)]
#[path = "broker_tests.rs"]
mod broker_tests;

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
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
        let mut buffer = vec![0; 65_536].into_boxed_slice();
        for root in [
            &self.node,
            &self.chromium_directory,
            &self.package_directory,
        ] {
            fingerprint(
                root,
                root,
                &mut hash,
                &mut count,
                &mut bytes,
                &mut buffer,
                FINGERPRINT_LIMITS,
            )?;
        }
        Ok(hex::encode(hash.finalize()))
    }
}
#[derive(Clone, Copy)]
struct FingerprintLimits {
    entries: usize,
    bytes: u64,
    depth: usize,
}
const FINGERPRINT_LIMITS: FingerprintLimits = FingerprintLimits {
    entries: 32768,
    bytes: 1024 * 1024 * 1024,
    depth: 128,
};
fn charge(bytes: &mut u64, amount: u64, limits: FingerprintLimits) -> Result<(), String> {
    *bytes = bytes
        .checked_add(amount)
        .ok_or("runtime artifact byte count overflow")?;
    if *bytes > limits.bytes {
        return Err("runtime artifact exceeds validation budget".into());
    }
    Ok(())
}
fn discover(
    root: &std::path::Path,
    path: &std::path::Path,
    count: &mut usize,
    bytes: &mut u64,
    limits: FingerprintLimits,
) -> Result<(), String> {
    *count = count
        .checked_add(1)
        .ok_or("runtime artifact entry count overflow")?;
    if *count > limits.entries {
        return Err("runtime artifact exceeds validation budget".into());
    }
    charge(
        bytes,
        path.strip_prefix(root)
            .map_err(|e| e.to_string())?
            .as_os_str()
            .as_encoded_bytes()
            .len() as u64,
        limits,
    )
}
fn fingerprint(
    root: &std::path::Path,
    path: &std::path::Path,
    hash: &mut Sha256,
    count: &mut usize,
    bytes: &mut u64,
    buffer: &mut [u8],
    limits: FingerprintLimits,
) -> Result<(), String> {
    discover(root, path, count, bytes, limits)?;
    let mut pending = vec![(path.to_owned(), 0)];
    while let Some((path, depth)) = pending.pop() {
        let metadata = std::fs::symlink_metadata(&path).map_err(|e| e.to_string())?;
        let relative = path
            .strip_prefix(root)
            .map_err(|e| e.to_string())?
            .as_os_str()
            .as_encoded_bytes();
        if metadata.file_type().is_symlink() {
            let command_link = path.parent().is_some_and(|parent| {
                parent.file_name().is_some_and(|n| n == ".bin")
                    && parent
                        .parent()
                        .is_some_and(|p| p.file_name().is_some_and(|n| n == "node_modules"))
            });
            let target = std::fs::read_link(&path).map_err(|e| e.to_string())?;
            let resolved = std::fs::canonicalize(&path).map_err(|e| e.to_string())?;
            if !command_link
                || target.is_absolute()
                || target.as_os_str().len() > 4096
                || !resolved.starts_with(root)
                || !resolved.is_file()
            {
                return Err("runtime artifact contains an unexpected or escaping symlink".into());
            }
            let literal = target.as_os_str().as_encoded_bytes();
            charge(bytes, literal.len() as u64, limits)?;
            hash.update(b"npm-command-link\0");
            hash.update((relative.len() as u64).to_le_bytes());
            hash.update(relative);
            hash.update((literal.len() as u64).to_le_bytes());
            hash.update(literal);
        } else if metadata.is_dir() {
            let mut paths = Vec::new();
            for child in std::fs::read_dir(&path).map_err(|e| e.to_string())? {
                if depth >= limits.depth {
                    return Err("runtime artifact exceeds depth budget".into());
                }
                let child = child.map_err(|e| e.to_string())?.path();
                discover(root, &child, count, bytes, limits)?;
                paths.push(child);
            }
            paths.sort();
            pending.extend(paths.into_iter().rev().map(|child| (child, depth + 1)));
        } else if metadata.is_file() {
            if metadata.len() > limits.bytes.saturating_sub(*bytes) {
                return Err("runtime artifact exceeds validation budget".into());
            }
            hash.update((relative.len() as u64).to_le_bytes());
            hash.update(relative);
            hash.update(metadata.len().to_le_bytes());
            let mut file = std::fs::File::open(&path).map_err(|e| e.to_string())?;
            let mut read = 0_u64;
            loop {
                let length = std::io::Read::read(&mut file, buffer).map_err(|e| e.to_string())?;
                if length == 0 {
                    break;
                }
                read = read
                    .checked_add(length as u64)
                    .ok_or("runtime artifact length overflow")?;
                charge(bytes, length as u64, limits)?;
                if read > metadata.len() {
                    return Err("runtime artifact changed length".into());
                }
                hash.update(&buffer[..length]);
            }
            if read != metadata.len() {
                return Err("runtime artifact changed length".into());
            }
        } else {
            return Err("runtime artifact is not regular storage".into());
        }
    }
    Ok(())
}

/// Existing providers retain OS ownership; this owner holds only browser policy and leases.
#[derive(Clone)]
pub struct NativeRuntime {
    config: RuntimeConfig,
    processes: Arc<dyn DuplexProcess>,
    sandbox: Arc<dyn Sandbox>,
    slots: Arc<tokio::sync::Semaphore>,
    verified: Arc<std::sync::atomic::AtomicBool>,
    diagnostic: Arc<std::sync::Mutex<Option<String>>>,
    preparation: Arc<Mutex<()>>,
    resolver: Arc<rsi_retrieval::PublicDestinationResolver>,
    #[cfg(feature = "test-support")]
    fixture: Option<u16>,
}
impl fmt::Debug for NativeRuntime {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("NativeRuntime").finish_non_exhaustive()
    }
}
static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
struct LaunchReservation {
    permit: Option<Arc<tokio::sync::OwnedSemaphorePermit>>,
    processes: Vec<ManagedDuplexProcess>,
    runtime: tokio::runtime::Handle,
    slots: Arc<tokio::sync::Semaphore>,
    verified: Arc<std::sync::atomic::AtomicBool>,
    diagnostic: Arc<std::sync::Mutex<Option<String>>>,
}
struct SettlementGuard {
    _permit: Arc<tokio::sync::OwnedSemaphorePermit>,
    slots: Arc<tokio::sync::Semaphore>,
    verified: Arc<std::sync::atomic::AtomicBool>,
    diagnostic: Arc<std::sync::Mutex<Option<String>>>,
    completed: bool,
}
async fn settle_processes(processes: &[ManagedDuplexProcess]) -> Result<(), String> {
    let receipts = futures_util::future::join_all(processes.iter().map(|process| async move {
        process
            .wait_settlement()
            .await
            .map_err(|error| format!("process {} settlement: {error}", process.pid()))
    }))
    .await;
    let failures: Vec<_> = receipts.into_iter().filter_map(Result::err).collect();
    if failures.is_empty() {
        Ok(())
    } else {
        Err(failures.join("; "))
    }
}
impl SettlementGuard {
    fn fence(&self) {
        fence_runtime(
            &self.slots,
            &self.verified,
            &self.diagnostic,
            SETTLEMENT_FAILURE,
        );
    }
    async fn settle(self, processes: Vec<ManagedDuplexProcess>) -> Result<(), String> {
        self.settle_after(processes, async {}).await
    }
    async fn settle_after(
        mut self,
        processes: Vec<ManagedDuplexProcess>,
        bridge: impl std::future::Future<Output = ()>,
    ) -> Result<(), String> {
        for process in &processes {
            process.terminate();
        }
        let result = settle_processes(&processes).await;
        if result.is_err() {
            self.fence();
        }
        bridge.await;
        self.completed = true;
        result
    }
}
impl Drop for SettlementGuard {
    fn drop(&mut self) {
        if !self.completed {
            self.fence();
        }
    }
}
fn fence_runtime(
    slots: &tokio::sync::Semaphore,
    verified: &std::sync::atomic::AtomicBool,
    diagnostic: &std::sync::Mutex<Option<String>>,
    reason: &str,
) {
    slots.close();
    verified.store(false, std::sync::atomic::Ordering::Release);
    *diagnostic
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(reason.into());
}

fn schedule_retirement(
    runtime: &tokio::runtime::Handle,
    cleanup: SettlementGuard,
    processes: Vec<ManagedDuplexProcess>,
    tasks: TaskTracker,
    stop: CancellationToken,
) -> tokio::sync::watch::Receiver<Option<Result<(), String>>> {
    let (receipt, waiter) = tokio::sync::watch::channel(None);
    let slots = cleanup.slots.clone();
    let verified = cleanup.verified.clone();
    let diagnostic = cleanup.diagnostic.clone();
    runtime.spawn(async move {
        tokio::select! {()=stop.cancelled()=>{},()=tokio::time::sleep(Duration::from_mins(10))=>stop.cancel()};
        let settled = tokio::time::timeout(Duration::from_secs(30), async move {
            cleanup.settle_after(processes, async move {
                tasks.close();
                tasks.wait().await;
            }).await
        }).await.unwrap_or_else(|_| {
            let reason = "browser retirement exceeded its settlement budget";
            fence_runtime(&slots, &verified, &diagnostic, reason);
            Err(reason.into())
        });
        receipt.send_replace(Some(settled));
    });
    waiter
}

async fn retirement_receipt(
    mut waiter: tokio::sync::watch::Receiver<Option<Result<(), String>>>,
) -> Result<(), String> {
    loop {
        if let Some(result) = waiter.borrow_and_update().clone() {
            return result;
        }
        waiter
            .changed()
            .await
            .map_err(|_| "browser retirement owner interrupted")?;
    }
}
impl LaunchReservation {
    async fn admit(
        &mut self,
        stop: &CancellationToken,
        spawn: impl std::future::Future<Output = Result<ManagedDuplexProcess, String>>,
    ) -> Result<ManagedDuplexProcess, String> {
        if stop.is_cancelled() {
            return Err("browser launch cancelled".into());
        }
        let process = spawn.await?;
        self.processes.push(process.clone());
        if stop.is_cancelled() {
            return Err("browser launch cancelled".into());
        }
        Ok(process)
    }
    fn handoff(&mut self) -> SettlementGuard {
        self.processes.clear();
        SettlementGuard {
            _permit: self.permit.take().expect("launch capacity"),
            slots: self.slots.clone(),
            verified: self.verified.clone(),
            diagnostic: self.diagnostic.clone(),
            completed: false,
        }
    }
    async fn retire(&mut self) -> Result<(), String> {
        if self.permit.is_none() {
            return Ok(());
        }
        let processes = std::mem::take(&mut self.processes);
        self.handoff().settle(processes).await
    }
}
impl Drop for LaunchReservation {
    fn drop(&mut self) {
        if self.permit.is_none() {
            return;
        }
        let processes = std::mem::take(&mut self.processes);
        let cleanup = self.handoff();
        cleanup.fence();
        for process in &processes {
            process.terminate();
        }
        self.runtime.spawn(async move {
            let _ = cleanup.settle(processes).await;
        });
    }
}
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
            preparation: Arc::new(Mutex::new(())),
            resolver: Arc::new(rsi_retrieval::PublicDestinationResolver::new()),
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
                ("proxy-flow.mjs", PROXY_FLOW),
                ("session-helper.mjs", SESSION_HELPER),
                ("http-proxy.mjs", HTTP_PROXY),
                ("cdp.mjs", CDP),
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
                    }
                    .into(),
                    &identity,
                    "checker",
                    CancellationToken::new(),
                )
                .await
                .map_err(|error| error.to_string())?;
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
    pub(super) fn require_ready(&self) -> Result<(), String> {
        if self.slots.is_closed() {
            return Err(self
                .diagnostic()
                .unwrap_or_else(|| SETTLEMENT_FAILURE.into()));
        }
        if !self.is_verified() {
            return Err(self
                .diagnostic()
                .unwrap_or_else(|| "browser runtime has not completed preparation".into()));
        }
        Ok(())
    }
    async fn spawn(
        &self,
        mode: &str,
        unit: &str,
        launch: &mut LaunchReservation,
        stop: &CancellationToken,
    ) -> Result<ManagedDuplexProcess, String> {
        if stop.is_cancelled() {
            return Err("browser launch cancelled".into());
        }
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
        let process = self.spawn_plan(plan, launch, stop).await?;
        if let Err(error) = self
            .sandbox
            .verify_isolated_limits(&request, &self.config.user_runtime_directory)
            .await
        {
            return Err(error.to_string());
        }
        if stop.is_cancelled() {
            return Err("browser launch cancelled".into());
        }
        Ok(process)
    }
    async fn spawn_plan(
        &self,
        plan: rsi_sandbox::ConfinedProcess,
        launch: &mut LaunchReservation,
        stop: &CancellationToken,
    ) -> Result<ManagedDuplexProcess, String> {
        launch
            .admit(stop, async {
                self.processes
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
                    .map_err(|e| e.to_string())
            })
            .await
    }
    /// # Errors
    /// Fails when the selected resources cannot be validated, exclusively owned or made ready.
    pub async fn open(
        &self,
        policy: BrowserPolicy,
        identity: &str,
        cancellation: CancellationToken,
    ) -> Result<Arc<BrowserSession>, OpenError> {
        self.require_ready()?;
        self.open_kind(policy.into(), identity, "checker", cancellation)
            .await
    }
    /// # Errors
    /// Rejects invalid policy, unavailable isolation, exhausted capacity or a failed Browser launch.
    pub async fn open_exploration(
        &self,
        policy: BrowserPolicy,
        identity: &str,
        cancellation: CancellationToken,
    ) -> Result<ExplorationBrowser, OpenError> {
        self.require_ready()?;
        let cancellation = cancellation.child_token();
        let abandon = cancellation.clone().drop_guard();
        let session = self
            .open_kind(policy.into(), identity, "mcp", cancellation)
            .await?;
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
                return Err(e.to_string().into());
            }
        };
        abandon.disarm();
        Ok(ExplorationBrowser {
            session,
            mcp,
            operations: Mutex::new(()),
        })
    }
    /// Opens one typed Session helper under the same two-slot runtime pool.
    /// # Errors
    /// Rejects invalid policy, unavailable isolation, capacity pressure or failed launch.
    pub async fn open_session(
        &self,
        policy: SessionPolicy,
        identity: &str,
        cancellation: CancellationToken,
    ) -> Result<Arc<BrowserSession>, OpenError> {
        self.require_ready()?;
        self.open_kind(
            RuntimePolicy::Session(policy),
            identity,
            "session",
            cancellation,
        )
        .await
    }
    async fn open_kind(
        &self,
        policy: RuntimePolicy,
        identity: &str,
        mode: &str,
        cancellation: CancellationToken,
    ) -> Result<Arc<BrowserSession>, OpenError> {
        policy.validate()?;
        if identity.len() > 48
            || identity.is_empty()
            || !identity
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-')
        {
            return Err("invalid browser scope identity".into());
        }
        let permit = Arc::new(self.slots.clone().try_acquire_owned().map_err(|error| {
            match error {
                tokio::sync::TryAcquireError::Closed => OpenError::Unavailable(
                    self.diagnostic()
                        .unwrap_or_else(|| SETTLEMENT_FAILURE.into()),
                ),
                tokio::sync::TryAcquireError::NoPermits => OpenError::Capacity,
            }
        })?);
        let stop = cancellation.child_token();
        let abandon = stop.clone().drop_guard();
        let runtime = tokio::runtime::Handle::current();
        let mut launch = LaunchReservation {
            permit: Some(permit),
            processes: vec![],
            runtime: runtime.clone(),
            slots: self.slots.clone(),
            verified: self.verified.clone(),
            diagnostic: self.diagnostic.clone(),
        };
        let owner = self.clone();
        let identity = identity.to_owned();
        let mode = mode.to_owned();
        let result = runtime
            .spawn(async move {
                let result = owner
                    .launch(policy, &identity, &mode, stop, &mut launch)
                    .await;
                if result.is_err() {
                    launch.retire().await?;
                }
                result
            })
            .await
            .map_err(|error| format!("browser launch task failed: {error}"))?;
        if result.is_ok() {
            abandon.disarm();
        }
        result.map_err(OpenError::from)
    }
    async fn launch(
        &self,
        policy: RuntimePolicy,
        identity: &str,
        mode: &str,
        stop: CancellationToken,
        launch: &mut LaunchReservation,
    ) -> Result<Arc<BrowserSession>, String> {
        let browser = self
            .spawn(
                "browser",
                &format!("rsi-browser-{identity}-browser"),
                launch,
                &stop,
            )
            .await?;
        let client = self
            .spawn(
                mode,
                &format!("rsi-browser-{identity}-client"),
                launch,
                &stop,
            )
            .await?;
        let permit = launch.permit.as_ref().expect("launch capacity").clone();
        let (send, receive) = mpsc::channel(16);
        let (mcp_output, mcp_receive) = mpsc::channel(16);
        let tasks = TaskTracker::new();
        let registration = tasks.token();
        let cleanup = launch.handoff();
        let settlement = schedule_retirement(
            &launch.runtime,
            cleanup,
            vec![browser.clone(), client.clone()],
            tasks.clone(),
            stop.clone(),
        );
        let session = Arc::new(BrowserSession {
            browser: browser.clone(),
            client: client.clone(),
            policy,
            resolver: self.resolver.clone(),
            commands: Mutex::new(receive),
            mcp_responses: Mutex::new(mcp_receive),
            writes: Arc::new(Mutex::new(())),
            tasks,
            stop: stop.clone(),
            permit: permit.clone(),
            settlement,
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
        drop(registration);
        // Initialization is explicit. A failed command never establishes replay safety.
        #[cfg(feature = "test-support")]
        let fixture_tls = self.fixture.is_some();
        #[cfg(not(feature = "test-support"))]
        let fixture_tls = false;
        let initialized = async {
            session
                .write(&browser, json!({"kind":"init","fixture_tls":fixture_tls,"session_policy":match &session.policy {RuntimePolicy::Session(p)=>Some(p),RuntimePolicy::Preview(_)=>None}}))
                .await?;
            session.wait_ready(1).await?;
            session
                .write(&client, {
                    let mut entropy = [0u8; 32];
                    getrandom::fill(&mut entropy)
                        .map_err(|_| "private bridge entropy unavailable")?;
                    json!({"kind":"init","token":hex::encode(entropy),"policy":match &session.policy {RuntimePolicy::Session(p)=>Some(p),RuntimePolicy::Preview(_)=>None}})
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
    policy: RuntimePolicy,
    resolver: Arc<rsi_retrieval::PublicDestinationResolver>,
    commands: Mutex<mpsc::Receiver<Result<Value, String>>>,
    mcp_responses: Mutex<mpsc::Receiver<Result<Value, String>>>,
    writes: Arc<Mutex<()>>,
    tasks: TaskTracker,
    stop: CancellationToken,
    permit: Arc<tokio::sync::OwnedSemaphorePermit>,
    settlement: tokio::sync::watch::Receiver<Option<Result<(), String>>>,
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
    /// Runs one owner-validated typed Session operation. A known business refusal
    /// is a complete response; transport errors retire the scope without replay.
    /// # Errors
    /// Rejects retired scopes, malformed or oversized output and uncertain operations.
    pub async fn session_command(
        &self,
        mut packet: Value,
        duration: Duration,
    ) -> Result<Value, String> {
        if !matches!(self.policy, RuntimePolicy::Session(_)) {
            return Err("Session operation requires a Session helper".into());
        }
        packet
            .as_object_mut()
            .ok_or("invalid Session command")?
            .insert("kind".into(), "session".into());
        let mut reader = self.commands.lock().await;
        scoped_command(
            &self.stop,
            duration,
            "Session browser deadline elapsed",
            async {
                self.write(&self.client, packet).await?;
                loop {
                    let packet = reader.recv().await.ok_or("runtime disconnected")??;
                    match packet["kind"].as_str() {
                        Some("session_result") => {
                            let result = packet["result"].clone();
                            validate_session_result(&self.policy, &result)?;
                            return Ok(result);
                        }
                        Some("error" | "exit") => {
                            return Err(
                                "Session browser outcome unknown; retire without replay".into()
                            );
                        }
                        _ => {}
                    }
                }
            },
        )
        .await
    }
    /// Read-only lifetime state; reading it never renews browser ownership.
    pub fn is_retired(&self) -> bool {
        self.stop.is_cancelled()
    }
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
                    json!({"kind":"check","url":self.policy.preview()?.entry_url,"spec":spec}),
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
                            filter_evidence(self.policy.preview()?, &mut result, &mut artifacts);
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
        retirement_receipt(self.settlement.clone()).await
    }
}

fn validate_session_result(policy: &RuntimePolicy, result: &Value) -> Result<(), String> {
    let object = result.as_object().ok_or("invalid Session result object")?;
    match result["status"].as_str() {
        Some("completed" | "screenshot_unavailable") => {
            policy.navigate(
                result["url"]
                    .as_str()
                    .ok_or("missing settled Session URL")?,
            )?;
        }
        Some("not_started") => {}
        _ => return Err("invalid Session result status".into()),
    }
    if let Some(snapshot) = object.get("snapshot") {
        let url = snapshot["url"].as_str().ok_or("missing snapshot URL")?;
        policy.navigate(url)?;
        if result["url"] != url {
            return Err("Session snapshot URL differs from settled URL".into());
        }
        if serde_json::to_vec(snapshot)
            .map_err(|error| error.to_string())?
            .len()
            > 65536
        {
            return Err("Session structure exceeds bound".into());
        }
    }
    if let Some(png) = object.get("png") {
        let encoded = png.as_str().ok_or("invalid Session PNG")?;
        let maximum = crate::session::MAXIMUM_SCREENSHOT_BYTES.div_ceil(3) * 4;
        if encoded.len() > maximum || (encoded.len() == maximum && !encoded.ends_with("==")) {
            return Err("Session PNG exceeds source bound".into());
        }
    }
    Ok(())
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
    policy: RuntimePolicy,
    resolver: Arc<rsi_retrieval::PublicDestinationResolver>,
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
            resolver: session.resolver.clone(),
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
async fn bridge(
    session: Arc<BridgeScope>,
    output: mpsc::Sender<Result<Value, String>>,
    mcp_output: mpsc::Sender<Result<Value, String>>,
    #[cfg(feature = "test-support")] fixture: Option<u16>,
) -> Result<(), String> {
    let connector = Arc::new(PublicProxyConnector {
        local: session.policy.local_destination(),
        resolver: session.resolver.clone(),
        #[cfg(feature = "test-support")]
        fixture,
    });
    bridge_with_connector(session, output, mcp_output, connector).await
}
#[expect(
    clippy::too_many_lines,
    reason = "Keep the bounded router and its packet admission decisions together"
)]
async fn bridge_with_connector(
    session: Arc<BridgeScope>,
    output: mpsc::Sender<Result<Value, String>>,
    mcp_output: mpsc::Sender<Result<Value, String>>,
    connector: Arc<dyn ProxyConnector>,
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
    let (events, mut completions) = mpsc::channel(16);
    let capacity = Arc::new(tokio::sync::Semaphore::new(8));
    let mut sockets: BTreeMap<u64, ProxyOwner> = BTreeMap::new();
    let bytes = Arc::new(std::sync::atomic::AtomicU64::new(0));
    let mut last_identity = 0;
    loop {
        let packet = tokio::select! {
            biased;
            () = session.stop.cancelled() => return Ok(()),
            event = completions.recv() => {
                let event = event.ok_or("proxy completion lane closed")?;
                match event {
                    ProxyEvent::Opened(id) => {
                        if sockets.get(&id).is_some_and(|owner| !owner.stop.is_cancelled()) {
                            session.write(&session.browser, json!({"kind":"proxy_opened","id":id})).await?;
                        }
                    }
                    ProxyEvent::Data(id, chunk, credit) => {
                        if let Some(owner) = sockets.get_mut(&id) && !owner.stop.is_cancelled() {
                            owner.outbound.push_back((chunk.len(), credit));
                            session.write(&session.browser, json!({"kind":"proxy_data","id":id,"data":STANDARD.encode(chunk)})).await?;
                        }
                    }
                    ProxyEvent::Written(id, length) => {
                        if let Some(owner) = sockets.get_mut(&id) && !owner.stop.is_cancelled() {
                            if owner.inbound.pop_front() != Some(length) { return Err("proxy write receipt mismatch".into()); }
                            session.write(&session.browser, json!({"kind":"proxy_ack","id":id,"bytes":length})).await?;
                        }
                    }
                    ProxyEvent::Closed(id) => {
                        sockets.remove(&id);
                        session.write(&session.browser, json!({"kind":"proxy_close","id":id})).await?;
                    }
                }
                continue;
            }
            packet = packets.recv() => packet.ok_or("browser bridge closed")?,
        };
        let (browser, packet) = packet;
        match packet["kind"].as_str() {
            Some("cdp") => {
                packet["value"]
                    .as_str()
                    .ok_or("invalid private CDP payload")?;
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
                let id = packet["id"]
                    .as_u64()
                    .filter(|id| *id > last_identity)
                    .ok_or("invalid or reused proxy identity")?;
                last_identity = id;
                let host = packet["host"]
                    .as_str()
                    .ok_or("invalid proxy host")?
                    .to_owned();
                let port = packet["port"]
                    .as_u64()
                    .and_then(|p| u16::try_from(p).ok())
                    .ok_or("invalid proxy port")?;
                let permit = capacity.clone().try_acquire_owned();
                if !session.policy.allows(
                    &host,
                    port,
                    packet["transport"]
                        .as_str()
                        .ok_or("missing typed proxy transport")?,
                ) || permit.is_err()
                {
                    session
                        .write(&session.browser, json!({"kind":"proxy_close","id":id}))
                        .await?;
                    continue;
                }
                let permit = permit.expect("checked proxy admission");
                let stop = session.stop.child_token();
                let (commands, input) = mpsc::channel(2);
                sockets.insert(
                    id,
                    ProxyOwner {
                        commands,
                        stop: stop.clone(),
                        inbound: std::collections::VecDeque::new(),
                        outbound: std::collections::VecDeque::new(),
                    },
                );
                let child = session.clone();
                let events = events.clone();
                let total = bytes.clone();
                let connector = connector.clone();
                session.tasks.spawn(async move {
                    let _permit = permit;
                    let connecting = connector.connect(&host, port);
                    let result =
                        proxy_socket(id, connecting, input, events.clone(), stop.clone(), total)
                            .await;
                    if result
                        .as_ref()
                        .is_err_and(|error| error == "proxy byte budget exceeded")
                    {
                        child.stop.cancel();
                    }
                    if result.is_err() {
                        stop.cancel();
                    }
                    let _ = relay(&events, ProxyEvent::Closed(id), &child.stop).await;
                });
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
                if chunk.is_empty() || chunk.len() > 32768 {
                    return Err("proxy chunk exceeds bound".into());
                }
                if let Some(owner) = sockets.get_mut(&id)
                    && !owner.stop.is_cancelled()
                {
                    owner.send(chunk, &bytes)?;
                }
            }
            Some("proxy_ack") if browser => {
                let id = packet["id"].as_u64().ok_or("invalid proxy identity")?;
                let length = packet["bytes"]
                    .as_u64()
                    .and_then(|n| usize::try_from(n).ok())
                    .ok_or("invalid proxy receipt")?;
                if let Some(owner) = sockets.get_mut(&id)
                    && !owner.stop.is_cancelled()
                {
                    let (expected, credit) = owner
                        .outbound
                        .pop_front()
                        .ok_or("unexpected proxy receipt")?;
                    if expected != length {
                        return Err("proxy receipt mismatch".into());
                    }
                    drop(credit);
                }
            }
            Some("proxy_close") if browser => {
                let id = packet["id"].as_u64().ok_or("invalid proxy identity")?;
                if let Some(owner) = sockets.get(&id) {
                    owner.stop.cancel();
                }
            }
            _ => route_response(&output, &mcp_output, packet, &session.stop).await?,
        }
    }
}

// Peer: ../runtime/proxy-flow.mjs::ProxySocket; wire bounds: ../README.md.
// inbound mirrors helper pending until Written; outbound retains socket-read
// permits until the helper finishes receive/write and returns proxy_ack.
struct ProxyOwner {
    commands: mpsc::Sender<Vec<u8>>,
    stop: CancellationToken,
    inbound: std::collections::VecDeque<usize>,
    outbound: std::collections::VecDeque<(usize, tokio::sync::OwnedSemaphorePermit)>,
}
impl ProxyOwner {
    fn send(&mut self, chunk: Vec<u8>, bytes: &std::sync::atomic::AtomicU64) -> Result<(), String> {
        let permit = match self.commands.try_reserve() {
            Ok(permit) => permit,
            Err(mpsc::error::TrySendError::Closed(())) => return Ok(()),
            Err(mpsc::error::TrySendError::Full(())) => {
                return Err("proxy input exceeds credit bound".into());
            }
        };
        if self.inbound.len() >= 2 || !reserve_proxy_bytes(bytes, chunk.len()) {
            return Err("proxy credit or byte budget exceeded".into());
        }
        self.inbound.push_back(chunk.len());
        permit.send(chunk);
        Ok(())
    }
}
#[async_trait::async_trait]
trait ProxyConnector: Send + Sync {
    async fn connect(&self, host: &str, port: u16) -> Result<tokio::net::TcpStream, String>;
}
struct PublicProxyConnector {
    local: Option<(String, u16, std::net::IpAddr)>,
    resolver: Arc<rsi_retrieval::PublicDestinationResolver>,
    #[cfg(feature = "test-support")]
    fixture: Option<u16>,
}
#[async_trait::async_trait]
impl ProxyConnector for PublicProxyConnector {
    async fn connect(&self, host: &str, port: u16) -> Result<tokio::net::TcpStream, String> {
        if let Some((exact, expected, ip)) = &self.local
            && exact == host
            && *expected == port
        {
            return tokio::net::TcpStream::connect(std::net::SocketAddr::new(*ip, port))
                .await
                .map_err(|e| e.to_string());
        }
        #[cfg(feature = "test-support")]
        let addresses =
            if let Some(port) = self.fixture.filter(|_| host.ends_with(".fixture.invalid")) {
                vec![std::net::SocketAddr::from(([127, 0, 0, 1], port))]
            } else {
                self.resolver
                    .resolve(host, port)
                    .await
                    .map_err(|e| e.to_string())?
            };
        #[cfg(not(feature = "test-support"))]
        let addresses = self
            .resolver
            .resolve(host, port)
            .await
            .map_err(|e| e.to_string())?;
        tokio::net::TcpStream::connect(addresses.as_slice())
            .await
            .map_err(|e| e.to_string())
    }
}
impl Drop for ProxyOwner {
    fn drop(&mut self) {
        self.stop.cancel();
    }
}
enum ProxyEvent {
    Opened(u64),
    Data(u64, Vec<u8>, tokio::sync::OwnedSemaphorePermit),
    Written(u64, usize),
    Closed(u64),
}
async fn proxy_socket(
    id: u64,
    connecting: impl std::future::Future<Output = Result<tokio::net::TcpStream, String>>,
    mut input: mpsc::Receiver<Vec<u8>>,
    events: mpsc::Sender<ProxyEvent>,
    stop: CancellationToken,
    total: Arc<std::sync::atomic::AtomicU64>,
) -> Result<(), String> {
    let stream = tokio::select! { biased;
        () = stop.cancelled() => return Ok(()),
        connected = tokio::time::timeout(Duration::from_secs(5), connecting) => connected.map_err(|_| "proxy connection deadline elapsed")??,
    };
    relay(&events, ProxyEvent::Opened(id), &stop).await?;
    let (mut reader, mut writer) = stream.into_split();
    let credits = Arc::new(tokio::sync::Semaphore::new(2));
    let reading = async {
        let mut buffer = vec![0; 32768].into_boxed_slice();
        loop {
            let credit = credits
                .clone()
                .acquire_owned()
                .await
                .map_err(|_| "proxy credits closed")?;
            let length = reader.read(&mut buffer).await.map_err(|e| e.to_string())?;
            if length == 0 {
                return Ok::<_, String>(());
            }
            if !reserve_proxy_bytes(&total, length) {
                return Err("proxy byte budget exceeded".into());
            }
            relay(
                &events,
                ProxyEvent::Data(id, buffer[..length].to_vec(), credit),
                &stop,
            )
            .await?;
        }
    };
    let writing = async {
        while let Some(chunk) = input.recv().await {
            tokio::time::timeout(Duration::from_secs(5), writer.write_all(&chunk))
                .await
                .map_err(|_| "proxy write deadline elapsed")?
                .map_err(|e| e.to_string())?;
            relay(&events, ProxyEvent::Written(id, chunk.len()), &stop).await?;
        }
        Ok::<_, String>(())
    };
    tokio::select! { biased; () = stop.cancelled() => Ok(()), result = reading => result, result = writing => result }
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
        packet.get("value").ok_or("missing private MCP value")?;
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
    async fn missing_mcp_value_is_rejected_before_forwarding_or_substituting_null() {
        let (commands, mut commands_rx) = mpsc::channel(1);
        let (mcp, mut mcp_rx) = mpsc::channel(1);
        let stop = CancellationToken::new();
        assert_eq!(
            route_response(&commands, &mcp, json!({"kind":"mcp"}), &stop).await,
            Err("missing private MCP value".into())
        );
        assert!(commands_rx.try_recv().is_err());
        assert!(mcp_rx.try_recv().is_err());
        let packet = json!({"kind":"mcp","value":null});
        route_response(&commands, &mcp, packet.clone(), &stop)
            .await
            .unwrap();
        assert_eq!(mcp_rx.recv().await.unwrap().unwrap(), packet);
    }
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
        fingerprint(
            path,
            path,
            &mut hash,
            &mut 0,
            &mut 0,
            &mut vec![0; 65_536].into_boxed_slice(),
            FINGERPRINT_LIMITS,
        )?;
        Ok(hex::encode(hash.finalize()))
    }
    fn test_directory() -> tempfile::TempDir {
        tempfile::tempdir_in(std::fs::canonicalize(std::env::temp_dir()).unwrap()).unwrap()
    }
    #[test]
    fn integrity_hash_includes_bin_directories() {
        let tmp = test_directory();
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
        let root = test_directory();
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
