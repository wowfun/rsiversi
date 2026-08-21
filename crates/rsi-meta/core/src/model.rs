use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use rsi_meta_loader::ContentHash;
use serde::{Deserialize, Serialize};

pub const COMPOSITION_FORMAT_VERSION: u32 = 0;
pub const LOCK_FORMAT_VERSION: u32 = 0;
const MAX_IDENTITY_BYTES: usize = 255;
pub const MAX_COMPOSITION_SCOPES: usize = 1_024;
pub const MAX_SCOPE_DEPTH: usize = 64;
pub const MAX_COMPOSITION_INSTANCES: usize = 1_024;
pub const MAX_COMPOSITION_BINDINGS: usize = 16_384;
pub const MAX_COMPOSITION_REQUIREMENTS: usize = 65_536;

/// Package identity declared by a validated plugin manifest.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(transparent)]
pub struct PackageId(pub String);

impl PackageId {
    /// Wraps a package identity; composition validation enforces its grammar.
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }
}

impl fmt::Display for PackageId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

/// Stable identity of one mount. It is distinct from package identity.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(transparent)]
pub struct InstanceId(pub String);

impl InstanceId {
    /// Wraps a mount identity; composition validation enforces its grammar.
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }
}

impl fmt::Display for InstanceId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

/// Identity of one lexical scope in a composition.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ScopeId(pub String);

impl ScopeId {
    /// Wraps a scope identity; composition validation enforces its grammar.
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }
}

impl fmt::Display for ScopeId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

/// Canonical contract name. There is deliberately no per-contract version.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ServiceKey(pub String);

impl ServiceKey {
    /// Wraps a service name; package and composition validation enforce its grammar.
    pub fn new(name: impl Into<String>) -> Self {
        Self(name.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for ServiceKey {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

/// Monotonic revision of an atomically published composition graph.
#[derive(
    Clone, Copy, Debug, Default, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize,
)]
#[serde(transparent)]
pub struct GraphRevision(pub u64);

/// Composition policy mode affecting validation and runtime trust decisions.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CompositionMode {
    /// Development defaults with local iteration allowances.
    #[default]
    Development,
    /// Production policy with deployment-oriented restrictions.
    Production,
}

/// Identity and policy metadata for a composition manifest.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CompositionMetadata {
    /// Stable composition identity.
    pub id: String,
    /// Validation and runtime policy mode; defaults to development.
    #[serde(default)]
    pub mode: CompositionMode,
}

/// One lexical scope and its optional parent relationship.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ScopeSpec {
    pub id: ScopeId,
    /// Parent scope, or `None` for a root scope.
    #[serde(default)]
    pub parent: Option<ScopeId>,
}

fn default_enabled() -> bool {
    true
}

/// One mount from `rsi-meta.toml`. Contracts stay package-owned in
/// `plugin.toml`; only explicit binding choices are composition-owned.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InstanceSpec {
    /// Stable mount identity, distinct from package identity.
    pub id: InstanceId,
    /// Package directory resolved relative to the composition manifest.
    pub package: PathBuf,
    /// Lexical scope controlling implicit service resolution.
    pub scope: ScopeId,
    /// Whether the instance participates in the active graph.
    #[serde(default = "default_enabled")]
    pub enabled: bool,
    /// Package-owned configuration validated by its declared schema.
    #[serde(default)]
    pub config: serde_json::Value,
    /// Explicit service-to-provider choices overriding scope resolution.
    #[serde(default)]
    pub bindings: BTreeMap<ServiceKey, InstanceId>,
}

impl InstanceSpec {
    pub(crate) fn clone_package_from(&mut self, package: &PathBuf) {
        package.clone_into(&mut self.package);
    }
}

/// Complete versioned composition manifest.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CompositionManifest {
    /// Exact manifest format version.
    pub format_version: u32,
    /// Composition identity and policy mode.
    pub composition: CompositionMetadata,
    /// Bounded lexical scope declarations.
    pub scopes: Vec<ScopeSpec>,
    /// Bounded plugin mount declarations.
    pub instances: Vec<InstanceSpec>,
}

impl CompositionManifest {
    /// Creates an empty development composition at the current format version.
    pub fn empty(composition_id: impl Into<String>) -> Self {
        Self {
            format_version: COMPOSITION_FORMAT_VERSION,
            composition: CompositionMetadata {
                id: composition_id.into(),
                mode: CompositionMode::Development,
            },
            scopes: Vec::new(),
            instances: Vec::new(),
        }
    }

    #[allow(clippy::too_many_lines)]
    /// Validates all manifest bounds, identities, scopes, instances, and bindings.
    pub fn validate(&self) -> ValidationReport {
        let mut diagnostics = Vec::new();
        if self.format_version != COMPOSITION_FORMAT_VERSION {
            diagnostics.push(Diagnostic::error(
                "unsupported_format_version",
                format!(
                    "composition format {} is unsupported; expected {}",
                    self.format_version, COMPOSITION_FORMAT_VERSION
                ),
                Some("format_version".to_owned()),
            ));
        }
        if self.composition.id.trim().is_empty() {
            diagnostics.push(Diagnostic::error(
                "empty_composition_id",
                "composition.id must not be empty",
                Some("composition.id".to_owned()),
            ));
        } else if !valid_identity(&self.composition.id) {
            diagnostics.push(Diagnostic::error(
                "invalid_composition_id",
                "composition.id must be at most 255 ASCII bytes and contain only letters, digits, '.', '_', or '-'",
                Some("composition.id".to_owned()),
            ));
        }

        let binding_count = self.instances.iter().fold(0_usize, |total, instance| {
            total.saturating_add(instance.bindings.len())
        });
        for (code, actual, maximum, path) in [
            (
                "scope_limit",
                self.scopes.len(),
                MAX_COMPOSITION_SCOPES,
                "scopes",
            ),
            (
                "instance_limit",
                self.instances.len(),
                MAX_COMPOSITION_INSTANCES,
                "instances",
            ),
            (
                "binding_limit",
                binding_count,
                MAX_COMPOSITION_BINDINGS,
                "instances.bindings",
            ),
        ] {
            if actual > maximum {
                diagnostics.push(Diagnostic::error(
                    code,
                    format!("composition contains {actual} entries; maximum is {maximum}"),
                    Some(path.to_owned()),
                ));
            }
        }
        if !diagnostics.is_empty()
            && (self.scopes.len() > MAX_COMPOSITION_SCOPES
                || self.instances.len() > MAX_COMPOSITION_INSTANCES
                || binding_count > MAX_COMPOSITION_BINDINGS)
        {
            return ValidationReport { diagnostics };
        }

        let mut scopes = BTreeMap::new();
        for (index, scope) in self.scopes.iter().enumerate() {
            if scope.id.0.trim().is_empty() {
                diagnostics.push(Diagnostic::error(
                    "empty_scope_id",
                    "scope id must not be empty",
                    Some(format!("scopes[{index}].id")),
                ));
            } else if !valid_identity(&scope.id.0) {
                diagnostics.push(Diagnostic::error(
                    "invalid_scope_id",
                    "scope id must be at most 255 ASCII bytes and contain only letters, digits, '.', '_', or '-'",
                    Some(format!("scopes[{index}].id")),
                ));
            } else if scopes
                .insert(scope.id.clone(), scope.parent.clone())
                .is_some()
            {
                diagnostics.push(Diagnostic::error(
                    "duplicate_scope_id",
                    format!("scope {} occurs more than once", scope.id),
                    Some(format!("scopes[{index}].id")),
                ));
            }
        }
        let root_count = scopes.values().filter(|parent| parent.is_none()).count();
        if (!scopes.is_empty() || !self.instances.is_empty()) && root_count != 1 {
            diagnostics.push(Diagnostic::error(
                "scope_root_count",
                format!(
                    "a non-empty composition scope tree must have exactly one root; found {root_count}"
                ),
                Some("scopes".to_owned()),
            ));
        }
        for (index, scope) in self.scopes.iter().enumerate() {
            if let Some(parent) = &scope.parent {
                if !valid_identity(&parent.0) {
                    diagnostics.push(Diagnostic::error(
                        "invalid_scope_id",
                        "parent scope id must be at most 255 ASCII bytes and contain only letters, digits, '.', '_', or '-'",
                        Some(format!("scopes[{index}].parent")),
                    ));
                }
                if parent == &scope.id {
                    diagnostics.push(Diagnostic::error(
                        "scope_cycle",
                        format!("scope {} is its own parent", scope.id),
                        Some(format!("scopes[{index}].parent")),
                    ));
                } else if !scopes.contains_key(parent) {
                    diagnostics.push(Diagnostic::error(
                        "unknown_parent_scope",
                        format!("parent scope {parent} is not declared"),
                        Some(format!("scopes[{index}].parent")),
                    ));
                }
            }
        }
        validate_scope_graph(&scopes, &mut diagnostics);

        let mut instances = BTreeSet::new();
        for (index, instance) in self.instances.iter().enumerate() {
            let prefix = format!("instances[{index}]");
            if instance.id.0.trim().is_empty() {
                diagnostics.push(Diagnostic::error(
                    "empty_instance_id",
                    "instance id must not be empty",
                    Some(format!("{prefix}.id")),
                ));
            } else if !valid_identity(&instance.id.0) {
                diagnostics.push(Diagnostic::error(
                    "invalid_instance_id",
                    "instance id must be at most 255 ASCII bytes and contain only letters, digits, '.', '_', or '-'",
                    Some(format!("{prefix}.id")),
                ));
            } else if !instances.insert(instance.id.clone()) {
                diagnostics.push(Diagnostic::error(
                    "duplicate_instance_id",
                    format!("instance {} occurs more than once", instance.id),
                    Some(format!("{prefix}.id")),
                ));
            }
            if instance.package.as_os_str().is_empty() {
                diagnostics.push(Diagnostic::error(
                    "empty_package_path",
                    "package path must not be empty",
                    Some(format!("{prefix}.package")),
                ));
            }
            if !scopes.contains_key(&instance.scope) {
                diagnostics.push(Diagnostic::error(
                    "unknown_instance_scope",
                    format!("scope {} is not declared", instance.scope),
                    Some(format!("{prefix}.scope")),
                ));
            }
            if !valid_identity(&instance.scope.0) {
                diagnostics.push(Diagnostic::error(
                    "invalid_scope_id",
                    "instance scope id must be at most 255 ASCII bytes and contain only letters, digits, '.', '_', or '-'",
                    Some(format!("{prefix}.scope")),
                ));
            }
            for service in instance.bindings.keys() {
                validate_service(service, &format!("{prefix}.bindings"), &mut diagnostics);
            }
        }
        for (index, instance) in self.instances.iter().enumerate() {
            for provider in instance.bindings.values() {
                if !valid_identity(&provider.0) {
                    diagnostics.push(Diagnostic::error(
                        "invalid_instance_id",
                        "binding provider id must be at most 255 ASCII bytes and contain only letters, digits, '.', '_', or '-'",
                        Some(format!("instances[{index}].bindings")),
                    ));
                }
                if !instances.contains(provider) {
                    diagnostics.push(Diagnostic::error(
                        "unknown_binding_provider",
                        format!("binding provider {provider} is not mounted"),
                        Some(format!("instances[{index}].bindings")),
                    ));
                }
            }
        }

        ValidationReport { diagnostics }
    }

    pub(crate) fn scope_parents(&self) -> BTreeMap<ScopeId, Option<ScopeId>> {
        self.scopes
            .iter()
            .map(|scope| (scope.id.clone(), scope.parent.clone()))
            .collect()
    }
}

fn validate_scope_graph(
    scopes: &BTreeMap<ScopeId, Option<ScopeId>>,
    diagnostics: &mut Vec<Diagnostic>,
) {
    let mut state = BTreeMap::<ScopeId, u8>::new();
    let mut depths = BTreeMap::<ScopeId, usize>::new();
    for start in scopes.keys() {
        if state.get(start) == Some(&2) {
            continue;
        }
        let mut path = Vec::new();
        let mut cursor = Some(start.clone());
        let mut base_depth = 0_usize;
        let mut cycle = None;
        while let Some(scope) = cursor {
            match state.get(&scope).copied().unwrap_or(0) {
                2 => {
                    base_depth = depths.get(&scope).copied().unwrap_or(0);
                    break;
                }
                1 => {
                    cycle = Some(scope);
                    break;
                }
                _ => {
                    state.insert(scope.clone(), 1);
                    path.push(scope.clone());
                    cursor = scopes.get(&scope).and_then(Clone::clone);
                }
            }
        }
        if let Some(scope) = cycle {
            diagnostics.push(Diagnostic::error(
                "scope_cycle",
                format!("scope parent graph contains a cycle through {scope}"),
                Some("scopes".to_owned()),
            ));
        }
        let mut maximum_depth = base_depth;
        while let Some(scope) = path.pop() {
            base_depth = base_depth.saturating_add(1);
            maximum_depth = maximum_depth.max(base_depth);
            depths.insert(scope.clone(), base_depth);
            state.insert(scope, 2);
        }
        if maximum_depth > MAX_SCOPE_DEPTH {
            diagnostics.push(Diagnostic::error(
                "scope_depth_limit",
                format!("scope tree depth {maximum_depth} exceeds maximum {MAX_SCOPE_DEPTH}"),
                Some("scopes".to_owned()),
            ));
        }
    }
}

fn validate_service(service: &ServiceKey, path: &str, diagnostics: &mut Vec<Diagnostic>) {
    if !valid_service_name(&service.0) {
        diagnostics.push(Diagnostic::error(
            "invalid_service_name",
            "contract name must be at most 255 ASCII bytes and contain only letters, digits, '.', '_', '-', or '/'",
            Some(path.to_owned()),
        ));
    }
}

fn valid_identity(value: &str) -> bool {
    if value.len() > MAX_IDENTITY_BYTES {
        return false;
    }
    let mut bytes = value.bytes();
    matches!(bytes.next(), Some(first) if first.is_ascii_alphanumeric())
        && bytes.all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
}

fn valid_service_name(value: &str) -> bool {
    if value.len() > MAX_IDENTITY_BYTES {
        return false;
    }
    let mut bytes = value.bytes();
    matches!(bytes.next(), Some(first) if first.is_ascii_alphanumeric())
        && bytes
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-' | b'/'))
}

/// Locked package identity and hashes. Package and artifact digests are
/// required; a package that declares a config schema also pins its exact bytes.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LockedPackage {
    /// Package identity declared by `plugin.toml`.
    pub id: PackageId,
    /// Package version declared by `plugin.toml`.
    pub version: String,
    /// Canonical absolute package directory.
    pub path: PathBuf,
    /// Digest of the package manifest bytes.
    pub manifest_sha256: ContentHash,
    /// Digest of the target artifact bytes.
    pub artifact_sha256: ContentHash,
    /// Digest of declared configuration schema bytes, when present.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub config_schema_sha256: Option<ContentHash>,
}

/// Canonical target-specific resolution of a composition manifest.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CompositionLock {
    /// Exact lock format version.
    pub format_version: u32,
    /// Rust target triple for all locked artifacts.
    pub target: String,
    /// Digest of the composition manifest bytes this lock resolves.
    pub manifest_sha256: ContentHash,
    /// Locked packages in canonical order.
    #[serde(default)]
    pub packages: Vec<LockedPackage>,
}

impl CompositionLock {
    /// Checks the locked target, composition digest, and package-entry shape.
    /// Exact membership is checked after composition paths are canonicalized.
    ///
    /// # Errors
    ///
    /// Returns [`crate::HostError::LockMismatch`] for any unpinned or
    /// inconsistent lock input.
    pub fn validate_for_host(
        &self,
        _manifest: &CompositionManifest,
        host_target: &str,
        actual_manifest_hash: ContentHash,
    ) -> crate::Result<()> {
        if self.format_version != LOCK_FORMAT_VERSION {
            return Err(crate::HostError::LockMismatch(format!(
                "lock format {} is unsupported; expected {}",
                self.format_version, LOCK_FORMAT_VERSION
            )));
        }
        if self.target != host_target {
            return Err(crate::HostError::LockMismatch(format!(
                "lock target {:?} does not match host target {:?}",
                self.target, host_target
            )));
        }
        if self.manifest_sha256 != actual_manifest_hash {
            return Err(crate::HostError::LockMismatch(
                "composition manifest hash differs from the lock".to_owned(),
            ));
        }
        let mut seen = BTreeSet::new();
        for package in &self.packages {
            if package.id.0.is_empty() || package.version.is_empty() {
                return Err(crate::HostError::LockMismatch(
                    "locked package id and version must not be empty".to_owned(),
                ));
            }
            if !package.path.is_absolute() {
                return Err(crate::HostError::LockMismatch(format!(
                    "locked package path {} is not canonical absolute",
                    package.path.display()
                )));
            }
            if !seen.insert(package.path.clone()) {
                return Err(crate::HostError::LockMismatch(format!(
                    "package path {} occurs more than once",
                    package.path.display()
                )));
            }
        }
        Ok(())
    }
}

/// Verified package provenance retained in an active graph snapshot.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct PackageSource {
    pub package_id: PackageId,
    pub version: String,
    /// Canonical package manifest path.
    pub manifest_path: PathBuf,
    /// Selected target triple.
    pub target: String,
    /// Verified package manifest digest.
    pub manifest_sha256: ContentHash,
    /// Verified target artifact digest.
    pub artifact_sha256: ContentHash,
    /// Verified configuration schema digest, when declared.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub config_schema_sha256: Option<ContentHash>,
}

/// One package-declared service dependency.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ServiceRequirement {
    /// Required canonical service name.
    pub service: ServiceKey,
    /// Whether absence leaves the instance active.
    pub optional: bool,
}

/// Severity of one composition validation diagnostic.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DiagnosticSeverity {
    /// Invalid input that prevents graph construction.
    Error,
    /// Non-fatal condition callers should surface.
    Warning,
}

/// One stable composition validation finding.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Diagnostic {
    pub severity: DiagnosticSeverity,
    /// Stable machine-readable finding code.
    pub code: String,
    pub message: String,
    /// Optional dotted path to the offending manifest value.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
}

impl Diagnostic {
    pub fn error(
        code: impl Into<String>,
        message: impl Into<String>,
        path: Option<String>,
    ) -> Self {
        Self {
            severity: DiagnosticSeverity::Error,
            code: code.into(),
            message: message.into(),
            path,
        }
    }
}

/// Complete deterministic result of composition validation.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ValidationReport {
    /// Findings in deterministic validation order.
    pub diagnostics: Vec<Diagnostic>,
}

impl ValidationReport {
    pub fn is_valid(&self) -> bool {
        !self
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.severity == DiagnosticSeverity::Error)
    }
}

/// Reason an enabled or disabled instance is absent from active routing.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum InactiveReason {
    /// Manifest explicitly disabled the instance.
    Disabled,
    /// No provider resolved for a required service.
    MissingService { service: ServiceKey },
    /// Explicitly selected provider is itself inactive.
    ExplicitProviderInactive {
        service: ServiceKey,
        provider: InstanceId,
    },
}

/// Current routing state of one mounted instance.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum InstanceStatus {
    Active,
    /// Runtime fault removed the instance from routing.
    Faulted {
        /// Bounded runtime fault summary.
        reason: String,
    },
    /// Static graph resolution kept the instance inactive.
    Inactive {
        /// Deterministic reasons preventing activation.
        reasons: Vec<InactiveReason>,
    },
}

impl InstanceStatus {
    pub fn is_active(&self) -> bool {
        matches!(self, Self::Active)
    }
}

/// Immutable graph view of one mounted plugin instance.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct InstanceSnapshot {
    pub id: InstanceId,
    pub package: PackageSource,
    pub scope: ScopeId,
    /// Current activation or fault state.
    pub status: InstanceStatus,
    /// Canonical services provided by this package.
    pub provides: Vec<ServiceKey>,
    /// Canonical package-declared service requirements.
    pub requires: Vec<ServiceRequirement>,
}

/// Resolved consumer-service-provider edge in a graph snapshot.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct BindingSnapshot {
    pub consumer: InstanceId,
    pub service: ServiceKey,
    /// Generation-pinned provider selected by the graph.
    pub provider: InstanceId,
    /// Whether the manifest selected this edge explicitly.
    pub explicit: bool,
}

/// Immutable atomically published composition routing graph.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct GraphSnapshot {
    /// Monotonic committed graph revision.
    pub revision: GraphRevision,
    /// Stable active composition identity.
    pub composition_id: String,
    /// Instance snapshots keyed by mount identity.
    pub instances: BTreeMap<InstanceId, InstanceSnapshot>,
    /// Resolved service edges in deterministic order.
    pub bindings: Vec<BindingSnapshot>,
    /// Transient runtime disposal state. It is not part of a durable graph
    /// revision and is refreshed on every snapshot/query.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub retiring_instances: Vec<RetiringInstanceSnapshot>,
}

/// Transient disposal phase of a removed private generation.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RetirementPhase {
    /// New calls are excluded while existing generation leases drain.
    Draining,
    /// Leases drained and runtime retirement is in progress.
    Retiring,
    /// Runtime stop acknowledgement is in progress.
    Stopping,
}

/// Public aggregate of private generations retiring for one instance.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct RetiringInstanceSnapshot {
    pub instance_id: InstanceId,
    /// Number of private generations represented by this instance aggregate.
    /// Generation identifiers remain host-internal.
    pub generation_count: usize,
    /// Total outstanding leases across private generations.
    pub lease_count: usize,
    /// Least advanced disposal phase represented by the aggregate.
    pub phase: RetirementPhase,
}

/// Safe summary of the most recently requested disk composition pair.
/// Resolved configuration and secret material never enter this DTO.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct DesiredState {
    /// Digest of the most recently requested manifest, when readable.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub manifest_sha256: Option<String>,
    /// Digest of the most recently requested lock, when readable.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lock_sha256: Option<String>,
    /// Whether the requested pair is currently active.
    pub applied: bool,
    /// Stable rejection code for the last failed request, when any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_rejection_code: Option<String>,
    /// True only while a durable process-fixed restart requested by an
    /// in-process plugin still needs to be crossed by the supervising daemon.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub plugin_restart_requested: bool,
}

#[derive(Debug)]
pub(crate) struct Generation {
    pub id: u64,
    pub instance: InstanceId,
    runtime: std::sync::OnceLock<crate::runtime::RuntimeHandle>,
    admission_guard: std::sync::Mutex<()>,
    admitting: AtomicBool,
    leases: AtomicUsize,
    lease_drained: tokio::sync::Notify,
}

impl Generation {
    pub(crate) fn new(id: u64, instance: InstanceId) -> Self {
        Self {
            id,
            instance,
            runtime: std::sync::OnceLock::new(),
            admission_guard: std::sync::Mutex::new(()),
            admitting: AtomicBool::new(false),
            leases: AtomicUsize::new(0),
            lease_drained: tokio::sync::Notify::new(),
        }
    }

    pub(crate) fn attach_runtime(
        &self,
        runtime: crate::runtime::RuntimeHandle,
    ) -> crate::Result<()> {
        self.runtime.set(runtime).map_err(|_| {
            crate::HostError::InvalidEnvelope(format!(
                "runtime for generation {} was attached more than once",
                self.id
            ))
        })
    }

    pub(crate) fn runtime(&self) -> crate::Result<&crate::runtime::RuntimeHandle> {
        self.runtime.get().ok_or_else(|| {
            crate::HostError::InvalidEnvelope(format!(
                "runtime for generation {} is not attached",
                self.id
            ))
        })
    }

    pub(crate) fn runtime_opt(&self) -> Option<&crate::runtime::RuntimeHandle> {
        self.runtime.get()
    }

    pub(crate) fn has_healthy_runtime(&self) -> bool {
        self.runtime_opt()
            .is_none_or(crate::runtime::RuntimeHandle::is_healthy)
    }

    pub(crate) fn mark_admitting(&self) {
        let _guard = self
            .admission_guard
            .lock()
            .expect("generation admission mutex poisoned");
        self.admitting.store(true, Ordering::Release);
    }

    pub(crate) fn stop_admission(&self) {
        let _guard = self
            .admission_guard
            .lock()
            .expect("generation admission mutex poisoned");
        self.admitting.store(false, Ordering::Release);
    }

    pub(crate) fn is_admitting(&self) -> bool {
        self.admitting.load(Ordering::Acquire)
    }

    pub(crate) fn try_admit_lease(self: &Arc<Self>) -> Option<GenerationLease> {
        let _guard = self
            .admission_guard
            .lock()
            .expect("generation admission mutex poisoned");
        if !self.admitting.load(Ordering::Acquire) || !self.has_healthy_runtime() {
            return None;
        }
        Some(self.new_lease())
    }

    /// Pins a dependency selected for a shadow generation. This deliberately
    /// bypasses public admission: registry-owned prepare has already selected
    /// the immutable generation before cutover.
    pub(crate) fn dependency_lease(self: &Arc<Self>) -> GenerationLease {
        self.new_lease()
    }

    fn new_lease(self: &Arc<Self>) -> GenerationLease {
        self.leases.fetch_add(1, Ordering::AcqRel);
        GenerationLease {
            generation: Arc::clone(self),
        }
    }

    pub(crate) async fn wait_for_lease_drain(&self) {
        loop {
            let drained = self.lease_drained.notified();
            tokio::pin!(drained);
            drained.as_mut().enable();
            if self.leases.load(Ordering::Acquire) == 0 {
                return;
            }
            drained.await;
        }
    }

    pub(crate) fn lease_count(&self) -> usize {
        self.leases.load(Ordering::Acquire)
    }
}

#[derive(Debug)]
pub(crate) struct GenerationLease {
    generation: Arc<Generation>,
}

impl Drop for GenerationLease {
    fn drop(&mut self) {
        let previous = self.generation.leases.fetch_sub(1, Ordering::AcqRel);
        debug_assert!(previous > 0, "generation lease underflow");
        if previous == 1 {
            self.generation.lease_drained.notify_waiters();
        }
    }
}

#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub(crate) struct RouteKey {
    pub consumer: InstanceId,
    pub service: ServiceKey,
}

#[derive(Clone, Debug)]
pub(crate) struct RouteTarget {
    pub provider: InstanceId,
    pub explicit: bool,
    pub generation: Arc<Generation>,
}

/// Immutable route table atomically published only after its durable commit.
#[derive(Clone, Debug)]
pub struct RoutingSnapshot {
    graph: GraphSnapshot,
    event_cursor: u64,
    pub(crate) routes: BTreeMap<RouteKey, RouteTarget>,
    generations: BTreeMap<InstanceId, Arc<Generation>>,
    token_generation: u64,
    active: Option<crate::domain::CompositionDigest>,
    admission: Arc<SnapshotAdmission>,
}

#[derive(Debug)]
struct SnapshotAdmission {
    guard: std::sync::Mutex<()>,
    admitting: AtomicBool,
}

impl RoutingSnapshot {
    pub(crate) fn new(
        graph: GraphSnapshot,
        routes: BTreeMap<RouteKey, RouteTarget>,
        generations: BTreeMap<InstanceId, Arc<Generation>>,
    ) -> Self {
        Self {
            graph,
            event_cursor: 0,
            routes,
            generations,
            token_generation: 0,
            active: None,
            admission: Arc::new(SnapshotAdmission {
                guard: std::sync::Mutex::new(()),
                admitting: AtomicBool::new(false),
            }),
        }
    }

    pub fn revision(&self) -> GraphRevision {
        self.graph.revision
    }

    pub fn graph(&self) -> &GraphSnapshot {
        &self.graph
    }

    /// Cursor of the durable event whose graph this snapshot reflects.
    pub fn event_cursor(&self) -> u64 {
        self.event_cursor
    }

    pub(crate) fn route(&self, key: &RouteKey) -> Option<&RouteTarget> {
        self.routes.get(key)
    }

    pub(crate) fn mark_admitting(&self) {
        let _guard = self
            .admission
            .guard
            .lock()
            .expect("routing admission mutex poisoned");
        self.admission.admitting.store(true, Ordering::Release);
    }

    pub(crate) fn stop_admission(&self) {
        let _guard = self
            .admission
            .guard
            .lock()
            .expect("routing admission mutex poisoned");
        self.admission.admitting.store(false, Ordering::Release);
    }

    #[cfg(test)]
    pub(crate) fn is_admitting(&self) -> bool {
        self.admission.admitting.load(Ordering::Acquire)
    }

    pub(crate) fn try_admit_route_lease(&self, key: &RouteKey) -> Option<GenerationLease> {
        let _guard = self
            .admission
            .guard
            .lock()
            .expect("routing admission mutex poisoned");
        if !self.admission.admitting.load(Ordering::Acquire) {
            return None;
        }
        self.routes.get(key)?.generation.try_admit_lease()
    }

    pub(crate) fn generation(&self, instance: &InstanceId) -> Option<&Arc<Generation>> {
        self.generations.get(instance)
    }

    pub(crate) fn generations(&self) -> impl Iterator<Item = &Arc<Generation>> {
        self.generations.values()
    }

    pub(crate) fn set_event_cursor(&mut self, cursor: u64) {
        self.event_cursor = cursor;
    }

    pub(crate) fn token_generation(&self) -> u64 {
        self.token_generation
    }

    pub(crate) fn set_token_generation(&mut self, generation: u64) {
        self.token_generation = generation;
    }

    pub(crate) fn active(&self) -> Option<&crate::domain::CompositionDigest> {
        self.active.as_ref()
    }

    pub(crate) fn set_active(&mut self, active: Option<crate::domain::CompositionDigest>) {
        self.active = active;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mount(id: &str, scope: &str) -> InstanceSpec {
        InstanceSpec {
            id: InstanceId::new(id),
            package: PathBuf::from(format!("{id}/plugin.toml")),
            scope: ScopeId::new(scope),
            enabled: true,
            config: serde_json::json!({}),
            bindings: BTreeMap::new(),
        }
    }

    #[test]
    fn empty_composition_may_have_no_scope() {
        assert!(CompositionManifest::empty("empty").validate().is_valid());
    }

    #[test]
    fn manifest_identity_fields_are_bounded_portable_ascii() {
        let mut manifest = CompositionManifest::empty("contains space");
        manifest.scopes.push(ScopeSpec {
            id: ScopeId::new("root"),
            parent: None,
        });
        let mut consumer = mount(&"i".repeat(256), "root");
        consumer.bindings.insert(
            ServiceKey::new("service with space"),
            InstanceId::new("provider"),
        );
        manifest.instances = vec![consumer, mount("provider", "root")];

        let report = manifest.validate();
        for expected in [
            "invalid_composition_id",
            "invalid_instance_id",
            "invalid_service_name",
        ] {
            assert!(
                report
                    .diagnostics
                    .iter()
                    .any(|diagnostic| diagnostic.code == expected),
                "missing {expected}: {:#?}",
                report.diagnostics
            );
        }
    }

    #[test]
    fn mounted_composition_requires_one_scope_root() {
        let mut manifest = CompositionManifest::empty("demo");
        manifest.instances.push(mount("consumer", "missing"));
        let report = manifest.validate();
        assert!(
            report
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.code == "scope_root_count")
        );

        manifest.scopes = vec![
            ScopeSpec {
                id: ScopeId::new("left"),
                parent: None,
            },
            ScopeSpec {
                id: ScopeId::new("right"),
                parent: None,
            },
        ];
        manifest.instances[0].scope = ScopeId::new("left");
        let report = manifest.validate();
        assert!(
            report
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.code == "scope_root_count")
        );
    }

    #[test]
    fn composition_cardinality_is_rejected_before_detailed_scans() {
        let mut manifest = CompositionManifest::empty("bounded");
        manifest.scopes = (0..=MAX_COMPOSITION_SCOPES)
            .map(|index| ScopeSpec {
                id: ScopeId::new(format!("scope-{index}")),
                parent: (index != 0).then(|| ScopeId::new("scope-0")),
            })
            .collect();
        let report = manifest.validate();
        assert!(
            report
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.code == "scope_limit")
        );
    }

    #[test]
    fn scope_depth_has_an_explicit_linear_time_limit() {
        let mut manifest = CompositionManifest::empty("bounded");
        manifest.scopes = (0..=MAX_SCOPE_DEPTH)
            .map(|index| ScopeSpec {
                id: ScopeId::new(format!("scope-{index}")),
                parent: (index != 0).then(|| ScopeId::new(format!("scope-{}", index - 1))),
            })
            .collect();
        let report = manifest.validate();
        assert!(
            report
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.code == "scope_depth_limit")
        );
    }

    #[test]
    fn explicit_binding_may_cross_sibling_scope_branches() {
        let mut consumer = mount("consumer", "left");
        consumer
            .bindings
            .insert(ServiceKey::new("fixture.echo"), InstanceId::new("provider"));
        let manifest = CompositionManifest {
            format_version: COMPOSITION_FORMAT_VERSION,
            composition: CompositionMetadata {
                id: "demo".to_owned(),
                mode: CompositionMode::Development,
            },
            scopes: vec![
                ScopeSpec {
                    id: ScopeId::new("root"),
                    parent: None,
                },
                ScopeSpec {
                    id: ScopeId::new("left"),
                    parent: Some(ScopeId::new("root")),
                },
                ScopeSpec {
                    id: ScopeId::new("right"),
                    parent: Some(ScopeId::new("root")),
                },
            ],
            instances: vec![consumer, mount("provider", "right")],
        };
        assert!(manifest.validate().is_valid());
    }

    #[test]
    fn stopped_generation_cannot_admit_a_new_public_lease() {
        let generation = Arc::new(Generation::new(7, InstanceId::new("provider")));
        generation.mark_admitting();
        let admitted = generation
            .try_admit_lease()
            .expect("committed generation admits");
        assert_eq!(generation.lease_count(), 1);

        generation.stop_admission();
        assert!(generation.try_admit_lease().is_none());
        assert_eq!(generation.lease_count(), 1);

        drop(admitted);
        assert_eq!(generation.lease_count(), 0);
    }

    #[tokio::test]
    async fn lease_drain_waiter_converges_without_per_lease_notifications() {
        let generation = Arc::new(Generation::new(7, InstanceId::new("provider")));
        generation.mark_admitting();
        let lease = generation.try_admit_lease().expect("lease");
        let waiting = Arc::clone(&generation);
        let waiter = tokio::spawn(async move { waiting.wait_for_lease_drain().await });
        tokio::task::yield_now().await;
        assert!(!waiter.is_finished());

        drop(lease);
        tokio::time::timeout(std::time::Duration::from_secs(1), waiter)
            .await
            .expect("drain notification")
            .expect("waiter task");
    }

    #[test]
    fn stopped_routing_snapshot_cannot_admit_a_reused_provider_route() {
        let consumer = InstanceId::new("consumer");
        let service = ServiceKey::new("fixture.echo");
        let key = RouteKey {
            consumer: consumer.clone(),
            service: service.clone(),
        };
        let provider_a = Arc::new(Generation::new(1, InstanceId::new("provider-a")));
        let provider_b = Arc::new(Generation::new(2, InstanceId::new("provider-b")));
        provider_a.mark_admitting();
        provider_b.mark_admitting();
        let graph = |revision| GraphSnapshot {
            revision: GraphRevision(revision),
            composition_id: "demo".to_owned(),
            instances: BTreeMap::new(),
            bindings: Vec::new(),
            retiring_instances: Vec::new(),
        };
        let old = RoutingSnapshot::new(
            graph(1),
            BTreeMap::from([(
                key.clone(),
                RouteTarget {
                    provider: provider_a.instance.clone(),
                    explicit: true,
                    generation: Arc::clone(&provider_a),
                },
            )]),
            BTreeMap::from([
                (provider_a.instance.clone(), Arc::clone(&provider_a)),
                (provider_b.instance.clone(), Arc::clone(&provider_b)),
            ]),
        );
        let new = RoutingSnapshot::new(
            graph(2),
            BTreeMap::from([(
                key.clone(),
                RouteTarget {
                    provider: provider_b.instance.clone(),
                    explicit: true,
                    generation: Arc::clone(&provider_b),
                },
            )]),
            BTreeMap::from([
                (provider_a.instance.clone(), Arc::clone(&provider_a)),
                (provider_b.instance.clone(), Arc::clone(&provider_b)),
            ]),
        );
        old.mark_admitting();
        new.mark_admitting();

        // This is the cutover linearization point. A caller retaining `old`
        // cannot acquire provider A after it, even though A itself is reused.
        old.stop_admission();
        assert!(old.try_admit_route_lease(&key).is_none());
        assert_eq!(provider_a.lease_count(), 0);
        let new_lease = new
            .try_admit_route_lease(&key)
            .expect("new snapshot admits provider B");
        assert_eq!(provider_b.lease_count(), 1);
        drop(new_lease);
    }
}
