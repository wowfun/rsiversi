use crate::{HostProfileDocument, RsiError, RunningRsi, StandardComposition};
use rsi_session::SessionApplication;
use rsi_session_host::{
    ApprovalBroker, HostEpoch, HostOwnerLease, HostOwnerMetadata, HostOwnerMode, SessionHostError,
    SessionHostPaths,
};
#[cfg(target_os = "linux")]
use rsi_session_host::{
    SessionHostDiagnostics, UdsSessionApplication, UdsSessionServer, owner_process_is_current,
};
use std::sync::Arc;
use std::time::Duration;
use tokio_util::sync::CancellationToken;

const OWNER_DISCOVERY_TIMEOUT: Duration = Duration::from_secs(15);
const OWNER_DISCOVERY_INTERVAL: Duration = Duration::from_millis(50);

/// Selected standard Session application ownership mode.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SessionHostConnectionMode {
    /// Compatible explicit daemon owner.
    Remote,
    /// Private in-process owner holding the same persistent lease.
    Embedded,
}

/// One selected Session application plus any embedded resources it owns.
pub struct SessionHostConnection {
    mode: SessionHostConnectionMode,
    application: Arc<dyn SessionApplication>,
    embedded: Option<EmbeddedSessionHost>,
}

impl std::fmt::Debug for SessionHostConnection {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("SessionHostConnection")
            .field("mode", &self.mode)
            .finish_non_exhaustive()
    }
}

impl SessionHostConnection {
    /// Returns the exact selected ownership mode.
    pub const fn mode(&self) -> SessionHostConnectionMode {
        self.mode
    }

    /// Clones the transport-independent application interface.
    pub fn application(&self) -> Arc<dyn SessionApplication> {
        Arc::clone(&self.application)
    }

    /// Deterministically shuts down an embedded owner; remote connections simply detach.
    pub async fn shutdown(mut self) -> crate::Result<()> {
        let Some(embedded) = self.embedded.take() else {
            return Ok(());
        };
        embedded.shutdown().await
    }
}

struct EmbeddedSessionHost {
    running: Arc<RunningRsi>,
    broker: ApprovalBroker,
    _approval_lease: rsi_approval_protocol::ApprovalLease,
    _owner_lease: HostOwnerLease,
}

impl std::fmt::Debug for EmbeddedSessionHost {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("EmbeddedSessionHost")
            .field("running", &self.running)
            .finish_non_exhaustive()
    }
}

impl EmbeddedSessionHost {
    async fn shutdown(self) -> crate::Result<()> {
        self.broker.stop();
        let outcome = self.running.shutdown().await;
        if outcome.is_clean() {
            Ok(())
        } else {
            Err(RsiError::Boot(format!(
                "embedded Session Host shutdown reported {} cleanup failures",
                outcome.report().total_failures()
            )))
        }
    }
}

/// Connects to an exact compatible daemon or acquires the shared lease for an embedded Host.
///
/// This never starts a daemon. A live starting, embedded, incompatible, or unresponsive owner is
/// waited for only within a fixed bound and is never bypassed by a second Host.
pub async fn connect_or_embed_session_host(
    composition: StandardComposition,
    host_profile: &HostProfileDocument,
) -> crate::Result<SessionHostConnection> {
    let preview = composition.preview_host(host_profile)?;
    let launch_key = preview.launch_key.as_str().to_owned();
    let paths = SessionHostPaths::from_host_paths(composition.paths()).map_err(host_error)?;
    let deadline = tokio::time::Instant::now() + OWNER_DISCOVERY_TIMEOUT;
    loop {
        if let Some(metadata) = paths.read_metadata().map_err(host_error)? {
            #[cfg(target_os = "linux")]
            if owner_process_is_current(&metadata).map_err(host_error)? {
                if !metadata.is_compatible_with_current().map_err(host_error)? {
                    return Err(RsiError::Boot(format!(
                        "the active Session Host has an incompatible protocol or product build; run `rsi host restart --profile {}`",
                        host_profile.id
                    )));
                }
                match metadata.mode {
                    HostOwnerMode::Daemon => {
                        if metadata.launch_key != launch_key {
                            return Err(RsiError::Boot(format!(
                                "the active Session Host has a different launch identity; run `rsi host restart --profile {}`",
                                host_profile.id
                            )));
                        }
                        let socket = metadata.socket_path.as_deref().ok_or_else(|| {
                            RsiError::Boot("active daemon metadata has no endpoint".into())
                        })?;
                        let remote = tokio::time::timeout_at(
                            deadline,
                            UdsSessionApplication::connect(
                                socket,
                                &launch_key,
                                metadata.host_epoch,
                            ),
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
                                    "the active compatible Session Host did not become responsive within {} seconds: {error}",
                                    OWNER_DISCOVERY_TIMEOUT.as_secs()
                                )));
                            }
                            Err(_) => {
                                return Err(RsiError::Boot(format!(
                                    "the active compatible Session Host did not become responsive within {} seconds",
                                    OWNER_DISCOVERY_TIMEOUT.as_secs()
                                )));
                            }
                        };
                        return Ok(SessionHostConnection {
                            mode: SessionHostConnectionMode::Remote,
                            application: Arc::new(remote),
                            embedded: None,
                        });
                    }
                    HostOwnerMode::Embedded => {
                        if tokio::time::Instant::now() >= deadline {
                            return Err(RsiError::Boot(
                                "an embedded Session Host owns the standard paths".into(),
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
                return boot_embedded(composition, host_profile, launch_key, lease).await;
            }
            Err(SessionHostError::OwnerActive) if tokio::time::Instant::now() < deadline => {
                tokio::time::sleep(OWNER_DISCOVERY_INTERVAL).await;
            }
            Err(SessionHostError::OwnerActive) => {
                return Err(RsiError::Boot(format!(
                    "a Session Host owner is active but did not publish a usable endpoint within {} seconds",
                    OWNER_DISCOVERY_TIMEOUT.as_secs()
                )));
            }
            Err(error) => return Err(host_error(error)),
        }
    }
}

async fn boot_embedded(
    composition: StandardComposition,
    host_profile: &HostProfileDocument,
    launch_key: String,
    mut owner_lease: HostOwnerLease,
) -> crate::Result<SessionHostConnection> {
    let booted = boot_session_application(composition, host_profile).await?;
    let epoch = match HostEpoch::generate() {
        Ok(epoch) => epoch,
        Err(error) => {
            booted.shutdown().await;
            return Err(host_error(error));
        }
    };
    let metadata =
        match HostOwnerMetadata::current(HostOwnerMode::Embedded, launch_key, epoch, None) {
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
    Ok(SessionHostConnection {
        mode: SessionHostConnectionMode::Embedded,
        application: Arc::clone(&booted.application),
        embedded: Some(EmbeddedSessionHost {
            running: booted.running,
            broker: booted.broker,
            _approval_lease: booted.approval_lease,
            _owner_lease: owner_lease,
        }),
    })
}

struct BootedSessionApplication {
    running: Arc<RunningRsi>,
    broker: ApprovalBroker,
    approval_lease: rsi_approval_protocol::ApprovalLease,
    application: Arc<dyn SessionApplication>,
}

impl BootedSessionApplication {
    async fn shutdown(self) {
        self.broker.stop();
        let _ = self.running.shutdown().await;
    }
}

async fn boot_session_application(
    composition: StandardComposition,
    host_profile: &HostProfileDocument,
) -> crate::Result<BootedSessionApplication> {
    let running = Arc::new(RunningRsi::boot_host_profile(composition, host_profile).await?);
    let broker = ApprovalBroker::new();
    let approval_lease = match running.register_approval_answerer(Arc::new(broker.clone())) {
        Ok(lease) => lease,
        Err(error) => {
            broker.stop();
            let _ = running.shutdown().await;
            return Err(error);
        }
    };
    let application = match running.session_application_with_approvals(Arc::new(broker.clone())) {
        Ok(application) => Arc::new(application) as Arc<dyn SessionApplication>,
        Err(error) => {
            broker.stop();
            let _ = running.shutdown().await;
            return Err(error);
        }
    };
    Ok(BootedSessionApplication {
        running,
        broker,
        approval_lease,
        application,
    })
}

/// Fully booted explicit daemon generation before admission begins.
#[cfg(target_os = "linux")]
pub struct StandardSessionDaemon {
    running: Arc<RunningRsi>,
    server: UdsSessionServer,
    broker: ApprovalBroker,
    _approval_lease: rsi_approval_protocol::ApprovalLease,
    _owner_lease: HostOwnerLease,
}

#[cfg(target_os = "linux")]
impl std::fmt::Debug for StandardSessionDaemon {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("StandardSessionDaemon")
            .field("running", &self.running)
            .finish_non_exhaustive()
    }
}

#[cfg(target_os = "linux")]
impl StandardSessionDaemon {
    /// Consumes the preacquired owner lease, boots the Host, and publishes its endpoint.
    pub async fn start(
        composition: StandardComposition,
        host_profile: &HostProfileDocument,
        mut owner_lease: HostOwnerLease,
    ) -> crate::Result<Self> {
        let preview = composition.preview_host(host_profile)?;
        let launch_key = preview.launch_key.as_str().to_owned();
        let paths = SessionHostPaths::from_host_paths(composition.paths()).map_err(host_error)?;
        if owner_lease.paths() != &paths {
            return Err(RsiError::Boot(
                "daemon owner lease does not protect the selected Host paths".into(),
            ));
        }
        let booted = boot_session_application(composition, host_profile).await?;
        let epoch = match HostEpoch::generate() {
            Ok(epoch) => epoch,
            Err(error) => {
                booted.shutdown().await;
                return Err(host_error(error));
            }
        };
        let server = match UdsSessionServer::bind(
            &paths,
            Arc::clone(&booted.application),
            &launch_key,
            epoch.clone(),
        ) {
            Ok(server) => server,
            Err(error) => {
                booted.shutdown().await;
                return Err(host_error(error));
            }
        };
        let metadata = match HostOwnerMetadata::current(
            HostOwnerMode::Daemon,
            launch_key,
            epoch,
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
            server,
            broker: booted.broker,
            _approval_lease: booted.approval_lease,
            _owner_lease: owner_lease,
        })
    }

    /// Shares the running Host for signal-driven reloads during serving.
    pub fn running(&self) -> Arc<RunningRsi> {
        Arc::clone(&self.running)
    }

    /// Shares monotonic transport diagnostics for this daemon generation.
    pub fn diagnostics(&self) -> SessionHostDiagnostics {
        self.server.diagnostics()
    }

    /// Serves until cancellation, drains clients, then shuts down all Host resources.
    pub async fn run(self, cancellation: CancellationToken) -> crate::Result<()> {
        let broker = self.broker.clone();
        let broker_shutdown = tokio::spawn({
            let cancellation = cancellation.clone();
            async move {
                cancellation.cancelled().await;
                broker.stop();
            }
        });
        let server = self.server.serve(cancellation).await;
        broker_shutdown.abort();
        self.broker.stop();
        let shutdown = self.running.shutdown().await;
        server.map_err(host_error)?;
        if !shutdown.is_clean() {
            return Err(RsiError::Boot(format!(
                "Session Host daemon shutdown reported {} cleanup failures",
                shutdown.report().total_failures()
            )));
        }
        Ok(())
    }
}

#[allow(clippy::needless_pass_by_value)] // Kept as a direct `map_err` adapter.
fn host_error(error: SessionHostError) -> RsiError {
    RsiError::Boot(error.to_string())
}
