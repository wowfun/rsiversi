use super::{Arc, Mutex};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

pub(super) const MAXIMUM_RESULT_BYTES: usize = 17 * 1024 * 1024;
pub(super) const RETAINED_TOOL_BYTES: usize = 256 * 1024 * 1024;

#[derive(Debug)]
pub(super) struct ToolSettlement {
    retention: Retained,
    arguments: usize,
    result: Option<(
        rsi_tools_protocol::ToolResultIdentity,
        Box<rsi_tools_protocol::ToolResult>,
    )>,
}
impl ToolSettlement {
    pub(super) fn new(retention: Retained, arguments: usize) -> Self {
        Self {
            retention,
            arguments,
            result: None,
        }
    }
    pub(super) fn matches(
        &self,
        identity: &rsi_tools_protocol::ToolResultIdentity,
        result: &rsi_tools_protocol::ToolResult,
    ) -> bool {
        self.result
            .as_ref()
            .is_some_and(|(expected_identity, expected)| {
                expected_identity == identity && expected.as_ref() == result
            })
    }
    pub(super) fn bind(
        &mut self,
        identity: &rsi_tools_protocol::ToolResultIdentity,
        result: &rsi_tools_protocol::ToolResult,
    ) -> super::ContributionResult<()> {
        if self.result.is_some() {
            return if self.matches(identity, result) {
                Ok(())
            } else {
                Err(super::invalid("retained Tool identity or result changed"))
            };
        }
        let bytes =
            rsi_api_protocol::measure_json(result, MAXIMUM_RESULT_BYTES).map_err(super::invalid)?;
        self.result = Some((identity.clone(), Box::new(result.clone())));
        self.retention.shrink(self.arguments + bytes);
        Ok(())
    }
}

/// Encoded payload weight, independent of the operation's receipt permit.
#[derive(Debug)]
pub(super) struct Retained(OwnedSemaphorePermit);
impl Retained {
    pub(super) fn reserve(budget: &Arc<Semaphore>, arguments: usize) -> Option<Self> {
        budget
            .clone()
            .try_acquire_many_owned(u32::try_from(arguments + MAXIMUM_RESULT_BYTES).ok()?)
            .ok()
            .map(Self)
    }
    pub(super) fn shrink(&mut self, bytes: usize) {
        let excess = self.0.num_permits() - bytes;
        drop(self.0.split(excess));
    }
}

#[derive(Debug)]
pub(super) struct Ownership(Mutex<Option<OwnedSemaphorePermit>>);
impl Ownership {
    pub(super) fn reserve(budget: &Arc<Semaphore>) -> Option<Arc<Self>> {
        budget
            .clone()
            .try_acquire_owned()
            .ok()
            .map(|permit| Arc::new(Self(Mutex::new(Some(permit)))))
    }
    pub(super) fn release(&self) {
        self.0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn final_result_is_frozen_once_and_receipt_weight_tracks_that_result() {
        use rsi_tools_protocol::{ToolResult, ToolResultIdentity};
        let budget = Arc::new(Semaphore::new(RETAINED_TOOL_BYTES));
        let mut settlement = ToolSettlement::new(Retained::reserve(&budget, 2).unwrap(), 2);
        let identity = ToolResultIdentity::new("owner", "invoke", "call", "a".repeat(64)).unwrap();
        let result = ToolResult::new(serde_json::json!({}), vec![], false).unwrap();
        assert!(!settlement.matches(&identity, &result));
        settlement.bind(&identity, &result).unwrap();
        settlement.bind(&identity, &result).unwrap();
        assert_eq!(
            budget.available_permits(),
            RETAINED_TOOL_BYTES - 2 - serde_json::to_vec(&result).unwrap().len()
        );
        let changed = ToolResult::new(serde_json::json!("changed"), vec![], false).unwrap();
        assert!(settlement.bind(&identity, &changed).is_err());
        assert!(!settlement.matches(&identity, &changed));
        drop(settlement);
        assert_eq!(budget.available_permits(), RETAINED_TOOL_BYTES);
    }
    #[test]
    fn payload_budget_admission_survives_receipt_and_observer_clones() {
        let budget = Arc::new(Semaphore::new(RETAINED_TOOL_BYTES));
        let arguments = 4 * 1024 * 1024;
        let mut retained = Vec::new();
        while let Some(credit) = Retained::reserve(&budget, arguments) {
            retained.push(credit);
        }
        assert_eq!(retained.len(), 12); // 12 * (4 + 17) MiB; the next reservation fails.
        drop(retained);
        assert_eq!(budget.available_permits(), RETAINED_TOOL_BYTES);

        let count = Arc::new(Semaphore::new(1));
        let operation = Ownership::reserve(&count).unwrap();
        let mut payload = Retained::reserve(&budget, arguments).unwrap();
        payload.shrink(arguments + 100);
        let pending = Arc::new((operation.clone(), payload));
        let observer = pending.clone();
        operation.release();
        assert_eq!(count.available_permits(), 1);
        assert_eq!(
            budget.available_permits(),
            RETAINED_TOOL_BYTES - arguments - 100
        );
        drop(pending);
        assert_eq!(
            budget.available_permits(),
            RETAINED_TOOL_BYTES - arguments - 100
        );
        drop(observer);
        assert_eq!(budget.available_permits(), RETAINED_TOOL_BYTES);
    }
    #[test]
    fn unresolved_and_prepared_owners_share_one_bound_until_definite_settlement() {
        let budget = Arc::new(Semaphore::new(256));
        let mut unknown: Vec<_> = (0..256)
            .map(|_| Ownership::reserve(&budget).unwrap())
            .collect();
        assert!(Ownership::reserve(&budget).is_none());
        let prepared = unknown[0].clone();
        unknown.remove(0);
        assert!(Ownership::reserve(&budget).is_none());
        prepared.release();
        let next = Ownership::reserve(&budget).unwrap();
        prepared.release();
        assert!(Ownership::reserve(&budget).is_none());
        drop((next, unknown));
        assert_eq!(budget.available_permits(), 256);
    }
}
