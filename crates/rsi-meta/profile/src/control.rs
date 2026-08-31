use super::{
    CandidateLeaf, IsolationSpec, ProfileCandidate, ProfileCompiler, ProfileEnvironment,
    ProfileError, ProfileLimits, ProfileProgram, Result, TreeNode, bound_message,
    read_file_bounded,
};
use async_trait::async_trait;
use rsi_meta::{
    ActivationPlan, ConfigValue, Context, FactoryIdentity, FiberHandle, FiberState, LocalContract,
    MetaError, PendingReport, PluginFactory, PluginId, PreparedActivation, PreparedPlugin,
    ResolvedFactory, Runtime, UpdateMode,
};
use sha2::{Digest as _, Sha256};
use std::collections::BTreeMap;
use std::fmt;
use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, Weak};
use std::time::{Duration, SystemTime};
use tokio::sync::{Mutex as AsyncMutex, Notify, watch};
use tokio_util::sync::CancellationToken;

const WATCH_INTERVAL: Duration = Duration::from_millis(100);
const FULL_CONTENT_AUDIT_TICKS: usize = 50;
const MINIMUM_AUTOMATIC_RELOAD_BACKOFF: Duration = Duration::from_secs(1);
const MAXIMUM_AUTOMATIC_RELOAD_BACKOFF: Duration = Duration::from_secs(5);

fn automatic_reload_backoff(consecutive_failures: u32) -> Duration {
    let exponent = consecutive_failures.saturating_sub(1).min(5);
    MINIMUM_AUTOMATIC_RELOAD_BACKOFF
        .saturating_mul(1_u32 << exponent)
        .min(MAXIMUM_AUTOMATIC_RELOAD_BACKOFF)
}

/// Frozen Host resolver used by Profile preflight and group isolation.
pub trait ProfileResolver: Send + Sync + fmt::Debug + 'static {
    /// Resolves one catalog key to immutable executable provenance.
    fn resolve(&self, plugin: &PluginId) -> Result<ResolvedFactory>;

    /// Applies one validated group isolation declaration to a child Context.
    fn isolate(&self, context: Context, isolation: &IsolationSpec) -> Result<Context>;
}

/// Health of the latest desired Profile target.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProfileHealth {
    /// A serialized Runtime convergence is in progress.
    Converging,
    /// Every desired leaf is observed as Active or Pending.
    Converged,
    /// Candidate application or later child activation left a failed graph.
    Degraded,
    /// The desired source is valid but needs a process restart.
    RestartRequired,
    /// The owning Profile Fiber retired.
    Stopped,
}

/// Health of the in-process required-source watcher.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WatcherHealth {
    /// The source program has no files.
    Inactive,
    /// Every watched source was readable at the latest probe.
    Healthy,
    /// A watched source could not be read; polling and manual reload continue.
    Faulted,
}

/// Resolver-owned desired leaf with configuration redacted.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProfileTargetStatus {
    id: rsi_meta::InstanceId,
    factory: FactoryIdentity,
}

impl ProfileTargetStatus {
    /// Stable all-tree leaf identity.
    pub const fn id(&self) -> &rsi_meta::InstanceId {
        &self.id
    }

    /// Immutable selected executable provenance.
    pub const fn factory(&self) -> &FactoryIdentity {
        &self.factory
    }
}

/// One actually observed Profile-owned child Fiber.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProfileInstanceStatus {
    id: rsi_meta::InstanceId,
    factory: FactoryIdentity,
    state: ProfileInstanceState,
}

/// Redacted lifecycle category for one Profile-owned child Fiber.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ProfileInstanceState {
    /// Dependency convergence has not produced an activatable snapshot.
    Pending(PendingReport),
    /// One generation is staging owned resources.
    Loading,
    /// The staged generation is published.
    Active,
    /// Activation or retirement failed; the plugin diagnostic is not exposed.
    Failed,
    /// Publications are withdrawn and owned resources are retiring.
    Unloading,
    /// Final teardown completed unexpectedly while Profile still desired the child.
    Disposed,
}

impl ProfileInstanceStatus {
    /// Stable all-tree leaf identity.
    pub const fn id(&self) -> &rsi_meta::InstanceId {
        &self.id
    }

    /// Immutable selected executable provenance.
    pub const fn factory(&self) -> &FactoryIdentity {
        &self.factory
    }

    /// Current redacted lifecycle observation.
    pub const fn state(&self) -> &ProfileInstanceState {
        &self.state
    }
}

/// Bounded redacted status published after Profile state changes.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProfileStatus {
    revision: u64,
    health: ProfileHealth,
    watcher: WatcherHealth,
    source_digest: String,
    target: Vec<ProfileTargetStatus>,
    observed: Vec<ProfileInstanceStatus>,
    diagnostic: Option<String>,
}

impl ProfileStatus {
    /// Monotonic completed convergence revision.
    pub const fn revision(&self) -> u64 {
        self.revision
    }

    /// Current target health.
    pub const fn health(&self) -> ProfileHealth {
        self.health
    }

    /// Current watcher health.
    pub const fn watcher(&self) -> WatcherHealth {
        self.watcher
    }

    /// Digest of the latest accepted source candidate.
    pub fn source_digest(&self) -> &str {
        &self.source_digest
    }

    /// Desired enabled leaves with configurations redacted.
    pub fn target(&self) -> &[ProfileTargetStatus] {
        &self.target
    }

    /// Actually observed child Fibers.
    pub fn observed(&self) -> &[ProfileInstanceStatus] {
        &self.observed
    }

    /// Latest bounded redacted operational diagnostic.
    pub fn diagnostic(&self) -> Option<&str> {
        self.diagnostic.as_deref()
    }
}

/// One redacted declarative node in a Profile snapshot.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SnapshotNode {
    id: String,
    plugin: Option<PluginId>,
    enabled: bool,
    children: Vec<SnapshotNode>,
}

impl SnapshotNode {
    /// Stable all-tree identity.
    pub fn id(&self) -> &str {
        &self.id
    }

    /// Plugin catalog key for a leaf, or `None` for a group.
    pub const fn plugin(&self) -> Option<&PluginId> {
        self.plugin.as_ref()
    }

    /// Evaluated literal enabled state at this node.
    pub const fn enabled(&self) -> bool {
        self.enabled
    }

    /// Redacted group children.
    pub fn children(&self) -> &[SnapshotNode] {
        &self.children
    }
}

/// Redacted desired tree snapshot.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProfileSnapshot {
    revision: u64,
    source_digest: String,
    nodes: Vec<SnapshotNode>,
}

impl ProfileSnapshot {
    /// Revision associated with this desired tree.
    pub const fn revision(&self) -> u64 {
        self.revision
    }

    /// Digest of its complete immutable sources.
    pub fn source_digest(&self) -> &str {
        &self.source_digest
    }

    /// Redacted top-level nodes.
    pub fn nodes(&self) -> &[SnapshotNode] {
        &self.nodes
    }
}

/// Structured result of one completed manual or watched rebuild.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ReloadOutcome {
    /// The candidate became the observed desired graph.
    Applied(ProfileStatus),
    /// The healthy semantic tree was equal and revision did not advance.
    Unchanged(ProfileStatus),
    /// No Runtime mutation occurred because a changed leaf requires restart.
    RestartRequired(ProfileStatus),
    /// Candidate application failed and the prior target was reconstructed.
    RolledBack {
        /// Restored converged status.
        status: ProfileStatus,
        /// Bounded redacted candidate failure.
        error: String,
    },
    /// Candidate application and compensation both failed.
    Degraded {
        /// Actually observed partial graph.
        status: ProfileStatus,
        /// Bounded redacted candidate failure.
        error: String,
        /// Bounded redacted compensation failure.
        rollback_error: String,
    },
}

impl ReloadOutcome {
    /// Status published by this completed attempt.
    pub const fn status(&self) -> &ProfileStatus {
        match self {
            Self::Applied(status)
            | Self::Unchanged(status)
            | Self::RestartRequired(status)
            | Self::RolledBack { status, .. }
            | Self::Degraded { status, .. } => status,
        }
    }
}

/// Process-local Profile control supplied by the Profile Fiber.
#[async_trait]
pub trait ProfileControl: Send + Sync + fmt::Debug + 'static {
    /// Rebuilds from the complete immutable source program.
    async fn reload(&self) -> Result<ReloadOutcome>;

    /// Returns a bounded point-in-time status.
    fn status(&self) -> ProfileStatus;

    /// Returns the desired tree without configs or expression source.
    fn snapshot(&self) -> ProfileSnapshot;

    /// Subscribes to last-value status changes.
    fn subscribe(&self) -> watch::Receiver<ProfileStatus>;
}

/// Nominal Local marker for [`ProfileControl`].
#[derive(Debug)]
pub enum ProfileControlContract {}

impl LocalContract for ProfileControlContract {
    const KEY: &'static str = "rsi.meta.profile.control";
    type Service = dyn ProfileControl;
}

/// Prepared direct bootstrap retained by `rsi-host` through activation.
pub struct ProfileBootstrap {
    factory: Arc<ProfileFactory>,
    control: Arc<Controller>,
}

impl fmt::Debug for ProfileBootstrap {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ProfileBootstrap")
            .finish_non_exhaustive()
    }
}

impl ProfileBootstrap {
    /// Compiles, resolves, and prepares every enabled initial leaf.
    pub fn prepare(
        runtime: &Runtime,
        resolver: Arc<dyn ProfileResolver>,
        program: ProfileProgram,
        environment: ProfileEnvironment,
        limits: ProfileLimits,
    ) -> Result<Self> {
        let compiler = ProfileCompiler::new(environment, limits);
        let candidate = compiler.compile(&program)?;
        let prepared = prepare_new_target(runtime, resolver.as_ref(), candidate)?;
        let control = Arc::new(Controller::new(
            runtime.clone(),
            resolver,
            program,
            compiler,
            &prepared.target,
        ));
        let factory = Arc::new(ProfileFactory {
            initial: Mutex::new(Some(prepared)),
            control: Arc::downgrade(&control),
        });
        Ok(Self { factory, control })
    }

    /// Returns the ordinary Profile plugin factory for one direct Host apply.
    pub fn factory(&self) -> Arc<dyn PluginFactory> {
        Arc::clone(&self.factory) as Arc<dyn PluginFactory>
    }

    /// Returns an external strong control handle retained by the running Host.
    pub fn control(&self) -> Arc<dyn ProfileControl> {
        Arc::new(StrongControl(Arc::clone(&self.control)))
    }
}

#[derive(Clone)]
struct ResolvedLeaf {
    candidate: CandidateLeaf,
    factory: ResolvedFactory,
    identity: FactoryIdentity,
    update_mode: UpdateMode,
}

impl fmt::Debug for ResolvedLeaf {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ResolvedLeaf")
            .field("id", self.candidate.id())
            .field("factory", &self.identity)
            .field("update_mode", &self.update_mode)
            .field("config", &"<redacted>")
            .finish()
    }
}

#[derive(Clone, Debug)]
struct ResolvedTarget {
    candidate: ProfileCandidate,
    leaves: Vec<ResolvedLeaf>,
}

/// Opaque one-shot plan for one static Profile generation.
///
/// The plan retains immutable resolved factories without exposing the resolver
/// or executable leaf details. It installs no source watcher or control plane.
pub struct ProfileGenerationPlan {
    target: ResolvedTarget,
    resolver: Arc<dyn ProfileResolver>,
}

impl fmt::Debug for ProfileGenerationPlan {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ProfileGenerationPlan")
            .field("source_digest", &self.target.candidate.source_digest())
            .field("instances", &self.target.leaves.len())
            .finish_non_exhaustive()
    }
}

impl ProfileGenerationPlan {
    /// Resolves every enabled candidate leaf against one immutable resolver.
    ///
    /// This operation does not prepare or apply a plugin and performs no
    /// Runtime mutation.
    pub fn resolve(
        candidate: ProfileCandidate,
        resolver: Arc<dyn ProfileResolver>,
    ) -> Result<Self> {
        let target = resolve_target(resolver.as_ref(), candidate)?;
        Ok(Self { target, resolver })
    }

    /// Returns the host-platform-scoped digest of the compiled source snapshot.
    pub fn source_digest(&self) -> &str {
        self.target.candidate.source_digest()
    }

    /// Returns the canonical required file sources captured by compilation.
    pub fn watch_paths(&self) -> &[PathBuf] {
        self.target.candidate.watch_paths()
    }

    /// Activates this plan once below `parent` and returns its owning wrapper.
    ///
    /// Every resolved leaf must become Active. Preparation failure leaves no
    /// Fiber behind; after wrapper creation, failure or cooperative
    /// cancellation disposes it before this method returns. The returned
    /// wrapper rejects reconfiguration; replacement requires a new plan.
    pub async fn activate(
        self,
        parent: &Context,
        cancellation: &CancellationToken,
    ) -> Result<FiberHandle> {
        if cancellation.is_cancelled() {
            return Err(ProfileError::Meta(MetaError::Cancelled));
        }
        let Self { target, resolver } = self;
        let prepared = prepare_generation(target, parent.runtime(), cancellation).await?;
        let source_digest = prepared.target.candidate.source_digest().to_owned();
        let first_instance = prepared
            .target
            .leaves
            .first()
            .map(|leaf| leaf.candidate.id().clone());
        let wrapper_context = Arc::new(Mutex::new(None));
        let factory = Arc::new(ProfileGenerationFactory {
            context: Arc::clone(&wrapper_context),
            activated: AtomicBool::new(false),
        });
        let handle = parent
            .apply(
                ResolvedFactory::linked(
                    "rsi.meta.profile.generation",
                    source_digest,
                    UpdateMode::RestartRequired,
                    factory,
                ),
                ConfigValue::Null,
            )
            .await?;
        if !matches!(handle.snapshot().state, FiberState::Active) {
            let _cleanup = handle.dispose().await;
            return Err(first_instance.map_or_else(
                || ProfileError::InvalidProgram("static Profile generation failed".to_owned()),
                |instance| ProfileError::Application { instance },
            ));
        }
        let wrapper_context = wrapper_context
            .lock()
            .ok()
            .and_then(|mut context| context.take());
        let Some(wrapper_context) = wrapper_context else {
            return rollback_generation(
                handle,
                ProfileError::InvalidProgram(
                    "static Profile generation did not capture its Context".to_owned(),
                ),
            )
            .await;
        };
        mount_generation(
            prepared,
            resolver.as_ref(),
            &wrapper_context,
            handle,
            cancellation,
        )
        .await
    }
}

async fn prepare_generation(
    target: ResolvedTarget,
    runtime: &Runtime,
    cancellation: &CancellationToken,
) -> Result<PreparedTarget> {
    let mut prepared = Vec::with_capacity(target.leaves.len());
    for leaf in &target.leaves {
        if cancellation.is_cancelled() {
            return Err(ProfileError::Meta(MetaError::Cancelled));
        }
        let instance = leaf.candidate.id().clone();
        let runtime = runtime.clone();
        let factory = leaf.factory.clone();
        let config = leaf.candidate.config().clone();
        let mut task = tokio::task::spawn_blocking(move || runtime.prepare(factory, config));
        let joined = tokio::select! {
            biased;
            () = cancellation.cancelled() => {
                let _completion = task.await;
                return Err(ProfileError::Meta(MetaError::Cancelled));
            }
            joined = &mut task => joined,
        };
        let proof = joined
            .map_err(|_| ProfileError::Preparation {
                instance: instance.clone(),
            })?
            .map_err(|_| ProfileError::Preparation { instance })?;
        prepared.push(Some(proof));
    }
    if cancellation.is_cancelled() {
        return Err(ProfileError::Meta(MetaError::Cancelled));
    }
    Ok(PreparedTarget { target, prepared })
}

async fn mount_generation(
    prepared: PreparedTarget,
    resolver: &dyn ProfileResolver,
    wrapper_context: &Context,
    handle: FiberHandle,
    cancellation: &CancellationToken,
) -> Result<FiberHandle> {
    if cancellation.is_cancelled() {
        return rollback_generation(handle, ProfileError::Meta(MetaError::Cancelled)).await;
    }
    let candidate = match bind_target(prepared, wrapper_context, resolver) {
        Ok(candidate) => candidate,
        Err(error) => return rollback_generation(handle, error).await,
    };
    for (index, mut bound) in candidate.leaves.into_iter().enumerate() {
        let leaf_target = candidate.target.leaves[index].clone();
        if cancellation.is_cancelled() {
            return rollback_generation(handle, ProfileError::Meta(MetaError::Cancelled)).await;
        }
        let outcome = {
            let proof = bound
                .prepared
                .take()
                .expect("static Profile generation retains every preparation proof");
            let application = bound.context.apply_prepared(proof);
            tokio::pin!(application);
            tokio::select! {
                biased;
                () = cancellation.cancelled() => None,
                result = &mut application => Some(result),
            }
        };
        let leaf = match outcome {
            None => {
                return rollback_generation(handle, ProfileError::Meta(MetaError::Cancelled)).await;
            }
            Some(Ok(leaf)) => leaf,
            Some(Err(_)) => {
                return rollback_generation(
                    handle,
                    ProfileError::Application {
                        instance: leaf_target.candidate.id().clone(),
                    },
                )
                .await;
            }
        };
        let error = match leaf.snapshot().state {
            FiberState::Active => None,
            FiberState::Pending(_) => Some(ProfileError::GenerationPending {
                instance: leaf_target.candidate.id().clone(),
            }),
            FiberState::Loading
            | FiberState::Failed(_)
            | FiberState::Unloading
            | FiberState::Disposed => Some(ProfileError::Application {
                instance: leaf_target.candidate.id().clone(),
            }),
        };
        if let Some(error) = error {
            return rollback_generation(handle, error).await;
        }
    }
    if cancellation.is_cancelled() {
        return rollback_generation(handle, ProfileError::Meta(MetaError::Cancelled)).await;
    }
    Ok(handle)
}

async fn rollback_generation(handle: FiberHandle, error: ProfileError) -> Result<FiberHandle> {
    let _cleanup = handle.dispose().await;
    Err(error)
}

#[derive(Debug)]
struct ProfileGenerationFactory {
    context: Arc<Mutex<Option<Context>>>,
    activated: AtomicBool,
}

#[async_trait]
impl PluginFactory for ProfileGenerationFactory {
    fn prepare(&self, desired: &ConfigValue) -> rsi_meta::Result<PreparedActivation> {
        if !desired.is_null() {
            return Err(MetaError::InvalidConfig(
                "static Profile generation configuration must be null".to_owned(),
            ));
        }
        Ok(PreparedActivation::new(ConfigValue::Null))
    }

    async fn activate(&self, plan: ActivationPlan) -> rsi_meta::Result<()> {
        if self.activated.swap(true, Ordering::AcqRel) {
            return Err(MetaError::Activation(
                "static Profile generation is single-use".to_owned(),
            ));
        }
        let Ok(mut context) = self.context.lock() else {
            return Err(MetaError::Activation(
                "static Profile generation Context is unavailable".to_owned(),
            ));
        };
        *context = Some(plan.context().clone());
        Ok(())
    }
}

#[derive(Debug)]
struct PreparedTarget {
    target: ResolvedTarget,
    prepared: Vec<Option<PreparedPlugin>>,
}

struct BoundTarget {
    target: ResolvedTarget,
    leaves: Vec<BoundLeaf>,
}

struct BoundLeaf {
    context: Context,
    prepared: Option<PreparedPlugin>,
}

#[derive(Clone, Debug)]
struct ActiveLeaf {
    resolved: ResolvedLeaf,
    handle: FiberHandle,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct WatchPlan {
    fingerprints: BTreeMap<PathBuf, [u8; 32]>,
    stamps: BTreeMap<PathBuf, SourceStamp>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct SourceStamp {
    length: u64,
    modified: Option<SystemTime>,
}

enum WatchProbe {
    MetadataUnchanged,
    ContentVerified(WatchPlan),
}

impl WatchPlan {
    fn capture(paths: &[PathBuf], limits: &ProfileLimits) -> Result<Self> {
        let mut fingerprints = BTreeMap::new();
        let mut stamps = BTreeMap::new();
        let mut total = 0_usize;
        for path in paths {
            let bytes =
                read_file_bounded(path, limits.maximum_document_bytes).map_err(|error| {
                    if error.kind() == std::io::ErrorKind::InvalidData {
                        ProfileError::CapacityExceeded {
                            resource: "document bytes",
                            maximum: limits.maximum_document_bytes,
                        }
                    } else {
                        ProfileError::Source {
                            message: "cannot read a required watched source".to_owned(),
                        }
                    }
                })?;
            total = total
                .checked_add(bytes.len())
                .ok_or(ProfileError::CapacityExceeded {
                    resource: "source bytes",
                    maximum: limits.maximum_source_bytes,
                })?;
            if total > limits.maximum_source_bytes {
                return Err(ProfileError::CapacityExceeded {
                    resource: "source bytes",
                    maximum: limits.maximum_source_bytes,
                });
            }
            let digest: [u8; 32] = Sha256::digest(&bytes).into();
            fingerprints.insert(path.clone(), digest);
            stamps.insert(path.clone(), source_stamp(path, limits)?);
        }
        Ok(Self {
            fingerprints,
            stamps,
        })
    }

    fn probe(
        baseline: &Self,
        limits: &ProfileLimits,
        force_content_audit: bool,
    ) -> Result<WatchProbe> {
        let paths = baseline.fingerprints.keys().cloned().collect::<Vec<_>>();
        let stamps = capture_stamps(&paths, limits)?;
        if !force_content_audit && stamps == baseline.stamps {
            return Ok(WatchProbe::MetadataUnchanged);
        }
        Self::capture(&paths, limits).map(WatchProbe::ContentVerified)
    }

    fn health(&self) -> WatcherHealth {
        if self.fingerprints.is_empty() {
            WatcherHealth::Inactive
        } else {
            WatcherHealth::Healthy
        }
    }

    fn establish(candidate: &ProfileCandidate, limits: &ProfileLimits) -> Result<Self> {
        let plan = Self::capture(candidate.watch_paths(), limits)?;
        if plan.fingerprints != candidate.source_fingerprints {
            return Err(ProfileError::Source {
                message: "a required source changed after Profile compilation".to_owned(),
            });
        }
        Ok(plan)
    }
}

fn source_stamp(path: &PathBuf, limits: &ProfileLimits) -> Result<SourceStamp> {
    let metadata = fs::symlink_metadata(path).map_err(|_| ProfileError::Source {
        message: "cannot read a required watched source".to_owned(),
    })?;
    if !metadata.file_type().is_file() {
        return Err(ProfileError::Source {
            message: "a required watched source is not a regular file".to_owned(),
        });
    }
    let length = metadata.len();
    if length > limits.maximum_document_bytes as u64 {
        return Err(ProfileError::CapacityExceeded {
            resource: "document bytes",
            maximum: limits.maximum_document_bytes,
        });
    }
    Ok(SourceStamp {
        length,
        modified: metadata.modified().ok(),
    })
}

fn capture_stamps(
    paths: &[PathBuf],
    limits: &ProfileLimits,
) -> Result<BTreeMap<PathBuf, SourceStamp>> {
    let mut stamps = BTreeMap::new();
    let mut total = 0_usize;
    for path in paths {
        let stamp = source_stamp(path, limits)?;
        let length = usize::try_from(stamp.length).map_err(|_| ProfileError::CapacityExceeded {
            resource: "source bytes",
            maximum: limits.maximum_source_bytes,
        })?;
        total = total
            .checked_add(length)
            .ok_or(ProfileError::CapacityExceeded {
                resource: "source bytes",
                maximum: limits.maximum_source_bytes,
            })?;
        if total > limits.maximum_source_bytes {
            return Err(ProfileError::CapacityExceeded {
                resource: "source bytes",
                maximum: limits.maximum_source_bytes,
            });
        }
        stamps.insert(path.clone(), stamp);
    }
    Ok(stamps)
}

#[cfg(test)]
mod watch_tests {
    use super::*;

    #[test]
    fn metadata_fast_path_and_forced_content_audit_are_distinct() {
        let temporary = tempfile::tempdir().unwrap();
        let path = temporary.path().join("profile.toml");
        fs::write(&path, b"format = 1\n").unwrap();
        let limits = ProfileLimits::default();
        let baseline = WatchPlan::capture(std::slice::from_ref(&path), &limits).unwrap();

        assert!(matches!(
            WatchPlan::probe(&baseline, &limits, false).unwrap(),
            WatchProbe::MetadataUnchanged
        ));
        fs::write(&path, b"format = 2\n").unwrap();
        let WatchProbe::ContentVerified(changed) =
            WatchPlan::probe(&baseline, &limits, true).unwrap()
        else {
            panic!("forced audit must hash content");
        };
        assert_ne!(changed.fingerprints, baseline.fingerprints);
    }
}

#[derive(Debug)]
struct Controller {
    runtime: Runtime,
    resolver: Arc<dyn ProfileResolver>,
    program: ProfileProgram,
    compiler: ProfileCompiler,
    state: Mutex<Option<ControllerState>>,
    reload_lock: AsyncMutex<()>,
    status_tx: watch::Sender<ProfileStatus>,
    membership_changed: Notify,
    dirty: AtomicBool,
    dirty_notify: Notify,
}

#[derive(Debug)]
struct ControllerState {
    revision: u64,
    health: ProfileHealth,
    watcher: WatcherHealth,
    diagnostic: Option<String>,
    context: Context,
    target: ResolvedTarget,
    converged_target: ResolvedTarget,
    active: Vec<ActiveLeaf>,
    watch_plan: WatchPlan,
}

impl Controller {
    fn new(
        runtime: Runtime,
        resolver: Arc<dyn ProfileResolver>,
        program: ProfileProgram,
        compiler: ProfileCompiler,
        initial: &ResolvedTarget,
    ) -> Self {
        let initial_status = ProfileStatus {
            revision: 0,
            health: ProfileHealth::Converging,
            watcher: WatcherHealth::Inactive,
            source_digest: initial.candidate.source_digest().to_owned(),
            target: target_status(initial),
            observed: Vec::new(),
            diagnostic: None,
        };
        let (status_tx, _) = watch::channel(initial_status);
        Self {
            runtime,
            resolver,
            program,
            compiler,
            state: Mutex::new(None),
            reload_lock: AsyncMutex::new(()),
            status_tx,
            membership_changed: Notify::new(),
            dirty: AtomicBool::new(false),
            dirty_notify: Notify::new(),
        }
    }

    async fn install(
        &self,
        context: Context,
        target: ResolvedTarget,
        active: Vec<ActiveLeaf>,
    ) -> Result<()> {
        let candidate = target.candidate.clone();
        let limits = self.compiler.limits.clone();
        let watch_plan =
            tokio::task::spawn_blocking(move || WatchPlan::establish(&candidate, &limits))
                .await
                .map_err(|_| {
                    ProfileError::InvalidProgram("Profile watcher task failed".to_owned())
                })??;
        let watcher = watch_plan.health();
        let mut state = self.state.lock().expect("Profile state poisoned");
        if state.is_some() {
            return Err(ProfileError::InvalidProgram(
                "Profile control activated more than once".to_owned(),
            ));
        }
        *state = Some(ControllerState {
            revision: 1,
            health: ProfileHealth::Converged,
            watcher,
            diagnostic: None,
            context,
            converged_target: target.clone(),
            target,
            active,
            watch_plan,
        });
        self.publish_locked(&mut state);
        Ok(())
    }

    fn status(&self) -> ProfileStatus {
        self.refresh_observed();
        self.status_tx.borrow().clone()
    }

    fn snapshot(&self) -> ProfileSnapshot {
        let state = self.state.lock().expect("Profile state poisoned");
        if let Some(state) = state.as_ref() {
            ProfileSnapshot {
                revision: state.revision,
                source_digest: state.target.candidate.source_digest().to_owned(),
                nodes: snapshot_nodes(&state.target.candidate.tree),
            }
        } else {
            ProfileSnapshot {
                revision: 0,
                source_digest: self.status_tx.borrow().source_digest.clone(),
                nodes: Vec::new(),
            }
        }
    }

    async fn reload(&self) -> Result<ReloadOutcome> {
        let _reload = self.reload_lock.lock().await;
        self.reload_serialized().await
    }

    async fn reload_serialized(&self) -> Result<ReloadOutcome> {
        if self
            .state
            .lock()
            .expect("Profile state poisoned")
            .as_ref()
            .is_none_or(|state| state.health == ProfileHealth::Stopped)
        {
            return Err(ProfileError::Stopped);
        }
        let (candidate, watch_plan) = match self.resolve_reload().await {
            Ok(result) => result,
            Err(error) => {
                self.publish_preflight_error(&error);
                return Err(error);
            }
        };

        let (revision, health, context, previous_target) = self.reload_base()?;

        if health == ProfileHealth::Converged {
            let equal = {
                let state = self.state.lock().expect("Profile state poisoned");
                semantic_equal(
                    &state.as_ref().expect("checked active state").target,
                    &candidate,
                )
            };
            if equal {
                let status = self.complete_unchanged(candidate, watch_plan);
                return Ok(ReloadOutcome::Unchanged(status));
            }
        }

        let needs_restart = {
            let state = self.state.lock().expect("Profile state poisoned");
            restart_required(
                &state
                    .as_ref()
                    .expect("checked active state")
                    .converged_target,
                &candidate,
            )
        };
        if needs_restart {
            let status = self.complete_restart(revision, candidate, watch_plan);
            return Ok(ReloadOutcome::RestartRequired(status));
        }

        let candidate = self.publish_pre_mutation_failure(bind_resolved_target(
            candidate,
            &context,
            self.resolver.as_ref(),
        ))?;
        let rollback = self.publish_pre_mutation_failure(bind_resolved_target(
            previous_target,
            &context,
            self.resolver.as_ref(),
        ))?;
        let candidate_target = candidate.target.clone();
        self.set_converging(&candidate.target);

        let mut active = {
            self.state
                .lock()
                .expect("Profile state poisoned")
                .as_ref()
                .expect("checked active state")
                .active
                .clone()
        };
        match self.converge_once(&mut active, candidate).await {
            Ok(target) => {
                let status = self.complete_applied(revision, target, active, watch_plan);
                Ok(ReloadOutcome::Applied(status))
            }
            Err(error) => {
                self.recover_failed_convergence(
                    revision,
                    candidate_target,
                    active,
                    rollback,
                    watch_plan,
                    error,
                )
                .await
            }
        }
    }

    async fn recover_failed_convergence(
        &self,
        revision: u64,
        candidate_target: ResolvedTarget,
        mut active: Vec<ActiveLeaf>,
        rollback: BoundTarget,
        watch_plan: WatchPlan,
        error: String,
    ) -> Result<ReloadOutcome> {
        let error = bound_message(error, self.compiler.limits.maximum_diagnostic_bytes);
        match self.converge_once(&mut active, rollback).await {
            Ok(restored) => {
                let status = self.complete_applied(revision, restored, active, watch_plan);
                Ok(ReloadOutcome::RolledBack { status, error })
            }
            Err(rollback_error) => {
                let rollback_error = bound_message(
                    rollback_error,
                    self.compiler.limits.maximum_diagnostic_bytes,
                );
                let status = self.complete_degraded(
                    revision,
                    candidate_target,
                    active,
                    error.clone(),
                    watch_plan,
                );
                Ok(ReloadOutcome::Degraded {
                    status,
                    error,
                    rollback_error,
                })
            }
        }
    }

    async fn resolve_reload(&self) -> Result<(ResolvedTarget, WatchPlan)> {
        let source_compiler = self.compiler.clone();
        let program = self.program.clone();
        let candidate = tokio::task::spawn_blocking(move || source_compiler.compile(&program))
            .await
            .map_err(|_| {
                ProfileError::InvalidProgram("Profile compiler task failed".to_owned())
            })??;
        let resolver = Arc::clone(&self.resolver);
        let limits = self.compiler.limits.clone();
        tokio::task::spawn_blocking(move || {
            let target = resolve_target(resolver.as_ref(), candidate)?;
            let watch_plan = WatchPlan::establish(&target.candidate, &limits)?;
            Ok::<_, ProfileError>((target, watch_plan))
        })
        .await
        .map_err(|_| ProfileError::InvalidProgram("Profile resolution task failed".to_owned()))?
    }

    fn reload_base(&self) -> Result<(u64, ProfileHealth, Context, ResolvedTarget)> {
        let state = self.state.lock().expect("Profile state poisoned");
        let state = state.as_ref().ok_or(ProfileError::Stopped)?;
        let revision = state
            .revision
            .checked_add(1)
            .ok_or_else(|| ProfileError::InvalidProgram("Profile revision exhausted".to_owned()))?;
        Ok((
            revision,
            state.health,
            state.context.clone(),
            state.converged_target.clone(),
        ))
    }

    async fn converge_once(
        &self,
        active: &mut Vec<ActiveLeaf>,
        mut candidate: BoundTarget,
    ) -> std::result::Result<ResolvedTarget, String> {
        let retained = retained_prefix(active, &candidate.target.leaves);
        while active.len() > retained {
            let removed = active.pop().expect("active suffix exists");
            self.remove_active_tail(&removed);
            let report = removed.handle.dispose().await;
            if !report.is_clean() {
                return Err(format!(
                    "disposing Profile instance `{}` reported {} cleanup failures",
                    removed.resolved.candidate.id(),
                    report.total_failures()
                ));
            }
        }
        for index in 0..retained {
            candidate.leaves[index].prepared.take();
        }
        for index in retained..candidate.target.leaves.len() {
            let resolved = candidate.target.leaves[index].clone();
            let prepared = match candidate.leaves[index].prepared.take() {
                Some(prepared) => prepared,
                None => self
                    .runtime
                    .prepare(
                        resolved.factory.clone(),
                        resolved.candidate.config().clone(),
                    )
                    .map_err(|_| {
                        ProfileError::Preparation {
                            instance: resolved.candidate.id().clone(),
                        }
                        .to_string()
                    })?,
            };
            let handle = candidate.leaves[index]
                .context
                .apply_prepared(prepared)
                .await
                .map_err(|_| {
                    ProfileError::Application {
                        instance: resolved.candidate.id().clone(),
                    }
                    .to_string()
                })?;
            let state = handle.snapshot().state;
            active.push(ActiveLeaf { resolved, handle });
            self.append_active_tail(active.last().expect("active leaf was appended"));
            if let Some(diagnostic) =
                settled_failure(candidate.target.leaves[index].candidate.id(), &state)
            {
                return Err(diagnostic);
            }
        }
        Ok(candidate.target)
    }

    fn set_converging(&self, target: &ResolvedTarget) {
        let mut state = self.state.lock().expect("Profile state poisoned");
        let state = state.as_mut().expect("checked active state");
        state.health = ProfileHealth::Converging;
        state.target = target.clone();
        state.diagnostic = None;
        self.publish_locked_state(state);
    }

    fn complete_unchanged(&self, target: ResolvedTarget, watch_plan: WatchPlan) -> ProfileStatus {
        let mut state = self.state.lock().expect("Profile state poisoned");
        let state = state.as_mut().expect("checked active state");
        state.target = target.clone();
        state.converged_target = target;
        state.watch_plan = watch_plan;
        state.watcher = state.watch_plan.health();
        state.diagnostic = None;
        self.publish_locked_state(state)
    }

    fn complete_restart(
        &self,
        revision: u64,
        target: ResolvedTarget,
        watch_plan: WatchPlan,
    ) -> ProfileStatus {
        let mut state = self.state.lock().expect("Profile state poisoned");
        let state = state.as_mut().expect("checked active state");
        state.revision = revision;
        state.health = ProfileHealth::RestartRequired;
        state.target = target;
        state.watch_plan = watch_plan;
        state.watcher = state.watch_plan.health();
        state.diagnostic = None;
        self.publish_locked_state(state)
    }

    fn complete_applied(
        &self,
        revision: u64,
        target: ResolvedTarget,
        active: Vec<ActiveLeaf>,
        watch_plan: WatchPlan,
    ) -> ProfileStatus {
        let status = {
            let mut state = self.state.lock().expect("Profile state poisoned");
            let state = state.as_mut().expect("checked active state");
            state.revision = revision;
            state.health = ProfileHealth::Converged;
            state.target = target.clone();
            state.converged_target = target;
            state.active = active;
            state.watch_plan = watch_plan;
            state.watcher = state.watch_plan.health();
            state.diagnostic = None;
            self.publish_locked_state(state)
        };
        self.membership_changed.notify_one();
        status
    }

    fn complete_degraded(
        &self,
        revision: u64,
        target: ResolvedTarget,
        active: Vec<ActiveLeaf>,
        diagnostic: String,
        watch_plan: WatchPlan,
    ) -> ProfileStatus {
        let status = {
            let mut state = self.state.lock().expect("Profile state poisoned");
            let state = state.as_mut().expect("checked active state");
            state.revision = revision;
            state.health = ProfileHealth::Degraded;
            state.target = target;
            state.active = active;
            state.diagnostic = Some(diagnostic);
            state.watch_plan = watch_plan;
            state.watcher = state.watch_plan.health();
            self.publish_locked_state(state)
        };
        self.membership_changed.notify_one();
        status
    }

    fn remove_active_tail(&self, removed: &ActiveLeaf) {
        let mut state = self.state.lock().expect("Profile state poisoned");
        if let Some(state) = state.as_mut() {
            let mirrored = state.active.pop().expect("active graph suffix exists");
            debug_assert_eq!(
                mirrored.resolved.candidate.id(),
                removed.resolved.candidate.id()
            );
        }
    }

    fn append_active_tail(&self, added: &ActiveLeaf) {
        let mut state = self.state.lock().expect("Profile state poisoned");
        if let Some(state) = state.as_mut() {
            state.active.push(added.clone());
        }
    }

    fn publish_preflight_error(&self, error: &ProfileError) {
        let mut state = self.state.lock().expect("Profile state poisoned");
        if let Some(state) = state.as_mut() {
            state.diagnostic = Some(bound_message(
                error.to_string(),
                self.compiler.limits.maximum_diagnostic_bytes,
            ));
            self.publish_locked_state(state);
        }
    }

    fn publish_pre_mutation_failure<T>(&self, result: Result<T>) -> Result<T> {
        if let Err(error) = &result {
            self.publish_preflight_error(error);
        }
        result
    }

    fn publish_watcher_error(&self, baseline: &WatchPlan) -> bool {
        const DIAGNOSTIC: &str = "a required watched Profile source is unavailable";
        let diagnostic = bound_message(
            DIAGNOSTIC.to_owned(),
            self.compiler.limits.maximum_diagnostic_bytes,
        );
        let mut state = self.state.lock().expect("Profile state poisoned");
        if let Some(state) = state.as_mut()
            && state.health != ProfileHealth::Stopped
            && state.health != ProfileHealth::Converging
            && &state.watch_plan == baseline
        {
            if state.watcher == WatcherHealth::Faulted
                && state.diagnostic.as_deref() == Some(diagnostic.as_str())
            {
                return false;
            }
            state.watcher = WatcherHealth::Faulted;
            state.diagnostic = Some(diagnostic);
            self.publish_locked_state(state);
            return true;
        }
        false
    }

    fn refresh_observed(&self) {
        let mut state = self.state.lock().expect("Profile state poisoned");
        let Some(state) = state.as_mut() else {
            return;
        };
        if state.health == ProfileHealth::Converging {
            return;
        }
        let failure = state.active.iter().find_map(|entry| {
            let snapshot = entry.handle.snapshot();
            settled_failure(entry.resolved.candidate.id(), &snapshot.state)
        });
        if let Some(diagnostic) = failure
            && state.health == ProfileHealth::Converged
        {
            state.health = ProfileHealth::Degraded;
            state.diagnostic = Some(bound_message(
                diagnostic,
                self.compiler.limits.maximum_diagnostic_bytes,
            ));
        }
        let status = status_from_state(state);
        if *self.status_tx.borrow() != status {
            self.status_tx.send_replace(status);
        }
    }

    fn publish_locked(&self, state: &mut Option<ControllerState>) {
        if let Some(state) = state.as_mut() {
            self.publish_locked_state(state);
        }
    }

    fn publish_locked_state(&self, state: &mut ControllerState) -> ProfileStatus {
        let status = status_from_state(state);
        self.status_tx.send_replace(status.clone());
        status
    }

    fn mark_dirty(&self) {
        if !self.dirty.swap(true, Ordering::AcqRel) {
            self.dirty_notify.notify_one();
        }
    }

    async fn poll_sources(self: &Arc<Self>, mut stop: watch::Receiver<bool>) {
        let mut ticks_since_content_audit = 0_usize;
        loop {
            tokio::select! {
                changed = stop.changed() => {
                    if changed.is_err() || *stop.borrow() {
                        return;
                    }
                }
                () = tokio::time::sleep(WATCH_INTERVAL) => {
                    ticks_since_content_audit = ticks_since_content_audit
                        .checked_add(1)
                        .unwrap_or(FULL_CONTENT_AUDIT_TICKS);
                    let force_periodic_audit =
                        ticks_since_content_audit >= FULL_CONTENT_AUDIT_TICKS;
                    if force_periodic_audit {
                        ticks_since_content_audit = 0;
                    }
                    let baseline = {
                        self.state.lock().expect("Profile state poisoned")
                            .as_ref().map(|state| {
                                (
                                    state.watch_plan.clone(),
                                    state.watcher == WatcherHealth::Faulted,
                                )
                            })
                    };
                    let Some((plan, was_faulted)) = baseline else { continue; };
                    let limits = self.compiler.limits.clone();
                    let probed_plan = plan.clone();
                    let current = tokio::task::spawn_blocking(move || {
                        WatchPlan::probe(
                            &probed_plan,
                            &limits,
                            force_periodic_audit || was_faulted,
                        )
                    }).await;
                    match current {
                        Ok(Ok(WatchProbe::ContentVerified(current)))
                            if current.fingerprints != plan.fingerprints || was_faulted =>
                        {
                            self.mark_dirty();
                        }
                        Ok(Ok(WatchProbe::ContentVerified(current))) => {
                            self.refresh_watch_plan(&plan, current);
                        }
                        Ok(Ok(WatchProbe::MetadataUnchanged)) => {}
                        Ok(Err(_)) | Err(_) => {
                            if self.publish_watcher_error(&plan) {
                                self.mark_dirty();
                            }
                        }
                    }
                }
            }
        }
    }

    fn refresh_watch_plan(&self, baseline: &WatchPlan, current: WatchPlan) {
        let mut state = self.state.lock().expect("Profile state poisoned");
        let Some(state) = state.as_mut() else {
            return;
        };
        if &state.watch_plan == baseline {
            state.watch_plan = current;
        }
    }

    async fn drive_dirty(self: &Arc<Self>, mut stop: watch::Receiver<bool>) {
        let mut consecutive_failures = 0_u32;
        loop {
            if self.dirty.swap(false, Ordering::AcqRel) {
                if self.reload().await.is_err() {
                    consecutive_failures = consecutive_failures.saturating_add(1);
                    tokio::select! {
                        changed = stop.changed() => {
                            if changed.is_err() || *stop.borrow() {
                                return;
                            }
                        }
                        () = tokio::time::sleep(automatic_reload_backoff(consecutive_failures)) => {}
                    }
                } else {
                    consecutive_failures = 0;
                }
                continue;
            }
            tokio::select! {
                changed = stop.changed() => {
                    if changed.is_err() || *stop.borrow() {
                        return;
                    }
                }
                () = self.dirty_notify.notified() => {}
            }
        }
    }

    async fn refresh_loop(self: &Arc<Self>, mut stop: watch::Receiver<bool>) {
        loop {
            let handles = self
                .state
                .lock()
                .expect("Profile state poisoned")
                .as_ref()
                .map(|state| {
                    state
                        .active
                        .iter()
                        .map(|entry| entry.handle.clone())
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default();
            let receivers = handles
                .into_iter()
                .map(|handle| handle.subscribe())
                .collect::<Vec<_>>();
            self.refresh_observed();
            let mut subscriptions = tokio::task::JoinSet::new();
            let child_changed = Arc::new(Notify::new());
            for mut receiver in receivers {
                let child_changed = Arc::clone(&child_changed);
                subscriptions.spawn(async move {
                    while receiver.changed().await.is_ok() {
                        child_changed.notify_one();
                    }
                    child_changed.notify_one();
                });
            }
            loop {
                if subscriptions.is_empty() {
                    tokio::select! {
                        stop_change = stop.changed() => {
                            if stop_change.is_err() || *stop.borrow() {
                                return;
                            }
                        }
                        () = self.membership_changed.notified() => break,
                        () = child_changed.notified() => self.refresh_observed(),
                    }
                } else {
                    tokio::select! {
                        stop_change = stop.changed() => {
                            if stop_change.is_err() || *stop.borrow() {
                                subscriptions.abort_all();
                                return;
                            }
                        }
                        () = self.membership_changed.notified() => break,
                        () = child_changed.notified() => self.refresh_observed(),
                        _ = subscriptions.join_next() => self.refresh_observed(),
                    }
                }
            }
            subscriptions.abort_all();
        }
    }

    fn has_watched_sources(&self) -> bool {
        self.state
            .lock()
            .expect("Profile state poisoned")
            .as_ref()
            .is_some_and(|state| !state.watch_plan.fingerprints.is_empty())
    }

    async fn stop(&self) {
        let _reload = self.reload_lock.lock().await;
        let mut state = self.state.lock().expect("Profile state poisoned");
        if let Some(state) = state.as_mut() {
            state.health = ProfileHealth::Stopped;
            self.publish_locked_state(state);
        }
    }
}

#[derive(Debug)]
struct StrongControl(Arc<Controller>);

#[async_trait]
impl ProfileControl for StrongControl {
    async fn reload(&self) -> Result<ReloadOutcome> {
        self.0.reload().await
    }

    fn status(&self) -> ProfileStatus {
        self.0.status()
    }

    fn snapshot(&self) -> ProfileSnapshot {
        self.0.snapshot()
    }

    fn subscribe(&self) -> watch::Receiver<ProfileStatus> {
        self.0.status_tx.subscribe()
    }
}

#[derive(Debug)]
struct WeakControl(Weak<Controller>);

#[async_trait]
impl ProfileControl for WeakControl {
    async fn reload(&self) -> Result<ReloadOutcome> {
        self.0
            .upgrade()
            .ok_or(ProfileError::Stopped)?
            .reload()
            .await
    }

    fn status(&self) -> ProfileStatus {
        self.0
            .upgrade()
            .map_or_else(stopped_status, |control| control.status())
    }

    fn snapshot(&self) -> ProfileSnapshot {
        self.0.upgrade().map_or_else(
            || ProfileSnapshot {
                revision: 0,
                source_digest: String::new(),
                nodes: Vec::new(),
            },
            |control| control.snapshot(),
        )
    }

    fn subscribe(&self) -> watch::Receiver<ProfileStatus> {
        self.0.upgrade().map_or_else(
            || watch::channel(stopped_status()).1,
            |control| control.status_tx.subscribe(),
        )
    }
}

#[derive(Debug)]
struct ProfileFactory {
    initial: Mutex<Option<PreparedTarget>>,
    control: Weak<Controller>,
}

#[async_trait]
impl PluginFactory for ProfileFactory {
    fn prepare(&self, desired: &ConfigValue) -> rsi_meta::Result<PreparedActivation> {
        if !desired.is_null() {
            return Err(MetaError::InvalidConfig(
                "Profile bootstrap configuration must be null".to_owned(),
            ));
        }
        Ok(PreparedActivation::new(ConfigValue::Null))
    }

    async fn activate(&self, plan: ActivationPlan) -> rsi_meta::Result<()> {
        let control = self.control.upgrade().ok_or_else(|| {
            MetaError::Activation("Profile control owner was dropped before activation".to_owned())
        })?;
        let prepared = self
            .initial
            .lock()
            .expect("initial Profile target poisoned")
            .take()
            .ok_or_else(|| MetaError::Activation("Profile bootstrap is single-use".to_owned()))?;
        let candidate = bind_target(prepared, plan.context(), control.resolver.as_ref())
            .map_err(|error| MetaError::Activation(error.to_string()))?;

        let tasks = Arc::new(TaskOwner::new());
        let cleanup_tasks = Arc::clone(&tasks);
        let weak = Arc::downgrade(&control);
        plan.defer(
            "stop Profile control and watcher",
            Box::new(move || {
                Box::pin(async move {
                    cleanup_tasks.stop.send_replace(true);
                    if let Some(control) = weak.upgrade() {
                        control.stop().await;
                    }
                    let handles = cleanup_tasks
                        .handles
                        .lock()
                        .expect("Profile task owner poisoned")
                        .take()
                        .unwrap_or_default();
                    for handle in handles {
                        let _ = handle.await;
                    }
                    Ok(())
                })
            }),
        )?;

        let service: Arc<dyn ProfileControl> = Arc::new(WeakControl(Arc::downgrade(&control)));
        let _control_supply = plan
            .context()
            .provide_local::<ProfileControlContract>(service)?;

        let mut active = Vec::new();
        let target = apply_initial(&mut active, candidate)
            .await
            .map_err(MetaError::Activation)?;
        control
            .install(plan.context().clone(), target, active)
            .await
            .map_err(|error| MetaError::Activation(error.to_string()))?;

        let mut handles = Vec::new();
        if control.has_watched_sources() {
            handles.push(tokio::spawn({
                let control = Arc::clone(&control);
                let stop = tasks.stop.subscribe();
                async move { control.poll_sources(stop).await }
            }));
            handles.push(tokio::spawn({
                let control = Arc::clone(&control);
                let stop = tasks.stop.subscribe();
                async move { control.drive_dirty(stop).await }
            }));
        }
        handles.push(tokio::spawn({
            let control = Arc::clone(&control);
            let stop = tasks.stop.subscribe();
            async move { control.refresh_loop(stop).await }
        }));
        *tasks.handles.lock().expect("Profile task owner poisoned") = Some(handles);
        Ok(())
    }
}

#[derive(Debug)]
struct TaskOwner {
    stop: watch::Sender<bool>,
    handles: Mutex<Option<Vec<tokio::task::JoinHandle<()>>>>,
}

impl TaskOwner {
    fn new() -> Self {
        let (stop, _) = watch::channel(false);
        Self {
            stop,
            handles: Mutex::new(None),
        }
    }
}

async fn apply_initial(
    active: &mut Vec<ActiveLeaf>,
    mut candidate: BoundTarget,
) -> std::result::Result<ResolvedTarget, String> {
    for index in 0..candidate.target.leaves.len() {
        let resolved = candidate.target.leaves[index].clone();
        let prepared = candidate.leaves[index]
            .prepared
            .take()
            .expect("initial candidate retains every preparation proof");
        let handle = candidate.leaves[index]
            .context
            .apply_prepared(prepared)
            .await
            .map_err(|_| {
                ProfileError::Application {
                    instance: resolved.candidate.id().clone(),
                }
                .to_string()
            })?;
        let state = handle.snapshot().state;
        active.push(ActiveLeaf { resolved, handle });
        if let Some(diagnostic) =
            settled_failure(candidate.target.leaves[index].candidate.id(), &state)
        {
            return Err(diagnostic);
        }
    }
    Ok(candidate.target)
}

fn prepare_new_target(
    runtime: &Runtime,
    resolver: &dyn ProfileResolver,
    candidate: ProfileCandidate,
) -> Result<PreparedTarget> {
    let target = resolve_target(resolver, candidate)?;
    let prepared = target
        .leaves
        .iter()
        .map(|leaf| {
            let proof = runtime
                .prepare(leaf.factory.clone(), leaf.candidate.config().clone())
                .map_err(|_| ProfileError::Preparation {
                    instance: leaf.candidate.id().clone(),
                })?;
            Ok(Some(proof))
        })
        .collect::<Result<Vec<_>>>()?;
    Ok(PreparedTarget { target, prepared })
}

fn resolve_target(
    resolver: &dyn ProfileResolver,
    candidate: ProfileCandidate,
) -> Result<ResolvedTarget> {
    let leaves = candidate
        .leaves()
        .iter()
        .map(|leaf| {
            let factory = resolver.resolve(leaf.plugin())?;
            let identity = factory.identity().clone();
            let update_mode = factory.update_mode();
            Ok(ResolvedLeaf {
                candidate: leaf.clone(),
                factory,
                identity,
                update_mode,
            })
        })
        .collect::<Result<Vec<_>>>()?;
    Ok(ResolvedTarget { candidate, leaves })
}

fn bind_resolved_target(
    target: ResolvedTarget,
    base: &Context,
    resolver: &dyn ProfileResolver,
) -> Result<BoundTarget> {
    let prepared = (0..target.leaves.len()).map(|_| None).collect();
    bind_target_parts(target, prepared, base, resolver)
}

fn bind_target(
    prepared: PreparedTarget,
    base: &Context,
    resolver: &dyn ProfileResolver,
) -> Result<BoundTarget> {
    bind_target_parts(prepared.target, prepared.prepared, base, resolver)
}

fn bind_target_parts(
    target: ResolvedTarget,
    mut prepared: Vec<Option<PreparedPlugin>>,
    base: &Context,
    resolver: &dyn ProfileResolver,
) -> Result<BoundTarget> {
    let mut contexts = BTreeMap::<Vec<String>, Context>::new();
    let mut leaves = Vec::with_capacity(target.leaves.len());
    for (index, leaf) in target.leaves.iter().enumerate() {
        let mut context = base.clone();
        for (depth, isolation) in leaf.candidate.isolations().iter().enumerate() {
            let key = leaf.candidate.groups()[..=depth].to_vec();
            if let Some(existing) = contexts.get(&key) {
                context = existing.clone();
                continue;
            }
            context = resolver.isolate(context, isolation)?;
            contexts.insert(key, context.clone());
        }
        leaves.push(BoundLeaf {
            context,
            prepared: prepared[index].take(),
        });
    }
    Ok(BoundTarget { target, leaves })
}

fn retained_prefix(active: &[ActiveLeaf], target: &[ResolvedLeaf]) -> usize {
    active
        .iter()
        .zip(target)
        .take_while(|(active, target)| resolved_leaf_equal(&active.resolved, target))
        .count()
}

fn semantic_equal(left: &ResolvedTarget, right: &ResolvedTarget) -> bool {
    left.leaves.len() == right.leaves.len()
        && left
            .leaves
            .iter()
            .zip(&right.leaves)
            .all(|(left, right)| resolved_leaf_equal(left, right))
}

fn resolved_leaf_equal(left: &ResolvedLeaf, right: &ResolvedLeaf) -> bool {
    left.candidate == right.candidate
        && left.identity == right.identity
        && left.update_mode == right.update_mode
}

fn restart_required(left: &ResolvedTarget, right: &ResolvedTarget) -> bool {
    let maximum = left.leaves.len().max(right.leaves.len());
    (0..maximum).any(
        |index| match (left.leaves.get(index), right.leaves.get(index)) {
            (Some(left), Some(right)) if resolved_leaf_equal(left, right) => false,
            (Some(left), Some(right)) => {
                left.update_mode == UpdateMode::RestartRequired
                    || right.update_mode == UpdateMode::RestartRequired
            }
            (Some(left), None) => left.update_mode == UpdateMode::RestartRequired,
            (None, Some(right)) => right.update_mode == UpdateMode::RestartRequired,
            (None, None) => false,
        },
    )
}

fn target_status(target: &ResolvedTarget) -> Vec<ProfileTargetStatus> {
    target
        .leaves
        .iter()
        .map(|leaf| ProfileTargetStatus {
            id: leaf.candidate.id().clone(),
            factory: leaf.identity.clone(),
        })
        .collect()
}

fn settled_failure(instance: &rsi_meta::InstanceId, state: &FiberState) -> Option<String> {
    match state {
        FiberState::Failed(_) => Some(
            ProfileError::Application {
                instance: instance.clone(),
            }
            .to_string(),
        ),
        FiberState::Disposed => Some(
            ProfileError::UnexpectedDisposal {
                instance: instance.clone(),
            }
            .to_string(),
        ),
        FiberState::Pending(_)
        | FiberState::Loading
        | FiberState::Active
        | FiberState::Unloading => None,
    }
}

fn redacted_instance_state(state: FiberState) -> ProfileInstanceState {
    match state {
        FiberState::Pending(report) => ProfileInstanceState::Pending(report),
        FiberState::Loading => ProfileInstanceState::Loading,
        FiberState::Active => ProfileInstanceState::Active,
        FiberState::Failed(_) => ProfileInstanceState::Failed,
        FiberState::Unloading => ProfileInstanceState::Unloading,
        FiberState::Disposed => ProfileInstanceState::Disposed,
    }
}

fn status_from_state(state: &ControllerState) -> ProfileStatus {
    ProfileStatus {
        revision: state.revision,
        health: state.health,
        watcher: state.watcher,
        source_digest: state.target.candidate.source_digest().to_owned(),
        target: target_status(&state.target),
        observed: state
            .active
            .iter()
            .map(|entry| {
                let snapshot = entry.handle.snapshot();
                ProfileInstanceStatus {
                    id: entry.resolved.candidate.id().clone(),
                    factory: snapshot.factory,
                    state: redacted_instance_state(snapshot.state),
                }
            })
            .collect(),
        diagnostic: state.diagnostic.clone(),
    }
}

fn snapshot_nodes(nodes: &[TreeNode]) -> Vec<SnapshotNode> {
    nodes
        .iter()
        .map(|node| match node {
            TreeNode::Group(group) => SnapshotNode {
                id: group.id.clone(),
                plugin: None,
                enabled: group.enabled,
                children: snapshot_nodes(&group.children),
            },
            TreeNode::Plugin(plugin) => SnapshotNode {
                id: plugin.id.as_str().to_owned(),
                plugin: Some(plugin.plugin.clone()),
                enabled: plugin.enabled,
                children: Vec::new(),
            },
        })
        .collect()
}

fn stopped_status() -> ProfileStatus {
    ProfileStatus {
        revision: 0,
        health: ProfileHealth::Stopped,
        watcher: WatcherHealth::Inactive,
        source_digest: String::new(),
        target: Vec::new(),
        observed: Vec::new(),
        diagnostic: None,
    }
}

#[cfg(test)]
mod tests {
    use super::{ProfileInstanceState, redacted_instance_state, settled_failure};
    use rsi_meta::{FiberState, InstanceId};

    #[test]
    fn failed_runtime_state_is_projected_without_its_diagnostic() {
        let instance = InstanceId::new("child");
        let runtime_state = FiberState::Failed("secret plugin diagnostic".to_owned());

        let state = redacted_instance_state(runtime_state.clone());
        let diagnostic = settled_failure(&instance, &runtime_state).unwrap();

        assert_eq!(state, ProfileInstanceState::Failed);
        assert!(!format!("{state:?}").contains("secret"));
        assert!(!diagnostic.contains("secret"));
        assert_eq!(diagnostic, "applying Profile instance `child` failed");
    }
}
