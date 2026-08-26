#![allow(clippy::wildcard_imports)] // This is one implementation partition of runtime.

use super::*;
use crate::service::LeaseGuard;
use std::collections::BTreeMap;
use std::sync::atomic::AtomicBool;

#[derive(Debug)]
struct CapabilitySetState {
    open: bool,
    entries: BTreeMap<u64, Weak<CapabilityEntry>>,
}

/// The single wrapper that owns every capability minted by one generation.
#[derive(Debug)]
pub(crate) struct GenerationCapabilitySet {
    state: Mutex<CapabilitySetState>,
}

impl GenerationCapabilitySet {
    pub(super) fn new() -> Arc<Self> {
        Arc::new(Self {
            state: Mutex::new(CapabilitySetState {
                open: true,
                entries: BTreeMap::new(),
            }),
        })
    }

    fn register(&self, entry: &Arc<CapabilityEntry>) -> Result<()> {
        let mut state = self
            .state
            .lock()
            .expect("generation capability set poisoned");
        if !state.open {
            return Err(MetaError::StaleContext {
                fiber: entry.owner.fiber,
                generation: entry.owner.generation,
            });
        }
        state.entries.insert(entry.id, Arc::downgrade(entry));
        Ok(())
    }

    fn unregister(&self, id: u64, expected: *const CapabilityEntry) {
        let mut state = self
            .state
            .lock()
            .expect("generation capability set poisoned");
        if state
            .entries
            .get(&id)
            .is_some_and(|entry| std::ptr::eq(entry.as_ptr(), expected))
        {
            state.entries.remove(&id);
        }
    }

    pub(super) fn close_and_take(&self) -> Vec<Arc<CapabilityEntry>> {
        let entries = {
            let mut state = self
                .state
                .lock()
                .expect("generation capability set poisoned");
            state.open = false;
            std::mem::take(&mut state.entries)
        };
        entries
            .into_values()
            .filter_map(|entry| entry.upgrade())
            .collect()
    }
}

pub(crate) struct CapabilityEntry {
    id: u64,
    owner: Owner,
    pub(crate) binding: Arc<ProviderBinding>,
    pub(crate) overlay: Arc<InterceptLayers>,
    pub(crate) admission: Arc<AdmissionLease>,
    generation: Weak<GenerationCapabilitySet>,
    _reservation: ResourceReservation,
    revoked: AtomicBool,
}

#[derive(Clone)]
struct DetachedHolder {
    runtime: Weak<RuntimeInner>,
    owner: Option<Owner>,
    isolation: Arc<BTreeMap<ServiceKey, IsolationId>>,
    intercepts: Arc<BTreeMap<ServiceKey, Arc<InterceptLayers>>>,
    extensions: Arc<ContextExtensions>,
    entries: usize,
    encoded_bytes: usize,
    trace: Option<CallTrace>,
}

/// Capability possession that does not keep its holder Runtime alive.
#[derive(Clone)]
pub struct DetachedCapability {
    holder: DetachedHolder,
    entry: Arc<CapabilityEntry>,
}

impl DetachedCapability {
    /// Reconstructs the exact original holder while its Runtime still exists.
    pub fn upgrade(&self) -> Result<Capability> {
        self.entry.validate_transfer()?;
        let inner = self
            .holder
            .runtime
            .upgrade()
            .ok_or(MetaError::StaleCapability)?;
        Ok(Capability {
            holder: Context {
                runtime: Runtime { inner },
                owner: self.holder.owner,
                setup_effect: None,
                isolation: Arc::clone(&self.holder.isolation),
                intercepts: Arc::clone(&self.holder.intercepts),
                extensions: Arc::clone(&self.holder.extensions),
                entries: self.holder.entries,
                encoded_bytes: self.holder.encoded_bytes,
                trace: self.holder.trace.clone(),
            },
            entry: Arc::clone(&self.entry),
        })
    }
}

impl fmt::Debug for DetachedCapability {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("DetachedCapability")
            .field("key", &self.entry.binding.key)
            .field("contract", &self.entry.binding.contract)
            .field("version", &self.entry.binding.version)
            .field(
                "provider",
                &(self.entry.binding.provider, self.entry.binding.generation),
            )
            .finish_non_exhaustive()
    }
}

impl Capability {
    /// Consumes this handle without retaining its holder Runtime lifetime.
    pub fn detach(self) -> DetachedCapability {
        let Self { holder, entry } = self;
        DetachedCapability {
            holder: DetachedHolder {
                runtime: Arc::downgrade(&holder.runtime.inner),
                owner: holder.owner,
                isolation: holder.isolation,
                intercepts: holder.intercepts,
                extensions: holder.extensions,
                entries: holder.entries,
                encoded_bytes: holder.encoded_bytes,
                trace: holder.trace,
            },
            entry,
        }
    }
}

impl fmt::Debug for CapabilityEntry {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CapabilityEntry")
            .field("owner", &self.owner)
            .field("provider", &self.binding.provider)
            .field("generation", &self.binding.generation)
            .finish_non_exhaustive()
    }
}

impl CapabilityEntry {
    pub(crate) fn validate_transfer(&self) -> Result<()> {
        if self.revoked.load(Ordering::Acquire) {
            Err(MetaError::StaleCapability)
        } else {
            Ok(())
        }
    }

    fn begin_revoke(&self) {
        if self.revoked.swap(true, Ordering::AcqRel) {
            return;
        }
        self.admission.close();
        if let Some(generation) = self.generation.upgrade() {
            generation.unregister(self.id, self);
        }
    }

    pub(super) fn acquire_use(self: &Arc<Self>) -> Result<CapabilityUse> {
        let lease = self
            .admission
            .acquire(false)
            .ok_or(MetaError::StaleCapability)?;
        if self.revoked.load(Ordering::Acquire) {
            return Err(MetaError::StaleCapability);
        }
        Ok(CapabilityUse {
            _entry: Arc::clone(self),
            _lease: lease,
        })
    }
}

impl Drop for CapabilityEntry {
    fn drop(&mut self) {
        self.begin_revoke();
    }
}

pub(crate) struct CapabilityUse {
    _entry: Arc<CapabilityEntry>,
    _lease: LeaseGuard,
}

impl fmt::Debug for CapabilityUse {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CapabilityUse")
            .finish_non_exhaustive()
    }
}

impl Runtime {
    fn next_capability_entry_id(&self) -> Result<u64> {
        self.inner
            .next_capability_entry
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |value| {
                value.checked_add(1)
            })
            .map(|previous| previous + 1)
            .map_err(|_| {
                self.inner.resources.capability_entries.record_rejection();
                MetaError::CapacityExhausted {
                    resource: "capability entry identities",
                }
            })
    }

    pub(super) fn mint_capability(
        &self,
        context: &Context,
        owner: Owner,
        generation: &Arc<GenerationCapabilitySet>,
        binding: Arc<ProviderBinding>,
        overlay: Arc<InterceptLayers>,
    ) -> Result<Capability> {
        let reservation = self
            .inner
            .resources
            .capability_entries
            .try_reserve(1)
            .ok_or(MetaError::CapacityExhausted {
                resource: "capability entries",
            })?;
        let id = self.next_capability_entry_id()?;
        let entry = Arc::new(CapabilityEntry {
            id,
            owner,
            binding,
            overlay,
            admission: Arc::new(AdmissionLease::default()),
            generation: Arc::downgrade(generation),
            _reservation: reservation,
            revoked: AtomicBool::new(false),
        });
        if let Err(error) = generation.register(&entry) {
            entry.begin_revoke();
            return Err(error);
        }
        Ok(Capability {
            holder: context.clone(),
            entry,
        })
    }

    pub(super) async fn revoke_capability_set(&self, set: Arc<GenerationCapabilitySet>) {
        let entries = set.close_and_take();
        for entry in &entries {
            entry.begin_revoke();
        }
        for entry in &entries {
            entry.admission.seal();
        }
        // Every admitted use is owned by a CallDriver. Closing every entry first
        // prevents new uses; existing drivers follow their validated absolute
        // service deadline or cancellation path and then drop the CapabilityUse.
        for entry in entries {
            entry.admission.wait_drained().await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn occupied_entry() -> (Runtime, Arc<CapabilityEntry>) {
        let runtime = Runtime::default();
        let generation = GenerationCapabilitySet::new();
        let binding = Arc::new(ProviderBinding {
            supply: SupplyId::new(FiberId(1), FiberGeneration(1), 1),
            key: ServiceKey::new("direct-entry-test"),
            contract: ContractId::new("test.direct-entry"),
            version: ContractVersion(1),
            provider: FiberId(1),
            generation: FiberGeneration(1),
            endpoint: Mutex::new(None),
            lease: Arc::new(AdmissionLease::default()),
        });
        let reservation = runtime
            .inner
            .resources
            .capability_entries
            .try_reserve(1)
            .unwrap();
        let entry = Arc::new(CapabilityEntry {
            id: 1,
            owner: Owner {
                fiber: FiberId(2),
                generation: FiberGeneration(2),
            },
            binding,
            overlay: InterceptLayers::shared_empty(),
            admission: Arc::new(AdmissionLease::default()),
            generation: Arc::downgrade(&generation),
            _reservation: reservation,
            revoked: AtomicBool::new(false),
        });
        generation.register(&entry).unwrap();
        (runtime, entry)
    }

    #[test]
    fn detached_capability_does_not_retain_its_runtime() {
        let (runtime, entry) = occupied_entry();
        let weak_runtime = Arc::downgrade(&runtime.inner);
        let capability = Capability {
            holder: runtime.root(),
            entry,
        };
        let detached = capability.detach();
        assert_eq!(runtime.resource_snapshot().capability_entries.current, 1);

        drop(runtime);
        assert!(weak_runtime.upgrade().is_none());
        assert_eq!(detached.upgrade().unwrap_err(), MetaError::StaleCapability);
    }

    #[test]
    fn capability_entry_identity_exhaustion_fails_closed() {
        let runtime = Runtime::default();
        runtime
            .inner
            .next_capability_entry
            .store(u64::MAX, Ordering::Release);

        assert_eq!(
            runtime.next_capability_entry_id(),
            Err(MetaError::CapacityExhausted {
                resource: "capability entry identities",
            }),
        );
        assert_eq!(runtime.resource_snapshot().capability_entries.rejected, 1);
    }
}
