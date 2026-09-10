use crate::owner::validate_launch_key;
use crate::socket::PublishedSocket;
use crate::{HostOwnerLease, ServiceHostDiagnostics, ServiceHostError, ServiceOwnerContract};
use async_trait::async_trait;
use rsi_api_http::LocalHttpService;
use rsi_api_protocol::{ApiDispatchContract, ConnectionDescriptionContract, LocalCompatibilityKey};
use rsi_meta::{
    ActivationPlan, ConfigValue, LocalContract, MetaError, PluginFactory, PreparedActivation,
};
use serde::Deserialize;
use sha2::{Digest as _, Sha256};
use std::{
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};
use tokio::{
    net::UnixListener,
    sync::{Semaphore, watch},
    task::JoinSet,
};
use tokio_util::sync::CancellationToken;

/// Maximum time graceful native listener shutdown waits for connection tasks.
pub const SERVICE_HOST_DRAIN_TIMEOUT: Duration = Duration::from_mins(1);

/// Derives an exact local deployment gate without treating it as authentication.
pub fn local_compatibility_key(
    launch_key: &str,
) -> Result<LocalCompatibilityKey, ServiceHostError> {
    validate_launch_key(launch_key)?;
    let mut digest = Sha256::new();
    digest.update(b"rsi.local.api.v1\0");
    digest.update(launch_key.as_bytes());
    digest.update(crate::service_host_product_build()?.as_bytes());
    Ok(LocalCompatibilityKey::from_bytes(digest.finalize().into()))
}

/// Native socket publication using the shared bounded API HTTP codec.
#[derive(Debug)]
pub struct LocalApiServer {
    listener: UnixListener,
    published: PublishedSocket,
    diagnostics: ServiceHostDiagnostics,
    service: LocalHttpService,
    _owner: Arc<HostOwnerLease>,
}
impl LocalApiServer {
    /// Stages and publishes the leased endpoint, retaining ownership through removal.
    pub fn bind(
        owner: Arc<HostOwnerLease>,
        service: LocalHttpService,
    ) -> Result<Self, ServiceHostError> {
        owner.paths().validate_daemon_endpoint()?;
        let (listener, published) = PublishedSocket::bind(owner.paths())?;
        Ok(Self {
            listener,
            published,
            diagnostics: ServiceHostDiagnostics::with_api(service.diagnostics()),
            service,
            _owner: owner,
        })
    }
    /// Shares counters for socket acceptance, peer identity, task bounds and cleanup.
    pub fn diagnostics(&self) -> ServiceHostDiagnostics {
        self.diagnostics.clone()
    }
    /// Serves until cancelled, then joins or aborts transport tasks within one minute.
    pub async fn serve(self, stop: CancellationToken) -> Result<(), ServiceHostError> {
        let permits = Arc::new(Semaphore::new(128));
        let uid = rustix::process::geteuid().as_raw();
        let mut tasks = JoinSet::new();
        loop {
            let accepted = tokio::select! {
                biased;
                () = stop.cancelled() => break,
                result = tasks.join_next(), if !tasks.is_empty() => {
                    if let Some(Err(_)) = result { self.diagnostics.connection_task_panic(); }
                    continue;
                },
                result = self.listener.accept() => result,
            };
            let Ok((stream, _)) = accepted else {
                self.diagnostics.accept_error();
                tokio::select! {
                    () = stop.cancelled() => break,
                    () = tokio::time::sleep(Duration::from_millis(50)) => {},
                }
                continue;
            };
            self.diagnostics.accepted_connection();
            let Ok(credentials) = stream.peer_cred() else {
                self.diagnostics.peer_credential_error();
                continue;
            };
            if credentials.uid() != uid {
                self.diagnostics.foreign_uid_rejection();
                continue;
            }
            let Ok(permit) = permits.clone().try_acquire_owned() else {
                self.diagnostics.capacity_rejection();
                continue;
            };
            let service = self.service.clone();
            let diagnostics = self.diagnostics.clone();
            let stop = stop.clone();
            tasks.spawn(async move {
                let _permit = permit;
                if let Err(error) = service.serve(stream, stop).await {
                    match error {
                        rsi_api_protocol::ApiError::Capacity => diagnostics.capacity_rejection(),
                        rsi_api_protocol::ApiError::Unauthorized => {
                            diagnostics.peer_credential_error();
                        }
                        _ => diagnostics.service_failure(),
                    }
                }
            });
        }
        drop(self.listener);
        let deadline = tokio::time::Instant::now() + SERVICE_HOST_DRAIN_TIMEOUT;
        while !tasks.is_empty() {
            match tokio::time::timeout_at(deadline, tasks.join_next()).await {
                Ok(Some(Err(_))) => self.diagnostics.connection_task_panic(),
                Ok(_) => {}
                Err(_) => {
                    tasks.abort_all();
                    while let Some(result) = tasks.join_next().await {
                        if result
                            .as_ref()
                            .is_err_and(tokio::task::JoinError::is_cancelled)
                        {
                            self.diagnostics.drain_aborted_connections(1);
                        } else if result.is_err() {
                            self.diagnostics.connection_task_panic();
                        }
                    }
                }
            }
        }
        drop(self.published);
        Ok(())
    }
}

/// Read-only observation of the ordinary local listener's lifetime.
#[derive(Debug)]
pub struct LocalApiListener {
    path: PathBuf,
    diagnostics: ServiceHostDiagnostics,
    stopped: watch::Receiver<Option<Result<(), ServiceHostError>>>,
}
impl LocalApiListener {
    /// Returns the published endpoint, independent of the caller's runtime-directory preference.
    pub fn path(&self) -> &Path {
        &self.path
    }
    /// Returns native connection diagnostics for this generation.
    pub fn diagnostics(&self) -> ServiceHostDiagnostics {
        self.diagnostics.clone()
    }
    /// Observes stop or failure; dropping this waiter does not stop the server.
    pub async fn stopped(&self) -> Result<(), ServiceHostError> {
        let mut receiver = self.stopped.clone();
        loop {
            if let Some(result) = receiver.borrow_and_update().clone() {
                return result;
            }
            receiver.changed().await.map_err(|_| {
                ServiceHostError::Io("local API listener lost its task owner".into())
            })?;
        }
    }
}
/// Local capability identifying one plugin-owned Unix listener.
#[derive(Debug)]
pub struct LocalApiListenerContract;
impl LocalContract for LocalApiListenerContract {
    const KEY: &'static str = "rsi.api.local.listener";
    type Service = LocalApiListener;
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Config {
    launch_key: String,
}
/// Ordinary owner of the shared API's native Unix publication and transport tasks.
#[derive(Clone, Debug, Default)]
pub struct LocalApiFactory;
#[async_trait]
impl PluginFactory for LocalApiFactory {
    fn prepare(&self, desired: &ConfigValue) -> rsi_meta::Result<PreparedActivation> {
        let config: Config = serde_json::from_value(desired.clone())
            .map_err(|error| MetaError::InvalidInput(error.to_string()))?;
        validate_launch_key(&config.launch_key)
            .map_err(|error| MetaError::InvalidInput(error.to_string()))?;
        let bytes = std::mem::size_of::<Config>() + config.launch_key.len();
        Ok(
            PreparedActivation::with_state(desired.clone(), config, bytes)
                .requiring_local::<ServiceOwnerContract>()
                .requiring_local::<ApiDispatchContract>()
                .requiring_local::<ConnectionDescriptionContract>(),
        )
    }
    async fn activate(&self, mut plan: ActivationPlan) -> rsi_meta::Result<()> {
        let config = plan.take_state::<Config>()?;
        let owner = plan.local::<ServiceOwnerContract>()?;
        let execution = plan.context().runtime().execution().clone();
        let description = plan.local::<ConnectionDescriptionContract>()?;
        let key = local_compatibility_key(&config.launch_key)
            .map_err(|error| MetaError::Activation(error.to_string()))?;
        let service = LocalHttpService::new(
            execution.clone(),
            plan.local::<ApiDispatchContract>()?,
            &description,
            key,
        )
        .map_err(|error| MetaError::Activation(error.to_string()))?;
        let server = LocalApiServer::bind(owner.clone(), service)
            .map_err(|error| MetaError::Activation(error.to_string()))?;
        let path = owner.paths().socket().to_owned();
        let diagnostics = server.diagnostics();
        let stop = CancellationToken::new();
        let guard = stop.clone().drop_guard();
        let (sender, stopped) = watch::channel(None);
        let task = execution.spawn(async move {
            let result = server.serve(stop).await;
            drop(owner);
            sender.send_replace(Some(result));
        });
        plan.defer(
            "stop local API listener",
            Box::new(move || {
                Box::pin(async move {
                    drop(guard);
                    task.await
                        .map_err(|_| "local API listener task failed".to_owned())?;
                    Ok(())
                })
            }),
        )?;
        let supply = plan
            .context()
            .provide_local::<LocalApiListenerContract>(Arc::new(LocalApiListener {
                path,
                diagnostics,
                stopped,
            }))?;
        plan.defer(
            "withdraw local API listener",
            Box::new(move || {
                Box::pin(async move {
                    drop(supply);
                    Ok(())
                })
            }),
        )
    }
}
