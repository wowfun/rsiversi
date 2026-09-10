use crate::SourceRef;
use std::collections::VecDeque;

/// Maximum metadata-only source entries retained by one conversation block.
pub const MAXIMUM_BLOCK_SOURCES: usize = 4096;

/// Exact deduplication result and position in the retained source order.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SourceAdmission {
    /// The renderer already retains this source.
    Existing(usize),
    /// The source was inserted at this position, possibly between newer and older sources.
    Inserted(usize),
}

/// Source admission failed without changing retained membership.
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum SourceIndexError {
    /// Sequence zero is not a durable Fact identity.
    #[error("source sequence must be positive")]
    InvalidSource,
    /// The renderer must evict a corresponding presentation piece before inserting another.
    #[error("block source index capacity exhausted")]
    Capacity,
}

/// Bounded exact source membership; payload and eviction policy remain with the renderer.
#[derive(Clone, Debug, Default)]
pub struct SourceIndex {
    sources: VecDeque<SourceRef>,
}
impl SourceIndex {
    /// Number of retained sources.
    pub fn len(&self) -> usize {
        self.sources.len()
    }
    /// Whether no source is retained.
    pub fn is_empty(&self) -> bool {
        self.sources.is_empty()
    }
    /// Allocated metadata bytes, including spare source slots.
    pub fn owned_bytes(&self) -> usize {
        self.sources.capacity() * std::mem::size_of::<SourceRef>()
    }
    /// Exact lookup independent of the highest retained sequence.
    pub fn position(&self, source: SourceRef) -> Option<usize> {
        self.sources.binary_search(&source).ok()
    }
    /// Borrows the retained source at a presentation position.
    pub fn get(&self, position: usize) -> Option<SourceRef> {
        self.sources.get(position).copied()
    }
    /// Iterates retained membership in semantic source order.
    pub fn iter(&self) -> impl DoubleEndedIterator<Item = SourceRef> + ExactSizeIterator + '_ {
        self.sources.iter().copied()
    }
    /// Inserts only missing sources; duplicates do not consume capacity.
    ///
    /// # Errors
    /// Rejects sequence zero or a missing source when the index is full.
    pub fn insert(&mut self, source: SourceRef) -> Result<SourceAdmission, SourceIndexError> {
        if source.seq == 0 {
            return Err(SourceIndexError::InvalidSource);
        }
        let position = match self.sources.binary_search(&source) {
            Ok(position) => return Ok(SourceAdmission::Existing(position)),
            Err(position) => position,
        };
        if self.len() == MAXIMUM_BLOCK_SOURCES {
            return Err(SourceIndexError::Capacity);
        }
        if self.sources.len() == self.sources.capacity() {
            let capacity = (self.sources.capacity().max(2) * 2).min(MAXIMUM_BLOCK_SOURCES);
            self.sources.reserve_exact(capacity - self.sources.len());
        }
        self.sources.insert(position, source);
        Ok(SourceAdmission::Inserted(position))
    }
    /// Forgets the source when its corresponding presentation piece is evicted.
    pub fn remove(&mut self, position: usize) -> Option<SourceRef> {
        self.sources.remove(position)
    }
}
