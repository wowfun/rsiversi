use async_trait::async_trait;
use rsi_api_protocol::{
    ApiError, AuthenticatedDevice, DeviceAdministration, DeviceAuthentication, DeviceId,
    DeviceRecord, EndpointId, RegisteredDevice, Result,
};
use rsi_credentials_protocol::SecretValue;
use rsi_meta::Execution;
use rsi_storage_domain::Domain;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::{Arc, Mutex},
};
use tokio::sync::Mutex as AsyncMutex;
use tokio_util::sync::CancellationToken;
use zeroize::Zeroizing;

const RECORD_KEY: &str = "deployment";
const MAX_DEVICES: usize = 64;

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct StoredDevice {
    record: DeviceRecord,
    token_hash: String,
    #[serde(default)]
    credential_retired: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    managed: Option<String>,
}
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Durable {
    endpoint: EndpointId,
    devices: Vec<StoredDevice>,
}

#[derive(Debug)]
struct State {
    durable: Durable,
    leases: BTreeMap<DeviceId, CancellationToken>,
    retired: bool,
}
#[derive(Debug)]
struct Inner {
    domain: Arc<dyn Domain>,
    state: Mutex<State>,
    commit: Arc<AsyncMutex<()>>,
}

/// One exclusive deployment authentication generation over an explicit durable domain.
#[derive(Clone, Debug)]
pub struct DeviceRegistry {
    execution: Execution,
    inner: Arc<Inner>,
}

impl DeviceRegistry {
    /// Loads a bounded domain whose exclusive writer is protected by the deployment owner.
    pub async fn open(
        execution: Execution,
        domain: Arc<dyn Domain>,
        endpoint: EndpointId,
    ) -> Result<Self> {
        let spec = domain.spec();
        if spec.version != 1 || spec.maximum_records != 1 || spec.maximum_bytes > 64 * 1024 {
            return Err(ApiError::Invalid(
                "device domain requires schema 1, one record and at most 64 KiB".into(),
            ));
        }
        let mut snapshot = domain.snapshot().await;
        if snapshot.len() > 1 || snapshot.keys().any(|key| key != RECORD_KEY) {
            return Err(ApiError::Invalid(
                "device domain contains unexpected records".into(),
            ));
        }
        let durable = if let Some(value) = snapshot.remove(RECORD_KEY) {
            let durable: Durable = serde_json::from_value(value)
                .map_err(|_| ApiError::Invalid("device domain record is malformed".into()))?;
            validate_durable(&durable, &endpoint)?;
            durable
        } else {
            let durable = Durable {
                endpoint,
                devices: Vec::new(),
            };
            publish(&domain, &durable).await?;
            durable
        };
        let leases = durable
            .devices
            .iter()
            .map(|device| (device.record.id.clone(), CancellationToken::new()))
            .collect();
        Ok(Self {
            execution,
            inner: Arc::new(Inner {
                domain,
                state: Mutex::new(State {
                    durable,
                    leases,
                    retired: false,
                }),
                commit: Arc::new(AsyncMutex::new(())),
            }),
        })
    }

    /// Fences access and escaped read leases before awaiting owned publication.
    pub async fn close(&self) {
        let leases = {
            let mut state = self
                .inner
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            state.retired = true;
            state.leases.values().cloned().collect::<Vec<_>>()
        };
        for lease in leases {
            lease.cancel();
        }
        let _commit = self.inner.commit.lock().await;
    }
}

impl DeviceAuthentication for DeviceRegistry {
    fn authenticate(&self, token: &SecretValue) -> Result<AuthenticatedDevice> {
        let state = self
            .inner
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if state.retired {
            return Err(ApiError::ShuttingDown);
        }
        let hash = token_hash(&state.durable.endpoint, token)?;
        let device = state
            .durable
            .devices
            .iter()
            .find(|device| !device.credential_retired && device.token_hash == hash)
            .ok_or(ApiError::Unauthorized)?;
        Ok(AuthenticatedDevice {
            id: device.record.id.clone(),
            revoked: state.leases[&device.record.id].clone(),
        })
    }
}

#[async_trait]
impl DeviceAdministration for DeviceRegistry {
    async fn register(&self, label: &str) -> Result<RegisteredDevice> {
        self.issue(None, label, Rotation::Any).await
    }

    async fn rotate_managed(&self, slot: &str, label: &str) -> Result<RegisteredDevice> {
        validate_slot(slot)?;
        self.issue(Some(slot), label, Rotation::Any).await
    }

    fn managed_device(&self, slot: &str) -> Result<Option<DeviceRecord>> {
        validate_slot(slot)?;
        let state = self
            .inner
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if state.retired {
            return Err(ApiError::ShuttingDown);
        }
        Ok(state
            .durable
            .devices
            .iter()
            .find(|device| device.managed.as_deref() == Some(slot))
            .map(|device| device.record.clone()))
    }
    async fn rotate_managed_if(
        &self,
        slot: &str,
        label: &str,
        expected: Option<&DeviceId>,
    ) -> Result<RegisteredDevice> {
        validate_slot(slot)?;
        self.issue(Some(slot), label, Rotation::Matching(expected.cloned()))
            .await
    }

    async fn revoke(&self, id: &DeviceId) -> Result<bool> {
        self.revoke_matching(id, None).await
    }
    async fn revoke_credential(&self, device: &RegisteredDevice) -> Result<bool> {
        self.revoke_matching(&device.record.id, Some(&device.token))
            .await
    }

    fn list(&self) -> Result<Vec<DeviceRecord>> {
        let state = self
            .inner
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if state.retired {
            return Err(ApiError::ShuttingDown);
        }
        Ok(state
            .durable
            .devices
            .iter()
            .map(|device| device.record.clone())
            .collect())
    }
}

enum Rotation {
    Any,
    Matching(Option<DeviceId>),
}

impl DeviceRegistry {
    async fn revoke_matching(&self, id: &DeviceId, token: Option<&SecretValue>) -> Result<bool> {
        let commit = self.inner.commit.clone().lock_owned().await;
        let mut durable = {
            let state = self
                .inner
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if state.retired {
                return Err(ApiError::ShuttingDown);
            }
            state.durable.clone()
        };
        let expected = token
            .map(|token| token_hash(&durable.endpoint, token))
            .transpose()?;
        let Some(index) = durable.devices.iter().position(|device| {
            &device.record.id == id
                && expected
                    .as_ref()
                    .is_none_or(|hash| !device.credential_retired && hash == &device.token_hash)
        }) else {
            return Ok(false);
        };
        if expected.is_some() && durable.devices[index].managed.is_some() {
            durable.devices[index].credential_retired = true;
        } else {
            durable.devices.remove(index);
        }
        let id = id.clone();
        let inner = self.inner.clone();
        self.execution
            .spawn(async move {
                let _commit = commit;
                publish(&inner.domain, &durable).await?;
                let lease = {
                    let mut state = inner
                        .state
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner);
                    state.durable = durable;
                    state.leases.remove(&id)
                };
                if let Some(lease) = lease {
                    lease.cancel();
                }
                Ok(true)
            })
            .await
            .map_err(|_| ApiError::Backend("device revocation task failed".into()))?
    }

    async fn issue(
        &self,
        managed: Option<&str>,
        label: &str,
        rotation: Rotation,
    ) -> Result<RegisteredDevice> {
        DeviceRecord::validate_label(label)?;
        let commit = self.inner.commit.clone().lock_owned().await;
        let mut durable = {
            let state = self
                .inner
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if state.retired {
                return Err(ApiError::ShuttingDown);
            }
            if state.durable.devices.len() == MAX_DEVICES
                && !state
                    .durable
                    .devices
                    .iter()
                    .any(|device| managed.is_some() && device.managed.as_deref() == managed)
            {
                return Err(ApiError::Capacity);
            }
            state.durable.clone()
        };
        if let Rotation::Matching(expected) = rotation {
            let actual = durable
                .devices
                .iter()
                .find(|device| managed.is_some() && device.managed.as_deref() == managed)
                .map(|device| &device.record.id);
            if actual != expected.as_ref() {
                return Err(ApiError::Invalid(
                    "managed device slot changed before credential rotation".into(),
                ));
            }
        }
        let mut entropy = Zeroizing::new([0; 32]);
        getrandom::fill(entropy.as_mut())
            .map_err(|_| ApiError::Backend("OS entropy failed".into()))?;
        let token = SecretValue::new(hex::encode(entropy.as_ref()))
            .map_err(|_| ApiError::Backend("token construction failed".into()))?;
        let existing = durable
            .devices
            .iter()
            .position(|device| managed.is_some() && device.managed.as_deref() == managed);
        let record = DeviceRecord {
            id: match existing {
                Some(index) => durable.devices[index].record.id.clone(),
                None => DeviceId::generate()?,
            },
            label: label.into(),
        };
        let stored = StoredDevice {
            record: record.clone(),
            token_hash: token_hash(&durable.endpoint, &token)?,
            credential_retired: false,
            managed: managed.map(str::to_owned),
        };
        match existing {
            Some(index) => durable.devices[index] = stored,
            None => durable.devices.push(stored),
        }
        // Entropy is not a proof of uniqueness; never publish a duplicate identity.
        validate_durable(&durable, &durable.endpoint)?;
        let inner = self.inner.clone();
        self.execution
            .spawn(async move {
                let _commit = commit;
                publish(&inner.domain, &durable).await?;
                let mut state = inner
                    .state
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                state.durable = durable;
                let lease = CancellationToken::new();
                if state.retired {
                    // No observer has received this fresh token yet.
                    lease.cancel();
                }
                if let Some(previous) = state.leases.insert(record.id.clone(), lease) {
                    previous.cancel();
                }
                Ok(RegisteredDevice { record, token })
            })
            .await
            .map_err(|_| ApiError::Backend("device registration task failed".into()))?
    }
}

fn validate_slot(slot: &str) -> Result<()> {
    if slot.is_empty()
        || slot.len() > 64
        || !slot
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
    {
        return Err(ApiError::Invalid("invalid managed device slot".into()));
    }
    Ok(())
}

fn token_hash(endpoint: &EndpointId, token: &SecretValue) -> Result<String> {
    let secret = token.expose_secret();
    if secret.len() != 64
        || !secret
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err(ApiError::Unauthorized);
    }
    let mut hash = Sha256::new();
    hash.update(b"rsi.api.device-token.v1\0");
    hash.update(endpoint.as_str());
    hash.update(secret);
    Ok(hex::encode(hash.finalize()))
}

fn validate_durable(durable: &Durable, endpoint: &EndpointId) -> Result<()> {
    if &durable.endpoint != endpoint || durable.devices.len() > MAX_DEVICES {
        return Err(ApiError::Invalid(
            "device domain identity or capacity is invalid".into(),
        ));
    }
    let mut ids = BTreeSet::new();
    let mut hashes = BTreeSet::new();
    let mut slots = BTreeSet::new();
    for device in &durable.devices {
        DeviceRecord::validate_label(&device.record.label)?;
        if let Some(slot) = &device.managed {
            validate_slot(slot)?;
            if !slots.insert(slot) {
                return Err(ApiError::Invalid("duplicate managed device slot".into()));
            }
        }
        if (device.credential_retired && device.managed.is_none())
            || !ids.insert(&device.record.id)
            || !hashes.insert(&device.token_hash)
            || device.token_hash.len() != 64
            || !device
                .token_hash
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        {
            return Err(ApiError::Invalid(
                "device domain has invalid or duplicate identities".into(),
            ));
        }
    }
    Ok(())
}

async fn publish(domain: &Arc<dyn Domain>, durable: &Durable) -> Result<()> {
    let value = serde_json::to_value(durable)
        .map_err(|_| ApiError::Backend("device encoding failed".into()))?;
    domain
        .put(RECORD_KEY, value)
        .await
        .map_err(|_| ApiError::Backend("device publication failed".into()))
}
