//! Standard RSI product composition and service lifecycle.

#![deny(unsafe_code)]
#![warn(missing_docs)]
#![allow(clippy::missing_errors_doc)]

mod addon;
mod addon_discovery;
pub use addon_discovery::{
    AddonDiscoveryCursor, AddonDiscoveryEntry, AddonDiscoveryPage, AddonDiscoverySnapshot,
    MAXIMUM_ADDON_DISCOVERY_BYTES, MAXIMUM_ADDON_DISCOVERY_ITEMS,
};
#[cfg(unix)]
mod addon_store;
#[cfg(unix)]
pub use addon_store::{
    MAXIMUM_NATIVE_ADDON_MANIFEST_BYTES, MAXIMUM_NATIVE_ADDON_OBJECT_BYTES,
    MAXIMUM_NATIVE_ADDON_OBJECTS, MAXIMUM_NATIVE_ADDON_SERVICES, MAXIMUM_NATIVE_ADDON_STATE_BYTES,
    MAXIMUM_NATIVE_ADDONS, NativeAddonBuild, NativeAddonError, NativeAddonReceipt,
    NativeAddonRecord, NativeAddonSnapshot, NativeAddonStore, NativeAddonStoreLimits,
    native_addon_target,
};
#[cfg(unix)]
mod addon_build;
#[cfg(unix)]
pub use addon_build::{
    NativeAddonBuildError, NativeAddonBuildManager, NativeAddonBuildReport,
    NativeAddonBuildService, NativeAddonBuildStatus,
};
mod agent_preset;
pub use addon::{
    AddonFactoryDescription, AddonScope, MAXIMUM_ADDON_DESCRIPTION_BYTES, MAXIMUM_ADDON_PLATFORMS,
    MAXIMUM_ADDON_SCHEMA_DEPTH, MAXIMUM_FACTORY_DESCRIPTION_BYTES, MAXIMUM_STANDARD_ADDONS,
    StandardAddon, StandardAddonBuilder, StandardAddonSet,
};
mod application_catalog;
pub use application_catalog::{
    ApplicationCatalog, ApplicationCatalogMetadata, ApplicationCatalogProvider,
};
mod application_connection;
pub mod application_services;
pub use application_connection::{
    ApplicationDiagnostics, BASE_APPLICATION_PLUGINS, base_application_catalog,
    standard_application_host,
};
mod application_composition;
pub use application_composition::ApplicationComposition;
mod application_bootstrap;
pub use application_bootstrap::start_application;
mod api_composition;
mod client_composition;
#[cfg(target_os = "linux")]
pub use client_composition::{probe_service_host, workflow_restart_impact};
#[cfg(any(target_os = "linux", windows))]
mod acp_configuration;
#[cfg(any(target_os = "linux", windows))]
mod acp_inputs;
#[cfg(any(target_os = "linux", windows))]
mod acp_owner;
mod composition;
mod integration_source;
mod local_api_client;
mod local_host;
#[cfg(unix)]
mod mcp_api;
#[cfg(not(unix))]
#[path = "mcp_api_unsupported.rs"]
mod mcp_api;
#[cfg(unix)]
mod mcp_ssh;
#[cfg(unix)]
mod native_addons;
mod retrieval_api;
mod workflow;
#[cfg(unix)]
pub use native_addons::{
    MAXIMUM_NATIVE_ADDON_REFRESH_REQUESTS, NativeAddonControl, NativeAddonControlContract,
    NativeAddonHealth, NativeAddonInspection, NativeAddonManager, NativeAddonRefresh,
    NativeAddonUpdateError,
};
mod execution_access;
mod output_read;
#[cfg(unix)]
mod profile_management;
mod profile_owner;
mod profiles;
#[cfg(unix)]
mod service_bootstrap;
mod settings;
#[cfg(target_os = "linux")]
mod ssh_targets;
#[cfg(unix)]
mod writer_lock;

pub use agent_preset::{
    AGENT_PRESET_SETTINGS_NAMESPACE, AgentPresetManager, DEFAULT_AGENT_PRESET_ID,
    USER_AGENT_PRESET_DIRECTORY, user_agent_preset_root,
};
pub use composition::{
    HostStartupPurpose, StandardCodingTools, StandardComposition, StandardHostPreview,
    capture_standard_environment, standard_agent_preset_root,
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
pub use profiles::{HostLeafEdit, ProfileEdit, ProfileEditError, ProfileEditReceipt};
pub use rsi_agent_presets::{AgentPresetSource, AgentPresetTrust};
pub use rsi_apply_patch::maybe_run_apply_patch_helper;
pub use rsi_session_protocol;
pub use rsi_shell_bash::scrub_child_environment;

use profile_owner::ProfileOwner;
use rsi_host::HostPaths;
use std::path::{Path, PathBuf};

const STARTUP_OWNERS: &[&str] = &[
    "rsi.managed-providers",
    "rsi-history",
    "rsi.history.api",
    "rsi.attention",
    "rsi.navigation",
    "rsi-mcp-api",
    "rsi-workspace-review",
    "rsi.workspace-review.api",
    "automation",
];

/// Running standard Host and its application-facing operations.
#[derive(Debug)]
pub struct RunningRsi {
    host: ProfileOwner,
    paths: HostPaths,
}

/// Read-only observations captured from the existing Profile and Meta owners.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RsiInspection {
    /// Bounded convergence status without configuration or raw plugin failures.
    pub profile_status: rsi_host::ProfileStatus,
    /// Redacted desired Profile tree.
    pub profile: rsi_host::ProfileSnapshot,
    /// Bounded owned Runtime metadata, scoped for embedded Hosts.
    pub runtime: rsi_meta::RuntimeInspection,
}

impl RunningRsi {
    pub(crate) fn lookup_addon<C: rsi_meta::LocalContract>(
        &self,
    ) -> Option<std::sync::Arc<C::Service>> {
        self.host.lookup_local::<C>()
    }

    /// Boots the standard Service bootstrap from one required Profile file.
    pub async fn boot(composition: StandardComposition, profile_path: &Path) -> Result<Self> {
        Self::start_service(
            composition,
            rsi_host::ProfileProgram::from_file(profile_path),
            None,
            None,
        )
        .await
    }

    /// Boots one catalog-resolved Host Profile document.
    pub async fn boot_host_profile(
        composition: StandardComposition,
        profile: &HostProfileDocument,
    ) -> Result<Self> {
        Self::start_service(composition, service_program(profile), None, None).await
    }

    /// Boots a standard service subtree within the caller's application Runtime.
    /// Parent lifetime and bootstrap cancellation belong to the caller.
    pub async fn boot_host_profile_in(
        composition: StandardComposition,
        profile: &HostProfileDocument,
        parent: &rsi_meta::Context,
    ) -> Result<Self> {
        Self::start_service(composition, service_program(profile), None, Some(parent)).await
    }

    #[cfg(target_os = "linux")]
    pub(crate) async fn boot_daemon_profile(
        composition: StandardComposition,
        profile: &HostProfileDocument,
        launch_key: &str,
        parent: Option<&rsi_meta::Context>,
    ) -> Result<Self> {
        Self::start_service(
            composition,
            service_program(profile),
            Some(launch_key.to_owned()),
            parent,
        )
        .await
    }

    async fn start_service(
        mut composition: StandardComposition,
        program: rsi_host::ProfileProgram,
        launch_key: Option<String>,
        parent: Option<&rsi_meta::Context>,
    ) -> Result<Self> {
        composition.agent_store_factory = composition.agent_store_reset.clone().map_or_else(
            rsi_agent_store_sqlite::SqliteStoreFactory::default,
            rsi_agent_store_sqlite::SqliteStoreFactory::with_reset_once,
        );
        let store_diagnostic = composition.agent_store_factory.clone();
        let started = Box::pin(async {
            #[cfg(unix)]
            let owner = Box::pin(service_bootstrap::start(
                composition,
                program,
                launch_key,
                parent,
            ))
            .await?;
            #[cfg(not(unix))]
            let owner = {
                let _ = launch_key;
                let paths = composition.paths().clone();
                let host = composition
                    .build()
                    .map_err(|error| RsiError::Boot(error.to_string()))?;
                match parent {
                    Some(parent) => {
                        ProfileOwner::start_scoped(host, paths, parent, program).await?
                    }
                    None => ProfileOwner::Root(
                        host.start_program(program)
                            .await
                            .map_err(|error| RsiError::Boot(error.to_string()))?,
                    ),
                }
            };
            Self::from_started_host(owner).await
        })
        .await;
        started.map_err(|error| {
            store_diagnostic.take_startup_failure().map_or(error, |failure| {
                use rsi_agent_store_sqlite::SqliteStoreStartupFailureKind as Kind;
                let reason = match failure.kind {
                    Kind::Unsupported => "Agent Store native filesystem protection is unsupported on this platform".into(),
                    Kind::SchemaMismatch { expected, actual } => format!(
                        "Agent Store schema mismatch: expected {expected}, actual {actual}; no automatic migration is available. Preserve the old Store and use a fresh root or a matching older build. Re-run with --reset-state to back up the Agent Store and start with empty session history."
                    ),
                    Kind::WriterLocked => "Agent Store root already has an active writer".into(),
                    Kind::Invalid => "Agent Store root or filesystem layout is invalid".into(),
                    Kind::Corrupt => "Agent Store metadata is corrupt; inspect it with agent-store verify".into(),
                    Kind::Io => "Agent Store could not be opened; inspect filesystem access and agent-store verify".into(),
                };
                #[allow(clippy::unnecessary_debug_formatting, reason = "Escape filesystem paths so terminal controls cannot execute in startup errors.")]
                let root = format!("{:?}", failure.root);
                RsiError::Boot(format!("{reason}; root {root}"))
            })
        })
    }

    async fn from_started_host(host: ProfileOwner) -> Result<Self> {
        let mut changes = host.subscribe_profile();
        let ready = tokio::time::timeout(std::time::Duration::from_secs(30), async {
            loop {
                {
                    let status = changes.borrow_and_update();
                    let mut pending = false;
                    for &id in STARTUP_OWNERS {
                        let desired = status
                            .target()
                            .iter()
                            .any(|target| target.id().as_str() == id);
                        let instance = status
                            .observed()
                            .iter()
                            .find(|instance| instance.id().as_str() == id);
                        match instance.map(rsi_host::ProfileInstanceStatus::state) {
                            None if !desired
                                && status.health() == rsi_host::ProfileHealth::Converged => {}
                            Some(rsi_host::ProfileInstanceState::Active) => {}
                            Some(
                                rsi_host::ProfileInstanceState::Failed
                                | rsi_host::ProfileInstanceState::Disposed
                                | rsi_host::ProfileInstanceState::Unloading,
                            ) => return Err("required product owner failed during startup"),
                            None
                            | Some(
                                rsi_host::ProfileInstanceState::Pending(_)
                                | rsi_host::ProfileInstanceState::Loading,
                            ) => pending = true,
                        }
                    }
                    if !pending {
                        break Ok(());
                    }
                }
                if changes.changed().await.is_err() {
                    break Err("Service Profile stopped during startup");
                }
            }
        })
        .await;
        if !matches!(ready, Ok(Ok(()))) {
            let _ = host.shutdown().await;
            return Err(RsiError::Boot(
                match ready {
                    Ok(Err(error)) => error,
                    _ => "required product startup timed out",
                }
                .into(),
            ));
        }
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

    /// Inspects this Host without exporting mutation or cross-scope resource authority.
    pub fn inspect(&self, request: rsi_meta::InspectionRequest) -> Result<RsiInspection> {
        self.host
            .inspect(request)
            .map_err(|error| RsiError::Run(error.to_string()))
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
    /// A clean outcome proves runtime teardown, not destruction of caller-retained
    /// Local capability Arcs. Drop those capabilities before reopening the same paths.
    pub async fn shutdown(&self) -> rsi_meta::ShutdownOutcome {
        self.host.shutdown().await
    }
}

fn service_program(profile: &HostProfileDocument) -> rsi_host::ProfileProgram {
    profile.path.as_ref().map_or_else(
        || rsi_host::ProfileProgram::from_profile(rsi_host::Profile::default()),
        rsi_host::ProfileProgram::from_file,
    )
}

fn required_local<C: rsi_meta::LocalContract>(
    host: &ProfileOwner,
    name: &str,
) -> Result<std::sync::Arc<C::Service>> {
    host.lookup_local::<C>()
        .ok_or_else(|| RsiError::Boot(format!("{name} is unavailable")))
}

/// Captures the canonical platform Bash selected by the standard product launcher.
/// Windows uses the Git for Windows installer registry; Unix uses `/bin/bash`.
pub fn capture_standard_bash() -> Result<PathBuf> {
    #[cfg(windows)]
    let path = {
        use winreg::{
            RegKey,
            enums::{HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE},
        };
        let install = [HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE]
            .into_iter()
            .find_map(|hive| {
                RegKey::predef(hive)
                    .open_subkey("SOFTWARE\\GitForWindows")
                    .ok()
                    .and_then(|key| key.get_value::<String, _>("InstallPath").ok())
            })
            .ok_or_else(|| {
                RsiError::Boot(
                    "native Windows coding requires an installed Git for Windows Bash".into(),
                )
            })?;
        PathBuf::from(install).join("bin/bash.exe")
    };
    #[cfg(not(windows))]
    let path = PathBuf::from("/bin/bash");
    canonical_standard_bash(&path)
}

fn canonical_standard_bash(path: &std::path::Path) -> Result<PathBuf> {
    if !path.is_absolute() {
        return Err(RsiError::Boot("standard Bash path must be absolute".into()));
    }
    std::fs::canonicalize(path).map_err(|error| {
        RsiError::Boot(format!(
            "standard Bash {} is unavailable: {error}",
            path.display()
        ))
    })
}

/// Captures the Windows Profile Known Folder, independent of HOME.
/// On Unix, missing, empty and relative HOME values skip this optional source.
pub fn capture_standard_home() -> Result<Option<PathBuf>> {
    #[cfg(windows)]
    return Ok(dirs::home_dir().filter(|path| path.is_absolute()));
    #[cfg(not(windows))]
    Ok(personal_home(std::env::var_os("HOME")))
}

#[cfg(not(windows))]
fn personal_home(value: Option<std::ffi::OsString>) -> Option<PathBuf> {
    value.map(PathBuf::from).filter(|path| path.is_absolute())
}

/// Resolves standard platform locations without creating directories or finding a Profile.
#[cfg(not(windows))]
pub fn standard_paths() -> Result<HostPaths> {
    let config = optional_environment_path("XDG_CONFIG_HOME")?;
    let state = optional_environment_path("XDG_STATE_HOME")?;
    let cache = optional_environment_path("XDG_CACHE_HOME")?;
    let home = (config.is_none() || state.is_none() || cache.is_none())
        .then(|| environment_path("HOME"))
        .transpose()?;
    standard_paths_from(config, state, cache, home.as_deref())
}

/// Resolves explicit XDG overrides or Windows Known Folders without creating directories.
#[cfg(windows)]
pub fn standard_paths() -> Result<HostPaths> {
    let config = optional_environment_path("XDG_CONFIG_HOME")?;
    let state = optional_environment_path("XDG_STATE_HOME")?;
    let cache = optional_environment_path("XDG_CACHE_HOME")?;
    let defaults = if config.is_none() || state.is_none() || cache.is_none() {
        Some(windows_standard_paths(
            dirs::config_dir(),
            dirs::data_local_dir(),
        )?)
    } else {
        None
    };
    let resolve = |override_path: Option<PathBuf>, fallback: Option<&Path>| {
        override_path
            .map(|path| path.join("rsi"))
            .or_else(|| fallback.map(Path::to_owned))
            .ok_or_else(|| RsiError::Boot("Windows standard path is unavailable".into()))
    };
    HostPaths::new(
        resolve(config, defaults.as_ref().map(HostPaths::config))?,
        resolve(state, defaults.as_ref().map(HostPaths::state))?,
        resolve(cache, defaults.as_ref().map(HostPaths::cache))?,
    )
    .map_err(|error| RsiError::Boot(error.to_string()))
}

#[cfg(windows)]
fn windows_standard_paths(roaming: Option<PathBuf>, local: Option<PathBuf>) -> Result<HostPaths> {
    let required = |path: Option<PathBuf>, folder: &str| {
        path.filter(|path| path.is_absolute()).ok_or_else(|| {
            RsiError::Boot(format!(
                "required Windows Known Folder `{folder}` is unavailable"
            ))
        })
    };
    let config = required(roaming, "RoamingAppData")?.join("rsi");
    let local = required(local, "LocalAppData")?.join("rsi");
    HostPaths::new(config, local.join("state"), local.join("cache"))
        .map_err(|error| RsiError::Boot(error.to_string()))
}

#[cfg(not(windows))]
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

#[cfg(not(windows))]
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

    #[derive(Debug)]
    struct StartupGate;
    impl rsi_meta::LocalContract for StartupGate {
        const KEY: &'static str = "fixture.startup-gate";
        type Service = ();
    }

    #[derive(Debug)]
    struct PendingApi(bool);
    #[async_trait::async_trait]
    impl rsi_meta::PluginFactory for PendingApi {
        fn prepare(
            &self,
            config: &rsi_meta::ConfigValue,
        ) -> rsi_meta::Result<rsi_meta::PreparedActivation> {
            let prepared = rsi_meta::PreparedActivation::new(config.clone());
            Ok(if self.0 {
                prepared.requiring_local::<StartupGate>()
            } else {
                prepared
            })
        }
        async fn activate(&self, _: rsi_meta::ActivationPlan) -> rsi_meta::Result<()> {
            assert!(!self.0, "the fixture keeps API activation pending");
            Ok(())
        }
    }

    #[derive(Debug)]
    struct StartupSession;
    #[async_trait::async_trait]
    impl rsi_session_protocol::SessionService for StartupSession {
        async fn read_header(
            &self,
            _: &rsi_agent_session_protocol::SessionId,
        ) -> rsi_session_protocol::Result<rsi_agent_session_protocol::SessionHeader> {
            panic!("startup does not read Session data")
        }
        async fn create(
            &self,
            _: rsi_session_protocol::CreateSession,
        ) -> rsi_session_protocol::Result<std::sync::Arc<dyn rsi_session_protocol::SessionHandle>>
        {
            panic!("startup does not create a Session")
        }
        async fn attach(
            &self,
            _: &rsi_agent_session_protocol::SessionId,
        ) -> rsi_session_protocol::Result<std::sync::Arc<dyn rsi_session_protocol::SessionHandle>>
        {
            panic!("startup does not attach a Session")
        }
        async fn list_recent(
            &self,
            _: Option<&rsi_session_protocol::RecentSessionCursor>,
            _: usize,
        ) -> rsi_session_protocol::Result<rsi_session_protocol::RecentSessionPage> {
            panic!("startup does not discover history")
        }
    }
    #[async_trait::async_trait]
    impl rsi_meta::PluginFactory for StartupSession {
        fn prepare(
            &self,
            config: &rsi_meta::ConfigValue,
        ) -> rsi_meta::Result<rsi_meta::PreparedActivation> {
            Ok(rsi_meta::PreparedActivation::new(config.clone()))
        }
        async fn activate(&self, plan: rsi_meta::ActivationPlan) -> rsi_meta::Result<()> {
            plan.context()
                .provide_local::<rsi_session_protocol::SessionContract>(std::sync::Arc::new(
                    Self,
                ))?;
            Ok(())
        }
    }

    #[tokio::test]
    async fn converged_profile_waits_for_enabled_navigation_api_owners() {
        use rsi_host::{HostBuilder, Profile, ProfileEntry, ProfileProgram};
        use std::sync::Arc;
        for id in ["rsi.attention", "rsi.navigation", "automation"] {
            let temporary = tempfile::tempdir().unwrap();
            let paths = HostPaths::new(
                temporary.path().join("config"),
                temporary.path().join("state"),
                temporary.path().join("cache"),
            )
            .unwrap();
            let mut builder = HostBuilder::new(paths.clone());
            builder.register_local_contract::<StartupGate>().unwrap();
            builder
                .register_local_contract::<rsi_session_protocol::SessionContract>()
                .unwrap();
            builder
                .register_linked(
                    "fixture.pending-api",
                    "1",
                    rsi_meta::UpdateMode::Replayable,
                    Arc::new(PendingApi(true)),
                )
                .unwrap();
            builder
                .register_linked(
                    "fixture.active-api",
                    "1",
                    rsi_meta::UpdateMode::Replayable,
                    Arc::new(PendingApi(false)),
                )
                .unwrap();
            builder
                .register_linked(
                    "fixture.session",
                    "1",
                    rsi_meta::UpdateMode::Replayable,
                    Arc::new(StartupSession),
                )
                .unwrap();
            let mut entries: Vec<_> = STARTUP_OWNERS
                .iter()
                .filter(|owner| **owner != id)
                .map(|owner| {
                    ProfileEntry::new(*owner, "fixture.active-api", serde_json::Value::Null)
                })
                .collect();
            entries.push(ProfileEntry::new(
                id,
                "fixture.pending-api",
                serde_json::Value::Null,
            ));
            entries.push(ProfileEntry::new(
                "fixture.session",
                "fixture.session",
                serde_json::Value::Null,
            ));
            let runtime = rsi_meta::Runtime::default();
            let profile = rsi_application::ScopedProfile::start(
                &builder.build().unwrap(),
                &runtime.root(),
                ProfileProgram::from_profile(Profile::new(entries)),
            )
            .await
            .unwrap();
            let status = profile.profile_status();
            assert_eq!(status.health(), rsi_host::ProfileHealth::Converged);
            assert!(matches!(
                status
                    .observed()
                    .iter()
                    .find(|row| row.id().as_str() == id)
                    .unwrap()
                    .state(),
                rsi_host::ProfileInstanceState::Pending(_)
            ));
            assert!(
                profile
                    .lookup_local::<rsi_session_protocol::SessionContract>()
                    .is_some()
            );
            tokio::task::yield_now().await;
            let mut boot = Box::pin(RunningRsi::from_started_host(ProfileOwner::Scoped {
                paths,
                profile: Arc::new(profile),
            }));
            assert!(
                futures_util::poll!(&mut boot).is_pending(),
                "startup must wait for enabled {id}"
            );
            drop(boot);
            assert!(runtime.shutdown().await.is_clean());
        }
    }

    #[test]
    #[cfg(not(windows))]
    fn optional_personal_home_ignores_missing_empty_and_relative_paths() {
        for value in [None, Some("".into()), Some("relative".into())] {
            assert_eq!(personal_home(value), None);
        }
        assert_eq!(
            personal_home(Some("/home/example".into())),
            Some(PathBuf::from("/home/example"))
        );
    }

    #[test]
    #[cfg(not(windows))]
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
    #[cfg(not(windows))]
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

    #[cfg(windows)]
    #[test]
    fn windows_locations_separate_roaming_config_from_local_data() {
        let roaming = PathBuf::from(r"D:\fixture\漫游");
        let local = PathBuf::from(r"D:\fixture\Local😀");
        let paths = windows_standard_paths(Some(roaming.clone()), Some(local.clone())).unwrap();
        assert_eq!(paths.config(), roaming.join("rsi"));
        assert_eq!(paths.state(), local.join("rsi/state"));
        assert_eq!(paths.cache(), local.join("rsi/cache"));
        for (roaming, local, folder) in [
            (None, Some(local.clone()), "RoamingAppData"),
            (Some(roaming.clone()), None, "LocalAppData"),
            (
                Some(PathBuf::from("relative")),
                Some(local.clone()),
                "RoamingAppData",
            ),
            (Some(PathBuf::new()), Some(local.clone()), "RoamingAppData"),
            (
                Some(roaming.clone()),
                Some(PathBuf::from("relative")),
                "LocalAppData",
            ),
            (Some(roaming), Some(PathBuf::new()), "LocalAppData"),
        ] {
            assert!(matches!(windows_standard_paths(roaming, local),
                Err(RsiError::Boot(message)) if message.contains(folder)));
        }
    }

    #[cfg(windows)]
    #[test]
    #[ignore = "opt-in installed Git for Windows lookup and subprocess; no user state is written"]
    fn windows_standard_bash_runs_the_installed_native_executable() {
        let bash = capture_standard_bash().unwrap();
        assert!(bash.is_absolute());
        assert_eq!(bash.file_name().unwrap(), "bash.exe");
        let output = std::process::Command::new(bash)
            .arg("--version")
            .output()
            .unwrap();
        assert!(output.status.success());
        assert!(String::from_utf8_lossy(&output.stdout).contains("GNU bash"));
    }

    #[test]
    fn standard_bash_resolution_rejects_cwd_relative_installations() {
        assert!(
            canonical_standard_bash(std::path::Path::new("."))
                .unwrap_err()
                .to_string()
                .contains("must be absolute")
        );
        let temp = tempfile::tempdir().unwrap();
        let executable = temp.path().join("bash-fixture");
        std::fs::write(&executable, b"fixture").unwrap();
        assert_eq!(
            canonical_standard_bash(&executable).unwrap(),
            executable.canonicalize().unwrap()
        );
    }

    #[cfg(windows)]
    #[test]
    #[ignore = "opt-in native Known Folder lookup; no directories are created"]
    fn windows_known_folders_resolve_without_home_or_xdg() {
        const CHILD: &str = "RSI_KNOWN_FOLDER_PROBE";
        if std::env::var_os(CHILD).is_some() {
            let paths = standard_paths().expect("native Known Folders");
            assert!(paths.config().is_absolute());
            assert!(paths.state().is_absolute());
            assert!(paths.cache().is_absolute());
            assert_eq!(
                capture_standard_home().unwrap(),
                dirs::home_dir().filter(|path| path.is_absolute())
            );
            return;
        }
        for home in [
            None,
            Some(r"C:\unrelated-git-home"),
            Some("/home/msys-user"),
            Some(""),
        ] {
            let mut command = std::process::Command::new(std::env::current_exe().unwrap());
            command
                .args([
                    "--exact",
                    "tests::windows_known_folders_resolve_without_home_or_xdg",
                    "--ignored",
                ])
                .env(CHILD, "yes")
                .env_remove("HOME")
                .env_remove("XDG_CONFIG_HOME")
                .env_remove("XDG_STATE_HOME")
                .env_remove("XDG_CACHE_HOME");
            if let Some(home) = home {
                command.env("HOME", home);
            }
            let output = command.output().unwrap();
            assert!(
                output.status.success(),
                "{}{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
        }
    }
}

mod inspector;
