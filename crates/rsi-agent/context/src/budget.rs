//! Shared, nonwaiting admission for Context ownership across execution generations.
use crate::{ContextError, Result};
use async_trait::async_trait;
use rsi_api_protocol::{ApiError, ByteAdmission, ByteReservation};
use rsi_meta::{
    ActivationPlan, ConfigValue, LocalContract, MetaError, PluginFactory, PreparedActivation,
};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

/// Default shared accounted Context capacity, independent of executor lanes.
pub const DEFAULT_CONTEXT_CAPACITY: usize = 512 * 1024 * 1024;
#[derive(Debug)]
struct Pool {
    maximum: usize,
    used: AtomicUsize,
}
/// Shared service authority; clones refer to the same pool.
#[derive(Clone, Debug)]
pub struct ContextBudget(Arc<Pool>);
impl Default for ContextBudget {
    fn default() -> Self {
        Self::new(DEFAULT_CONTEXT_CAPACITY).expect("positive default")
    }
}
impl ContextBudget {
    /// Creates a positive, checked service capacity.
    pub fn new(maximum: usize) -> Result<Self> {
        if maximum == 0 {
            return Err(ContextError::Invalid(
                "Context capacity must be positive".into(),
            ));
        }
        Ok(Self(Arc::new(Pool {
            maximum,
            used: AtomicUsize::new(0),
        })))
    }
    /// Currently admitted bytes, including retained state and temporary work.
    pub fn used(&self) -> usize {
        self.0.used.load(Ordering::Acquire)
    }
    /// Maximum admitted accounted bytes.
    pub fn maximum(&self) -> usize {
        self.0.maximum
    }
    /// Reserves before allocation; never queues behind a parked claim.
    pub fn reserve(&self, bytes: usize) -> Result<ContextCredit> {
        self.0
            .used
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |used| {
                used.checked_add(bytes)
                    .filter(|next| *next <= self.0.maximum)
            })
            .map_err(|_| ContextError::Capacity)?;
        Ok(ContextCredit {
            budget: self.clone(),
            bytes,
        })
    }
    /// Transfers exact-length checkpoint admission into the actual Store job.
    pub fn checkpoint_read(&self) -> ByteAdmission {
        let budget = self.clone();
        ByteAdmission::new(move |bytes| {
            let credit = budget.reserve(bytes).map_err(|_| ApiError::Capacity)?;
            ByteReservation::from_retention(bytes, credit)
        })
    }
}
/// Exclusive credit. Attach an Arc owner when backing storage is shared.
#[derive(Debug)]
pub struct ContextCredit {
    budget: ContextBudget,
    bytes: usize,
}
impl ContextCredit {
    /// Current accounted weight.
    pub fn bytes(&self) -> usize {
        self.bytes
    }
    /// Admits growth before allocation, or releases excess credit after replacement.
    pub fn resize(&mut self, bytes: usize) -> Result<()> {
        if bytes > self.bytes {
            let mut additional = self.budget.reserve(bytes - self.bytes)?;
            // Transfer the charged delta to this credit without releasing it on drop.
            additional.bytes = 0;
        } else {
            self.budget
                .0
                .used
                .fetch_sub(self.bytes - bytes, Ordering::AcqRel);
        }
        self.bytes = bytes;
        Ok(())
    }
}
impl Drop for ContextCredit {
    fn drop(&mut self) {
        self.budget.0.used.fetch_sub(self.bytes, Ordering::AcqRel);
    }
}
/// Shared Local resource authority inherited by Agent scopes.
#[derive(Debug)]
pub struct ContextBudgetContract;
impl LocalContract for ContextBudgetContract {
    const KEY: &'static str = "rsi.agent.context-budget";
    type Service = ContextBudget;
}
/// Service factory. Product composition installs it with restart-required policy.
#[derive(Debug, Default)]
pub struct ContextBudgetFactory;
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Config {
    #[serde(default = "default_capacity")]
    maximum_bytes: usize,
}
fn default_capacity() -> usize {
    DEFAULT_CONTEXT_CAPACITY
}
#[async_trait]
impl PluginFactory for ContextBudgetFactory {
    fn prepare(&self, config: &ConfigValue) -> rsi_meta::Result<PreparedActivation> {
        let parsed: Config = serde_json::from_value(config.clone())
            .map_err(|e| MetaError::InvalidInput(e.to_string()))?;
        let budget = ContextBudget::new(parsed.maximum_bytes)
            .map_err(|e| MetaError::InvalidInput(e.to_string()))?;
        Ok(PreparedActivation::with_state(
            config.clone(),
            budget,
            std::mem::size_of::<Pool>(),
        ))
    }
    async fn activate(&self, mut plan: ActivationPlan) -> rsi_meta::Result<()> {
        let budget = Arc::new(plan.take_state::<ContextBudget>()?);
        let supply = plan
            .context()
            .provide_local::<ContextBudgetContract>(budget)?;
        plan.defer(
            "withdraw Context budget",
            Box::new(move || {
                Box::pin(async move {
                    drop(supply);
                    Ok(())
                })
            }),
        )
    }
}
pub(crate) fn weight<T: Serialize + ?Sized>(value: &T) -> Result<usize> {
    rsi_api_protocol::measure_json(value, rsi_api_protocol::MAXIMUM_API_BYTES)
        .map_err(|e| ContextError::Invalid(e.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn sixteen_small_checkpoint_buffers_charge_exact_lengths_and_last_slices() {
        let budget = ContextBudget::new(16 * 1024).unwrap();
        let admissions: Vec<_> = (0..16).map(|_| budget.checkpoint_read()).collect();
        assert_eq!(budget.used(), 0);
        let buffers: Vec<_> = admissions
            .into_iter()
            .map(|admission| admission.reserve(1024).unwrap().copy(&[0; 1024]).unwrap())
            .collect();
        assert_eq!(budget.used(), 16 * 1024);
        assert!(matches!(
            budget.checkpoint_read().reserve(1),
            Err(ApiError::Capacity)
        ));
        let slice = buffers[0].slice(0..1).unwrap();
        drop(buffers);
        assert_eq!(budget.used(), 1024);
        drop(slice);
        assert_eq!(budget.used(), 0);
    }

    #[test]
    fn shared_credit_is_checked_before_growth_and_survives_last_owner() {
        let budget = ContextBudget::new(10).unwrap();
        let mut credit = budget.reserve(8).unwrap();
        assert!(matches!(credit.resize(11), Err(ContextError::Capacity)));
        assert_eq!(credit.bytes(), 8);
        assert_eq!(budget.used(), 8);
        assert!(matches!(
            budget.reserve(usize::MAX),
            Err(ContextError::Capacity)
        ));
        credit.resize(3).unwrap();
        let owner = Arc::new(credit);
        let parked = owner.clone();
        drop(owner);
        assert_eq!(budget.used(), 3);
        drop(parked);
        assert_eq!(budget.used(), 0);
        assert!(ContextBudget::new(0).is_err());
    }

    #[test]
    fn growth_overflow_preserves_credit_and_can_retry_after_other_owner_releases() {
        let budget = ContextBudget::new(usize::MAX).unwrap();
        let mut credit = budget.reserve(usize::MAX - 8).unwrap();
        let other = budget.reserve(8).unwrap();
        assert!(matches!(
            credit.resize(usize::MAX),
            Err(ContextError::Capacity)
        ));
        assert_eq!(credit.bytes(), usize::MAX - 8);
        assert_eq!(budget.used(), usize::MAX);
        drop(other);
        credit.resize(usize::MAX).unwrap();
        assert_eq!(credit.bytes(), usize::MAX);
        assert_eq!(budget.used(), usize::MAX);
        credit.resize(3).unwrap();
        assert_eq!(budget.used(), 3);
        drop(credit);
        assert_eq!(budget.used(), 0);
    }
}
