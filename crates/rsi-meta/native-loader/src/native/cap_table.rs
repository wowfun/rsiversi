use crate::LoaderError;
use crate::catalog_resources::{HostCapabilityReservation, HostResourceLedger};
use crate::native::slot_allocator::FreeSlotIndex;
use rsi_meta_native::{CapId, RIGHT_RETAIN};
use std::sync::{Arc, Mutex};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum CapError {
    Invalid,
    Stale,
    Protocol,
    Wrong,
    NotRetainable,
    RefcountExhausted,
}

/// Bounded slot-and-epoch table for host-issued capabilities.
///
/// Values are stored behind `Arc`, so lookups never execute user `Clone` code
/// while the table mutex is held. Retired values and reservations are always
/// extracted before their destructors run.
pub(super) struct CapTable<T> {
    issuer: u64,
    maximum_slots: usize,
    resources: Arc<HostResourceLedger>,
    state: Mutex<TableState<T>>,
}

struct TableState<T> {
    slots: Vec<Slot<T>>,
    free: FreeSlotIndex,
}

impl<T> Default for TableState<T> {
    fn default() -> Self {
        Self {
            slots: Vec::new(),
            free: FreeSlotIndex::default(),
        }
    }
}

struct Slot<T> {
    epoch: u64,
    last_consumed_epoch: Option<u64>,
    reserved: bool,
    kind: u32,
    rights: u32,
    references: u64,
    value: Option<Arc<T>>,
    reservation: Option<HostCapabilityReservation>,
}

impl<T> CapTable<T> {
    pub(super) fn new(
        issuer: u64,
        maximum_slots: usize,
        resources: Arc<HostResourceLedger>,
    ) -> Self {
        debug_assert_ne!(issuer, 0);
        debug_assert_ne!(maximum_slots, 0);
        Self {
            issuer,
            maximum_slots,
            resources,
            state: Mutex::new(TableState::default()),
        }
    }

    pub(super) fn insert(
        self: &Arc<Self>,
        kind: u32,
        rights: u32,
        value: Arc<T>,
    ) -> Result<CapId, (LoaderError, Arc<T>)> {
        let reservation = match self.reserve(kind, rights) {
            Ok(value) => value,
            Err(error) => return Err((error, value)),
        };
        Ok(reservation.fill(value))
    }

    pub(super) fn reserve(
        self: &Arc<Self>,
        kind: u32,
        rights: u32,
    ) -> Result<CapReservation<T>, LoaderError> {
        if kind == 0 || rights == 0 {
            return Err(LoaderError::InvalidInput(
                "host capability metadata is empty".to_owned(),
            ));
        }
        let resource = self.resources.reserve_capability()?;
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(index) = state.free.take() {
            let slot = state
                .slots
                .get_mut(index)
                .expect("free capability index remains addressable");
            debug_assert!(!slot.reserved && slot.value.is_none() && slot.epoch != u64::MAX);
            slot.epoch += 1;
            slot.reserved = true;
            slot.kind = 0;
            slot.rights = 0;
            slot.references = 0;
            slot.reservation = Some(resource);
            return Ok(CapReservation {
                table: Arc::clone(self),
                index,
                epoch: slot.epoch,
                kind,
                rights,
                active: true,
            });
        }
        if state.slots.len() >= self.maximum_slots {
            drop(state);
            return Err(LoaderError::CapacityExhausted {
                resource: "host capability slots",
                limit: u64::try_from(self.maximum_slots).unwrap_or(u64::MAX),
            });
        }
        let Some(slot_number) = u64::try_from(state.slots.len())
            .ok()
            .and_then(|index| index.checked_add(1))
        else {
            drop(state);
            return Err(LoaderError::CapacityExhausted {
                resource: "host capability slots",
                limit: u64::MAX,
            });
        };
        state.slots.push(Slot {
            epoch: 1,
            last_consumed_epoch: None,
            reserved: true,
            kind: 0,
            rights: 0,
            references: 0,
            value: None,
            reservation: Some(resource),
        });
        Ok(CapReservation {
            table: Arc::clone(self),
            index: usize::try_from(slot_number - 1).expect("new cap slot index fits usize"),
            epoch: 1,
            kind,
            rights,
            active: true,
        })
    }

    pub(super) fn get_exact(&self, id: CapId, kind: u32, rights: u32) -> Result<Arc<T>, CapError> {
        let state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let slot = self.slot(&state, id)?;
        exact_metadata(id, slot, kind, rights)?;
        Ok(Arc::clone(
            slot.value.as_ref().expect("validated live slot"),
        ))
    }

    /// Takes one atomic snapshot of a complete incoming capability array.
    /// `Arc` cloning cannot invoke user code under the table lock; one invalid
    /// member rejects the entire snapshot.
    pub(super) fn get_many_exact(
        &self,
        ids: &[CapId],
        kind: u32,
        rights: u32,
    ) -> Result<Vec<Arc<T>>, CapError> {
        let state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut values = Vec::with_capacity(ids.len());
        for id in ids {
            let slot = self.slot(&state, *id)?;
            exact_metadata(*id, slot, kind, rights)?;
            values.push(Arc::clone(
                slot.value.as_ref().expect("validated live slot"),
            ));
        }
        Ok(values)
    }

    pub(super) fn retain(&self, id: CapId) -> Result<(), CapError> {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let slot = self.slot_mut(&mut state, id)?;
        exact_metadata(id, slot, slot.kind, slot.rights)?;
        if slot.rights & RIGHT_RETAIN == 0 {
            return Err(CapError::NotRetainable);
        }
        slot.references = slot
            .references
            .checked_add(1)
            .ok_or(CapError::RefcountExhausted)?;
        Ok(())
    }

    /// Releases one transferable ABI lease and returns the retired value.
    /// Callers drop the returned value only after the table lock is gone.
    pub(super) fn release(&self, id: CapId) -> Result<Option<Arc<T>>, CapError> {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let index = self.slot_index(&state, id)?;
        let slot = state
            .slots
            .get_mut(index)
            .expect("validated capability index remains addressable");
        if slot.reserved {
            return Err(CapError::Stale);
        }
        if slot.value.is_none() {
            if slot.last_consumed_epoch != Some(id.epoch) {
                return Err(CapError::Stale);
            }
            exact_metadata(id, slot, slot.kind, slot.rights)?;
            return Err(CapError::Protocol);
        }
        exact_metadata(id, slot, slot.kind, slot.rights)?;
        if slot.rights & RIGHT_RETAIN == 0 {
            return Err(CapError::NotRetainable);
        }
        slot.references = slot.references.checked_sub(1).ok_or(CapError::Invalid)?;
        if slot.references != 0 {
            return Ok(None);
        }
        let value = slot.value.take().expect("validated live slot");
        slot.last_consumed_epoch = Some(id.epoch);
        let reservation = slot.reservation.take();
        if slot.epoch != u64::MAX {
            state.free.release(index);
        }
        drop(state);
        drop(reservation);
        Ok(Some(value))
    }

    /// Seals one callback-local, non-retainable entry regardless of foreign use.
    pub(super) fn seal(&self, id: CapId) -> Result<Arc<T>, CapError> {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let index = self.slot_index(&state, id)?;
        let slot = state
            .slots
            .get_mut(index)
            .expect("validated capability index remains addressable");
        if slot.reserved || slot.value.is_none() {
            return Err(CapError::Stale);
        }
        exact_metadata(id, slot, slot.kind, slot.rights)?;
        if slot.rights & RIGHT_RETAIN != 0 || slot.references != 1 {
            return Err(CapError::Wrong);
        }
        let value = slot.value.take().expect("validated live slot");
        let reservation = slot.reservation.take();
        if slot.epoch != u64::MAX {
            state.free.release(index);
        }
        drop(state);
        drop(reservation);
        Ok(value)
    }

    fn id(&self, index: usize, slot: &Slot<T>) -> CapId {
        CapId {
            issuer: self.issuer,
            slot: u64::try_from(index).expect("host cap index fits u64") + 1,
            epoch: slot.epoch,
            kind: slot.kind,
            rights: slot.rights,
        }
    }

    fn slot<'state>(
        &self,
        state: &'state TableState<T>,
        id: CapId,
    ) -> Result<&'state Slot<T>, CapError> {
        validate_id(self.issuer, id)?;
        let index = usize::try_from(id.slot - 1).map_err(|_| CapError::Stale)?;
        let slot = state.slots.get(index).ok_or(CapError::Stale)?;
        if slot.reserved || slot.value.is_none() || slot.epoch != id.epoch {
            return Err(CapError::Stale);
        }
        Ok(slot)
    }

    fn slot_index(&self, state: &TableState<T>, id: CapId) -> Result<usize, CapError> {
        validate_id(self.issuer, id)?;
        let index = usize::try_from(id.slot - 1).map_err(|_| CapError::Stale)?;
        let slot = state.slots.get(index).ok_or(CapError::Stale)?;
        if slot.epoch != id.epoch {
            return Err(CapError::Stale);
        }
        Ok(index)
    }

    fn slot_mut<'state>(
        &self,
        state: &'state mut TableState<T>,
        id: CapId,
    ) -> Result<&'state mut Slot<T>, CapError> {
        validate_id(self.issuer, id)?;
        let index = usize::try_from(id.slot - 1).map_err(|_| CapError::Stale)?;
        let slot = state.slots.get_mut(index).ok_or(CapError::Stale)?;
        if slot.reserved || slot.value.is_none() || slot.epoch != id.epoch {
            return Err(CapError::Stale);
        }
        Ok(slot)
    }
}

pub(super) struct CapReservation<T> {
    table: Arc<CapTable<T>>,
    index: usize,
    epoch: u64,
    kind: u32,
    rights: u32,
    active: bool,
}

impl<T> CapReservation<T> {
    /// Publishes into the exclusively reserved slot. All fallible validation
    /// happens in `reserve`, so callers may safely use this as the final step
    /// of a larger publication transaction.
    pub(super) fn fill(mut self, value: Arc<T>) -> CapId {
        let mut state = self
            .table
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let slot = state
            .slots
            .get_mut(self.index)
            .expect("reserved cap slot remains addressable");
        assert!(
            slot.reserved && slot.epoch == self.epoch && slot.value.is_none(),
            "reserved capability slot changed before publication"
        );
        slot.reserved = false;
        slot.kind = self.kind;
        slot.rights = self.rights;
        slot.references = 1;
        slot.value = Some(value);
        let id = self.table.id(self.index, slot);
        self.active = false;
        id
    }
}

impl<T> Drop for CapReservation<T> {
    fn drop(&mut self) {
        if !self.active {
            return;
        }
        let reservation = {
            let mut state = self
                .table
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let slot = state
                .slots
                .get_mut(self.index)
                .expect("reserved cap slot remains addressable");
            if !slot.reserved || slot.epoch != self.epoch || slot.value.is_some() {
                return;
            }
            slot.reserved = false;
            let reservation = slot.reservation.take();
            if slot.epoch != u64::MAX {
                state.free.release(self.index);
            }
            reservation
        };
        drop(reservation);
    }
}

impl<T> Drop for CapTable<T> {
    fn drop(&mut self) {
        let state = self
            .state
            .get_mut()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut values = Vec::new();
        let mut reservations = Vec::new();
        for slot in &mut state.slots {
            if let Some(value) = slot.value.take() {
                values.push(value);
            }
            if let Some(reservation) = slot.reservation.take() {
                reservations.push(reservation);
            }
            slot.references = 0;
        }
        drop(values);
        drop(reservations);
    }
}

fn validate_id(issuer: u64, id: CapId) -> Result<(), CapError> {
    if !id.is_structurally_valid() {
        Err(CapError::Invalid)
    } else if id.issuer != issuer {
        Err(CapError::Stale)
    } else {
        Ok(())
    }
}

fn exact_metadata<T>(id: CapId, slot: &Slot<T>, kind: u32, rights: u32) -> Result<(), CapError> {
    if id.kind != slot.kind
        || id.rights != slot.rights
        || slot.kind != kind
        || slot.rights != rights
    {
        Err(CapError::Wrong)
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::catalog_resources::NativeCatalogLimits;
    use rsi_meta_native::{CAP_KIND_SERVICE, RIGHT_OPEN};
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[derive(Clone, Debug)]
    struct DropProbe(Arc<AtomicUsize>);

    impl Drop for DropProbe {
        fn drop(&mut self) {
            self.0.fetch_add(1, Ordering::Relaxed);
        }
    }

    fn table(limit: usize) -> Arc<CapTable<DropProbe>> {
        let limits = NativeCatalogLimits {
            maximum_host_capabilities: limit,
            ..NativeCatalogLimits::default()
        };
        Arc::new(CapTable::new(17, limit, HostResourceLedger::new(&limits)))
    }

    #[test]
    fn stale_kind_rights_and_nonretainable_operations_fail_exactly() {
        let table = table(2);
        let drops = Arc::new(AtomicUsize::new(0));
        let id = table
            .insert(
                CAP_KIND_SERVICE,
                RIGHT_OPEN,
                Arc::new(DropProbe(Arc::clone(&drops))),
            )
            .unwrap();
        assert_eq!(table.retain(id), Err(CapError::NotRetainable));
        let mut wrong = id;
        wrong.rights |= RIGHT_RETAIN;
        assert_eq!(
            table.get_exact(wrong, CAP_KIND_SERVICE, RIGHT_OPEN).err(),
            Some(CapError::Wrong)
        );
        let retired = table.seal(id).unwrap();
        assert_eq!(drops.load(Ordering::Relaxed), 0);
        drop(retired);
        assert_eq!(drops.load(Ordering::Relaxed), 1);
        assert_eq!(
            table.get_exact(id, CAP_KIND_SERVICE, RIGHT_OPEN).err(),
            Some(CapError::Stale)
        );
    }

    #[test]
    fn release_extracts_the_last_value_before_user_drop_and_slot_reuse_changes_epoch() {
        let table = table(1);
        let drops = Arc::new(AtomicUsize::new(0));
        let rights = RIGHT_RETAIN | RIGHT_OPEN;
        let first = table
            .insert(
                CAP_KIND_SERVICE,
                rights,
                Arc::new(DropProbe(Arc::clone(&drops))),
            )
            .unwrap();
        table.retain(first).unwrap();
        assert!(table.release(first).unwrap().is_none());
        let retired = table.release(first).unwrap().unwrap();
        assert_eq!(drops.load(Ordering::Relaxed), 0);
        drop(retired);
        let second = table
            .insert(
                CAP_KIND_SERVICE,
                rights,
                Arc::new(DropProbe(Arc::clone(&drops))),
            )
            .unwrap();
        assert_eq!(second.slot, first.slot);
        assert_eq!(second.epoch, first.epoch + 1);
    }

    #[test]
    fn duplicate_release_is_protocol_misuse_but_reused_epoch_is_stale() {
        let table = table(1);
        let rights = RIGHT_RETAIN | RIGHT_OPEN;
        let first = table
            .insert(
                CAP_KIND_SERVICE,
                rights,
                Arc::new(DropProbe(Arc::new(AtomicUsize::new(0)))),
            )
            .unwrap();
        drop(table.release(first).unwrap());
        let duplicate = table.release(first);
        let second = table
            .insert(
                CAP_KIND_SERVICE,
                rights,
                Arc::new(DropProbe(Arc::new(AtomicUsize::new(0)))),
            )
            .unwrap();
        let old_epoch = table.release(first);
        drop(table.release(second).unwrap());

        assert!(matches!(duplicate, Err(CapError::Protocol)));
        assert!(matches!(old_epoch, Err(CapError::Stale)));
    }

    #[test]
    fn growth_and_reuse_have_constant_reserve_probe_budgets() {
        const COUNT: usize = 256;
        let table = table(COUNT);
        let rights = RIGHT_RETAIN | RIGHT_OPEN;
        let ids: Vec<_> = (0..COUNT)
            .map(|_| {
                table
                    .insert(
                        CAP_KIND_SERVICE,
                        rights,
                        Arc::new(DropProbe(Arc::new(AtomicUsize::new(0)))),
                    )
                    .unwrap()
            })
            .collect();
        let probes = table
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .free
            .take_probes();

        assert_eq!(probes, 0, "monotonic growth must append without probing");

        for id in ids {
            drop(table.release(id).unwrap());
        }
        let _reused: Vec<_> = (0..COUNT)
            .map(|_| {
                table
                    .insert(
                        CAP_KIND_SERVICE,
                        rights,
                        Arc::new(DropProbe(Arc::new(AtomicUsize::new(0)))),
                    )
                    .unwrap()
            })
            .collect();
        let probes = table
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .free
            .take_probes();
        assert_eq!(probes, COUNT, "reuse must pop one free index per slot");
    }

    #[test]
    fn capacity_failure_returns_the_incoming_value_without_locked_drop() {
        let table = table(1);
        let drops = Arc::new(AtomicUsize::new(0));
        let rights = RIGHT_RETAIN | RIGHT_OPEN;
        table
            .insert(
                CAP_KIND_SERVICE,
                rights,
                Arc::new(DropProbe(Arc::clone(&drops))),
            )
            .unwrap();
        let (_, rejected) = table
            .insert(
                CAP_KIND_SERVICE,
                rights,
                Arc::new(DropProbe(Arc::clone(&drops))),
            )
            .unwrap_err();
        assert_eq!(drops.load(Ordering::Relaxed), 0);
        drop(rejected);
        assert_eq!(drops.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn unfilled_reservation_is_exclusive_stale_and_rollback_reuses_with_new_epoch() {
        let table = table(1);
        let resources = Arc::clone(&table.resources);
        let rights = RIGHT_RETAIN | RIGHT_OPEN;
        let reservation = table.reserve(CAP_KIND_SERVICE, rights).unwrap();
        let predicted = CapId {
            issuer: table.issuer,
            slot: u64::try_from(reservation.index).unwrap() + 1,
            epoch: reservation.epoch,
            kind: CAP_KIND_SERVICE,
            rights,
        };
        assert_eq!(
            table.get_exact(predicted, CAP_KIND_SERVICE, rights).err(),
            Some(CapError::Stale)
        );
        assert_eq!(resources.snapshot().capabilities, 1);
        assert!(table.reserve(CAP_KIND_SERVICE, rights).is_err());
        assert_eq!(resources.snapshot().capabilities, 1);

        drop(reservation);
        assert_eq!(resources.snapshot().capabilities, 0);
        assert!(matches!(table.release(predicted), Err(CapError::Stale)));
        let next = table.reserve(CAP_KIND_SERVICE, rights).unwrap();
        let id = next.fill(Arc::new(DropProbe(Arc::new(AtomicUsize::new(0)))));
        assert_eq!(id.slot, predicted.slot);
        assert_eq!(id.epoch, predicted.epoch + 1);
        assert!(table.release(id).unwrap().is_some());
        assert_eq!(resources.snapshot().capabilities, 0);
    }

    #[test]
    fn multi_capability_snapshot_is_all_or_nothing() {
        let table = table(2);
        let drops = Arc::new(AtomicUsize::new(0));
        let rights = RIGHT_RETAIN | RIGHT_OPEN;
        let first = table
            .insert(
                CAP_KIND_SERVICE,
                rights,
                Arc::new(DropProbe(Arc::clone(&drops))),
            )
            .unwrap();
        let second = table
            .insert(
                CAP_KIND_SERVICE,
                rights,
                Arc::new(DropProbe(Arc::clone(&drops))),
            )
            .unwrap();
        let mut stale = second;
        stale.epoch += 1;
        assert_eq!(
            table
                .get_many_exact(&[first, stale], CAP_KIND_SERVICE, rights)
                .err(),
            Some(CapError::Stale)
        );
        assert!(table.release(first).unwrap().is_some());
        assert!(table.release(second).unwrap().is_some());
    }

    #[test]
    fn table_drop_extracts_values_and_reservations() {
        let drops = Arc::new(AtomicUsize::new(0));
        let resources;
        {
            let limits = NativeCatalogLimits {
                maximum_host_capabilities: 2,
                ..NativeCatalogLimits::default()
            };
            resources = HostResourceLedger::new(&limits);
            let table = Arc::new(CapTable::new(17, 2, Arc::clone(&resources)));
            table
                .insert(
                    CAP_KIND_SERVICE,
                    RIGHT_RETAIN | RIGHT_OPEN,
                    Arc::new(DropProbe(Arc::clone(&drops))),
                )
                .unwrap();
            assert_eq!(resources.snapshot().capabilities, 1);
        }
        assert_eq!(resources.snapshot().capabilities, 0);
        assert_eq!(drops.load(Ordering::Relaxed), 1);
    }
}
