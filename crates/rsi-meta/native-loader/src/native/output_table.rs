use crate::LoaderError;
use crate::catalog_resources::{HostOutputReservation, HostResourceLedger};
use crate::native::slot_allocator::FreeSlotIndex;
use rsi_meta_native::ReleaseId;
use std::sync::{Arc, Mutex};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum OutputError {
    Invalid,
    Stale,
    Protocol,
}

/// Bounded release-token table. Values and reservations leave the mutex before
/// any destructor can run.
pub(super) struct OutputTable<T> {
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
    value: Option<Arc<T>>,
    reservation: Option<HostOutputReservation>,
}

impl<T> OutputTable<T> {
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
        bytes: u64,
        value: Arc<T>,
    ) -> Result<ReleaseId, (LoaderError, Arc<T>)> {
        let reservation = match self.reserve(bytes) {
            Ok(value) => value,
            Err(error) => return Err((error, value)),
        };
        Ok(reservation.fill(value))
    }

    pub(super) fn reserve(
        self: &Arc<Self>,
        bytes: u64,
    ) -> Result<OutputReservation<T>, LoaderError> {
        let resource = self.resources.reserve_output(bytes)?;
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(index) = state.free.take() {
            let slot = state
                .slots
                .get_mut(index)
                .expect("free output index remains addressable");
            debug_assert!(!slot.reserved && slot.value.is_none() && slot.epoch != u64::MAX);
            slot.epoch += 1;
            slot.reserved = true;
            slot.reservation = Some(resource);
            return Ok(OutputReservation {
                table: Arc::clone(self),
                index,
                epoch: slot.epoch,
                active: true,
            });
        }
        if state.slots.len() >= self.maximum_slots {
            drop(state);
            return Err(LoaderError::CapacityExhausted {
                resource: "host output slots",
                limit: u64::try_from(self.maximum_slots).unwrap_or(u64::MAX),
            });
        }
        let index = state.slots.len();
        state.slots.push(Slot {
            epoch: 1,
            last_consumed_epoch: None,
            reserved: true,
            value: None,
            reservation: Some(resource),
        });
        Ok(OutputReservation {
            table: Arc::clone(self),
            index,
            epoch: 1,
            active: true,
        })
    }

    pub(super) fn release(&self, id: ReleaseId) -> Result<Arc<T>, OutputError> {
        if !id.is_valid_or_empty() || id.is_empty() {
            return Err(OutputError::Invalid);
        }
        if id.issuer != self.issuer {
            return Err(OutputError::Stale);
        }
        let index = usize::try_from(id.slot - 1).map_err(|_| OutputError::Stale)?;
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let slot = state.slots.get_mut(index).ok_or(OutputError::Stale)?;
        if slot.epoch != id.epoch || slot.reserved {
            return Err(OutputError::Stale);
        }
        if slot.value.is_none() {
            return if slot.last_consumed_epoch == Some(id.epoch) {
                Err(OutputError::Protocol)
            } else {
                Err(OutputError::Stale)
            };
        }
        let value = slot.value.take().expect("validated live output slot");
        slot.last_consumed_epoch = Some(id.epoch);
        let reservation = slot.reservation.take();
        if slot.epoch != u64::MAX {
            state.free.release(index);
        }
        drop(state);
        drop(reservation);
        Ok(value)
    }

    fn id(&self, index: usize, epoch: u64) -> ReleaseId {
        ReleaseId {
            issuer: self.issuer,
            slot: u64::try_from(index).expect("bounded output index fits u64") + 1,
            epoch,
        }
    }
}

pub(super) struct OutputReservation<T> {
    table: Arc<OutputTable<T>>,
    index: usize,
    epoch: u64,
    active: bool,
}

impl<T> OutputReservation<T> {
    /// Publishes into an exclusively reserved output slot without another
    /// allocation or capacity check.
    pub(super) fn fill(mut self, value: Arc<T>) -> ReleaseId {
        let mut state = self
            .table
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let slot = state
            .slots
            .get_mut(self.index)
            .expect("reserved output slot remains addressable");
        assert!(
            slot.reserved && slot.epoch == self.epoch && slot.value.is_none(),
            "reserved output slot changed before publication"
        );
        slot.reserved = false;
        slot.value = Some(value);
        self.active = false;
        self.table.id(self.index, self.epoch)
    }
}

impl<T> Drop for OutputReservation<T> {
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
                .expect("reserved output slot remains addressable");
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

impl<T> Drop for OutputTable<T> {
    fn drop(&mut self) {
        let state = self
            .state
            .get_mut()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut values = Vec::new();
        let mut reservations = Vec::new();
        for slot in &mut state.slots {
            slot.reserved = false;
            if let Some(value) = slot.value.take() {
                values.push(value);
            }
            if let Some(reservation) = slot.reservation.take() {
                reservations.push(reservation);
            }
        }
        drop(values);
        drop(reservations);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::catalog_resources::NativeCatalogLimits;

    #[test]
    fn release_is_exact_and_slot_reuse_advances_epoch() {
        let limits = NativeCatalogLimits {
            maximum_host_outputs: 1,
            maximum_host_output_bytes: 8,
            ..NativeCatalogLimits::default()
        };
        let resources = HostResourceLedger::new(&limits);
        let table = Arc::new(OutputTable::new(9, 1, Arc::clone(&resources)));
        let first = table.insert(8, Arc::new(1_u8)).unwrap();
        assert_eq!(resources.snapshot().outputs, 1);
        assert_eq!(*table.release(first).unwrap(), 1);
        assert_eq!(resources.snapshot().outputs, 0);
        assert_eq!(table.release(first), Err(OutputError::Protocol));
        let second = table.insert(1, Arc::new(2_u8)).unwrap();
        assert_eq!(second.slot, first.slot);
        assert_eq!(second.epoch, first.epoch + 1);
    }

    #[test]
    fn duplicate_release_is_protocol_misuse_but_reused_epoch_is_stale() {
        let limits = NativeCatalogLimits {
            maximum_host_outputs: 1,
            maximum_host_output_bytes: 8,
            ..NativeCatalogLimits::default()
        };
        let resources = HostResourceLedger::new(&limits);
        let table = Arc::new(OutputTable::new(9, 1, resources));
        let first = table.insert(1, Arc::new(1_u8)).unwrap();
        drop(table.release(first).unwrap());
        let duplicate = table.release(first);
        let second = table.insert(1, Arc::new(2_u8)).unwrap();
        let old_epoch = table.release(first);
        drop(table.release(second).unwrap());

        assert_eq!(duplicate, Err(OutputError::Protocol));
        assert_eq!(old_epoch, Err(OutputError::Stale));
    }

    #[test]
    fn growth_and_reuse_have_constant_reserve_probe_budgets() {
        const COUNT: usize = 256;
        let limits = NativeCatalogLimits {
            maximum_host_outputs: COUNT,
            maximum_host_output_bytes: u64::try_from(COUNT).unwrap(),
            ..NativeCatalogLimits::default()
        };
        let resources = HostResourceLedger::new(&limits);
        let table = Arc::new(OutputTable::new(9, COUNT, resources));
        let ids: Vec<_> = (0..COUNT)
            .map(|value| table.insert(1, Arc::new(value)).unwrap())
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
            .map(|value| table.insert(1, Arc::new(value)).unwrap())
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
    fn byte_and_slot_bounds_are_both_enforced() {
        let limits = NativeCatalogLimits {
            maximum_host_outputs: 1,
            maximum_host_output_bytes: 2,
            ..NativeCatalogLimits::default()
        };
        let resources = HostResourceLedger::new(&limits);
        let table = Arc::new(OutputTable::new(9, 1, resources));
        assert!(table.insert(3, Arc::new(1_u8)).is_err());
        let first = table.insert(2, Arc::new(2_u8)).unwrap();
        assert!(table.insert(0, Arc::new(3_u8)).is_err());
        drop(table.release(first).unwrap());
    }

    #[test]
    fn unfilled_reservation_is_exclusive_stale_and_rollback_reuses_with_new_epoch() {
        let limits = NativeCatalogLimits {
            maximum_host_outputs: 1,
            maximum_host_output_bytes: 8,
            ..NativeCatalogLimits::default()
        };
        let resources = HostResourceLedger::new(&limits);
        let table = Arc::new(OutputTable::new(9, 1, Arc::clone(&resources)));
        let reservation = table.reserve(8).unwrap();
        let predicted = ReleaseId {
            issuer: table.issuer,
            slot: u64::try_from(reservation.index).unwrap() + 1,
            epoch: reservation.epoch,
        };
        assert_eq!(table.release(predicted), Err(OutputError::Stale));
        let snapshot = resources.snapshot();
        assert_eq!(snapshot.outputs, 1);
        assert_eq!(snapshot.output_bytes, 8);
        assert!(table.reserve(0).is_err());
        assert_eq!(resources.snapshot().outputs, 1);

        drop(reservation);
        let snapshot = resources.snapshot();
        assert_eq!(snapshot.outputs, 0);
        assert_eq!(snapshot.output_bytes, 0);
        assert_eq!(table.release(predicted), Err(OutputError::Stale));
        let next = table.reserve(1).unwrap();
        let release = next.fill(Arc::new(3_u8));
        assert_eq!(release.slot, predicted.slot);
        assert_eq!(release.epoch, predicted.epoch + 1);
        assert_eq!(*table.release(release).unwrap(), 3);
        assert_eq!(resources.snapshot().outputs, 0);
    }
}
