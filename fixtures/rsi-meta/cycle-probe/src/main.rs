use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use rsi_meta::{
    CleanupFuture, ConfigValue, Context, ContractVersion, FactoryIdentity, FiberHandle, FiberState,
    InvocationContext, IsolationId, PluginDescriptor, PluginFactory, ProviderChannel, Provision,
    Requirement, Result, Runtime, RuntimeLimits, RuntimeResourceSnapshot, ServiceEndpoint,
    TopologyLimits,
};
use serde_json::Value;

const POPULATIONS: [usize; 3] = [1_024, 2_048, 4_096];
const RATIO_HEADROOM: u32 = 3;
const FIXED_HEADROOM: Duration = Duration::from_millis(50);
const ABSOLUTE_LIMIT: Duration = Duration::from_secs(5);
const DENSE_PROVIDERS: usize = 256;
const DENSE_CONSUMERS: usize = 256;
const DENSE_FIBERS: usize = 4_096;
const DENSE_EDGES: usize = DENSE_PROVIDERS * DENSE_CONSUMERS;
const SNAPSHOT_LATENCY_LIMIT: Duration = Duration::from_millis(500);

#[derive(Debug)]
struct MissingFactory;

#[async_trait]
impl PluginFactory for MissingFactory {
    fn descriptor(&self) -> &PluginDescriptor {
        static DESCRIPTOR: OnceLock<PluginDescriptor> = OnceLock::new();
        DESCRIPTOR.get_or_init(|| {
            PluginDescriptor::new(FactoryIdentity::builtin("cycle-probe-missing", "1")).requiring(
                Requirement::new(
                    "never-published",
                    "fixture.never-published",
                    ContractVersion(1),
                ),
            )
        })
    }

    async fn activate(&self, _: Context, _: Arc<ConfigValue>) -> Result<()> {
        Ok(())
    }
}

#[derive(Debug)]
struct NoopEndpoint;

#[async_trait]
impl ServiceEndpoint for NoopEndpoint {
    async fn serve(&self, _: InvocationContext, _: ProviderChannel<'_>) -> Result<()> {
        Ok(())
    }
}

#[derive(Debug)]
struct DenseProvider {
    descriptor: PluginDescriptor,
    service: String,
    cleanup_latch: Option<(Arc<tokio::sync::Notify>, Arc<tokio::sync::Notify>)>,
}

#[async_trait]
impl PluginFactory for DenseProvider {
    fn descriptor(&self) -> &PluginDescriptor {
        &self.descriptor
    }

    async fn activate(&self, context: Context, _: Arc<ConfigValue>) -> Result<()> {
        context.provide(
            self.service.clone(),
            "fixture.dense",
            ContractVersion(1),
            Arc::new(NoopEndpoint),
        )?;
        if let Some((entered, release)) = self.cleanup_latch.clone() {
            context.defer(
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
struct ActiveFactory(PluginDescriptor);

#[async_trait]
impl PluginFactory for ActiveFactory {
    fn descriptor(&self) -> &PluginDescriptor {
        &self.0
    }

    async fn activate(&self, _: Context, _: Arc<ConfigValue>) -> Result<()> {
        Ok(())
    }
}

async fn measure(population: usize) -> Duration {
    let runtime = Runtime::default();
    let root = runtime.root();
    let factory: Arc<dyn PluginFactory> = Arc::new(MissingFactory);
    let started = Instant::now();
    for _ in 0..population {
        root.apply(Arc::clone(&factory), Value::Null)
            .await
            .expect("the bounded missing dependency remains a valid pending Fiber");
    }
    let elapsed = started.elapsed();
    assert_eq!(runtime.snapshot().fibers.len(), population);
    elapsed
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

fn assert_near_linear(label: &str, observations: &[Duration]) {
    for pair in observations.windows(2) {
        let permitted = pair[0]
            .saturating_mul(RATIO_HEADROOM)
            .saturating_add(FIXED_HEADROOM);
        assert!(
            pair[1] <= permitted,
            "{label} doubling grew from {:?} to {:?}, above {:?}",
            pair[0],
            pair[1],
            permitted
        );
    }
}

async fn dense_runtime() -> (
    Runtime,
    FiberHandle,
    Arc<tokio::sync::Notify>,
    Arc<tokio::sync::Notify>,
) {
    let runtime = Runtime::new(RuntimeLimits {
        topology: TopologyLimits {
            maximum_fibers: DENSE_FIBERS,
            maximum_service_declarations: DENSE_EDGES + DENSE_PROVIDERS,
            maximum_dependency_edges: DENSE_EDGES,
            ..TopologyLimits::default()
        },
        ..RuntimeLimits::default()
    })
    .expect("dense probe limits are valid");
    let root = runtime.root();
    let cleanup_entered = Arc::new(tokio::sync::Notify::new());
    let cleanup_release = Arc::new(tokio::sync::Notify::new());
    let mut providers = Vec::with_capacity(DENSE_PROVIDERS);
    for index in 0..DENSE_PROVIDERS {
        let service = format!("dense-{index}");
        providers.push(
            root.apply(
                Arc::new(DenseProvider {
                    descriptor: PluginDescriptor::new(FactoryIdentity::builtin(
                        format!("dense-provider-{index}"),
                        "1",
                    ))
                    .providing(Provision::new(
                        service.clone(),
                        "fixture.dense",
                        ContractVersion(1),
                    )),
                    service,
                    cleanup_latch: (index == 0)
                        .then(|| (Arc::clone(&cleanup_entered), Arc::clone(&cleanup_release))),
                }),
                Value::Null,
            )
            .await
            .expect("dense provider activates"),
        );
    }

    let mut consumer_descriptor =
        PluginDescriptor::new(FactoryIdentity::builtin("dense-consumer", "1"));
    for index in 0..DENSE_PROVIDERS {
        consumer_descriptor = consumer_descriptor.requiring(Requirement::new(
            format!("dense-{index}"),
            "fixture.dense",
            ContractVersion(1),
        ));
    }
    let consumer: Arc<dyn PluginFactory> = Arc::new(ActiveFactory(consumer_descriptor));
    for _ in 0..DENSE_CONSUMERS {
        root.apply(Arc::clone(&consumer), Value::Null)
            .await
            .expect("dense consumer activates");
    }
    let filler: Arc<dyn PluginFactory> = Arc::new(ActiveFactory(PluginDescriptor::new(
        FactoryIdentity::builtin("dense-filler", "1"),
    )));
    for _ in (DENSE_PROVIDERS + DENSE_CONSUMERS)..DENSE_FIBERS {
        root.apply(Arc::clone(&filler), Value::Null)
            .await
            .expect("filler Fiber activates");
    }
    assert_eq!(runtime.snapshot().fibers.len(), DENSE_FIBERS);
    (
        runtime,
        providers.remove(0),
        cleanup_entered,
        cleanup_release,
    )
}

async fn measure_dense_withdrawal() -> (Duration, RuntimeResourceSnapshot, Duration) {
    let (runtime, provider, cleanup_entered, cleanup_release) = dense_runtime().await;
    let provider_id = provider.id();
    let stop = Arc::new(AtomicBool::new(false));
    let sampler_ready = Arc::new(tokio::sync::Notify::new());
    let overlapping_samples = Arc::new(AtomicUsize::new(0));
    let overlap_observed = Arc::new(tokio::sync::Notify::new());
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
    let maximum_snapshot_latency = *maximum_snapshot_latency
        .lock()
        .expect("snapshot latency poisoned");
    (
        elapsed,
        runtime.resource_snapshot(),
        maximum_snapshot_latency,
    )
}

#[tokio::main(flavor = "multi_thread", worker_threads = 4)]
async fn main() {
    let mut observations = Vec::with_capacity(POPULATIONS.len());
    for population in POPULATIONS {
        let elapsed = measure(population).await;
        println!("population={population:5} elapsed={elapsed:?}");
        assert!(
            elapsed <= ABSOLUTE_LIMIT,
            "reachable-only cycle probe exceeded {ABSOLUTE_LIMIT:?} at {population} Fibers"
        );
        observations.push(elapsed);
    }
    assert_near_linear("unrelated pending Fiber", &observations);

    let mut context_observations = Vec::with_capacity(POPULATIONS.len());
    for population in POPULATIONS {
        let elapsed = measure_context_scope(population);
        println!("context scopes={population:5} elapsed={elapsed:?}");
        assert!(
            elapsed <= ABSOLUTE_LIMIT,
            "Context scope probe exceeded {ABSOLUTE_LIMIT:?} at {population} entries"
        );
        context_observations.push(elapsed);
    }
    assert_near_linear("Context scope", &context_observations);

    let (dense_elapsed, resources, maximum_snapshot_latency) = measure_dense_withdrawal().await;
    println!(
        "dense fibers={DENSE_FIBERS:5} edges={DENSE_EDGES:5} elapsed={dense_elapsed:?} reconciliation_peak={} scheduler_worker_peak={} snapshot_latency_max={maximum_snapshot_latency:?}",
        resources.reconciliations.high_watermark, resources.scheduler_workers.high_watermark,
    );
    assert!(
        dense_elapsed <= ABSOLUTE_LIMIT,
        "dense withdrawal exceeded {ABSOLUTE_LIMIT:?}"
    );
    assert!(
        resources.reconciliations.high_watermark <= resources.reconciliations.limit,
        "scheduler exceeded its configured reconciliation limit"
    );
    assert_eq!(
        resources.scheduler_workers.high_watermark, 1,
        "dense reconciliation created more than one Runtime scheduler worker"
    );
    assert!(
        maximum_snapshot_latency <= SNAPSHOT_LATENCY_LIMIT,
        "public snapshot latency exceeded {SNAPSHOT_LATENCY_LIMIT:?}"
    );
}
