use crate::{ProfileOwner, Result, RsiError};
use async_trait::async_trait;
use rsi_agent_presets::{
    AgentPresetCatalog, AgentPresetDefaultStore, AgentPresetId, AgentPresetProfileCompiler,
    AgentPresetTrust, MAX_ROOTS, PresetError,
};
use rsi_host::{HostBuilder, HostPaths, Profile, ProfileEntry, ProfileProgram};
use rsi_meta::UpdateMode;
use rsi_meta_profile::{ProfileCompiler, ProfileEnvironment, ProfileLimits};
use rsi_settings_protocol::{
    SettingsContract, SettingsError, SettingsProviderContract, SettingsScope,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::sync::Arc;

/// Settings namespace owned by standard Agent-preset selection and roots.
pub const AGENT_PRESET_SETTINGS_NAMESPACE: &str = "rsi.agent-presets";
/// Deployment default used when the user has not selected an override.
pub const DEFAULT_AGENT_PRESET_ID: &str = "standard";
/// Directory below the standard configuration root that owns user presets.
pub const USER_AGENT_PRESET_DIRECTORY: &str = "agent-presets";

mod plugin;
const CATALOG_FACTORY: &str = "rsi.agent.preset-catalog";

const SETTINGS_LOCAL_FACTORY: &str = "rsi.settings.local";
const SETTINGS_CORE_FACTORY: &str = "rsi.settings";

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
enum RootTrustWire {
    System,
    #[default]
    User,
}

impl From<RootTrustWire> for AgentPresetTrust {
    fn from(value: RootTrustWire) -> Self {
        match value {
            RootTrustWire::System => Self::System,
            RootTrustWire::User => Self::User,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct RootWire {
    path: PathBuf,
    #[serde(default)]
    trust: RootTrustWire,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct SettingsWire {
    default: AgentPresetId,
    roots: Vec<RootWire>,
}

#[derive(Debug, Serialize)]
struct UserSettingsWire<'a> {
    #[serde(skip_serializing_if = "Option::is_none")]
    default: Option<&'a AgentPresetId>,
    roots: &'a [RootWire],
}

/// Management lifetime for one settings-backed Agent-preset catalog.
#[derive(Debug)]
pub struct AgentPresetManager {
    catalog: Arc<AgentPresetCatalog>,
    host: ProfileOwner,
}

#[derive(Debug)]
enum SystemPresetSource {
    Root(PathBuf),
    Exact { id: AgentPresetId, path: PathBuf },
}

impl AgentPresetManager {
    /// Opens the standard Settings document and derives one fresh catalog.
    ///
    /// Product-owned system roots are injected in precedence order. Settings
    /// contributes configured read-only roots after them, and
    /// `<config>/agent-presets` is always the final writable user root. The
    /// coding-tools flag freezes the same standard Profile define and
    /// contribution allowlist later used by the standard composition.
    pub async fn open<I, P>(
        paths: HostPaths,
        system_roots: I,
        coding_tools_enabled: bool,
    ) -> Result<Self>
    where
        I: IntoIterator<Item = P>,
        P: Into<PathBuf>,
    {
        Self::open_with_system_sources(
            None,
            paths,
            system_roots
                .into_iter()
                .map(|root| SystemPresetSource::Root(root.into()))
                .collect(),
            coding_tools_enabled,
        )
        .await
    }

    /// Opens the standard Settings document with the sole byte-verified
    /// built-in preset, without trusting sibling cache directories.
    pub async fn open_standard(
        paths: HostPaths,
        system_root: impl Into<PathBuf>,
        coding_tools_enabled: bool,
    ) -> Result<Self> {
        Self::open_standard_with_context(None, paths, system_root.into(), coding_tools_enabled)
            .await
    }

    /// Mounts the standard catalog and Settings plugins below an existing Context.
    pub async fn open_standard_in(
        parent: &rsi_meta::Context,
        paths: HostPaths,
        system_root: impl Into<PathBuf>,
        coding_tools_enabled: bool,
    ) -> Result<Self> {
        Self::open_standard_with_context(
            Some(parent),
            paths,
            system_root.into(),
            coding_tools_enabled,
        )
        .await
    }

    async fn open_standard_with_context(
        parent: Option<&rsi_meta::Context>,
        paths: HostPaths,
        system_root: PathBuf,
        coding_tools_enabled: bool,
    ) -> Result<Self> {
        let id = AgentPresetId::new(DEFAULT_AGENT_PRESET_ID)
            .map_err(|error| RsiError::Boot(error.to_string()))?;
        let path = system_root.join(id.as_str());
        Self::open_with_system_sources(
            parent,
            paths,
            vec![SystemPresetSource::Exact { id, path }],
            coding_tools_enabled,
        )
        .await
    }

    /// Opens the standard Settings-backed catalog for a non-activating preview.
    ///
    /// The built-in preset contributes its deterministic final cache location
    /// to launch identity without materializing that asset.
    pub async fn open_standard_preview(
        paths: HostPaths,
        coding_tools_enabled: bool,
    ) -> Result<Self> {
        Self::open_standard(
            paths.clone(),
            crate::composition::standard_agent_preset_root_candidate(&paths),
            coding_tools_enabled,
        )
        .await
    }

    async fn open_with_system_sources(
        parent: Option<&rsi_meta::Context>,
        paths: HostPaths,
        system_sources: Vec<SystemPresetSource>,
        coding_tools_enabled: bool,
    ) -> Result<Self> {
        let factory = Arc::new(plugin::CatalogFactory {
            compiler: standard_agent_profile_compiler(&paths, coding_tools_enabled)?,
            paths: paths.clone(),
            sources: system_sources,
            diagnostic: std::sync::Mutex::new(None),
        });
        let host = boot_settings_host(parent, paths, factory).await?;
        let Some(catalog) = host.lookup_local::<plugin::CatalogContract>() else {
            let _shutdown = host.shutdown().await;
            return Err(RsiError::Boot(
                "Agent-preset catalog plugin did not become active".into(),
            ));
        };
        Ok(Self { catalog, host })
    }

    /// Returns the live catalog backed by this manager's Settings scope.
    pub fn catalog(&self) -> &AgentPresetCatalog {
        &self.catalog
    }

    /// Disposes the management Profile and its ordinary catalog/Settings owners.
    pub async fn shutdown(self) -> rsi_meta::ShutdownOutcome {
        self.host.shutdown().await
    }
}

pub(crate) fn standard_agent_profile_compiler(
    paths: &HostPaths,
    linux_tools_enabled: bool,
) -> Result<AgentPresetProfileCompiler> {
    let environment = ProfileEnvironment::new(
        paths.config(),
        paths.state(),
        paths.cache(),
        format!("{}-{}", std::env::consts::OS, std::env::consts::ARCH),
        BTreeMap::from([(
            "standard_linux_coding_tools".to_owned(),
            Value::Bool(linux_tools_enabled),
        )]),
    )
    .map_err(|error| RsiError::Boot(error.to_string()))?;
    Ok(AgentPresetProfileCompiler::new(
        ProfileCompiler::new(environment, ProfileLimits::default()),
        crate::composition::standard_agent_contribution_ids(linux_tools_enabled)
            .iter()
            .copied(),
    ))
}

/// Derives the sole writable Agent-preset root from frozen standard paths.
pub fn user_agent_preset_root(paths: &HostPaths) -> PathBuf {
    paths.config().join(USER_AGENT_PRESET_DIRECTORY)
}

async fn boot_settings_host(
    parent: Option<&rsi_meta::Context>,
    paths: HostPaths,
    factory: Arc<plugin::CatalogFactory>,
) -> Result<ProfileOwner> {
    let settings_path = paths.config().join("settings.json");
    let mut builder = HostBuilder::new(paths.clone());
    builder
        .register_local_contract::<SettingsProviderContract>()
        .and_then(|builder| builder.register_local_contract::<SettingsContract>())
        .and_then(|builder| {
            builder.register_local_contract::<rsi_settings_protocol::SettingsAccessContract>()
        })
        .map_err(host_boot)?;
    builder
        .register_linked(
            SETTINGS_LOCAL_FACTORY,
            env!("CARGO_PKG_VERSION"),
            UpdateMode::RestartRequired,
            Arc::new(rsi_settings_local::LocalSettingsFactory),
        )
        .and_then(|builder| {
            builder.register_linked(
                SETTINGS_CORE_FACTORY,
                env!("CARGO_PKG_VERSION"),
                UpdateMode::Replayable,
                Arc::new(rsi_settings::SettingsFactory),
            )
        })
        .map_err(host_boot)?;
    builder
        .register_local_contract::<plugin::CatalogContract>()
        .map_err(host_boot)?;
    builder
        .register_linked(
            CATALOG_FACTORY,
            env!("CARGO_PKG_VERSION"),
            UpdateMode::RestartRequired,
            factory.clone(),
        )
        .map_err(host_boot)?;
    let host = builder.build().map_err(host_boot)?;
    let profile = Profile::new([
        ProfileEntry::new(
            "rsi-agent-preset-settings-local",
            SETTINGS_LOCAL_FACTORY,
            json!({ "path": settings_path }),
        ),
        ProfileEntry::new(
            "rsi-agent-preset-settings",
            SETTINGS_CORE_FACTORY,
            Value::Null,
        ),
        ProfileEntry::new("rsi-agent-preset-catalog", CATALOG_FACTORY, Value::Null),
    ]);
    let started = if let Some(parent) = parent {
        ProfileOwner::start_scoped(host, paths, parent, ProfileProgram::from_profile(profile)).await
    } else {
        host.start(profile)
            .await
            .map(ProfileOwner::Root)
            .map_err(host_boot)
    };
    started.map_err(|error| {
        let diagnostic = factory
            .diagnostic
            .lock()
            .expect("catalog diagnostic poisoned")
            .take();
        diagnostic.map_or_else(|| error, host_boot)
    })
}

fn validate_settings(value: &Value) -> rsi_settings_protocol::Result<()> {
    let wire: SettingsWire = serde_json::from_value(value.clone())
        .map_err(|error| SettingsError::InvalidInput(error.to_string()))?;
    if wire.roots.len() > MAX_ROOTS {
        return Err(SettingsError::InvalidInput(format!(
            "`{AGENT_PRESET_SETTINGS_NAMESPACE}.roots` exceeds {MAX_ROOTS} entries"
        )));
    }
    let mut paths = BTreeSet::new();
    for root in wire.roots {
        if !root.path.is_absolute() {
            return Err(SettingsError::InvalidInput(format!(
                "`{AGENT_PRESET_SETTINGS_NAMESPACE}.roots[].path` must be absolute"
            )));
        }
        if !paths.insert(root.path) {
            return Err(SettingsError::InvalidInput(format!(
                "`{AGENT_PRESET_SETTINGS_NAMESPACE}.roots` contains a duplicate path"
            )));
        }
    }
    Ok(())
}

fn read_settings(scope: &dyn SettingsScope) -> rsi_settings_protocol::Result<SettingsWire> {
    let snapshot = scope.get()?;
    serde_json::from_value(snapshot.value)
        .map_err(|error| SettingsError::InvalidInput(error.to_string()))
}

#[derive(Debug)]
struct SettingsDefaultStore {
    scope: Arc<dyn SettingsScope>,
    path: PathBuf,
}

#[async_trait]
impl AgentPresetDefaultStore for SettingsDefaultStore {
    async fn load(&self) -> rsi_agent_presets::Result<Option<AgentPresetId>> {
        let wire = read_settings(self.scope.as_ref()).map_err(|error| self.error("read", error))?;
        if wire.default.as_str() == DEFAULT_AGENT_PRESET_ID {
            Ok(None)
        } else {
            Ok(Some(wire.default))
        }
    }

    async fn replace(&self, selected: Option<AgentPresetId>) -> rsi_agent_presets::Result<()> {
        let snapshot = self
            .scope
            .get()
            .map_err(|error| self.error("read", error))?;
        let wire: SettingsWire =
            serde_json::from_value(snapshot.value).map_err(|error| self.error("decode", error))?;
        let selected = selected.filter(|id| id.as_str() != DEFAULT_AGENT_PRESET_ID);
        let replacement = UserSettingsWire {
            default: selected.as_ref(),
            roots: &wire.roots,
        };
        if selected.is_none() && wire.roots.is_empty() {
            self.scope
                .clear(snapshot.revision)
                .await
                .map_err(|error| self.error("clear", error))?;
        } else {
            let replacement =
                serde_json::to_value(replacement).map_err(|error| self.error("encode", error))?;
            self.scope
                .replace(snapshot.revision, replacement)
                .await
                .map_err(|error| self.error("replace", error))?;
        }
        Ok(())
    }
}

impl SettingsDefaultStore {
    fn error(&self, operation: &'static str, error: impl std::fmt::Display) -> PresetError {
        PresetError::Io {
            operation,
            path: self.path.clone(),
            message: error.to_string(),
        }
    }
}

fn host_boot(error: impl std::fmt::Display) -> RsiError {
    RsiError::Boot(format!("Agent-preset Settings bootstrap failed: {error}"))
}

fn settings_boot(error: impl std::fmt::Display) -> RsiError {
    RsiError::Boot(format!(
        "invalid `{AGENT_PRESET_SETTINGS_NAMESPACE}` Settings: {error}"
    ))
}
