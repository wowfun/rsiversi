//! Owned local builds using the ordinary Process and Sandbox providers.
use crate::{NativeAddonBuild, NativeAddonError, NativeAddonReceipt, NativeAddonStore};
use async_trait::async_trait;
use rsi_host::{HostBuilder, HostPaths, Profile, ProfileEntry, RunningHost};
use rsi_meta::{
    ActivationPlan, ConfigValue, Execution, LocalContract, MetaError, PluginFactory,
    PreparedActivation, UpdateMode,
};
use rsi_process::{Process, ProcessContract, ProcessRead, ProcessSpec};
use rsi_sandbox::{Sandbox, SandboxContract, SandboxMode};
use std::{
    ffi::OsString,
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::sync::{Semaphore, oneshot};
use tokio_util::{sync::CancellationToken, task::TaskTracker};

/// Failure before a build result or source publication receipt is available.
#[derive(Debug, thiserror::Error)]
pub enum NativeAddonBuildError {
    /// Invalid or changed source, or source-store publication failure.
    #[error(transparent)]
    Source(#[from] NativeAddonError),
    /// The explicit Sandbox provider rejected the command plan.
    #[error(transparent)]
    Sandbox(#[from] rsi_sandbox::SandboxError),
    /// Spawn, output or managed-group settlement failed.
    #[error(transparent)]
    Process(#[from] rsi_process::ProcessError),
    /// A build already owns this service's bounded admission.
    #[error("native build busy")]
    Busy,
    /// The service has stopped admitting builds.
    #[error("native build service closed")]
    Closed,
    /// Cancellation was observed before process spawn.
    #[error("native build cancelled before spawn")]
    Cancelled,
    /// Owned work ended without publishing a result.
    #[error("native build worker failed")]
    Worker,
}
type Result<T> = std::result::Result<T, NativeAddonBuildError>;

/// Process classification, independent of activation or Session generation.
#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum NativeAddonBuildStatus {
    /// The command settled with exit zero and its source artifact was installed.
    Succeeded,
    /// A nonzero code or signal prevented installation.
    Failed,
    /// The configured command deadline caused termination and settlement.
    TimedOut,
    /// Cancellation caused termination or fenced source publication.
    Cancelled,
}
/// Exact finite outcome with bounded raw output and separate installation receipt.
#[derive(Debug)]
pub struct NativeAddonBuildReport {
    /// Final command classification.
    pub status: NativeAddonBuildStatus,
    /// Input fingerprint captured under the source lock before execution.
    pub source_fingerprint: String,
    /// Actual direct-child exit code after managed-group settlement.
    pub exit_code: Option<i32>,
    /// Actual direct-child terminating signal.
    pub signal: Option<i32>,
    /// Raw stdout tail with loss marker and whole-stream offsets.
    pub stdout: ProcessRead,
    /// Raw stderr tail with loss marker and whole-stream offsets.
    pub stderr: ProcessRead,
    /// Actual Sandbox evidence; local builds explicitly request unconfined access.
    pub enforcement: rsi_sandbox::EnforcementStamp,
    /// Source publication only; never an enable or Runtime apply result.
    pub installed: Option<NativeAddonReceipt>,
}
/// One ordinary build owner's admission and cancellation-safe result seam.
#[async_trait]
pub trait NativeAddonBuildService: std::fmt::Debug + Send + Sync + 'static {
    /// Runs one captured declaration with an exact child environment.
    /// Dropped waits request cancellation; owned work and locks remain until settlement.
    async fn run(
        &self,
        build: Arc<NativeAddonBuild>,
        store: Arc<NativeAddonStore>,
        environment: Vec<(OsString, OsString)>,
        cancel: CancellationToken,
    ) -> Result<NativeAddonBuildReport>;
}
struct BuildContract;
impl LocalContract for BuildContract {
    const KEY: &'static str = "rsi.native-addons.build";
    type Service = dyn NativeAddonBuildService;
}
/// Isolated management Profile containing only ordinary Process, Sandbox and build plugins.
#[derive(Debug)]
pub struct NativeAddonBuildManager {
    host: RunningHost,
    service: Arc<dyn NativeAddonBuildService>,
}
impl NativeAddonBuildManager {
    /// Activates the management lifetime without a Service Host, Loader, Settings or Session.
    pub async fn open(paths: HostPaths) -> crate::Result<Self> {
        let boot = |error: rsi_host::HostError| crate::RsiError::Boot(error.to_string());
        let mut builder = HostBuilder::new(paths);
        builder
            .register_local_contract::<ProcessContract>()
            .map_err(boot)?;
        builder
            .register_local_contract::<rsi_process::ProcessOutputCacheContract>()
            .map_err(boot)?;
        builder
            .register_local_contract::<SandboxContract>()
            .map_err(boot)?;
        builder
            .register_local_contract::<BuildContract>()
            .map_err(boot)?;
        for (name, factory) in [
            (
                "rsi.process.local",
                Arc::new(rsi_process_local::ProcessLocalFactory) as Arc<dyn PluginFactory>,
            ),
            (
                "rsi.sandbox.local",
                Arc::new(rsi_sandbox_local::SandboxLocalFactory::default()),
            ),
            ("rsi.native-addons.build", Arc::new(BuildFactory)),
        ] {
            builder
                .register_linked(
                    name,
                    env!("CARGO_PKG_VERSION"),
                    UpdateMode::RestartRequired,
                    factory,
                )
                .map_err(boot)?;
        }
        let host = builder
            .build()
            .map_err(boot)?
            .start(Profile::new([
                ProfileEntry::new("process", "rsi.process.local", serde_json::json!({})),
                ProfileEntry::new(
                    "sandbox",
                    "rsi.sandbox.local",
                    serde_json::json!({"bubblewrap":[],"landlock":[]}),
                ),
                ProfileEntry::new("build", "rsi.native-addons.build", ConfigValue::Null),
            ]))
            .await
            .map_err(boot)?;
        let Some(service) = host.lookup_local::<BuildContract>() else {
            let status = host.profile_status();
            let _ = host.shutdown().await;
            return Err(crate::RsiError::Boot(format!(
                "native build service unavailable: {status:?}"
            )));
        };
        Ok(Self { host, service })
    }
    /// Returns the service while preserving this manager's lifecycle ownership.
    pub fn service(&self) -> &Arc<dyn NativeAddonBuildService> {
        &self.service
    }
    /// Closes build admission, cancels and joins owned work, then retires providers.
    pub async fn shutdown(self) -> rsi_meta::ShutdownOutcome {
        self.host.shutdown().await
    }
}

#[derive(Debug)]
struct BuildFactory;
#[async_trait]
impl PluginFactory for BuildFactory {
    fn prepare(&self, desired: &ConfigValue) -> rsi_meta::Result<PreparedActivation> {
        if !desired.is_null() {
            return Err(MetaError::InvalidInput(
                "build service configuration must be null".into(),
            ));
        }
        Ok(PreparedActivation::new(ConfigValue::Null)
            .requiring_local::<ProcessContract>()
            .requiring_local::<SandboxContract>())
    }
    async fn activate(&self, plan: ActivationPlan) -> rsi_meta::Result<()> {
        let service = Arc::new(Service {
            process: plan.local::<ProcessContract>()?,
            sandbox: plan.local::<SandboxContract>()?,
            execution: plan.context().runtime().execution().clone(),
            stop: CancellationToken::new(),
            tasks: TaskTracker::new(),
            closed: Mutex::new(false),
            capacity: Arc::new(Semaphore::new(1)),
        });
        let cleanup = service.clone();
        plan.defer(
            "stop and join native builds",
            Box::new(move || {
                Box::pin(async move {
                    *cleanup
                        .closed
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner) = true;
                    cleanup.stop.cancel();
                    cleanup.tasks.close();
                    cleanup.tasks.wait().await;
                    Ok(())
                })
            }),
        )?;
        plan.context().provide_local::<BuildContract>(service)?;
        Ok(())
    }
}
#[derive(Debug)]
struct Service {
    process: Arc<dyn Process>,
    sandbox: Arc<dyn Sandbox>,
    execution: Execution,
    stop: CancellationToken,
    tasks: TaskTracker,
    closed: Mutex<bool>,
    capacity: Arc<Semaphore>,
}
#[async_trait]
impl NativeAddonBuildService for Service {
    async fn run(
        &self,
        build: Arc<NativeAddonBuild>,
        store: Arc<NativeAddonStore>,
        environment: Vec<(OsString, OsString)>,
        cancel: CancellationToken,
    ) -> Result<NativeAddonBuildReport> {
        let (receiver, stop) = {
            let closed = self
                .closed
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if *closed {
                return Err(NativeAddonBuildError::Closed);
            }
            let permit = self
                .capacity
                .clone()
                .try_acquire_owned()
                .map_err(|_| NativeAddonBuildError::Busy)?;
            let stop = self.stop.child_token();
            let cancellation = Cancellation {
                owner: stop.clone(),
                request: cancel,
            };
            let process = self.process.clone();
            let sandbox = self.sandbox.clone();
            let execution = self.execution.clone();
            let (sender, receiver) = oneshot::channel();
            let work = self.tasks.track_future(async move {
                let result = run_owned(
                    build,
                    store,
                    environment,
                    cancellation,
                    process,
                    sandbox,
                    execution,
                )
                .await;
                drop(permit);
                let _ = sender.send(result);
            });
            drop(self.execution.spawn(work));
            (receiver, stop)
        };
        let cancel_on_drop = stop.drop_guard();
        let result = receiver.await.map_err(|_| NativeAddonBuildError::Worker)?;
        cancel_on_drop.disarm();
        result
    }
}
#[derive(Clone)]
struct Cancellation {
    owner: CancellationToken,
    request: CancellationToken,
}
impl Cancellation {
    fn is_cancelled(&self) -> bool {
        self.owner.is_cancelled() || self.request.is_cancelled()
    }
    async fn cancelled(&self) {
        tokio::select! { () = self.owner.cancelled() => {}, () = self.request.cancelled() => {} }
    }
}
struct TerminateOnDrop(rsi_process::ManagedProcess);
impl Drop for TerminateOnDrop {
    fn drop(&mut self) {
        self.0.terminate();
    }
}

async fn run_owned(
    build: Arc<NativeAddonBuild>,
    store: Arc<NativeAddonStore>,
    environment: Vec<(OsString, OsString)>,
    cancel: Cancellation,
    process: Arc<dyn Process>,
    sandbox: Arc<dyn Sandbox>,
    execution: Execution,
) -> Result<NativeAddonBuildReport> {
    if cancel.is_cancelled() {
        return Err(NativeAddonBuildError::Cancelled);
    }
    let source = build.clone();
    let (lock, fingerprint) = execution
        .prepare(move || {
            let lock = source.lock()?;
            Ok::<_, NativeAddonError>((lock, source.fingerprint()?))
        })
        .await
        .map_err(|_| NativeAddonBuildError::Worker)??;
    let mut arguments = vec!["--".to_owned()];
    arguments.extend_from_slice(build.command());
    let confined = sandbox
        .confine(rsi_sandbox::ProcessRequest {
            mode: SandboxMode::DangerFullAccess,
            program: "/usr/bin/env".into(),
            arguments,
            cwd: build.cwd().to_owned(),
            workspace: build.cwd().to_owned(),
        })
        .await?;
    if cancel.is_cancelled() {
        return Err(NativeAddonBuildError::Cancelled);
    }
    let enforcement = confined.stamp.clone();
    let managed = process.spawn(ProcessSpec {
        process: confined,
        stdin: Vec::new(),
        environment,
        stdout_max_bytes: 64 * 1024,
        stderr_max_bytes: 64 * 1024,
        termination_grace_ms: 100,
    })?;
    let _terminate = TerminateOnDrop(managed.clone());
    let (mut status, outcome) = tokio::select! {
        biased;
        () = cancel.cancelled() => { managed.terminate(); (NativeAddonBuildStatus::Cancelled, managed.wait().await?) },
        () = tokio::time::sleep(Duration::from_secs(build.timeout_seconds())) => { managed.terminate(); (NativeAddonBuildStatus::TimedOut, managed.wait().await?) },
        result = managed.wait() => {
            let outcome = result?;
            (if outcome.exit_code == Some(0) { NativeAddonBuildStatus::Succeeded } else { NativeAddonBuildStatus::Failed }, outcome)
        }
    };
    let stdout = managed.stdout().read_from(0)?;
    let stderr = managed.stderr().read_from(0)?;
    let installed = if status == NativeAddonBuildStatus::Succeeded {
        let expected = fingerprint.clone();
        let cancelled = cancel.clone();
        // Move the source lock into owned blocking publication; dropping the
        // result waiter cannot free it while copying or checking inputs.
        let result = execution
            .prepare(move || {
                let _lock = lock;
                build.install(&store, &expected, || cancelled.is_cancelled())
            })
            .await
            .map_err(|_| NativeAddonBuildError::Worker)?;
        match result {
            Err(NativeAddonError::Conflict) if cancel.is_cancelled() => {
                status = NativeAddonBuildStatus::Cancelled;
                None
            }
            result => Some(result?),
        }
    } else {
        drop(lock);
        None
    };
    Ok(NativeAddonBuildReport {
        status,
        source_fingerprint: fingerprint,
        exit_code: outcome.exit_code,
        signal: outcome.signal,
        stdout,
        stderr,
        enforcement,
        installed,
    })
}
