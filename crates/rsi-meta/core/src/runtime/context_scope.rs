use super::{
    ContextScope, IsolationId, LocalIsolationId, LocalSlot, ServiceKey, ServiceSlot, TypeId,
};

impl ContextScope {
    pub(super) fn service_slot(&self, key: &ServiceKey) -> ServiceSlot {
        ServiceSlot {
            key: key.clone(),
            isolation: self.isolation.get(key).copied().unwrap_or(IsolationId(0)),
        }
    }

    pub(super) fn local_slot(&self, contract: TypeId) -> LocalSlot {
        LocalSlot {
            contract,
            isolation: self
                .local_isolation
                .get(&contract)
                .copied()
                .unwrap_or(LocalIsolationId(0)),
        }
    }
}
