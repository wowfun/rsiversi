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
        profile: Arc<ScopedProfile>,
    },
    #[cfg(unix)]
    Product {
        owner: Box<Self>,
        profile: Arc<ScopedProfile>,
    },
}
impl std::fmt::Debug for ProfileOwner {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            #[cfg(unix)]
            Self::Product { owner, profile } => formatter
                .debug_struct("ProductProfile")
                .field("owner", owner)
                .field("profile", profile)
                .finish(),
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
    #[cfg(unix)]
    pub(crate) fn follow_catalog(
        &mut self,
        source: Arc<dyn rsi_application::ProfileCatalogSource>,
        program: ProfileProgram,
    ) -> Result<()> {
        match self {
            Self::Scoped { profile, .. } => Arc::get_mut(profile)
                .ok_or_else(|| error("Profile already shared"))?
                .follow_catalog(source, program)
                .map_err(error),
            Self::Root(_) | Self::Product { .. } => Err(RsiError::Boot(
                "catalog following requires a uniquely owned child Profile".into(),
            )),
        }
    }
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
            profile: Arc::new(profile),
        })
    }

    pub(crate) fn paths(&self) -> Option<&HostPaths> {
        match self {
            Self::Root(host) => host.paths(),
            Self::Scoped { paths, .. } => Some(paths),
            #[cfg(unix)]
            Self::Product { owner, .. } => owner.paths(),
        }
    }
    pub(crate) fn lookup_local<C: LocalContract>(&self) -> Option<Arc<C::Service>> {
        match self {
            Self::Root(host) => host.lookup_local::<C>(),
            Self::Scoped { profile, .. } => profile.lookup_local::<C>(),
            #[cfg(unix)]
            Self::Product { profile, .. } => profile.lookup_local::<C>(),
        }
    }
    pub(crate) async fn reload(&self) -> rsi_host::Result<rsi_host::ReloadOutcome> {
        match self {
            Self::Root(host) => host.reload().await,
            Self::Scoped { profile, .. } => profile.reload().await,
            #[cfg(unix)]
            Self::Product { profile, .. } => profile.reload().await,
        }
    }

    pub(crate) fn inspect(
        &self,
        request: rsi_meta::InspectionRequest,
    ) -> rsi_meta::Result<crate::RsiInspection> {
        let (profile_status, profile, runtime) = match self {
            Self::Root(host) => (
                host.profile_status(),
                host.profile_snapshot(),
                host.inspect(request)?,
            ),
            Self::Scoped { profile, .. } => (
                profile.profile_status(),
                profile.profile_snapshot(),
                profile.inspect(request)?,
            ),
            #[cfg(unix)]
            Self::Product { owner, profile } => (
                profile.profile_status(),
                profile.profile_snapshot(),
                owner.inspect(request)?.runtime,
            ),
        };
        Ok(crate::RsiInspection {
            profile_status,
            profile,
            runtime,
        })
    }
    #[cfg(target_os = "linux")]
    pub(crate) fn subscribe_profile(
        &self,
    ) -> tokio::sync::watch::Receiver<rsi_host::ProfileStatus> {
        match self {
            Self::Root(host) => host.subscribe_profile(),
            Self::Scoped { profile, .. } | Self::Product { profile, .. } => {
                profile.subscribe_profile()
            }
        }
    }
    pub(crate) async fn shutdown(&self) -> ShutdownOutcome {
        match self {
            Self::Root(host) => host.shutdown().await,
            Self::Scoped { profile, .. } => ShutdownOutcome::Complete(profile.shutdown().await),
            #[cfg(unix)]
            Self::Product { owner, .. } => Box::pin(owner.shutdown()).await,
        }
    }
}
fn error(error: impl std::fmt::Display) -> RsiError {
    RsiError::Boot(error.to_string())
}
