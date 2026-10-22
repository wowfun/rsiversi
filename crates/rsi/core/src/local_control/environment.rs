use rsi_api_protocol::HostEpoch;
use rsi_credentials_protocol::SecretValue;
use sha2::{Digest as _, Sha256};
use std::{collections::BTreeMap, ffi::OsString};

/// Kept only in process memory, never written into owner metadata or diagnostics.
pub(super) struct EnvironmentFingerprint([u8; 32]);
impl std::fmt::Debug for EnvironmentFingerprint {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("EnvironmentFingerprint(<redacted>)")
    }
}
impl EnvironmentFingerprint {
    pub(super) fn capture(credentials: &BTreeMap<String, SecretValue>) -> Self {
        Self::from_inputs(
            credentials,
            crate::scrub_child_environment(std::env::vars_os()),
        )
    }
    fn from_inputs(
        credentials: &BTreeMap<String, SecretValue>,
        tools: Vec<(OsString, OsString)>,
    ) -> Self {
        let mut hash = Sha256::new();
        hash.update(b"rsi.host.environment/v1\0");
        let mut component = |bytes: &[u8]| {
            hash.update((bytes.len() as u64).to_le_bytes());
            hash.update(bytes);
        };
        component(b"credentials");
        for (name, value) in credentials {
            component(name.as_bytes());
            component(value.expose_secret().as_bytes());
        }
        component(b"tools");
        let tools: BTreeMap<_, _> = tools.into_iter().collect();
        for (name, value) in tools {
            component(name.as_encoded_bytes());
            component(value.as_encoded_bytes());
        }
        Self(hash.finalize().into())
    }
    pub(super) fn for_epoch(&self, epoch: &HostEpoch) -> String {
        let mut hash = Sha256::new();
        hash.update(epoch.as_str().as_bytes());
        hash.update(self.0);
        format!("{:x}", hash.finalize())
    }
}
