//! Recomputable fold cache admission, including the currently attached Session.
use super::Client;
use std::collections::VecDeque;
#[cfg(test)]
std::thread_local! { pub(super) static SCANNED_KEYS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) }; }
fn bytes(folds: &VecDeque<(String, bool)>) -> usize {
    #[cfg(test)]
    SCANNED_KEYS.set(SCANNED_KEYS.get() + folds.len());
    size_of::<VecDeque<(String, bool)>>()
        + folds.capacity() * size_of::<(String, bool)>()
        + folds.iter().map(|(key, _)| key.capacity()).sum::<usize>()
}
impl Client {
    pub(super) fn fold_retention(&self) -> (usize, usize) {
        self.drafts.values().fold(
            (self.state.folds.len(), bytes(&self.state.folds)),
            |(count, retained), saved| (count + saved.folds.len(), retained + bytes(&saved.folds)),
        )
    }
    pub(super) fn enforce_fold_budget(&mut self) {
        let (mut count, mut retained) = self.fold_retention();
        if count <= 4096 && retained <= 1024 * 1024 {
            return;
        }
        let mut oldest: Vec<_> = self
            .drafts
            .iter()
            .map(|(id, saved)| (saved.last_used, id.clone()))
            .collect();
        oldest.sort_unstable();
        for (_, id) in oldest {
            if count <= 4096 && retained <= 1024 * 1024 {
                break;
            }
            trim(
                &mut self.drafts.get_mut(&id).expect("retained session").folds,
                &mut count,
                &mut retained,
            );
        }
        if count > 4096 || retained > 1024 * 1024 {
            trim(&mut self.state.folds, &mut count, &mut retained);
        }
    }
}

fn trim(folds: &mut VecDeque<(String, bool)>, count: &mut usize, retained: &mut usize) {
    let slot = size_of::<(String, bool)>();
    // Account for the capacity that the one final shrink will release.
    *retained -= (folds.capacity() - folds.len()) * slot;
    while *count > 4096 || *retained > 1024 * 1024 {
        let Some((key, _)) = folds.pop_front() else {
            break;
        };
        *count -= 1;
        *retained -= key.capacity() + slot;
    }
    folds.shrink_to_fit();
    *retained += (folds.capacity() - folds.len()) * slot;
}
