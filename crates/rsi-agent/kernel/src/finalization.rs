use super::*;
use rsi_agent_turn_protocol::{FinalizationResult, MAXIMUM_TURN_FINALIZERS};
use rsi_meta::{
    RegistrationContext, RegistrationOrderSnapshot, RegistrationPosition, RuntimeIdentity,
};

type Snapshot = Arc<[Arc<Entry>]>;

#[derive(Default)]
pub(super) struct Registry {
    pub(super) runtime: Option<RuntimeIdentity>,
    next: u64,
    entries: BTreeMap<u64, Arc<Entry>>,
    names: BTreeSet<String>,
    snapshot: Option<(RegistrationOrderSnapshot, Snapshot)>,
}

struct Entry {
    name: String,
    finalizer: Arc<dyn TurnFinalizer>,
    position: RegistrationPosition,
}

impl Registry {
    fn snapshot(&mut self) -> FinalizationResult<Snapshot> {
        if let Some((order, snapshot)) = &self.snapshot
            && order.is_current()
            && snapshot.iter().all(|entry| entry.position.is_admitting())
        {
            return Ok(snapshot.clone());
        }
        let entries: Vec<_> = self
            .entries
            .values()
            .filter(|entry| entry.position.is_admitting())
            .cloned()
            .collect();
        let positions: Vec<_> = entries.iter().map(|entry| entry.position.clone()).collect();
        let order = RegistrationOrderSnapshot::capture(&positions).map_err(invalid)?;
        let mut ranked: Vec<_> = entries.into_iter().zip(order.ranks()).collect();
        ranked.sort_by(|left, right| left.1.cmp(right.1));
        let entries: Snapshot = ranked.into_iter().map(|(entry, _)| entry).collect();
        let entries = self
            .snapshot
            .as_ref()
            .filter(|(_, previous)| {
                previous.len() == entries.len()
                    && previous
                        .iter()
                        .zip(entries.iter())
                        .all(|(left, right)| Arc::ptr_eq(left, right))
            })
            .map_or(entries.clone(), |(_, previous)| previous.clone());
        self.snapshot = Some((order, entries.clone()));
        Ok(entries)
    }

    fn insert(&mut self, runtime: RuntimeIdentity, id: u64, entry: Entry) -> rsi_meta::Result<()> {
        if self.runtime.as_ref().is_some_and(|bound| *bound != runtime) {
            return Err(MetaError::InvalidInput(
                "finalizer belongs to another Runtime".into(),
            ));
        }
        if self.names.contains(&entry.name) {
            return Err(MetaError::InvalidInput(format!(
                "turn finalizer `{}` is already registered",
                entry.name,
            )));
        }
        if self.entries.len() >= MAXIMUM_TURN_FINALIZERS {
            return Err(MetaError::CapacityExhausted {
                resource: "turn finalizers",
            });
        }
        self.runtime = Some(runtime);
        self.names.insert(entry.name.clone());
        self.entries.insert(id, Arc::new(entry));
        self.snapshot = None;
        Ok(())
    }
}

fn invalid(error: impl fmt::Display) -> TurnFinalizationError {
    TurnFinalizationError::Invalid(error.to_string())
}

fn remove(inner: &Weak<KernelInner>, id: u64) {
    if let Some(inner) = inner.upgrade() {
        let removed = {
            let mut state = lock_state(&inner);
            let registry = &mut state.finalizers;
            let removed = registry.entries.remove(&id);
            if let Some(entry) = &removed {
                registry.names.remove(&entry.name);
            }
            (removed, registry.snapshot.take())
        };
        // A hook's destructor may run plugin code. Release Kernel state first.
        drop(removed);
    }
}

#[async_trait]
impl TurnFinalization for AgentKernel {
    fn register(
        &self,
        context: &RegistrationContext,
        name: String,
        finalizer: Arc<dyn TurnFinalizer>,
    ) -> FinalizationResult<TurnFinalizerLease> {
        validate_identifier("turn finalizer", &name).map_err(invalid)?;
        let registration = {
            let mut state = lock_state(&self.inner);
            let registry = &mut state.finalizers;
            registry.next = registry
                .next
                .checked_add(1)
                .ok_or_else(|| invalid("turn finalizer identity is exhausted"))?;
            registry.next
        };
        let inner = Arc::downgrade(&self.inner);
        let runtime = context.runtime_identity();
        let ((), lease) = context
            .register(
                "withdraw turn finalizer",
                move || {
                    remove(&inner, registration);
                    Ok(())
                },
                |position| {
                    let mut state = lock_state(&self.inner);
                    if !state.accepting {
                        return Err(MetaError::InvalidInput("Kernel is shutting down".into()));
                    }
                    state.finalizers.insert(
                        runtime,
                        registration,
                        Entry {
                            name,
                            finalizer,
                            position,
                        },
                    )
                },
            )
            .map_err(invalid)?;
        Ok(TurnFinalizerLease::from_registration(lease))
    }

    async fn finalize(
        &self,
        context: &TurnFinalizationContext,
    ) -> FinalizationResult<TurnFinalizationReport> {
        let finalizers = lock_state(&self.inner).finalizers.snapshot()?;
        let results = futures_util::future::join_all(finalizers.iter().map(|entry| async move {
            std::panic::AssertUnwindSafe(entry.finalizer.finalize(context))
                .catch_unwind()
                .await
        }))
        .await;
        for (entry, result) in finalizers.iter().zip(&results) {
            match result {
                Ok(Err(error)) => return Err(error.clone()),
                Err(_) => {
                    return Err(TurnFinalizationError::Failed {
                        code: "turn.finalizer_panic".into(),
                        message: format!("turn finalizer `{}` panicked", entry.name),
                    });
                }
                Ok(Ok(_)) => {}
            }
        }
        for result in results {
            if let Ok(Ok(report)) = result
                && let Some(blocker) = report.completion_blocker()
            {
                return Ok(TurnFinalizationReport::blocked(blocker.clone()));
            }
        }
        Ok(TurnFinalizationReport::complete())
    }
}
