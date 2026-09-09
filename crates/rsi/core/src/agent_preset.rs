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
    composition_identity: String,
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
    /// composition supplies the exact Profile defines and contribution allowlist
    /// used by service startup.
    pub async fn open<I, P>(
        composition: &crate::StandardComposition,
        system_roots: I,
    ) -> Result<Self>
    where
        I: IntoIterator<Item = P>,
        P: Into<PathBuf>,
    {
        Self::open_with_system_sources(
            None,
            composition,
            system_roots
                .into_iter()
                .map(|root| SystemPresetSource::Root(root.into()))
                .collect(),
        )
        .await
    }

    /// Opens the standard Settings document with the sole byte-verified
    /// built-in preset, without trusting sibling cache directories.
    pub async fn open_standard(
        composition: &crate::StandardComposition,
        system_root: impl Into<PathBuf>,
    ) -> Result<Self> {
        Self::open_standard_with_context(None, composition, system_root.into()).await
    }

    /// Mounts the standard catalog and Settings plugins below an existing Context.
    pub async fn open_standard_in(
        parent: &rsi_meta::Context,
        composition: &crate::StandardComposition,
        system_root: impl Into<PathBuf>,
    ) -> Result<Self> {
        Self::open_standard_with_context(Some(parent), composition, system_root.into()).await
    }

    async fn open_standard_with_context(
        parent: Option<&rsi_meta::Context>,
        composition: &crate::StandardComposition,
        system_root: PathBuf,
    ) -> Result<Self> {
        let id = AgentPresetId::new(DEFAULT_AGENT_PRESET_ID)
            .map_err(|error| RsiError::Boot(error.to_string()))?;
        let path = system_root.join(id.as_str());
        Self::open_with_system_sources(
            parent,
            composition,
            vec![SystemPresetSource::Exact { id, path }],
        )
        .await
    }

    /// Opens the standard Settings-backed catalog for a non-activating preview.
    ///
    /// The built-in preset contributes its deterministic final cache location
    /// to launch identity without materializing that asset.
    pub async fn open_standard_preview(composition: &crate::StandardComposition) -> Result<Self> {
        Self::open_standard(
            composition,
            crate::composition::standard_agent_preset_root_candidate(composition.paths()),
        )
        .await
    }

    async fn open_with_system_sources(
        parent: Option<&rsi_meta::Context>,
        composition: &crate::StandardComposition,
        system_sources: Vec<SystemPresetSource>,
    ) -> Result<Self> {
        let composition_identity = composition.agent_compiler_identity()?;
        let paths = composition.paths().clone();
        let factory = Arc::new(plugin::CatalogFactory {
            compiler: composition.agent_profile_compiler()?,
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
        Ok(Self {
            catalog,
            host,
            composition_identity,
        })
    }

    pub(crate) fn composition_identity(&self) -> &str {
        &self.composition_identity
    }

    /// Returns the frozen base-declaration catalog backed by this manager's live Settings scope.
    pub fn catalog(&self) -> &AgentPresetCatalog {
        &self.catalog
    }

    /// Captures current declared native selection for one non-executing authoring operation.
    /// Host construction keeps using the base catalog; source health is not ABI admission.
    pub fn authoring_catalog(
        &self,
        composition: &crate::StandardComposition,
    ) -> Result<AgentPresetCatalog> {
        if self.composition_identity != composition.agent_compiler_identity()? {
            return Err(RsiError::Boot(
                "Agent-preset manager uses different addon declarations".into(),
            ));
        }
        Ok(self
            .catalog
            .as_ref()
            .clone()
            .with_compiler(composition.agent_authoring_compiler()?))
    }

    /// Disposes the management Profile and its ordinary catalog/Settings owners.
    pub async fn shutdown(self) -> rsi_meta::ShutdownOutcome {
        self.host.shutdown().await
    }
}

pub(crate) fn standard_agent_profile_compiler(
    paths: &HostPaths,
    linux_tools_enabled: bool,
    addons: &crate::StandardAddonSet,
) -> Result<AgentPresetProfileCompiler> {
    Ok(AgentPresetProfileCompiler::new(
        standard_agent_compiler(paths, linux_tools_enabled, addons)?,
        addons
            .descriptions()
            .filter(|factory| factory.scope == crate::AddonScope::Agent)
            .map(|factory| factory.plugin.clone()),
    ))
}

#[cfg(unix)]
pub(crate) fn native_agent_profile_compiler(
    paths: &HostPaths,
    linux_tools_enabled: bool,
    base: &crate::StandardAddonSet,
    selected: &[crate::NativeAddonRecord],
) -> Result<AgentPresetProfileCompiler> {
    use sha2::{Digest as _, Sha256};
    if selected.is_empty() {
        return standard_agent_profile_compiler(paths, linux_tools_enabled, base);
    }
    // Callers have checked the complete selection against this frozen base.
    // ABI metadata belongs to the independently frozen executable catalog.
    let mut digest = Sha256::new();
    digest.update(b"rsi.native-agent-declarations/v1\0");
    digest.update(base.digest().map_err(host_boot)?.as_bytes());
    digest.update(serde_json::to_vec(selected).map_err(host_boot)?);
    let compiler = agent_compiler(paths, linux_tools_enabled, hex::encode(digest.finalize()))?;
    Ok(AgentPresetProfileCompiler::new(
        compiler,
        base.descriptions()
            .filter(|entry| entry.scope == crate::AddonScope::Agent)
            .map(|entry| entry.plugin.clone())
            .chain(selected.iter().map(|entry| entry.plugin().to_owned())),
    ))
}

pub(crate) fn standard_agent_compiler_identity(
    paths: &HostPaths,
    linux_tools_enabled: bool,
    addons: &crate::StandardAddonSet,
) -> Result<String> {
    let candidate = standard_agent_compiler(paths, linux_tools_enabled, addons)?
        .compile(&ProfileProgram::from_profile(Profile::default()))
        .map_err(|error| RsiError::Boot(error.to_string()))?;
    Ok(candidate.source_digest().to_owned())
}

fn standard_agent_compiler(
    paths: &HostPaths,
    linux_tools_enabled: bool,
    addons: &crate::StandardAddonSet,
) -> Result<ProfileCompiler> {
    agent_compiler(
        paths,
        linux_tools_enabled,
        addons.digest().map_err(host_boot)?,
    )
}
fn agent_compiler(
    paths: &HostPaths,
    linux_tools_enabled: bool,
    declaration_digest: String,
) -> Result<ProfileCompiler> {
    let environment = ProfileEnvironment::new(
        paths.config(),
        paths.state(),
        paths.cache(),
        format!("{}-{}", std::env::consts::OS, std::env::consts::ARCH),
        BTreeMap::from([
            ("standard_unix_files".to_owned(), Value::Bool(cfg!(unix))),
            (
                "standard_linux_coding_tools".to_owned(),
                Value::Bool(linux_tools_enabled),
            ),
            (
                "rsi_standard_addons".to_owned(),
                Value::String(declaration_digest),
            ),
        ]),
    )
    .map_err(|error| RsiError::Boot(error.to_string()))?;
    Ok(ProfileCompiler::new(environment, ProfileLimits::default()))
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
