use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use rsi_meta::{
    ActivationPlan, CleanupFuture, ConfigValue, Context, ContractVersion, FactoryIdentity,
    FiberHandle, FiberSnapshot, FiberState, InvocationContext, IsolationId, Message, MetaError,
    PendingReason, PluginFactory, PreparedActivation, ProviderChannel, Requirement,
    ResourceUsageSnapshot, Result, Runtime, RuntimeLimits, RuntimeResourceSnapshot,
    ServiceEndpoint, SupplyHandle, TopologyLimits,
};
use serde_json::Value;
use tokio::sync::Notify;

const V1: ContractVersion = ContractVersion(1);
const POPULATIONS: [usize; 3] = [1_024, 2_048, 4_096];
const ABSOLUTE_LIMIT: Duration = Duration::from_secs(5);
const DENSE_PROVIDERS: usize = 256;
const DENSE_CONSUMERS: usize = 256;
const DENSE_FIBERS: usize = 4_096;
const DENSE_EDGES: usize = DENSE_PROVIDERS * DENSE_CONSUMERS;
const DENSE_EDGE_LIMIT: usize = DENSE_EDGES * 2;

#[derive(Debug)]
struct MissingFactory;

#[async_trait]
impl PluginFactory for MissingFactory {
    fn identity(&self) -> FactoryIdentity {
        FactoryIdentity::builtin("foundation-probe-missing", "1")
    }

    fn prepare(&self, desired: &ConfigValue) -> Result<PreparedActivation> {
        Ok(
            PreparedActivation::new(desired.clone()).requiring(Requirement::new(
                "never-published",
                "fixture.never-published",
                V1,
            )),
        )
    }

    async fn activate(&self, _: ActivationPlan) -> Result<()> {
        panic!("a factory with an actual missing requirement must not activate")
    }
}

#[derive(Debug)]
struct EchoEndpoint;

#[async_trait]
impl ServiceEndpoint for EchoEndpoint {
    async fn serve(&self, _: InvocationContext, mut channel: ProviderChannel<'_>) -> Result<()> {
        while let Some(message) = channel.recv().await {
            channel.send(message).await?;
        }
        Ok(())
    }
}

#[derive(Debug)]
struct PreparedFactory {
    identity: FactoryIdentity,
    requirements: Vec<Requirement>,
    activations: Option<Arc<AtomicUsize>>,
}

impl PreparedFactory {
    fn new(identity: FactoryIdentity) -> Self {
        Self {
            identity,
            requirements: Vec::new(),
            activations: None,
        }
    }

    fn requiring(mut self, requirement: Requirement) -> Self {
        self.requirements.push(requirement);
        self
    }

    fn requiring_all(mut self, requirements: Vec<Requirement>) -> Self {
        self.requirements = requirements;
        self
    }

    fn counting(mut self, activations: Arc<AtomicUsize>) -> Self {
        self.activations = Some(activations);
        self
    }
}

#[async_trait]
impl PluginFactory for PreparedFactory {
    fn identity(&self) -> FactoryIdentity {
        self.identity.clone()
    }

    fn prepare(&self, desired: &ConfigValue) -> Result<PreparedActivation> {
        let mut prepared = PreparedActivation::new(desired.clone());
        for requirement in &self.requirements {
            prepared = prepared.requiring(requirement.clone());
        }
        Ok(prepared)
    }

    async fn activate(&self, _: ActivationPlan) -> Result<()> {
        if let Some(activations) = &self.activations {
            activations.fetch_add(1, Ordering::AcqRel);
        }
        Ok(())
    }
}

#[derive(Debug)]
struct DenseProvider {
    identity: FactoryIdentity,
    service: String,
    cleanup_latch: Option<(Arc<Notify>, Arc<Notify>)>,
}

#[async_trait]
impl PluginFactory for DenseProvider {
    fn identity(&self) -> FactoryIdentity {
        self.identity.clone()
    }

    fn prepare(&self, desired: &ConfigValue) -> Result<PreparedActivation> {
        Ok(PreparedActivation::new(desired.clone()))
    }

    async fn activate(&self, plan: ActivationPlan) -> Result<()> {
        let _supply =
            plan.context()
                .provide(&self.service, "fixture.dense", V1, Arc::new(EchoEndpoint))?;
        if let Some((entered, release)) = self.cleanup_latch.clone() {
            plan.defer(
                "dense withdrawal overlap",
                Box::new(move || -> CleanupFuture {
                    Box::pin(async move {
                        entered.notify_one();
                        release.notified().await;
                        Ok(())
                    })
                }),
            )?;
        }
        Ok(())
    }
}

#[derive(Debug)]
struct ProviderCapture {
    context: Context,
    initial_supply: SupplyHandle,
    self_response: Vec<u8>,
}

#[derive(Debug)]
struct BlockingProvider {
    identity: FactoryIdentity,
    activation_entered: Arc<Notify>,
    activation_release: Arc<Notify>,
    cleanup_entered: Arc<Notify>,
    cleanup_release: Arc<Notify>,
    captured: Arc<Mutex<Option<ProviderCapture>>>,
}

#[async_trait]
impl PluginFactory for BlockingProvider {
    fn identity(&self) -> FactoryIdentity {
        self.identity.clone()
    }

    fn prepare(&self, desired: &ConfigValue) -> Result<PreparedActivation> {
        Ok(PreparedActivation::new(desired.clone()))
    }

    async fn activate(&self, plan: ActivationPlan) -> Result<()> {
        let context = plan.context().clone();
        let initial_supply = context.provide(
            "foundation-service",
            "fixture.foundation",
            V1,
            Arc::new(EchoEndpoint),
        )?;
        let self_response = context
            .service("foundation-service")?
            .invoke(Message::new(b"loading-self".as_slice()))
            .await?
            .as_bytes()
            .to_vec();
        let cleanup_entered = Arc::clone(&self.cleanup_entered);
        let cleanup_release = Arc::clone(&self.cleanup_release);
        plan.defer(
            "observable foundation cleanup",
            Box::new(move || -> CleanupFuture {
                Box::pin(async move {
                    cleanup_entered.notify_one();
                    cleanup_release.notified().await;
                    Ok(())
                })
            }),
        )?;
        *self.captured.lock().expect("provider capture poisoned") = Some(ProviderCapture {
            context,
            initial_supply,
            self_response,
        });
        self.activation_entered.notify_one();
        self.activation_release.notified().await;
        Ok(())
    }
}

async fn wait_for_state(
    handle: &FiberHandle,
    label: &str,
    predicate: impl Fn(&FiberState) -> bool,
) -> FiberSnapshot {
    let mut snapshots = handle.subscribe();
    tokio::time::timeout(ABSOLUTE_LIMIT, async {
        loop {
            let snapshot = snapshots.borrow().clone();
            if predicate(&snapshot.state) {
                return snapshot;
            }
            snapshots
                .changed()
                .await
                .expect("a live Fiber keeps its snapshot stream open");
        }
    })
    .await
    .unwrap_or_else(|_| panic!("{label} did not converge within {ABSOLUTE_LIMIT:?}"))
}

fn assert_missing(handle: &FiberHandle, expected: &str) {
    let snapshot = handle.snapshot();
    let FiberState::Pending(report) = snapshot.state else {
        panic!("{expected} requirement was not observably Pending");
    };
    assert_eq!(report.total_reasons, 1);
    assert!(!report.truncated);
    assert!(matches!(
        report.reasons.as_slice(),
        [PendingReason::MissingService { service, .. }] if service.as_ref() == expected
    ));
}

fn resource_usages(
    resources: &RuntimeResourceSnapshot,
) -> [(&'static str, &ResourceUsageSnapshot); 17] {
    [
        ("preparations", &resources.preparations),
        ("fibers", &resources.fibers),
        ("retained_plugin_bytes", &resources.retained_plugin_bytes),
        ("dependency_edges", &resources.dependency_edges),
        ("services", &resources.services),
        ("effects", &resources.effects),
        ("effect_transactions", &resources.effect_transactions),
        ("listeners", &resources.listeners),
        ("capability_entries", &resources.capability_entries),
        (
            "queued_capability_references",
            &resources.queued_capability_references,
        ),
        ("service_calls", &resources.service_calls),
        ("buffered_message_bytes", &resources.buffered_message_bytes),
        ("reconciliations", &resources.reconciliations),
        ("scheduler_workers", &resources.scheduler_workers),
        ("event_dispatches", &resources.event_dispatches),
        ("event_callbacks", &resources.event_callbacks),
        ("cleanup_runs", &resources.cleanup_runs),
    ]
}

fn assert_resource_bounds(resources: &RuntimeResourceSnapshot) {
    for (name, usage) in resource_usages(resources) {
        assert!(
            usage.current <= usage.limit,
            "{name} current usage exceeded its configured bound"
        );
        assert!(
            usage.high_watermark <= usage.limit,
            "{name} high-water mark exceeded its configured bound"
        );
    }
}

fn assert_resources_zero(resources: &RuntimeResourceSnapshot) {
    for (name, usage) in resource_usages(resources) {
        assert_eq!(usage.current, 0, "{name} remained retained after shutdown");
    }
}

fn assert_dense_convergence(runtime: &Runtime) {
    let snapshot = runtime.snapshot();
    let active = snapshot
        .fibers
        .iter()
        .filter(|fiber| matches!(fiber.state, FiberState::Active))
        .count();
    let pending = snapshot
        .fibers
        .iter()
        .filter(|fiber| matches!(fiber.state, FiberState::Pending(_)))
        .count();
    let resources = runtime.resource_snapshot();
    println!(
        "dense convergence: active={active} pending={pending} services_current={} edges_current={} edges_peak={} edges_rejected={}",
        resources.services.current,
        resources.dependency_edges.current,
        resources.dependency_edges.high_watermark,
        resources.dependency_edges.rejected,
    );
    assert_eq!(active, DENSE_FIBERS - DENSE_CONSUMERS - 1);
    assert_eq!(pending, DENSE_CONSUMERS);
    assert_eq!(resources.services.current, DENSE_PROVIDERS - 1);
    assert_eq!(resources.dependency_edges.current, DENSE_EDGES);
    assert_eq!(resources.dependency_edges.rejected, 0);
}

#[allow(clippy::too_many_lines)] // One public lifecycle trace keeps its synchronization and ownership proof adjacent.
async fn exercise_foundation_lifecycle() -> RuntimeResourceSnapshot {
    let runtime = Runtime::default();
    let root = runtime.root();
    let activations = Arc::new(AtomicUsize::new(0));
    let consumer = root
        .apply(
            Arc::new(
                PreparedFactory::new(FactoryIdentity::builtin("foundation-consumer", "1"))
                    .requiring(Requirement::new(
                        "foundation-service",
                        "fixture.foundation",
                        V1,
                    ))
                    .counting(Arc::clone(&activations)),
            ),
            Value::Null,
        )
        .await
        .expect("an actual missing service produces a valid Pending Fiber");
    assert_missing(&consumer, "foundation-service");
    assert_eq!(activations.load(Ordering::Acquire), 0);

    let provider_identity = FactoryIdentity::builtin("foundation-provider", "1");
    let activation_entered = Arc::new(Notify::new());
    let activation_release = Arc::new(Notify::new());
    let cleanup_entered = Arc::new(Notify::new());
    let cleanup_release = Arc::new(Notify::new());
    let captured = Arc::new(Mutex::new(None));
    let provider_application = tokio::spawn({
        let root = root.clone();
        let provider_identity = provider_identity.clone();
        let activation_entered = Arc::clone(&activation_entered);
        let activation_release = Arc::clone(&activation_release);
        let cleanup_entered = Arc::clone(&cleanup_entered);
        let cleanup_release = Arc::clone(&cleanup_release);
        let captured = Arc::clone(&captured);
        async move {
            root.apply(
                Arc::new(BlockingProvider {
                    identity: provider_identity,
                    activation_entered,
                    activation_release,
                    cleanup_entered,
                    cleanup_release,
                    captured,
                }),
                Value::Null,
            )
            .await
        }
    });
    tokio::time::timeout(ABSOLUTE_LIMIT, activation_entered.notified())
        .await
        .expect("provider did not enter blocked Loading activation");

    let loading = runtime.snapshot();
    assert!(loading.fibers.iter().any(|fiber| {
        fiber.factory == provider_identity && matches!(fiber.state, FiberState::Loading)
    }));
    assert_missing(&consumer, "foundation-service");
    assert_eq!(activations.load(Ordering::Acquire), 0);
    let loading_resources = runtime.resource_snapshot();
    assert_eq!(loading_resources.services.current, 1);
    assert_resource_bounds(&loading_resources);

    activation_release.notify_one();
    let provider = tokio::time::timeout(ABSOLUTE_LIMIT, provider_application)
        .await
        .expect("provider application exceeded the absolute deadline")
        .expect("provider application task remained healthy")
        .expect("provider activation succeeded");
    wait_for_state(&consumer, "consumer activation", |state| {
        matches!(state, FiberState::Active)
    })
    .await;
    assert_eq!(activations.load(Ordering::Acquire), 1);

    let ProviderCapture {
        context,
        initial_supply,
        self_response,
    } = captured
        .lock()
        .expect("provider capture poisoned")
        .take()
        .expect("provider activation captured its public handles");
    assert_eq!(self_response, b"loading-self");
    assert!(initial_supply.dispose().await.is_clean());
    wait_for_state(&consumer, "consumer withdrawal", |state| {
        matches!(state, FiberState::Pending(_))
    })
    .await;
    assert_missing(&consumer, "foundation-service");
    assert_eq!(activations.load(Ordering::Acquire), 1);
    assert!(matches!(provider.snapshot().state, FiberState::Active));
    assert_eq!(runtime.resource_snapshot().services.current, 0);

    let replacement = context
        .provide(
            "foundation-service",
            "fixture.foundation",
            V1,
            Arc::new(EchoEndpoint),
        )
        .expect("an Active generation can dynamically re-provide its service");
    wait_for_state(&consumer, "consumer reactivation", |state| {
        matches!(state, FiberState::Active)
    })
    .await;
    assert_eq!(activations.load(Ordering::Acquire), 2);
    assert_ne!(replacement.id(), initial_supply.id());
    drop(initial_supply);

    let provider_disposal = tokio::spawn({
        let provider = provider.clone();
        async move { provider.dispose().await }
    });
    tokio::time::timeout(ABSOLUTE_LIMIT, cleanup_entered.notified())
        .await
        .expect("provider did not enter its owned cleanup effect");
    assert!(matches!(provider.snapshot().state, FiberState::Unloading));
    cleanup_release.notify_one();
    let report = tokio::time::timeout(ABSOLUTE_LIMIT, provider_disposal)
        .await
        .expect("provider disposal exceeded the absolute deadline")
        .expect("provider disposal task remained healthy");
    assert!(report.is_clean());
    wait_for_state(&consumer, "consumer final withdrawal", |state| {
        matches!(state, FiberState::Pending(_))
    })
    .await;
    assert_missing(&consumer, "foundation-service");
    assert!(replacement.dispose().await.is_clean());
    assert!(consumer.dispose().await.is_clean());
    drop(replacement);
    drop(context);

    assert!(runtime.shutdown().await.is_complete());
    let resources = runtime.resource_snapshot();
    assert_resource_bounds(&resources);
    assert_resources_zero(&resources);
    resources
}

async fn measure_missing(population: usize) -> (Duration, RuntimeResourceSnapshot) {
    let runtime = Runtime::default();
    let root = runtime.root();
    let factory: Arc<dyn PluginFactory> = Arc::new(MissingFactory);
    let mut fibers = Vec::with_capacity(population);
    let started = Instant::now();
    for _ in 0..population {
        fibers.push(
            root.apply(Arc::clone(&factory), Value::Null)
                .await
                .expect("the bounded missing requirement remains a valid Pending Fiber"),
        );
    }
    let elapsed = started.elapsed();
    assert_eq!(runtime.snapshot().fibers.len(), population);
    assert_missing(
        fibers
            .last()
            .expect("the probe populations are all nonempty"),
        "never-published",
    );
    let resources = runtime.resource_snapshot();
    assert_eq!(resources.fibers.current, population);
    assert_eq!(resources.dependency_edges.current, population);
    assert_resource_bounds(&resources);
    drop(fibers);
    assert!(runtime.shutdown().await.is_complete());
    assert_resources_zero(&runtime.resource_snapshot());
    (elapsed, resources)
}

fn measure_context_scope(population: usize) -> Duration {
    let runtime = Runtime::new(RuntimeLimits {
        topology: TopologyLimits {
            maximum_context_entries: population,
            ..TopologyLimits::default()
        },
        ..RuntimeLimits::default()
    })
    .expect("probe limits are valid");
    let mut context = runtime.root();
    let started = Instant::now();
    for index in 0..population {
        context = context
            .isolate(
                format!("scope-{index}"),
                IsolationId(u64::try_from(index).expect("probe population fits u64") + 1),
            )
            .expect("probe stays within its configured Context entry bound");
    }
    std::hint::black_box(&context);
    started.elapsed()
}

fn report_scaling(label: &str, observations: &[Duration]) {
    for pair in observations.windows(2) {
        println!(
            "{label} scaling {:?} -> {:?} ratio={:.3}",
            pair[0],
            pair[1],
            pair[1].as_secs_f64() / pair[0].as_secs_f64(),
        );
    }
}

async fn dense_runtime() -> (
    Runtime,
    FiberHandle,
    Vec<FiberHandle>,
    Arc<Notify>,
    Arc<Notify>,
) {
    let runtime = Runtime::new(RuntimeLimits {
        topology: TopologyLimits {
            maximum_fibers: DENSE_FIBERS,
            maximum_services: DENSE_PROVIDERS,
            maximum_dependency_edges: DENSE_EDGE_LIMIT,
            maximum_requirements_per_fiber: DENSE_PROVIDERS,
            ..TopologyLimits::default()
        },
        ..RuntimeLimits::default()
    })
    .expect("dense probe limits are valid");
    let root = runtime.root();
    let cleanup_entered = Arc::new(Notify::new());
    let cleanup_release = Arc::new(Notify::new());
    let mut providers = Vec::with_capacity(DENSE_PROVIDERS);
    for index in 0..DENSE_PROVIDERS {
        let service = format!("dense-{index}");
        providers.push(
            root.apply(
                Arc::new(DenseProvider {
                    identity: FactoryIdentity::builtin(format!("dense-provider-{index}"), "1"),
                    service,
                    cleanup_latch: (index == 0)
                        .then(|| (Arc::clone(&cleanup_entered), Arc::clone(&cleanup_release))),
                }),
                Value::Null,
            )
            .await
            .expect("dense provider activates from its actual supply"),
        );
    }

    let filler: Arc<dyn PluginFactory> = Arc::new(PreparedFactory::new(FactoryIdentity::builtin(
        "dense-filler",
        "1",
    )));
    for _ in DENSE_PROVIDERS..(DENSE_FIBERS - DENSE_CONSUMERS) {
        root.apply(Arc::clone(&filler), Value::Null)
            .await
            .expect("filler Fiber activates");
    }

    let requirements = (0..DENSE_PROVIDERS)
        .map(|index| Requirement::new(format!("dense-{index}"), "fixture.dense", V1))
        .collect();
    let consumer: Arc<dyn PluginFactory> = Arc::new(
        PreparedFactory::new(FactoryIdentity::builtin("dense-consumer", "1"))
            .requiring_all(requirements),
    );
    let mut consumers = Vec::with_capacity(DENSE_CONSUMERS);
    for _ in 0..DENSE_CONSUMERS {
        let handle = root
            .apply(Arc::clone(&consumer), Value::Null)
            .await
            .expect("dense consumer activates from actual supplies");
        assert!(matches!(handle.snapshot().state, FiberState::Active));
        consumers.push(handle);
    }
    assert_eq!(runtime.snapshot().fibers.len(), DENSE_FIBERS);

    let rejected = root.apply(Arc::clone(&filler), Value::Null).await;
    assert!(matches!(
        rejected,
        Err(MetaError::CapacityExhausted { resource: "fibers" })
    ));
    let saturated = runtime.resource_snapshot();
    assert_eq!(saturated.fibers.current, DENSE_FIBERS);
    assert_eq!(saturated.fibers.high_watermark, DENSE_FIBERS);
    assert_eq!(saturated.fibers.rejected, 1);
    assert_eq!(saturated.services.current, DENSE_PROVIDERS);
    assert_eq!(saturated.services.high_watermark, DENSE_PROVIDERS);
    assert_eq!(saturated.dependency_edges.current, DENSE_EDGES);
    assert_eq!(saturated.dependency_edges.high_watermark, DENSE_EDGES);
    assert_resource_bounds(&saturated);

    (
        runtime,
        providers.remove(0),
        consumers,
        cleanup_entered,
        cleanup_release,
    )
}

async fn measure_dense_withdrawal() -> (Duration, RuntimeResourceSnapshot, Duration) {
    let (runtime, provider, consumers, cleanup_entered, cleanup_release) = dense_runtime().await;
    let provider_id = provider.id();
    let stop = Arc::new(AtomicBool::new(false));
    let sampler_ready = Arc::new(Notify::new());
    let overlapping_samples = Arc::new(AtomicUsize::new(0));
    let overlap_observed = Arc::new(Notify::new());
    let maximum_snapshot_latency = Arc::new(Mutex::new(Duration::ZERO));
    let sampler =
        tokio::spawn({
            let runtime = runtime.clone();
            let stop = Arc::clone(&stop);
            let ready = Arc::clone(&sampler_ready);
            let samples = Arc::clone(&overlapping_samples);
            let overlap = Arc::clone(&overlap_observed);
            let maximum = Arc::clone(&maximum_snapshot_latency);
            async move {
                let mut announced_ready = false;
                while !stop.load(Ordering::Acquire) {
                    let started = Instant::now();
                    let snapshot = std::hint::black_box(runtime.snapshot());
                    let elapsed = started.elapsed();
                    {
                        let mut observed = maximum.lock().expect("snapshot latency poisoned");
                        *observed = (*observed).max(elapsed);
                    }
                    if snapshot.fibers.iter().any(|fiber| {
                        fiber.id == provider_id && fiber.state == FiberState::Unloading
                    }) {
                        samples.fetch_add(1, Ordering::Relaxed);
                        overlap.notify_one();
                    }
                    if !announced_ready {
                        announced_ready = true;
                        ready.notify_one();
                    }
                    tokio::task::yield_now().await;
                }
            }
        });
    sampler_ready.notified().await;
    let started = Instant::now();
    let disposal = tokio::spawn(async move { provider.dispose().await });
    tokio::time::timeout(ABSOLUTE_LIMIT, cleanup_entered.notified())
        .await
        .expect("dense provider never entered its Unloading cleanup");
    tokio::time::timeout(ABSOLUTE_LIMIT, overlap_observed.notified())
        .await
        .expect("snapshot sampler never observed the dense provider in Unloading");
    cleanup_release.notify_one();
    let report = tokio::time::timeout(ABSOLUTE_LIMIT, disposal)
        .await
        .expect("dense disposal exceeded the absolute deadline")
        .expect("dense disposal task remains healthy");
    let elapsed = started.elapsed();
    stop.store(true, Ordering::Release);
    sampler.await.expect("snapshot sampler remains healthy");
    assert!(report.is_clean());
    assert!(
        overlapping_samples.load(Ordering::Relaxed) > 0,
        "snapshot sampler never overlapped pending dense disposal"
    );
    assert_dense_convergence(&runtime);
    for (index, consumer) in consumers.iter().enumerate() {
        let label = format!("dense consumer {index} withdrawal");
        wait_for_state(consumer, &label, |state| {
            matches!(state, FiberState::Pending(_))
        })
        .await;
        assert_missing(consumer, "dense-0");
    }
    let maximum_snapshot_latency = *maximum_snapshot_latency
        .lock()
        .expect("snapshot latency poisoned");
    let resources = runtime.resource_snapshot();
    assert_eq!(resources.fibers.current, DENSE_FIBERS - 1);
    assert_eq!(resources.services.current, DENSE_PROVIDERS - 1);
    assert_eq!(resources.dependency_edges.current, DENSE_EDGES);
    assert_resource_bounds(&resources);
    drop(consumers);
    assert!(runtime.shutdown().await.is_complete());
    assert_resources_zero(&runtime.resource_snapshot());
    (elapsed, resources, maximum_snapshot_latency)
}

#[tokio::main(flavor = "multi_thread", worker_threads = 4)]
async fn main() {
    let lifecycle_resources = exercise_foundation_lifecycle().await;
    println!(
        "foundation lifecycle: services_peak={} effects_peak={} reconciliations_peak={} scheduler_worker_peak={}",
        lifecycle_resources.services.high_watermark,
        lifecycle_resources.effects.high_watermark,
        lifecycle_resources.reconciliations.high_watermark,
        lifecycle_resources.scheduler_workers.high_watermark,
    );

    let mut observations = Vec::with_capacity(POPULATIONS.len());
    for population in POPULATIONS {
        let (elapsed, resources) = measure_missing(population).await;
        println!(
            "missing requirements={population:5} elapsed={elapsed:?} fibers_peak={} edges_peak={}",
            resources.fibers.high_watermark, resources.dependency_edges.high_watermark,
        );
        observations.push(elapsed);
    }
    report_scaling("unrelated Pending Fiber", &observations);

    let mut context_observations = Vec::with_capacity(POPULATIONS.len());
    for population in POPULATIONS {
        let elapsed = measure_context_scope(population);
        println!("context scopes={population:5} elapsed={elapsed:?}");
        context_observations.push(elapsed);
    }
    report_scaling("Context scope", &context_observations);

    let (dense_elapsed, resources, maximum_snapshot_latency) = measure_dense_withdrawal().await;
    println!(
        "dense fibers={DENSE_FIBERS:5} edges={DENSE_EDGES:5} elapsed={dense_elapsed:?} reconciliation_peak={} scheduler_worker_peak={} snapshot_latency_max={maximum_snapshot_latency:?}",
        resources.reconciliations.high_watermark, resources.scheduler_workers.high_watermark,
    );
    assert_eq!(
        resources.scheduler_workers.high_watermark, 1,
        "dense reconciliation created more than one Runtime scheduler worker"
    );
}
