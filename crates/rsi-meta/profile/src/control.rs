use super::{
    CandidateLeaf, ProfileCandidate, ProfileCompiler, ProfileEnvironment, ProfileError,
    ProfileLimits, ProfileProgram, Result, TreeNode, bound_message,
};
mod attempt;
mod bindings;
mod updates;
use async_trait::async_trait;
use attempt::ConvergenceFailure;
pub use attempt::{
    ProfileAttempt, ProfileAttemptOrigin, ProfileAttemptOutcome, ProfileFailureKind,
};
use bindings::{BindingSnapshot, EffectiveBindings, Namespace};
use futures_util::FutureExt as _;
use futures_util::stream::{FuturesUnordered, StreamExt as _};
use updates::Command;
pub use updates::{ProfileInput, ProfileUpdateHandle, ProfileUpdateTicket};
#[cfg(not(target_family = "wasm"))]
mod native_watch;
#[cfg(not(target_family = "wasm"))]
use native_watch::{WatchPlan, WatchProbe};

#[cfg(target_family = "wasm")]
#[derive(Clone, Debug, Eq, PartialEq)]
struct WatchPlan;
#[cfg(target_family = "wasm")]
impl WatchPlan {
    #[allow(clippy::unused_self)] // The shared controller observes either a native or immutable plan.
    fn health(&self) -> WatcherHealth {
        WatcherHealth::Inactive
    }
    fn establish(candidate: &ProfileCandidate, _: &ProfileLimits) -> Result<Self> {
        if !candidate.watch_paths().is_empty() || !candidate.source_fingerprints.is_empty() {
            return Err(ProfileError::InvalidProgram(
                "native watching is unavailable in a Worker".into(),
            ));
        }
        Ok(Self)
    }
}

use rsi_meta::{
    ActivationPlan, ConfigValue, Context, FactoryIdentity, FiberHandle, FiberState, LocalContract,
    MetaError, PendingReport, PluginFactory, PluginId, PreparedActivation, PreparedPlugin,
    ResolvedFactory, Runtime, UpdateMode,
};
use std::collections::BTreeMap;
use std::fmt;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, Weak};
#[cfg(not(target_family = "wasm"))]
use std::time::Duration;
use tokio::sync::{Mutex as AsyncMutex, Notify, mpsc, watch};
use tokio_util::sync::CancellationToken;

#[cfg(not(target_family = "wasm"))]
const WATCH_INTERVAL: Duration = Duration::from_millis(100);
#[cfg(not(target_family = "wasm"))]
const FULL_CONTENT_AUDIT_TICKS: usize = 50;
#[cfg(not(target_family = "wasm"))]
const MINIMUM_AUTOMATIC_RELOAD_BACKOFF: Duration = Duration::from_secs(1);
#[cfg(not(target_family = "wasm"))]
const MAXIMUM_AUTOMATIC_RELOAD_BACKOFF: Duration = Duration::from_secs(5);

#[cfg(not(target_family = "wasm"))]
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

    /// Verifies that a successor preserves every registered nominal Local/event marker.
    /// Resolvers must opt in with a complete check; ordinary reload needs no successor.
    fn validate_successor(&self, _successor: &dyn ProfileResolver) -> Result<()> {
        Err(ProfileError::IncompatibleInput(
            "resolver does not support input replacement".to_owned(),
        ))
    }

    /// Resolves a stable Local key to its frozen nominal marker; never allocates isolation.
    fn local_contract_type(&self, key: &str) -> Result<std::any::TypeId> {
        Err(ProfileError::UnknownLocalContract {
            key: key.to_owned(),
        })
    }

    /// Resolves a stable event key to its frozen nominal marker; never allocates isolation.
    fn local_event_type(&self, key: &str) -> Result<std::any::TypeId> {
        Err(ProfileError::UnknownLocalEvent {
            key: key.to_owned(),
        })
    }
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

/// Bounded local status published after Profile state changes.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProfileStatus {
    revision: u64,
    health: ProfileHealth,
    watcher: WatcherHealth,
    source_digest: String,
    target: Arc<[ProfileTargetStatus]>,
    observed: Vec<ProfileInstanceStatus>,
    diagnostic: Option<String>,
    last_attempt: Option<ProfileAttempt>,
}

impl ProfileStatus {
    /// Last command completed by the serialized worker, absent at bootstrap.
    pub fn last_attempt(&self) -> Option<&ProfileAttempt> {
        self.last_attempt.as_ref()
    }
    /// Monotonic version of the complete published observation.
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

    /// Latest bounded local operational diagnostic, including plugin errors.
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
    pub(super) fn from_candidate(candidate: &ProfileCandidate) -> Self {
        Self {
            revision: 0,
            source_digest: candidate.source_digest.clone(),
            nodes: snapshot_nodes(&candidate.tree),
        }
    }

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
    /// Candidate accepted; successful leaves remain while failed leaves can be retried.
    Partial {
        /// Actually observed candidate graph.
        status: ProfileStatus,
        /// First bounded local leaf failure.
        error: String,
        /// Product-neutral leaf failure category.
        error_kind: ProfileFailureKind,
    },
    /// The healthy semantic tree was equal and revision did not advance.
    Unchanged(ProfileStatus),
    /// No Runtime mutation occurred because a changed leaf requires restart.
    RestartRequired(ProfileStatus),
    /// Candidate application failed and the prior target was reconstructed.
    RolledBack {
        /// Restored converged status.
        status: ProfileStatus,
        /// Bounded local candidate failure.
        error: String,
        /// Product-neutral candidate failure category.
        error_kind: ProfileFailureKind,
    },
    /// Candidate application and compensation both failed.
    Degraded {
        /// Actually observed partial graph.
        status: ProfileStatus,
        /// Bounded local candidate failure.
        error: String,
        /// Bounded local compensation failure.
        rollback_error: String,
        /// Product-neutral candidate failure category.
        error_kind: ProfileFailureKind,
        /// Product-neutral compensation failure category.
        rollback_error_kind: ProfileFailureKind,
    },
}

impl ReloadOutcome {
    /// Status published by this completed attempt.
    pub const fn status(&self) -> &ProfileStatus {
        match self {
            Self::Applied(status)
            | Self::Partial { status, .. }
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
    /// Compiles and resolves every enabled initial leaf before activation.
    pub fn prepare(
        runtime: &Runtime,
        resolver: Arc<dyn ProfileResolver>,
        program: ProfileProgram,
        environment: ProfileEnvironment,
        limits: ProfileLimits,
    ) -> Result<Self> {
        Self::prepare_input(
            runtime,
            ProfileInput::new(resolver, program, environment, limits),
        )
    }

    /// Prepares an immutable complete input for one ordinary Profile activation.
    pub fn prepare_input(runtime: &Runtime, input: ProfileInput) -> Result<Self> {
        let candidate = input.compiler.compile(&input.program)?;
        let target = resolve_target(input.resolver.as_ref(), candidate)?;
        let control = Arc::new(Controller::new(runtime, input, &target));
        let factory = Arc::new(ProfileFactory {
            initial: Mutex::new(Some(target)),
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

    /// Grants input replacement to the composition owner, separate from published control.
    pub fn updater(&self) -> ProfileUpdateHandle {
        ProfileUpdateHandle::new(Arc::clone(&self.control))
    }
}

#[derive(Clone)]
struct ResolvedLeaf {
    candidate: Arc<CandidateLeaf>,
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
    candidate: Arc<ProfileCandidate>,
    leaves: Vec<ResolvedLeaf>,
    bindings: BindingSnapshot,
    status: Arc<[ProfileTargetStatus]>,
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
        let state = handle.snapshot().state;
        if !matches!(state, FiberState::Active) {
            let _cleanup = handle.dispose().await;
            return Err(first_instance.map_or_else(
                || ProfileError::InvalidProgram("static Profile generation failed".to_owned()),
                |instance| ProfileError::Application {
                    instance,
                    reason: format!("generation wrapper settled as {state:?}"),
                },
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

async fn prepare_leaf(
    leaf: &ResolvedLeaf,
    runtime: &Runtime,
    cancellation: Option<&CancellationToken>,
) -> Result<PreparedPlugin> {
    if cancellation.is_some_and(CancellationToken::is_cancelled) {
        return Err(ProfileError::Meta(MetaError::Cancelled));
    }
    let instance = leaf.candidate.id().clone();
    let runtime = runtime.clone();
    let factory = leaf.factory.clone();
    let config = leaf.candidate.config().clone();
    let mut task = runtime
        .execution()
        .clone()
        .prepare(move || runtime.prepare(factory, config));
    let joined = if let Some(cancellation) = cancellation {
        tokio::select! {
            biased;
            () = cancellation.cancelled() => {
                let _completion = task.await;
                return Err(ProfileError::Meta(MetaError::Cancelled));
            }
            joined = &mut task => joined,
        }
    } else {
        task.await
    };
    joined
        .map_err(|error| ProfileError::Preparation {
            instance: instance.clone(),
            reason: error.to_string(),
        })?
        .map_err(|error| {
            ProfileFailureKind::with_diagnostic(error, |reason| ProfileError::Preparation {
                instance,
                reason,
            })
        })
}

async fn prepare_generation(
    target: ResolvedTarget,
    runtime: &Runtime,
    cancellation: &CancellationToken,
) -> Result<PreparedTarget> {
    let mut prepared = Vec::with_capacity(target.leaves.len());
    for leaf in &target.leaves {
        let proof = prepare_leaf(leaf, runtime, Some(cancellation)).await?;
        prepared.push(proof);
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
    let PreparedTarget { target, prepared } = prepared;
    let candidate = match bind_resolved_target(
        target,
        wrapper_context,
        resolver,
        &BindingSnapshot::default(),
    ) {
        Ok(candidate) => candidate,
        Err(error) => return rollback_generation(handle, error).await,
    };
    if let Err(error) = publish_order(&candidate) {
        return rollback_generation(handle, error).await;
    }
    for (index, (bound, proof)) in candidate.leaves.into_iter().zip(prepared).enumerate() {
        let leaf_target = candidate.target.leaves[index].clone();
        if cancellation.is_cancelled() {
            return rollback_generation(handle, ProfileError::Meta(MetaError::Cancelled)).await;
        }
        let outcome = {
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
            Some(Err(error)) => {
                return rollback_generation(
                    handle,
                    ProfileError::Application {
                        instance: leaf_target.candidate.id().clone(),
                        reason: error.to_string(),
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
            FiberState::Failed(reason) => Some(ProfileError::Application {
                instance: leaf_target.candidate.id().clone(),
                reason,
            }),
            state @ (FiberState::Loading | FiberState::Unloading | FiberState::Disposed) => {
                Some(ProfileError::Application {
                    instance: leaf_target.candidate.id().clone(),
                    reason: format!("child settled as {state:?}"),
                })
            }
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
    prepared: Vec<PreparedPlugin>,
}

struct BoundTarget {
    parent: Context,
    target: ResolvedTarget,
    leaves: Vec<BoundLeaf>,
}

struct RollbackTarget {
    bound: BoundTarget,
    desired: ResolvedTarget,
    health: ProfileHealth,
    diagnostic: Option<String>,
}

struct BoundLeaf {
    context: Context,
    bindings: EffectiveBindings,
}

impl BoundLeaf {
    async fn activate(&self, resolved: &ResolvedLeaf) -> Result<FiberHandle> {
        let prepared = prepare_leaf(resolved, self.context.runtime(), None).await?;
        self.context
            .apply_prepared(prepared)
            .await
            .map_err(|error| {
                ProfileFailureKind::with_diagnostic(error, |reason| ProfileError::Application {
                    instance: resolved.candidate.id().clone(),
                    reason,
                })
            })
    }
}

#[derive(Clone, Debug)]
struct ActiveLeaf {
    resolved: ResolvedLeaf,
    handle: FiberHandle,
    position: rsi_meta::ChildPosition,
    bindings: EffectiveBindings,
}

#[derive(Debug)]
struct Controller {
    execution: rsi_meta::Execution,
    source_watch: crate::SourceWatchPolicy,
    input: Mutex<(u64, Option<ProfileInput>)>,
    retired_snapshot: Mutex<Option<ProfileSnapshot>>,
    limits: ProfileLimits,
    commands: mpsc::Sender<Command>,
    receiver: Mutex<Option<mpsc::Receiver<Command>>>,
    accepting: AtomicBool,
    command_stop: CancellationToken,
    state: Mutex<Option<ControllerState>>,
    reload_lock: AsyncMutex<()>,
    status_tx: watch::Sender<ProfileStatus>,
    membership_changed: Notify,
    #[cfg(not(target_family = "wasm"))]
    dirty: AtomicBool,
    #[cfg(not(target_family = "wasm"))]
    dirty_notify: Notify,
}

#[derive(Debug)]
struct ControllerState {
    revision: u64,
    health: ProfileHealth,
    watcher: WatcherHealth,
    diagnostic: Option<String>,
    last_attempt: Option<ProfileAttempt>,
    context: Context,
    target: ResolvedTarget,
    converged_target: ResolvedTarget,
    input_restart_required: bool,
    active: Vec<ActiveLeaf>,
    watch_plan: WatchPlan,
}

impl Controller {
    fn watcher_health(&self, plan: &WatchPlan) -> WatcherHealth {
        if self.source_watch.enabled() {
            plan.health()
        } else {
            WatcherHealth::Inactive
        }
    }
    fn new(runtime: &Runtime, input: ProfileInput, initial: &ResolvedTarget) -> Self {
        let initial_status = ProfileStatus {
            revision: 0,
            health: ProfileHealth::Converging,
            watcher: WatcherHealth::Inactive,
            source_digest: initial.candidate.source_digest().to_owned(),
            target: initial.status.clone(),
            observed: Vec::new(),
            diagnostic: None,
            last_attempt: None,
        };
        let (status_tx, _) = watch::channel(initial_status);
        let (commands, receiver) = mpsc::channel(1);
        Self {
            execution: runtime.execution().clone(),
            source_watch: input.program.source_watch.clone(),
            limits: input.compiler.limits.clone(),
            input: Mutex::new((1, Some(input))),
            retired_snapshot: Mutex::new(None),
            commands,
            receiver: Mutex::new(Some(receiver)),
            accepting: AtomicBool::new(false),
            command_stop: CancellationToken::new(),
            state: Mutex::new(None),
            reload_lock: AsyncMutex::new(()),
            status_tx,
            membership_changed: Notify::new(),
            #[cfg(not(target_family = "wasm"))]
            dirty: AtomicBool::new(false),
            #[cfg(not(target_family = "wasm"))]
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
        let limits = self.limits.clone();
        let watch_plan = self
            .execution
            .prepare(move || WatchPlan::establish(&candidate, &limits))
            .await
            .map_err(|_| {
                ProfileError::InvalidProgram("Profile watcher task failed".to_owned())
            })??;
        let watcher = self.watcher_health(&watch_plan);
        let mut state = self.state.lock().expect("Profile state poisoned");
        if state.is_some() {
            return Err(ProfileError::InvalidProgram(
                "Profile control activated more than once".to_owned(),
            ));
        }
        *state = Some(ControllerState {
            revision: 1,
            health: ProfileHealth::Converging,
            watcher,
            diagnostic: None,
            last_attempt: None,
            context,
            converged_target: target.clone(),
            input_restart_required: false,
            target,
            active,
            watch_plan,
        });
        Ok(())
    }

    async fn start_initial(
        &self,
        context: Context,
        candidate: BoundTarget,
    ) -> rsi_meta::Result<()> {
        let mut active = Vec::new();
        let target = candidate.target.clone();
        self.install(context, target.clone(), Vec::new())
            .await
            .map_err(|error| MetaError::Activation(error.to_string()))?;
        let failure = self.converge_once(&mut active, candidate, true).await.err();
        let watch_plan = self
            .state
            .lock()
            .expect("Profile state poisoned")
            .as_ref()
            .expect("installed Profile state")
            .watch_plan
            .clone();
        match failure {
            Some(error) => {
                self.complete_partial(1, target, active, watch_plan, false, error);
            }
            None => {
                self.complete_applied(1, target, active, watch_plan, false, None);
            }
        }

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
            self.retired_snapshot
                .lock()
                .expect("retired Profile snapshot poisoned")
                .clone()
                .unwrap_or_else(|| ProfileSnapshot {
                    revision: 0,
                    source_digest: self.status_tx.borrow().source_digest.clone(),
                    nodes: Vec::new(),
                })
        }
    }

    async fn reload(&self) -> Result<ReloadOutcome> {
        self.reload_from(ProfileAttemptOrigin::Manual).await
    }

    async fn reload_from(&self, origin: ProfileAttemptOrigin) -> Result<ReloadOutcome> {
        self.submit_command(None, origin)?.wait().await
    }

    #[allow(clippy::too_many_lines)] // Preflight and both failure policies share one owned transaction.
    async fn reload_serialized(
        &self,
        input: &ProfileInput,
        replacement: bool,
    ) -> Result<ReloadOutcome> {
        if self
            .state
            .lock()
            .expect("Profile state poisoned")
            .as_ref()
            .is_none_or(|state| state.health == ProfileHealth::Stopped)
        {
            return Err(ProfileError::Stopped);
        }
        let (candidate, watch_plan) = match self.resolve_reload(input).await {
            Ok(result) => result,
            Err(error) => {
                self.publish_preflight_error(&error);
                return Err(error);
            }
        };

        let (revision, health, context, previous_target) = self.reload_base()?;

        let candidate = self.publish_pre_mutation_failure(bind_resolved_target(
            candidate,
            &context,
            input.resolver.as_ref(),
            &previous_target.bindings,
        ))?;
        if matches!(
            health,
            ProfileHealth::Converged | ProfileHealth::RestartRequired
        ) && semantic_equal(&previous_target, &candidate.target)
        {
            let status = self.complete_unchanged(candidate.target, watch_plan, replacement);
            return Ok(if status.health() == ProfileHealth::RestartRequired {
                ReloadOutcome::RestartRequired(status)
            } else {
                ReloadOutcome::Unchanged(status)
            });
        }
        if restart_required(&previous_target, &candidate.target) {
            let status = self.complete_restart(candidate.target, watch_plan, replacement);
            return Ok(ReloadOutcome::RestartRequired(status));
        }

        let rollback = replacement
            .then(|| self.prepare_rollback(&context))
            .transpose()?;
        let candidate_target = candidate.target.clone();
        let acknowledge_restart =
            replacement || !semantic_equal(&previous_target, &candidate_target);
        // Replacement failure retains the previous complete input, so its
        // watcher must retain that input's sources as well. A file reload still
        // watches the candidate sources, allowing the operator to repair them.
        let rollback_watch = if replacement {
            self.state
                .lock()
                .expect("Profile state poisoned")
                .as_ref()
                .expect("checked active state")
                .watch_plan
                .clone()
        } else {
            watch_plan.clone()
        };
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
        match self
            .converge_once(&mut active, candidate, !replacement)
            .await
        {
            Ok(target) => {
                let status = self.complete_applied(
                    revision,
                    target,
                    active,
                    watch_plan,
                    acknowledge_restart,
                    None,
                );
                Ok(ReloadOutcome::Applied(status))
            }
            Err(error) => {
                let Some(rollback) = rollback else {
                    return Ok(self.complete_partial(
                        revision,
                        candidate_target,
                        active,
                        watch_plan,
                        acknowledge_restart,
                        error,
                    ));
                };
                self.recover_failed_convergence(
                    revision,
                    candidate_target,
                    active,
                    rollback,
                    rollback_watch,
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
        rollback: RollbackTarget,
        watch_plan: WatchPlan,
        error: ConvergenceFailure,
    ) -> Result<ReloadOutcome> {
        let error_kind = error.kind;
        let error = bound_message(error.message, self.limits.maximum_diagnostic_bytes);
        match self.converge_once(&mut active, rollback.bound, false).await {
            Ok(_) => {
                // Compensation restores the prior bindings and health. Preserve
                // converged_target: a failed candidate never becomes a completed target.
                let status = {
                    let mut state = self.state.lock().expect("Profile state poisoned");
                    let state = state.as_mut().expect("checked active state");
                    state.revision = revision;
                    state.target = rollback.desired;
                    state.health = rollback.health;
                    state.diagnostic = rollback.diagnostic;
                    state.active = active;
                    state.watch_plan = watch_plan;
                    state.watcher = self.watcher_health(&state.watch_plan);
                    self.publish_locked_state(state)
                };
                self.membership_changed.notify_one();
                Ok(ReloadOutcome::RolledBack {
                    status,
                    error,
                    error_kind,
                })
            }
            Err(rollback_error) => {
                let rollback_error_kind = rollback_error.kind;
                let rollback_error =
                    bound_message(rollback_error.message, self.limits.maximum_diagnostic_bytes);
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
                    error_kind,
                    rollback_error_kind,
                })
            }
        }
    }

    async fn resolve_reload(&self, input: &ProfileInput) -> Result<(ResolvedTarget, WatchPlan)> {
        let source_compiler = input.compiler.clone();
        let program = input.program.clone();
        let candidate = self
            .execution
            .prepare(move || source_compiler.compile(&program))
            .await
            .map_err(|_| {
                ProfileError::InvalidProgram("Profile compiler task failed".to_owned())
            })??;
        let resolver = Arc::clone(&input.resolver);
        let limits = self.limits.clone();
        self.execution
            .prepare(move || {
                let target = resolve_target(resolver.as_ref(), candidate)?;
                let watch_plan = WatchPlan::establish(&target.candidate, &limits)?;
                Ok::<_, ProfileError>((target, watch_plan))
            })
            .await
            .map_err(|_| {
                ProfileError::InvalidProgram("Profile resolution task failed".to_owned())
            })?
    }

    fn reload_base(&self) -> Result<(u64, ProfileHealth, Context, ResolvedTarget)> {
        let state = self.state.lock().expect("Profile state poisoned");
        let state = state.as_ref().ok_or(ProfileError::Stopped)?;
        let revision = state
            .revision
            .checked_add(1)
            .ok_or_else(|| ProfileError::InvalidProgram("Profile revision exhausted".to_owned()))?;
        let mut previous = state.converged_target.clone();
        // Failed compensation can leave members absent from the accepted target.
        // Reordering must address their occupied Runtime positions, not new ones.
        for leaf in &state.active {
            previous
                .bindings
                .positions
                .insert(leaf.resolved.candidate.id().clone(), leaf.position.clone());
        }
        Ok((
            revision,
            status_from_state(state, self.limits.maximum_diagnostic_bytes).health,
            state.context.clone(),
            previous,
        ))
    }

    fn prepare_rollback(&self, context: &Context) -> Result<RollbackTarget> {
        let (previous, desired, health, diagnostic) = {
            let state = self.state.lock().expect("Profile state poisoned");
            let state = state.as_ref().expect("checked active state");
            let mut previous = state.converged_target.clone();
            previous.leaves.clear();
            previous.bindings.positions.clear();
            previous.bindings.effective.clear();
            for leaf in &state.active {
                if settled_failure(leaf.resolved.candidate.id(), &leaf.handle.snapshot().state)
                    .is_some()
                {
                    continue;
                }
                previous.leaves.push(leaf.resolved.clone());
                previous
                    .bindings
                    .positions
                    .insert(leaf.resolved.candidate.id().clone(), leaf.position.clone());
                previous
                    .bindings
                    .effective
                    .insert(leaf.resolved.candidate.id().clone(), leaf.bindings.clone());
            }
            (
                previous,
                state.target.clone(),
                state.health,
                state.diagnostic.clone(),
            )
        };
        let leaves = previous
            .leaves
            .iter()
            .map(|leaf| {
                let id = leaf.candidate.id();
                let bindings = previous.bindings.effective[id].clone();
                Ok(BoundLeaf {
                    context: bindings::apply(
                        context.with_child_position(&previous.bindings.positions[id])?,
                        &bindings,
                    )?,
                    bindings,
                })
            })
            .collect::<Result<Vec<_>>>()?;
        let bound = BoundTarget {
            parent: context.clone(),
            target: previous,
            leaves,
        };
        Ok(RollbackTarget {
            bound,
            desired,
            health,
            diagnostic,
        })
    }

    async fn converge_once(
        &self,
        active: &mut Vec<ActiveLeaf>,
        candidate: BoundTarget,
        keep_successful: bool,
    ) -> std::result::Result<ResolvedTarget, ConvergenceFailure> {
        let mut failure = None;
        let wanted = candidate
            .target
            .leaves
            .iter()
            .enumerate()
            .map(|(index, leaf)| (leaf.candidate.id().clone(), index))
            .collect::<BTreeMap<_, _>>();
        for index in (0..active.len()).rev() {
            let retained = wanted
                .get(active[index].resolved.candidate.id())
                .is_some_and(|next| {
                    keep_leaf(
                        &active[index],
                        &candidate.target.leaves[*next],
                        &candidate.leaves[*next],
                    )
                });
            if retained {
                continue;
            }
            let removed = active.remove(index);
            self.remove_active(index, &removed);
            let report = removed.handle.dispose().await;
            if !report.is_clean() {
                let error = ConvergenceFailure {
                    kind: ProfileFailureKind::Retire,
                    message: format!(
                        "disposing Profile instance `{}` reported {} cleanup failures",
                        removed.resolved.candidate.id(),
                        report.total_failures()
                    ),
                };
                if !keep_successful {
                    return Err(error);
                }
                failure.get_or_insert(error);
            }
        }
        publish_order(&candidate)?;
        active.sort_by_key(|leaf| wanted[leaf.resolved.candidate.id()]);
        for leaf in active.iter_mut() {
            debug_assert_eq!(
                leaf.position,
                candidate.target.bindings.positions[leaf.resolved.candidate.id()]
            );
            leaf.resolved = candidate.target.leaves[wanted[leaf.resolved.candidate.id()]].clone();
        }
        self.replace_active(active);
        // The ordered retained subsequence is extended one leaf at a time. No
        // preparation proof or Fiber admission is reserved for a second graph.
        let mut position = 0;
        for index in 0..candidate.target.leaves.len() {
            let resolved = candidate.target.leaves[index].clone();
            if active
                .get(position)
                .is_some_and(|leaf| leaf.resolved.candidate.id() == resolved.candidate.id())
            {
                position += 1;
                continue;
            }
            let handle = match candidate.leaves[index].activate(&resolved).await {
                Ok(handle) => handle,
                Err(error) if keep_successful => {
                    failure.get_or_insert(error.into());
                    continue;
                }
                Err(error) => return Err(error.into()),
            };
            let state = handle.snapshot().state;
            active.insert(
                position,
                ActiveLeaf {
                    position: candidate.target.bindings.positions[resolved.candidate.id()].clone(),
                    resolved,
                    handle,
                    bindings: candidate.leaves[index].bindings.clone(),
                },
            );
            self.insert_active(position, &active[position]);
            position += 1;
            if let Some(diagnostic) =
                settled_failure(candidate.target.leaves[index].candidate.id(), &state)
            {
                let error = ConvergenceFailure {
                    kind: ProfileFailureKind::Apply,
                    message: diagnostic,
                };
                if !keep_successful {
                    return Err(error);
                }
                failure.get_or_insert(error);
            }
        }
        match failure {
            Some(error) => Err(error),
            None => Ok(candidate.target),
        }
    }

    fn set_converging(&self, target: &ResolvedTarget) {
        let mut state = self.state.lock().expect("Profile state poisoned");
        let state = state.as_mut().expect("checked active state");
        state.health = ProfileHealth::Converging;
        state.target = target.clone();
        state.diagnostic = None;
        self.publish_locked_state(state);
    }

    fn complete_unchanged(
        &self,
        target: ResolvedTarget,
        watch_plan: WatchPlan,
        replacement: bool,
    ) -> ProfileStatus {
        let mut state = self.state.lock().expect("Profile state poisoned");
        let state = state.as_mut().expect("checked active state");
        if replacement {
            state.input_restart_required = false;
        }
        state.health = if state.input_restart_required {
            ProfileHealth::RestartRequired
        } else {
            ProfileHealth::Converged
        };
        state.target = target.clone();
        state.converged_target = target;
        state.watch_plan = watch_plan;
        state.watcher = self.watcher_health(&state.watch_plan);
        state.diagnostic = None;
        self.publish_locked_state(state)
    }

    fn complete_restart(
        &self,
        target: ResolvedTarget,
        watch_plan: WatchPlan,
        replacement: bool,
    ) -> ProfileStatus {
        let mut state = self.state.lock().expect("Profile state poisoned");
        let state = state.as_mut().expect("checked active state");
        state.health = ProfileHealth::RestartRequired;
        state.input_restart_required = replacement;
        if !replacement {
            state.target = target;
            state.watch_plan = watch_plan;
            state.watcher = self.watcher_health(&state.watch_plan);
        }
        state.diagnostic = None;
        self.publish_locked_state(state)
    }

    fn complete_applied(
        &self,
        revision: u64,
        target: ResolvedTarget,
        active: Vec<ActiveLeaf>,
        watch_plan: WatchPlan,
        acknowledge_restart: bool,
        failure: Option<&str>,
    ) -> ProfileStatus {
        let status = {
            let mut state = self.state.lock().expect("Profile state poisoned");
            let state = state.as_mut().expect("checked active state");
            state.revision = revision;
            if acknowledge_restart {
                state.input_restart_required = false;
            }
            state.health = if failure.is_some() {
                ProfileHealth::Degraded
            } else if state.input_restart_required {
                ProfileHealth::RestartRequired
            } else {
                ProfileHealth::Converged
            };
            state.target = target.clone();
            state.converged_target = target;
            state.active = active;
            state.watch_plan = watch_plan;
            state.watcher = self.watcher_health(&state.watch_plan);
            state.diagnostic = failure.map(str::to_owned);
            self.publish_locked_state(state)
        };
        self.membership_changed.notify_one();
        status
    }

    fn complete_partial(
        &self,
        revision: u64,
        target: ResolvedTarget,
        active: Vec<ActiveLeaf>,
        watch_plan: WatchPlan,
        acknowledge_restart: bool,
        error: ConvergenceFailure,
    ) -> ReloadOutcome {
        let error_kind = error.kind;
        let error = bound_message(error.message, self.limits.maximum_diagnostic_bytes);
        let status = self.complete_applied(
            revision,
            target,
            active,
            watch_plan,
            acknowledge_restart,
            Some(&error),
        );
        ReloadOutcome::Partial {
            status,
            error,
            error_kind,
        }
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
            state.watcher = self.watcher_health(&state.watch_plan);
            self.publish_locked_state(state)
        };
        self.membership_changed.notify_one();
        status
    }

    fn remove_active(&self, index: usize, removed: &ActiveLeaf) {
        let mut state = self.state.lock().expect("Profile state poisoned");
        if let Some(state) = state.as_mut() {
            let mirrored = state.active.remove(index);
            debug_assert_eq!(
                mirrored.resolved.candidate.id(),
                removed.resolved.candidate.id()
            );
        }
    }

    fn insert_active(&self, index: usize, added: &ActiveLeaf) {
        let mut state = self.state.lock().expect("Profile state poisoned");
        if let Some(state) = state.as_mut() {
            state.active.insert(index, added.clone());
        }
    }

    fn replace_active(&self, active: &[ActiveLeaf]) {
        let mut state = self.state.lock().expect("Profile state poisoned");
        if let Some(state) = state.as_mut() {
            state.active = active.to_vec();
        }
    }

    fn publish_preflight_error(&self, error: &ProfileError) {
        let mut state = self.state.lock().expect("Profile state poisoned");
        if let Some(state) = state.as_mut() {
            state.diagnostic = Some(bound_message(
                error.to_string(),
                self.limits.maximum_diagnostic_bytes,
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

    #[cfg(not(target_family = "wasm"))]
    fn publish_watcher_error(&self, baseline: &WatchPlan) -> bool {
        const DIAGNOSTIC: &str = "a required watched Profile source is unavailable";
        let diagnostic = bound_message(DIAGNOSTIC.to_owned(), self.limits.maximum_diagnostic_bytes);
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
        self.publish_locked_state(state);
    }

    fn publish_locked_state(&self, state: &mut ControllerState) -> ProfileStatus {
        let status = status_from_state(state, self.limits.maximum_diagnostic_bytes);
        state.health = status.health;
        state.diagnostic.clone_from(&status.diagnostic);
        self.publish_status(status)
    }

    fn publish_status(&self, mut status: ProfileStatus) -> ProfileStatus {
        self.status_tx.send_if_modified(|previous| {
            status.revision = previous.revision;
            if *previous == status {
                return false;
            }
            status.revision = previous
                .revision
                .checked_add(1)
                .expect("Profile observation revision exhausted");
            *previous = status.clone();
            true
        });
        status
    }

    #[cfg(not(target_family = "wasm"))]
    fn mark_dirty(&self) {
        if !self.dirty.swap(true, Ordering::AcqRel) {
            self.dirty_notify.notify_one();
        }
    }

    #[cfg(not(target_family = "wasm"))]
    async fn poll_sources(self: &Arc<Self>, mut stop: watch::Receiver<bool>) {
        let mut ticks_since_content_audit = 0_usize;
        let mut policy = self.source_watch.subscribe();
        loop {
            if *stop.borrow() {
                return;
            }
            let enabled = *policy.borrow_and_update();
            {
                let mut state = self.state.lock().expect("Profile state poisoned");
                if let Some(state) = state.as_mut() {
                    let watcher = if enabled {
                        if state.watcher == WatcherHealth::Inactive {
                            state.watch_plan.health()
                        } else {
                            state.watcher
                        }
                    } else {
                        WatcherHealth::Inactive
                    };
                    if state.watcher != watcher {
                        state.watcher = watcher;
                        self.publish_locked_state(state);
                    }
                }
            }
            tokio::select! {
                _ = policy.changed() => {
                    ticks_since_content_audit = 0;
                    if !self.source_watch.enabled() { self.dirty.store(false, Ordering::Release); }
                }
                changed = stop.changed() => {
                    if changed.is_err() || *stop.borrow() {
                        return;
                    }
                }
                () = self.execution.sleep(WATCH_INTERVAL), if enabled => {
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
                    let limits = self.limits.clone();
                    let probed_plan = plan.clone();
                    let current = self.execution.prepare(move || {
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

    #[cfg(not(target_family = "wasm"))]
    fn refresh_watch_plan(&self, baseline: &WatchPlan, current: WatchPlan) {
        let mut state = self.state.lock().expect("Profile state poisoned");
        let Some(state) = state.as_mut() else {
            return;
        };
        if &state.watch_plan == baseline {
            state.watch_plan = current;
        }
    }

    #[cfg(not(target_family = "wasm"))]
    async fn drive_dirty(self: &Arc<Self>, mut stop: watch::Receiver<bool>) {
        let mut consecutive_failures = 0_u32;
        loop {
            if self.dirty.swap(false, Ordering::AcqRel) {
                if !self.source_watch.enabled() {
                    continue;
                }
                if let Err(error) = self.reload_from(ProfileAttemptOrigin::Watcher).await {
                    if matches!(error, ProfileError::Busy) {
                        self.mark_dirty();
                    }
                    consecutive_failures = consecutive_failures.saturating_add(1);
                    tokio::select! {
                        changed = stop.changed() => {
                            if changed.is_err() || *stop.borrow() {
                                return;
                            }
                        }
                        () = self.execution.sleep(automatic_reload_backoff(consecutive_failures)) => {}
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
            let mut subscriptions = receivers
                .into_iter()
                .map(next_child_change)
                .collect::<FuturesUnordered<_>>();
            loop {
                tokio::select! { biased;
                    stop_change = stop.changed() => {
                        if stop_change.is_err() || *stop.borrow() { return; }
                    }
                    () = self.membership_changed.notified() => break,
                    changed = subscriptions.next(), if !subscriptions.is_empty() => {
                        if let Some((receiver, open)) = changed {
                            let mut ready = vec![(receiver, open)];
                            // Removed subscriptions cannot contribute twice to this batch.
                            while let Some(Some(change)) = subscriptions.next().now_or_never() {
                                ready.push(change);
                            }
                            self.refresh_observed();
                            for (receiver, open) in ready {
                                if open { subscriptions.push(next_child_change(receiver)); }
                            }
                            tokio::task::yield_now().await;
                        }
                    }
                }
            }
        }
    }

    async fn stop(&self) {
        self.accepting.store(false, Ordering::Release);
        self.command_stop.cancel();
        let reload = self.reload_lock.lock().await;
        let mut stopped = self.state.lock().expect("Profile state poisoned").take();
        if let Some(stopped) = stopped.as_mut() {
            stopped.health = ProfileHealth::Stopped;
            *self
                .retired_snapshot
                .lock()
                .expect("retired Profile snapshot poisoned") = Some(ProfileSnapshot {
                revision: stopped.revision,
                source_digest: stopped.target.candidate.source_digest().to_owned(),
                nodes: snapshot_nodes(&stopped.target.candidate.tree),
            });
            self.publish_locked_state(stopped);
        }
        let input = self.input.lock().expect("Profile input poisoned").1.take();
        drop(reload);
        drop(input);
        drop(stopped);
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
    initial: Mutex<Option<ResolvedTarget>>,
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
        // Owner close joins initial convergence and worker installation just as
        // it joins later reloads; it cannot remove state halfway through startup.
        let _initial = control.reload_lock.lock().await;
        if control.command_stop.is_cancelled() {
            return Err(MetaError::Cancelled);
        }
        let prepared = self
            .initial
            .lock()
            .expect("initial Profile target poisoned")
            .take()
            .ok_or_else(|| MetaError::Activation("Profile bootstrap is single-use".to_owned()))?;
        let input = control
            .input
            .lock()
            .expect("Profile input poisoned")
            .1
            .clone()
            .ok_or_else(|| MetaError::Activation("Profile input was closed".into()))?;
        let candidate = bind_resolved_target(
            prepared,
            plan.context(),
            input.resolver.as_ref(),
            &BindingSnapshot::default(),
        )
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
                    let mut failed = false;
                    for handle in handles {
                        failed |= handle.await.is_err();
                    }
                    if failed {
                        Err("Profile background task failed".to_owned())
                    } else {
                        Ok(())
                    }
                })
            }),
        )?;

        let service: Arc<dyn ProfileControl> = Arc::new(WeakControl(Arc::downgrade(&control)));
        let _control_supply = plan
            .context()
            .provide_local::<ProfileControlContract>(service)?;

        control
            .start_initial(plan.context().clone(), candidate)
            .await?;

        let mut handles = Vec::new();
        let receiver = control
            .receiver
            .lock()
            .expect("Profile receiver poisoned")
            .take()
            .ok_or_else(|| {
                MetaError::Activation("Profile command worker is single-use".to_owned())
            })?;
        control.accepting.store(true, Ordering::Release);
        handles.push(control.execution.spawn({
            let control = Arc::clone(&control);
            let stop = tasks.stop.subscribe();
            async move { control.drive_commands(receiver, stop).await }
        }));
        #[cfg(not(target_family = "wasm"))]
        {
            handles.push(control.execution.spawn({
                let control = Arc::clone(&control);
                let stop = tasks.stop.subscribe();
                async move { control.poll_sources(stop).await }
            }));
            handles.push(control.execution.spawn({
                let control = Arc::clone(&control);
                let stop = tasks.stop.subscribe();
                async move { control.drive_dirty(stop).await }
            }));
        }
        handles.push(control.execution.spawn({
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
    handles: Mutex<Option<Vec<rsi_meta::Task<()>>>>,
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

fn resolve_target(
    resolver: &dyn ProfileResolver,
    candidate: ProfileCandidate,
) -> Result<ResolvedTarget> {
    for leaf in candidate.leaves() {
        for isolation in leaf.isolations() {
            for (lane, keys) in [
                (crate::IsolationLane::Local, isolation.local()),
                (crate::IsolationLane::Event, isolation.events()),
            ] {
                for key in keys {
                    bindings::resolve_contract(resolver, lane, key)?;
                }
            }
            for named in isolation.named() {
                bindings::resolve_contract(resolver, named.lane(), named.key())?;
            }
        }
    }
    let leaves = candidate
        .leaves()
        .iter()
        .map(|leaf| {
            let factory = resolver.resolve(leaf.plugin())?;
            let identity = factory.identity().clone();
            let update_mode = factory.update_mode();
            Ok(ResolvedLeaf {
                candidate: Arc::new(leaf.clone()),
                factory,
                identity,
                update_mode,
            })
        })
        .collect::<Result<Vec<_>>>()?;
    Ok(ResolvedTarget {
        status: leaves
            .iter()
            .map(|leaf| ProfileTargetStatus {
                id: leaf.candidate.id().clone(),
                factory: leaf.identity.clone(),
            })
            .collect(),
        candidate: Arc::new(candidate),
        leaves,
        bindings: BindingSnapshot::default(),
    })
}

fn bind_resolved_target(
    mut target: ResolvedTarget,
    base: &Context,
    resolver: &dyn ProfileResolver,
    previous: &BindingSnapshot,
) -> Result<BoundTarget> {
    let namespace = Namespace::select(base, previous)?;
    let mut snapshot = BindingSnapshot {
        namespace: Some(namespace.clone()),
        ..BindingSnapshot::default()
    };
    let mut leaves = Vec::with_capacity(target.leaves.len());
    let mut groups: BTreeMap<_, EffectiveBindings> = BTreeMap::new();
    let empty = EffectiveBindings::default();
    for leaf in &target.leaves {
        let mut effective = empty.clone();
        for (group, isolation) in leaf
            .candidate
            .groups()
            .iter()
            .zip(leaf.candidate.isolations())
        {
            if let Some(cached) = groups.get(group) {
                effective = cached.clone();
            } else {
                let delta = namespace.delta(base, resolver, group, isolation)?;
                if !delta.is_empty() {
                    let mut inherited = (*effective).clone();
                    inherited.extend(delta);
                    effective = Arc::new(inherited);
                }
                groups.insert(group.clone(), effective.clone());
            }
        }
        let id = leaf.candidate.id();
        let position = match previous.positions.get(id) {
            Some(position) => position.clone(),
            None => base.child_position()?,
        };
        let context = bindings::apply(base.with_child_position(&position)?, &effective)?;
        snapshot.positions.insert(id.clone(), position);
        snapshot.effective.insert(id.clone(), effective.clone());
        leaves.push(BoundLeaf {
            context,
            bindings: effective,
        });
    }
    target.bindings = snapshot;
    Ok(BoundTarget {
        parent: base.clone(),
        target,
        leaves,
    })
}

fn publish_order(target: &BoundTarget) -> Result<()> {
    let positions = target
        .target
        .leaves
        .iter()
        .map(|leaf| target.target.bindings.positions[leaf.candidate.id()].clone())
        .collect::<Vec<_>>();
    target.parent.reorder_children(&positions)?;
    Ok(())
}

fn keep_leaf(active: &ActiveLeaf, resolved: &ResolvedLeaf, bound: &BoundLeaf) -> bool {
    resolved_leaf_equal(&active.resolved, resolved)
        && active.bindings == bound.bindings
        && matches!(
            active.handle.snapshot().state,
            FiberState::Active | FiberState::Pending(_) | FiberState::Loading
        )
}

fn semantic_equal(left: &ResolvedTarget, right: &ResolvedTarget) -> bool {
    left.leaves.len() == right.leaves.len()
        && left.leaves.iter().zip(&right.leaves).all(|(a, b)| {
            resolved_leaf_equal(a, b)
                && left.bindings.effective.get(a.candidate.id())
                    == right.bindings.effective.get(b.candidate.id())
        })
}

fn resolved_leaf_equal(left: &ResolvedLeaf, right: &ResolvedLeaf) -> bool {
    left.candidate.id() == right.candidate.id()
        && left.candidate.config() == right.candidate.config()
        && left.identity == right.identity
        && left.update_mode == right.update_mode
}

fn restart_required(left: &ResolvedTarget, right: &ResolvedTarget) -> bool {
    let old = left
        .leaves
        .iter()
        .map(|leaf| (leaf.candidate.id(), leaf))
        .collect::<BTreeMap<_, _>>();
    let new = right
        .leaves
        .iter()
        .map(|leaf| (leaf.candidate.id(), leaf))
        .collect::<BTreeMap<_, _>>();
    let unchanged = |a: &ResolvedLeaf, b: &ResolvedLeaf| {
        resolved_leaf_equal(a, b)
            && left.bindings.effective.get(a.candidate.id())
                == right.bindings.effective.get(b.candidate.id())
    };
    old.iter().any(|(id, leaf)| {
        leaf.update_mode == UpdateMode::RestartRequired
            && new.get(id).is_none_or(|next| !unchanged(leaf, next))
    }) || new.iter().any(|(id, leaf)| {
        leaf.update_mode == UpdateMode::RestartRequired
            && old
                .get(id)
                .is_none_or(|previous| !unchanged(previous, leaf))
    })
}

fn settled_failure(instance: &rsi_meta::InstanceId, state: &FiberState) -> Option<String> {
    match state {
        FiberState::Failed(reason) => Some(
            ProfileError::Application {
                instance: instance.clone(),
                reason: reason.clone(),
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

fn status_from_state(state: &ControllerState, maximum_diagnostic_bytes: usize) -> ProfileStatus {
    let mut failure = (state.active.len() != state.converged_target.leaves.len())
        .then(|| "accepted Profile has unavailable members".to_owned());
    let mut status = ProfileStatus {
        revision: state.revision,
        health: state.health,
        watcher: state.watcher,
        source_digest: state.target.candidate.source_digest().to_owned(),
        target: state.target.status.clone(),
        observed: state
            .active
            .iter()
            .map(|entry| {
                let snapshot = entry.handle.snapshot();
                if failure.is_none() {
                    failure = settled_failure(entry.resolved.candidate.id(), &snapshot.state);
                }
                ProfileInstanceStatus {
                    id: entry.resolved.candidate.id().clone(),
                    factory: snapshot.factory,
                    state: redacted_instance_state(snapshot.state),
                }
            })
            .collect(),
        diagnostic: state.diagnostic.clone(),
        last_attempt: state.last_attempt.clone(),
    };
    if matches!(
        status.health,
        ProfileHealth::Converged | ProfileHealth::RestartRequired
    ) && let Some(diagnostic) = &failure
    {
        status.health = ProfileHealth::Degraded;
        status.diagnostic = Some(bound_message(diagnostic.clone(), maximum_diagnostic_bytes));
    }
    status
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
        target: Arc::from([]),
        observed: Vec::new(),
        diagnostic: None,
        last_attempt: None,
    }
}

async fn next_child_change(
    mut receiver: watch::Receiver<rsi_meta::FiberSnapshot>,
) -> (watch::Receiver<rsi_meta::FiberSnapshot>, bool) {
    let open = receiver.changed().await.is_ok();
    (receiver, open)
}

#[cfg(test)]
mod tests {
    use super::{ProfileInstanceState, redacted_instance_state, settled_failure};
    use rsi_meta::{FiberState, InstanceId};

    #[tokio::test]
    async fn stopped_input_destructors_can_inspect_control_without_locked_ownership() {
        use super::*;
        #[derive(Debug)]
        struct InspectOnDrop {
            control: Arc<Mutex<Weak<Controller>>>,
            dropped: Arc<AtomicBool>,
        }
        impl ProfileResolver for InspectOnDrop {
            fn resolve(&self, _: &PluginId) -> Result<ResolvedFactory> {
                unreachable!("empty profile")
            }
        }
        impl Drop for InspectOnDrop {
            fn drop(&mut self) {
                let control = self.control.lock().unwrap().upgrade().unwrap();
                assert!(control.state.try_lock().is_ok());
                assert!(control.input.try_lock().is_ok());
                assert!(control.reload_lock.try_lock().is_ok());
                assert_eq!(control.status().health(), ProfileHealth::Stopped);
                self.dropped.store(true, Ordering::Release);
            }
        }
        let runtime = Runtime::default();
        let target = Arc::new(Mutex::new(Weak::new()));
        let dropped = Arc::new(AtomicBool::new(false));
        let bootstrap = ProfileBootstrap::prepare_input(
            &runtime,
            ProfileInput::new(
                Arc::new(InspectOnDrop {
                    control: target.clone(),
                    dropped: dropped.clone(),
                }),
                ProfileProgram::from_profile(crate::Profile::new([])),
                ProfileEnvironment::without_paths("test", BTreeMap::new()).unwrap(),
                ProfileLimits::default(),
            ),
        )
        .unwrap();
        *target.lock().unwrap() = Arc::downgrade(&bootstrap.control);
        let handle = runtime
            .root()
            .apply(
                ResolvedFactory::linked(
                    "profile",
                    "1",
                    UpdateMode::Replayable,
                    bootstrap.factory(),
                ),
                ConfigValue::Null,
            )
            .await
            .unwrap();
        assert!(handle.dispose().await.is_clean());
        assert!(dropped.load(Ordering::Acquire));
        assert!(runtime.shutdown().await.is_clean());
    }

    #[tokio::test]
    async fn compensation_cannot_restore_stale_healthy_status_with_missing_members() {
        use super::*;
        #[derive(Debug)]
        struct Probe;
        #[async_trait]
        impl PluginFactory for Probe {
            fn prepare(&self, config: &ConfigValue) -> rsi_meta::Result<PreparedActivation> {
                Ok(PreparedActivation::new(config.clone()))
            }
            async fn activate(&self, plan: ActivationPlan) -> rsi_meta::Result<()> {
                if plan.config().as_bool() == Some(true) {
                    return Err(MetaError::Activation("candidate failed".into()));
                }
                Ok(())
            }
        }
        impl ProfileResolver for Probe {
            fn resolve(&self, _: &PluginId) -> Result<ResolvedFactory> {
                Ok(ResolvedFactory::linked(
                    "probe",
                    "1",
                    UpdateMode::Replayable,
                    Arc::new(Probe),
                ))
            }
        }
        let input = |fail| {
            ProfileInput::new(
                Arc::new(Probe),
                ProfileProgram::from_profile(crate::Profile::new([crate::ProfileEntry::new(
                    "leaf",
                    "probe",
                    ConfigValue::Bool(fail),
                )])),
                ProfileEnvironment::without_paths("test", BTreeMap::new()).unwrap(),
                ProfileLimits::default(),
            )
        };
        let runtime = Runtime::default();
        let old = input(false);
        let bootstrap = ProfileBootstrap::prepare_input(&runtime, old.clone()).unwrap();
        let parent = runtime
            .root()
            .apply(
                ResolvedFactory::linked(
                    "profile",
                    "1",
                    UpdateMode::Replayable,
                    bootstrap.factory(),
                ),
                ConfigValue::Null,
            )
            .await
            .unwrap();
        let control = &bootstrap.control;
        let active = control.state.lock().unwrap().as_mut().unwrap().active[0]
            .handle
            .clone();
        active.dispose().await;
        // Pin the observation-lag interleaving: compensation captures stale health,
        // but excludes the already disposed member from its usable target.
        let (context, desired, watch_plan) = {
            let mut state = control.state.lock().unwrap();
            let state = state.as_mut().unwrap();
            state.health = ProfileHealth::Converged;
            (
                state.context.clone(),
                state.target.clone(),
                state.watch_plan.clone(),
            )
        };
        let rollback = control.prepare_rollback(&context).unwrap();
        assert_eq!(rollback.health, ProfileHealth::Converged);
        let outcome = control
            .recover_failed_convergence(
                2,
                desired,
                vec![],
                rollback,
                watch_plan,
                ConvergenceFailure {
                    kind: ProfileFailureKind::Apply,
                    message: "candidate failed".into(),
                },
            )
            .await
            .unwrap();
        assert!(matches!(outcome, ReloadOutcome::RolledBack { .. }));
        assert_eq!(outcome.status().health(), ProfileHealth::Degraded);
        assert!(outcome.status().observed().is_empty());
        assert!(outcome.status().diagnostic().is_some());
        assert!(matches!(
            control.reload_serialized(&old, false).await.unwrap(),
            ReloadOutcome::Applied(_)
        ));
        assert_eq!(control.status().observed().len(), 1);
        parent.dispose().await;
        assert!(runtime.shutdown().await.is_clean());
    }

    #[cfg(not(target_family = "wasm"))]
    #[tokio::test(start_paused = true)]
    async fn automatic_reload_retries_busy_without_another_source_notification() {
        use super::*;
        #[derive(Debug)]
        struct EmptyResolver;
        impl ProfileResolver for EmptyResolver {
            fn resolve(&self, _: &PluginId) -> Result<ResolvedFactory> {
                panic!("empty Profile has no factories")
            }
        }
        let runtime = Runtime::default();
        let bootstrap = ProfileBootstrap::prepare(
            &runtime,
            Arc::new(EmptyResolver),
            ProfileProgram::from_profile(crate::Profile::new([])),
            ProfileEnvironment::without_paths("test", BTreeMap::new()).unwrap(),
            ProfileLimits::default(),
        )
        .unwrap();
        let control = &bootstrap.control;
        control.accepting.store(true, Ordering::Release);
        let mut commands = control.receiver.lock().unwrap().take().unwrap();
        let _queued = control.submit(None).unwrap();
        assert!(matches!(control.submit(None), Err(ProfileError::Busy)));
        control.mark_dirty();
        let (stop, receiver) = watch::channel(false);
        let driver = tokio::spawn({
            let control = control.clone();
            async move { control.drive_dirty(receiver).await }
        });
        tokio::task::yield_now().await;
        drop(commands.recv().await.unwrap());
        // Only queue capacity changes: there is no new filesystem event.
        let retried = tokio::time::timeout(Duration::from_secs(6), commands.recv())
            .await
            .is_ok();
        stop.send_replace(true);
        driver.await.unwrap();
        assert!(retried, "Busy discarded the automatic reload notification");
        let _ = runtime.shutdown().await;
    }

    #[test]
    fn local_failure_retains_cause_while_child_category_remains_redacted() {
        let instance = InstanceId::new("child");
        let runtime_state = FiberState::Failed("plugin diagnostic".to_owned());

        let state = redacted_instance_state(runtime_state.clone());
        let diagnostic = settled_failure(&instance, &runtime_state).unwrap();

        assert_eq!(state, ProfileInstanceState::Failed);
        assert!(!format!("{state:?}").contains("plugin diagnostic"));
        assert_eq!(
            diagnostic,
            "applying Profile instance `child` failed: plugin diagnostic"
        );
    }
}
