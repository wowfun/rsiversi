//! Runtime-independent contracts shared by Meta composition modules.

#![deny(unsafe_code)]
#![warn(missing_docs)]

use serde::{Deserialize, Serialize};
use std::fmt;

/// Nominal marker for one process-local safe-Rust service contract.
///
/// Runtime matching uses the marker's Rust [`std::any::TypeId`], not `KEY`.
/// Hosts use `KEY` only as a stable Profile/catalog name and must reject a
/// conflicting registration before plugin execution.
pub trait LocalContract: 'static {
    /// Stable Host/Profile catalog name for this contract marker.
    const KEY: &'static str;

    /// Safe-Rust object shared directly between plugins in one process.
    type Service: ?Sized + Send + Sync + 'static;
}

/// Validated JSON-compatible configuration retained for one plugin instance.
pub type ConfigValue = serde_json::Value;

macro_rules! string_id {
    ($name:ident, $doc:literal) => {
        #[doc = $doc]
        #[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
        #[serde(transparent)]
        pub struct $name(String);

        impl $name {
            /// Creates an identity from its exact string representation.
            pub fn new(value: impl Into<String>) -> Self {
                Self(value.into())
            }

            /// Returns the exact string representation.
            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl From<&str> for $name {
            fn from(value: &str) -> Self {
                Self::new(value)
            }
        }

        impl From<String> for $name {
            fn from(value: String) -> Self {
                Self::new(value)
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str(&self.0)
            }
        }

        impl AsRef<str> for $name {
            fn as_ref(&self) -> &str {
                self.as_str()
            }
        }
    };
}

string_id!(
    PluginId,
    "Stable identity of one Host-registered plugin implementation."
);
string_id!(
    InstanceId,
    "Stable identity of one Profile application of a plugin implementation."
);
string_id!(
    LocalContractKey,
    "Stable Profile key registered for one Rust Local contract type."
);
string_id!(
    LocalEventKey,
    "Stable Profile key registered for one Rust Local event type."
);

/// Immutable provenance of code selected before plugin execution.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum FactoryIdentity {
    /// Process-linked implementation selected from one Host-local catalog.
    Linked {
        /// Stable catalog key.
        plugin: PluginId,
        /// Caller-defined build or implementation revision.
        revision: String,
    },
    /// Explicit trusted native artifact selected and hashed by the Host.
    Native {
        /// Stable identity reported by validated ABI discovery.
        plugin: PluginId,
        /// Lowercase SHA-256 of the exact staged top-level artifact bytes.
        sha256: String,
    },
}

impl FactoryIdentity {
    /// Creates provenance for process-linked code.
    pub fn linked(plugin: impl Into<PluginId>, revision: impl Into<String>) -> Self {
        Self::Linked {
            plugin: plugin.into(),
            revision: revision.into(),
        }
    }

    /// Creates provenance for an explicitly resolved native artifact.
    pub fn native(plugin: impl Into<PluginId>, sha256: impl Into<String>) -> Self {
        Self::Native {
            plugin: plugin.into(),
            sha256: sha256.into(),
        }
    }
}

impl fmt::Display for FactoryIdentity {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Linked { plugin, revision } => write!(formatter, "{plugin}@{revision}"),
            Self::Native { plugin, sha256 } => write!(formatter, "{plugin}@sha256:{sha256}"),
        }
    }
}

/// Static policy for applying a changed configuration to one factory.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UpdateMode {
    /// Profile may retire and recreate the instance in the running Runtime.
    Replayable,
    /// A changed configuration is valid only after process restart.
    RestartRequired,
}
