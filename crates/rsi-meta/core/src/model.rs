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

#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(transparent)]
pub struct PackageId(pub String);

impl PackageId {
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
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }
}

impl fmt::Display for InstanceId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ScopeId(pub String);

impl ScopeId {
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

#[derive(
    Clone, Copy, Debug, Default, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize,
)]
#[serde(transparent)]
pub struct GraphRevision(pub u64);

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CompositionMode {
    #[default]
    Development,
    Production,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CompositionMetadata {
    pub id: String,
    #[serde(default)]
    pub mode: CompositionMode,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ScopeSpec {
    pub id: ScopeId,
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
    pub id: InstanceId,
    pub package: PathBuf,
    pub scope: ScopeId,
    #[serde(default = "default_enabled")]
    pub enabled: bool,
    #[serde(default)]
    pub config: serde_json::Value,
    #[serde(default)]
    pub bindings: BTreeMap<ServiceKey, InstanceId>,
}

impl InstanceSpec {
    pub(crate) fn clone_package_from(&mut self, package: &PathBuf) {
        package.clone_into(&mut self.package);
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CompositionManifest {
    pub format_version: u32,
    pub composition: CompositionMetadata,
    pub scopes: Vec<ScopeSpec>,
    pub instances: Vec<InstanceSpec>,
}

impl CompositionManifest {
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
        for scope in scopes.keys() {
            let mut seen = BTreeSet::new();
            let mut cursor = Some(scope);
            while let Some(id) = cursor {
                if !seen.insert(id.clone()) {
                    diagnostics.push(Diagnostic::error(
                        "scope_cycle",
                        format!("scope parent graph contains a cycle through {id}"),
                        Some("scopes".to_owned()),
                    ));
                    break;
                }
                cursor = scopes.get(id).and_then(Option::as_ref);
            }
        }

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
    pub id: PackageId,
    pub version: String,
    pub path: PathBuf,
    pub manifest_sha256: ContentHash,
    pub artifact_sha256: ContentHash,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub config_schema_sha256: Option<ContentHash>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CompositionLock {
    pub format_version: u32,
    pub target: String,
    pub manifest_sha256: ContentHash,
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

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct PackageSource {
    pub package_id: PackageId,
    pub version: String,
    pub manifest_path: PathBuf,
    pub target: String,
    pub manifest_sha256: ContentHash,
    pub artifact_sha256: ContentHash,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub config_schema_sha256: Option<ContentHash>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ServiceRequirement {
    pub service: ServiceKey,
    pub optional: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DiagnosticSeverity {
    Error,
    Warning,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Diagnostic {
    pub severity: DiagnosticSeverity,
    pub code: String,
    pub message: String,
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

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ValidationReport {
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

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum InactiveReason {
    Disabled,
    MissingService {
        service: ServiceKey,
    },
    ExplicitProviderInactive {
        service: ServiceKey,
        provider: InstanceId,
    },
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum InstanceStatus {
    Active,
    Inactive { reasons: Vec<InactiveReason> },
}

impl InstanceStatus {
    pub fn is_active(&self) -> bool {
        matches!(self, Self::Active)
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct InstanceSnapshot {
    pub id: InstanceId,
    pub package: PackageSource,
    pub scope: ScopeId,
    pub status: InstanceStatus,
    pub provides: Vec<ServiceKey>,
    pub requires: Vec<ServiceRequirement>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct BindingSnapshot {
    pub consumer: InstanceId,
    pub service: ServiceKey,
    pub provider: InstanceId,
    pub explicit: bool,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct GraphSnapshot {
    pub revision: GraphRevision,
    pub composition_id: String,
    pub instances: BTreeMap<InstanceId, InstanceSnapshot>,
    pub bindings: Vec<BindingSnapshot>,
    /// Transient runtime disposal state. It is not part of a durable graph
    /// revision and is refreshed on every snapshot/query.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub retiring_instances: Vec<RetiringInstanceSnapshot>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RetirementPhase {
    Draining,
    Retiring,
    Stopping,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct RetiringInstanceSnapshot {
    pub instance_id: InstanceId,
    /// Number of private generations represented by this instance aggregate.
    /// Generation identifiers remain host-internal.
    pub generation_count: usize,
    pub lease_count: usize,
    pub phase: RetirementPhase,
}

/// Safe summary of the most recently requested disk composition pair.
/// Resolved configuration and secret material never enter this DTO.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct DesiredState {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub manifest_sha256: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lock_sha256: Option<String>,
    pub applied: bool,
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
    lease_changes: tokio::sync::watch::Sender<usize>,
}

impl Generation {
    pub(crate) fn new(id: u64, instance: InstanceId) -> Self {
        let (lease_changes, _receiver) = tokio::sync::watch::channel(0);
        Self {
            id,
            instance,
            runtime: std::sync::OnceLock::new(),
            admission_guard: std::sync::Mutex::new(()),
            admitting: AtomicBool::new(false),
            leases: AtomicUsize::new(0),
            lease_changes,
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
        if !self.admitting.load(Ordering::Acquire) {
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
        let count = self.leases.fetch_add(1, Ordering::AcqRel).saturating_add(1);
        self.lease_changes.send_replace(count);
        GenerationLease {
            generation: Arc::clone(self),
        }
    }

    pub(crate) async fn wait_for_lease_drain(&self) {
        let mut changes = self.lease_changes.subscribe();
        loop {
            if self.leases.load(Ordering::Acquire) == 0 {
                return;
            }
            if changes.changed().await.is_err() {
                return;
            }
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
        self.generation
            .lease_changes
            .send_replace(previous.saturating_sub(1));
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
