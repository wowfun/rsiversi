use crate::{MAXIMUM_STORE_FACT_PAGE_BYTES, Result, SessionFact, StoreError};
use rsi_agent_session_protocol::validate_fact_sequence;

/// Maximum Facts in one atomic, byte-bounded suffix capture.
pub const MAXIMUM_STORE_SUFFIX_FACTS: usize = 1024;

/// A bounded contiguous suffix and its atomically captured source horizon.
#[derive(Clone, Debug, PartialEq)]
pub struct StoreFactSuffix {
    /// Source Fact watermark captured before selecting rows.
    pub through_seq: u64,
    /// Full canonical Fact-prefix digest at that watermark.
    pub fact_prefix_sha256: String,
    /// Ascending contiguous suffix; empty when no first body fits.
    pub facts: Vec<SessionFact>,
    /// Exact aggregate canonical encoded Fact bytes copied.
    pub encoded_bytes: usize,
    /// A next Fact existed but could not fit the remaining byte budget.
    pub byte_limited: bool,
}
/// Validates caller bounds before any Store preparation or payload work.
pub fn validate_suffix_limits(limit: usize, maximum_bytes: usize) -> Result<()> {
    if limit == 0
        || limit > MAXIMUM_STORE_SUFFIX_FACTS
        || maximum_bytes == 0
        || maximum_bytes > MAXIMUM_STORE_FACT_PAGE_BYTES
    {
        return Err(StoreError::Invalid("invalid suffix byte limit".into()));
    }
    Ok(())
}
impl StoreFactSuffix {
    /// Exclusive start of the retained contiguous suffix.
    pub fn after_seq(&self) -> u64 {
        self.facts
            .first()
            .map_or(self.through_seq, |fact| fact.seq() - 1)
    }
    /// Revalidates count, bytes, cursor coverage and the fixed prefix digest.
    pub fn validate(&self, limit: usize, maximum_bytes: usize) -> Result<()> {
        validate_suffix_limits(limit, maximum_bytes)?;
        if self.facts.len() > limit
            || self.encoded_bytes > maximum_bytes
            || self
                .facts
                .iter()
                .try_fold(0_usize, |bytes, fact| bytes.checked_add(fact.encoded_len()))
                != Some(self.encoded_bytes)
            || self
                .facts
                .last()
                .is_some_and(|fact| fact.seq() != self.through_seq)
            || (self.facts.is_empty() && self.through_seq != 0 && !self.byte_limited)
            || (self.byte_limited && self.after_seq() == 0)
            || self.fact_prefix_sha256.len() != 64
            || !self
                .fact_prefix_sha256
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        {
            return Err(StoreError::Corrupt("invalid bounded Fact suffix".into()));
        }
        let mut after = self.after_seq();
        for page in self
            .facts
            .chunks(rsi_agent_session_protocol::MAXIMUM_FACTS_PER_READ)
        {
            validate_fact_sequence(after, page)
                .map_err(|error| StoreError::Corrupt(error.to_string()))?;
            if let Some(last) = page.last() {
                after = last.seq();
            }
        }
        Ok(())
    }
}
