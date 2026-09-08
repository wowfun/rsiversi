use crate::{Result, RsiError};
use rsi_application::ScopedProfile;
use rsi_host::{Host, HostPaths, ProfileProgram, RunningHost};
use rsi_meta::{Context, LocalContract, ShutdownOutcome};
use std::sync::Arc;

/// The product can own a service Runtime or one child Profile within an application.
pub(crate) enum ProfileOwner {
    Root(RunningHost),
    Scoped {
        paths: HostPaths,
        profile: Box<ScopedProfile>,
    },
}
impl std::fmt::Debug for ProfileOwner {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Root(host) => host.fmt(formatter),
            Self::Scoped { paths, profile } => formatter
                .debug_struct("ScopedProfile")
                .field("paths", paths)
                .field("profile", profile)
                .finish_non_exhaustive(),
        }
    }
}
impl ProfileOwner {
    pub(crate) async fn start_scoped(
        host: Host,
        paths: HostPaths,
        parent: &Context,
        program: ProfileProgram,
    ) -> Result<Self> {
        let profile = ScopedProfile::start(&host, parent, program)
            .await
            .map_err(error)?;
        Ok(Self::Scoped {
            paths,
            profile: Box::new(profile),
        })
    }

    pub(crate) fn paths(&self) -> Option<&HostPaths> {
        match self {
            Self::Root(host) => host.paths(),
            Self::Scoped { paths, .. } => Some(paths),
        }
    }
    pub(crate) fn lookup_local<C: LocalContract>(&self) -> Option<Arc<C::Service>> {
        match self {
            Self::Root(host) => host.lookup_local::<C>(),
            Self::Scoped { profile, .. } => profile.lookup_local::<C>(),
        }
    }
    pub(crate) async fn reload(&self) -> rsi_host::Result<rsi_host::ReloadOutcome> {
        match self {
            Self::Root(host) => host.reload().await,
            Self::Scoped { profile, .. } => profile.reload().await,
        }
    }
    #[cfg(target_os = "linux")]
    pub(crate) fn subscribe_profile(
        &self,
    ) -> tokio::sync::watch::Receiver<rsi_host::ProfileStatus> {
        match self {
            Self::Root(host) => host.subscribe_profile(),
            Self::Scoped { profile, .. } => profile.subscribe_profile(),
        }
    }
    pub(crate) async fn shutdown(&self) -> ShutdownOutcome {
        match self {
            Self::Root(host) => host.shutdown().await,
            Self::Scoped { profile, .. } => ShutdownOutcome::Complete(profile.shutdown().await),
        }
    }
}
fn error(error: impl std::fmt::Display) -> RsiError {
    RsiError::Boot(error.to_string())
}
