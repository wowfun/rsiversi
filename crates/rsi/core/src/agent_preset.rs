use crate::{Result, RsiError};
use async_trait::async_trait;
use rsi_agent_presets::{
    AgentPresetCatalog, AgentPresetCatalogConfig, AgentPresetDefaultStore, AgentPresetId,
    AgentPresetProfileCompiler, AgentPresetRoot, AgentPresetTrust, MAX_ROOTS, PresetError,
};
use rsi_host::{HostBuilder, HostPaths, Profile, ProfileEntry, RunningHost};
use rsi_meta::UpdateMode;
use rsi_meta_profile::{ProfileCompiler, ProfileEnvironment, ProfileLimits};
use rsi_settings_protocol::{
    SettingsContract, SettingsError, SettingsProviderContract, SettingsRegistration, SettingsScope,
    SettingsSpec, ValidateWith,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// Settings namespace owned by standard Agent-preset selection and roots.
pub const AGENT_PRESET_SETTINGS_NAMESPACE: &str = "rsi.agent-presets";
/// Deployment default used when the user has not selected an override.
pub const DEFAULT_AGENT_PRESET_ID: &str = "standard";
/// Directory below the standard configuration root that owns user presets.
pub const USER_AGENT_PRESET_DIRECTORY: &str = "agent-presets";

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
    catalog: AgentPresetCatalog,
    registration: Option<SettingsRegistration>,
    host: RunningHost,
}

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
        let id = AgentPresetId::new(DEFAULT_AGENT_PRESET_ID)
            .map_err(|error| RsiError::Boot(error.to_string()))?;
        let path = system_root.into().join(id.as_str());
        Self::open_with_system_sources(
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
        paths: HostPaths,
        system_sources: Vec<SystemPresetSource>,
        coding_tools_enabled: bool,
    ) -> Result<Self> {
        let base_default = AgentPresetId::new(DEFAULT_AGENT_PRESET_ID)
            .map_err(|error| RsiError::Boot(error.to_string()))?;
        let settings_path = paths.config().join("settings.json");
        let host = boot_settings_host(paths.clone(), &settings_path).await?;
        let Some(settings) = host.lookup_local::<SettingsContract>() else {
            let _shutdown = host.shutdown().await;
            return Err(RsiError::Boot(
                "Agent-preset Settings registry did not become active".into(),
            ));
        };
        let registration = match settings.register(SettingsSpec {
            namespace: AGENT_PRESET_SETTINGS_NAMESPACE.into(),
            defaults: json!({ "default": DEFAULT_AGENT_PRESET_ID, "roots": [] }),
            base: json!({}),
            validator: Arc::new(ValidateWith(validate_settings)),
        }) {
            Ok(registration) => registration,
            Err(error) => {
                let _shutdown = host.shutdown().await;
                return Err(settings_boot(error));
            }
        };
        let wire = match read_settings(registration.scope.as_ref()) {
            Ok(wire) => wire,
            Err(error) => {
                drop(registration);
                let _shutdown = host.shutdown().await;
                return Err(settings_boot(error));
            }
        };
        let mut config = AgentPresetCatalogConfig::new(base_default);
        for source in system_sources {
            config = match source {
                SystemPresetSource::Root(path) => config.with_system_root(path),
                SystemPresetSource::Exact { id, path } => config.with_system_preset(id, path),
            };
        }
        for root in &wire.roots {
            let root = match AgentPresetRoot::new(root.path.clone(), root.trust.into()) {
                Ok(root) => root,
                Err(error) => {
                    drop(registration);
                    let _shutdown = host.shutdown().await;
                    return Err(preset_boot(&error));
                }
            };
            config = config.with_configured_root(root);
        }
        config = config.with_user_root(user_agent_preset_root(&paths));
        let defaults: Arc<dyn AgentPresetDefaultStore> = Arc::new(SettingsDefaultStore {
            scope: Arc::clone(&registration.scope),
            path: settings_path,
        });
        let compiler = standard_agent_profile_compiler(&paths, coding_tools_enabled)?;
        let catalog = match AgentPresetCatalog::with_default_store(config, defaults, compiler) {
            Ok(catalog) => catalog,
            Err(error) => {
                drop(registration);
                let _shutdown = host.shutdown().await;
                return Err(preset_boot(&error));
            }
        };
        Ok(Self {
            catalog,
            registration: Some(registration),
            host,
        })
    }

    /// Returns the live catalog backed by this manager's Settings scope.
    pub const fn catalog(&self) -> &AgentPresetCatalog {
        &self.catalog
    }

    /// Releases namespace ownership before shutting down the management Host.
    pub async fn shutdown(mut self) -> rsi_meta::ShutdownOutcome {
        drop(self.registration.take());
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

async fn boot_settings_host(paths: HostPaths, settings_path: &Path) -> Result<RunningHost> {
    let mut builder = HostBuilder::new(paths);
    builder
        .register_local_contract::<SettingsProviderContract>()
        .and_then(|builder| builder.register_local_contract::<SettingsContract>())
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
    let host = builder.build().map_err(host_boot)?;
    host.start(Profile::new([
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
    ]))
    .await
    .map_err(host_boot)
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

fn preset_boot(error: &PresetError) -> RsiError {
    RsiError::Boot(error.to_string())
}
