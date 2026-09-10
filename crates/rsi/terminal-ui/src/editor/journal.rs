use std::collections::VecDeque;

pub const MAXIMUM_CHANGES: usize = 128;
pub const MAXIMUM_BYTES: usize = 1024 * 1024;

#[derive(Clone, Debug)]
pub struct Change {
    pub start: usize,
    pub removed: Box<str>,
    pub inserted: Box<str>,
    pub before: usize,
    pub after: usize,
}
impl Change {
    fn bytes(&self) -> usize {
        self.removed.len() + self.inserted.len()
    }
}

#[derive(Clone, Debug, Default)]
pub struct Journal {
    pub undo: VecDeque<Change>,
    pub redo: Vec<Change>,
    pub bytes: usize,
}
impl Journal {
    pub fn clear(&mut self) {
        self.undo.clear();
        self.redo.clear();
        self.bytes = 0;
    }
    pub fn trim(&mut self, budget: usize) {
        while self.bytes > budget.min(MAXIMUM_BYTES)
            || self.undo.len() + self.redo.len() > MAXIMUM_CHANGES
        {
            let change = self.undo.pop_front().unwrap_or_else(|| self.redo.remove(0));
            self.bytes -= change.bytes();
        }
    }
    pub fn record(&mut self, change: Change, budget: usize) {
        for change in self.redo.drain(..) {
            self.bytes -= change.bytes();
        }
        self.bytes += change.bytes();
        self.undo.push_back(change);
        self.trim(budget);
    }
}
