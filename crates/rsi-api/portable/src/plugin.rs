use crate::{PortableApiClient, export::Export};
use async_trait::async_trait;
use rsi_api_protocol::{ApiClientContract, OperationId, portable};
use rsi_meta::{
    ActivationPlan, Capability, ConfigValue, ContractVersion, LocalContract, MetaError,
    PluginFactory, PreparedActivation, Requirement,
};
use serde::Deserialize;
use std::{collections::BTreeSet, sync::Arc};

/// Explicitly transferable API capability; the supplying product owns target narrowing.
#[derive(Debug)]
pub struct ApiExportContract;
impl LocalContract for ApiExportContract {
    const KEY: &'static str = "rsi.api.portable.export";
    type Service = Capability;
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ImportConfig {
    service: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ExportConfig {
    service: String,
    operations: Vec<OperationId>,
    #[serde(default)]
    publish_local: bool,
}
fn service(value: &str) -> rsi_meta::Result<()> {
    if value.is_empty()
        || value.len() > 128
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"._-".contains(&byte))
    {
        return Err(invalid());
    }
    Ok(())
}
fn invalid() -> MetaError {
    MetaError::InvalidInput("invalid Portable API configuration".into())
}
fn activation(error: impl std::fmt::Display) -> MetaError {
    MetaError::Activation(error.to_string())
}
fn watch_retirement(
    plan: &ActivationPlan,
    retire: impl FnOnce() + Send + 'static,
) -> rsi_meta::Result<(tokio_util::sync::DropGuard, rsi_meta::Task<()>)> {
    let retiring = plan.context().retirement_observer()?;
    let stop = tokio_util::sync::CancellationToken::new();
    let guard = stop.clone().drop_guard();
    let task = plan.context().runtime().execution().spawn(async move {
        tokio::select! {
            () = retiring.cancelled() => retire(),
            () = stop.cancelled() => {}
        }
    });
    Ok((guard, task))
}
/// Imports one explicitly injected Portable capability into the Local API client contract.
#[derive(Clone, Debug, Default)]
pub struct PortableApiClientFactory;
#[async_trait]
impl PluginFactory for PortableApiClientFactory {
    fn prepare(&self, config: &ConfigValue) -> rsi_meta::Result<PreparedActivation> {
        let value: ImportConfig = serde_json::from_value(config.clone()).map_err(|_| invalid())?;
        service(&value.service)?;
        let requirement = Requirement::new(
            value.service.clone(),
            portable::CONTRACT,
            ContractVersion(portable::VERSION),
        );
        Ok(PreparedActivation::with_state(config.clone(), value, 256).requiring(requirement))
    }
    async fn activate(&self, mut plan: ActivationPlan) -> rsi_meta::Result<()> {
        let config = plan.take_state::<ImportConfig>()?;
        let capability = plan.inject(&config.service).ok_or_else(invalid)?.clone();
        let client = Arc::new(
            PortableApiClient::connect(plan.context().runtime().execution().clone(), capability)
                .await
                .map_err(activation)?,
        );
        let supply = plan
            .context()
            .provide_local::<ApiClientContract>(client.clone())?;
        let weak = Arc::downgrade(&client);
        let (watcher_guard, watcher) = watch_retirement(&plan, move || {
            if let Some(client) = weak.upgrade() {
                client.retire();
            }
        })?;
        plan.defer(
            "close Portable API client",
            Box::new(move || {
                Box::pin(async move {
                    drop(supply);
                    drop(watcher_guard);
                    client.close().await;
                    watcher.await.map_err(|error| error.to_string())?;
                    Ok(())
                })
            }),
        )
    }
}
/// Exports an explicit operation subset of the injected Local API client.
#[derive(Clone, Debug, Default)]
pub struct PortableApiExportFactory;
#[async_trait]
impl PluginFactory for PortableApiExportFactory {
    fn prepare(&self, config: &ConfigValue) -> rsi_meta::Result<PreparedActivation> {
        let value: ExportConfig = serde_json::from_value(config.clone()).map_err(|_| invalid())?;
        service(&value.service)?;
        if value.operations.len() > rsi_api_protocol::MAXIMUM_OPERATIONS - 2
            || value.operations.iter().collect::<BTreeSet<_>>().len() != value.operations.len()
        {
            return Err(invalid());
        }
        let bytes = serde_json::to_vec(config).map_err(|_| invalid())?.len();
        Ok(PreparedActivation::with_state(config.clone(), value, bytes)
            .requiring_local::<ApiClientContract>())
    }
    async fn activate(&self, mut plan: ActivationPlan) -> rsi_meta::Result<()> {
        let config = plan.take_state::<ExportConfig>()?;
        let capability = export_api(
            &plan,
            &config.service,
            plan.local::<ApiClientContract>()?,
            &config.operations,
        )?;
        if config.publish_local {
            let local = plan
                .context()
                .provide_local::<ApiExportContract>(Arc::new(capability))?;
            plan.defer(
                "withdraw Portable API grant",
                Box::new(move || {
                    Box::pin(async move {
                        drop(local);
                        Ok(())
                    })
                }),
            )?;
        }
        Ok(())
    }
}

/// Publishes explicitly selected API authority within the activating plugin generation.
///
/// The returned capability and the plugin's other injected capabilities have the
/// same holder, allowing deliberate Message transfer. The product must narrow
/// semantic target authority before supplying `api`; wire callers cannot do so.
pub fn export_api(
    plan: &ActivationPlan,
    key: &str,
    api: Arc<dyn rsi_api_protocol::ApiClient>,
    operations: &[rsi_api_protocol::OperationId],
) -> rsi_meta::Result<Capability> {
    service(key)?;
    if operations.len() > rsi_api_protocol::MAXIMUM_OPERATIONS - 2
        || operations.iter().collect::<BTreeSet<_>>().len() != operations.len()
    {
        return Err(invalid());
    }
    let export = Arc::new(
        Export::new(
            api,
            operations,
            plan.context().runtime().execution().clone(),
        )
        .map_err(activation)?,
    );
    let (supply, capability) = plan.context().provide_and_capture(
        key,
        portable::CONTRACT,
        ContractVersion(portable::VERSION),
        export.clone(),
    )?;
    let weak = Arc::downgrade(&export);
    let (watcher_guard, watcher) = watch_retirement(plan, move || {
        if let Some(export) = weak.upgrade() {
            export.retire();
        }
    })?;
    plan.defer(
        "close Portable API export",
        Box::new(move || {
            Box::pin(async move {
                drop(watcher_guard);
                export.close().await;
                watcher.await.map_err(|error| error.to_string())?;
                drop(supply);
                Ok(())
            })
        }),
    )?;
    Ok(capability)
}
