use super::values::{FactoryCell, InstanceCell};
use crate::{NativeInstance, STATUS_BUSY, STATUS_PROTOCOL_ERROR, STATUS_REENTRANT};
use std::sync::atomic::Ordering;

pub(super) struct FactoryGate<'a, P> {
    cell: &'a FactoryCell<P>,
}

impl<'a, P> FactoryGate<'a, P> {
    pub(super) fn acquire(cell: &'a FactoryCell<P>) -> Result<Self, u32> {
        cell.gate
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .map_err(|_| STATUS_BUSY)?;
        Ok(Self { cell })
    }
}

impl<P> Drop for FactoryGate<'_, P> {
    fn drop(&mut self) {
        self.cell.gate.store(false, Ordering::Release);
    }
}

pub(super) struct InstanceGate<'a, I: NativeInstance> {
    cell: &'a InstanceCell<I>,
}

impl<'a, I: NativeInstance> InstanceGate<'a, I> {
    pub(super) fn acquire(cell: &'a InstanceCell<I>, callback_id: u64) -> Result<Self, u32> {
        if cell.closing.load(Ordering::SeqCst) {
            return Err(STATUS_BUSY);
        }
        let owner = cell
            .owner_lineage
            .compare_exchange(0, callback_id, Ordering::SeqCst, Ordering::SeqCst)
            .unwrap_or_else(|owner| owner);
        if owner != 0 {
            return Err(if owner == callback_id {
                STATUS_REENTRANT
            } else {
                STATUS_BUSY
            });
        }
        // Destruction may have closed admission after the first check but
        // before this callback claimed the lineage. These cross-checks are
        // sequentially consistent so callback admission and destruction cannot
        // both succeed after reading stale values from the other atomic.
        if cell.closing.load(Ordering::SeqCst) {
            cell.owner_lineage.store(0, Ordering::SeqCst);
            return Err(STATUS_BUSY);
        }
        Ok(Self { cell })
    }

    pub(super) fn begin_destruction(cell: &InstanceCell<I>) -> Result<(), u32> {
        cell.closing
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .map_err(|_| STATUS_PROTOCOL_ERROR)?;
        // A callback that raced the close rechecks `closing` after claiming
        // its lineage. Observing an existing owner instead reopens admission
        // and leaves the one-shot destroy capability unconsumed.
        if cell.owner_lineage.load(Ordering::SeqCst) != 0 {
            cell.closing.store(false, Ordering::SeqCst);
            return Err(STATUS_BUSY);
        }
        Ok(())
    }
}

impl<I: NativeInstance> Drop for InstanceGate<'_, I> {
    fn drop(&mut self) {
        self.cell.owner_lineage.store(0, Ordering::SeqCst);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Instance;

    impl crate::NativeInstance for Instance {
        fn activate(&mut self, _: &mut crate::Activation<'_>) -> Result<(), String> {
            Ok(())
        }

        fn serve(&mut self, _: &[u8], _: &mut crate::ProviderChannel<'_>) -> Result<(), String> {
            Ok(())
        }
    }

    #[test]
    fn instance_owner_lineage_linearizes_reentry_with_acquisition() {
        let cell = InstanceCell::new(Instance, Vec::new());
        let owner = InstanceGate::acquire(&cell, 41).expect("first callback owns gate");
        assert_eq!(
            InstanceGate::acquire(&cell, 41).err(),
            Some(STATUS_REENTRANT)
        );
        assert_eq!(InstanceGate::acquire(&cell, 42).err(), Some(STATUS_BUSY));
        drop(owner);
        assert!(InstanceGate::acquire(&cell, 42).is_ok());
    }

    #[test]
    fn successful_destruction_fences_a_previously_loaded_instance_reference() {
        let cell = InstanceCell::new(Instance, Vec::new());

        InstanceGate::begin_destruction(&cell).expect("idle instance closes");

        assert_eq!(InstanceGate::acquire(&cell, 41).err(), Some(STATUS_BUSY));
    }
}
