use crate::{ContractId, ContractVersion, ServiceKey};
use serde::{Deserialize, Serialize};
use std::fmt;

/// Stable diagnostic identity of the code that creates a Fiber generation.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum FactoryIdentity {
    /// Process-linked implementation identified by a name and revision.
    Builtin {
        /// Stable implementation name.
        name: String,
        /// Caller-defined implementation revision.
        revision: String,
    },
    /// Verified native artifact identified by self-reported name and host digest.
    Artifact {
        /// Plugin identity observed at the verified adapter seam.
        plugin: String,
        /// Lowercase SHA-256 of the exact mapped top-level artifact bytes.
        sha256: String,
    },
}

impl FactoryIdentity {
    /// Creates an identity for process-linked code.
    pub fn builtin(name: impl Into<String>, revision: impl Into<String>) -> Self {
        Self::Builtin {
            name: name.into(),
            revision: revision.into(),
        }
    }
}

impl fmt::Display for FactoryIdentity {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Builtin { name, revision } => write!(formatter, "{name}@{revision}"),
            Self::Artifact { plugin, sha256 } => write!(formatter, "{plugin}@sha256:{sha256}"),
        }
    }
}

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
