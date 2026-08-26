use serde::{Deserialize, Serialize};
use std::fmt;

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
    ServiceKey,
    "Logical service slot selected by a plugin requirement."
);
string_id!(ContractId, "Language-neutral service contract identity.");
string_id!(EventKey, "Language-neutral event identity.");

/// Exact service contract revision. M1 deliberately performs no range negotiation.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ContractVersion(pub u32);

/// Runtime-local plugin application identity.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(transparent)]
pub struct FiberId(pub u64);

/// Monotonic activation identity within one Fiber.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(transparent)]
pub struct FiberGeneration(pub u64);

/// Runtime-local service isolation slot.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(transparent)]
pub struct IsolationId(pub u64);

/// Runtime-local event listener identity.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(transparent)]
pub struct EventListenerId(pub u64);

/// Runtime-local service-call identity.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(transparent)]
pub struct CallId(pub u64);

/// Non-repeating identity of one generation-owned dynamic service supply.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
pub struct SupplyId {
    owner: FiberId,
    generation: FiberGeneration,
    token: u64,
}

impl SupplyId {
    pub(crate) fn new(owner: FiberId, generation: FiberGeneration, token: u64) -> Self {
        Self {
            owner,
            generation,
            token,
        }
    }

    /// Returns the exact Fiber generation that created this supply.
    pub fn owner(&self) -> (FiberId, FiberGeneration) {
        (self.owner, self.generation)
    }

    /// Returns the Runtime-local monotonic supply token.
    pub fn token(&self) -> u64 {
        self.token
    }
}
