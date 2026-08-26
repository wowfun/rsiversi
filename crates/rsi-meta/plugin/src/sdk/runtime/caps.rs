use super::values::CapValue;
use crate::{
    CAP_KIND_CLEANUP, CAP_KIND_FACTORY, CAP_KIND_INSTANCE, CapId, NativePlugin, RIGHT_MUTATE,
    RIGHT_RETAIN, STATUS_LIMIT_EXCEEDED, STATUS_PROTOCOL_ERROR, STATUS_STALE_CAPABILITY,
    STATUS_WRONG_CAPABILITY,
};

const MAX_CAPABILITIES: usize = 4_096;

struct Slot<P: NativePlugin> {
    epoch: u64,
    kind: u32,
    rights: u32,
    value: Option<CapValue<P>>,
    refs: u64,
    closing: bool,
    last_consumed_epoch: u64,
}

pub(super) struct CapTable<P: NativePlugin> {
    issuer: u64,
    slots: Vec<Slot<P>>,
    free: Vec<usize>,
    factory_destroyed: bool,
}

impl<P: NativePlugin> CapTable<P> {
    pub(super) fn new(issuer: u64) -> Self {
        Self {
            issuer,
            slots: Vec::new(),
            free: Vec::new(),
            factory_destroyed: false,
        }
    }

    pub(super) fn insert(
        &mut self,
        kind: u32,
        rights: u32,
        value: CapValue<P>,
    ) -> Result<CapId, (u32, CapValue<P>)> {
        let index = match self.next_slot() {
            Ok(index) => index,
            Err(status) => return Err((status, value)),
        };
        let slot = &mut self.slots[index];
        slot.kind = kind;
        slot.rights = rights;
        slot.value = Some(value);
        slot.refs = 1;
        slot.closing = false;
        Ok(CapId {
            issuer: self.issuer,
            slot: u64::try_from(index + 1).expect("bounded capability index fits u64"),
            epoch: slot.epoch,
            kind,
            rights,
        })
    }

    fn next_slot(&mut self) -> Result<usize, u32> {
        while let Some(index) = self.free.pop() {
            let slot = &mut self.slots[index];
            if let Some(epoch) = slot.epoch.checked_add(1) {
                slot.epoch = epoch;
                return Ok(index);
            }
        }
        if self.slots.len() >= MAX_CAPABILITIES {
            return Err(STATUS_LIMIT_EXCEEDED);
        }
        self.slots.push(Slot {
            epoch: 1,
            kind: 0,
            rights: 0,
            value: None,
            refs: 0,
            closing: false,
            last_consumed_epoch: 0,
        });
        Ok(self.slots.len() - 1)
    }

    pub(super) fn get(
        &self,
        id: CapId,
        kind: u32,
        required_rights: u32,
    ) -> Result<CapValue<P>, u32> {
        let slot = self.valid_slot(id, kind, required_rights)?;
        if slot.closing {
            return Err(STATUS_PROTOCOL_ERROR);
        }
        slot.value.clone().ok_or(STATUS_STALE_CAPABILITY)
    }

    pub(super) fn retain(&mut self, id: CapId) -> Result<(), u32> {
        let slot = self.valid_slot_mut(id, id.kind, RIGHT_RETAIN)?;
        if slot.closing {
            return Err(STATUS_PROTOCOL_ERROR);
        }
        slot.refs = slot.refs.checked_add(1).ok_or(STATUS_LIMIT_EXCEEDED)?;
        Ok(())
    }

    pub(super) fn release_external(&mut self, id: CapId) -> Result<Option<CapValue<P>>, u32> {
        let index = self.valid_index(id, id.kind, 0)?;
        let slot = &mut self.slots[index];
        if id.kind == CAP_KIND_CLEANUP
            && matches!(
                slot.value.as_ref(),
                Some(CapValue::Cleanup(cleanup)) if !cleanup.is_finished()
            )
        {
            return Err(STATUS_PROTOCOL_ERROR);
        }
        if slot.refs == 1
            && !slot.closing
            && matches!(id.kind, CAP_KIND_FACTORY | CAP_KIND_INSTANCE)
        {
            return Err(STATUS_PROTOCOL_ERROR);
        }
        Ok(self.release_index(index))
    }

    pub(super) fn release_owned(&mut self, id: CapId) -> Option<CapValue<P>> {
        if let Ok(index) = self.valid_index(id, id.kind, 0) {
            self.release_index(index)
        } else {
            None
        }
    }

    pub(super) fn destroy(&mut self, id: CapId, kind: u32) -> Result<Option<CapValue<P>>, u32> {
        let index = self.valid_index(id, kind, RIGHT_MUTATE)?;
        let slot = &mut self.slots[index];
        if slot.closing {
            return Err(STATUS_PROTOCOL_ERROR);
        }
        slot.closing = true;
        if kind == CAP_KIND_FACTORY {
            self.factory_destroyed = true;
        }
        Ok(self.release_index(index))
    }

    fn release_index(&mut self, index: usize) -> Option<CapValue<P>> {
        let slot = &mut self.slots[index];
        slot.refs -= 1;
        if slot.refs == 0 {
            let retired = slot.value.take();
            slot.last_consumed_epoch = slot.epoch;
            if slot.epoch != u64::MAX {
                self.free.push(index);
            }
            retired
        } else {
            None
        }
    }

    fn valid_slot(&self, id: CapId, kind: u32, rights: u32) -> Result<&Slot<P>, u32> {
        let index = self.valid_index(id, kind, rights)?;
        Ok(&self.slots[index])
    }

    fn valid_slot_mut(&mut self, id: CapId, kind: u32, rights: u32) -> Result<&mut Slot<P>, u32> {
        let index = self.valid_index(id, kind, rights)?;
        Ok(&mut self.slots[index])
    }

    fn valid_index(&self, id: CapId, kind: u32, rights: u32) -> Result<usize, u32> {
        if !id.is_structurally_valid() || id.issuer != self.issuer {
            return Err(STATUS_WRONG_CAPABILITY);
        }
        let index = usize::try_from(id.slot - 1).map_err(|_| STATUS_STALE_CAPABILITY)?;
        let Some(slot) = self.slots.get(index) else {
            return Err(STATUS_STALE_CAPABILITY);
        };
        if slot.value.is_none() {
            return Err(if slot.last_consumed_epoch == id.epoch {
                STATUS_PROTOCOL_ERROR
            } else {
                STATUS_STALE_CAPABILITY
            });
        }
        if slot.epoch != id.epoch {
            return Err(STATUS_STALE_CAPABILITY);
        }
        if id.kind != slot.kind
            || id.rights != slot.rights
            || id.kind != kind
            || id.rights & rights != rights
        {
            return Err(STATUS_WRONG_CAPABILITY);
        }
        Ok(index)
    }

    pub(super) fn can_finalize(&self) -> bool {
        self.factory_destroyed && self.slots.iter().all(|slot| slot.value.is_none())
    }
}
