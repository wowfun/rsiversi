//! Standard `RSIversi` local Session application, Host, and headless adapter.

#![deny(unsafe_code)]
#![warn(missing_docs)]
#![allow(clippy::missing_errors_doc)]

mod agent_preset;
mod composition;
mod local_host;
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
pub use local_host::StandardSessionDaemon;
pub use local_host::{
    SessionHostConnection, SessionHostConnectionMode, connect_or_embed_session_host,
};
pub use profiles::{
    APPLICATION_PROFILE_DIRECTORY, APPLICATION_PROFILE_FILE, ApplicationKind, ApplicationProfile,
    ApplicationProfileDocument, ApplicationProfileId, HOST_PROFILE_DIRECTORY, HOST_PROFILE_FILE,
    HostLaunchKey, HostProfileDocument, HostProfileId, ProfileCatalog, ProfileCatalogError,
    ProfileRow, ProfileSource,
};
pub use rsi_agent_presets::{AgentPresetSource, AgentPresetTrust};
pub use rsi_apply_patch::maybe_run_apply_patch_helper;
pub use rsi_session;
pub use rsi_shell_bash::scrub_child_environment;
pub use settings::{AgentSettings, AgentSettingsContract};

use rsi_host::{HostPaths, RunningHost};
use std::path::{Path, PathBuf};
use thiserror::Error;

/// Running standard Host and its application-facing operations.
#[derive(Debug)]
pub struct RunningRsi {
    host: RunningHost,
}

impl RunningRsi {
    /// Boots the standard immutable catalog from one required Profile file.
    pub async fn boot(composition: StandardComposition, profile_path: &Path) -> Result<Self> {
        let host = composition
            .build()
            .map_err(|error| RsiError::Boot(error.to_string()))?;
        let host = host
            .start_file(profile_path)
            .await
            .map_err(|error| RsiError::Boot(error.to_string()))?;
        if host.lookup_local::<AgentSettingsContract>().is_none() {
            let _outcome = host.shutdown().await;
            return Err(RsiError::Boot(
                "Session Agent Settings did not become active".into(),
            ));
        }
        Ok(Self { host })
    }

    /// Boots one catalog-resolved Host Profile document.
    pub async fn boot_host_profile(
        composition: StandardComposition,
        profile: &HostProfileDocument,
    ) -> Result<Self> {
        let host = composition
            .build()
            .map_err(|error| RsiError::Boot(error.to_string()))?;
        let host = match &profile.path {
            Some(path) => host.start_file(path).await,
            None => host.start(rsi_host::Profile::default()).await,
        }
        .map_err(|error| RsiError::Boot(error.to_string()))?;
        if host.lookup_local::<AgentSettingsContract>().is_none() {
            let _outcome = host.shutdown().await;
            return Err(RsiError::Boot(
                "Session Agent Settings did not become active".into(),
            ));
        }
        Ok(Self { host })
    }

    /// Returns the frozen Host paths.
    pub const fn paths(&self) -> &HostPaths {
        self.host.paths()
    }

    /// Builds the process-local adapter over this running Host generation.
    pub fn session_application(&self) -> Result<rsi_session::LocalSessionApplication> {
        self.session_application_with_approvals(std::sync::Arc::new(rsi_session::NoApprovalControl))
    }

    /// Builds the process-local adapter with Host-owned live approval control.
    pub fn session_application_with_approvals(
        &self,
        approvals: std::sync::Arc<dyn rsi_session::SessionApprovalControl>,
    ) -> Result<rsi_session::LocalSessionApplication> {
        use rsi_agent_composition_protocol::AgentCompositionContract;
        use rsi_agent_store_protocol::SessionStoreContract;
        use rsi_agent_turn_protocol::TurnServiceContract;
        use rsi_ai_protocol::{ImageCallContract, LanguageCallContract};
        use rsi_workspace::WorkspaceRegistryContract;

        let turns = required_local::<TurnServiceContract>(&self.host, "Agent Turn service")?;
        let store = required_local::<SessionStoreContract>(&self.host, "Agent Store")?;
        let composition =
            required_local::<AgentCompositionContract>(&self.host, "Agent composition")?;
        let workspace =
            required_local::<WorkspaceRegistryContract>(&self.host, "Workspace registry")?;
        let settings = required_local::<AgentSettingsContract>(&self.host, "Agent settings")?;
        let settings: std::sync::Arc<dyn rsi_session::AgentSettingsSource> =
            std::sync::Arc::new(SessionSettingsSource(settings));
        let language = required_local::<LanguageCallContract>(&self.host, "Language router")?;
        let image = required_local::<ImageCallContract>(&self.host, "Image router")?;
        Ok(rsi_session::LocalSessionApplication::new(
            turns,
            store,
            composition,
            workspace,
            settings,
            language,
            image,
            approvals,
        ))
    }

    /// Registers one process-generation approval answerer until its lease drops.
    pub fn register_approval_answerer(
        &self,
        answerer: std::sync::Arc<dyn rsi_approval_protocol::ApprovalAnswerer>,
    ) -> Result<rsi_approval_protocol::ApprovalLease> {
        use rsi_approval_protocol::ApprovalAnswerersContract;

        required_local::<ApprovalAnswerersContract>(&self.host, "Approval answerers")?
            .register(answerer)
            .map_err(|error| RsiError::Boot(error.to_string()))
    }

    /// Rebuilds the complete Host Profile source program.
    pub async fn reload(&self) -> Result<rsi_host::ReloadOutcome> {
        self.host
            .reload()
            .await
            .map_err(|error| RsiError::Boot(error.to_string()))
    }

    /// Shuts down all Profile Fibers and process-local Jobs deterministically.
    pub async fn shutdown(&self) -> rsi_meta::ShutdownOutcome {
        self.host.shutdown().await
    }
}

#[derive(Debug)]
struct SessionSettingsSource(std::sync::Arc<dyn AgentSettings>);

impl rsi_session::AgentSettingsSource for SessionSettingsSource {
    fn current(&self) -> rsi_agent_session_protocol::FrozenAgentSettings {
        self.0.current().clone()
    }
}

fn required_local<C: rsi_meta::LocalContract>(
    host: &RunningHost,
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

/// Standard application failure classified by process exit contract.
#[derive(Clone, Debug, Eq, Error, PartialEq)]
pub enum RsiError {
    /// CLI, path, Profile, Settings, or Host bootstrap failure.
    #[error("{0}")]
    Boot(String),
    /// Accepted turn or runtime execution failure.
    #[error("{0}")]
    Run(String),
}

impl RsiError {
    /// Stable process exit code for this failure class.
    pub const fn exit_code(&self) -> u8 {
        match self {
            Self::Boot(_) => 2,
            Self::Run(_) => 1,
        }
    }
}

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
