use async_trait::async_trait;
use rsi_host_control::{Command, ReloadResult, Reply, Request, decode_frame, encode_frame};
use rsi_meta::{ActivationPlan, ConfigValue, MetaError, PluginFactory, PreparedActivation};
use rsi_meta_profile::{ProfileControlContract, ReloadOutcome, SourceWatchPolicy};
use rsi_service_host::{LocalServiceListener, LocalStream, ServiceOwnerContract};
use std::time::Duration;
use tokio::io::AsyncReadExt as _;
use tokio_util::sync::CancellationToken;
mod environment;
mod source_build;
use environment::EnvironmentFingerprint;

#[derive(Debug)]
pub(crate) struct Factory {
    policy: SourceWatchPolicy,
    environment: EnvironmentFingerprint,
    packages: std::path::PathBuf,
    interfaces: Vec<rsi_packages::Interface>,
}
impl Factory {
    pub(crate) fn new(
        policy: SourceWatchPolicy,
        credentials: &std::collections::BTreeMap<String, rsi_credentials_protocol::SecretValue>,
        paths: &rsi_host::HostPaths,
        interfaces: Vec<rsi_packages::Interface>,
    ) -> Self {
        Self {
            policy,
            environment: EnvironmentFingerprint::capture(credentials),
            packages: paths.state().join("packages"),
            interfaces,
        }
    }
}
#[async_trait]
impl PluginFactory for Factory {
    fn prepare(&self, config: &ConfigValue) -> rsi_meta::Result<PreparedActivation> {
        if !config.is_null() {
            return Err(failure("Host management takes no configuration"));
        }
        Ok(PreparedActivation::new(config.clone())
            .requiring_local::<ServiceOwnerContract>()
            .requiring_local::<rsi_api_protocol::ConnectionDescriptionContract>()
            .requiring_local::<rsi_process::ProcessContract>()
            .requiring_local::<rsi_sandbox::SandboxContract>()
            .requiring_local::<ProfileControlContract>())
    }
    #[allow(clippy::too_many_lines)] // This bounded command router keeps admission and connection-owned cleanup in one scope.
    async fn activate(&self, plan: ActivationPlan) -> rsi_meta::Result<()> {
        let listener = LocalServiceListener::bind(plan.local::<ServiceOwnerContract>()?, "manage")
            .map_err(failure)?;
        let epoch = plan
            .local::<rsi_api_protocol::ConnectionDescriptionContract>()?
            .host_epoch
            .clone();
        let control = plan.local::<ProfileControlContract>()?;
        let policy = self.policy.clone();
        let environment = self.environment.for_epoch(&epoch);
        let builds = source_build::Manager::new(
            plan.local::<rsi_process::ProcessContract>()?,
            plan.local::<rsi_sandbox::SandboxContract>()?,
            self.packages.clone(),
            self.interfaces.clone(),
        );
        let leases = std::sync::Arc::new(tokio::sync::Semaphore::new(16));
        let stop = CancellationToken::new();
        let guard = stop.clone().drop_guard();
        let task = plan.context().runtime().execution().spawn(async move {
            let mut tasks = tokio::task::JoinSet::new();
            loop {
                let accepted = tokio::select! { biased;
                    () = stop.cancelled() => break,
                    _ = tasks.join_next(), if !tasks.is_empty() => continue,
                    stream = listener.accept() => stream,
                };
                let Ok(mut stream) = accepted else {
                    tokio::select! { () = stop.cancelled() => break, () = tokio::time::sleep(Duration::from_millis(50)) => {} }
                    continue;
                };
                if tasks.len() >= 32 {
                    let _ = tokio::time::timeout(Duration::from_millis(100), encode_frame(&mut stream,
                        &Reply::Error { message: "Host management capacity exhausted".into() })).await;
                    continue;
                }
                let epoch = epoch.clone();
                let control = control.clone();
                let policy = policy.clone();
                let environment = environment.clone();
                let builds = builds.clone();
                let leases = leases.clone();
                let stop = stop.child_token();
                tasks.spawn(async move {
                    let request = tokio::time::timeout(Duration::from_secs(3), decode_frame::<Request>(&mut stream)).await;
                    let Ok(Ok(request)) = request else { return; };
                    if request.epoch != epoch || request.protocol != rsi_host_control::protocol_descriptor() { return; }
                    let _lease = if matches!(request.command, Command::Build { .. } | Command::Watch {}) {
                        if let Ok(lease) = leases.try_acquire() {
                            Some(lease)
                        } else {
                            let _ = tokio::time::timeout(Duration::from_secs(3), encode_frame(&mut stream,
                                &Reply::Error { message: "Host management lease capacity exhausted".into() })).await;
                            return;
                        }
                    } else { None };
                    let reply = match request.command {
                        Command::Build { recipe } => {
                            let mut lease = match builds.acquire(recipe).await {
                                Ok(lease) => lease,
                                Err(failure) => {
                                    let reply = match failure {
                                        source_build::AcquireError::Busy => Reply::BuildBusy {},
                                        source_build::AcquireError::Invalid(message) => Reply::Error { message: message.into() },
                                    };
                                    let _ = tokio::time::timeout(Duration::from_secs(3), encode_frame(&mut stream,
                                        &reply)).await;
                                    return;
                                },
                            };
                            let mut byte = [0];
                            loop {
                                let update = lease.updates.borrow_and_update().clone();
                                if !matches!(tokio::time::timeout(Duration::from_secs(3),
                                    encode_frame(&mut stream, &Reply::Build { update })).await, Ok(Ok(()))) { break; }
                                tokio::select! { biased;
                                    () = stop.cancelled() => break,
                                    _ = stream.read(&mut byte) => break,
                                    changed = lease.updates.changed() => if changed.is_err() { break; },
                                }
                            }
                            return;
                        },
                        Command::Environment { fingerprint } => {
                            if fingerprint.len() != 64 || !fingerprint.bytes().all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)) { return; }
                            Reply::Environment { matches: fingerprint == environment }
                        }
                        Command::Reload {} => match control.reload().await {
                            Ok(outcome) => {
                                let status = outcome.status();
                                Reply::Reloaded {
                                    revision: status.revision(), source_revision: status.source_digest().to_owned(),
                                    outcome: match &outcome {
                                        ReloadOutcome::Applied(_) => ReloadResult::Applied,
                                        ReloadOutcome::Unchanged(_) => ReloadResult::Unchanged,
                                        ReloadOutcome::RestartRequired(_) => ReloadResult::RestartRequired,
                                        ReloadOutcome::RolledBack {..} => ReloadResult::RolledBack,
                                        ReloadOutcome::Degraded {..} => ReloadResult::Degraded,
                                    },
                                }
                            }
                            Err(_) => Reply::Error { message:"Service Profile reload failed; inspect Host diagnostics".into() },
                        },
                        Command::Watch {} => {
                            let Ok(lease) = policy.acquire() else {
                                let _ = tokio::time::timeout(Duration::from_secs(3), encode_frame(&mut stream, &Reply::Error {
                                    message:"Host observation is not leased or has reached capacity".into()
                                })).await;
                                return;
                            };
                            if !matches!(tokio::time::timeout(Duration::from_secs(3), encode_frame(&mut stream, &Reply::Watching {})).await, Ok(Ok(()))) { return; }
                            let mut byte = [0];
                            tokio::select! { () = stop.cancelled() => {}, _ = stream.read(&mut byte) => {} }
                            drop(lease);
                            return;
                        }
                    };
                    let _ = tokio::time::timeout(Duration::from_secs(3), encode_frame(&mut stream, &reply)).await;
                });
            }
            // Profile retirement closes its command worker before child cleanup.
            // Admitted reloads remain owned until that worker settles them.
            while tasks.join_next().await.is_some() {}
            builds.shutdown().await;
            Ok(())
        });
        plan.defer(
            "drain Host management attachments",
            Box::new(move || {
                Box::pin(async move {
                    drop(guard);
                    task.await
                        .map_err(|_| "Host management task failed".to_owned())?
                })
            }),
        )
    }
}

/// Retains a source package build; dropping it closes the native ownership stream.
#[derive(Debug)]
pub struct SourceBuildLease {
    stream: Box<dyn LocalStream>,
    initial: Option<rsi_host_control::BuildUpdate>,
}
impl SourceBuildLease {
    /// Reads coalesced progress; EOF means the Host no longer owns the build.
    pub async fn next(&mut self) -> crate::Result<rsi_host_control::BuildUpdate> {
        if let Some(update) = self.initial.take() {
            return Ok(update);
        }
        match decode_frame::<Reply>(&mut self.stream)
            .await
            .map_err(error)?
        {
            Reply::Build { update } => Ok(update),
            _ => Err(error("unexpected source build update")),
        }
    }
}
/// Source admission distinguishes retryable output ownership from other failures.
#[derive(Debug, thiserror::Error)]
pub enum SourceBuildError {
    /// The previous or conflicting recipe still owns this output.
    #[error("another source recipe owns this output")]
    Busy,
    /// Invalid input, unavailable Host or failed transport.
    #[error(transparent)]
    Failed(#[from] crate::RsiError),
}
/// Acquires a source build from the active Host using only explicit caller authority.
pub async fn acquire_source_build(
    paths: &rsi_host::HostPaths,
    recipe: rsi_host_control::BuildRecipe,
) -> std::result::Result<SourceBuildLease, SourceBuildError> {
    let (stream, reply) = request(paths, Command::Build { recipe }).await?;
    match reply {
        Reply::Build { update } => Ok(SourceBuildLease {
            stream,
            initial: Some(update),
        }),
        Reply::BuildBusy {} => Err(SourceBuildError::Busy),
        _ => Err(error("unexpected source build reply").into()),
    }
}
fn failure(value: impl std::fmt::Display) -> MetaError {
    MetaError::Activation(value.to_string())
}
fn error(value: impl std::fmt::Display) -> crate::RsiError {
    crate::RsiError::Boot(value.to_string())
}

async fn request(
    paths: &rsi_host::HostPaths,
    command: Command,
) -> crate::Result<(Box<dyn LocalStream>, Reply)> {
    let owner = rsi_service_host::ServiceHostPaths::from_host_paths(paths)
        .map_err(error)?
        .read_metadata()
        .map_err(error)?
        .ok_or_else(|| error("no Service Host owner exists"))?;
    let mut stream = tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            match rsi_service_host::connect_local_service(&owner, "manage").await {
                Ok(stream) => return stream,
                Err(_) => tokio::time::sleep(Duration::from_millis(25)).await,
            }
        }
    })
    .await
    .map_err(|_| {
        error("Host management endpoint is unavailable; restart the Host with this rsi build")
    })?;
    let reply = tokio::time::timeout(Duration::from_secs(40), async {
        encode_frame(
            &mut stream,
            &Request {
                epoch: owner.host_epoch,
                protocol: rsi_host_control::protocol_descriptor(),
                command,
            },
        )
        .await?;
        decode_frame::<Reply>(&mut stream).await
    })
    .await
    .map_err(|_| error("Host management response deadline elapsed"))?
    .map_err(error)?;
    if let Reply::Error { message } = &reply {
        return Err(error(message));
    }
    Ok((stream, reply))
}
/// A client-owned configuration observation lease; Drop closes its native stream.
#[derive(Debug)]
pub struct ConfigurationWatch(Box<dyn LocalStream>);
impl ConfigurationWatch {
    /// Waits for Host loss without releasing the observation lease beforehand.
    pub async fn disconnected(&mut self) {
        let mut byte = [0];
        let _ = self.0.read(&mut byte).await;
    }
}
/// Acquires observation from an already running Host without rereading its Profile.
pub async fn acquire_configuration_watch(
    paths: &rsi_host::HostPaths,
) -> crate::Result<ConfigurationWatch> {
    let (stream, reply) = request(paths, Command::Watch {}).await?;
    if !matches!(reply, Reply::Watching {}) {
        return Err(error("unexpected Host watch reply"));
    }
    Ok(ConfigurationWatch(stream))
}
/// Reloads the active Service through the same interface on Windows and Linux.
pub async fn reload_service_host(paths: &rsi_host::HostPaths) -> crate::Result<Reply> {
    let (_, reply) = request(paths, Command::Reload {}).await?;
    if !matches!(reply, Reply::Reloaded { .. }) {
        return Err(error("unexpected Host reload reply"));
    }
    Ok(reply)
}

/// Compares caller inputs without exposing secrets or modifying the active Host.
pub async fn service_environment_matches(paths: &rsi_host::HostPaths) -> crate::Result<bool> {
    let owner = rsi_service_host::ServiceHostPaths::from_host_paths(paths)
        .map_err(error)?
        .read_metadata()
        .map_err(error)?
        .ok_or_else(|| error("no Service Host owner exists"))?;
    let fingerprint = EnvironmentFingerprint::capture(&crate::capture_standard_environment()?)
        .for_epoch(&owner.host_epoch);
    let (_, reply) = request(paths, Command::Environment { fingerprint }).await?;
    match reply {
        Reply::Environment { matches } => Ok(matches),
        _ => Err(error("unexpected Host environment reply")),
    }
}
