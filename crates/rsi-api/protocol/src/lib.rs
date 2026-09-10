//! Runtime-independent API identities and retained buffer accounting.

#![deny(unsafe_code)]
#![warn(missing_docs)]
#![allow(clippy::missing_errors_doc)]

mod authentication;
mod budget;
mod client;
mod identity;
mod json;
pub mod portable;
mod service;
mod stream;
pub use authentication::*;
pub use budget::{
    ByteAccumulator, ByteBudget, ByteReceiver, ByteReservation, RetainedBytes, measure_json,
};
pub use client::*;
pub use identity::{DeviceId, EndpointId, HostEpoch, LocalCompatibilityKey};
pub use json::{call_json, json_handler};
pub use service::*;
pub use stream::supervised_stream;

use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Maximum encoded bytes retained by one ordinary API budget.
pub const MAXIMUM_API_BYTES: usize = 64 * 1024 * 1024;

/// API foundation failure, independent of domain error taxonomies.
#[derive(Clone, Debug, Eq, PartialEq, Error)]
pub enum ApiError {
    /// Mutation execution may have happened, but its outcome cannot be established.
    #[error("API operation outcome is unknown; reconcile using its domain identity")]
    OutcomeUnknown,
    /// The presented device credential is absent, invalid or revoked.
    #[error("API authentication required")]
    Unauthorized,
    /// Internal implementation failure with a bounded transport diagnostic.
    #[error("API backend failed: {0}")]
    Backend(String),
    /// Domain-owned error JSON, kept under the response byte lease.
    #[error("domain operation failed")]
    Domain(RetainedBytes),
    /// Malformed or permanently out-of-bounds input.
    #[error("invalid API input: {0}")]
    Invalid(String),
    /// A bounded resource is temporarily full.
    #[error("API capacity is exhausted")]
    Capacity,
    /// The operation or service generation has retired.
    #[error("API service is shutting down")]
    ShuttingDown,
    /// Exact operation identity or version is unavailable.
    #[error("API operation is unavailable")]
    Unavailable,
}

/// API foundation result.
pub type Result<T> = std::result::Result<T, ApiError>;

/// Exact domain-owned operation identity, independent of a transport address.
#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Hash, Serialize)]
#[serde(deny_unknown_fields)]
pub struct OperationId {
    domain: String,
    name: String,
    version: u16,
}

impl OperationId {
    /// Validates two bounded path-safe names and a positive domain version.
    pub fn new(domain: impl Into<String>, name: impl Into<String>, version: u16) -> Result<Self> {
        let domain = domain.into();
        let name = name.into();
        for value in [&domain, &name] {
            if value.is_empty()
                || value.len() > 64
                || !value.as_bytes()[0].is_ascii_lowercase()
                || !value.bytes().all(|byte| {
                    byte.is_ascii_lowercase()
                        || byte.is_ascii_digit()
                        || matches!(byte, b'.' | b'_' | b'-')
                })
            {
                return Err(ApiError::Invalid(
                    "operation names must be 1..=64 path-safe lowercase ASCII bytes".into(),
                ));
            }
        }
        if version == 0 {
            return Err(ApiError::Invalid(
                "operation version must be positive".into(),
            ));
        }
        Ok(Self {
            domain,
            name,
            version,
        })
    }
    /// Borrows the owning domain name.
    pub fn domain(&self) -> &str {
        &self.domain
    }
    /// Borrows the operation name within its domain.
    pub fn name(&self) -> &str {
        &self.name
    }
    /// Returns the exact domain contract version.
    pub const fn version(&self) -> u16 {
        self.version
    }
}

impl<'de> Deserialize<'de> for OperationId {
    fn deserialize<D: serde::Deserializer<'de>>(
        deserializer: D,
    ) -> std::result::Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Wire {
            domain: String,
            name: String,
            version: u16,
        }
        let wire = Wire::deserialize(deserializer)?;
        Self::new(wire.domain, wire.name, wire.version).map_err(serde::de::Error::custom)
    }
}

/// Independent server admission class selected by a registered operation.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OperationClass {
    /// Small lifecycle or interaction controls with reserved capacity.
    Control,
    /// Finite domain reads, uploads or mutations.
    Data,
    /// Long-lived reconnectable observation.
    Subscription,
}

/// Work ownership selected by a registered operation, never by its caller.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OperationEffect {
    /// Work is released when its caller or stream goes away.
    Read,
    /// Admitted work survives cancellation of its response waiter.
    Mutation,
}
