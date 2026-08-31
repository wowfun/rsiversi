//! Standing builders for immutable Agent composition generations.

#![deny(unsafe_code)]
#![warn(missing_docs)]

use async_trait::async_trait;
use rsi_agent_composition_protocol::{
    AgentComposition, AgentCompositionContract, AgentCompositionError, AgentCompositionPin,
};
use rsi_agent_presets::{AgentPresetCatalog, AgentPresetId, PresetError};
use rsi_meta::{
    ActivationPlan, ConfigValue, Context, FactoryIdentity, FiberState, MetaError, PluginFactory,
    PluginId, PreparedActivation, ResolvedFactory, UpdateMode,
};
use rsi_meta_profile::{IsolationSpec, ProfileError, ProfileGenerationPlan, ProfileResolver};
use rsi_meta_scope::{ScopeHandle, ScopeRoot};
use rsi_tools_protocol::{
    ToolCatalogProvider, ToolCatalogProviderContract, ToolCatalogStage, ToolRegistrar,
    ToolRegistrarContract, ToolRuntime,
};
use std::collections::BTreeMap;
use std::fmt;
use std::sync::{Arc, Mutex, Weak};
use tokio::sync::{Mutex as AsyncMutex, Notify, OwnedMutexGuard, OwnedSemaphorePermit, Semaphore};
use tokio_util::sync::CancellationToken;

/// Maximum Agent-generation builds executing across presets.
pub const MAXIMUM_CONCURRENT_BUILDS: usize = 8;
/// Maximum preset rows retained by one standing composition provider.
pub const MAXIMUM_CURRENT_PRESETS: usize = 256;

const REGISTRAR_FACTORY_ID: &str = "rsi.agent.composition.tool-registrar";

/// Frozen Agent-only allowlist of exact resolved contribution factories.
#[derive(Clone)]
pub struct AgentContributionCatalog {
    factories: BTreeMap<PluginId, ResolvedFactory>,
}

impl AgentContributionCatalog {
    /// Freezes exact executable identities selected by the application.
    ///
    /// Duplicate plugin identities are rejected rather than resolved by input
    /// order.
    ///
    /// # Errors
    ///
    /// Returns [`ProfileError::InvalidProgram`] when the input repeats one
    /// plugin identity.
    pub fn new(
        factories: impl IntoIterator<Item = ResolvedFactory>,
    ) -> rsi_meta_profile::Result<Self> {
        let mut by_plugin = BTreeMap::new();
        for factory in factories {
            let plugin = match factory.identity() {
                FactoryIdentity::Linked { plugin, .. } | FactoryIdentity::Native { plugin, .. } => {
                    plugin.clone()
                }
            };
            if by_plugin.insert(plugin.clone(), factory).is_some() {
                return Err(ProfileError::InvalidProgram(format!(
                    "Agent contribution factory `{plugin}` appears more than once"
                )));
            }
        }
        Ok(Self {
            factories: by_plugin,
        })
    }
}

impl fmt::Debug for AgentContributionCatalog {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("AgentContributionCatalog")
            .field("factories", &self.factories.keys())
            .finish()
    }
}

impl ProfileResolver for AgentContributionCatalog {
    fn resolve(&self, plugin: &PluginId) -> rsi_meta_profile::Result<ResolvedFactory> {
        self.factories
            .get(plugin)
            .cloned()
            .ok_or_else(|| ProfileError::UnknownPlugin {
                plugin: plugin.clone(),
            })
    }

    fn isolate(
        &self,
        mut context: Context,
        isolation: &IsolationSpec,
    ) -> rsi_meta_profile::Result<Context> {
        if let Some(key) = isolation.local().first() {
            return Err(ProfileError::UnknownLocalContract { key: key.clone() });
        }
        if let Some(key) = isolation.events().first() {
            return Err(ProfileError::UnknownLocalEvent { key: key.clone() });
        }
        for key in isolation.portable() {
            context = context.isolate_fresh(key)?.0;
        }
        Ok(context)
    }
}

/// Ordinary plugin factory for one standing Agent composition provider.
#[derive(Clone)]
pub struct AgentCompositionFactory {
    presets: AgentPresetCatalog,
    contributions: Arc<AgentContributionCatalog>,
    scopes: ScopeRoot,
}

impl AgentCompositionFactory {
    /// Freezes all application-owned authority needed by the provider.
    pub fn new(
        presets: AgentPresetCatalog,
        contributions: AgentContributionCatalog,
        scopes: ScopeRoot,
    ) -> Self {
        Self {
            presets,
            contributions: Arc::new(contributions),
            scopes,
        }
    }
}

impl fmt::Debug for AgentCompositionFactory {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("AgentCompositionFactory")
            .field("contributions", &self.contributions)
            .finish_non_exhaustive()
    }
}

#[async_trait]
impl PluginFactory for AgentCompositionFactory {
    fn prepare(&self, desired: &ConfigValue) -> rsi_meta::Result<PreparedActivation> {
        if !desired.is_null() {
            return Err(MetaError::InvalidInput(
                "Agent composition configuration must be null".to_owned(),
            ));
        }
        Ok(PreparedActivation::new(ConfigValue::Null)
            .requiring_local::<ToolCatalogProviderContract>())
    }

    async fn activate(&self, plan: ActivationPlan) -> rsi_meta::Result<()> {
        let tools = plan.local::<ToolCatalogProviderContract>()?;
        let state = Arc::new(CompositionState {
            presets: self.presets.clone(),
            contributions: Arc::clone(&self.contributions),
            scopes: self.scopes.clone(),
            // Generation Fibers are owned by their pins, not by the provider
            // Fiber. Root them beside the provider so its deferred shutdown
            // can wait for the final pin before explicitly disposing them.
            parent: plan.context().runtime().root(),
            tools,
            build_slots: Arc::new(Semaphore::new(MAXIMUM_CONCURRENT_BUILDS)),
            shutdown: CancellationToken::new(),
            executor: tokio::runtime::Handle::current(),
            scope_changed: Notify::new(),
            inner: Mutex::new(CompositionInner {
                accepting: true,
                next_scope: 0,
                rows: BTreeMap::new(),
                scopes: BTreeMap::new(),
                cleanup_failed: false,
            }),
        });
        let service: Arc<dyn AgentComposition> = Arc::new(CompositionService {
            state: Arc::clone(&state),
        });
        let supply = plan
            .context()
            .provide_local::<AgentCompositionContract>(service)?;
        plan.defer(
            "withdraw Agent composition provider",
            Box::new(move || {
                Box::pin(async move {
                    let result = state.shutdown().await;
                    drop(supply);
                    result
                })
            }),
        )
    }
}

#[derive(Debug)]
struct CompositionService {
    state: Arc<CompositionState>,
}

#[async_trait]
impl AgentComposition for CompositionService {
    async fn default_preset_id(&self) -> rsi_agent_composition_protocol::Result<AgentPresetId> {
        self.state.default_preset_id().await
    }

    async fn pin(
        &self,
        preset_id: &AgentPresetId,
    ) -> rsi_agent_composition_protocol::Result<AgentCompositionPin> {
        self.state.pin(preset_id).await
    }
}

struct CompositionState {
    presets: AgentPresetCatalog,
    contributions: Arc<AgentContributionCatalog>,
    scopes: ScopeRoot,
    parent: Context,
    tools: Arc<dyn ToolCatalogProvider>,
    build_slots: Arc<Semaphore>,
    shutdown: CancellationToken,
    executor: tokio::runtime::Handle,
    scope_changed: Notify,
    inner: Mutex<CompositionInner>,
}

impl fmt::Debug for CompositionState {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let inner = self.inner.lock().expect("composition state poisoned");
        formatter
            .debug_struct("CompositionState")
            .field("accepting", &inner.accepting)
            .field("preset_rows", &inner.rows.len())
            .field("owned_scopes", &inner.scopes.len())
            .finish_non_exhaustive()
    }
}

struct CompositionInner {
    accepting: bool,
    next_scope: u64,
    rows: BTreeMap<AgentPresetId, Arc<PresetRow>>,
    scopes: BTreeMap<u64, Weak<ScopeRecord>>,
    cleanup_failed: bool,
}

#[derive(Debug, Default)]
struct PresetRow {
    build: Arc<AsyncMutex<()>>,
    current: Mutex<Option<Arc<Generation>>>,
}

impl PresetRow {
    fn is_evictable(&self) -> bool {
        Arc::strong_count(&self.build) == 1
            && self
                .current
                .lock()
                .expect("composition row poisoned")
                .is_none()
    }
}

struct Generation {
    preset_id: AgentPresetId,
    source_digest: String,
    tools: Arc<dyn ToolRuntime>,
    owner: Arc<GenerationOwner>,
}

impl fmt::Debug for Generation {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Generation")
            .field("preset_id", &self.preset_id)
            .field("source_digest", &self.source_digest)
            .finish_non_exhaustive()
    }
}

impl Generation {
    fn pin(&self) -> rsi_agent_composition_protocol::Result<AgentCompositionPin> {
        AgentCompositionPin::new(
            self.preset_id.clone(),
            self.source_digest.clone(),
            Arc::clone(&self.tools),
            self.owner.clone(),
        )
    }
}

struct ScopeRecord {
    id: u64,
    scope: ScopeHandle,
}

impl fmt::Debug for ScopeRecord {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ScopeRecord")
            .field("id", &self.id)
            .finish_non_exhaustive()
    }
}

struct GenerationOwner {
    record: Option<Arc<ScopeRecord>>,
    state: Weak<CompositionState>,
    executor: tokio::runtime::Handle,
}

struct UnpublishedGeneration {
    stage: Option<Box<dyn ToolCatalogStage>>,
    scope: Option<ScopeHandle>,
    tools: Option<Arc<dyn ToolRuntime>>,
    singleflight: Option<OwnedMutexGuard<()>>,
    build_slot: Option<OwnedSemaphorePermit>,
    state: Weak<CompositionState>,
    executor: tokio::runtime::Handle,
}

impl fmt::Debug for UnpublishedGeneration {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("UnpublishedGeneration")
            .field("has_stage", &self.stage.is_some())
            .field("has_scope", &self.scope.is_some())
            .field("sealed", &self.tools.is_some())
            .finish_non_exhaustive()
    }
}

impl UnpublishedGeneration {
    fn new(
        stage: Box<dyn ToolCatalogStage>,
        scope: ScopeHandle,
        singleflight: OwnedMutexGuard<()>,
        build_slot: OwnedSemaphorePermit,
        owner_state: Weak<CompositionState>,
        executor: tokio::runtime::Handle,
    ) -> Self {
        Self {
            stage: Some(stage),
            scope: Some(scope),
            tools: None,
            singleflight: Some(singleflight),
            build_slot: Some(build_slot),
            state: owner_state,
            executor,
        }
    }

    fn registrar(&self) -> Arc<dyn ToolRegistrar> {
        self.stage
            .as_ref()
            .expect("unsealed Agent generation owns its Tool stage")
            .registrar()
    }

    fn scope(&self) -> &ScopeHandle {
        self.scope
            .as_ref()
            .expect("unpublished Agent generation owns its Scope")
    }

    fn seal(&mut self) -> rsi_tools_protocol::Result<()> {
        let stage = self
            .stage
            .take()
            .expect("unsealed Agent generation owns its Tool stage");
        self.tools = Some(stage.seal()?);
        Ok(())
    }

    fn into_published_parts(mut self) -> (Arc<dyn ToolRuntime>, ScopeHandle) {
        let tools = self
            .tools
            .take()
            .expect("published Agent generation has a sealed Tool catalog");
        let scope = self
            .scope
            .take()
            .expect("published Agent generation owns its Scope");
        drop(self.singleflight.take());
        drop(self.build_slot.take());
        (tools, scope)
    }

    async fn rollback(mut self) -> bool {
        let report = self
            .scope
            .as_ref()
            .expect("unpublished Agent generation owns its Scope")
            .dispose()
            .await;
        self.record_cleanup(report.is_clean());
        drop(self.scope.take());
        drop(self.tools.take());
        drop(self.stage.take());
        drop(self.singleflight.take());
        drop(self.build_slot.take());
        report.is_clean()
    }

    fn record_cleanup(&self, clean: bool) {
        if !clean && let Some(state) = self.state.upgrade() {
            state
                .inner
                .lock()
                .expect("composition state poisoned")
                .cleanup_failed = true;
        }
    }
}

impl Drop for UnpublishedGeneration {
    fn drop(&mut self) {
        let Some(scope) = self.scope.take() else {
            return;
        };
        let tools = self.tools.take();
        let stage = self.stage.take();
        let singleflight = self.singleflight.take();
        let build_slot = self.build_slot.take();
        let owner_state = self.state.clone();
        self.executor.spawn(async move {
            let report = scope.dispose().await;
            if !report.is_clean()
                && let Some(state) = owner_state.upgrade()
            {
                state
                    .inner
                    .lock()
                    .expect("composition state poisoned")
                    .cleanup_failed = true;
            }
            drop(tools);
            drop(stage);
            drop(singleflight);
            drop(build_slot);
        });
    }
}

#[derive(Debug)]
struct CancelBuildOnDrop {
    cancellation: CancellationToken,
    armed: bool,
}

impl CancelBuildOnDrop {
    fn new(cancellation: CancellationToken) -> Self {
        Self {
            cancellation,
            armed: true,
        }
    }

    fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for CancelBuildOnDrop {
    fn drop(&mut self) {
        if self.armed {
            self.cancellation.cancel();
        }
    }
}

impl fmt::Debug for GenerationOwner {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("AgentGenerationOwner")
            .field("scope", &self.record.as_ref().map(|record| record.id))
            .finish_non_exhaustive()
    }
}

impl Drop for GenerationOwner {
    fn drop(&mut self) {
        let Some(record) = self.record.take() else {
            return;
        };
        if let Some(state) = self.state.upgrade() {
            state.schedule_disposal(record);
        } else {
            self.executor.spawn(async move {
                let _report = record.scope.dispose().await;
            });
        }
    }
}

impl CompositionState {
    async fn default_preset_id(&self) -> rsi_agent_composition_protocol::Result<AgentPresetId> {
        if !self
            .inner
            .lock()
            .expect("composition state poisoned")
            .accepting
        {
            return Err(AgentCompositionError::ShuttingDown);
        }
        let default = self.presets.default_id().await;
        if self.shutdown.is_cancelled() {
            return Err(AgentCompositionError::ShuttingDown);
        }
        default.map_err(|_| AgentCompositionError::DefaultUnavailable {
            reason: "preset default store could not be read".into(),
        })
    }

    async fn pin(
        self: &Arc<Self>,
        preset_id: &AgentPresetId,
    ) -> rsi_agent_composition_protocol::Result<AgentCompositionPin> {
        let row = self.row(preset_id)?;
        let singleflight = tokio::select! {
            () = self.shutdown.cancelled() => return Err(AgentCompositionError::ShuttingDown),
            guard = Arc::clone(&row.build).lock_owned() => guard,
        };
        if self.shutdown.is_cancelled() {
            return Err(AgentCompositionError::ShuttingDown);
        }
        let build_slot = tokio::select! {
            () = self.shutdown.cancelled() => return Err(AgentCompositionError::ShuttingDown),
            permit = Arc::clone(&self.build_slots).acquire_owned() => {
                permit.map_err(|_| AgentCompositionError::ShuttingDown)?
            },
        };

        let presets = self.presets.clone();
        let compile_preset_id = preset_id.clone();
        let compilation = blocking_with_build_admission(
            self.executor.clone(),
            singleflight,
            build_slot,
            move || presets.compile(&compile_preset_id),
        );
        let (candidate, singleflight, build_slot) = compilation
            .await
            .map_err(|_| unavailable(preset_id, "Agent Profile compilation task failed"))?;
        let candidate = candidate.map_err(|error| preset_unavailable(preset_id, &error))?;
        if self.shutdown.is_cancelled() {
            return Err(AgentCompositionError::ShuttingDown);
        }

        let current = {
            let inner = self.inner.lock().expect("composition state poisoned");
            if !inner.accepting {
                return Err(AgentCompositionError::ShuttingDown);
            }
            row.current
                .lock()
                .expect("composition row poisoned")
                .as_ref()
                .filter(|generation| generation.source_digest == candidate.source_digest())
                .cloned()
        };
        if let Some(generation) = current {
            return generation.pin();
        }

        let resolver: Arc<dyn ProfileResolver> = self.contributions.clone();
        let generation_plan = ProfileGenerationPlan::resolve(candidate, resolver)
            .map_err(|error| profile_unavailable(preset_id, &error))?;
        let source_digest = generation_plan.source_digest().to_owned();
        let cancellation = self.shutdown.child_token();
        let mut cancel_on_drop = CancelBuildOnDrop::new(cancellation.clone());
        let state = Arc::clone(self);
        let build_preset_id = preset_id.clone();
        let build = self.executor.spawn(async move {
            state
                .build_unpublished_generation(
                    &build_preset_id,
                    generation_plan,
                    singleflight,
                    build_slot,
                    cancellation,
                )
                .await
        });
        let unpublished = build
            .await
            .map_err(|_| unavailable(preset_id, "Agent generation build task failed"))??;
        cancel_on_drop.disarm();
        self.publish(
            Arc::clone(&row),
            preset_id.clone(),
            source_digest,
            unpublished,
        )
        .await
    }

    async fn build_unpublished_generation(
        self: &Arc<Self>,
        preset_id: &AgentPresetId,
        generation_plan: ProfileGenerationPlan,
        singleflight: OwnedMutexGuard<()>,
        build_slot: OwnedSemaphorePermit,
        cancellation: CancellationToken,
    ) -> rsi_agent_composition_protocol::Result<UnpublishedGeneration> {
        let stage = self
            .tools
            .begin_stage()
            .map_err(|_| unavailable(preset_id, "Tool catalog staging failed"))?;
        let scope = self
            .scopes
            .create(&self.parent)
            .await
            .map_err(|_| unavailable(preset_id, "Agent generation Scope creation failed"))?;
        let mut unpublished = UnpublishedGeneration::new(
            stage,
            scope,
            singleflight,
            build_slot,
            Arc::downgrade(self),
            self.executor.clone(),
        );
        if cancellation.is_cancelled() {
            let _clean = unpublished.rollback().await;
            return Err(self.cancelled_build_error(preset_id));
        }
        let generation_context = match unpublished
            .scope()
            .context()
            .meta()
            .clone()
            .isolate_local_fresh::<ToolRegistrarContract>()
        {
            Ok((context, _isolation)) => context,
            Err(_error) => {
                let _clean = unpublished.rollback().await;
                return Err(unavailable(
                    preset_id,
                    "Agent Tool registrar isolation failed",
                ));
            }
        };

        let registrar_handle = match generation_context
            .apply(
                ResolvedFactory::linked(
                    REGISTRAR_FACTORY_ID,
                    env!("CARGO_PKG_VERSION"),
                    UpdateMode::RestartRequired,
                    Arc::new(ToolRegistrarFactory {
                        registrar: unpublished.registrar(),
                    }),
                ),
                ConfigValue::Null,
            )
            .await
        {
            Ok(handle) if matches!(handle.snapshot().state, FiberState::Active) => handle,
            Ok(handle) => {
                let _cleanup = handle.dispose().await;
                let _clean = unpublished.rollback().await;
                return Err(unavailable(
                    preset_id,
                    "Agent Tool registrar activation failed",
                ));
            }
            Err(_error) => {
                let _clean = unpublished.rollback().await;
                return Err(unavailable(
                    preset_id,
                    "Agent Tool registrar activation failed",
                ));
            }
        };
        let _registrar_handle = registrar_handle;

        if let Err(error) = generation_plan
            .activate(&generation_context, &cancellation)
            .await
        {
            let _clean = unpublished.rollback().await;
            if self.shutdown.is_cancelled() {
                return Err(AgentCompositionError::ShuttingDown);
            }
            return Err(profile_unavailable(preset_id, &error));
        }
        if cancellation.is_cancelled() {
            let _clean = unpublished.rollback().await;
            return Err(self.cancelled_build_error(preset_id));
        }

        match unpublished.seal() {
            Ok(()) => {}
            Err(_error) => {
                let _clean = unpublished.rollback().await;
                return Err(unavailable(preset_id, "Tool catalog sealing failed"));
            }
        }
        if cancellation.is_cancelled() {
            let _clean = unpublished.rollback().await;
            return Err(self.cancelled_build_error(preset_id));
        }

        Ok(unpublished)
    }

    fn cancelled_build_error(&self, preset_id: &AgentPresetId) -> AgentCompositionError {
        if self.shutdown.is_cancelled() {
            AgentCompositionError::ShuttingDown
        } else {
            unavailable(preset_id, "Agent generation build was cancelled")
        }
    }

    fn row(
        &self,
        preset_id: &AgentPresetId,
    ) -> rsi_agent_composition_protocol::Result<Arc<PresetRow>> {
        let mut inner = self.inner.lock().expect("composition state poisoned");
        if !inner.accepting {
            return Err(AgentCompositionError::ShuttingDown);
        }
        if let Some(row) = inner.rows.get(preset_id) {
            return Ok(Arc::clone(row));
        }
        if inner.rows.len() >= MAXIMUM_CURRENT_PRESETS {
            let evictable = inner.rows.iter().find_map(|(id, row)| {
                (Arc::strong_count(row) == 1 && row.is_evictable()).then(|| id.clone())
            });
            if let Some(evictable) = evictable {
                inner.rows.remove(&evictable);
            } else {
                return Err(AgentCompositionError::Capacity);
            }
        }
        let row = Arc::new(PresetRow::default());
        inner.rows.insert(preset_id.clone(), Arc::clone(&row));
        Ok(row)
    }

    async fn publish(
        self: &Arc<Self>,
        row: Arc<PresetRow>,
        preset_id: AgentPresetId,
        source_digest: String,
        unpublished: UnpublishedGeneration,
    ) -> rsi_agent_composition_protocol::Result<AgentCompositionPin> {
        let published = {
            let mut inner = self.inner.lock().expect("composition state poisoned");
            let rejection = if !inner.accepting {
                Some(AgentCompositionError::ShuttingDown)
            } else if inner.next_scope == u64::MAX {
                Some(AgentCompositionError::Capacity)
            } else {
                None
            };
            if let Some(error) = rejection {
                Err((error, unpublished))
            } else {
                let (tools, scope) = unpublished.into_published_parts();
                inner.next_scope += 1;
                let record = Arc::new(ScopeRecord {
                    id: inner.next_scope,
                    scope,
                });
                inner.scopes.insert(record.id, Arc::downgrade(&record));
                let owner = Arc::new(GenerationOwner {
                    record: Some(record),
                    state: Arc::downgrade(self),
                    executor: self.executor.clone(),
                });
                let generation = Arc::new(Generation {
                    preset_id,
                    source_digest,
                    tools,
                    owner,
                });
                let previous = row
                    .current
                    .lock()
                    .expect("composition row poisoned")
                    .replace(Arc::clone(&generation));
                Ok((generation, previous))
            }
        };
        match published {
            Ok((generation, previous)) => {
                drop(previous);
                generation.pin()
            }
            Err((error, unpublished)) => {
                let _clean = unpublished.rollback().await;
                Err(error)
            }
        }
    }

    fn schedule_disposal(self: &Arc<Self>, record: Arc<ScopeRecord>) {
        let state = Arc::clone(self);
        self.executor.spawn(async move {
            let report = record.scope.dispose().await;
            let mut inner = state.inner.lock().expect("composition state poisoned");
            inner.cleanup_failed |= !report.is_clean();
            inner.scopes.remove(&record.id);
            drop(inner);
            state.scope_changed.notify_waiters();
        });
    }

    async fn shutdown(self: Arc<Self>) -> std::result::Result<(), String> {
        {
            let mut inner = self.inner.lock().expect("composition state poisoned");
            inner.accepting = false;
        }
        self.shutdown.cancel();
        let all_build_slots = Arc::clone(&self.build_slots)
            .acquire_many_owned(
                u32::try_from(MAXIMUM_CONCURRENT_BUILDS)
                    .expect("Agent composition build limit fits u32"),
            )
            .await
            .map_err(|_| "Agent composition build admission closed".to_owned())?;

        let current = {
            let mut inner = self.inner.lock().expect("composition state poisoned");
            let current = inner
                .rows
                .values()
                .filter_map(|row| row.current.lock().expect("composition row poisoned").take())
                .collect::<Vec<_>>();
            inner.rows.clear();
            current
        };
        drop(current);
        loop {
            let changed = self.scope_changed.notified();
            if self
                .inner
                .lock()
                .expect("composition state poisoned")
                .scopes
                .is_empty()
            {
                break;
            }
            changed.await;
        }
        drop(all_build_slots);
        if self
            .inner
            .lock()
            .expect("composition state poisoned")
            .cleanup_failed
        {
            Err("Agent generation Scope cleanup failed".to_owned())
        } else {
            Ok(())
        }
    }
}

async fn blocking_with_build_admission<T, F>(
    executor: tokio::runtime::Handle,
    singleflight: OwnedMutexGuard<()>,
    build_slot: OwnedSemaphorePermit,
    operation: F,
) -> std::result::Result<(T, OwnedMutexGuard<()>, OwnedSemaphorePermit), tokio::task::JoinError>
where
    T: Send + 'static,
    F: FnOnce() -> T + Send + 'static,
{
    executor
        .spawn_blocking(move || (operation(), singleflight, build_slot))
        .await
}

#[derive(Debug)]
struct ToolRegistrarFactory {
    registrar: Arc<dyn ToolRegistrar>,
}

#[async_trait]
impl PluginFactory for ToolRegistrarFactory {
    fn prepare(&self, desired: &ConfigValue) -> rsi_meta::Result<PreparedActivation> {
        if !desired.is_null() {
            return Err(MetaError::InvalidInput(
                "Agent Tool registrar configuration must be null".to_owned(),
            ));
        }
        Ok(PreparedActivation::new(ConfigValue::Null))
    }

    async fn activate(&self, plan: ActivationPlan) -> rsi_meta::Result<()> {
        let supply = plan
            .context()
            .provide_local::<ToolRegistrarContract>(Arc::clone(&self.registrar))?;
        plan.defer(
            "withdraw Agent Tool registrar",
            Box::new(move || {
                Box::pin(async move {
                    drop(supply);
                    Ok(())
                })
            }),
        )
    }
}

fn unavailable(preset_id: &AgentPresetId, reason: &'static str) -> AgentCompositionError {
    AgentCompositionError::Unavailable {
        preset_id: preset_id.clone(),
        reason: reason.to_owned(),
    }
}

fn preset_unavailable(preset_id: &AgentPresetId, error: &PresetError) -> AgentCompositionError {
    let reason = match error {
        PresetError::BrokenPreset { reason, .. } => reason.clone(),
        PresetError::PresetNotFound { .. } => "preset source is missing".to_owned(),
        _ => "preset source is unavailable".to_owned(),
    };
    AgentCompositionError::Unavailable {
        preset_id: preset_id.clone(),
        reason,
    }
}

fn profile_unavailable(preset_id: &AgentPresetId, error: &ProfileError) -> AgentCompositionError {
    let reason = match error {
        ProfileError::UnknownPlugin { .. } => "Profile references an unknown Agent contribution",
        ProfileError::UnknownLocalContract { .. } => {
            "Profile references an unavailable Agent Local contract"
        }
        ProfileError::UnknownLocalEvent { .. } => {
            "Profile references an unavailable Agent Local event"
        }
        ProfileError::Preparation { .. } => "preparing an Agent contribution failed",
        ProfileError::Application { .. } => "activating an Agent contribution failed",
        ProfileError::GenerationPending { .. } => "an Agent contribution remained Pending",
        _ => "Agent Profile compilation or activation failed",
    };
    AgentCompositionError::Unavailable {
        preset_id: preset_id.clone(),
        reason: reason.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::Duration;

    #[tokio::test]
    async fn preset_row_is_not_evictable_while_its_singleflight_guard_is_held() {
        let row = PresetRow::default();
        assert!(row.is_evictable());

        let guard = Arc::clone(&row.build).lock_owned().await;
        assert!(!row.is_evictable());

        drop(guard);
        assert!(row.is_evictable());
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn cancelled_blocking_work_retains_singleflight_and_global_admission_until_exit() {
        let singleflight = Arc::new(AsyncMutex::new(()));
        let singleflight_guard = Arc::clone(&singleflight).lock_owned().await;
        let build_slots = Arc::new(Semaphore::new(1));
        let build_slot = Arc::clone(&build_slots).acquire_owned().await.unwrap();
        let entered = Arc::new(AtomicBool::new(false));
        let release = Arc::new(AtomicBool::new(false));
        let operation_entered = Arc::clone(&entered);
        let operation_release = Arc::clone(&release);

        let work = tokio::spawn(blocking_with_build_admission(
            tokio::runtime::Handle::current(),
            singleflight_guard,
            build_slot,
            move || {
                operation_entered.store(true, Ordering::Release);
                while !operation_release.load(Ordering::Acquire) {
                    std::thread::sleep(Duration::from_millis(1));
                }
            },
        ));
        tokio::time::timeout(Duration::from_secs(2), async {
            while !entered.load(Ordering::Acquire) {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("blocking operation never started");

        work.abort();
        assert!(work.await.unwrap_err().is_cancelled());
        assert!(Arc::clone(&singleflight).try_lock_owned().is_err());
        assert_eq!(build_slots.available_permits(), 0);

        release.store(true, Ordering::Release);
        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                if build_slots.available_permits() == 1
                    && Arc::clone(&singleflight).try_lock_owned().is_ok()
                {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("blocking operation did not release build admission after exit");
    }
}
