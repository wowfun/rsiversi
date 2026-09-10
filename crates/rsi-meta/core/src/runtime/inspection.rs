use super::{Context, Fiber, Owner, Runtime, RuntimeState};
use crate::{
    ContractId, ContractVersion, FactoryIdentity, FiberGeneration, FiberId, FiberState,
    IsolationId, LocalContractKey, LocalIsolationId, MetaError, Result, RuntimeResourceSnapshot,
    ServiceKey, UpdateMode,
};
use std::ops::Bound::{Excluded, Unbounded};
use std::sync::{Arc, atomic::Ordering};

/// Maximum Fiber rows in one inspection page.
pub const MAXIMUM_INSPECTION_FIBERS: usize = 64;
/// Maximum retained items in each per-Fiber inspection collection.
pub const MAXIMUM_INSPECTION_ITEMS: usize = 128;

/// Bounded inspection selection; the cursor is exclusive and Runtime-local.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct InspectionRequest {
    /// Last returned Fiber identity, or the start of membership.
    pub after: Option<FiberId>,
    /// Requested rows, in `1..=MAXIMUM_INSPECTION_FIBERS`.
    pub maximum_fibers: usize,
    /// Prefix per dependency, supply and effect collection, in `1..=MAXIMUM_INSPECTION_ITEMS`.
    pub maximum_items: usize,
}
impl Default for InspectionRequest {
    fn default() -> Self {
        Self {
            after: None,
            maximum_fibers: 32,
            maximum_items: 32,
        }
    }
}

/// One total and a bounded prefix; omitted items are not silently counted as absent.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InspectedCollection<T> {
    /// Complete collection length at its observation boundary.
    pub total: usize,
    /// Retained prefix within the request's per-Fiber bound.
    pub items: Vec<T>,
}

/// Lifecycle kind without raw diagnostics or plugin-controlled failure strings.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InspectedFiberState {
    /// Waiting for dependencies.
    Pending,
    /// Staging an activation generation.
    Loading,
    /// Published generation.
    Active,
    /// Activation or cleanup failure; raw diagnostics are omitted.
    Failed,
    /// Withdrawing and retiring.
    Unloading,
    /// Removed from live membership after capture.
    Disposed,
}

/// Exact parent or provider generation; conveys no callable authority.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct InspectedOwner {
    /// Runtime-local Fiber identity.
    pub fiber: FiberId,
    /// Exact generation.
    pub generation: FiberGeneration,
}
impl From<Owner> for InspectedOwner {
    fn from(owner: Owner) -> Self {
        Self {
            fiber: owner.fiber,
            generation: owner.generation,
        }
    }
}

/// Actual selected contract namespace and isolation, without service values.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum InspectedService {
    /// Generation-fenced Portable service and its exact contract.
    Portable {
        /// Logical service key.
        key: ServiceKey,
        /// Actual selected isolation.
        isolation: IsolationId,
        /// Exact required or provided contract.
        contract: ContractId,
        /// Exact contract version.
        version: ContractVersion,
    },
    /// Nominal safe-Rust Local service.
    Local {
        /// Stable catalog key; Rust `TypeId` is not exported.
        key: LocalContractKey,
        /// Actual selected Local isolation.
        isolation: LocalIsolationId,
    },
}

/// One captured provider binding with its namespace-local supply token.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InspectedProvider {
    /// Exact generation that supplied the dependency.
    pub owner: InspectedOwner,
    /// Non-repeating token within the selected Local or Portable namespace.
    pub supply_token: u64,
}

/// One prepared requirement and the installed generation's captured provider, if any.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InspectedDependency {
    /// Prepared contract and actual Context isolation.
    pub service: InspectedService,
    /// Captured binding, absent before dependency resolution installs a generation.
    pub provider: Option<InspectedProvider>,
}

/// One owned staged or published service supply.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InspectedSupply {
    /// Provided contract and actual isolation slot.
    pub service: InspectedService,
    /// Token within this service's namespace.
    pub supply_token: u64,
    /// Whether the owning generation reached publication.
    pub generation_published: bool,
}

/// Effect cleanup progression, independent of whether setup is still open.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InspectedCleanupState {
    /// No cleanup claimant exists.
    Unclaimed,
    /// A claimant exists, but execution has not started.
    Claimed,
    /// Execution started, possibly still waiting for the setup owner to close.
    Running,
    /// A completion report exists; this does not imply successful cleanup.
    Complete,
}

/// One effect transaction's observable bookkeeping, excluding labels and callbacks.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InspectedEffect {
    /// Runtime-local transaction identity.
    pub id: u64,
    /// The setup owner can still register undo entries.
    pub open: bool,
    /// Observed cleanup progression, independent of setup completion.
    pub cleanup: InspectedCleanupState,
    /// Total failures once a report exists; raw diagnostics are omitted.
    pub cleanup_failures: Option<usize>,
    /// Entries still in the transaction table, excluding entries moved to executing cleanup.
    pub queued_entries: usize,
}

/// Owned redacted metadata captured for one Fiber.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InspectedFiber {
    /// Stable Runtime-local identity.
    pub id: FiberId,
    /// Observed activation generation.
    pub generation: FiberGeneration,
    /// Resolver-owned executable provenance.
    pub factory: FactoryIdentity,
    /// Static update policy.
    pub update_mode: UpdateMode,
    /// Lifecycle kind without raw error text.
    pub state: InspectedFiberState,
    /// Parent generation, absent at the Runtime root.
    pub parent: Option<InspectedOwner>,
    /// Actual composition path, absent after position release.
    pub order: Option<Vec<u64>>,
    /// Prepared dependency collection, Portable then Local.
    pub dependencies: InspectedCollection<InspectedDependency>,
    /// Owned supply collection, Portable then Local.
    pub supplies: InspectedCollection<InspectedSupply>,
    /// Effect transactions in identity order.
    pub effects: InspectedCollection<InspectedEffect>,
    /// Entries retained by the generation budget, including executing cleanup.
    pub retained_effect_entries: usize,
    /// Transactions retained by the generation budget, including claimed cleanup.
    pub retained_effect_transactions: usize,
    /// Generation teardown phase, observed separately from Fiber metadata.
    pub cleanup_phase: Option<crate::CleanupPhase>,
    /// Current generation's retained Local event listener count.
    pub listeners: usize,
    /// Children still registered in the generation table; cleanup may already own others.
    pub children: usize,
}

/// Bounded observation across registry, Fiber, order and effect boundaries.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RuntimeInspection {
    /// Registry revision at membership capture.
    pub revision: u64,
    /// Shutdown admission has closed.
    pub shutting_down: bool,
    /// Terminal admission fence exists; the raw reason is omitted.
    pub terminal: bool,
    /// Complete selected membership count before cursor filtering.
    pub total_fibers: usize,
    /// Last returned identity when more selected membership existed at capture.
    pub next_after: Option<FiberId>,
    /// Metadata in Fiber identity order.
    pub fibers: Vec<InspectedFiber>,
    /// Global logical resource usage only for whole-Runtime inspection.
    pub resources: Option<RuntimeResourceSnapshot>,
}

impl Runtime {
    /// Reads bounded redacted metadata without invoking plugins or granting mutable authority.
    pub fn inspect(&self, request: InspectionRequest) -> Result<RuntimeInspection> {
        self.inspect_scope(request, None)
    }

    fn inspect_scope(
        &self,
        request: InspectionRequest,
        scope: Option<Owner>,
    ) -> Result<RuntimeInspection> {
        if !(1..=MAXIMUM_INSPECTION_FIBERS).contains(&request.maximum_fibers)
            || !(1..=MAXIMUM_INSPECTION_ITEMS).contains(&request.maximum_items)
        {
            return Err(MetaError::InvalidInput(
                "inspection page limits are out of bounds".into(),
            ));
        }
        self.check_inspection_owner(scope)?;
        let (revision, terminal, total_fibers, more, fibers) = {
            let state = self.inner.state.lock().expect("runtime state poisoned");
            let belongs =
                |fiber: &Fiber| scope.is_none_or(|owner| belongs_to(fiber, owner, &state));
            let total = state.fibers.values().filter(|fiber| belongs(fiber)).count();
            let lower = request.after.map_or(Unbounded, Excluded);
            let mut selected = state
                .fibers
                .range((lower, Unbounded))
                .map(|(_, fiber)| fiber)
                .filter(|fiber| belongs(fiber));
            let fibers = selected
                .by_ref()
                .take(request.maximum_fibers)
                .cloned()
                .collect::<Vec<_>>();
            (
                state.revision,
                state.terminal.is_some(),
                total,
                selected.next().is_some(),
                fibers,
            )
        };
        let next_after = more.then(|| fibers.last().expect("nonempty bounded page").id);
        let fibers = fibers
            .iter()
            .map(|fiber| inspect_fiber(fiber, request.maximum_items, self))
            .collect();
        self.check_inspection_owner(scope)?;
        Ok(RuntimeInspection {
            revision,
            shutting_down: self.inner.shutting_down.load(Ordering::Acquire),
            terminal,
            total_fibers,
            next_after,
            fibers,
            resources: scope.is_none().then(|| self.resource_snapshot()),
        })
    }

    fn check_inspection_owner(&self, owner: Option<Owner>) -> Result<()> {
        let Some(owner) = owner else {
            return Ok(());
        };
        let fiber = self
            .inner
            .state
            .lock()
            .expect("runtime state poisoned")
            .fibers
            .get(&owner.fiber)
            .cloned();
        if fiber.is_some_and(|fiber| {
            let data = fiber.data.lock().expect("fiber data poisoned");
            data.generation == owner.generation && !data.disposed
        }) {
            return Ok(());
        }
        Err(MetaError::StaleContext {
            fiber: owner.fiber,
            generation: owner.generation,
        })
    }
}

impl Context {
    /// Inspects only this owning generation's subtree, or the whole Runtime for a root Context.
    pub fn inspect(&self, request: InspectionRequest) -> Result<RuntimeInspection> {
        self.runtime.inspect_scope(request, self.owner)
    }
}

fn belongs_to(fiber: &Fiber, scope: Owner, state: &RuntimeState) -> bool {
    if fiber.id == scope.fiber {
        return true;
    }
    let mut owner = fiber.parent;
    while let Some(parent) = owner {
        if parent.fiber == scope.fiber {
            return parent.generation == scope.generation;
        }
        owner = state
            .fibers
            .get(&parent.fiber)
            .and_then(|fiber| fiber.parent);
    }
    false
}

fn inspect_fiber(fiber: &Arc<Fiber>, maximum: usize, runtime: &Runtime) -> InspectedFiber {
    let data = fiber.data.lock().expect("fiber data poisoned");
    let position = data
        .position
        .as_ref()
        .map(|position| position.position.clone());
    let active = data.active.as_ref();
    let cleanup_started =
        active.is_some_and(|active| active.cleanup.started.load(Ordering::Acquire));
    let dependencies = dependencies(fiber, &data, maximum);
    let supplies = supplies(active, maximum);
    let effects = active
        .map(|active| {
            active
                .effects
                .values()
                .take(maximum)
                .cloned()
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let mut result = InspectedFiber {
        id: fiber.id,
        generation: data.generation,
        factory: data.identity.clone(),
        update_mode: data.update_mode,
        state: match data.state {
            FiberState::Pending(_) => InspectedFiberState::Pending,
            FiberState::Loading => InspectedFiberState::Loading,
            FiberState::Active => InspectedFiberState::Active,
            FiberState::Failed(_) => InspectedFiberState::Failed,
            FiberState::Unloading => InspectedFiberState::Unloading,
            FiberState::Disposed => InspectedFiberState::Disposed,
        },
        parent: fiber.parent.map(Into::into),
        order: None,
        dependencies,
        supplies,
        effects: InspectedCollection {
            total: active.map_or(0, |active| active.effects.len()),
            items: Vec::new(),
        },
        retained_effect_entries: active.map_or(0, |active| active.effect_budget.current()),
        retained_effect_transactions: active
            .map_or(0, |active| active.effect_transaction_budget.current()),
        cleanup_phase: None,
        listeners: active.map_or(0, |active| active.local_listener_ids.len()),
        children: active.map_or(0, |active| active.children.len()),
    };
    drop(data);
    result.cleanup_phase =
        cleanup_started.then(|| *fiber.cleanup_phase.lock().expect("cleanup phase poisoned"));
    result.order = position.map(|position| {
        runtime
            .inner
            .composition_order
            .snapshot(|order| order.key(&position))
    });
    result.effects.items = effects.iter().map(|effect| effect.inspection()).collect();
    result
}

fn dependencies(
    fiber: &Fiber,
    data: &super::FiberData,
    maximum: usize,
) -> InspectedCollection<InspectedDependency> {
    let Some(attempt) = &data.attempt else {
        return InspectedCollection {
            total: 0,
            items: Vec::new(),
        };
    };
    let active = data.active.as_ref();
    let portable = attempt
        .requirements
        .iter()
        .map(|requirement| InspectedDependency {
            service: InspectedService::Portable {
                key: requirement.key.clone(),
                contract: requirement.contract.clone(),
                version: requirement.version,
                isolation: fiber
                    .base_context
                    .isolation
                    .get(&requirement.key)
                    .copied()
                    .unwrap_or(IsolationId(0)),
            },
            provider: active
                .and_then(|active| active.bindings.get(&requirement.key))
                .map(|binding| InspectedProvider {
                    owner: InspectedOwner {
                        fiber: binding.provider,
                        generation: binding.generation,
                    },
                    supply_token: binding.supply.token(),
                }),
        });
    let local = attempt
        .local_requirements
        .iter()
        .map(|requirement| InspectedDependency {
            service: InspectedService::Local {
                key: requirement.key.clone(),
                isolation: fiber
                    .base_context
                    .local_isolation
                    .get(&requirement.contract)
                    .copied()
                    .unwrap_or(LocalIsolationId(0)),
            },
            provider: active
                .and_then(|active| active.local_bindings.get(&requirement.contract))
                .map(|binding| InspectedProvider {
                    owner: InspectedOwner {
                        fiber: binding.provider,
                        generation: binding.generation,
                    },
                    supply_token: binding.supply.token(),
                }),
        });
    InspectedCollection {
        total: attempt.requirements.len() + attempt.local_requirements.len(),
        items: portable.chain(local).take(maximum).collect(),
    }
}

fn supplies(
    active: Option<&super::GenerationData>,
    maximum: usize,
) -> InspectedCollection<InspectedSupply> {
    let Some(active) = active else {
        return InspectedCollection {
            total: 0,
            items: Vec::new(),
        };
    };
    let portable = active
        .services
        .iter()
        .map(|(slot, supply)| InspectedSupply {
            service: InspectedService::Portable {
                key: slot.key.clone(),
                isolation: slot.isolation,
                contract: supply.binding.contract.clone(),
                version: supply.binding.version,
            },
            supply_token: supply.binding.supply.token(),
            generation_published: active.published,
        });
    let local = active
        .local_services
        .iter()
        .map(|(slot, supply)| InspectedSupply {
            service: InspectedService::Local {
                key: supply.binding.key.clone(),
                isolation: slot.isolation,
            },
            supply_token: supply.binding.supply.token(),
            generation_published: active.published,
        });
    InspectedCollection {
        total: active.services.len() + active.local_services.len(),
        items: portable.chain(local).take(maximum).collect(),
    }
}
