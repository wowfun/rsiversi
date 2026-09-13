use super::{AgentKernel, KernelInner, TurnError, TurnResult};
use std::sync::{
    Arc, Weak,
    atomic::{AtomicUsize, Ordering},
};

/// Observer admission diagnostics for one finite lifecycle category.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct ObserverUsage {
    /// Leases currently retained, including suspended stream consumers.
    pub current: usize,
    /// Largest admitted count observed since Kernel construction.
    pub peak: usize,
    /// Rejections at the shared observer admission limit.
    pub rejected: usize,
}
/// Independently sampled diagnostics; not an atomic ownership graph or admission proof.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct ObserverSnapshot {
    /// Configured shared admission limit.
    pub maximum: usize,
    /// All observer categories sharing the same admission counter.
    pub total: ObserverUsage,
    /// Live turn observations.
    pub turn: ObserverUsage,
    /// Durable Session observations.
    pub session: ObserverUsage,
    /// Descendant tree change observations.
    pub tree: ObserverUsage,
    /// Projection change observations.
    pub projection: ObserverUsage,
    /// Canonical payload bytes still owned by delivered observation clones.
    pub retained_observation_bytes: usize,
}
#[derive(Default)]
struct Usage {
    current: AtomicUsize,
    peak: AtomicUsize,
    rejected: AtomicUsize,
}
impl Usage {
    fn snapshot(&self) -> ObserverUsage {
        ObserverUsage {
            current: self.current.load(Ordering::Acquire),
            peak: self.peak.load(Ordering::Relaxed),
            rejected: self.rejected.load(Ordering::Relaxed),
        }
    }
}
#[derive(Default)]
pub(super) struct ObserverResources {
    total: Usage,
    categories: [Usage; 4],
}
#[derive(Clone, Copy)]
pub(super) enum ObserverKind {
    Turn,
    Session,
    Tree,
    Projection,
}
pub(super) struct ObserverLease {
    inner: Weak<KernelInner>,
    kind: ObserverKind,
}
impl ObserverLease {
    pub(super) fn acquire(inner: &Arc<KernelInner>, kind: ObserverKind) -> TurnResult<Self> {
        let resources = &inner.observers;
        let category = &resources.categories[kind as usize];
        let mut current = resources.total.current.load(Ordering::Acquire);
        loop {
            if current >= inner.limits.maximum_active_observers {
                resources.total.rejected.fetch_add(1, Ordering::Relaxed);
                category.rejected.fetch_add(1, Ordering::Relaxed);
                return Err(TurnError::ObserverCapacity);
            }
            match resources.total.current.compare_exchange_weak(
                current,
                current + 1,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => {
                    resources
                        .total
                        .peak
                        .fetch_max(current + 1, Ordering::Relaxed);
                    let active = category.current.fetch_add(1, Ordering::AcqRel) + 1;
                    category.peak.fetch_max(active, Ordering::Relaxed);
                    return Ok(Self {
                        inner: Arc::downgrade(inner),
                        kind,
                    });
                }
                Err(observed) => current = observed,
            }
        }
    }
}
impl Drop for ObserverLease {
    fn drop(&mut self) {
        if let Some(inner) = self.inner.upgrade() {
            inner.observers.categories[self.kind as usize]
                .current
                .fetch_sub(1, Ordering::AcqRel);
            inner.observers.total.current.fetch_sub(1, Ordering::AcqRel);
        }
    }
}
impl AgentKernel {
    /// Samples observer ownership and payload retention without changing admission.
    pub fn observer_snapshot(&self) -> ObserverSnapshot {
        let resources = &self.inner.observers;
        ObserverSnapshot {
            maximum: self.inner.limits.maximum_active_observers,
            total: resources.total.snapshot(),
            turn: resources.categories[ObserverKind::Turn as usize].snapshot(),
            session: resources.categories[ObserverKind::Session as usize].snapshot(),
            tree: resources.categories[ObserverKind::Tree as usize].snapshot(),
            projection: resources.categories[ObserverKind::Projection as usize].snapshot(),
            retained_observation_bytes: self.inner.observation_retention.retained_bytes(),
        }
    }
}
