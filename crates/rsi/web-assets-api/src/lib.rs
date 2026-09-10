//! Authenticated complete renderer-generation leases, independent of DOM policy.
#![forbid(unsafe_code)]
#![warn(missing_docs)]
#![allow(clippy::missing_errors_doc)]

mod client;
pub use client::{AssetObservation, AssetsClient};
#[cfg(not(target_family = "wasm"))]
mod server;
use rsi_api_protocol::{
    ApiError, OperationAccess, OperationClass, OperationEffect, OperationId, OperationSpec,
    RequestEncoding, Result,
};
use rsi_ui_protocol::{RendererCatalog, digest_valid};
use serde::{Deserialize, Serialize};
#[cfg(not(target_family = "wasm"))]
pub use server::WebAssetsApi;

/// One connection-local application ownership selection, never an authentication credential.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Observe {
    /// Fresh 128-bit lowercase hexadecimal nonce.
    pub application: String,
}
impl Observe {
    /// Checks the bounded application nonce.
    pub fn validate(&self) -> Result<()> {
        application(&self.application)
    }
}
/// Complete generation already retained by the observation owner.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Offer {
    /// Exact SHA-256 used in every renderer file URL.
    pub revision: String,
    /// Validated executable admission; absent for a static-only bundle.
    pub catalog: Option<RendererCatalog>,
}
impl Offer {
    /// Validates the closed wire metadata before any module resolution.
    pub fn validate(&self) -> Result<()> {
        if !digest_valid(&self.revision) {
            return Err(malformed());
        }
        if let Some(catalog) = &self.catalog {
            catalog.validate().map_err(|_| malformed())?;
        }
        Ok(())
    }
}
/// Settles exactly one outstanding offer after rendering or rollback has completed.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Commit {
    /// The observing application's nonce; origin comes from the trusted transport.
    pub application: String,
    /// Exact offered generation.
    pub revision: String,
    /// True only after successful DOM commit and disposal of the old renderer.
    pub accept: bool,
}
impl Commit {
    /// Checks request identities without granting ownership.
    pub fn validate(&self) -> Result<()> {
        application(&self.application)?;
        if !digest_valid(&self.revision) {
            return Err(malformed());
        }
        Ok(())
    }
}
fn application(value: &str) -> Result<()> {
    if value.len() != 32
        || !value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        return Err(malformed());
    }
    Ok(())
}
fn malformed() -> ApiError {
    ApiError::Invalid("invalid Web asset lease message".into())
}
/// Exact policies negotiated by the typed client and native server.
pub fn operations() -> [OperationSpec; 2] {
    [operation(false), operation(true)]
}
fn operation(commit: bool) -> OperationSpec {
    OperationSpec {
        id: OperationId::new("web-assets", if commit { "commit" } else { "observe" }, 1)
            .expect("constant operation"),
        access: OperationAccess::Authenticated,
        class: if commit {
            OperationClass::Control
        } else {
            OperationClass::Subscription
        },
        effect: if commit {
            OperationEffect::Mutation
        } else {
            OperationEffect::Read
        },
        encoding: RequestEncoding::Json,
        maximum_request_bytes: 1024,
        maximum_response_bytes: if commit { 16 } else { 256 * 1024 },
    }
}
