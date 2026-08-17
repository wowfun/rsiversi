use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::{Arc, Mutex};

use tokio::sync::watch;

use crate::model::{
    GraphSnapshot, InstanceId, RetirementPhase, RetiringInstanceSnapshot, RoutingSnapshot,
};

type RetirementKey = (InstanceId, u64);
pub(super) type RetirementRegistry = Arc<Mutex<BTreeMap<RetirementKey, RetirementEntry>>>;

#[derive(Clone)]
pub(super) struct RetirementEntry {
    pub(super) generation: Arc<crate::model::Generation>,
    pub(super) phase: Arc<AtomicU8>,
    pub(super) cancel: watch::Sender<bool>,
    pub(super) done: watch::Receiver<bool>,
}

pub(super) fn graph_with_runtime_state(
    routing: &RoutingSnapshot,
    retirements: &RetirementRegistry,
) -> GraphSnapshot {
    let mut graph = routing.graph().clone();
    for generation in routing
        .generations()
        .filter(|generation| !generation.has_healthy_runtime())
    {
        if let Some(instance) = graph.instances.get_mut(&generation.instance) {
            let reason = generation
                .runtime_opt()
                .and_then(crate::runtime::RuntimeHandle::fault_reason)
                .unwrap_or_else(|| "runtime_stopped".to_owned());
            instance.status = crate::model::InstanceStatus::Faulted { reason };
        }
    }
    let mut by_instance = BTreeMap::<InstanceId, RetiringInstanceSnapshot>::new();
    for entry in retirements
        .lock()
        .expect("retirement registry mutex poisoned")
        .values()
    {
        let phase = match entry.phase.load(Ordering::Acquire) {
            0 => RetirementPhase::Draining,
            1 => RetirementPhase::Retiring,
            _ => RetirementPhase::Stopping,
        };
        let aggregate = by_instance
            .entry(entry.generation.instance.clone())
            .or_insert_with(|| RetiringInstanceSnapshot {
                instance_id: entry.generation.instance.clone(),
                generation_count: 0,
                lease_count: 0,
                phase,
            });
        aggregate.generation_count = aggregate.generation_count.saturating_add(1);
        aggregate.lease_count = aggregate
            .lease_count
            .saturating_add(entry.generation.lease_count());
        // Report the least advanced phase across the private generations so a
        // caller never mistakes a partially drained aggregate for completed.
        if retirement_phase_rank(phase) < retirement_phase_rank(aggregate.phase) {
            aggregate.phase = phase;
        }
    }
    graph.retiring_instances = by_instance.into_values().collect();
    graph
}

const fn retirement_phase_rank(phase: RetirementPhase) -> u8 {
    match phase {
        RetirementPhase::Draining => 0,
        RetirementPhase::Retiring => 1,
        RetirementPhase::Stopping => 2,
    }
}
