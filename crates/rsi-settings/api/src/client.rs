use crate::wire::{Clear, Failure, List, Operation, Read, Replace};
use async_trait::async_trait;
use rsi_api_protocol::{ApiClient, ApiClientContract, ApiError, call_json};
use rsi_meta::{ActivationPlan, ConfigValue, MetaError, PluginFactory, PreparedActivation};
use rsi_settings_protocol::{
    Result, SettingsAccess, SettingsAccessContract, SettingsDescription, SettingsError,
    SettingsPage, SettingsSnapshot, SettingsVersion, validate_namespace, validate_section,
    validate_settings_page,
};
use serde::Serialize;
use serde_json::Value;
use std::sync::Arc;

/// Namespace projection client over one negotiated generic connection.
#[derive(Debug)]
pub struct SettingsClient {
    api: Arc<dyn ApiClient>,
}
impl SettingsClient {
    /// Requires every exact Settings operation before publishing projection authority.
    pub fn new(api: Arc<dyn ApiClient>) -> rsi_api_protocol::Result<Self> {
        if Operation::ALL
            .iter()
            .any(|operation| !api.operations().contains(&operation.spec()))
        {
            return Err(ApiError::Unavailable);
        }
        Ok(Self { api })
    }
    async fn call<I: Serialize + Sync>(
        &self,
        operation: Operation,
        namespace: &str,
        expected: Option<&SettingsVersion>,
        request: &I,
    ) -> Result<SettingsSnapshot> {
        let snapshot = call_json::<_, SettingsSnapshot, Failure>(
            self.api.as_ref(),
            &operation.spec(),
            request,
        )
        .await
        .map_err(SettingsError::Api)?
        .map_err(|failure| failure.into_error(namespace, expected))?;
        let malformed = || {
            SettingsError::Api(if expected.is_some() {
                ApiError::OutcomeUnknown
            } else {
                ApiError::Invalid("invalid Settings snapshot".into())
            })
        };
        validate_section(&snapshot.value).map_err(|_| malformed())?;
        if expected.is_some_and(|version| {
            snapshot.scope_id != version.scope_id
                || Some(snapshot.revision) != version.revision.checked_add(1)
        }) {
            return Err(malformed());
        }
        Ok(snapshot)
    }
}
#[async_trait]
impl SettingsAccess for SettingsClient {
    async fn list(&self, after: Option<&str>, limit: usize) -> Result<SettingsPage> {
        validate_settings_page(after, limit)?;
        let page = call_json::<_, SettingsPage, Failure>(
            self.api.as_ref(),
            &Operation::List.spec(),
            &List {
                after: after.map(str::to_owned),
                limit,
            },
        )
        .await
        .map_err(SettingsError::Api)?
        .map_err(|failure| failure.into_error("discovery", None))?;
        page.validate(after, limit).map_err(|_| {
            SettingsError::Api(ApiError::Invalid("invalid Settings discovery page".into()))
        })?;
        Ok(page)
    }
    async fn describe(&self, namespace: &str) -> Result<SettingsDescription> {
        validate_namespace(namespace)?;
        let description = call_json::<_, SettingsDescription, Failure>(
            self.api.as_ref(),
            &Operation::Describe.spec(),
            &Read {
                namespace: namespace.into(),
            },
        )
        .await
        .map_err(SettingsError::Api)?
        .map_err(|failure| failure.into_error(namespace, None))?;
        description.validate(namespace).map_err(|_| {
            SettingsError::Api(ApiError::Invalid("invalid Settings description".into()))
        })?;
        Ok(description)
    }
    async fn read(&self, namespace: &str) -> Result<SettingsSnapshot> {
        validate_namespace(namespace)?;
        self.call(
            Operation::Read,
            namespace,
            None,
            &Read {
                namespace: namespace.into(),
            },
        )
        .await
    }
    async fn replace(
        &self,
        namespace: &str,
        expected: &SettingsVersion,
        value: Value,
    ) -> Result<SettingsSnapshot> {
        validate_namespace(namespace)?;
        validate_section(&value)?;
        self.call(
            Operation::Replace,
            namespace,
            Some(expected),
            &Replace {
                namespace: namespace.into(),
                expected: expected.clone(),
                value,
            },
        )
        .await
    }
    async fn clear(&self, namespace: &str, expected: &SettingsVersion) -> Result<SettingsSnapshot> {
        validate_namespace(namespace)?;
        self.call(
            Operation::Clear,
            namespace,
            Some(expected),
            &Clear {
                namespace: namespace.into(),
                expected: expected.clone(),
            },
        )
        .await
    }
}

/// Ordinary client plugin granting namespace projection without raw-document access.
#[derive(Clone, Debug, Default)]
pub struct SettingsClientFactory;
#[async_trait]
impl PluginFactory for SettingsClientFactory {
    fn prepare(&self, config: &ConfigValue) -> rsi_meta::Result<PreparedActivation> {
        Ok(crate::prepare(config)?.requiring_local::<ApiClientContract>())
    }
    async fn activate(&self, plan: ActivationPlan) -> rsi_meta::Result<()> {
        let client = SettingsClient::new(plan.local::<ApiClientContract>()?)
            .map_err(|error| MetaError::Activation(error.to_string()))?;
        let supply = plan
            .context()
            .provide_local::<SettingsAccessContract>(Arc::new(client))?;
        plan.defer(
            "withdraw Settings client",
            Box::new(move || {
                Box::pin(async move {
                    drop(supply);
                    Ok(())
                })
            }),
        )
    }
}
