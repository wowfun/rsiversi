use serde_json::Value;
use std::sync::{Arc, OnceLock};

use super::{ContextScope, IsolationId, ServiceKey, ServiceSlot};

impl ContextScope {
    pub(super) fn service_slot(&self, key: &ServiceKey) -> ServiceSlot {
        ServiceSlot {
            key: key.clone(),
            isolation: self.isolation.get(key).copied().unwrap_or(IsolationId(0)),
        }
    }
}

#[derive(Clone, Debug)]
pub(crate) struct InterceptLayers {
    pub(super) values: Vec<Value>,
    pub(super) encoded_bytes: usize,
}

impl InterceptLayers {
    pub(crate) fn empty() -> Self {
        Self {
            values: Vec::new(),
            encoded_bytes: 2,
        }
    }

    pub(crate) fn shared_empty() -> Arc<Self> {
        static EMPTY: OnceLock<Arc<InterceptLayers>> = OnceLock::new();
        Arc::clone(EMPTY.get_or_init(|| Arc::new(Self::empty())))
    }

    pub(crate) fn as_slice(&self) -> &[Value] {
        &self.values
    }
}
