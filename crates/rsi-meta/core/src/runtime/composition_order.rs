use super::{Context, FiberId, MetaError, Owner, Result, Runtime};
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::sync::{Arc, Mutex};

/// Stable identity of one direct child's composition position.
///
/// A position belongs to its exact parent generation. It retains bounded order
/// metadata, but neither a Runtime nor execution admission. Its rank may change
/// without changing this identity or an occupying Fiber's generation.
#[derive(Clone)]
pub struct ChildPosition(Arc<Position>);

struct Position {
    order: Arc<CompositionOrder>,
    id: u64,
    parent: Option<ChildPosition>,
    owner: Option<Owner>,
}

impl PartialEq for ChildPosition {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}
impl Eq for ChildPosition {}
impl fmt::Debug for ChildPosition {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ChildPosition")
            .field("id", &self.0.id)
            .finish_non_exhaustive()
    }
}

#[derive(Default)]
struct OrderState {
    next: u64,
    revision: Arc<()>,
    positions: BTreeMap<u64, Entry>,
    scopes: BTreeMap<Option<Owner>, BTreeSet<u64>>,
}
struct Entry {
    parent: Option<u64>,
    rank: u64,
    occupant: Option<FiberId>,
}

pub(super) struct CompositionOrder {
    maximum: usize,
    maximum_order_entries: usize,
    state: Mutex<OrderState>,
}

impl CompositionOrder {
    pub(super) fn new(maximum: usize, maximum_order_entries: usize) -> Arc<Self> {
        Arc::new(Self {
            maximum,
            maximum_order_entries,
            state: Mutex::new(OrderState::default()),
        })
    }

    fn allocate(
        self: &Arc<Self>,
        owner: Option<Owner>,
        parent: Option<ChildPosition>,
    ) -> Result<ChildPosition> {
        let mut state = self.state.lock().expect("composition order poisoned");
        if state.positions.len() >= self.maximum {
            return Err(MetaError::CapacityExhausted {
                resource: "composition positions",
            });
        }
        let id = state
            .next
            .checked_add(1)
            .ok_or(MetaError::CapacityExhausted {
                resource: "composition position identities",
            })?;
        state.next = id;
        state.positions.insert(
            id,
            Entry {
                parent: parent.as_ref().map(|p| p.0.id),
                rank: id,
                occupant: None,
            },
        );
        state.scopes.entry(owner).or_default().insert(id);
        state.revision = Arc::new(());
        Ok(ChildPosition(Arc::new(Position {
            order: self.clone(),
            id,
            parent,
            owner,
        })))
    }

    fn reorder(&self, owner: Option<Owner>, positions: &[ChildPosition]) -> Result<()> {
        let mut state = self.state.lock().expect("composition order poisoned");
        let mut selected = BTreeSet::new();
        for position in positions {
            if position.0.owner != owner || !std::ptr::eq(self, Arc::as_ptr(&position.0.order)) {
                return Err(MetaError::InvalidInput(
                    "composition position belongs to another parent or Runtime".into(),
                ));
            }
            if !selected.insert(position.0.id) {
                return Err(MetaError::InvalidInput(
                    "duplicate composition position in order".into(),
                ));
            }
        }
        let Some(scope) = state.scopes.get(&owner) else {
            return Ok(());
        };
        let mut tail: Vec<_> = scope
            .iter()
            .filter(|id| !selected.contains(id))
            .map(|id| (state.positions[id].rank, *id))
            .collect();
        tail.sort_unstable();
        let mut changed = false;
        for (rank, id) in positions
            .iter()
            .map(|position| position.0.id)
            .chain(tail.into_iter().map(|(_, id)| id))
            .enumerate()
        {
            let entry = state
                .positions
                .get_mut(&id)
                .expect("live position retains its order entry");
            let rank = u64::try_from(rank).expect("position count fits its identity space");
            changed |= entry.rank != rank;
            entry.rank = rank;
        }
        if changed {
            state.revision = Arc::new(());
        }
        Ok(())
    }

    pub(super) fn snapshot<T>(&self, operation: impl FnOnce(&OrderView<'_>) -> T) -> T {
        let state = self.state.lock().expect("composition order poisoned");
        operation(&OrderView(&state))
    }
}

pub(super) struct OrderView<'a>(&'a OrderState);
impl OrderView<'_> {
    pub(super) fn revision(&self) -> &Arc<()> {
        &self.0.revision
    }
    pub(super) fn key(&self, position: &ChildPosition) -> Vec<u64> {
        let mut result = Vec::new();
        let mut id = Some(position.0.id);
        while let Some(current) = id {
            let entry = &self.0.positions[&current];
            result.push(entry.rank);
            id = entry.parent;
        }
        result.reverse();
        result
    }
}

impl Drop for Position {
    fn drop(&mut self) {
        let mut state = self.order.state.lock().expect("composition order poisoned");
        state.positions.remove(&self.id);
        if let Some(scope) = state.scopes.get_mut(&self.owner) {
            scope.remove(&self.id);
            if scope.is_empty() {
                state.scopes.remove(&self.owner);
            }
        }
        state.revision = Arc::new(());
        drop(state);
        // Last-owner ancestry release must not recurse with configurable Fiber
        // depth. Shared ancestors remain retained by their other exact owners.
        let mut parent = self.parent.take();
        while let Some(position) = parent {
            match Arc::try_unwrap(position.0) {
                Ok(mut ancestor) => {
                    parent = ancestor.parent.take();
                    drop(ancestor);
                }
                Err(shared) => {
                    drop(shared);
                    break;
                }
            }
        }
    }
}

pub(super) struct PositionOccupancy {
    pub(super) position: ChildPosition,
    fiber: FiberId,
}
impl ChildPosition {
    pub(super) fn claim(
        &self,
        runtime: &Runtime,
        owner: Option<Owner>,
        fiber: FiberId,
    ) -> Result<PositionOccupancy> {
        self.validate(runtime, owner)?;
        let mut state = self
            .0
            .order
            .state
            .lock()
            .expect("composition order poisoned");
        let entry = state
            .positions
            .get_mut(&self.0.id)
            .expect("live position retains its order entry");
        if entry.occupant.is_some() {
            return Err(MetaError::InvalidInput(
                "composition position already has a live Fiber".into(),
            ));
        }
        entry.occupant = Some(fiber);
        Ok(PositionOccupancy {
            position: self.clone(),
            fiber,
        })
    }
    fn validate(&self, runtime: &Runtime, owner: Option<Owner>) -> Result<()> {
        if !Arc::ptr_eq(&self.0.order, &runtime.inner.composition_order) || self.0.owner != owner {
            return Err(MetaError::InvalidInput(
                "composition position belongs to another parent or Runtime".into(),
            ));
        }
        Ok(())
    }
}
impl Drop for PositionOccupancy {
    fn drop(&mut self) {
        let mut state = self
            .position
            .0
            .order
            .state
            .lock()
            .expect("composition order poisoned");
        let entry = state
            .positions
            .get_mut(&self.position.0.id)
            .expect("occupied position retains its order entry");
        if entry.occupant == Some(self.fiber) {
            entry.occupant = None;
        }
    }
}

impl Context {
    /// Reserves a stable direct-child position in this exact parent generation.
    pub fn child_position(&self) -> Result<ChildPosition> {
        let _admission = self.runtime.begin_admission(false)?;
        self.with_live_position(|parent| {
            self.runtime
                .inner
                .composition_order
                .allocate(self.owner, parent)
        })
    }

    /// Selects a reserved direct-child position for subsequent apply operations.
    /// At most one admitted live Fiber may occupy the selected position.
    pub fn with_child_position(&self, position: &ChildPosition) -> Result<Self> {
        let _admission = self.runtime.begin_admission(false)?;
        self.with_live_position(|_| position.validate(&self.runtime, self.owner))?;
        let mut context = self.clone();
        context.child_position = Some(position.clone());
        Ok(context)
    }

    /// Atomically updates only this parent's composition order.
    /// Listed positions come first; omitted positions keep their relative order.
    /// No Fiber, effect, or registration generation changes.
    pub fn reorder_children(&self, positions: &[ChildPosition]) -> Result<()> {
        let _admission = self.runtime.begin_admission(false)?;
        if positions.len()
            > self
                .runtime
                .inner
                .limits
                .topology
                .maximum_composition_positions
        {
            return Err(MetaError::CapacityExhausted {
                resource: "composition order inputs",
            });
        }
        self.with_live_position(|_| {
            self.runtime
                .inner
                .composition_order
                .reorder(self.owner, positions)
        })
    }

    pub(super) fn with_live_position<T>(
        &self,
        operation: impl FnOnce(Option<ChildPosition>) -> Result<T>,
    ) -> Result<T> {
        let state = self
            .runtime
            .inner
            .state
            .lock()
            .expect("runtime state poisoned");
        let position = if let Some(owner) = self.owner {
            let fiber = state
                .fibers
                .get(&owner.fiber)
                .ok_or(MetaError::StaleContext {
                    fiber: owner.fiber,
                    generation: owner.generation,
                })?;
            let data = fiber.data.lock().expect("fiber state poisoned");
            Runtime::validate_live_owner_data(owner, &data)?;
            Some(
                data.position
                    .as_ref()
                    .expect("live Fiber occupies a position")
                    .position
                    .clone(),
            )
        } else {
            None
        };
        operation(position)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn retained_ancestry_drops_iteratively_without_historical_order_entries() {
        let order = CompositionOrder::new(30_000, 30_000);
        let mut tail = None;
        for _ in 0..30_000 {
            tail = Some(order.allocate(None, tail).unwrap());
        }
        assert_eq!(order.state.lock().unwrap().positions.len(), 30_000);
        drop(tail);
        let state = order.state.lock().unwrap();
        assert!(state.positions.is_empty());
        assert!(state.scopes.is_empty());
    }
}

/// Frozen comparison key for one contribution; rank is never an identity or map key.
#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd)]
pub struct RegistrationRank {
    path: Vec<u64>,
    sequence: u64,
}

impl RegistrationRank {
    /// Compares composition positions without the owner-local registration sequence.
    /// Products can append their own stable contribution identity tie-break.
    pub fn compare_position(&self, other: &Self) -> std::cmp::Ordering {
        self.path.cmp(&other.path)
    }
}

/// One immutable capture of contribution order for a registry dispatch.
/// Registrars invalidate their membership cache separately when inserting/removing.
#[derive(Clone)]
pub struct RegistrationOrderSnapshot {
    order: Option<Arc<CompositionOrder>>,
    revision: Arc<()>,
    ranks: Vec<RegistrationRank>,
}
impl fmt::Debug for RegistrationOrderSnapshot {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RegistrationOrderSnapshot")
            .field("ranks", &self.ranks)
            .finish_non_exhaustive()
    }
}
impl RegistrationOrderSnapshot {
    /// Captures all supplied positions at one rank publication boundary.
    /// Foreign Runtime positions are rejected. No plugin callback is invoked.
    pub fn capture(positions: &[super::RegistrationPosition]) -> Result<Self> {
        let Some(first) = positions.first() else {
            return Ok(Self {
                order: None,
                revision: Arc::new(()),
                ranks: Vec::new(),
            });
        };
        let order = &first.position.0.order;
        if positions.len() > order.maximum_order_entries {
            return Err(MetaError::CapacityExhausted {
                resource: "registration order snapshot entries",
            });
        }
        if positions
            .iter()
            .any(|position| !Arc::ptr_eq(order, &position.position.0.order))
        {
            return Err(MetaError::InvalidInput(
                "contribution order cannot combine different Runtimes".into(),
            ));
        }
        Ok(order.snapshot(|view| Self {
            order: Some(order.clone()),
            revision: view.revision().clone(),
            ranks: positions
                .iter()
                .map(|position| RegistrationRank {
                    path: view.key(&position.position),
                    sequence: position.sequence,
                })
                .collect(),
        }))
    }

    /// Immutable comparison keys in exactly the input order passed to capture.
    pub fn ranks(&self) -> &[RegistrationRank] {
        &self.ranks
    }

    /// Whether ranks have remained unchanged since capture. Existing dispatches
    /// continue using their captured ranks even if this returns false.
    pub fn is_current(&self) -> bool {
        self.order
            .as_ref()
            .is_none_or(|order| order.snapshot(|view| Arc::ptr_eq(&self.revision, view.revision())))
    }
}

/// Opaque process-local Runtime identity. It retains no Runtime or admission,
/// cannot be serialized, and is not an authorization credential.
#[derive(Clone)]
pub struct RuntimeIdentity(Arc<CompositionOrder>);
impl PartialEq for RuntimeIdentity {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}
impl Eq for RuntimeIdentity {}
impl fmt::Debug for RuntimeIdentity {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("RuntimeIdentity(..)")
    }
}
impl Runtime {
    /// Returns this Runtime's opaque process-local identity without retaining its lifetime.
    pub fn identity(&self) -> RuntimeIdentity {
        RuntimeIdentity(self.inner.composition_order.clone())
    }
}
impl Context {
    /// Returns the identity of the Runtime retained by this Context.
    pub fn runtime_identity(&self) -> RuntimeIdentity {
        self.runtime.identity()
    }
}
