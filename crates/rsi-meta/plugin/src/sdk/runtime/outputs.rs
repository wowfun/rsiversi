use super::output_records::OutputRecord;
use crate::{ReleaseId, STATUS_LIMIT_EXCEEDED, STATUS_PROTOCOL_ERROR, STATUS_STALE_CAPABILITY};

const MAX_OUTPUTS: usize = 4_096;

struct Slot {
    epoch: u64,
    record: Option<OutputRecord>,
    last_consumed_epoch: u64,
}

pub(super) struct OutputTable {
    issuer: u64,
    slots: Vec<Slot>,
    free: Vec<usize>,
}

impl OutputTable {
    pub(super) fn new(issuer: u64) -> Self {
        Self {
            issuer,
            slots: Vec::new(),
            free: Vec::new(),
        }
    }

    pub(super) fn insert(&mut self, record: OutputRecord) -> Result<ReleaseId, u32> {
        let index = self.next_slot()?;
        let slot = &mut self.slots[index];
        slot.record = Some(record);
        Ok(ReleaseId {
            issuer: self.issuer,
            slot: u64::try_from(index + 1).map_err(|_| STATUS_LIMIT_EXCEEDED)?,
            epoch: slot.epoch,
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
        if self.slots.len() >= MAX_OUTPUTS {
            return Err(STATUS_LIMIT_EXCEEDED);
        }
        self.slots.push(Slot {
            epoch: 1,
            record: None,
            last_consumed_epoch: 0,
        });
        Ok(self.slots.len() - 1)
    }

    pub(super) fn get(&self, id: ReleaseId) -> &OutputRecord {
        let index = usize::try_from(id.slot - 1).expect("fresh output slot fits usize");
        self.slots[index]
            .record
            .as_ref()
            .expect("fresh output record exists")
    }

    pub(super) fn release(&mut self, id: ReleaseId) -> Result<OutputRecord, u32> {
        if !id.is_valid_or_empty() || id.is_empty() || id.issuer != self.issuer {
            return Err(STATUS_PROTOCOL_ERROR);
        }
        let index = usize::try_from(id.slot - 1).map_err(|_| STATUS_STALE_CAPABILITY)?;
        let Some(slot) = self.slots.get_mut(index) else {
            return Err(STATUS_STALE_CAPABILITY);
        };
        if slot.epoch != id.epoch {
            return Err(STATUS_STALE_CAPABILITY);
        }
        let Some(record) = slot.record.take() else {
            return Err(if slot.last_consumed_epoch == id.epoch {
                STATUS_PROTOCOL_ERROR
            } else {
                STATUS_STALE_CAPABILITY
            });
        };
        slot.last_consumed_epoch = id.epoch;
        if slot.epoch != u64::MAX {
            self.free.push(index);
        }
        Ok(record)
    }

    pub(super) fn is_empty(&self) -> bool {
        self.slots.iter().all(|slot| slot.record.is_none())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn live_output_bound_is_exact_and_released_slots_advance_epoch() {
        let mut table = OutputTable::new(17);
        let releases: Vec<_> = (0..MAX_OUTPUTS)
            .map(|_| {
                table
                    .insert(OutputRecord::diagnostic(String::new()))
                    .expect("the declared live-output population fits")
            })
            .collect();
        assert!(matches!(
            table.insert(OutputRecord::diagnostic(String::new())),
            Err(STATUS_LIMIT_EXCEEDED)
        ));

        let first = releases[0];
        drop(table.release(first).expect("live output releases exactly"));
        assert!(matches!(table.release(first), Err(STATUS_PROTOCOL_ERROR)));

        let reused = table
            .insert(OutputRecord::diagnostic(String::new()))
            .expect("released capacity is reusable");
        assert_eq!(reused.slot, first.slot);
        assert_eq!(reused.epoch, first.epoch + 1);
    }
}
