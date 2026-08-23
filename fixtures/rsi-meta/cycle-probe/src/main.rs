use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use rsi_meta::{
    ConfigValue, Context, ContractVersion, FactoryIdentity, IsolationId, PluginDescriptor,
    PluginFactory, Requirement, Result, Runtime,
};
use serde_json::Value;

const POPULATIONS: [usize; 3] = [1_024, 2_048, 4_096];
const RATIO_HEADROOM: u32 = 3;
const FIXED_HEADROOM: Duration = Duration::from_millis(50);
const ABSOLUTE_LIMIT: Duration = Duration::from_secs(5);

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

    async fn activate(&self, _: Context, _: ConfigValue) -> Result<()> {
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
    let runtime = Runtime::default();
    let mut context = runtime.root();
    let started = Instant::now();
    for index in 0..population {
        context = context.isolate(
            format!("scope-{index}"),
            IsolationId(u64::try_from(index).expect("probe population fits u64") + 1),
        );
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

#[tokio::main(flavor = "current_thread")]
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
}
