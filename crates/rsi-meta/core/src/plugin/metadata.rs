use crate::{ContractId, ContractVersion, ServiceKey};
use serde::{Deserialize, Serialize};

/// Exact service dependency selected by one preparation attempt.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Requirement {
    /// Logical service slot.
    pub key: ServiceKey,
    /// Required contract identity.
    pub contract: ContractId,
    /// Required exact contract version.
    pub version: ContractVersion,
}

impl Requirement {
    /// Creates one exact service requirement.
    pub fn new(
        key: impl Into<ServiceKey>,
        contract: impl Into<ContractId>,
        version: ContractVersion,
    ) -> Self {
        Self {
            key: key.into(),
            contract: contract.into(),
            version,
        }
    }
}
