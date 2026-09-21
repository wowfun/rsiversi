use super::{CancellationToken, PathBuf, ProductHistorySearch, Request, invalid};
use async_trait::async_trait;
use rsi_api_protocol::{ApiRegistrarContract, ApiRegistration, json_handler};
use rsi_meta::{
    ActivationPlan, ConfigValue, LocalContract, MetaError, PluginFactory, PreparedActivation,
};
use serde::{Deserialize, Serialize};
/// Local source-authorized history owner; no cache connection escapes it.
#[derive(Debug)]
pub struct HistoryContract;
impl LocalContract for HistoryContract {
    const KEY: &'static str = "rsi.history";
    type Service = ProductHistorySearch;
}
/// Ordinary cache plugin with an explicit dedicated directory.
#[derive(Clone, Debug, Default)]
pub struct HistoryFactory;
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Config {
    directory: PathBuf,
}
fn meta(e: impl std::fmt::Display) -> MetaError {
    MetaError::Activation(e.to_string())
}
#[async_trait]
impl PluginFactory for HistoryFactory {
    fn prepare(&self, config: &ConfigValue) -> rsi_meta::Result<PreparedActivation> {
        let c: Config = serde_json::from_value(config.clone()).map_err(meta)?;
        if !c.directory.is_absolute() {
            return Err(meta("history directory must be absolute"));
        }
        Ok(PreparedActivation::new(config.clone())
            .requiring_local::<rsi_agent_store_protocol::SessionStoreContract>()
            .requiring_local::<rsi_session_protocol::SessionContract>()
            .requiring_local::<rsi_agent_references::ReferencesContract>()
            .requiring_local::<rsi_acp_protocol::service::ExternalConversationsContract>()
            .requiring_local::<rsi_workspace_protocol::WorkspaceRegistryContract>())
    }
    async fn activate(&self, plan: ActivationPlan) -> rsi_meta::Result<()> {
        let config: Config =
            serde_json::from_value(plan.config().as_ref().clone()).map_err(meta)?;
        let owner = ProductHistorySearch::open(
            config.directory,
            plan.local::<rsi_agent_store_protocol::SessionStoreContract>()?,
            plan.local::<rsi_session_protocol::SessionContract>()?,
            plan.local::<rsi_acp_protocol::service::ExternalConversationsContract>()?,
            plan.local::<rsi_workspace_protocol::WorkspaceRegistryContract>()?,
            plan.local::<rsi_agent_references::ReferencesContract>()?,
            plan.context().runtime().execution().clone(),
        )
        .await
        .map_err(meta)?;
        let supply = plan
            .context()
            .provide_local::<HistoryContract>(owner.clone())?;
        plan.defer(
            "drain history cache",
            Box::new(move || {
                Box::pin(async move {
                    drop(supply);
                    owner.close().await;
                    Ok(())
                })
            }),
        )
    }
}
/// API adapter; authority and cache ownership remain with the source owner.
#[derive(Clone, Debug, Default)]
pub struct HistoryApiFactory;
#[derive(Serialize)]
enum Never {}
#[async_trait]
impl PluginFactory for HistoryApiFactory {
    fn prepare(&self, config: &ConfigValue) -> rsi_meta::Result<PreparedActivation> {
        if !config.is_null() {
            return Err(meta("history API config must be null"));
        }
        Ok(PreparedActivation::new(config.clone())
            .requiring_local::<HistoryContract>()
            .requiring_local::<ApiRegistrarContract>())
    }
    async fn activate(&self, plan: ActivationPlan) -> rsi_meta::Result<()> {
        let owner = plan.local::<HistoryContract>()?;
        let registrar = plan.local::<ApiRegistrarContract>()?;
        let mut registrations = Vec::new();
        for spec in rsi_history_api::operations() {
            let owner = owner.clone();
            let expected = spec.clone();
            registrations.push(
                registrar
                    .register(
                        spec,
                        json_handler(move |_context, request: Request| {
                            let owner = owner.clone();
                            let expected = expected.clone();
                            async move {
                                if request.spec() != expected {
                                    return Err(invalid("history operation mismatch"));
                                }
                                owner
                                    .call(request, CancellationToken::new())
                                    .await
                                    .map(Ok::<_, Never>)
                            }
                        }),
                    )
                    .map_err(meta)?,
            );
        }
        plan.defer(
            "withdraw history API",
            Box::new(move || {
                Box::pin(async move {
                    futures_util::future::join_all(
                        registrations.into_iter().map(ApiRegistration::close),
                    )
                    .await;
                    Ok(())
                })
            }),
        )
    }
}
