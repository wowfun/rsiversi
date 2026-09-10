use super::{AgentControlRecord, Result, SessionFact, TurnError};
use rsi_agent_session_protocol::{MAXIMUM_FACTS_PER_READ, MAXIMUM_SESSION_FACT_BYTES};
use std::fmt;
use std::ops::Deref;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

/// Default aggregate encoded payload retained by one observation owner.
pub const DEFAULT_MAXIMUM_RETAINED_OBSERVATION_BYTES: usize = 64 * 1024 * 1024;

/// Shared admission used by a Turn service or transport decoder to issue observations.
///
/// This integration seam owns canonical encoded-byte accounting, independently
/// of the adapter's transient read and serialization buffers.
#[derive(Clone, Debug)]
pub struct ObservationRetention {
    inner: Arc<RetentionState>,
}

#[derive(Debug)]
struct RetentionState {
    maximum: usize,
    retained: AtomicUsize,
}

struct ByteReservation {
    state: Arc<RetentionState>,
    bytes: usize,
}

impl ByteReservation {
    fn split(&mut self, bytes: usize) -> Self {
        self.bytes = self
            .bytes
            .checked_sub(bytes)
            .expect("item was admitted with its page");
        Self {
            state: self.state.clone(),
            bytes,
        }
    }
}

impl Drop for ByteReservation {
    fn drop(&mut self) {
        self.state.retained.fetch_sub(self.bytes, Ordering::AcqRel);
    }
}

impl ObservationRetention {
    /// Creates a pool within the standard maximum, large enough for any valid Fact.
    pub fn new(maximum: usize) -> Result<Self> {
        if !(MAXIMUM_SESSION_FACT_BYTES..=DEFAULT_MAXIMUM_RETAINED_OBSERVATION_BYTES)
            .contains(&maximum)
        {
            return Err(TurnError::Invalid(format!(
                "observation retention must be within {MAXIMUM_SESSION_FACT_BYTES}..={DEFAULT_MAXIMUM_RETAINED_OBSERVATION_BYTES} bytes"
            )));
        }
        Ok(Self {
            inner: Arc::new(RetentionState {
                maximum,
                retained: AtomicUsize::new(0),
            }),
        })
    }

    /// Current canonical payload bytes retained by all issued item clones.
    pub fn retained_bytes(&self) -> usize {
        self.inner.retained.load(Ordering::Acquire)
    }

    fn reserve(&self, bytes: usize) -> Result<ByteReservation> {
        self.inner
            .retained
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |retained| {
                retained
                    .checked_add(bytes)
                    .filter(|total| *total <= self.inner.maximum)
            })
            .map_err(|_| TurnError::Capacity)?;
        Ok(ByteReservation {
            state: self.inner.clone(),
            bytes,
        })
    }

    fn retain_page<T, U>(
        &self,
        values: Vec<Arc<T>>,
        encoded_len: fn(&T) -> usize,
        wrap: fn(Arc<Retained<T>>) -> U,
    ) -> Result<Vec<U>> {
        if values.len() > MAXIMUM_FACTS_PER_READ {
            return Err(TurnError::Invalid(format!(
                "observation page exceeds {MAXIMUM_FACTS_PER_READ} records"
            )));
        }
        let bytes = values
            .iter()
            .try_fold(0_usize, |total, value| {
                total.checked_add(encoded_len(value))
            })
            .ok_or(TurnError::Capacity)?;
        let mut page = self.reserve(bytes)?;
        Ok(values
            .into_iter()
            .map(|value| {
                let lease = page.split(encoded_len(&value));
                wrap(Arc::new(Retained {
                    value,
                    _lease: lease,
                }))
            })
            .collect())
    }

    /// Atomically admits one page and shares each Fact with its final-owner lease.
    pub fn retain_facts(&self, facts: Vec<Arc<SessionFact>>) -> Result<Vec<ObservedFact>> {
        self.retain_page(facts, SessionFact::encoded_len, ObservedFact)
    }

    /// Atomically admits one control page with independent final-owner item leases.
    pub fn retain_controls(
        &self,
        records: Vec<Arc<AgentControlRecord>>,
    ) -> Result<Vec<ObservedControl>> {
        self.retain_page(records, AgentControlRecord::encoded_len, ObservedControl)
    }

    /// Admits one already shared live Fact without copying its payload.
    pub fn retain_fact(&self, fact: Arc<SessionFact>) -> Result<ObservedFact> {
        let lease = self.reserve(fact.encoded_len())?;
        Ok(ObservedFact(Arc::new(Retained {
            value: fact,
            _lease: lease,
        })))
    }
}

impl Default for ObservationRetention {
    fn default() -> Self {
        Self::new(DEFAULT_MAXIMUM_RETAINED_OBSERVATION_BYTES).expect("valid standard retention")
    }
}

struct Retained<T> {
    value: Arc<T>,
    _lease: ByteReservation,
}

macro_rules! observed_handle {
    ($name:ident, $value:ty, $description:literal) => {
        #[doc = $description]
        #[derive(Clone)]
        pub struct $name(Arc<Retained<$value>>);

        impl AsRef<$value> for $name {
            fn as_ref(&self) -> &$value {
                &self.0.value
            }
        }
        impl Deref for $name {
            type Target = $value;
            fn deref(&self) -> &Self::Target {
                self.as_ref()
            }
        }
        impl fmt::Debug for $name {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter
                    .debug_tuple(stringify!($name))
                    .field(self.as_ref())
                    .finish()
            }
        }
        impl PartialEq for $name {
            fn eq(&self, other: &Self) -> bool {
                self.as_ref() == other.as_ref()
            }
        }
    };
}

observed_handle!(
    ObservedFact,
    SessionFact,
    "Immutable observed Fact retaining its byte reservation through the last clone."
);
observed_handle!(
    ObservedControl,
    AgentControlRecord,
    "Immutable observed control retaining its byte reservation through the last clone."
);

#[cfg(test)]
mod tests {
    use super::*;
    use rsi_agent_session_protocol::{SessionFactBody, TurnId};

    fn fact() -> Arc<SessionFact> {
        Arc::new(
            SessionFact::new(
                1,
                1,
                SessionFactBody::CancelRequested {
                    turn_id: TurnId::new("retained-fact").unwrap(),
                    reason: None,
                },
            )
            .unwrap(),
        )
    }

    #[test]
    fn final_item_clone_owns_its_reservation_independently_of_the_page_and_pool() {
        let pool = ObservationRetention::default();
        let value = fact();
        let bytes = value.encoded_len();
        let mut page = pool.retain_facts(vec![value.clone(), value]).unwrap();
        assert_eq!(pool.retained_bytes(), 2 * bytes);
        let first = page.remove(0);
        let clone = first.clone();
        drop(first);
        drop(page);
        assert_eq!(pool.retained_bytes(), bytes);
        let witness = pool.clone();
        drop(pool);
        assert_eq!(clone.seq(), 1);
        drop(clone);
        assert_eq!(witness.retained_bytes(), 0);
    }

    #[test]
    fn failed_page_admission_is_atomic_and_capacity_is_reusable() {
        // A private tiny pool isolates reservation arithmetic from payload size.
        let value = fact();
        let bytes = value.encoded_len();
        let pool = ObservationRetention {
            inner: Arc::new(RetentionState {
                maximum: bytes * 2,
                retained: AtomicUsize::new(0),
            }),
        };
        let held = pool.retain_fact(value.clone()).unwrap();
        assert!(matches!(
            pool.retain_facts(vec![value.clone(), value.clone()]),
            Err(TurnError::Capacity)
        ));
        assert_eq!(pool.retained_bytes(), bytes);
        drop(held);
        let page = pool.retain_facts(vec![value.clone(), value]).unwrap();
        assert_eq!(pool.retained_bytes(), 2 * bytes);
        drop(page);
        assert_eq!(pool.retained_bytes(), 0);
    }

    #[test]
    fn configured_pool_must_admit_a_maximum_fact_and_never_widen_the_standard_budget() {
        assert!(ObservationRetention::new(MAXIMUM_SESSION_FACT_BYTES - 1).is_err());
        assert!(ObservationRetention::new(MAXIMUM_SESSION_FACT_BYTES).is_ok());
        assert!(ObservationRetention::new(DEFAULT_MAXIMUM_RETAINED_OBSERVATION_BYTES + 1).is_err());
    }

    #[test]
    fn oversized_page_is_invalid_even_when_payload_capacity_is_available() {
        let pool = ObservationRetention::default();
        assert!(matches!(
            pool.retain_facts(vec![fact(); MAXIMUM_FACTS_PER_READ + 1]),
            Err(TurnError::Invalid(_))
        ));
        assert_eq!(pool.retained_bytes(), 0);
        assert!(pool.retain_fact(fact()).is_ok());
    }
}
