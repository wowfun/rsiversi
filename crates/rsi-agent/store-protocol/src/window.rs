use crate::{MAXIMUM_STORE_FACT_PAGE_BYTES, Result, SessionFact, StoreError};

/// Maximum inspected coordinates in one forward byte-bounded window.
pub const MAXIMUM_STORE_WINDOW_FACTS: usize = 256;

/// An original too large for the caller's complete materialization budget.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StoreFactOmission {
    /// Exact omitted Fact coordinate.
    pub seq: u64,
    /// Original canonical byte length; no body was copied or decoded.
    pub encoded_bytes: usize,
}
/// One atomic forward interval with explicit oversized originals.
#[derive(Clone, Debug, PartialEq)]
pub struct StoreFactWindow {
    /// Exclusive caller cursor.
    pub after_seq: u64,
    /// Last inspected coordinate, including omissions.
    pub through_seq: u64,
    /// Read-time source horizon in the same snapshot.
    pub durable_seq: u64,
    /// Admitted originals, ordered by sequence.
    pub facts: Vec<SessionFact>,
    /// Length-only omitted originals, ordered by sequence.
    pub omitted: Vec<StoreFactOmission>,
    /// Total canonical bytes of admitted originals.
    pub encoded_bytes: usize,
}
/// Rejects invalid window bounds before Store preparation or allocation.
pub fn validate_window_limits(limit: usize, maximum_bytes: usize) -> Result<()> {
    if limit == 0
        || limit > MAXIMUM_STORE_WINDOW_FACTS
        || maximum_bytes == 0
        || maximum_bytes > MAXIMUM_STORE_FACT_PAGE_BYTES
    {
        return Err(StoreError::Invalid(
            "invalid forward Fact window bounds".into(),
        ));
    }
    Ok(())
}
impl StoreFactWindow {
    /// Checks exact contiguous coordinate coverage and pre-body budget observations.
    pub fn validate(&self, limit: usize, maximum_bytes: usize) -> Result<()> {
        validate_window_limits(limit, maximum_bytes)?;
        let count = self.facts.len().checked_add(self.omitted.len());
        if self.after_seq > self.through_seq
            || self.through_seq > self.durable_seq
            || count.is_none_or(|count| {
                count > limit || self.through_seq - self.after_seq != count as u64
            })
            || self.encoded_bytes > maximum_bytes
            || self
                .facts
                .iter()
                .try_fold(0usize, |total, fact| total.checked_add(fact.encoded_len()))
                != Some(self.encoded_bytes)
            || self
                .facts
                .windows(2)
                .any(|pair| pair[0].seq() >= pair[1].seq())
            || self
                .omitted
                .windows(2)
                .any(|pair| pair[0].seq >= pair[1].seq)
            || self.omitted.iter().any(|row| {
                row.encoded_bytes <= maximum_bytes
                    || row.encoded_bytes > rsi_agent_session_protocol::MAXIMUM_SESSION_FACT_BYTES
            })
        {
            return Err(StoreError::Corrupt("invalid forward Fact window".into()));
        }
        let mut positions = self
            .facts
            .iter()
            .map(SessionFact::seq)
            .chain(self.omitted.iter().map(|row| row.seq))
            .collect::<Vec<_>>();
        positions.sort_unstable();
        if positions
            .iter()
            .enumerate()
            .any(|(index, &seq)| self.after_seq.checked_add(index as u64 + 1) != Some(seq))
        {
            return Err(StoreError::Corrupt(
                "noncontiguous forward Fact window".into(),
            ));
        }
        Ok(())
    }
}
