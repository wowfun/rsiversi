use crate::{ApiError, DeviceId, EndpointId, HostEpoch, Result};
use async_trait::async_trait;
use rsi_credentials_protocol::SecretValue;
use rsi_meta_contract::LocalContract;
use serde::{Deserialize, Serialize};
use std::fmt;
use tokio_util::sync::CancellationToken;

/// Non-secret device information available to the local operator.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeviceRecord {
    /// Exact registered device identity.
    pub id: DeviceId,
    /// Operator-selected display name, at most 128 UTF-8 bytes without control characters.
    pub label: String,
}
impl DeviceRecord {
    /// Validates an external or durable label at the registration boundary.
    pub fn validate_label(label: &str) -> Result<()> {
        if label.trim().is_empty() || label.len() > 128 || label.chars().any(char::is_control) {
            return Err(ApiError::Invalid(
                "device label must be 1..=128 bytes without control characters".into(),
            ));
        }
        Ok(())
    }
}

/// One freshly generated bearer credential, returned only to its registering operator.
#[derive(Debug)]
pub struct RegisteredDevice {
    /// Safe display and revocation identity.
    pub record: DeviceRecord,
    /// Zeroizing credential; never serialized or included in formatting output.
    pub token: SecretValue,
}

/// Authentication result issued by the credential owner, never decoded from a request.
#[derive(Clone, Debug)]
pub struct AuthenticatedDevice {
    /// Identity used for admission and domain draft ownership.
    pub id: DeviceId,
    /// Cancels read/stream access after revocation or authentication-provider retirement.
    pub revoked: CancellationToken,
}

/// Read-only device authentication, independent of credential administration.
pub trait DeviceAuthentication: fmt::Debug + Send + Sync + 'static {
    /// Verifies one bounded token and returns its revocable authenticated identity.
    fn authenticate(&self, token: &SecretValue) -> Result<AuthenticatedDevice>;
}

/// Local operator authority; it is not implicitly exported as a remote domain API.
#[async_trait]
pub trait DeviceAdministration: fmt::Debug + Send + Sync + 'static {
    /// Durably registers a device and returns its new secret once.
    async fn register(&self, label: &str) -> Result<RegisteredDevice>;
    /// Revokes a registered device after durable publication; repeated revocation is false.
    async fn revoke(&self, id: &DeviceId) -> Result<bool>;
    /// Returns at most 64 non-secret device records.
    fn list(&self) -> Result<Vec<DeviceRecord>>;
}

/// Stable endpoint identity supplied by the deployment owner under its exclusive lease.
#[derive(Debug)]
pub struct EndpointIdentityContract;
impl LocalContract for EndpointIdentityContract {
    const KEY: &'static str = "rsi.api.endpoint.identity";
    type Service = EndpointId;
}

/// Running generation identity supplied by the same exclusive deployment owner.
#[derive(Debug)]
pub struct HostGenerationContract;
impl LocalContract for HostGenerationContract {
    const KEY: &'static str = "rsi.api.host.generation";
    type Service = HostEpoch;
}

/// Nominal Local contract for authentication without administrative authority.
#[derive(Debug)]
pub struct DeviceAuthenticationContract;
impl LocalContract for DeviceAuthenticationContract {
    const KEY: &'static str = "rsi.api.device.authentication";
    type Service = dyn DeviceAuthentication;
}
/// Nominal Local contract for local operator administration.
#[derive(Debug)]
pub struct DeviceAdministrationContract;
impl LocalContract for DeviceAdministrationContract {
    const KEY: &'static str = "rsi.api.device.administration";
    type Service = dyn DeviceAdministration;
}
