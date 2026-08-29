/// Shared reusable-slot index for bounded native host tables.
///
/// Owners publish an index only after its slot becomes vacant and omit
/// epoch-exhausted slots. Consequently, reservation never scans table storage:
/// growth appends and reuse pops one known-vacant index.
#[derive(Default)]
pub(super) struct FreeSlotIndex {
    indices: Vec<usize>,
    #[cfg(test)]
    take_probes: usize,
}

impl FreeSlotIndex {
    pub(super) fn take(&mut self) -> Option<usize> {
        let index = self.indices.pop();
        #[cfg(test)]
        if index.is_some() {
            self.take_probes += 1;
        }
        index
    }

    pub(super) fn release(&mut self, index: usize) {
        self.indices.push(index);
    }

    #[cfg(test)]
    pub(super) fn take_probes(&self) -> usize {
        self.take_probes
    }
}
