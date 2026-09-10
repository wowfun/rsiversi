//! Local operator device operations over the shared authenticated registry.
#![deny(unsafe_code)]
#![warn(missing_docs)]
#![allow(clippy::missing_errors_doc)]

use async_trait::async_trait;
use rsi_api_protocol::{
    ApiClient, ApiError, ApiRegistrar, ApiRegistrarContract, ApiRegistration, DeviceAdministration,
    DeviceAdministrationContract, DeviceId, DeviceRecord, OperationAccess, OperationClass,
    OperationEffect, OperationId, OperationSpec, RegisteredDevice, RequestEncoding, Result,
    call_json, json_handler,
};
use rsi_credentials_protocol::SecretValue;
use rsi_meta::{ActivationPlan, ConfigValue, MetaError, PluginFactory, PreparedActivation};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use std::{collections::BTreeSet, sync::Arc};

fn operation(name: &str) -> OperationSpec {
    OperationSpec {
        id: OperationId::new("devices", name, 1).expect("constant device operation"),
        access: OperationAccess::Local,
        class: OperationClass::Control,
        effect: if name == "list" {
            OperationEffect::Read
        } else {
            OperationEffect::Mutation
        },
        encoding: RequestEncoding::Json,
        maximum_request_bytes: 1024,
        maximum_response_bytes: 32 * 1024,
    }
}

#[derive(Deserialize, Serialize)]
enum Never {}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Empty {}
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Register {
    label: String,
}
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Revoke {
    id: DeviceId,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Issued {
    record: DeviceRecord,
    #[serde(with = "token")]
    token: SecretValue,
}
mod token {
    use rsi_credentials_protocol::SecretValue;
    use serde::{Deserialize, Deserializer, Serializer, de::Error};
    pub fn serialize<S: Serializer>(value: &SecretValue, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(value.expose_secret())
    }
    pub fn deserialize<'de, D: Deserializer<'de>>(
        deserializer: D,
    ) -> Result<SecretValue, D::Error> {
        let value = SecretValue::new(String::deserialize(deserializer)?)
            .map_err(|_| D::Error::custom("invalid device credential"))?;
        let bytes = value.expose_secret().as_bytes();
        if bytes.len() != 64
            || !bytes
                .iter()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(byte))
        {
            return Err(D::Error::custom("invalid device credential"));
        }
        Ok(value)
    }
}

/// Owns the three registrations independently of listeners and their clients.
#[derive(Debug)]
pub struct DeviceApi(Vec<ApiRegistration>);
impl DeviceApi {
    /// Registers local-only operations over the exact live administration owner.
    pub fn register(
        registrar: &dyn ApiRegistrar,
        devices: Arc<dyn DeviceAdministration>,
    ) -> Result<Self> {
        let registration_owner = devices.clone();
        let register = registrar.register(
            operation("register"),
            json_handler(move |_, request: Register| {
                let devices = registration_owner.clone();
                async move {
                    DeviceRecord::validate_label(&request.label)?;
                    let result = devices.register(&request.label).await?;
                    Ok::<_, ApiError>(Ok::<_, Never>(Issued {
                        record: result.record,
                        token: result.token,
                    }))
                }
            }),
        )?;
        let list_owner = devices.clone();
        let list = registrar.register(
            operation("list"),
            json_handler(move |_, _: Empty| {
                let devices = list_owner.clone();
                async move { Ok::<_, ApiError>(Ok::<_, Never>(devices.list()?)) }
            }),
        )?;
        let revoke = registrar.register(
            operation("revoke"),
            json_handler(move |_, request: Revoke| {
                let devices = devices.clone();
                async move { Ok::<_, ApiError>(Ok::<_, Never>(devices.revoke(&request.id).await?)) }
            }),
        )?;
        Ok(Self(vec![register, list, revoke]))
    }
    /// Fences each operation and drains its already admitted mutations.
    pub async fn close(self) {
        for registration in self.0 {
            registration.close().await;
        }
    }
}

/// Stateless typed projection over a negotiated local connection.
#[derive(Debug)]
pub struct DeviceClient {
    api: Arc<dyn ApiClient>,
}
impl DeviceClient {
    /// Requires all exact local-only operation descriptors before use.
    pub fn new(api: Arc<dyn ApiClient>) -> Result<Self> {
        if !["register", "list", "revoke"]
            .iter()
            .all(|name| api.operations().contains(&operation(name)))
        {
            return Err(ApiError::Unavailable);
        }
        Ok(Self { api })
    }
    /// Returns the endpoint identity to bind a new credential to its deployment.
    pub fn endpoint_id(&self) -> &rsi_api_protocol::EndpointId {
        &self.api.description().endpoint_id
    }
    async fn call<I: Serialize + Sync, O: DeserializeOwned>(
        &self,
        name: &str,
        request: &I,
    ) -> Result<O> {
        match call_json::<_, _, Never>(self.api.as_ref(), &operation(name), request).await? {
            Ok(value) => Ok(value),
            Err(never) => match never {},
        }
    }
    /// Registers once; an unknown reply must be reconciled by explicit list/revoke.
    pub async fn register(&self, label: &str) -> Result<RegisteredDevice> {
        DeviceRecord::validate_label(label)?;
        let issued: Issued = self
            .call(
                "register",
                &Register {
                    label: label.into(),
                },
            )
            .await?;
        if issued.record.label != label {
            return Err(ApiError::OutcomeUnknown);
        }
        Ok(RegisteredDevice {
            record: issued.record,
            token: issued.token,
        })
    }
    /// Reads and validates the bounded non-secret device roster.
    pub async fn list(&self) -> Result<Vec<DeviceRecord>> {
        let records: Vec<DeviceRecord> = self.call("list", &Empty {}).await?;
        let mut identities = BTreeSet::new();
        if records.len() > 64
            || records.iter().any(|record| {
                DeviceRecord::validate_label(&record.label).is_err()
                    || !identities.insert(&record.id)
            })
        {
            return Err(ApiError::Invalid("invalid device roster".into()));
        }
        Ok(records)
    }
    /// Revokes once; false means that exact identity was already absent.
    pub async fn revoke(&self, id: &DeviceId) -> Result<bool> {
        self.call("revoke", &Revoke { id: id.clone() }).await
    }
}

/// Ordinary endpoint plugin consuming independent local operator authority.
#[derive(Clone, Debug, Default)]
pub struct DeviceApiFactory;
#[async_trait]
impl PluginFactory for DeviceApiFactory {
    fn prepare(&self, config: &ConfigValue) -> rsi_meta::Result<PreparedActivation> {
        if !config.is_null() && !config.as_object().is_some_and(serde_json::Map::is_empty) {
            return Err(MetaError::InvalidInput(
                "Device API configuration must be null or empty".into(),
            ));
        }
        Ok(PreparedActivation::new(ConfigValue::Null)
            .requiring_local::<ApiRegistrarContract>()
            .requiring_local::<DeviceAdministrationContract>())
    }
    async fn activate(&self, plan: ActivationPlan) -> rsi_meta::Result<()> {
        let api = DeviceApi::register(
            plan.local::<ApiRegistrarContract>()?.as_ref(),
            plan.local::<DeviceAdministrationContract>()?,
        )
        .map_err(|error| MetaError::Activation(error.to_string()))?;
        plan.defer(
            "retire Device API",
            Box::new(move || {
                Box::pin(async move {
                    api.close().await;
                    Ok(())
                })
            }),
        )
    }
}
