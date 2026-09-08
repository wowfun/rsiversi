//! Ordered, bounded Profile programs above `rsi-meta`.

#![deny(unsafe_code)]
#![warn(missing_docs)]
#![allow(clippy::missing_errors_doc)]

use rhai::packages::{
    ArithmeticPackage, BasicArrayPackage, BasicIteratorPackage, BasicMapPackage, BasicMathPackage,
    BasicStringPackage, LanguageCorePackage, LogicPackage, MoreStringPackage, Package as _,
};
use rhai::{Dynamic, Engine, Map, Scope};
use rsi_meta::{ConfigValue, InstanceId, PluginId};
use serde::Deserialize;
use serde_json::Value;
use sha2::{Digest as _, Sha256};
use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

mod bundle;
mod control;
#[cfg(test)]
mod isolation_tests;
pub use bundle::ProfileBundle;
#[cfg(not(target_family = "wasm"))]
mod native_source;
#[cfg(not(target_family = "wasm"))]
use native_source::read_profile_source;

pub use control::{
    ProfileBootstrap, ProfileControl, ProfileControlContract, ProfileGenerationPlan, ProfileHealth,
    ProfileInstanceState, ProfileInstanceStatus, ProfileResolver, ProfileSnapshot, ProfileStatus,
    ProfileTargetStatus, ReloadOutcome, SnapshotNode, WatcherHealth,
};

const PROFILE_FORMAT: u32 = 1;

/// Fixed resource limits for source loading and pure compilation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProfileLimits {
    /// Maximum bytes in one source document.
    pub maximum_document_bytes: usize,
    /// Maximum bytes across every file source in one rebuild.
    pub maximum_source_bytes: usize,
    /// Maximum transitive source files.
    pub maximum_source_files: usize,
    /// Maximum include nesting, including the root file.
    pub maximum_include_depth: usize,
    /// Maximum program steps across linked, file, and launch layers.
    pub maximum_steps: usize,
    /// Maximum nodes in the resulting declarative tree.
    pub maximum_nodes: usize,
    /// Maximum nested declarative groups, including the outermost group.
    pub maximum_group_depth: usize,
    /// Maximum isolation declarations across the resulting tree, including disabled groups.
    pub maximum_isolation_bindings: usize,
    /// Maximum bytes in an identifier, path diagnostic, or platform name.
    pub maximum_identifier_bytes: usize,
    /// Maximum Rhai operations across every expression in one rebuild.
    pub maximum_expression_operations: u64,
    /// Maximum Rhai expression and function-call nesting.
    pub maximum_expression_depth: usize,
    /// Maximum retained JSON bytes in the resolved tree.
    pub maximum_config_bytes: usize,
    /// Maximum bytes retained for one redacted operational diagnostic.
    pub maximum_diagnostic_bytes: usize,
}

impl Default for ProfileLimits {
    fn default() -> Self {
        Self {
            maximum_document_bytes: 1024 * 1024,
            maximum_source_bytes: 16 * 1024 * 1024,
            maximum_source_files: 256,
            maximum_include_depth: 32,
            maximum_steps: 16_384,
            maximum_nodes: 4_096,
            maximum_group_depth: 128,
            maximum_isolation_bindings: 16_384,
            maximum_identifier_bytes: 256,
            maximum_expression_operations: 100_000,
            maximum_expression_depth: 64,
            maximum_config_bytes: 16 * 1024 * 1024,
            maximum_diagnostic_bytes: 4 * 1024,
        }
    }
}

impl ProfileLimits {
    /// Validates every configured compiler and retained-state bound.
    pub fn validate(&self) -> Result<()> {
        validate_limits(self)
    }
}

/// Frozen values visible to pure Profile expressions.
#[derive(Clone, Debug, PartialEq)]
pub struct ProfileEnvironment {
    paths: Option<[PathBuf; 3]>,
    platform: String,
    defines: BTreeMap<String, ConfigValue>,
}

impl ProfileEnvironment {
    /// Creates an environment from explicit absolute paths and frozen values.
    pub fn new(
        config: impl Into<PathBuf>,
        state: impl Into<PathBuf>,
        cache: impl Into<PathBuf>,
        platform: impl Into<String>,
        defines: BTreeMap<String, ConfigValue>,
    ) -> Result<Self> {
        let config = absolute_path("config", config.into())?;
        let state = absolute_path("state", state.into())?;
        let cache = absolute_path("cache", cache.into())?;
        let platform = platform.into();
        if platform.is_empty() {
            return Err(ProfileError::InvalidEnvironment(
                "platform must not be empty".to_owned(),
            ));
        }
        Ok(Self {
            paths: Some([config, state, cache]),
            platform,
            defines,
        })
    }

    /// Creates an environment without filesystem authority.
    pub fn without_paths(
        platform: impl Into<String>,
        defines: BTreeMap<String, ConfigValue>,
    ) -> Result<Self> {
        let platform = platform.into();
        if platform.is_empty() {
            return Err(ProfileError::InvalidEnvironment(
                "platform must not be empty".into(),
            ));
        }
        Ok(Self {
            paths: None,
            platform,
            defines,
        })
    }

    /// Frozen configuration root, when supplied.
    pub fn config(&self) -> Option<&Path> {
        self.paths.as_ref().map(|paths| paths[0].as_path())
    }

    /// Frozen state root.
    pub fn state(&self) -> Option<&Path> {
        self.paths.as_ref().map(|paths| paths[1].as_path())
    }

    /// Frozen cache root.
    pub fn cache(&self) -> Option<&Path> {
        self.paths.as_ref().map(|paths| paths[2].as_path())
    }

    /// Frozen application-selected platform name.
    pub fn platform(&self) -> &str {
        &self.platform
    }

    /// Frozen application-defined JSON values.
    pub const fn defines(&self) -> &BTreeMap<String, ConfigValue> {
        &self.defines
    }

    /// Validates this complete frozen environment against the selected limits.
    pub fn validate(&self, limits: &ProfileLimits) -> Result<()> {
        limits.validate()?;
        if self.platform.len() > limits.maximum_identifier_bytes {
            return Err(ProfileError::CapacityExceeded {
                resource: "platform bytes",
                maximum: limits.maximum_identifier_bytes,
            });
        }
        let value = Value::Object(
            self.defines
                .clone()
                .into_iter()
                .collect::<serde_json::Map<_, _>>(),
        );
        validate_define_numbers(&value)?;
        bounded_json_bytes(&value, limits.maximum_config_bytes)?;
        Ok(())
    }
}

/// One programmatic desired plugin leaf.
#[derive(Clone, Debug, PartialEq)]
pub struct ProfileEntry {
    id: InstanceId,
    plugin: PluginId,
    config: ConfigValue,
}

impl ProfileEntry {
    /// Creates an enabled plugin leaf with literal configuration.
    pub fn new(
        id: impl Into<InstanceId>,
        plugin: impl Into<PluginId>,
        config: ConfigValue,
    ) -> Self {
        Self {
            id: id.into(),
            plugin: plugin.into(),
            config,
        }
    }

    /// Stable application identity.
    pub const fn id(&self) -> &InstanceId {
        &self.id
    }

    /// Host catalog key.
    pub const fn plugin(&self) -> &PluginId {
        &self.plugin
    }

    /// Complete literal desired configuration.
    pub const fn config(&self) -> &ConfigValue {
        &self.config
    }
}

/// One in-memory top-level Profile document.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Profile {
    entries: Vec<ProfileEntry>,
    steps: Vec<ProfileStep>,
}

impl Profile {
    /// Creates an in-memory document containing ordered plugin leaves.
    pub fn new(entries: impl IntoIterator<Item = ProfileEntry>) -> Self {
        Self {
            entries: entries.into_iter().collect::<Vec<_>>(),
            steps: Vec::new(),
        }
    }

    /// Ordered programmatic leaves.
    pub fn entries(&self) -> &[ProfileEntry] {
        &self.entries
    }

    /// Creates an in-memory document from groups, leaves, and strict patches.
    pub fn program(steps: impl IntoIterator<Item = ProfileStep>) -> Self {
        Self {
            entries: Vec::new(),
            steps: steps.into_iter().collect(),
        }
    }
}

/// Immutable linked program segment supplied by a Host.
#[derive(Clone, Debug, PartialEq)]
pub struct ProfileFragment {
    id: String,
    entries: Vec<ProfileEntry>,
    steps: Vec<ProfileStep>,
}

impl ProfileFragment {
    /// Fingerprints this declaration using the Profile source identity encoding.
    /// This performs no expression evaluation, target lookup, or configuration validation.
    /// A patch may therefore refer to a target supplied by another fragment.
    pub fn source_digest(&self) -> String {
        let compiler = ProfileCompiler::new(
            ProfileEnvironment {
                paths: None,
                platform: "fragment-identity".into(),
                defines: BTreeMap::new(),
            },
            ProfileLimits::default(),
        );
        let mut state = CompileState::new(&compiler);
        state.hash_fragment(self);
        hex_lower(state.digest.finalize().as_slice())
    }

    /// Creates an ordered linked fragment of plugin leaves.
    pub fn new(id: impl Into<String>, entries: impl IntoIterator<Item = ProfileEntry>) -> Self {
        Self {
            id: id.into(),
            entries: entries.into_iter().collect(),
            steps: Vec::new(),
        }
    }

    /// Stable Host registration key.
    pub fn id(&self) -> &str {
        &self.id
    }

    /// Ordered fragment leaves.
    pub fn entries(&self) -> &[ProfileEntry] {
        &self.entries
    }

    /// Creates a linked fragment from declarative nodes and strict patches.
    pub fn program(id: impl Into<String>, steps: impl IntoIterator<Item = ProfileStep>) -> Self {
        Self {
            id: id.into(),
            entries: Vec::new(),
            steps: steps.into_iter().collect(),
        }
    }
}

/// One programmatic ordered Profile step.
#[derive(Clone, Debug, PartialEq)]
pub enum ProfileStep {
    /// Appends one declarative node at the root.
    Node(ProfileNode),
    /// Applies one strict patch to the tree built so far.
    Patch(ProfilePatch),
}

/// One programmatic declarative group or plugin leaf.
#[derive(Clone, Debug, PartialEq)]
pub enum ProfileNode {
    /// Declarative group.
    Group(ProfileGroup),
    /// Enabled plugin leaf with literal config.
    Plugin(ProfileEntry),
}

impl Drop for ProfileNode {
    fn drop(&mut self) {
        let mut descendants = match self {
            Self::Group(group) => std::mem::take(&mut group.children),
            Self::Plugin(_) => return,
        };
        while let Some(mut node) = descendants.pop() {
            if let Self::Group(group) = &mut node {
                descendants.extend(std::mem::take(&mut group.children));
            }
        }
    }
}

/// One programmatic declarative group.
#[derive(Clone, Debug, PartialEq)]
pub struct ProfileGroup {
    id: String,
    enabled: bool,
    isolation: IsolationSpec,
    children: Vec<ProfileNode>,
}

impl ProfileGroup {
    /// Creates an enabled group without new isolation.
    pub fn new(id: impl Into<String>, children: impl IntoIterator<Item = ProfileNode>) -> Self {
        Self {
            id: id.into(),
            enabled: true,
            isolation: IsolationSpec::default(),
            children: children.into_iter().collect(),
        }
    }

    /// Replaces the group's literal enabled state.
    #[must_use]
    pub const fn enabled(mut self, enabled: bool) -> Self {
        self.enabled = enabled;
        self
    }

    /// Replaces the group's complete isolation declaration.
    #[must_use]
    pub fn isolation(mut self, isolation: IsolationSpec) -> Self {
        self.isolation = isolation;
        self
    }
}

/// One strict programmatic patch operation.
#[derive(Clone, Debug, PartialEq)]
pub enum ProfilePatch {
    /// Appends new child nodes to one group.
    Append {
        /// Existing group ID.
        target: String,
        /// New ordered children.
        nodes: Vec<ProfileNode>,
    },
    /// Replaces one plugin leaf's complete configuration value.
    ReplaceConfig {
        /// Existing plugin leaf ID.
        target: String,
        /// Complete replacement value.
        config: ConfigValue,
    },
    /// Replaces one node's enabled state; group disabling cascades at flattening.
    SetEnabled {
        /// Existing group or plugin ID.
        target: String,
        /// Replacement state.
        enabled: bool,
    },
    /// Replaces one group's complete isolation declaration.
    ReplaceIsolation {
        /// Existing group ID.
        target: String,
        /// Complete replacement declaration.
        isolation: IsolationSpec,
    },
}

/// Immutable source set rebuilt by startup and every reload.
#[derive(Clone, Debug, PartialEq)]
pub struct ProfileProgram {
    root: ProgramRoot,
    linked: Vec<ProfileFragment>,
    launch_patches: Vec<ProfilePatch>,
}

#[derive(Clone, Debug, PartialEq)]
enum ProgramRoot {
    #[cfg(not(target_family = "wasm"))]
    File(PathBuf),
    Memory(Profile),
    Bundle(ProfileBundle),
}

impl ProfileProgram {
    /// Uses one required root file and enables transitive watching.
    #[cfg(not(target_family = "wasm"))]
    pub fn from_file(path: impl Into<PathBuf>) -> Self {
        Self {
            root: ProgramRoot::File(path.into()),
            linked: Vec::new(),
            launch_patches: Vec::new(),
        }
    }

    /// Uses one immutable in-memory root document.
    pub fn from_profile(profile: Profile) -> Self {
        Self {
            root: ProgramRoot::Memory(profile),
            linked: Vec::new(),
            launch_patches: Vec::new(),
        }
    }

    /// Uses one immutable bounded source bundle.
    pub fn from_bundle(bundle: ProfileBundle) -> Self {
        Self {
            root: ProgramRoot::Bundle(bundle),
            linked: Vec::new(),
            launch_patches: Vec::new(),
        }
    }

    /// Replaces the ordered linked prefix frozen by a Host.
    #[must_use]
    pub fn with_linked_fragments(mut self, linked: Vec<ProfileFragment>) -> Self {
        self.linked = linked;
        self
    }

    /// Replaces the immutable ordered launch-patch suffix.
    #[must_use]
    pub fn with_launch_patches(mut self, patches: Vec<ProfilePatch>) -> Self {
        self.launch_patches = patches;
        self
    }
}

/// Complete group isolation replacement inherited by descendants.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct IsolationSpec {
    local: Arc<Vec<String>>,
    events: Arc<Vec<String>>,
    portable: Arc<Vec<String>>,
    named: Arc<Vec<NamedIsolation>>,
}

/// Independent namespace lane for Profile isolation.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum IsolationLane {
    /// Nominal safe-Rust Local services.
    Local,
    /// Nominal Local events.
    Event,
    /// Portable service keys.
    Portable,
}

/// One named allocation shared within one Profile activation namespace.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NamedIsolation {
    lane: IsolationLane,
    key: String,
    label: String,
}
impl NamedIsolation {
    /// Selected isolation lane.
    pub const fn lane(&self) -> IsolationLane {
        self.lane
    }
    /// Exact nominal contract or Portable service key.
    pub fn key(&self) -> &str {
        &self.key
    }
    /// Label shared only in this Profile namespace and exact lane/key.
    pub fn label(&self) -> &str {
        &self.label
    }
}

impl IsolationSpec {
    /// Creates a complete isolation declaration for all three contract lanes.
    pub fn new(
        local: impl IntoIterator<Item = String>,
        events: impl IntoIterator<Item = String>,
        portable: impl IntoIterator<Item = String>,
    ) -> Self {
        Self {
            local: Arc::new(local.into_iter().collect()),
            events: Arc::new(events.into_iter().collect()),
            portable: Arc::new(portable.into_iter().collect()),
            named: Arc::default(),
        }
    }

    /// Adds one named selection; the compiler rejects duplicate keys and invalid labels.
    #[must_use]
    pub fn with_named(
        mut self,
        lane: IsolationLane,
        key: impl Into<String>,
        label: impl Into<String>,
    ) -> Self {
        Arc::make_mut(&mut self.named).push(NamedIsolation {
            lane,
            key: key.into(),
            label: label.into(),
        });
        self
    }

    /// Named selections across all lanes, in declaration order.
    pub fn named(&self) -> &[NamedIsolation] {
        &self.named
    }

    fn len(&self) -> usize {
        self.local
            .len()
            .saturating_add(self.events.len())
            .saturating_add(self.portable.len())
            .saturating_add(self.named.len())
    }

    /// Stable Local contract keys receiving a fresh group identity.
    pub fn local(&self) -> &[String] {
        &self.local
    }

    /// Stable Local event keys receiving a fresh group identity.
    pub fn events(&self) -> &[String] {
        &self.events
    }

    /// Portable service keys receiving a fresh group identity.
    pub fn portable(&self) -> &[String] {
        &self.portable
    }
}

/// One enabled leaf after source execution and pure expression evaluation.
#[derive(Clone, Debug, PartialEq)]
pub struct CandidateLeaf {
    id: InstanceId,
    plugin: PluginId,
    config: ConfigValue,
    groups: Vec<String>,
    isolations: Vec<IsolationSpec>,
}

impl CandidateLeaf {
    /// Stable all-tree instance identity.
    pub const fn id(&self) -> &InstanceId {
        &self.id
    }

    /// Host catalog key.
    pub const fn plugin(&self) -> &PluginId {
        &self.plugin
    }

    /// Complete evaluated desired configuration.
    pub const fn config(&self) -> &ConfigValue {
        &self.config
    }

    /// Ordered enabled ancestor group IDs.
    pub fn groups(&self) -> &[String] {
        &self.groups
    }

    /// Ordered isolation declarations matching `groups` positions that isolate.
    pub fn isolations(&self) -> &[IsolationSpec] {
        &self.isolations
    }
}

/// Pure compiled candidate before factory resolution or Runtime preparation.
#[derive(Clone, Debug, PartialEq)]
pub struct ProfileCandidate {
    leaves: Vec<CandidateLeaf>,
    tree: Vec<TreeNode>,
    watch_paths: Vec<PathBuf>,
    source_fingerprints: BTreeMap<PathBuf, [u8; 32]>,
    source_digest: String,
}

impl ProfileCandidate {
    /// Enabled plugin leaves in executable tree order.
    pub fn leaves(&self) -> &[CandidateLeaf] {
        &self.leaves
    }

    /// Canonical required file sources in deterministic order.
    pub fn watch_paths(&self) -> &[PathBuf] {
        &self.watch_paths
    }

    /// SHA-256 of the complete frozen source program and environment inputs.
    pub fn source_digest(&self) -> &str {
        &self.source_digest
    }
}

/// Bounded pure compiler for one frozen Profile environment.
#[derive(Clone, Debug)]
pub struct ProfileCompiler {
    environment: ProfileEnvironment,
    limits: ProfileLimits,
}

impl ProfileCompiler {
    /// Creates a compiler without touching sources.
    pub fn new(environment: ProfileEnvironment, limits: ProfileLimits) -> Self {
        Self {
            environment,
            limits,
        }
    }

    /// Rebuilds one candidate from empty state.
    pub fn compile(&self, program: &ProfileProgram) -> Result<ProfileCandidate> {
        validate_limits(&self.limits)?;
        self.validate_environment()?;
        let mut state = CompileState::new(self);
        if let ProgramRoot::Bundle(bundle) = &program.root {
            bundle.validate(&self.limits)?;
            state.bundle = Some(bundle);
        }
        for fragment in &program.linked {
            state.hash_fragment(fragment);
            state.charge_identifier("fragment", &fragment.id)?;
            for entry in &fragment.entries {
                state.charge_step()?;
                let node = state.compile_public_entry(entry)?;
                state.execute_node(node)?;
            }
            for step in &fragment.steps {
                state.charge_step()?;
                state.execute_public_step(step)?;
            }
        }
        match &program.root {
            #[cfg(not(target_family = "wasm"))]
            ProgramRoot::File(path) => {
                state.hash_marker(b"root-file");
                state.execute_source(path, 1)?;
            }
            ProgramRoot::Bundle(bundle) => {
                state.hash_marker(b"root-bundle");
                state.execute_source(Path::new(bundle.root()), 1)?;
            }
            ProgramRoot::Memory(profile) => {
                state.hash_marker(b"root-memory");
                for entry in &profile.entries {
                    state.hash_entry(entry);
                    state.charge_step()?;
                    let node = state.compile_public_entry(entry)?;
                    state.execute_node(node)?;
                }
                for step in &profile.steps {
                    state.hash_step(step);
                    state.charge_step()?;
                    state.execute_public_step(step)?;
                }
            }
        }
        for patch in &program.launch_patches {
            state.hash_patch(patch);
            state.charge_step()?;
            state.execute_public_patch(patch)?;
        }
        state.finish()
    }

    fn validate_environment(&self) -> Result<()> {
        self.environment.validate(&self.limits)
    }
}

/// Failure at the Profile source, language, or pure preflight boundary.
#[derive(Debug, thiserror::Error)]
pub enum ProfileError {
    /// One explicit environment path is not absolute.
    #[error("{kind} path must be absolute")]
    PathNotAbsolute {
        /// Logical path role.
        kind: &'static str,
    },
    /// One frozen environment value is invalid.
    #[error("invalid Profile environment: {0}")]
    InvalidEnvironment(String),
    /// A fixed collection or byte budget was exceeded.
    #[error("{resource} exceeds the configured maximum of {maximum}")]
    CapacityExceeded {
        /// Bounded resource.
        resource: &'static str,
        /// Configured maximum.
        maximum: usize,
    },
    /// A required source could not be canonicalized, read, or decoded.
    #[error("required Profile source failed: {message}")]
    Source {
        /// Bounded redacted failure without file contents.
        message: String,
    },
    /// A canonical include is already active in the include stack.
    #[error("Profile include cycle detected")]
    IncludeCycle {
        /// Bounded canonical path for operator diagnosis.
        path: PathBuf,
    },
    /// A document declared an unsupported format.
    #[error("unsupported Profile format {format}")]
    UnsupportedFormat {
        /// Rejected format.
        format: u32,
    },
    /// TOML configuration contains a value outside JSON's data model.
    #[error("Profile TOML contains a non-JSON value at {field}")]
    NonJsonToml {
        /// Redacted structural field name.
        field: &'static str,
    },
    /// A node or patch violated the strict Profile schema.
    #[error("invalid Profile program: {0}")]
    InvalidProgram(String),
    /// An ID does not name a current candidate node.
    #[error("Profile patch target `{target}` does not exist")]
    MissingPatchTarget {
        /// Missing stable node ID.
        target: String,
    },
    /// A pure expression failed; the source and engine diagnostic are redacted.
    #[error("Profile expression for `{node}` field `{field}` failed")]
    Expression {
        /// Stable node ID.
        node: String,
        /// Expression role.
        field: &'static str,
    },
    /// Two nodes declared the same all-tree identity.
    #[error("Profile instance `{id}` appears more than once")]
    DuplicateInstance {
        /// Duplicate identity.
        id: String,
    },
    /// A leaf references no implementation in the frozen Host catalog.
    #[error("Profile references unknown plugin `{plugin}`")]
    UnknownPlugin {
        /// Missing plugin key.
        plugin: PluginId,
    },
    /// A group references no nominal Local contract in the frozen Host catalog.
    #[error("Profile references unknown Local contract `{key}`")]
    UnknownLocalContract {
        /// Missing stable key.
        key: String,
    },
    /// A group references no nominal Local event in the frozen Host catalog.
    #[error("Profile references unknown Local event `{key}`")]
    UnknownLocalEvent {
        /// Missing stable key.
        key: String,
    },
    /// Factory preparation failed; executable diagnostics are redacted.
    #[error("preparing Profile instance `{instance}` failed")]
    Preparation {
        /// Stable instance ID.
        instance: InstanceId,
    },
    /// Runtime convergence failed; plugin diagnostics are redacted.
    #[error("applying Profile instance `{instance}` failed")]
    Application {
        /// Stable instance ID.
        instance: InstanceId,
    },
    /// A static generation could not activate because one leaf remained Pending.
    #[error("Profile generation instance `{instance}` remained Pending")]
    GenerationPending {
        /// Stable instance ID.
        instance: InstanceId,
    },
    /// A desired child retired without a Profile convergence transaction.
    #[error("Profile instance `{instance}` was disposed unexpectedly")]
    UnexpectedDisposal {
        /// Stable instance ID.
        instance: InstanceId,
    },
    /// The owning Profile Fiber has retired.
    #[error("Profile control is stopped")]
    Stopped,
    /// Meta rejected a resolver-owned isolation or bounded operation.
    #[error(transparent)]
    Meta(#[from] rsi_meta::MetaError),
}

/// Profile result type.
pub type Result<T> = std::result::Result<T, ProfileError>;

#[derive(Clone, Debug, PartialEq)]
enum TreeNode {
    Group(GroupNode),
    Plugin(PluginNode),
}

enum CompiledPatch {
    Config(ConfigValue),
    Enabled(bool),
    Isolation(IsolationSpec),
    Append(Vec<TreeNode>),
}

impl TreeNode {
    fn from_entry(entry: ProfileEntry) -> Self {
        Self::Plugin(PluginNode {
            id: entry.id,
            plugin: entry.plugin,
            enabled: true,
            config: entry.config,
        })
    }

    fn id(&self) -> &str {
        match self {
            Self::Group(group) => &group.id,
            Self::Plugin(plugin) => plugin.id.as_str(),
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
struct GroupNode {
    id: String,
    enabled: bool,
    isolation: IsolationSpec,
    children: Vec<TreeNode>,
}

#[derive(Clone, Debug, PartialEq)]
struct PluginNode {
    id: InstanceId,
    plugin: PluginId,
    enabled: bool,
    config: ConfigValue,
}

struct CompileState<'a> {
    compiler: &'a ProfileCompiler,
    bundle: Option<&'a ProfileBundle>,
    seen_sources: BTreeSet<PathBuf>,
    tree: Vec<TreeNode>,
    instance_ids: HashSet<String>,
    node_count: usize,
    expression_engine: Engine,
    watch_paths: BTreeSet<PathBuf>,
    source_fingerprints: BTreeMap<PathBuf, [u8; 32]>,
    include_stack: Vec<PathBuf>,
    source_bytes: usize,
    source_files: usize,
    steps: usize,
    digest: Sha256,
}

impl<'a> CompileState<'a> {
    fn new(compiler: &'a ProfileCompiler) -> Self {
        let mut digest = Sha256::new();
        digest_component(&mut digest, b"format", b"rsi-meta-profile-source-v1");
        if let Some(paths) = &compiler.environment.paths {
            for (label, path) in [
                b"environment-config".as_slice(),
                b"environment-state",
                b"environment-cache",
            ]
            .into_iter()
            .zip(paths)
            {
                digest_component(&mut digest, label, path.as_os_str().as_encoded_bytes());
            }
        } else {
            digest_component(&mut digest, b"environment-paths", b"absent");
        }
        digest_component(
            &mut digest,
            b"environment-platform",
            compiler.environment.platform.as_bytes(),
        );
        digest_component(
            &mut digest,
            b"environment-defines",
            &serde_json::to_vec(&compiler.environment.defines)
                .expect("JSON-compatible defines always serialize"),
        );
        Self {
            compiler,
            bundle: None,
            seen_sources: BTreeSet::new(),
            tree: Vec::new(),
            instance_ids: HashSet::new(),
            node_count: 0,
            expression_engine: expression_engine(&compiler.limits),
            watch_paths: BTreeSet::new(),
            source_fingerprints: BTreeMap::new(),
            include_stack: Vec::new(),
            source_bytes: 0,
            source_files: 0,
            steps: 0,
            digest,
        }
    }

    fn hash_marker(&mut self, marker: &[u8]) {
        digest_component(&mut self.digest, b"marker", marker);
    }

    fn hash_fragment(&mut self, fragment: &ProfileFragment) {
        self.hash_marker(b"linked-fragment");
        digest_component(&mut self.digest, b"fragment-id", fragment.id.as_bytes());
        for entry in &fragment.entries {
            self.hash_entry(entry);
        }
        for step in &fragment.steps {
            self.hash_step(step);
        }
        self.hash_marker(b"linked-fragment-end");
    }

    fn hash_entry(&mut self, entry: &ProfileEntry) {
        self.hash_marker(b"plugin");
        digest_component(
            &mut self.digest,
            b"instance-id",
            entry.id.as_str().as_bytes(),
        );
        digest_component(
            &mut self.digest,
            b"plugin-id",
            entry.plugin.as_str().as_bytes(),
        );
        digest_component(
            &mut self.digest,
            b"plugin-config",
            &serde_json::to_vec(&entry.config)
                .expect("JSON-compatible Profile configs always serialize"),
        );
    }

    fn hash_step(&mut self, step: &ProfileStep) {
        match step {
            ProfileStep::Node(node) => self.hash_node(node),
            ProfileStep::Patch(patch) => self.hash_patch(patch),
        }
    }

    fn hash_node(&mut self, root: &ProfileNode) {
        enum Frame<'a> {
            Node(&'a ProfileNode),
            GroupEnd,
        }

        let mut frames = vec![Frame::Node(root)];
        while let Some(frame) = frames.pop() {
            match frame {
                Frame::Node(ProfileNode::Plugin(entry)) => self.hash_entry(entry),
                Frame::Node(ProfileNode::Group(group)) => {
                    self.hash_marker(b"group");
                    digest_component(&mut self.digest, b"group-id", group.id.as_bytes());
                    digest_component(
                        &mut self.digest,
                        b"group-enabled",
                        &[u8::from(group.enabled)],
                    );
                    self.hash_isolation(&group.isolation);
                    frames.push(Frame::GroupEnd);
                    frames.extend(group.children.iter().rev().map(Frame::Node));
                }
                Frame::GroupEnd => self.hash_marker(b"group-end"),
            }
        }
    }

    fn hash_patch(&mut self, patch: &ProfilePatch) {
        self.hash_marker(b"patch");
        match patch {
            ProfilePatch::Append { target, nodes } => {
                digest_component(&mut self.digest, b"patch-kind", b"append");
                digest_component(&mut self.digest, b"patch-target", target.as_bytes());
                for node in nodes {
                    self.hash_node(node);
                }
                self.hash_marker(b"append-end");
            }
            ProfilePatch::ReplaceConfig { target, config } => {
                digest_component(&mut self.digest, b"patch-kind", b"replace-config");
                digest_component(&mut self.digest, b"patch-target", target.as_bytes());
                digest_component(
                    &mut self.digest,
                    b"patch-config",
                    &serde_json::to_vec(config)
                        .expect("JSON-compatible Profile configs always serialize"),
                );
            }
            ProfilePatch::SetEnabled { target, enabled } => {
                digest_component(&mut self.digest, b"patch-kind", b"set-enabled");
                digest_component(&mut self.digest, b"patch-target", target.as_bytes());
                digest_component(&mut self.digest, b"patch-enabled", &[u8::from(*enabled)]);
            }
            ProfilePatch::ReplaceIsolation { target, isolation } => {
                digest_component(&mut self.digest, b"patch-kind", b"replace-isolation");
                digest_component(&mut self.digest, b"patch-target", target.as_bytes());
                self.hash_isolation(isolation);
            }
        }
        self.hash_marker(b"patch-end");
    }

    fn hash_isolation(&mut self, isolation: &IsolationSpec) {
        for (lane, values) in [
            (b"isolation-local".as_slice(), isolation.local.as_slice()),
            (b"isolation-events".as_slice(), isolation.events.as_slice()),
            (
                b"isolation-portable".as_slice(),
                isolation.portable.as_slice(),
            ),
        ] {
            digest_component(&mut self.digest, b"isolation-lane", lane);
            for value in values {
                digest_component(&mut self.digest, b"isolation-key", value.as_bytes());
            }
            self.hash_marker(b"isolation-lane-end");
        }
        for named in isolation.named.iter() {
            let lane = match named.lane {
                IsolationLane::Local => b"local".as_slice(),
                IsolationLane::Event => b"event",
                IsolationLane::Portable => b"portable",
            };
            digest_component(&mut self.digest, b"named-isolation-lane", lane);
            digest_component(
                &mut self.digest,
                b"named-isolation-key",
                named.key.as_bytes(),
            );
            digest_component(
                &mut self.digest,
                b"named-isolation-label",
                named.label.as_bytes(),
            );
        }
        self.hash_marker(b"named-isolation-end");
    }

    fn load_source(&self, requested: &Path) -> Result<(PathBuf, Arc<[u8]>)> {
        let (canonical, bytes): (PathBuf, Arc<[u8]>) = if let Some(bundle) = self.bundle {
            bundle.read(requested)?
        } else {
            #[cfg(not(target_family = "wasm"))]
            {
                let (canonical, bytes) =
                    read_profile_source(requested, self.compiler.limits.maximum_document_bytes)
                        .map_err(|error| {
                            if error.kind() == std::io::ErrorKind::InvalidData {
                                ProfileError::CapacityExceeded {
                                    resource: "document bytes",
                                    maximum: self.compiler.limits.maximum_document_bytes,
                                }
                            } else {
                                ProfileError::Source {
                                    message: bound_message(
                                        format!("cannot read required source: {error}"),
                                        self.compiler.limits.maximum_identifier_bytes,
                                    ),
                                }
                            }
                        })?;
                (canonical, bytes.into())
            }
            #[cfg(target_family = "wasm")]
            return Err(ProfileError::Source {
                message: "native Profile sources are unavailable".into(),
            });
        };
        Ok((canonical, bytes))
    }

    fn execute_source(&mut self, requested: &Path, depth: usize) -> Result<()> {
        if depth > self.compiler.limits.maximum_include_depth {
            return Err(ProfileError::CapacityExceeded {
                resource: "include depth",
                maximum: self.compiler.limits.maximum_include_depth,
            });
        }
        let (canonical, bytes) = self.load_source(requested)?;
        if self.include_stack.contains(&canonical) {
            return Err(ProfileError::IncludeCycle { path: canonical });
        }
        if self.seen_sources.insert(canonical.clone()) {
            self.source_files =
                self.source_files
                    .checked_add(1)
                    .ok_or(ProfileError::CapacityExceeded {
                        resource: "source files",
                        maximum: self.compiler.limits.maximum_source_files,
                    })?;
            if self.source_files > self.compiler.limits.maximum_source_files {
                return Err(ProfileError::CapacityExceeded {
                    resource: "source files",
                    maximum: self.compiler.limits.maximum_source_files,
                });
            }
        }
        self.source_bytes =
            self.source_bytes
                .checked_add(bytes.len())
                .ok_or(ProfileError::CapacityExceeded {
                    resource: "source bytes",
                    maximum: self.compiler.limits.maximum_source_bytes,
                })?;
        if self.source_bytes > self.compiler.limits.maximum_source_bytes {
            return Err(ProfileError::CapacityExceeded {
                resource: "source bytes",
                maximum: self.compiler.limits.maximum_source_bytes,
            });
        }
        let source = std::str::from_utf8(&bytes).map_err(|_| ProfileError::Source {
            message: "required source is not UTF-8".to_owned(),
        })?;
        let document = toml::from_str::<RawDocument>(source).map_err(|_| ProfileError::Source {
            message: "invalid Profile TOML document".to_owned(),
        })?;
        if document.format != PROFILE_FORMAT {
            return Err(ProfileError::UnsupportedFormat {
                format: document.format,
            });
        }
        digest_component(
            &mut self.digest,
            b"source-path",
            canonical.as_os_str().as_encoded_bytes(),
        );
        digest_component(&mut self.digest, b"source-bytes", &bytes);
        if self.bundle.is_none() {
            let fingerprint: [u8; 32] = Sha256::digest(&bytes).into();
            if let Some(previous) = self
                .source_fingerprints
                .insert(canonical.clone(), fingerprint)
                && previous != fingerprint
            {
                return Err(ProfileError::Source {
                    message: "a required source changed during Profile rebuild".to_owned(),
                });
            }
            self.watch_paths.insert(canonical.clone());
        }
        self.include_stack.push(canonical.clone());
        let base = canonical.parent().ok_or_else(|| ProfileError::Source {
            message: "required source has no parent directory".to_owned(),
        })?;
        for step in document.steps {
            self.charge_step()?;
            self.execute_step(step, base, depth)?;
        }
        let popped = self.include_stack.pop();
        debug_assert_eq!(popped.as_deref(), Some(canonical.as_path()));
        Ok(())
    }

    fn execute_step(&mut self, step: RawStep, base: &Path, depth: usize) -> Result<()> {
        match step {
            RawStep::Include { path } => {
                if self.bundle.is_some() {
                    let requested = bundle::resolve_include(base, &path)?;
                    return self.execute_source(&requested, depth + 1);
                }
                let path = PathBuf::from(path);
                let path = if path.is_absolute() {
                    path
                } else {
                    base.join(path)
                };
                self.execute_source(&path, depth + 1)
            }
            RawStep::Group(raw) => {
                let node = self.compile_group(raw)?;
                self.execute_node(TreeNode::Group(node))
            }
            RawStep::Plugin(raw) => {
                let node = self.compile_plugin(raw)?;
                self.execute_node(TreeNode::Plugin(node))
            }
            RawStep::Patch(raw) => self.execute_patch(raw),
        }
    }

    fn execute_public_step(&mut self, step: &ProfileStep) -> Result<()> {
        match step {
            ProfileStep::Node(node) => {
                let node = self.compile_public_node(node)?;
                self.execute_node(node)
            }
            ProfileStep::Patch(patch) => self.execute_public_patch(patch),
        }
    }

    fn compile_public_entry(&self, entry: &ProfileEntry) -> Result<TreeNode> {
        self.validate_identifier("instance", entry.id.as_str())?;
        self.validate_identifier("plugin", entry.plugin.as_str())?;
        bounded_json_bytes(&entry.config, self.compiler.limits.maximum_config_bytes)?;
        Ok(TreeNode::from_entry(entry.clone()))
    }

    fn compile_public_node(&self, node: &ProfileNode) -> Result<TreeNode> {
        enum Frame<'a> {
            Visit(&'a ProfileNode, usize),
            Finish {
                id: &'a str,
                enabled: bool,
                isolation: &'a IsolationSpec,
                children: usize,
            },
        }

        let mut frames = vec![Frame::Visit(node, 1)];
        let mut compiled = Vec::new();
        let mut failure = None;
        while let Some(frame) = frames.pop() {
            match frame {
                Frame::Visit(ProfileNode::Plugin(entry), _) => {
                    if failure.is_none() {
                        match self.compile_public_entry(entry) {
                            Ok(node) => compiled.push(node),
                            Err(error) => failure = Some(error),
                        }
                    }
                }
                Frame::Visit(ProfileNode::Group(group), depth) => {
                    if failure.is_none() {
                        let result = (|| {
                            if depth > self.compiler.limits.maximum_group_depth {
                                return Err(ProfileError::CapacityExceeded {
                                    resource: "group depth",
                                    maximum: self.compiler.limits.maximum_group_depth,
                                });
                            }
                            self.validate_identifier("group", &group.id)?;
                            validate_isolation(&group.isolation, self)
                        })();
                        if let Err(error) = result {
                            failure = Some(error);
                        } else {
                            frames.push(Frame::Finish {
                                id: &group.id,
                                enabled: group.enabled,
                                isolation: &group.isolation,
                                children: group.children.len(),
                            });
                        }
                    }
                    frames.extend(
                        group
                            .children
                            .iter()
                            .rev()
                            .map(|child| Frame::Visit(child, depth.saturating_add(1))),
                    );
                }
                Frame::Finish {
                    id,
                    enabled,
                    isolation,
                    children,
                } => {
                    if failure.is_none() {
                        let start = compiled
                            .len()
                            .checked_sub(children)
                            .expect("compiled child count matches group frame");
                        let children = compiled.split_off(start);
                        compiled.push(TreeNode::Group(GroupNode {
                            id: id.to_owned(),
                            enabled,
                            isolation: isolation.clone(),
                            children,
                        }));
                    }
                }
            }
        }
        if let Some(error) = failure {
            return Err(error);
        }
        compiled.pop().ok_or_else(|| {
            ProfileError::InvalidProgram("Profile node compilation produced no node".to_owned())
        })
    }

    fn execute_public_patch(&mut self, patch: &ProfilePatch) -> Result<()> {
        let (target, operation) = match patch {
            ProfilePatch::Append { target, nodes } => (
                target.as_str(),
                CompiledPatch::Append(
                    nodes
                        .iter()
                        .map(|node| self.compile_public_node(node))
                        .collect::<Result<Vec<_>>>()?,
                ),
            ),
            ProfilePatch::ReplaceConfig { target, config } => {
                bounded_json_bytes(config, self.compiler.limits.maximum_config_bytes)?;
                (target.as_str(), CompiledPatch::Config(config.clone()))
            }
            ProfilePatch::SetEnabled { target, enabled } => {
                (target.as_str(), CompiledPatch::Enabled(*enabled))
            }
            ProfilePatch::ReplaceIsolation { target, isolation } => {
                validate_isolation(isolation, self)?;
                (target.as_str(), CompiledPatch::Isolation(isolation.clone()))
            }
        };
        self.apply_compiled_patch(target, operation)
    }

    fn compile_group(&self, raw: RawGroup) -> Result<GroupNode> {
        enum Frame {
            Visit(RawNode, usize),
            Finish {
                id: String,
                enabled: bool,
                isolation: IsolationSpec,
                children: usize,
            },
        }

        let mut frames = vec![Frame::Visit(RawNode::Group(raw), 1)];
        let mut compiled = Vec::new();
        let mut failure = None;
        while let Some(frame) = frames.pop() {
            match frame {
                Frame::Visit(RawNode::Plugin(plugin), _) => {
                    if failure.is_none() {
                        match self.compile_plugin(plugin) {
                            Ok(plugin) => compiled.push(TreeNode::Plugin(plugin)),
                            Err(error) => failure = Some(error),
                        }
                    }
                }
                Frame::Visit(RawNode::Group(group), depth) => {
                    let RawGroup {
                        id,
                        enabled,
                        enabled_rhai,
                        isolation,
                        nodes,
                    } = group;
                    if failure.is_none() {
                        let result = (|| {
                            if depth > self.compiler.limits.maximum_group_depth {
                                return Err(ProfileError::CapacityExceeded {
                                    resource: "group depth",
                                    maximum: self.compiler.limits.maximum_group_depth,
                                });
                            }
                            self.validate_identifier("group", &id)?;
                            let enabled =
                                self.evaluate_enabled(&id, enabled, enabled_rhai.as_deref())?;
                            let isolation = isolation.unwrap_or_default().validate(self)?;
                            Ok((enabled, isolation))
                        })();
                        match result {
                            Ok((enabled, isolation)) => frames.push(Frame::Finish {
                                id,
                                enabled,
                                isolation,
                                children: nodes.len(),
                            }),
                            Err(error) => failure = Some(error),
                        }
                    }
                    frames.extend(
                        nodes
                            .into_iter()
                            .rev()
                            .map(|node| Frame::Visit(node, depth.saturating_add(1))),
                    );
                }
                Frame::Finish {
                    id,
                    enabled,
                    isolation,
                    children,
                } => {
                    if failure.is_none() {
                        let start = compiled
                            .len()
                            .checked_sub(children)
                            .expect("compiled child count matches group frame");
                        let children = compiled.split_off(start);
                        compiled.push(TreeNode::Group(GroupNode {
                            id,
                            enabled,
                            isolation,
                            children,
                        }));
                    }
                }
            }
        }
        match (failure, compiled.pop()) {
            (Some(error), _) => Err(error),
            (None, Some(TreeNode::Group(group))) => Ok(group),
            (None, Some(TreeNode::Plugin(_)) | None) => Err(ProfileError::InvalidProgram(
                "Profile group compilation produced no group".to_owned(),
            )),
        }
    }

    fn compile_plugin(&self, raw: RawPlugin) -> Result<PluginNode> {
        self.validate_identifier("instance", &raw.id)?;
        self.validate_identifier("plugin", &raw.plugin)?;
        let enabled = self.evaluate_enabled(&raw.id, raw.enabled, raw.enabled_rhai.as_deref())?;
        if raw.config.is_some() && raw.config_rhai.is_some() {
            return Err(ProfileError::InvalidProgram(format!(
                "plugin `{}` sets both config and config_rhai",
                raw.id
            )));
        }
        let config = match (raw.config, raw.config_rhai.as_deref()) {
            (Some(value), None) => toml_to_json(value, "config")?,
            (None, Some(expression)) => self.evaluate_config(&raw.id, expression)?,
            (None, None) => Value::Null,
            (Some(_), Some(_)) => unreachable!("mutual exclusion checked above"),
        };
        bounded_json_bytes(&config, self.compiler.limits.maximum_config_bytes)?;
        Ok(PluginNode {
            id: raw.id.into(),
            plugin: raw.plugin.into(),
            enabled,
            config,
        })
    }

    fn execute_node(&mut self, node: TreeNode) -> Result<()> {
        self.register_nodes(std::slice::from_ref(&node))?;
        self.tree.push(node);
        Ok(())
    }

    fn execute_patch(&mut self, raw: RawPatch) -> Result<()> {
        let RawPatch {
            target,
            config,
            config_rhai,
            enabled,
            enabled_rhai,
            isolation,
            append,
        } = raw;
        self.validate_identifier("patch target", &target)?;
        if config.is_some() && config_rhai.is_some() {
            return Err(ProfileError::InvalidProgram(format!(
                "patch `{target}` sets both config and config_rhai"
            )));
        }
        if enabled.is_some() && enabled_rhai.is_some() {
            return Err(ProfileError::InvalidProgram(format!(
                "patch `{target}` sets both enabled and enabled_rhai"
            )));
        }
        let operation_count = usize::from(config.is_some() || config_rhai.is_some())
            + usize::from(enabled.is_some() || enabled_rhai.is_some())
            + usize::from(isolation.is_some())
            + usize::from(append.is_some());
        if operation_count != 1 {
            return Err(ProfileError::InvalidProgram(format!(
                "patch `{target}` must declare exactly one operation"
            )));
        }
        let config = match (config, config_rhai.as_deref()) {
            (Some(config), None) => Some(toml_to_json(config, "patch config")?),
            (None, Some(expression)) => Some(self.evaluate_config(&target, expression)?),
            (None, None) => None,
            (Some(_), Some(_)) => unreachable!("mutual exclusion checked above"),
        };
        if let Some(config) = &config {
            bounded_json_bytes(config, self.compiler.limits.maximum_config_bytes)?;
        }
        let isolation = isolation.map(|value| value.validate(self)).transpose()?;
        let append = append
            .map(|nodes| {
                nodes
                    .into_iter()
                    .map(|node| match node {
                        RawNode::Group(group) => self.compile_group(group).map(TreeNode::Group),
                        RawNode::Plugin(plugin) => {
                            self.compile_plugin(plugin).map(TreeNode::Plugin)
                        }
                    })
                    .collect::<Result<Vec<_>>>()
            })
            .transpose()?;
        let enabled = match (enabled, enabled_rhai.as_deref()) {
            (Some(enabled), None) => Some(enabled),
            (None, Some(expression)) => {
                Some(self.evaluate::<bool>(&target, "enabled", expression)?)
            }
            (None, None) => None,
            (Some(_), Some(_)) => unreachable!("mutual exclusion checked above"),
        };
        let operation = if let Some(config) = config {
            CompiledPatch::Config(config)
        } else if let Some(enabled) = enabled {
            CompiledPatch::Enabled(enabled)
        } else if let Some(isolation) = isolation {
            CompiledPatch::Isolation(isolation)
        } else {
            CompiledPatch::Append(append.expect("one patch operation was validated"))
        };
        self.apply_compiled_patch(&target, operation)
    }

    fn apply_compiled_patch(&mut self, target: &str, operation: CompiledPatch) -> Result<()> {
        self.validate_identifier("patch target", target)?;
        if let CompiledPatch::Append(nodes) = &operation {
            match find_node(&self.tree, target) {
                Some(TreeNode::Group(_)) => self.register_nodes(nodes)?,
                Some(TreeNode::Plugin(_)) => {
                    return Err(ProfileError::InvalidProgram(format!(
                        "plugin patch `{target}` has a group-only operation"
                    )));
                }
                None => {
                    return Err(ProfileError::MissingPatchTarget {
                        target: target.to_owned(),
                    });
                }
            }
        }
        let node = find_node_mut(&mut self.tree, target).ok_or_else(|| {
            ProfileError::MissingPatchTarget {
                target: target.to_owned(),
            }
        })?;
        match node {
            TreeNode::Plugin(plugin) => match operation {
                CompiledPatch::Config(config) => plugin.config = config,
                CompiledPatch::Enabled(enabled) => plugin.enabled = enabled,
                CompiledPatch::Isolation(_) | CompiledPatch::Append(_) => {
                    return Err(ProfileError::InvalidProgram(format!(
                        "plugin patch `{target}` has a group-only operation"
                    )));
                }
            },
            TreeNode::Group(group) => match operation {
                CompiledPatch::Config(_) => {
                    return Err(ProfileError::InvalidProgram(format!(
                        "group patch `{target}` cannot replace config"
                    )));
                }
                CompiledPatch::Enabled(enabled) => group.enabled = enabled,
                CompiledPatch::Isolation(isolation) => group.isolation = isolation,
                CompiledPatch::Append(nodes) => group.children.extend(nodes),
            },
        }
        Ok(())
    }

    fn evaluate_enabled(
        &self,
        id: &str,
        literal: Option<bool>,
        expression: Option<&str>,
    ) -> Result<bool> {
        if literal.is_some() && expression.is_some() {
            return Err(ProfileError::InvalidProgram(format!(
                "node `{id}` sets both enabled and enabled_rhai"
            )));
        }
        match (literal, expression) {
            (Some(value), None) => Ok(value),
            (None, Some(expression)) => self.evaluate::<bool>(id, "enabled", expression),
            (None, None) => Ok(true),
            (Some(_), Some(_)) => unreachable!("mutual exclusion checked above"),
        }
    }

    fn evaluate_config(&self, id: &str, expression: &str) -> Result<Value> {
        let dynamic = self.evaluate::<Dynamic>(id, "config", expression)?;
        rhai::serde::from_dynamic(&dynamic).map_err(|_| ProfileError::Expression {
            node: id.to_owned(),
            field: "config",
        })
    }

    fn evaluate<T: Clone + Send + Sync + 'static>(
        &self,
        id: &str,
        field: &'static str,
        source: &str,
    ) -> Result<T> {
        if source.len() > self.compiler.limits.maximum_document_bytes {
            return Err(ProfileError::CapacityExceeded {
                resource: "expression bytes",
                maximum: self.compiler.limits.maximum_document_bytes,
            });
        }
        let mut scope = Scope::new();
        let mut paths = Map::new();
        if let Some(values) = &self.compiler.environment.paths {
            for (label, path) in ["config", "state", "cache"].into_iter().zip(values) {
                paths.insert(label.into(), path.display().to_string().into());
            }
        }
        scope.push("paths", paths);
        scope.push("platform", self.compiler.environment.platform.clone());
        scope.push(
            "defines",
            json_object_to_map(&self.compiler.environment.defines)?,
        );
        self.expression_engine
            .eval_expression_with_scope::<T>(&mut scope, source)
            .map_err(|_| ProfileError::Expression {
                node: id.to_owned(),
                field,
            })
    }

    fn validate_identifier(&self, kind: &'static str, value: &str) -> Result<()> {
        if value.is_empty() || value.len() > self.compiler.limits.maximum_identifier_bytes {
            Err(ProfileError::InvalidProgram(format!(
                "{kind} identifier must contain 1..={} bytes",
                self.compiler.limits.maximum_identifier_bytes
            )))
        } else {
            Ok(())
        }
    }

    fn charge_identifier(&self, kind: &'static str, value: &str) -> Result<()> {
        self.validate_identifier(kind, value)
    }

    fn charge_step(&mut self) -> Result<()> {
        self.steps = self
            .steps
            .checked_add(1)
            .ok_or(ProfileError::CapacityExceeded {
                resource: "Profile steps",
                maximum: self.compiler.limits.maximum_steps,
            })?;
        if self.steps > self.compiler.limits.maximum_steps {
            return Err(ProfileError::CapacityExceeded {
                resource: "Profile steps",
                maximum: self.compiler.limits.maximum_steps,
            });
        }
        Ok(())
    }

    fn register_nodes(&mut self, nodes: &[TreeNode]) -> Result<()> {
        let additional = total_nodes(nodes);
        let projected =
            self.node_count
                .checked_add(additional)
                .ok_or(ProfileError::CapacityExceeded {
                    resource: "Profile nodes",
                    maximum: self.compiler.limits.maximum_nodes,
                })?;
        if projected > self.compiler.limits.maximum_nodes {
            return Err(ProfileError::CapacityExceeded {
                resource: "Profile nodes",
                maximum: self.compiler.limits.maximum_nodes,
            });
        }
        visit_nodes(nodes, &mut |node| {
            if self.instance_ids.insert(node.id().to_owned()) {
                Ok(())
            } else {
                Err(ProfileError::DuplicateInstance {
                    id: node.id().to_owned(),
                })
            }
        })?;
        self.node_count = projected;
        Ok(())
    }

    fn finish(self) -> Result<ProfileCandidate> {
        let mut retained = 0_usize;
        let mut isolation_bindings = 0_usize;
        visit_nodes(&self.tree, &mut |node| {
            if let TreeNode::Group(group) = node {
                isolation_bindings = isolation_bindings.saturating_add(group.isolation.len());
                if isolation_bindings > self.compiler.limits.maximum_isolation_bindings {
                    return Err(ProfileError::CapacityExceeded {
                        resource: "isolation bindings",
                        maximum: self.compiler.limits.maximum_isolation_bindings,
                    });
                }
            }
            let TreeNode::Plugin(plugin) = node else {
                return Ok(());
            };
            let bytes = serde_json::to_vec(&plugin.config)
                .expect("JSON-compatible Profile configs always serialize")
                .len();
            retained = retained
                .checked_add(bytes)
                .ok_or(ProfileError::CapacityExceeded {
                    resource: "resolved config bytes",
                    maximum: self.compiler.limits.maximum_config_bytes,
                })?;
            Ok(())
        })?;
        if retained > self.compiler.limits.maximum_config_bytes {
            return Err(ProfileError::CapacityExceeded {
                resource: "resolved config bytes",
                maximum: self.compiler.limits.maximum_config_bytes,
            });
        }
        let mut leaves = Vec::new();
        flatten_nodes(
            &self.tree,
            true,
            &mut Vec::new(),
            &mut Vec::new(),
            &mut leaves,
        );
        let source_digest = hex_lower(self.digest.finalize().as_slice());
        Ok(ProfileCandidate {
            leaves,
            tree: self.tree,
            watch_paths: self.watch_paths.into_iter().collect(),
            source_fingerprints: self.source_fingerprints,
            source_digest,
        })
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawDocument {
    format: u32,
    #[serde(default)]
    steps: Vec<RawStep>,
}

#[derive(Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum RawStep {
    Include { path: String },
    Group(RawGroup),
    Plugin(RawPlugin),
    Patch(RawPatch),
}

#[derive(Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum RawNode {
    Group(RawGroup),
    Plugin(RawPlugin),
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawGroup {
    id: String,
    enabled: Option<bool>,
    enabled_rhai: Option<String>,
    isolation: Option<RawIsolation>,
    #[serde(default)]
    nodes: Vec<RawNode>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawPlugin {
    id: String,
    plugin: String,
    enabled: Option<bool>,
    enabled_rhai: Option<String>,
    config: Option<toml::Value>,
    config_rhai: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawPatch {
    target: String,
    config: Option<toml::Value>,
    config_rhai: Option<String>,
    enabled: Option<bool>,
    enabled_rhai: Option<String>,
    isolation: Option<RawIsolation>,
    append: Option<Vec<RawNode>>,
}

#[derive(Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawIsolation {
    #[serde(default)]
    local: Vec<RawIsolationValue>,
    #[serde(default)]
    events: Vec<RawIsolationValue>,
    #[serde(default)]
    portable: Vec<RawIsolationValue>,
}

#[derive(Clone, Deserialize)]
#[serde(untagged)]
enum RawIsolationValue {
    Fresh(String),
    Named(RawNamedIsolation),
}
#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawNamedIsolation {
    key: String,
    label: String,
}

impl RawIsolation {
    fn validate(self, state: &CompileState<'_>) -> Result<IsolationSpec> {
        let mut isolation = IsolationSpec::default();
        for (lane, values) in [
            (IsolationLane::Local, self.local),
            (IsolationLane::Event, self.events),
            (IsolationLane::Portable, self.portable),
        ] {
            for value in values {
                match value {
                    RawIsolationValue::Fresh(key) => match lane {
                        IsolationLane::Local => Arc::make_mut(&mut isolation.local).push(key),
                        IsolationLane::Event => Arc::make_mut(&mut isolation.events).push(key),
                        IsolationLane::Portable => Arc::make_mut(&mut isolation.portable).push(key),
                    },
                    RawIsolationValue::Named(value) => {
                        Arc::make_mut(&mut isolation.named).push(NamedIsolation {
                            lane,
                            key: value.key,
                            label: value.label,
                        });
                    }
                }
            }
        }
        validate_isolation(&isolation, state)?;
        Ok(isolation)
    }
}

fn validate_isolation(isolation: &IsolationSpec, state: &CompileState<'_>) -> Result<()> {
    if isolation.len() > state.compiler.limits.maximum_isolation_bindings {
        return Err(ProfileError::CapacityExceeded {
            resource: "isolation bindings",
            maximum: state.compiler.limits.maximum_isolation_bindings,
        });
    }
    for value in isolation
        .local
        .iter()
        .chain(isolation.events.iter())
        .chain(isolation.portable.iter())
    {
        state.validate_identifier("isolation", value)?;
    }
    if has_duplicates(&isolation.local)
        || has_duplicates(&isolation.events)
        || has_duplicates(&isolation.portable)
    {
        return Err(ProfileError::InvalidProgram(
            "group isolation keys must be unique within each lane".to_owned(),
        ));
    }
    let mut keys = BTreeSet::new();
    for (lane, values) in [
        (IsolationLane::Local, &isolation.local),
        (IsolationLane::Event, &isolation.events),
        (IsolationLane::Portable, &isolation.portable),
    ] {
        for key in values.iter() {
            keys.insert((lane, key.as_str()));
        }
    }
    for named in isolation.named.iter() {
        state.validate_identifier("isolation", &named.key)?;
        state.validate_identifier("isolation label", &named.label)?;
        if !keys.insert((named.lane, named.key.as_str())) {
            return Err(ProfileError::InvalidProgram(
                "group isolation keys must be unique within each lane".into(),
            ));
        }
    }
    Ok(())
}

fn flatten_nodes(
    nodes: &[TreeNode],
    inherited_enabled: bool,
    groups: &mut Vec<String>,
    isolations: &mut Vec<IsolationSpec>,
    leaves: &mut Vec<CandidateLeaf>,
) {
    for node in nodes {
        match node {
            TreeNode::Group(group) => {
                let enabled = inherited_enabled && group.enabled;
                groups.push(group.id.clone());
                isolations.push(group.isolation.clone());
                flatten_nodes(&group.children, enabled, groups, isolations, leaves);
                isolations.pop();
                groups.pop();
            }
            TreeNode::Plugin(plugin) if inherited_enabled && plugin.enabled => {
                leaves.push(CandidateLeaf {
                    id: plugin.id.clone(),
                    plugin: plugin.plugin.clone(),
                    config: plugin.config.clone(),
                    groups: groups.clone(),
                    isolations: isolations.clone(),
                });
            }
            TreeNode::Plugin(_) => {}
        }
    }
}

fn find_node_mut<'a>(nodes: &'a mut [TreeNode], target: &str) -> Option<&'a mut TreeNode> {
    for node in nodes {
        if node.id() == target {
            return Some(node);
        }
        if let TreeNode::Group(group) = node
            && let Some(found) = find_node_mut(&mut group.children, target)
        {
            return Some(found);
        }
    }
    None
}

fn find_node<'a>(nodes: &'a [TreeNode], target: &str) -> Option<&'a TreeNode> {
    for node in nodes {
        if node.id() == target {
            return Some(node);
        }
        if let TreeNode::Group(group) = node
            && let Some(found) = find_node(&group.children, target)
        {
            return Some(found);
        }
    }
    None
}

fn expression_engine(limits: &ProfileLimits) -> Engine {
    let mut engine = Engine::new_raw();
    LanguageCorePackage::new().register_into_engine(&mut engine);
    ArithmeticPackage::new().register_into_engine(&mut engine);
    BasicStringPackage::new().register_into_engine(&mut engine);
    BasicIteratorPackage::new().register_into_engine(&mut engine);
    LogicPackage::new().register_into_engine(&mut engine);
    BasicMathPackage::new().register_into_engine(&mut engine);
    BasicArrayPackage::new().register_into_engine(&mut engine);
    BasicMapPackage::new().register_into_engine(&mut engine);
    MoreStringPackage::new().register_into_engine(&mut engine);
    let total_operations = Arc::new(AtomicU64::new(0));
    let maximum_operations = limits.maximum_expression_operations;
    engine.on_progress(move |_| {
        let admitted = total_operations
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |operations| {
                operations.checked_add(1)
            })
            .is_ok_and(|previous| previous < maximum_operations);
        (!admitted).then_some(Dynamic::UNIT)
    });
    engine.disable_symbol("print");
    engine.disable_symbol("debug");
    engine.set_max_operations(limits.maximum_expression_operations);
    engine.set_max_expr_depths(
        limits.maximum_expression_depth,
        limits.maximum_expression_depth,
    );
    engine.set_max_call_levels(limits.maximum_expression_depth);
    engine.set_max_string_size(limits.maximum_config_bytes);
    engine.set_max_array_size(limits.maximum_nodes);
    engine.set_max_map_size(limits.maximum_nodes);
    engine
}

fn visit_nodes(
    nodes: &[TreeNode],
    visitor: &mut impl FnMut(&TreeNode) -> Result<()>,
) -> Result<()> {
    for node in nodes {
        visitor(node)?;
        if let TreeNode::Group(group) = node {
            visit_nodes(&group.children, visitor)?;
        }
    }
    Ok(())
}

fn total_nodes(nodes: &[TreeNode]) -> usize {
    nodes.iter().map(count_nodes).sum()
}

fn count_nodes(node: &TreeNode) -> usize {
    match node {
        TreeNode::Plugin(_) => 1,
        TreeNode::Group(group) => 1 + total_nodes(&group.children),
    }
}

fn has_duplicates(values: &[String]) -> bool {
    let mut unique = HashSet::new();
    values.iter().any(|value| !unique.insert(value))
}

fn toml_to_json(value: toml::Value, field: &'static str) -> Result<Value> {
    if contains_datetime(&value) {
        return Err(ProfileError::NonJsonToml { field });
    }
    serde_json::to_value(value).map_err(|_| ProfileError::NonJsonToml { field })
}

fn contains_datetime(value: &toml::Value) -> bool {
    match value {
        toml::Value::Datetime(_) => true,
        toml::Value::Array(values) => values.iter().any(contains_datetime),
        toml::Value::Table(values) => values.values().any(contains_datetime),
        _ => false,
    }
}

fn bounded_json_bytes(value: &Value, maximum: usize) -> Result<()> {
    validate_json_depth(value)?;
    let bytes = serde_json::to_vec(value)
        .expect("serde_json::Value always serializes")
        .len();
    if bytes > maximum {
        Err(ProfileError::CapacityExceeded {
            resource: "config bytes",
            maximum,
        })
    } else {
        Ok(())
    }
}

fn validate_json_depth(value: &Value) -> Result<()> {
    let mut stack = vec![(value, 1_usize)];
    while let Some((value, depth)) = stack.pop() {
        if depth > rsi_meta::MAXIMUM_JSON_DEPTH {
            return Err(ProfileError::InvalidProgram(format!(
                "configuration exceeds the maximum JSON depth of {}",
                rsi_meta::MAXIMUM_JSON_DEPTH
            )));
        }
        match value {
            Value::Array(values) => {
                stack.extend(values.iter().map(|value| (value, depth + 1)));
            }
            Value::Object(values) => {
                stack.extend(values.values().map(|value| (value, depth + 1)));
            }
            _ => {}
        }
    }
    Ok(())
}

fn validate_define_numbers(value: &Value) -> Result<()> {
    let mut stack = vec![value];
    while let Some(value) = stack.pop() {
        match value {
            Value::Number(number) if number.as_i64().is_none() => {
                return Err(ProfileError::InvalidEnvironment(
                    "Rhai defines accept only signed 64-bit integers as JSON numbers".to_owned(),
                ));
            }
            Value::Array(values) => stack.extend(values),
            Value::Object(values) => stack.extend(values.values()),
            Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => {}
        }
    }
    Ok(())
}

fn json_object_to_map(values: &BTreeMap<String, Value>) -> Result<Map> {
    values
        .iter()
        .map(|(key, value)| (key.as_str().into(), json_to_dynamic(value)))
        .map(|(key, value)| value.map(|value| (key, value)))
        .collect()
}

fn json_to_dynamic(value: &Value) -> Result<Dynamic> {
    Ok(match value {
        Value::Null => Dynamic::UNIT,
        Value::Bool(value) => (*value).into(),
        Value::Number(value) => Dynamic::from_int(value.as_i64().ok_or_else(|| {
            ProfileError::InvalidEnvironment(
                "Rhai defines accept only signed 64-bit integers as JSON numbers".to_owned(),
            )
        })?),
        Value::String(value) => value.clone().into(),
        Value::Array(values) => values
            .iter()
            .map(json_to_dynamic)
            .collect::<Result<Vec<_>>>()?
            .into(),
        Value::Object(values) => values
            .iter()
            .map(|(key, value)| (key.as_str().into(), json_to_dynamic(value)))
            .map(|(key, value)| value.map(|value| (key, value)))
            .collect::<Result<Map>>()?
            .into(),
    })
}

fn absolute_path(kind: &'static str, path: PathBuf) -> Result<PathBuf> {
    if path.is_absolute() {
        Ok(path)
    } else {
        Err(ProfileError::PathNotAbsolute { kind })
    }
}

fn validate_limits(limits: &ProfileLimits) -> Result<()> {
    for (resource, value) in [
        ("document bytes", limits.maximum_document_bytes),
        ("source bytes", limits.maximum_source_bytes),
        ("source files", limits.maximum_source_files),
        ("include depth", limits.maximum_include_depth),
        ("Profile steps", limits.maximum_steps),
        ("Profile nodes", limits.maximum_nodes),
        ("group depth", limits.maximum_group_depth),
        ("isolation bindings", limits.maximum_isolation_bindings),
        ("identifier bytes", limits.maximum_identifier_bytes),
        (
            "expression operations",
            usize::try_from(limits.maximum_expression_operations).unwrap_or(usize::MAX),
        ),
        ("expression depth", limits.maximum_expression_depth),
        ("config bytes", limits.maximum_config_bytes),
        ("diagnostic bytes", limits.maximum_diagnostic_bytes),
    ] {
        if value == 0 {
            return Err(ProfileError::CapacityExceeded {
                resource,
                maximum: 0,
            });
        }
    }
    if limits.maximum_group_depth > 128 {
        return Err(ProfileError::CapacityExceeded {
            resource: "group depth",
            maximum: 128,
        });
    }
    Ok(())
}

fn bound_message(mut message: String, maximum: usize) -> String {
    if message.len() > maximum {
        let mut end = maximum;
        while !message.is_char_boundary(end) {
            end -= 1;
        }
        message.truncate(end);
    }
    message
}

fn hex_lower(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        output.push(char::from(HEX[usize::from(byte >> 4)]));
        output.push(char::from(HEX[usize::from(byte & 0x0f)]));
    }
    output
}

fn digest_component(digest: &mut Sha256, tag: &[u8], bytes: &[u8]) {
    digest.update(u64::try_from(tag.len()).unwrap_or(u64::MAX).to_le_bytes());
    digest.update(tag);
    digest.update(u64::try_from(bytes.len()).unwrap_or(u64::MAX).to_le_bytes());
    digest.update(bytes);
}

impl fmt::Display for ProfileCandidate {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "Profile candidate {} with {} enabled leaves",
            self.source_digest,
            self.leaves.len()
        )
    }
}
