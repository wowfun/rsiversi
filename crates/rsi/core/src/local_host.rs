use crate::{HostProfileDocument, RsiError, RunningRsi, StandardComposition};
use rsi_api_protocol::HostEpoch;
use rsi_service_host::{
    ApprovalBroker, HostOwnerLease, HostOwnerMetadata, HostOwnerMode, ServiceHostError,
    ServiceHostPaths,
};
#[cfg(target_os = "linux")]
use rsi_service_host::{LocalApiListener, ServiceHostDiagnostics, owner_process_is_current};
use rsi_session_protocol::SessionService;
use std::sync::Arc;
use std::time::Duration;
#[cfg(target_os = "linux")]
use tokio_util::sync::CancellationToken;

const OWNER_DISCOVERY_TIMEOUT: Duration = Duration::from_secs(15);
const OWNER_DISCOVERY_INTERVAL: Duration = Duration::from_millis(50);

/// Selected standard Service Host ownership mode.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ServiceHostConnectionMode {
    /// Compatible explicit daemon owner.
    Remote,
    /// Private in-process owner holding the same persistent lease.
    Embedded,
}

/// One selected Service Host connection plus any embedded resources it owns.
pub struct ServiceHostConnection {
    mode: ServiceHostConnectionMode,
    application: Arc<dyn SessionService>,
    output_cache: Arc<dyn rsi_process::ProcessOutputCache>,
    model_catalog: Arc<dyn rsi_ai_protocol::LanguageModels>,
    workspace: Arc<dyn rsi_workspace_protocol::WorkspaceRegistry>,
    media: Arc<dyn rsi_media_protocol::Media>,
    files: Arc<dyn rsi_session_files::SessionFiles>,
    settings: Arc<dyn rsi_settings_protocol::SettingsAccess>,
    embedded: Option<EmbeddedServiceHost>,
    remote: Option<crate::ProfileOwner>,
}

impl std::fmt::Debug for ServiceHostConnection {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ServiceHostConnection")
            .field("mode", &self.mode)
            .finish_non_exhaustive()
    }
}

impl ServiceHostConnection {
    #[cfg(target_os = "linux")]
    async fn from_remote(host: crate::ProfileOwner) -> crate::Result<Self> {
        let result = (|| {
            Ok(Self {
                mode: ServiceHostConnectionMode::Remote,
                settings: crate::required_local::<rsi_settings_protocol::SettingsAccessContract>(
                    &host,
                    "remote Settings",
                )?,
                application: crate::required_local::<rsi_session_protocol::SessionContract>(
                    &host,
                    "remote Session",
                )?,
                output_cache: crate::required_local::<rsi_process::ProcessOutputCacheContract>(
                    &host,
                    "remote Output",
                )?,
                model_catalog: crate::required_local::<rsi_ai_protocol::LanguageModelsContract>(
                    &host,
                    "remote Models",
                )?,
                workspace: crate::required_local::<
                    rsi_workspace_protocol::WorkspaceRegistryContract,
                >(&host, "remote Workspace")?,
                media: crate::required_local::<rsi_media_protocol::MediaContract>(
                    &host,
                    "remote Media",
                )?,
                files: crate::required_local::<rsi_session_files::SessionFilesContract>(
                    &host,
                    "remote Files",
                )?,
                embedded: None,
                remote: None,
            })
        })();
        match result {
            Ok(mut connection) => {
                connection.remote = Some(host);
                Ok(connection)
            }
            Err(error) => {
                let _ = host.shutdown().await;
                Err(error)
            }
        }
    }
    pub(crate) fn lookup_addon<C: rsi_meta::LocalContract>(&self) -> Option<Arc<C::Service>> {
        if let Some(remote) = &self.remote {
            return remote.lookup_local::<C>();
        }
        self.embedded.as_ref()?.running.lookup_addon::<C>()
    }

    /// Returns the exact selected ownership mode.
    pub const fn mode(&self) -> ServiceHostConnectionMode {
        self.mode
    }

    /// Clones the connected Session domain service.
    pub fn session_service(&self) -> Arc<dyn SessionService> {
        Arc::clone(&self.application)
    }

    /// Clones the connected finite Session workspace browser.
    pub fn session_files(&self) -> Arc<dyn rsi_session_files::SessionFiles> {
        self.files.clone()
    }

    /// Clones the connected Settings inspection and versioned edit capability.
    pub fn settings_access(&self) -> Arc<dyn rsi_settings_protocol::SettingsAccess> {
        self.settings.clone()
    }

    /// Clones the connected independent canonical Media service.
    pub fn media_service(&self) -> Arc<dyn rsi_media_protocol::Media> {
        self.media.clone()
    }

    /// Clones the connected independent Workspace registry.
    pub fn workspace_registry(&self) -> Arc<dyn rsi_workspace_protocol::WorkspaceRegistry> {
        self.workspace.clone()
    }

    /// Clones the connected read-only Language model catalog.
    pub fn language_models(&self) -> Arc<dyn rsi_ai_protocol::LanguageModels> {
        self.model_catalog.clone()
    }

    /// Clones the connected read-only completed Process output capability.
    pub fn output_cache(&self) -> Arc<dyn rsi_process::ProcessOutputCache> {
        self.output_cache.clone()
    }

    /// Deterministically shuts down an embedded owner; remote connections simply detach.
    pub async fn shutdown(mut self) -> crate::Result<()> {
        if let Some(remote) = self.remote.take() {
            let outcome = remote.shutdown().await;
            if !outcome.is_clean() {
                return Err(RsiError::Boot("remote client cleanup failed".into()));
            }
        }
        let Some(embedded) = self.embedded.take() else {
            return Ok(());
        };
        embedded.shutdown().await
    }
}

struct EmbeddedServiceHost {
    running: Arc<RunningRsi>,
    broker: Arc<ApprovalBroker>,
    _owner_lease: Arc<HostOwnerLease>,
}

impl std::fmt::Debug for EmbeddedServiceHost {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("EmbeddedServiceHost")
            .field("running", &self.running)
            .finish_non_exhaustive()
    }
}

impl EmbeddedServiceHost {
    async fn shutdown(self) -> crate::Result<()> {
        self.broker.stop();
        let outcome = self.running.shutdown().await;
        if outcome.is_clean() {
            Ok(())
        } else {
            Err(RsiError::Boot(format!(
                "embedded Service Host shutdown reported {} cleanup failures",
                outcome.report().total_failures()
            )))
        }
    }
}

/// Connects to an exact compatible daemon or acquires the shared lease for an embedded Host.
///
/// Both connection modes mount child Profiles in the supplied application Context.
/// The caller owns its parent lifecycle, including cancellation during bootstrap.
/// This never starts a daemon. A live starting, embedded, incompatible, or unresponsive owner is
/// waited for only within a fixed bound and is never bypassed by a second Host.
pub async fn connect_or_embed_service_host(
    parent: &rsi_meta::Context,
    composition: StandardComposition,
    host_profile: &HostProfileDocument,
) -> crate::Result<ServiceHostConnection> {
    let preview = composition.preview_host(host_profile)?;
    let launch_key = preview.launch_key.as_str().to_owned();
    let paths = ServiceHostPaths::from_host_paths(composition.paths()).map_err(host_error)?;
    let deadline = tokio::time::Instant::now() + OWNER_DISCOVERY_TIMEOUT;
    loop {
        if let Some(metadata) = paths.read_metadata().map_err(host_error)? {
            #[cfg(target_os = "linux")]
            if owner_process_is_current(&metadata).map_err(host_error)? {
                if !metadata.is_compatible_with_current().map_err(host_error)? {
                    return Err(RsiError::Boot(format!(
                        "the active Service Host has an incompatible protocol or product build; run `rsi host restart --profile {}`",
                        host_profile.id
                    )));
                }
                match metadata.mode {
                    HostOwnerMode::Daemon => {
                        if metadata.launch_key != launch_key {
                            return Err(RsiError::Boot(format!(
                                "the active Service Host has a different launch identity; run `rsi host restart --profile {}`",
                                host_profile.id
                            )));
                        }
                        let remote = tokio::time::timeout_at(
                            deadline,
                            crate::client_composition::connect(&metadata, parent, &composition),
                        )
                        .await;
                        let remote = match remote {
                            Ok(Ok(remote)) => remote,
                            Ok(Err(_)) if tokio::time::Instant::now() < deadline => {
                                tokio::time::sleep(OWNER_DISCOVERY_INTERVAL).await;
                                continue;
                            }
                            Ok(Err(error)) => {
                                return Err(RsiError::Boot(format!(
                                    "the active compatible Service Host did not become responsive within {} seconds: {error}",
                                    OWNER_DISCOVERY_TIMEOUT.as_secs()
                                )));
                            }
                            Err(_) => {
                                return Err(RsiError::Boot(format!(
                                    "the active compatible Service Host did not become responsive within {} seconds",
                                    OWNER_DISCOVERY_TIMEOUT.as_secs()
                                )));
                            }
                        };
                        return ServiceHostConnection::from_remote(remote).await;
                    }
                    HostOwnerMode::Embedded => {
                        if tokio::time::Instant::now() >= deadline {
                            return Err(RsiError::Boot(
                                "an embedded Service Host owns the standard paths".into(),
                            ));
                        }
                        tokio::time::sleep(OWNER_DISCOVERY_INTERVAL).await;
                        continue;
                    }
                }
            }
            #[cfg(not(target_os = "linux"))]
            let _ = metadata;
        }

        match HostOwnerLease::try_acquire(paths.clone()) {
            Ok(lease) => {
                return boot_embedded(parent, composition, host_profile, launch_key, lease).await;
            }
            Err(ServiceHostError::OwnerActive) if tokio::time::Instant::now() < deadline => {
                tokio::time::sleep(OWNER_DISCOVERY_INTERVAL).await;
            }
            Err(ServiceHostError::OwnerActive) => {
                return Err(RsiError::Boot(format!(
                    "a Service Host owner is active but did not publish a usable endpoint within {} seconds",
                    OWNER_DISCOVERY_TIMEOUT.as_secs()
                )));
            }
            Err(error) => return Err(host_error(error)),
        }
    }
}

async fn boot_embedded(
    parent: &rsi_meta::Context,
    composition: StandardComposition,
    host_profile: &HostProfileDocument,
    launch_key: String,
    owner_lease: HostOwnerLease,
) -> crate::Result<ServiceHostConnection> {
    let owner_lease = Arc::new(owner_lease);
    let epoch = HostEpoch::generate().map_err(|error| RsiError::Boot(error.to_string()))?;
    let composition = composition.with_service_owner(owner_lease.clone(), epoch.clone())?;
    let booted = boot_service_host(parent, composition, host_profile).await?;
    let metadata = match HostOwnerMetadata::current(
        HostOwnerMode::Embedded,
        launch_key,
        epoch,
        booted.description.endpoint_id.clone(),
        None,
    ) {
        Ok(metadata) => metadata,
        Err(error) => {
            booted.shutdown().await;
            return Err(host_error(error));
        }
    };
    if let Err(error) = owner_lease.publish(&metadata) {
        booted.shutdown().await;
        return Err(host_error(error));
    }
    Ok(ServiceHostConnection {
        mode: ServiceHostConnectionMode::Embedded,
        application: Arc::clone(&booted.application),
        output_cache: booted.output_cache.clone(),
        model_catalog: booted.model_catalog.clone(),
        workspace: booted.workspace.clone(),
        media: booted.media.clone(),
        files: booted.files.clone(),
        settings: booted.settings.clone(),
        remote: None,
        embedded: Some(EmbeddedServiceHost {
            running: booted.running,
            broker: booted.broker,
            _owner_lease: owner_lease,
        }),
    })
}

struct BootedServiceHost {
    settings: Arc<dyn rsi_settings_protocol::SettingsAccess>,
    description: Arc<rsi_api_protocol::ConnectionDescription>,
    running: Arc<RunningRsi>,
    broker: Arc<ApprovalBroker>,
    application: Arc<dyn SessionService>,
    output_cache: Arc<dyn rsi_process::ProcessOutputCache>,
    model_catalog: Arc<dyn rsi_ai_protocol::LanguageModels>,
    workspace: Arc<dyn rsi_workspace_protocol::WorkspaceRegistry>,
    media: Arc<dyn rsi_media_protocol::Media>,
    files: Arc<dyn rsi_session_files::SessionFiles>,
}

impl BootedServiceHost {
    async fn shutdown(self) {
        self.broker.stop();
        let _ = self.running.shutdown().await;
    }
}

async fn boot_service_host(
    parent: &rsi_meta::Context,
    composition: StandardComposition,
    host_profile: &HostProfileDocument,
) -> crate::Result<BootedServiceHost> {
    BootedServiceHost::from_running(
        RunningRsi::boot_host_profile_in(composition, host_profile, parent).await?,
    )
    .await
}

impl BootedServiceHost {
    async fn from_running(running: RunningRsi) -> crate::Result<Self> {
        let running = Arc::new(running);
        let result = (|| {
            Ok(Self {
                description: running.connection_description()?,
                broker: running.approval_broker()?,
                application: running.session_service()?,
                output_cache: running.output_cache()?,
                model_catalog: running.language_models()?,
                workspace: running.workspace_registry()?,
                media: running.media_service()?,
                files: running.session_files()?,
                settings: running.settings_access()?,
                running: running.clone(),
            })
        })();
        if result.is_err() {
            let _ = running.shutdown().await;
        }
        result
    }
}

/// Fully booted explicit daemon generation before admission begins.
#[cfg(target_os = "linux")]
pub struct StandardServiceDaemon {
    running: Arc<RunningRsi>,
    listener: Arc<LocalApiListener>,
    diagnostics: tokio::sync::watch::Sender<ServiceHostDiagnostics>,
    _owner_lease: Arc<HostOwnerLease>,
}

#[cfg(target_os = "linux")]
impl std::fmt::Debug for StandardServiceDaemon {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("StandardServiceDaemon")
            .field("running", &self.running)
            .finish_non_exhaustive()
    }
}

#[cfg(target_os = "linux")]
impl StandardServiceDaemon {
    /// Consumes the preacquired owner lease, boots the Host, and publishes its endpoint.
    pub async fn start(
        composition: StandardComposition,
        host_profile: &HostProfileDocument,
        owner_lease: HostOwnerLease,
    ) -> crate::Result<Self> {
        Self::start_under(composition, host_profile, owner_lease, None).await
    }

    /// Publishes a daemon in an isolated child Profile of the supplied Runtime.
    /// The caller owns the parent lifecycle, including cancellation during startup.
    pub async fn start_in(
        composition: StandardComposition,
        host_profile: &HostProfileDocument,
        owner_lease: HostOwnerLease,
        parent: &rsi_meta::Context,
    ) -> crate::Result<Self> {
        Self::start_under(composition, host_profile, owner_lease, Some(parent)).await
    }

    async fn start_under(
        composition: StandardComposition,
        host_profile: &HostProfileDocument,
        owner_lease: HostOwnerLease,
        parent: Option<&rsi_meta::Context>,
    ) -> crate::Result<Self> {
        let preview = composition.preview_host(host_profile)?;
        let launch_key = preview.launch_key.as_str().to_owned();
        let paths = ServiceHostPaths::from_host_paths(composition.paths()).map_err(host_error)?;
        if owner_lease.paths() != &paths {
            return Err(RsiError::Boot(
                "daemon owner lease does not protect the selected Host paths".into(),
            ));
        }
        let owner_lease = Arc::new(owner_lease);
        let epoch = HostEpoch::generate().map_err(|error| RsiError::Boot(error.to_string()))?;
        let composition = composition.with_service_owner(owner_lease.clone(), epoch.clone())?;
        let booted = BootedServiceHost::from_running(
            RunningRsi::boot_daemon_profile(composition, host_profile, &launch_key, parent).await?,
        )
        .await?;
        let listener = match booted.running.local_api_listener() {
            Ok(listener) => listener,
            Err(error) => {
                booted.shutdown().await;
                return Err(error);
            }
        };
        let metadata = match HostOwnerMetadata::current(
            HostOwnerMode::Daemon,
            launch_key,
            epoch,
            booted.description.endpoint_id.clone(),
            Some(paths.socket().to_owned()),
        ) {
            Ok(metadata) => metadata,
            Err(error) => {
                booted.shutdown().await;
                return Err(host_error(error));
            }
        };
        if let Err(error) = owner_lease.publish(&metadata) {
            booted.shutdown().await;
            return Err(host_error(error));
        }
        Ok(Self {
            running: booted.running,
            diagnostics: tokio::sync::watch::channel(listener.diagnostics()).0,
            listener,
            _owner_lease: owner_lease,
        })
    }

    /// Shares the running Host for signal-driven reloads during serving.
    pub fn running(&self) -> Arc<RunningRsi> {
        Arc::clone(&self.running)
    }

    /// Observes the current listener's monotonic counters, replaced after Profile reload.
    pub fn diagnostics(&self) -> tokio::sync::watch::Receiver<ServiceHostDiagnostics> {
        self.diagnostics.subscribe()
    }

    /// Serves until cancellation, drains clients, then shuts down all Host resources.
    pub async fn run(mut self, cancellation: CancellationToken) -> crate::Result<()> {
        let server = self.supervise(cancellation).await;
        if let Ok(broker) = self.running.approval_broker() {
            broker.stop();
        }
        let shutdown = self.running.shutdown().await;
        server?;
        if !shutdown.is_clean() {
            return Err(RsiError::Boot(format!(
                "Service Host daemon shutdown reported {} cleanup failures",
                shutdown.report().total_failures()
            )));
        }
        Ok(())
    }

    async fn supervise(&mut self, cancellation: CancellationToken) -> crate::Result<()> {
        let mut profile = self.running.host.subscribe_profile();
        let mut listener_stopped = false;
        loop {
            if cancellation.is_cancelled() {
                return Ok(());
            }
            {
                // Retain the status read guard through lookup: convergence publishes
                // Converging before withdrawing any capability in the old suffix.
                let status = profile.borrow_and_update();
                if status.health() == rsi_host::ProfileHealth::Stopped {
                    return Ok(());
                }
                if status.health() != rsi_host::ProfileHealth::Converging {
                    // Parent teardown fences Local lookups before Profile cleanup
                    // publishes Stopped. The restart-required owner precedes every
                    // reloadable leaf and cannot retire as part of suffix replacement.
                    let current = self.running.local_api_listener();
                    if self
                        .running
                        .host
                        .lookup_local::<rsi_service_host::ServiceOwnerContract>()
                        .is_none()
                    {
                        return Ok(());
                    }
                    let current = current?;
                    if !Arc::ptr_eq(&current, &self.listener) {
                        self.listener = current;
                        self.diagnostics.send_replace(self.listener.diagnostics());
                        listener_stopped = false;
                    } else if listener_stopped {
                        return Err(RsiError::Boot(
                            "local API listener stopped without a Profile replacement".into(),
                        ));
                    }
                }
            }
            tokio::select! {
                biased;
                () = cancellation.cancelled() => return Ok(()),
                result = profile.changed() => result.map_err(|_| RsiError::Boot(
                    "Service Host lost its Profile observer".into()
                ))?,
                result = self.listener.stopped(), if !listener_stopped => {
                    result.map_err(host_error)?;
                    listener_stopped = true;
                },
            }
        }
    }
}

#[allow(clippy::needless_pass_by_value)] // Kept as a direct `map_err` adapter.
fn host_error(error: ServiceHostError) -> RsiError {
    RsiError::Boot(error.to_string())
}
