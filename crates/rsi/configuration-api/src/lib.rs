//! Typed, bounded configuration-grant operations for native and browser clients.
#![deny(unsafe_code)]
#![warn(missing_docs)]
#![allow(clippy::missing_errors_doc)]
use rsi_api_protocol::{
    ApiClient, ApiError, DeviceId, OperationAccess, OperationClass, OperationEffect, OperationId,
    OperationSpec, RequestEncoding, Result, call_json,
};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use std::sync::Arc;

/// Closed grant operation identities and their exact admission policy.
#[derive(Clone, Copy, Debug)]
pub enum ConfigurationOperation {
    /// Current caller's effective authority.
    Status,
    /// Local durable grant snapshot.
    Grants,
    /// Local expected-revision grant or revocation.
    SetGrant,
}
impl ConfigurationOperation {
    /// Returns the exact negotiated wire contract.
    ///
    /// # Panics
    /// Panics if the static operation identities are invalid.
    pub fn spec(self) -> OperationSpec {
        let (name, access, effect) = match self {
            Self::Status => (
                "status",
                OperationAccess::Authenticated,
                OperationEffect::Read,
            ),
            Self::Grants => ("grants", OperationAccess::Local, OperationEffect::Read),
            Self::SetGrant => (
                "set-grant",
                OperationAccess::Local,
                OperationEffect::Mutation,
            ),
        };
        OperationSpec {
            id: OperationId::new("configuration", name, 1).expect("static operation"),
            access,
            class: OperationClass::Data,
            effect,
            encoding: RequestEncoding::Json,
            maximum_request_bytes: 1024,
            maximum_response_bytes: 64 * 1024,
        }
    }
}
/// Redacted Local snapshot for exact grant reconciliation.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GrantSnapshot {
    /// Canonical decimal revision of the durable grant document.
    pub revision: String,
    /// Durably granted registered `DeviceId` values in stable order.
    pub devices: Vec<DeviceId>,
}
impl GrantSnapshot {
    /// Validates externally decoded bounds and exact order before exposing a snapshot.
    pub fn validate(&self) -> Result<()> {
        revision(&self.revision)?;
        if self.devices.len() > 64 || self.devices.windows(2).any(|pair| pair[0] >= pair[1]) {
            return Err(ApiError::Invalid(
                "invalid configuration grant roster".into(),
            ));
        }
        Ok(())
    }
}
fn revision(text: &str) -> Result<u64> {
    if text.len() > 20 {
        return Err(ApiError::Invalid("invalid configuration revision".into()));
    }
    text.parse::<u64>()
        .ok()
        .filter(|value| value.to_string() == text)
        .ok_or_else(|| ApiError::Invalid("invalid configuration revision".into()))
}
#[derive(Serialize, Deserialize)]
enum Never {}
#[derive(Serialize)]
struct Empty {}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Status {
    allowed: bool,
}
#[derive(Serialize)]
struct Change<'a> {
    device: &'a DeviceId,
    expected_revision: &'a str,
    granted: bool,
}

/// Exact negotiated configuration operations, without secret resolution authority.
#[derive(Clone, Debug)]
pub struct ConfigurationClient {
    api: Arc<dyn ApiClient>,
}
impl ConfigurationClient {
    /// Requires the status operation; Local administration negotiates independently.
    pub fn new(api: Arc<dyn ApiClient>) -> Result<Self> {
        if !api
            .operations()
            .contains(&ConfigurationOperation::Status.spec())
        {
            return Err(ApiError::Unavailable);
        }
        Ok(Self { api })
    }
    async fn call<I: Serialize + Sync, O: DeserializeOwned>(
        &self,
        operation: ConfigurationOperation,
        request: &I,
    ) -> Result<O> {
        let spec = operation.spec();
        if !self.api.operations().contains(&spec) {
            return Err(ApiError::Unavailable);
        }
        match call_json::<_, _, Never>(self.api.as_ref(), &spec, request).await? {
            Ok(value) => Ok(value),
            Err(never) => match never {},
        }
    }
    /// Reads this connection's effective grant without exposing other devices.
    pub async fn allowed(&self) -> Result<bool> {
        self.call::<_, Status>(ConfigurationOperation::Status, &Empty {})
            .await
            .map(|status| status.allowed)
    }
    /// Reads and validates the durable Local grant document.
    pub async fn grants(&self) -> Result<GrantSnapshot> {
        let snapshot: GrantSnapshot = self.call(ConfigurationOperation::Grants, &Empty {}).await?;
        snapshot.validate()?;
        Ok(snapshot)
    }
    /// Changes a Local grant once against an exact observed revision.
    pub async fn set_grant(
        &self,
        device: &DeviceId,
        expected_revision: &str,
        granted: bool,
    ) -> Result<GrantSnapshot> {
        let expected = revision(expected_revision)?
            .checked_add(1)
            .ok_or_else(|| ApiError::Invalid("configuration revision exhausted".into()))?;
        let snapshot: GrantSnapshot = self
            .call(
                ConfigurationOperation::SetGrant,
                &Change {
                    device,
                    expected_revision,
                    granted,
                },
            )
            .await?;
        if snapshot.validate().is_err()
            || snapshot.revision != expected.to_string()
            || snapshot.devices.contains(device) != granted
        {
            return Err(ApiError::OutcomeUnknown);
        }
        Ok(snapshot)
    }
}

mod providers;
pub use providers::{
    ManagedProvider, ManagedProvidersClient, ProviderKind, ProvidersOperation, ProvidersSnapshot,
};
mod credentials;
pub use credentials::{CredentialOperation, CredentialReceipt, ProviderCredentialsClient};
