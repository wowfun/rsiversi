//! Shared operation identity and transport path codec.
use crate::{
    ApiError, OperationClass, OperationEffect, OperationId, OperationSpec, ProtocolDescriptor,
    Result,
};
use serde::{Deserialize, Serialize};

/// API carrier revision, shared by paths, headers and connection negotiation.
macro_rules! wire_version {
    ($version:literal) => {
        /// API carrier revision.
        pub const API_WIRE_VERSION: u16 = $version;
        /// Canonical API route prefix.
        pub const API_PATH_PREFIX: &str = concat!("/api/v", stringify!($version));
        /// Browser session bootstrap route.
        pub const BROWSER_BOOTSTRAP_PATH: &str =
            concat!("/api/v", stringify!($version), "/browser-bootstrap");
        /// Explicit device login route.
        pub const LOGIN_PATH: &str = concat!("/api/v", stringify!($version), "/login");
        /// Explicit browser logout route.
        pub const LOGOUT_PATH: &str = concat!("/api/v", stringify!($version), "/logout");
    };
}
wire_version!(2);

/// Complete identity of an independently owned operation.
#[derive(Clone, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(deny_unknown_fields)]
pub struct OperationKey {
    /// Owning wire protocol.
    pub protocol: ProtocolDescriptor,
    /// Domain operation identity.
    pub id: OperationId,
}
impl OperationKey {
    /// Validates a decoded key at its owning transport or configuration boundary.
    pub fn validate(&self) -> Result<()> {
        self.protocol.validate()?;
        OperationId::new(self.id.domain(), self.id.name(), self.id.version()).map(|_| ())
    }
}

/// Minimal call semantics required to classify cancellation and uncertain outcomes.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CallShape {
    /// Finite cancellable read.
    Read,
    /// Finite mutation retained by its execution owner.
    Mutation,
    /// Subscription with explicit termination.
    Stream,
}

/// Identity and shape supplied by a caller, without execution policy or authority.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct OperationCall {
    /// Complete operation identity.
    pub key: OperationKey,
    /// Expected finite or streaming behavior.
    pub shape: CallShape,
}
impl OperationSpec {
    /// Returns identity without copying independent execution policy.
    pub fn key(&self) -> OperationKey {
        OperationKey {
            protocol: self.protocol.clone(),
            id: self.id.clone(),
        }
    }
    /// Returns the shared request descriptor derived from local call semantics.
    pub fn call(&self) -> OperationCall {
        OperationCall {
            key: self.key(),
            shape: self.shape(),
        }
    }
    /// Returns call semantics without allocating an operation identity.
    pub fn shape(&self) -> CallShape {
        if self.class == OperationClass::Subscription {
            CallShape::Stream
        } else if self.effect == OperationEffect::Mutation {
            CallShape::Mutation
        } else {
            CallShape::Read
        }
    }
}
impl OperationCall {
    /// Encodes the canonical path shared by all URL transports.
    pub fn to_path(&self) -> String {
        let shape = match self.shape {
            CallShape::Read => "read",
            CallShape::Mutation => "mutation",
            CallShape::Stream => "stream",
        };
        format!(
            "{API_PATH_PREFIX}/{}/{}/{}/{}/{}/{shape}",
            self.key.protocol.name,
            self.key.protocol.version,
            self.key.id.domain(),
            self.key.id.name(),
            self.key.id.version()
        )
    }
    /// Decodes canonical paths only, including bounded, validated identities.
    pub fn from_path(path: &str) -> Result<Self> {
        let invalid = || ApiError::Invalid("invalid operation path".into());
        if path.len() > 300 {
            return Err(invalid());
        }
        let suffix = path
            .strip_prefix(API_PATH_PREFIX)
            .and_then(|value| value.strip_prefix('/'))
            .ok_or_else(invalid)?;
        let mut parts = suffix.split('/');
        let protocol = ProtocolDescriptor {
            name: parts.next().ok_or_else(invalid)?.into(),
            version: parts
                .next()
                .ok_or_else(invalid)?
                .parse()
                .map_err(|_| invalid())?,
        };
        protocol.validate()?;
        let domain = parts.next().ok_or_else(invalid)?;
        let name = parts.next().ok_or_else(invalid)?;
        let version = parts
            .next()
            .ok_or_else(invalid)?
            .parse()
            .map_err(|_| invalid())?;
        let shape = match parts.next() {
            Some("read") => CallShape::Read,
            Some("mutation") => CallShape::Mutation,
            Some("stream") => CallShape::Stream,
            _ => return Err(invalid()),
        };
        if parts.next().is_some() {
            return Err(invalid());
        }
        let call = Self {
            key: OperationKey {
                protocol,
                id: OperationId::new(domain, name, version)?,
            },
            shape,
        };
        if call.to_path() != path {
            return Err(invalid());
        }
        Ok(call)
    }
}
