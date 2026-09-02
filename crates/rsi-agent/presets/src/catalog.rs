use crate::authoring;
use crate::{AgentPresetId, AgentPresetProfileCompiler, PresetError, Result, clean_metadata_text};
use async_trait::async_trait;
use rsi_meta_profile::ProfileCandidate;
use serde::Deserialize;
use std::collections::BTreeSet;
use std::fs::{self, File};
use std::io::Read as _;
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// Required Profile source file inside one preset directory.
pub const COMPOSITION_FILE: &str = "agent.profile.toml";
/// Optional display metadata beside the Profile source.
pub const METADATA_FILE: &str = "preset.toml";
/// Maximum configured roots, including derived system and user roots.
pub const MAX_ROOTS: usize = 32;
/// Maximum filesystem rows examined by one roster discovery.
pub const MAX_ROSTER_ROWS: usize = 256;
/// Maximum encoded metadata bytes.
pub const MAX_METADATA_BYTES: usize = 16 * 1024;
/// Maximum directory depth copied by authoring.
pub const MAX_COPY_DEPTH: usize = 32;
/// Maximum filesystem entries traversed by authoring.
pub const MAX_COPY_ENTRIES: usize = 256;
/// Maximum aggregate bytes copied by authoring.
pub const MAX_COPY_BYTES: u64 = 16 * 1024 * 1024;

/// Trust assigned by the root from which a preset was discovered.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AgentPresetTrust {
    /// Deployment-owned preset.
    System,
    /// Locally authored preset with shell-equivalent trust.
    User,
}

/// Root class that supplied the winning preset row.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AgentPresetSource {
    /// Product-supplied system root.
    System,
    /// Explicit Settings-configured read-only root.
    Configured,
    /// Standard writable user root.
    User,
}

/// One explicitly configured, read-only discovery root.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AgentPresetRoot {
    path: PathBuf,
    trust: AgentPresetTrust,
}

/// One immutable root input that participates in standard Host selection.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AgentPresetLaunchRoot {
    /// Exact absolute discovery authority.
    pub path: PathBuf,
    /// Optional sole preset identity for a byte-verified system asset.
    pub exact_id: Option<AgentPresetId>,
    /// Root precedence class.
    pub source: AgentPresetSource,
    /// Trust inherited by presets from this root.
    pub trust: AgentPresetTrust,
    /// Whether catalog authoring may mutate this root.
    pub writable: bool,
}

/// Frozen Agent-preset inputs relevant to one standard Host generation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AgentPresetLaunchIdentity {
    /// Deployment default before any user Settings override.
    pub base_default: AgentPresetId,
    /// Ordered discovery roots and exact-preset authorities.
    pub roots: Vec<AgentPresetLaunchRoot>,
}

impl AgentPresetRoot {
    /// Creates one root from explicit absolute filesystem authority.
    ///
    /// # Errors
    ///
    /// Returns [`PresetError::InvalidRoot`] when `path` is not absolute.
    pub fn new(path: impl Into<PathBuf>, trust: AgentPresetTrust) -> Result<Self> {
        let path = path.into();
        validate_root_path(&path)?;
        Ok(Self { path, trust })
    }

    /// Returns the exact configured path.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Returns the trust assigned to rows from this root.
    pub const fn trust(&self) -> AgentPresetTrust {
        self.trust
    }
}

/// Frozen catalog roots and deployment default.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AgentPresetCatalogConfig {
    base_default: AgentPresetId,
    system_sources: Vec<SystemSourceSpec>,
    configured_roots: Vec<AgentPresetRoot>,
    user_root: Option<PathBuf>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum SystemSourceSpec {
    Root(PathBuf),
    Preset { id: AgentPresetId, path: PathBuf },
}

impl AgentPresetCatalogConfig {
    /// Creates a catalog configuration without derived roots.
    pub fn new(base_default: AgentPresetId) -> Self {
        Self {
            base_default,
            system_sources: Vec::new(),
            configured_roots: Vec::new(),
            user_root: None,
        }
    }

    /// Appends one deployment-owned system root before every configured root.
    #[must_use]
    pub fn with_system_root(mut self, path: impl Into<PathBuf>) -> Self {
        self.system_sources
            .push(SystemSourceSpec::Root(path.into()));
        self
    }

    /// Appends one exact deployment-owned preset directory.
    ///
    /// Unlike a system root, this authority never discovers sibling
    /// directories. It is intended for a byte-verified shipped asset whose
    /// containing cache directory is not itself trusted.
    #[must_use]
    pub fn with_system_preset(mut self, id: AgentPresetId, path: impl Into<PathBuf>) -> Self {
        self.system_sources.push(SystemSourceSpec::Preset {
            id,
            path: path.into(),
        });
        self
    }

    /// Appends one configured read-only root.
    #[must_use]
    pub fn with_configured_root(mut self, root: AgentPresetRoot) -> Self {
        self.configured_roots.push(root);
        self
    }

    /// Appends the sole locally writable user root.
    #[must_use]
    pub fn with_user_root(mut self, path: impl Into<PathBuf>) -> Self {
        self.user_root = Some(path.into());
        self
    }
}

/// Catalog health of one discoverable id.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum AgentPresetHealth {
    /// Composition passed bounded pure Profile compilation.
    Healthy,
    /// Composition cannot be handed to later Profile compilation.
    Broken {
        /// Safe reason naming the owning file condition.
        reason: String,
    },
}

/// One path-free roster row.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AgentPresetRow {
    /// Stable directory identity.
    pub id: AgentPresetId,
    /// Root class that supplied this winning row.
    pub source: AgentPresetSource,
    /// Trust inherited from the winning root.
    pub trust: AgentPresetTrust,
    /// Whether this is the effective default.
    pub is_default: bool,
    /// Optional display name.
    pub name: Option<String>,
    /// Optional one-sentence description.
    pub description: Option<String>,
    /// Current filesystem health.
    pub health: AgentPresetHealth,
}

/// Fresh roster plus its local authoring capability.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AgentPresetRoster {
    /// First-root-wins rows in deterministic root and metadata order.
    pub presets: Vec<AgentPresetRow>,
    /// Whether an explicit writable user root is configured.
    pub authorable: bool,
}

/// One preset's bounded composition document.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AgentPresetDocument {
    /// Stable preset id.
    pub id: AgentPresetId,
    /// Root class that supplied the document.
    pub source: AgentPresetSource,
    /// Winning root trust.
    pub trust: AgentPresetTrust,
    /// Exact UTF-8 Profile source.
    pub content: String,
    /// Optional display name.
    pub name: Option<String>,
    /// Optional description.
    pub description: Option<String>,
}

/// User-default persistence adapter supplied by the owning application.
#[async_trait]
pub trait AgentPresetDefaultStore: std::fmt::Debug + Send + Sync + 'static {
    /// Loads the current user override, or `None` to inherit the deployment base.
    async fn load(&self) -> Result<Option<AgentPresetId>>;
    /// Atomically replaces or clears the user override.
    async fn replace(&self, selected: Option<AgentPresetId>) -> Result<()>;
}

#[derive(Debug)]
struct NoDefaultOverride;

#[async_trait]
impl AgentPresetDefaultStore for NoDefaultOverride {
    async fn load(&self) -> Result<Option<AgentPresetId>> {
        Ok(None)
    }

    async fn replace(&self, selected: Option<AgentPresetId>) -> Result<()> {
        if selected.is_some() {
            Err(PresetError::DefaultStoreUnavailable)
        } else {
            Ok(())
        }
    }
}

#[derive(Clone, Debug)]
struct RootSpec {
    path: PathBuf,
    exact_id: Option<AgentPresetId>,
    source: AgentPresetSource,
    trust: AgentPresetTrust,
    writable: bool,
}

#[derive(Clone, Debug, Default)]
struct Metadata {
    name: Option<String>,
    description: Option<String>,
    order: Option<i64>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct MetadataWire {
    name: Option<String>,
    description: Option<String>,
    order: Option<i64>,
}

#[derive(Clone, Debug)]
struct DiscoveredPreset {
    row: AgentPresetRow,
}

#[derive(Clone, Debug)]
struct ResolvedPreset {
    id: AgentPresetId,
    source: AgentPresetSource,
    trust: AgentPresetTrust,
    path: PathBuf,
    writable: bool,
    metadata: Metadata,
}

/// Bounded process-local Agent-preset catalog.
#[derive(Clone, Debug)]
pub struct AgentPresetCatalog {
    roots: Vec<RootSpec>,
    user_root: Option<PathBuf>,
    base_default: AgentPresetId,
    defaults: Arc<dyn AgentPresetDefaultStore>,
    compiler: AgentPresetProfileCompiler,
    mutations: Arc<tokio::sync::Mutex<()>>,
}

impl AgentPresetCatalog {
    /// Freezes root precedence without touching the filesystem.
    ///
    /// # Errors
    ///
    /// Returns an error for a relative or duplicate root, or more than
    /// [`MAX_ROOTS`] roots.
    pub fn new(
        config: AgentPresetCatalogConfig,
        compiler: AgentPresetProfileCompiler,
    ) -> Result<Self> {
        Self::with_default_store(config, Arc::new(NoDefaultOverride), compiler)
    }

    /// Freezes root precedence with an injected user-default store.
    ///
    /// # Errors
    ///
    /// Returns an error for a relative or duplicate root, or more than
    /// [`MAX_ROOTS`] roots.
    pub fn with_default_store(
        config: AgentPresetCatalogConfig,
        defaults: Arc<dyn AgentPresetDefaultStore>,
        compiler: AgentPresetProfileCompiler,
    ) -> Result<Self> {
        let mut roots = Vec::new();
        for source in config.system_sources {
            let (path, exact_id) = match source {
                SystemSourceSpec::Root(path) => (path, None),
                SystemSourceSpec::Preset { id, path } => (path, Some(id)),
            };
            validate_root_path(&path)?;
            roots.push(RootSpec {
                path,
                exact_id,
                source: AgentPresetSource::System,
                trust: AgentPresetTrust::System,
                writable: false,
            });
        }
        roots.extend(config.configured_roots.into_iter().map(|root| RootSpec {
            path: root.path,
            exact_id: None,
            source: AgentPresetSource::Configured,
            trust: root.trust,
            writable: false,
        }));
        if let Some(path) = &config.user_root {
            validate_root_path(path)?;
            roots.push(RootSpec {
                path: path.clone(),
                exact_id: None,
                source: AgentPresetSource::User,
                trust: AgentPresetTrust::User,
                writable: true,
            });
        }
        if roots.len() > MAX_ROOTS {
            return Err(PresetError::TooManyRoots { maximum: MAX_ROOTS });
        }
        let mut seen = BTreeSet::new();
        for root in &roots {
            if !seen.insert(root.path.clone()) {
                return Err(PresetError::InvalidRoot(format!(
                    "duplicate root {}",
                    root.path.display()
                )));
            }
        }
        Ok(Self {
            roots,
            user_root: config.user_root,
            base_default: config.base_default,
            defaults,
            compiler,
            mutations: Arc::new(tokio::sync::Mutex::new(())),
        })
    }

    /// Returns the frozen root/trust identity without reading preset sources or Settings.
    ///
    /// The current user default and every preset source digest are deliberately
    /// excluded: they select an Agent generation inside an already running Host.
    pub fn launch_identity(&self) -> AgentPresetLaunchIdentity {
        AgentPresetLaunchIdentity {
            base_default: self.base_default.clone(),
            roots: self
                .roots
                .iter()
                .map(|root| AgentPresetLaunchRoot {
                    path: root.path.clone(),
                    exact_id: root.exact_id.clone(),
                    source: root.source,
                    trust: root.trust,
                    writable: root.writable,
                })
                .collect(),
        }
    }

    /// Re-reads every root and returns a path-free roster.
    ///
    /// # Errors
    ///
    /// Returns an error when the default adapter or root scan fails, or the
    /// scan exceeds [`MAX_ROSTER_ROWS`].
    pub async fn roster(&self) -> Result<AgentPresetRoster> {
        let default = self.default_id().await?;
        let mut presets = self.discover()?;
        for preset in &mut presets {
            preset.row.is_default = preset.row.id == default;
        }
        Ok(AgentPresetRoster {
            presets: presets.into_iter().map(|preset| preset.row).collect(),
            authorable: self.user_root.is_some(),
        })
    }

    /// Reads one healthy preset's exact bounded Profile source.
    ///
    /// # Errors
    ///
    /// Returns an error when `id` is absent or broken, or its bounded file
    /// changes while it is read.
    pub fn document(&self, id: &AgentPresetId) -> Result<AgentPresetDocument> {
        let preset = self.resolve_selected(id)?;
        let path = preset.path.join(COMPOSITION_FILE);
        let content = read_utf8_regular(&path, MAX_COPY_BYTES, "read composition")?;
        self.compiler
            .compile(&path)
            .map_err(|reason| PresetError::BrokenPreset {
                id: id.as_str().to_owned(),
                reason,
            })?;
        if read_utf8_regular(&path, MAX_COPY_BYTES, "re-read composition")? != content {
            return Err(PresetError::BrokenPreset {
                id: id.as_str().to_owned(),
                reason: "composition changed while it was compiled".into(),
            });
        }
        Ok(AgentPresetDocument {
            id: preset.id,
            source: preset.source,
            trust: preset.trust,
            content,
            name: preset.metadata.name,
            description: preset.metadata.description,
        })
    }

    /// Rebuilds one selected preset into a pure Profile candidate.
    ///
    /// This is the authoritative source-compilation seam for Agent generation
    /// construction. Roster health is only an earlier point-in-time result.
    ///
    /// # Errors
    ///
    /// Returns a bounded redacted [`PresetError::BrokenPreset`] when the
    /// selected source or one of its transitive includes no longer compiles.
    pub fn compile(&self, id: &AgentPresetId) -> Result<ProfileCandidate> {
        let preset = self.resolve_selected(id)?;
        self.compiler
            .compile(preset.path.join(COMPOSITION_FILE))
            .map_err(|reason| PresetError::BrokenPreset {
                id: id.as_str().to_owned(),
                reason,
            })
    }

    /// Resolves one id to its winning local directory.
    ///
    /// # Errors
    ///
    /// Returns an error when root discovery fails or `id` is absent.
    pub fn location(&self, id: &AgentPresetId) -> Result<PathBuf> {
        Ok(self.resolve_selected(id)?.path)
    }

    /// Returns the user override or the deployment base default beneath it.
    ///
    /// # Errors
    ///
    /// Returns an error when the injected default adapter cannot be read.
    pub async fn default_id(&self) -> Result<AgentPresetId> {
        Ok(self
            .defaults
            .load()
            .await?
            .unwrap_or_else(|| self.base_default.clone()))
    }

    /// Stores one syntactically valid preset identity as the user default.
    ///
    /// # Errors
    ///
    /// Returns an error when the injected default adapter cannot persist the
    /// selection. Availability is resolved only when a generation is needed.
    pub async fn set_default(&self, id: &AgentPresetId) -> Result<()> {
        let _mutation = self.mutations.lock().await;
        self.defaults.replace(Some(id.clone())).await
    }

    /// Clears the user override and re-inherits the deployment base.
    ///
    /// # Errors
    ///
    /// Returns an error when the injected default adapter cannot clear its
    /// override.
    pub async fn clear_default(&self) -> Result<()> {
        let _mutation = self.mutations.lock().await;
        self.defaults.replace(None).await
    }

    /// Copies one discovered preset into the explicit user root and publishes it atomically.
    ///
    /// # Errors
    ///
    /// Returns an error for an absent source or user root, an occupied target,
    /// unsafe or over-limit source content, or a filesystem failure.
    pub async fn copy(
        &self,
        source: &AgentPresetId,
        target: AgentPresetId,
        name: Option<String>,
    ) -> Result<()> {
        let _mutation = self.mutations.lock().await;
        let source = self.resolve_selected(source)?;
        if self.id_is_occupied(&target)? {
            return Err(PresetError::PresetExists {
                id: target.as_str().to_owned(),
            });
        }
        let user_root = self.user_root.as_deref().ok_or(PresetError::NoUserRoot)?;
        authoring::copy_preset(&source.path, user_root, &target, name)
    }

    /// Deletes one preset whose winning row comes from the explicit user root.
    ///
    /// # Errors
    ///
    /// Returns an error for an absent, read-only, or base-default row, a
    /// default-adapter failure, or a filesystem failure.
    pub async fn delete(&self, id: &AgentPresetId) -> Result<()> {
        let _mutation = self.mutations.lock().await;
        let preset = self.resolve_selected(id)?;
        if !preset.writable {
            return Err(PresetError::ReadOnlyPreset {
                id: id.as_str().to_owned(),
            });
        }
        if *id == self.base_default {
            return Err(PresetError::BaseDefaultPreset {
                id: id.as_str().to_owned(),
            });
        }
        if self.defaults.load().await?.as_ref() == Some(id) {
            self.defaults.replace(None).await?;
        }
        let user_root = self.user_root.as_deref().ok_or(PresetError::NoUserRoot)?;
        authoring::delete_preset(user_root, id)
    }

    fn id_is_occupied(&self, id: &AgentPresetId) -> Result<bool> {
        for root in &self.roots {
            let Some(path) = preset_path(root, id) else {
                continue;
            };
            match fs::symlink_metadata(&path) {
                Ok(_) => return Ok(true),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(io_error("inspect target id", &path, error)),
            }
        }
        Ok(false)
    }

    fn resolve_selected(&self, id: &AgentPresetId) -> Result<ResolvedPreset> {
        for root in &self.roots {
            if let Some(preset) = resolve_in_root(root, id)? {
                return Ok(preset);
            }
        }
        let available = self.available_ids()?;
        Err(PresetError::PresetNotFound {
            id: id.as_str().to_owned(),
            available,
        })
    }

    fn available_ids(&self) -> Result<Vec<String>> {
        let mut ids = BTreeSet::new();
        let mut examined = 0_usize;
        for root in &self.roots {
            if let Some(id) = &root.exact_id {
                let metadata = match fs::symlink_metadata(&root.path) {
                    Ok(metadata) => metadata,
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                    Err(error) => return Err(io_error("inspect preset row", &root.path, error)),
                };
                examined = examined.saturating_add(1);
                if examined > MAX_ROSTER_ROWS {
                    return Err(PresetError::RosterCapacity {
                        maximum: MAX_ROSTER_ROWS,
                    });
                }
                if !metadata.file_type().is_dir() {
                    return Err(PresetError::InvalidRoot(format!(
                        "exact preset is not a real directory: {}",
                        root.path.display()
                    )));
                }
                ids.insert(id.as_str().to_owned());
                continue;
            }
            let metadata = match fs::symlink_metadata(&root.path) {
                Ok(metadata) => metadata,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                Err(error) => return Err(io_error("inspect root", &root.path, error)),
            };
            if !metadata.file_type().is_dir() {
                return Err(PresetError::InvalidRoot(format!(
                    "root is not a real directory: {}",
                    root.path.display()
                )));
            }
            for entry in fs::read_dir(&root.path)
                .map_err(|error| io_error("read root", &root.path, error))?
            {
                let entry =
                    entry.map_err(|error| io_error("read root entry", &root.path, error))?;
                examined = examined.saturating_add(1);
                if examined > MAX_ROSTER_ROWS {
                    return Err(PresetError::RosterCapacity {
                        maximum: MAX_ROSTER_ROWS,
                    });
                }
                let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
                    continue;
                };
                let Ok(id) = AgentPresetId::new(name) else {
                    continue;
                };
                let file_type = entry
                    .file_type()
                    .map_err(|error| io_error("inspect preset row", &entry.path(), error))?;
                if file_type.is_dir() {
                    ids.insert(id.as_str().to_owned());
                }
            }
        }
        Ok(ids.into_iter().collect())
    }

    fn discover(&self) -> Result<Vec<DiscoveredPreset>> {
        let mut found = Vec::new();
        let mut ids = BTreeSet::new();
        let mut examined = 0_usize;
        for root in &self.roots {
            for preset in scan_root(root, &self.compiler, &mut examined)? {
                if ids.insert(preset.row.id.clone()) {
                    found.push(preset);
                }
            }
        }
        for preset in &mut found {
            preset.row.is_default = preset.row.id == self.base_default;
        }
        Ok(found)
    }
}

fn resolve_in_root(root: &RootSpec, id: &AgentPresetId) -> Result<Option<ResolvedPreset>> {
    let Some(path) = preset_path(root, id) else {
        return Ok(None);
    };
    if root.exact_id.is_none() {
        let root_metadata = match fs::symlink_metadata(&root.path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(io_error("inspect root", &root.path, error)),
        };
        if !root_metadata.file_type().is_dir() {
            return Err(PresetError::InvalidRoot(format!(
                "root is not a real directory: {}",
                root.path.display()
            )));
        }
    }
    let metadata = match fs::symlink_metadata(&path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(io_error("inspect preset row", &path, error)),
    };
    if !metadata.file_type().is_dir() {
        return Ok(None);
    }
    Ok(Some(ResolvedPreset {
        id: id.clone(),
        source: root.source,
        trust: root.trust,
        metadata: read_metadata(&path.join(METADATA_FILE)),
        path,
        writable: root.writable,
    }))
}

fn preset_path(root: &RootSpec, id: &AgentPresetId) -> Option<PathBuf> {
    match &root.exact_id {
        Some(exact_id) if exact_id == id => Some(root.path.clone()),
        Some(_) => None,
        None => Some(root.path.join(id.as_str())),
    }
}

fn validate_root_path(path: &Path) -> Result<()> {
    if !path.is_absolute() || path.as_os_str().is_empty() {
        return Err(PresetError::InvalidRoot(format!(
            "root must be an absolute path: {}",
            path.display()
        )));
    }
    Ok(())
}

fn scan_root(
    root: &RootSpec,
    compiler: &AgentPresetProfileCompiler,
    examined: &mut usize,
) -> Result<Vec<DiscoveredPreset>> {
    if let Some(id) = &root.exact_id {
        let metadata = match fs::symlink_metadata(&root.path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(error) => return Err(io_error("inspect preset row", &root.path, error)),
        };
        *examined = examined.saturating_add(1);
        if *examined > MAX_ROSTER_ROWS {
            return Err(PresetError::RosterCapacity {
                maximum: MAX_ROSTER_ROWS,
            });
        }
        if !metadata.file_type().is_dir() {
            return Err(PresetError::InvalidRoot(format!(
                "exact preset is not a real directory: {}",
                root.path.display()
            )));
        }
        return Ok(vec![discovered_preset(
            root,
            id.clone(),
            &root.path,
            compiler,
        )]);
    }
    let root_metadata = match fs::symlink_metadata(&root.path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(io_error("inspect root", &root.path, error)),
    };
    if !root_metadata.file_type().is_dir() {
        return Err(PresetError::InvalidRoot(format!(
            "root is not a real directory: {}",
            root.path.display()
        )));
    }
    let entries =
        fs::read_dir(&root.path).map_err(|error| io_error("read root", &root.path, error))?;
    let mut presets = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|error| io_error("read root entry", &root.path, error))?;
        *examined = examined.saturating_add(1);
        if *examined > MAX_ROSTER_ROWS {
            return Err(PresetError::RosterCapacity {
                maximum: MAX_ROSTER_ROWS,
            });
        }
        let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
            continue;
        };
        let Ok(id) = AgentPresetId::new(name) else {
            continue;
        };
        let path = entry.path();
        let file_type = entry
            .file_type()
            .map_err(|error| io_error("inspect preset row", &path, error))?;
        if !file_type.is_dir() {
            continue;
        }
        let metadata = read_metadata(&path.join(METADATA_FILE));
        let health = composition_health(&path.join(COMPOSITION_FILE), compiler);
        presets.push((
            metadata.order,
            DiscoveredPreset {
                row: AgentPresetRow {
                    id,
                    source: root.source,
                    trust: root.trust,
                    is_default: false,
                    name: metadata.name,
                    description: metadata.description,
                    health,
                },
            },
        ));
    }
    presets.sort_by(|(left_order, left), (right_order, right)| {
        left_order
            .unwrap_or(i64::MAX)
            .cmp(&right_order.unwrap_or(i64::MAX))
            .then_with(|| left.row.id.cmp(&right.row.id))
    });
    Ok(presets.into_iter().map(|(_, preset)| preset).collect())
}

fn discovered_preset(
    root: &RootSpec,
    id: AgentPresetId,
    path: &Path,
    compiler: &AgentPresetProfileCompiler,
) -> DiscoveredPreset {
    let metadata = read_metadata(&path.join(METADATA_FILE));
    let health = composition_health(&path.join(COMPOSITION_FILE), compiler);
    DiscoveredPreset {
        row: AgentPresetRow {
            id,
            source: root.source,
            trust: root.trust,
            is_default: false,
            name: metadata.name,
            description: metadata.description,
            health,
        },
    }
}

fn composition_health(path: &Path, compiler: &AgentPresetProfileCompiler) -> AgentPresetHealth {
    match compiler.compile(path) {
        Ok(_) => AgentPresetHealth::Healthy,
        Err(reason) => AgentPresetHealth::Broken { reason },
    }
}

fn read_metadata(path: &Path) -> Metadata {
    let Ok(content) = read_utf8_regular(
        path,
        u64::try_from(MAX_METADATA_BYTES).expect("metadata bound fits u64"),
        "read metadata",
    ) else {
        return Metadata::default();
    };
    let Ok(wire) = toml::from_str::<MetadataWire>(&content) else {
        return Metadata::default();
    };
    Metadata {
        name: clean_metadata_text(wire.name),
        description: clean_metadata_text(wire.description),
        order: wire.order,
    }
}

fn read_utf8_regular(path: &Path, maximum: u64, operation: &'static str) -> Result<String> {
    let initial = fs::symlink_metadata(path).map_err(|error| io_error(operation, path, error))?;
    if !initial.file_type().is_file() {
        return Err(PresetError::BrokenPreset {
            id: path
                .parent()
                .and_then(Path::file_name)
                .and_then(|name| name.to_str())
                .unwrap_or("<unknown>")
                .to_owned(),
            reason: format!(
                "the composition file {} is not a no-follow regular file",
                bounded_file_label(path)
            ),
        });
    }
    if initial.len() > maximum {
        return Err(PresetError::BrokenPreset {
            id: path
                .parent()
                .and_then(Path::file_name)
                .and_then(|name| name.to_str())
                .unwrap_or("<unknown>")
                .to_owned(),
            reason: format!(
                "the composition file {} exceeds {maximum} bytes",
                bounded_file_label(path)
            ),
        });
    }
    #[cfg(unix)]
    let file = {
        use rustix::fs::{Mode, OFlags};
        rustix::fs::open(
            path,
            OFlags::RDONLY | OFlags::CLOEXEC | OFlags::NONBLOCK | OFlags::NOFOLLOW,
            Mode::empty(),
        )
        .map(File::from)
        .map_err(|error| io_error(operation, path, error))?
    };
    #[cfg(not(unix))]
    let file = File::open(path).map_err(|error| io_error(operation, path, error))?;
    let opened = file
        .metadata()
        .map_err(|error| io_error(operation, path, error))?;
    if !opened.is_file() || opened.len() != initial.len() {
        return Err(PresetError::BrokenPreset {
            id: path
                .parent()
                .and_then(Path::file_name)
                .and_then(|name| name.to_str())
                .unwrap_or("<unknown>")
                .to_owned(),
            reason: format!(
                "the composition file {} changed while it was opened",
                bounded_file_label(path)
            ),
        });
    }
    let capacity = usize::try_from(opened.len()).map_err(|_| PresetError::BrokenPreset {
        id: "<unknown>".to_owned(),
        reason: format!(
            "the composition file {} length does not fit memory",
            bounded_file_label(path)
        ),
    })?;
    let mut bytes = Vec::with_capacity(capacity);
    file.take(maximum.saturating_add(1))
        .read_to_end(&mut bytes)
        .map_err(|error| io_error(operation, path, error))?;
    if u64::try_from(bytes.len()).unwrap_or(u64::MAX) > maximum {
        return Err(PresetError::BrokenPreset {
            id: "<unknown>".to_owned(),
            reason: format!(
                "the composition file {} grew beyond {maximum} bytes",
                bounded_file_label(path)
            ),
        });
    }
    String::from_utf8(bytes).map_err(|_| PresetError::BrokenPreset {
        id: path
            .parent()
            .and_then(Path::file_name)
            .and_then(|name| name.to_str())
            .unwrap_or("<unknown>")
            .to_owned(),
        reason: format!(
            "the composition file {} is not UTF-8",
            bounded_file_label(path)
        ),
    })
}

fn bounded_file_label(path: &Path) -> &str {
    path.file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("<unknown>")
}

fn io_error(operation: &'static str, path: &Path, error: impl std::fmt::Display) -> PresetError {
    PresetError::Io {
        operation,
        path: path.to_path_buf(),
        message: error.to_string(),
    }
}
