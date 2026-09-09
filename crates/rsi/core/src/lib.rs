//! Standard RSI product composition and service lifecycle.

#![deny(unsafe_code)]
#![warn(missing_docs)]
#![allow(clippy::missing_errors_doc)]

mod addon;
mod agent_preset;
pub use addon::{
    AddonFactoryDescription, AddonScope, MAXIMUM_ADDON_DESCRIPTION_BYTES, MAXIMUM_ADDON_PLATFORMS,
    MAXIMUM_ADDON_SCHEMA_DEPTH, MAXIMUM_FACTORY_DESCRIPTION_BYTES, MAXIMUM_STANDARD_ADDONS,
    StandardAddon, StandardAddonBuilder, StandardAddonSet,
};
mod application_connection;
pub use application_connection::{ApplicationDiagnostics, standard_application_host};
mod api_composition;
#[cfg(target_os = "linux")]
mod client_composition;
#[cfg(target_os = "linux")]
pub use client_composition::probe_service_host;
mod composition;
mod local_host;
mod output_read;
mod profile_owner;
mod profiles;
mod settings;

pub use agent_preset::{
    AGENT_PRESET_SETTINGS_NAMESPACE, AgentPresetManager, DEFAULT_AGENT_PRESET_ID,
    USER_AGENT_PRESET_DIRECTORY, user_agent_preset_root,
};
pub use composition::{
    StandardCodingTools, StandardComposition, StandardHostPreview, capture_standard_environment,
    standard_agent_preset_root,
};
#[cfg(target_os = "linux")]
pub use local_host::StandardServiceDaemon;
pub use local_host::{
    ServiceHostConnection, ServiceHostConnectionMode, connect_or_embed_service_host,
};
pub use profiles::{
    APPLICATION_PROFILE_DIRECTORY, APPLICATION_PROFILE_FILE, ApplicationProfileDocument,
    ApplicationProfileId, HOST_PROFILE_DIRECTORY, HOST_PROFILE_FILE, HostLaunchKey,
    HostProfileDocument, HostProfileId, MAXIMUM_PROFILE_DOCUMENT_BYTES, ProfileCatalog,
    ProfileCatalogError, ProfileRow, ProfileSource,
};
#[cfg(unix)]
pub use profiles::{ProfileEdit, ProfileEditError, ProfileEditReceipt};
pub use rsi_agent_presets::{AgentPresetSource, AgentPresetTrust};
pub use rsi_apply_patch::maybe_run_apply_patch_helper;
pub use rsi_session_protocol;
pub use rsi_shell_bash::scrub_child_environment;

use profile_owner::ProfileOwner;
use rsi_host::HostPaths;
use std::path::{Path, PathBuf};

/// Running standard Host and its application-facing operations.
#[derive(Debug)]
pub struct RunningRsi {
    host: ProfileOwner,
    paths: HostPaths,
}

impl RunningRsi {
    pub(crate) fn lookup_addon<C: rsi_meta::LocalContract>(
        &self,
    ) -> Option<std::sync::Arc<C::Service>> {
        self.host.lookup_local::<C>()
    }

    /// Boots the standard immutable catalog from one required Profile file.
    pub async fn boot(composition: StandardComposition, profile_path: &Path) -> Result<Self> {
        let host = composition
            .build()
            .map_err(|error| RsiError::Boot(error.to_string()))?;
        let host = host
            .start_file(profile_path)
            .await
            .map_err(|error| RsiError::Boot(error.to_string()))?;
        Self::from_started_host(ProfileOwner::Root(host)).await
    }

    /// Boots one catalog-resolved Host Profile document.
    pub async fn boot_host_profile(
        composition: StandardComposition,
        profile: &HostProfileDocument,
    ) -> Result<Self> {
        let host = composition
            .build()
            .map_err(|error| RsiError::Boot(error.to_string()))?;
        Self::start_host_profile(host, profile).await
    }

    /// Boots a standard service child Profile within the caller's application Runtime.
    /// Parent lifetime and bootstrap cancellation belong to the caller; shutdown affects
    /// only this isolated child subtree.
    pub async fn boot_host_profile_in(
        composition: StandardComposition,
        profile: &HostProfileDocument,
        parent: &rsi_meta::Context,
    ) -> Result<Self> {
        let paths = composition.paths().clone();
        let host = composition
            .build()
            .map_err(|error| RsiError::Boot(error.to_string()))?;
        let program = match &profile.path {
            Some(path) => rsi_host::ProfileProgram::from_file(path),
            None => rsi_host::ProfileProgram::from_profile(rsi_host::Profile::default()),
        };
        Self::from_started_host(ProfileOwner::start_scoped(host, paths, parent, program).await?)
            .await
    }

    #[cfg(target_os = "linux")]
    pub(crate) async fn boot_daemon_profile(
        composition: StandardComposition,
        profile: &HostProfileDocument,
        launch_key: &str,
        parent: Option<&rsi_meta::Context>,
    ) -> Result<Self> {
        let paths = composition.paths().clone();
        let host = composition
            .build_daemon(launch_key)
            .map_err(|error| RsiError::Boot(error.to_string()))?;
        match parent {
            Some(parent) => {
                let program = match &profile.path {
                    Some(path) => rsi_host::ProfileProgram::from_file(path),
                    None => rsi_host::ProfileProgram::from_profile(rsi_host::Profile::default()),
                };
                Self::from_started_host(
                    ProfileOwner::start_scoped(host, paths, parent, program).await?,
                )
                .await
            }
            None => Self::start_host_profile(host, profile).await,
        }
    }

    async fn start_host_profile(
        host: rsi_host::Host,
        profile: &HostProfileDocument,
    ) -> Result<Self> {
        let host = match &profile.path {
            Some(path) => host.start_file(path).await,
            None => host.start(rsi_host::Profile::default()).await,
        }
        .map_err(|error| RsiError::Boot(error.to_string()))?;
        Self::from_started_host(ProfileOwner::Root(host)).await
    }

    async fn from_started_host(host: ProfileOwner) -> Result<Self> {
        let Some(paths) = host.paths().cloned() else {
            let _outcome = host.shutdown().await;
            return Err(RsiError::Boot(
                "standard composition requires native Host paths".into(),
            ));
        };
        if host
            .lookup_local::<rsi_session_protocol::SessionContract>()
            .is_none()
        {
            let _outcome = host.shutdown().await;
            return Err(RsiError::Boot(
                "Session service did not become active".into(),
            ));
        }
        Ok(Self { host, paths })
    }

    /// Returns the frozen Host paths.
    pub const fn paths(&self) -> &HostPaths {
        &self.paths
    }

    /// Clones the persisted deployment and live generation identity of this API.
    pub fn connection_description(
        &self,
    ) -> Result<std::sync::Arc<rsi_api_protocol::ConnectionDescription>> {
        required_local::<rsi_api_protocol::ConnectionDescriptionContract>(
            &self.host,
            "connection description",
        )
    }

    /// Clones the shared dispatcher for an explicitly composed server adapter.
    pub fn api_dispatch(&self) -> Result<std::sync::Arc<dyn rsi_api_protocol::ApiDispatch>> {
        required_local::<rsi_api_protocol::ApiDispatchContract>(&self.host, "API dispatcher")
    }

    /// Clones local operator authority; this capability is never registered as a remote API.
    pub fn device_administration(
        &self,
    ) -> Result<std::sync::Arc<dyn rsi_api_protocol::DeviceAdministration>> {
        required_local::<rsi_api_protocol::DeviceAdministrationContract>(
            &self.host,
            "device administration",
        )
    }

    /// Clones the device verifier for an explicitly composed authenticated transport.
    pub fn device_authentication(
        &self,
    ) -> Result<std::sync::Arc<dyn rsi_api_protocol::DeviceAuthentication>> {
        required_local::<rsi_api_protocol::DeviceAuthenticationContract>(
            &self.host,
            "device authentication",
        )
    }

    #[cfg(target_os = "linux")]
    pub(crate) fn local_api_listener(
        &self,
    ) -> Result<std::sync::Arc<rsi_service_host::LocalApiListener>> {
        required_local::<rsi_service_host::LocalApiListenerContract>(
            &self.host,
            "local API listener",
        )
    }

    /// Clones the shared Session plugin service for this Host generation.
    pub fn session_service(
        &self,
    ) -> Result<std::sync::Arc<dyn rsi_session_protocol::SessionService>> {
        required_local::<rsi_session_protocol::SessionContract>(&self.host, "Session service")
    }

    /// Clones the finite workspace browser bound to this Host generation.
    pub fn session_files(&self) -> Result<std::sync::Arc<dyn rsi_session_files::SessionFiles>> {
        required_local::<rsi_session_files::SessionFilesContract>(&self.host, "Session Files")
    }

    /// Clones independent canonical Media upload and read capabilities.
    pub fn media_service(&self) -> Result<std::sync::Arc<dyn rsi_media_protocol::Media>> {
        required_local::<rsi_media_protocol::MediaContract>(&self.host, "Media service")
    }

    /// Clones the independent host Workspace registry.
    pub fn workspace_registry(
        &self,
    ) -> Result<std::sync::Arc<dyn rsi_workspace_protocol::WorkspaceRegistry>> {
        required_local::<rsi_workspace_protocol::WorkspaceRegistryContract>(
            &self.host,
            "Workspace registry",
        )
    }

    /// Clones the read-only committed Language model catalog.
    pub fn language_models(&self) -> Result<std::sync::Arc<dyn rsi_ai_protocol::LanguageModels>> {
        required_local::<rsi_ai_protocol::LanguageModelsContract>(
            &self.host,
            "Language model catalog",
        )
    }

    /// Clones the read-only completed Process output capability.
    pub fn output_cache(&self) -> Result<std::sync::Arc<dyn rsi_process::ProcessOutputCache>> {
        required_local::<rsi_process::ProcessOutputCacheContract>(
            &self.host,
            "Process output cache",
        )
    }

    /// Clones the registered Settings inspection and versioned edit capability.
    pub fn settings_access(
        &self,
    ) -> Result<std::sync::Arc<dyn rsi_settings_protocol::SettingsAccess>> {
        required_local::<rsi_settings_protocol::SettingsAccessContract>(
            &self.host,
            "Settings access",
        )
    }

    pub(crate) fn approval_broker(
        &self,
    ) -> Result<std::sync::Arc<rsi_service_host::ApprovalBroker>> {
        required_local::<rsi_service_host::ApprovalBrokerContract>(&self.host, "Approval broker")
    }

    /// Rebuilds the complete Host Profile source program.
    pub async fn reload(&self) -> Result<rsi_host::ReloadOutcome> {
        self.host
            .reload()
            .await
            .map_err(|error| RsiError::Boot(error.to_string()))
    }

    /// Shuts down this service's Profile and Jobs, preserving a scoped parent's Runtime.
    pub async fn shutdown(&self) -> rsi_meta::ShutdownOutcome {
        self.host.shutdown().await
    }
}

fn required_local<C: rsi_meta::LocalContract>(
    host: &ProfileOwner,
    name: &str,
) -> Result<std::sync::Arc<C::Service>> {
    host.lookup_local::<C>()
        .ok_or_else(|| RsiError::Boot(format!("{name} is unavailable")))
}

/// Resolves standard XDG-style paths without searching for a Profile.
pub fn standard_paths() -> Result<HostPaths> {
    let config = optional_environment_path("XDG_CONFIG_HOME")?;
    let state = optional_environment_path("XDG_STATE_HOME")?;
    let cache = optional_environment_path("XDG_CACHE_HOME")?;
    let home = (config.is_none() || state.is_none() || cache.is_none())
        .then(|| environment_path("HOME"))
        .transpose()?;
    standard_paths_from(config, state, cache, home.as_deref())
}

fn standard_paths_from(
    config: Option<PathBuf>,
    state: Option<PathBuf>,
    cache: Option<PathBuf>,
    home: Option<&Path>,
) -> Result<HostPaths> {
    let fallback = |suffix: &str| {
        home.map(|home| home.join(suffix)).ok_or_else(|| {
            RsiError::Boot("required environment path `HOME` is not configured".into())
        })
    };
    let config = config.map_or_else(|| fallback(".config"), Ok)?.join("rsi");
    let state = state
        .map_or_else(|| fallback(".local/state"), Ok)?
        .join("rsi");
    let cache = cache.map_or_else(|| fallback(".cache"), Ok)?.join("rsi");
    HostPaths::new(config, state, cache).map_err(|error| RsiError::Boot(error.to_string()))
}

fn environment_path(name: &str) -> Result<PathBuf> {
    optional_environment_path(name)?.ok_or_else(|| {
        RsiError::Boot(format!(
            "required environment path `{name}` is not configured"
        ))
    })
}

fn optional_environment_path(name: &str) -> Result<Option<PathBuf>> {
    let Some(value) = std::env::var_os(name) else {
        return Ok(None);
    };
    let path = PathBuf::from(value);
    if !path.is_absolute() {
        return Err(RsiError::Boot(format!(
            "environment path `{name}` must be absolute"
        )));
    }
    Ok(Some(path))
}

pub use rsi_application::RsiError;

/// Standard application result.
pub type Result<T> = std::result::Result<T, RsiError>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn complete_xdg_paths_do_not_require_home() {
        let paths = standard_paths_from(
            Some(PathBuf::from("/config")),
            Some(PathBuf::from("/state")),
            Some(PathBuf::from("/cache")),
            None,
        )
        .unwrap();

        assert_eq!(paths.config(), Path::new("/config/rsi"));
        assert_eq!(paths.state(), Path::new("/state/rsi"));
        assert_eq!(paths.cache(), Path::new("/cache/rsi"));
    }

    #[test]
    fn missing_xdg_path_requires_home_for_its_fallback() {
        assert!(matches!(
            standard_paths_from(
                Some(PathBuf::from("/config")),
                None,
                Some(PathBuf::from("/cache")),
                None,
            ),
            Err(RsiError::Boot(message)) if message.contains("HOME")
        ));
    }
}
