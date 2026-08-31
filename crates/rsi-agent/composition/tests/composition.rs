use rsi_agent_composition::{
    AgentCompositionFactory, AgentContributionCatalog, MAXIMUM_CONCURRENT_BUILDS,
    MAXIMUM_CURRENT_PRESETS,
};
use rsi_agent_composition_protocol::{
    AgentComposition, AgentCompositionContract, AgentCompositionError,
};
use rsi_agent_presets::{
    AgentPresetCatalog, AgentPresetCatalogConfig, AgentPresetDefaultStore, AgentPresetId,
    AgentPresetProfileCompiler, AgentPresetRoot, AgentPresetTrust, COMPOSITION_FILE,
};
use rsi_meta::{
    ActivationPlan, ConfigValue, FiberHandle, FiberState, MetaError, PluginFactory,
    PreparedActivation, ResolvedFactory, Runtime, UpdateMode,
};
use rsi_meta_profile::{
    IsolationSpec, ProfileCompiler, ProfileEnvironment, ProfileError, ProfileLimits,
    ProfileResolver,
};
use rsi_meta_scope::ScopeRoot;
use rsi_tools::ToolsFactory;
use rsi_tools_protocol::{
    ToolDefinition, ToolExecution, ToolExecutor, ToolRegistrarContract, ToolRegistration,
    ToolResult,
};
use std::collections::{BTreeMap, HashMap};
use std::fs;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tempfile::TempDir;
use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;

#[derive(Debug)]
struct NoopFactory;

#[async_trait::async_trait]
impl PluginFactory for NoopFactory {
    fn prepare(&self, desired: &ConfigValue) -> rsi_meta::Result<PreparedActivation> {
        Ok(PreparedActivation::new(desired.clone()))
    }

    async fn activate(&self, _plan: ActivationPlan) -> rsi_meta::Result<()> {
        Ok(())
    }
}

#[test]
fn contribution_catalog_resolves_only_exact_allowlisted_factories() {
    let expected = ResolvedFactory::linked(
        "agent.allowed",
        "revision-a",
        UpdateMode::Replayable,
        Arc::new(NoopFactory),
    );
    let catalog = AgentContributionCatalog::new([expected.clone()]).unwrap();

    let resolved = catalog.resolve(&"agent.allowed".into()).unwrap();
    assert_eq!(resolved.identity(), expected.identity());
    assert!(matches!(
        catalog.resolve(&"agent.unknown".into()),
        Err(ProfileError::UnknownPlugin { .. })
    ));

    let context = rsi_meta::Runtime::default().root();
    assert!(catalog.isolate(context, &IsolationSpec::default()).is_ok());
}

#[derive(Debug, Default)]
struct Probe {
    activations: AtomicUsize,
    active: Mutex<HashMap<String, usize>>,
    changed: Notify,
}

impl Probe {
    fn active(&self, marker: &str) -> usize {
        self.active
            .lock()
            .expect("probe state poisoned")
            .get(marker)
            .copied()
            .unwrap_or(0)
    }

    async fn wait_active(&self, marker: &str, expected: usize) {
        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                if self.active(marker) == expected {
                    return;
                }
                self.changed.notified().await;
            }
        })
        .await
        .expect("probe active count did not converge");
    }
}

#[derive(Debug)]
struct ProbeFactory {
    probe: Arc<Probe>,
}

#[derive(Debug, Default)]
struct BuildGate {
    entered: AtomicUsize,
    active: AtomicUsize,
    maximum_active: AtomicUsize,
    changed: Notify,
    release: CancellationToken,
}

#[derive(Debug)]
struct ActiveBuild<'a> {
    gate: &'a BuildGate,
}

impl Drop for ActiveBuild<'_> {
    fn drop(&mut self) {
        self.gate.active.fetch_sub(1, Ordering::AcqRel);
        self.gate.changed.notify_waiters();
    }
}

impl BuildGate {
    async fn wait_entered(&self, expected: usize) {
        loop {
            let changed = self.changed.notified();
            if self.entered.load(Ordering::Acquire) >= expected {
                return;
            }
            changed.await;
        }
    }
}

#[derive(Debug)]
struct BlockingFactory {
    gate: Arc<BuildGate>,
}

#[async_trait::async_trait]
impl PluginFactory for BlockingFactory {
    fn prepare(&self, desired: &ConfigValue) -> rsi_meta::Result<PreparedActivation> {
        Ok(PreparedActivation::new(desired.clone()).requiring_local::<ToolRegistrarContract>())
    }

    async fn activate(&self, plan: ActivationPlan) -> rsi_meta::Result<()> {
        let _registrar = plan.local::<ToolRegistrarContract>()?;
        let active = self.gate.active.fetch_add(1, Ordering::AcqRel) + 1;
        let _active = ActiveBuild { gate: &self.gate };
        self.gate.maximum_active.fetch_max(active, Ordering::AcqRel);
        self.gate.entered.fetch_add(1, Ordering::AcqRel);
        self.gate.changed.notify_waiters();
        self.gate.release.cancelled().await;
        Ok(())
    }
}

#[derive(Debug)]
struct ProbeTool;

#[async_trait::async_trait]
impl ToolExecutor for ProbeTool {
    async fn execute(
        &self,
        arguments: ConfigValue,
        _execution: ToolExecution,
    ) -> rsi_tools_protocol::Result<ToolResult> {
        ToolResult::new(arguments, vec![], false)
    }
}

#[async_trait::async_trait]
impl PluginFactory for ProbeFactory {
    fn prepare(&self, desired: &ConfigValue) -> rsi_meta::Result<PreparedActivation> {
        Ok(PreparedActivation::new(desired.clone()).requiring_local::<ToolRegistrarContract>())
    }

    async fn activate(&self, plan: ActivationPlan) -> rsi_meta::Result<()> {
        let registrar = plan.local::<ToolRegistrarContract>()?;
        if plan
            .config()
            .get("fail")
            .and_then(ConfigValue::as_bool)
            .unwrap_or(false)
        {
            return Err(MetaError::Activation("requested probe failure".into()));
        }
        let marker = plan
            .config()
            .get("marker")
            .and_then(ConfigValue::as_str)
            .ok_or_else(|| MetaError::InvalidInput("probe marker is required".into()))?
            .to_owned();
        let lease = registrar
            .register(ToolRegistration {
                definition: ToolDefinition::new(
                    format!("probe-{marker}"),
                    "composition probe",
                    true.into(),
                )
                .map_err(|error| MetaError::Activation(error.to_string()))?,
                timeout_ms: 1_000,
                executor: Arc::new(ProbeTool),
            })
            .map_err(|error| MetaError::Activation(error.to_string()))?;
        self.probe.activations.fetch_add(1, Ordering::AcqRel);
        {
            let mut active = self.probe.active.lock().expect("probe state poisoned");
            *active.entry(marker.clone()).or_default() += 1;
        }
        self.probe.changed.notify_waiters();
        let probe = Arc::clone(&self.probe);
        plan.defer(
            "retire composition test probe",
            Box::new(move || {
                Box::pin(async move {
                    drop(lease);
                    let mut active = probe.active.lock().expect("probe state poisoned");
                    let count = active
                        .get_mut(&marker)
                        .expect("active probe marker must exist");
                    *count -= 1;
                    probe.changed.notify_waiters();
                    Ok(())
                })
            }),
        )
    }
}

struct Fixture {
    _temp: TempDir,
    source: std::path::PathBuf,
    id: AgentPresetId,
    probe: Arc<Probe>,
    runtime: Runtime,
    tools_fiber: FiberHandle,
    composition_fiber: FiberHandle,
    service: Arc<dyn AgentComposition>,
}

struct BuildFixture {
    _temp: TempDir,
    ids: Vec<AgentPresetId>,
    gate: Arc<BuildGate>,
    runtime: Runtime,
    tools_fiber: FiberHandle,
    composition_fiber: FiberHandle,
    service: Arc<dyn AgentComposition>,
}

async fn activate_composition(
    presets: AgentPresetCatalog,
    contributions: AgentContributionCatalog,
) -> (Runtime, FiberHandle, FiberHandle, Arc<dyn AgentComposition>) {
    let runtime = Runtime::default();
    let tools_fiber = runtime
        .root()
        .apply(
            ResolvedFactory::linked(
                "rsi.tools",
                "test-revision",
                UpdateMode::RestartRequired,
                Arc::new(ToolsFactory),
            ),
            ConfigValue::Null,
        )
        .await
        .unwrap();
    assert!(matches!(tools_fiber.snapshot().state, FiberState::Active));
    let composition_fiber = runtime
        .root()
        .apply(
            ResolvedFactory::linked(
                "rsi.agent.composition",
                "test-revision",
                UpdateMode::RestartRequired,
                Arc::new(AgentCompositionFactory::new(
                    presets,
                    contributions,
                    ScopeRoot::new(128).unwrap(),
                )),
            ),
            ConfigValue::Null,
        )
        .await
        .unwrap();
    assert!(matches!(
        composition_fiber.snapshot().state,
        FiberState::Active
    ));
    let service = runtime
        .root()
        .lookup_local::<AgentCompositionContract>()
        .unwrap();
    (runtime, tools_fiber, composition_fiber, service)
}

fn test_compiler(temp: &TempDir) -> AgentPresetProfileCompiler {
    for directory in ["config", "state", "cache"] {
        fs::create_dir_all(temp.path().join(directory)).unwrap();
    }
    let environment = ProfileEnvironment::new(
        temp.path().join("config"),
        temp.path().join("state"),
        temp.path().join("cache"),
        "test",
        BTreeMap::new(),
    )
    .unwrap();
    AgentPresetProfileCompiler::new(
        ProfileCompiler::new(environment, ProfileLimits::default()),
        ["test.contribution", "test.blocking", "test.unknown"],
    )
}

fn current_resource_usage(runtime: &Runtime) -> [usize; 16] {
    let resources = runtime.resource_snapshot();
    [
        resources.preparations.current,
        resources.fibers.current,
        resources.retained_plugin_bytes.current,
        resources.dependency_edges.current,
        resources.services.current,
        resources.effects.current,
        resources.effect_transactions.current,
        resources.listeners.current,
        resources.capability_entries.current,
        resources.queued_capability_references.current,
        resources.service_calls.current,
        resources.buffered_message_bytes.current,
        resources.pending_message_sends.current,
        resources.reconciliations.current,
        resources.scheduler_workers.current,
        resources.cleanup_runs.current,
    ]
}

impl BuildFixture {
    async fn new(presets_count: usize) -> Self {
        let temp = tempfile::tempdir().unwrap();
        let preset_root = temp.path().join("presets");
        fs::create_dir(&preset_root).unwrap();
        let mut ids = Vec::new();
        for index in 0..presets_count {
            let id = AgentPresetId::new(format!("preset-{index}")).unwrap();
            let preset = preset_root.join(id.as_str());
            fs::create_dir(&preset).unwrap();
            fs::write(
                preset.join(COMPOSITION_FILE),
                "format = 1\n[[steps]]\nkind = \"plugin\"\nid = \"block\"\nplugin = \"test.blocking\"\n",
            )
            .unwrap();
            ids.push(id);
        }
        let root = AgentPresetRoot::new(preset_root, AgentPresetTrust::User).unwrap();
        let compiler = test_compiler(&temp);
        let presets = AgentPresetCatalog::new(
            AgentPresetCatalogConfig::new(ids[0].clone()).with_configured_root(root),
            compiler,
        )
        .unwrap();
        let gate = Arc::new(BuildGate::default());
        let contributions = AgentContributionCatalog::new([ResolvedFactory::linked(
            "test.blocking",
            "test-revision",
            UpdateMode::Replayable,
            Arc::new(BlockingFactory {
                gate: Arc::clone(&gate),
            }),
        )])
        .unwrap();
        let (runtime, tools_fiber, composition_fiber, service) =
            activate_composition(presets, contributions).await;
        Self {
            _temp: temp,
            ids,
            gate,
            runtime,
            tools_fiber,
            composition_fiber,
            service,
        }
    }

    async fn stop(self) {
        let cleanup = self.composition_fiber.dispose().await;
        assert!(cleanup.is_clean());
        drop(self.service);
        let cleanup = self.tools_fiber.dispose().await;
        assert!(cleanup.is_clean());
        let _shutdown = self.runtime.shutdown().await;
    }
}

impl Fixture {
    async fn new(source: &str) -> Self {
        let temp = tempfile::tempdir().unwrap();
        let preset_root = temp.path().join("presets");
        let id = AgentPresetId::new("default").unwrap();
        let preset_dir = preset_root.join(id.as_str());
        fs::create_dir_all(&preset_dir).unwrap();
        let source_path = preset_dir.join(COMPOSITION_FILE);
        fs::write(&source_path, source).unwrap();
        let preset_root = AgentPresetRoot::new(preset_root, AgentPresetTrust::User).unwrap();
        let compiler = test_compiler(&temp);
        let presets = AgentPresetCatalog::new(
            AgentPresetCatalogConfig::new(id.clone()).with_configured_root(preset_root),
            compiler,
        )
        .unwrap();
        let probe = Arc::new(Probe::default());
        let contributions = AgentContributionCatalog::new([ResolvedFactory::linked(
            "test.contribution",
            "test-revision",
            UpdateMode::Replayable,
            Arc::new(ProbeFactory {
                probe: Arc::clone(&probe),
            }),
        )])
        .unwrap();
        let (runtime, tools_fiber, composition_fiber, service) =
            activate_composition(presets, contributions).await;
        Self {
            _temp: temp,
            source: source_path,
            id,
            probe,
            runtime,
            tools_fiber,
            composition_fiber,
            service,
        }
    }

    fn replace_source(&self, source: &str) {
        fs::write(&self.source, source).unwrap();
    }

    async fn stop(self) {
        let _cleanup = self.composition_fiber.dispose().await;
        drop(self.service);
        let _cleanup = self.tools_fiber.dispose().await;
        let _shutdown = self.runtime.shutdown().await;
    }
}

fn profile(marker: &str) -> String {
    format!(
        "format = 1\n[[steps]]\nkind = \"plugin\"\nid = \"probe\"\nplugin = \"test.contribution\"\nconfig = {{ marker = \"{marker}\" }}\n"
    )
}

fn failing_profile(marker: &str) -> String {
    format!(
        "format = 1\n[[steps]]\nkind = \"plugin\"\nid = \"probe\"\nplugin = \"test.contribution\"\nconfig = {{ marker = \"{marker}\", fail = true }}\n"
    )
}

#[derive(Debug, Default)]
struct MutableDefaultStore {
    selected: Mutex<Option<AgentPresetId>>,
}

#[async_trait::async_trait]
impl AgentPresetDefaultStore for MutableDefaultStore {
    async fn load(&self) -> rsi_agent_presets::Result<Option<AgentPresetId>> {
        Ok(self.selected.lock().unwrap().clone())
    }

    async fn replace(&self, selected: Option<AgentPresetId>) -> rsi_agent_presets::Result<()> {
        *self.selected.lock().unwrap() = selected;
        Ok(())
    }
}

#[tokio::test]
async fn default_identity_comes_from_the_compositions_settings_backed_catalog() {
    let temp = tempfile::tempdir().unwrap();
    let preset_root = temp.path().join("presets");
    let alpha = AgentPresetId::new("alpha").unwrap();
    let beta = AgentPresetId::new("beta").unwrap();
    for id in [&alpha, &beta] {
        let directory = preset_root.join(id.as_str());
        fs::create_dir_all(&directory).unwrap();
        fs::write(directory.join(COMPOSITION_FILE), profile(id.as_str())).unwrap();
    }
    let defaults = Arc::new(MutableDefaultStore::default());
    let compiler = test_compiler(&temp);
    let presets = AgentPresetCatalog::with_default_store(
        AgentPresetCatalogConfig::new(alpha.clone()).with_configured_root(
            AgentPresetRoot::new(&preset_root, AgentPresetTrust::User).unwrap(),
        ),
        defaults,
        compiler,
    )
    .unwrap();
    let catalog_authority = presets.clone();
    let contributions =
        AgentContributionCatalog::new(std::iter::empty::<ResolvedFactory>()).unwrap();
    let (runtime, tools_fiber, composition_fiber, service) =
        activate_composition(presets, contributions).await;

    assert_eq!(service.default_preset_id().await.unwrap(), alpha);
    catalog_authority.set_default(&beta).await.unwrap();
    assert_eq!(service.default_preset_id().await.unwrap(), beta);

    assert!(composition_fiber.dispose().await.is_clean());
    drop(service);
    assert!(tools_fiber.dispose().await.is_clean());
    let _shutdown = runtime.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_same_digest_pins_build_one_generation() {
    let fixture = Fixture::new(&profile("a")).await;
    let mut tasks = Vec::new();
    for _ in 0..16 {
        let service = Arc::clone(&fixture.service);
        let id = fixture.id.clone();
        tasks.push(tokio::spawn(async move { service.pin(&id).await.unwrap() }));
    }
    let mut pins = Vec::new();
    for task in tasks {
        pins.push(task.await.unwrap());
    }

    assert_eq!(fixture.probe.activations.load(Ordering::Acquire), 1);
    assert!(
        pins.windows(2)
            .all(|pair| pair[0].source_digest() == pair[1].source_digest())
    );

    drop(pins);
    fixture.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 12)]
async fn builds_across_presets_never_exceed_the_global_limit() {
    let fixture = BuildFixture::new(MAXIMUM_CONCURRENT_BUILDS + 1).await;
    let mut tasks = Vec::new();
    for id in &fixture.ids {
        let service = Arc::clone(&fixture.service);
        let id = id.clone();
        tasks.push(tokio::spawn(async move { service.pin(&id).await.unwrap() }));
    }

    fixture.gate.wait_entered(MAXIMUM_CONCURRENT_BUILDS).await;
    assert!(
        tokio::time::timeout(
            Duration::from_millis(100),
            fixture.gate.wait_entered(MAXIMUM_CONCURRENT_BUILDS + 1),
        )
        .await
        .is_err()
    );
    assert_eq!(
        fixture.gate.maximum_active.load(Ordering::Acquire),
        MAXIMUM_CONCURRENT_BUILDS
    );

    fixture.gate.release.cancel();
    for task in tasks {
        drop(task.await.unwrap());
    }
    fixture.stop().await;
}

#[tokio::test]
async fn dropping_a_build_future_rolls_back_its_scope_and_profile_fibers() {
    let fixture = BuildFixture::new(1).await;
    let resources = current_resource_usage(&fixture.runtime);
    let baseline_fibers = fixture.runtime.resource_snapshot().fibers.current;
    let service = Arc::clone(&fixture.service);
    let id = fixture.ids[0].clone();
    let build = tokio::spawn(async move { service.pin(&id).await });

    fixture.gate.wait_entered(1).await;
    assert!(
        fixture.runtime.resource_snapshot().fibers.current > baseline_fibers,
        "the cancellation point must be after generation Fibers exist"
    );
    build.abort();
    assert!(build.await.unwrap_err().is_cancelled());

    let quiescence = tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            if fixture.gate.active.load(Ordering::Acquire) == 0
                && current_resource_usage(&fixture.runtime) == resources
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await;
    assert!(
        quiescence.is_ok(),
        "abandoned Agent generation did not return to its resource baseline: active={}, baseline={resources:?}, current={:?}",
        fixture.gate.active.load(Ordering::Acquire),
        current_resource_usage(&fixture.runtime),
    );

    fixture.gate.release.cancel();
    let retry = tokio::time::timeout(Duration::from_secs(2), fixture.service.pin(&fixture.ids[0]))
        .await
        .expect("a cancelled build left later admission stuck")
        .expect("a cancelled build poisoned the preset row or Tool stage capacity");
    drop(retry);

    fixture.stop().await;
}

#[tokio::test]
async fn provider_shutdown_cancels_and_joins_an_inflight_generation_build() {
    let fixture = BuildFixture::new(1).await;
    let service = Arc::clone(&fixture.service);
    let id = fixture.ids[0].clone();
    let build = tokio::spawn(async move { service.pin(&id).await });
    fixture.gate.wait_entered(1).await;

    let cleanup = tokio::time::timeout(Duration::from_secs(2), fixture.composition_fiber.dispose())
        .await
        .expect("provider shutdown did not join its cancelled build");
    assert!(cleanup.is_clean());
    assert_eq!(
        build.await.unwrap().unwrap_err(),
        AgentCompositionError::ShuttingDown
    );
    assert_eq!(fixture.gate.active.load(Ordering::Acquire), 0);
    assert_eq!(fixture.runtime.resource_snapshot().cleanup_runs.current, 0);

    drop(fixture.service);
    assert!(fixture.tools_fiber.dispose().await.is_clean());
    let shutdown = fixture.runtime.shutdown().await;
    assert!(shutdown.is_clean());
}

#[tokio::test]
async fn failed_idle_preset_rows_cannot_exhaust_healthy_preset_admission() {
    let fixture = Fixture::new(&profile("a")).await;
    for index in 0..MAXIMUM_CURRENT_PRESETS {
        let id = AgentPresetId::new(format!("missing-{index}")).unwrap();
        assert!(matches!(
            fixture.service.pin(&id).await,
            Err(AgentCompositionError::Unavailable { .. })
        ));
    }
    let healthy = fixture.service.pin(&fixture.id).await.unwrap();
    assert_eq!(healthy.tools().definitions()[0].name(), "probe-a");
    drop(healthy);

    fixture.stop().await;
}

#[tokio::test]
async fn changed_source_publishes_b_while_a_pin_stays_active() {
    let fixture = Fixture::new(&profile("a")).await;
    let a = fixture.service.pin(&fixture.id).await.unwrap();
    fixture.replace_source(&profile("b"));
    let b = fixture.service.pin(&fixture.id).await.unwrap();

    assert_ne!(a.source_digest(), b.source_digest());
    assert_eq!(a.tools().definitions()[0].name(), "probe-a");
    assert_eq!(b.tools().definitions()[0].name(), "probe-b");
    assert_eq!(fixture.probe.active("a"), 1);
    assert_eq!(fixture.probe.active("b"), 1);

    drop(a);
    fixture.probe.wait_active("a", 0).await;
    drop(b);
    fixture.stop().await;
}

#[tokio::test]
async fn failed_replacement_returns_error_without_replacing_cached_generation() {
    let fixture = Fixture::new(&profile("a")).await;
    let a = fixture.service.pin(&fixture.id).await.unwrap();
    fixture.replace_source(&failing_profile("b"));
    assert!(matches!(
        fixture.service.pin(&fixture.id).await,
        Err(AgentCompositionError::Unavailable { .. })
    ));
    assert_eq!(fixture.probe.active("a"), 1);
    assert_eq!(fixture.probe.active("b"), 0);

    fixture.replace_source(&profile("a"));
    let restored = fixture.service.pin(&fixture.id).await.unwrap();
    assert_eq!(a.source_digest(), restored.source_digest());
    assert_eq!(fixture.probe.activations.load(Ordering::Acquire), 1);

    drop((a, restored));
    fixture.stop().await;
}

#[tokio::test]
async fn deleting_current_source_returns_error_instead_of_falling_back() {
    let fixture = Fixture::new(&profile("a")).await;
    let a = fixture.service.pin(&fixture.id).await.unwrap();
    fs::remove_file(&fixture.source).unwrap();

    assert!(matches!(
        fixture.service.pin(&fixture.id).await,
        Err(AgentCompositionError::Unavailable { .. })
    ));
    assert_eq!(fixture.probe.active("a"), 1);

    drop(a);
    fixture.stop().await;
}

#[tokio::test]
async fn superseded_generation_is_disposed_after_its_last_pin_drops() {
    let fixture = Fixture::new(&profile("a")).await;
    let a = fixture.service.pin(&fixture.id).await.unwrap();
    fixture.replace_source(&profile("b"));
    let b = fixture.service.pin(&fixture.id).await.unwrap();

    assert_eq!(fixture.probe.active("a"), 1);
    drop(a);
    fixture.probe.wait_active("a", 0).await;
    assert_eq!(fixture.probe.active("b"), 1);

    drop(b);
    fixture.stop().await;
}

#[tokio::test]
async fn unknown_factory_is_rejected_before_any_runtime_mutation() {
    let fixture = Fixture::new(
        "format = 1\n[[steps]]\nkind = \"plugin\"\nid = \"unknown\"\nplugin = \"test.unknown\"\n",
    )
    .await;
    let before = fixture.runtime.snapshot();
    let resources = fixture.runtime.resource_snapshot();

    let error = fixture.service.pin(&fixture.id).await.unwrap_err();
    assert_eq!(
        error,
        AgentCompositionError::Unavailable {
            preset_id: fixture.id.clone(),
            reason: "Profile references an unknown Agent contribution".into(),
        }
    );
    assert_eq!(fixture.runtime.snapshot(), before);
    assert_eq!(fixture.runtime.resource_snapshot(), resources);
    assert_eq!(fixture.probe.activations.load(Ordering::Acquire), 0);

    fixture.stop().await;
}

#[tokio::test]
async fn provider_shutdown_waits_for_the_last_external_pin_before_disposing_its_scope() {
    let fixture = Fixture::new(&profile("a")).await;
    let pin = fixture.service.pin(&fixture.id).await.unwrap();
    assert_eq!(fixture.probe.active("a"), 1);

    let composition_fiber = fixture.composition_fiber.clone();
    let mut shutdown = tokio::spawn(async move { composition_fiber.dispose().await });
    assert!(
        tokio::time::timeout(Duration::from_millis(100), &mut shutdown)
            .await
            .is_err(),
        "provider shutdown must wait while an external generation pin exists"
    );
    assert_eq!(fixture.probe.active("a"), 1);
    assert!(matches!(
        fixture.service.pin(&fixture.id).await,
        Err(AgentCompositionError::ShuttingDown)
    ));

    drop(pin);
    let cleanup = tokio::time::timeout(Duration::from_secs(2), shutdown)
        .await
        .expect("provider shutdown did not observe the final pin release")
        .unwrap();
    assert!(cleanup.is_clean());
    fixture.probe.wait_active("a", 0).await;
    drop(fixture.service);
    let _cleanup = fixture.tools_fiber.dispose().await;
    let _shutdown = fixture.runtime.shutdown().await;
}
