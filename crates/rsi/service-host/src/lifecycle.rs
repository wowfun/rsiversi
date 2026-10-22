use crate::{HostOwnerMetadata, HostOwnerMode, ServiceHostError, ServiceOwnerContract};
use async_trait::async_trait;
use rsi_api_protocol::{ConnectionDescriptionContract, HostEpoch};
use rsi_meta::{
    ActivationPlan, ConfigValue, LocalContract, MetaError, PluginFactory, PreparedActivation,
};
use serde::{Deserialize, Serialize};
use std::{sync::Arc, time::Duration};
use tokio::io::{AsyncRead, AsyncReadExt as _, AsyncWrite, AsyncWriteExt as _};
use tokio_util::sync::CancellationToken;

const MAX_FRAME: usize = 16 * 1024;
const EXCHANGE_TIMEOUT: Duration = Duration::from_secs(3);

/// Frozen Lifecycle v1 projection; unrelated product fields are deliberately ignored.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct LifecycleIdentity {
    /// Native process ID.
    pub pid: u32,
    /// Native creation-time token fencing PID reuse.
    pub process_start_token: String,
    /// Resident or embedded ownership.
    pub mode: HostOwnerMode,
    /// Random Host generation.
    pub host_epoch: HostEpoch,
    /// Canonical pair of owning homes; absent only in legacy records.
    #[serde(default)]
    pub home_authority: Option<String>,
    /// Recorded local product endpoint; control derives its sibling endpoint.
    pub socket_path: Option<std::path::PathBuf>,
}
impl From<&HostOwnerMetadata> for LifecycleIdentity {
    fn from(owner: &HostOwnerMetadata) -> Self {
        Self {
            pid: owner.pid,
            process_start_token: owner.process_start_token.clone(),
            mode: owner.mode,
            host_epoch: owner.host_epoch.clone(),
            home_authority: owner.home_authority.clone(),
            socket_path: owner.socket_path.clone(),
        }
    }
}
impl LifecycleIdentity {
    /// Validates the fixed lifecycle contract without parsing product metadata.
    pub fn validate(&self) -> Result<(), ServiceHostError> {
        crate::owner::validate_process_identity(self.pid, &self.process_start_token)?;
        if let Some(authority) = &self.home_authority {
            crate::owner::validate_launch_key(authority)?;
        }
        match (self.mode, &self.socket_path) {
            (HostOwnerMode::Daemon, Some(path)) => crate::owner::validate_socket_path(path),
            (HostOwnerMode::Embedded, None) => Ok(()),
            _ => Err(ServiceHostError::Invalid(
                "invalid lifecycle endpoint".into(),
            )),
        }
    }
    /// Checks the native process generation and same-user identity.
    pub fn is_current(&self) -> Result<bool, ServiceHostError> {
        self.validate()?;
        crate::owner::process_generation_is_current(self.pid, &self.process_start_token)
    }
    /// Force-stops the exact native generation through a retained process handle.
    pub fn force_stop(&self) -> Result<(), ServiceHostError> {
        self.validate()?;
        crate::owner::signal_generation(
            self.pid,
            &self.process_start_token,
            self.mode,
            crate::HostSignal::ForceStop,
        )
    }
}

/// Fixed lifecycle operations, independent from product API schemas.
#[derive(Clone, Copy, Debug, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LifecycleOperation {
    /// Inspect this exact running generation.
    Status,
    /// Request graceful shutdown of this exact generation.
    Stop,
}
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Request {
    version: u8,
    epoch: HostEpoch,
    operation: LifecycleOperation,
}
/// Bounded lifecycle evidence, readable across product builds.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct LifecycleStatus {
    /// Independent lifecycle protocol version.
    pub version: u8,
    /// Exact generation that answered.
    pub epoch: HostEpoch,
    /// Whether graceful shutdown has already been requested.
    pub stopping: bool,
    /// Frozen native identity; no product schema is embedded.
    pub owner: LifecycleIdentity,
}
/// Read-only process-owner observation of explicit lifecycle shutdown.
#[derive(Debug)]
pub struct LifecycleControl(CancellationToken);
impl LifecycleControl {
    /// Waits for an authenticated request; dropping this waiter has no effect.
    pub async fn stop_requested(&self) {
        self.0.cancelled().await;
    }
}
/// Local observation capability consumed by the daemon's process owner.
#[derive(Debug)]
pub struct LifecycleControlContract;
impl LocalContract for LifecycleControlContract {
    const KEY: &'static str = "rsi.service.lifecycle";
    type Service = LifecycleControl;
}
/// Ordinary plugin owning the independent lifecycle endpoint.
#[derive(Clone, Debug, Default)]
pub struct LifecycleFactory;
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Config {
    launch_key: String,
}

#[async_trait]
impl PluginFactory for LifecycleFactory {
    fn prepare(&self, desired: &ConfigValue) -> rsi_meta::Result<PreparedActivation> {
        let config: Config = serde_json::from_value(desired.clone()).map_err(activation)?;
        crate::owner::validate_launch_key(&config.launch_key).map_err(activation)?;
        Ok(PreparedActivation::with_state(desired.clone(), config, 128)
            .requiring_local::<ServiceOwnerContract>()
            .requiring_local::<ConnectionDescriptionContract>())
    }
    async fn activate(&self, mut plan: ActivationPlan) -> rsi_meta::Result<()> {
        let config = plan.take_state::<Config>()?;
        let owner = plan.local::<ServiceOwnerContract>()?;
        let description = plan.local::<ConnectionDescriptionContract>()?;
        let metadata = HostOwnerMetadata::current(
            HostOwnerMode::Daemon,
            config.launch_key,
            description.host_epoch.clone(),
            description.endpoint_id.clone(),
            Some(owner.paths().socket().into()),
        )
        .map_err(activation)?;
        let paths = owner.paths().lifecycle();
        #[cfg(unix)]
        let (listener, publication) =
            crate::socket::PublishedSocket::bind(&paths).map_err(activation)?;
        #[cfg(windows)]
        let listener = rsi_api_local_transport::PipeListener::bind(
            rsi_api_local_transport::PipeName::new(paths.socket()).map_err(activation)?,
        )
        .map_err(activation)?;
        let control = Arc::new(LifecycleControl(CancellationToken::new()));
        let observed = control.clone();
        let stop = CancellationToken::new();
        let stop_guard = stop.clone().drop_guard();
        let task = plan.context().runtime().execution().spawn(async move {
            let mut tasks = tokio::task::JoinSet::new();
            loop {
                let accepted = tokio::select! { biased;
                    () = stop.cancelled() => break,
                    _ = tasks.join_next(), if !tasks.is_empty() => continue,
                    accepted = listener.accept() => accepted,
                };
                #[cfg(unix)]
                let accepted = accepted.map(|(stream, _)| stream);
                let Ok(stream) = accepted else {
                    tokio::select! {
                        () = stop.cancelled() => break,
                        () = tokio::time::sleep(Duration::from_millis(50)) => {},
                    }
                    continue;
                };
                #[cfg(unix)]
                if !stream
                    .peer_cred()
                    .is_ok_and(|peer| peer.uid() == rustix::process::geteuid().as_raw())
                {
                    continue;
                }
                if tasks.len() == 32 {
                    continue;
                }
                let paths = owner.paths().clone();
                let fallback = metadata.clone();
                let control = observed.clone();
                tasks.spawn(async move {
                    let metadata = tokio::task::spawn_blocking(move || {
                        paths
                            .read_metadata()
                            .ok()
                            .flatten()
                            .filter(|active| active.host_epoch == fallback.host_epoch)
                            .unwrap_or(fallback)
                    })
                    .await;
                    let Ok(metadata) = metadata else {
                        return;
                    };
                    let _ = tokio::time::timeout(
                        EXCHANGE_TIMEOUT,
                        exchange(stream, &metadata, &control),
                    )
                    .await;
                });
            }
            tasks.abort_all();
            while tasks.join_next().await.is_some() {}
            drop(listener);
            #[cfg(unix)]
            drop(publication);
            drop(owner);
        });
        plan.defer(
            "stop lifecycle listener",
            Box::new(move || {
                Box::pin(async move {
                    drop(stop_guard);
                    task.await
                        .map_err(|_| "lifecycle listener failed".to_owned())
                })
            }),
        )?;
        plan.context()
            .provide_local::<LifecycleControlContract>(control)?;
        Ok(())
    }
}

async fn exchange(
    mut stream: impl AsyncRead + AsyncWrite + Unpin,
    metadata: &HostOwnerMetadata,
    control: &LifecycleControl,
) -> Result<(), ServiceHostError> {
    let request: Request = read_frame(&mut stream).await?;
    if request.version != 1 || request.epoch != metadata.host_epoch {
        return Err(ServiceHostError::Invalid(
            "lifecycle generation or version differs".into(),
        ));
    }
    let stop = matches!(request.operation, LifecycleOperation::Stop);
    let response = LifecycleStatus {
        version: 1,
        epoch: metadata.host_epoch.clone(),
        stopping: control.0.is_cancelled() || stop,
        owner: metadata.into(),
    };
    // Once authenticated and admitted, shutdown does not depend on delivery.
    // Keep a guard so a failed write still settles the explicit request.
    let requested = stop.then(|| control.0.clone().drop_guard());
    let sent = write_frame(&mut stream, &response).await;
    drop(requested);
    sent
}

/// Reads status or requests stop without requiring compatible product code.
pub async fn request_lifecycle(
    owner: &HostOwnerMetadata,
    operation: LifecycleOperation,
) -> Result<LifecycleStatus, ServiceHostError> {
    request_lifecycle_identity(&owner.into(), operation).await
}

/// Controls a discovered process without decoding any evolving product fields.
pub async fn request_lifecycle_identity(
    owner: &LifecycleIdentity,
    operation: LifecycleOperation,
) -> Result<LifecycleStatus, ServiceHostError> {
    owner.validate()?;
    if owner.mode != HostOwnerMode::Daemon || !owner.is_current()? {
        return Err(ServiceHostError::Invalid(
            "lifecycle owner is no longer current".into(),
        ));
    }
    let endpoint = crate::owner::lifecycle_endpoint(
        owner
            .socket_path
            .as_ref()
            .ok_or_else(|| ServiceHostError::Invalid("missing endpoint".into()))?,
    );
    tokio::time::timeout(EXCHANGE_TIMEOUT, async {
        #[cfg(unix)]
        let mut stream = {
            let stream = tokio::net::UnixStream::connect(&endpoint)
                .await
                .map_err(crate::owner::io_error)?;
            if stream.peer_cred().map_err(crate::owner::io_error)?.uid()
                != rustix::process::geteuid().as_raw()
            {
                return Err(ServiceHostError::Invalid("foreign lifecycle peer".into()));
            }
            stream
        };
        #[cfg(windows)]
        let mut stream = {
            let current = rsi_process_native::ProcessHandle::open(std::process::id())
                .map_err(crate::owner::io_error)?;
            let expected = rsi_process_native::ProcessIdentity {
                pid: owner.pid,
                start_token: owner.process_start_token.clone(),
                user_sid: current.identity().user_sid.clone(),
            };
            let name = rsi_api_local_transport::PipeName::new(&endpoint)
                .map_err(crate::owner::io_error)?;
            rsi_api_local_transport::connect(&name, &expected)
                .await
                .map_err(crate::owner::io_error)?
        };
        write_frame(
            &mut stream,
            &Request {
                version: 1,
                epoch: owner.host_epoch.clone(),
                operation,
            },
        )
        .await?;
        let response: LifecycleStatus = read_frame(&mut stream).await?;
        response.owner.validate()?;
        if response.version != 1
            || response.epoch != owner.host_epoch
            || response.owner.host_epoch != owner.host_epoch
            || response.owner.pid != owner.pid
            || response.owner.process_start_token != owner.process_start_token
        {
            return Err(ServiceHostError::Invalid(
                "lifecycle response names another generation".into(),
            ));
        }
        Ok(response)
    })
    .await
    .map_err(|_| ServiceHostError::Io("lifecycle deadline elapsed".into()))?
}

async fn read_frame<T: serde::de::DeserializeOwned>(
    stream: &mut (impl AsyncRead + Unpin),
) -> Result<T, ServiceHostError> {
    let length = stream.read_u32().await.map_err(crate::owner::io_error)? as usize;
    if length == 0 || length > MAX_FRAME {
        return Err(ServiceHostError::Invalid(
            "invalid lifecycle frame length".into(),
        ));
    }
    let mut bytes = vec![0; length];
    stream
        .read_exact(&mut bytes)
        .await
        .map_err(crate::owner::io_error)?;
    serde_json::from_slice(&bytes).map_err(|error| ServiceHostError::Invalid(error.to_string()))
}
async fn write_frame<T: Serialize>(
    stream: &mut (impl AsyncWrite + Unpin),
    value: &T,
) -> Result<(), ServiceHostError> {
    let bytes =
        serde_json::to_vec(value).map_err(|error| ServiceHostError::Invalid(error.to_string()))?;
    if bytes.len() > MAX_FRAME {
        return Err(ServiceHostError::Invalid(
            "lifecycle reply exceeds limit".into(),
        ));
    }
    let length =
        u32::try_from(bytes.len()).map_err(|error| ServiceHostError::Invalid(error.to_string()))?;
    stream
        .write_u32(length)
        .await
        .map_err(crate::owner::io_error)?;
    stream
        .write_all(&bytes)
        .await
        .map_err(crate::owner::io_error)?;
    stream.flush().await.map_err(crate::owner::io_error)
}
fn activation(error: impl std::fmt::Display) -> MetaError {
    MetaError::Activation(error.to_string())
}
