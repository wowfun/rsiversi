use crate::{ContractId, ContractVersion, MetaError, Result, ServiceKey};
use serde::{Deserialize, Serialize};
use std::fmt;

/// Stable identity of the code that creates a Fiber generation.
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
        /// Plugin identity observed in the verified descriptor.
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

/// Exact service dependency declared before a Fiber is inserted.
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

/// Exact service capability declared before a Fiber is inserted.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Provision {
    /// Logical service slot.
    pub key: ServiceKey,
    /// Published contract identity.
    pub contract: ContractId,
    /// Published exact contract version.
    pub version: ContractVersion,
}

impl Provision {
    /// Creates one exact service provision.
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

/// Immutable dependency and capability declaration for a plugin factory.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct PluginDescriptor {
    /// Stable identity of the factory implementation.
    pub identity: FactoryIdentity,
    /// Exact dependencies resolved atomically for each activation.
    #[serde(default)]
    pub requires: Vec<Requirement>,
    /// Capabilities the activation is allowed to publish.
    #[serde(default)]
    pub provides: Vec<Provision>,
}

impl PluginDescriptor {
    /// Creates an empty descriptor for the given factory identity.
    pub fn new(identity: FactoryIdentity) -> Self {
        Self {
            identity,
            requires: Vec::new(),
            provides: Vec::new(),
        }
    }

    #[must_use]
    /// Appends one declared requirement.
    pub fn requiring(mut self, requirement: Requirement) -> Self {
        self.requires.push(requirement);
        self
    }

    #[must_use]
    /// Appends one declared provision.
    pub fn providing(mut self, provision: Provision) -> Self {
        self.provides.push(provision);
        self
    }

    pub(crate) fn validate(&self) -> Result<()> {
        let mut requirements = std::collections::BTreeSet::new();
        for requirement in &self.requires {
            if !requirements.insert(requirement.key.clone()) {
                return Err(MetaError::InvalidInput(format!(
                    "factory {} declares requirement {} more than once",
                    self.identity, requirement.key
                )));
            }
        }
        let mut provisions = std::collections::BTreeSet::new();
        for provision in &self.provides {
            if !provisions.insert(provision.key.clone()) {
                return Err(MetaError::InvalidInput(format!(
                    "factory {} declares provision {} more than once",
                    self.identity, provision.key
                )));
            }
        }
        Ok(())
    }
}
