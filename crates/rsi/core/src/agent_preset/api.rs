//! Read the same catalog and compiler used to create native Session generations.
use async_trait::async_trait;
use rsi_agent_composition::AgentCompositionSourceContract;
use rsi_agent_presets::{AgentPresetHealth, PresetError};
use rsi_api_protocol::{ApiError, ApiRegistrarContract, json_handler};
use rsi_configuration_access::ConfigurationAccessContract;
use rsi_configuration_api::agent_presets::{
    AgentPresetDocument, AgentPresetDocumentError, AgentPresetDocumentRequest, AgentPresetFormat,
    AgentPresetList, AgentPresetRow, AgentPresetsOperation,
};
use rsi_meta::{ActivationPlan, ConfigValue, MetaError, PluginFactory, PreparedActivation};
use serde::{Deserialize, Serialize};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Empty {}
#[derive(Serialize)]
enum Never {}

fn meta(error: impl std::fmt::Display) -> MetaError {
    MetaError::Activation(error.to_string())
}

#[derive(Debug, Default)]
pub(crate) struct AgentPresetsApiFactory;

#[async_trait]
impl PluginFactory for AgentPresetsApiFactory {
    fn prepare(&self, config: &ConfigValue) -> rsi_meta::Result<PreparedActivation> {
        if !config.is_null() {
            return Err(meta("Agent preset API configuration must be null"));
        }
        Ok(PreparedActivation::new(ConfigValue::Null)
            .requiring_local::<ApiRegistrarContract>()
            .requiring_local::<ConfigurationAccessContract>()
            .requiring_local::<AgentCompositionSourceContract>())
    }

    async fn activate(&self, plan: ActivationPlan) -> rsi_meta::Result<()> {
        let registrar = plan.local::<ApiRegistrarContract>()?;
        let grant = plan.local::<ConfigurationAccessContract>()?;
        let source = plan.local::<AgentCompositionSourceContract>()?;
        let authority = grant.clone();
        let catalog = source.clone();
        let list = registrar
            .register(
                AgentPresetsOperation::List.spec(),
                json_handler(move |context, _: Empty| {
                    let grant = authority.clone();
                    let source = catalog.clone();
                    async move {
                        grant
                            .retained(&context.origin, async move {
                                let snapshot =
                                    source.snapshot().map_err(|_| ApiError::Unavailable)?;
                                // Catalog discovery compiles local files. The async portion
                                // only loads its existing Settings-backed default.
                                let roster = tokio::task::spawn_blocking(move || {
                                    tokio::runtime::Handle::current()
                                        .block_on(snapshot.presets().roster())
                                })
                                .await
                                .map_err(|_| ApiError::Unavailable)?
                                .map_err(|error| match error {
                                    PresetError::RosterCapacity { .. } => ApiError::Capacity,
                                    _ => ApiError::Unavailable,
                                })?;
                                Ok(Ok::<_, Never>(AgentPresetList {
                                    presets: roster
                                        .presets
                                        .into_iter()
                                        .map(|row| AgentPresetRow {
                                            id: row.id,
                                            is_default: row.is_default,
                                            name: row.name,
                                            description: row.description,
                                            broken: match row.health {
                                                AgentPresetHealth::Healthy => None,
                                                AgentPresetHealth::Broken { reason } => {
                                                    Some(reason)
                                                }
                                            },
                                        })
                                        .collect(),
                                }))
                            })?
                            .await
                    }
                }),
            )
            .map_err(meta)?;
        let document = registrar
            .register(
                AgentPresetsOperation::Document.spec(),
                json_handler(move |context, request: AgentPresetDocumentRequest| {
                    let grant = grant.clone();
                    let source = source.clone();
                    async move {
                        grant
                            .retained(&context.origin, async move {
                                let snapshot =
                                    source.snapshot().map_err(|_| ApiError::Unavailable)?;
                                let result = tokio::task::spawn_blocking(move || {
                                    snapshot.presets().document(&request.id)
                                })
                                .await
                                .map_err(|_| ApiError::Unavailable)?;
                                match result {
                                    Ok(document) => Ok(Ok(AgentPresetDocument {
                                        id: document.id,
                                        format: AgentPresetFormat::Toml,
                                        content: document.content,
                                        name: document.name,
                                        description: document.description,
                                    })),
                                    Err(PresetError::PresetNotFound { .. }) => {
                                        Ok(Err(AgentPresetDocumentError::NotFound))
                                    }
                                    Err(PresetError::BrokenPreset { reason, .. }) => {
                                        Ok(Err(AgentPresetDocumentError::Broken { reason }))
                                    }
                                    Err(_) => Err(ApiError::Unavailable),
                                }
                            })?
                            .await
                    }
                }),
            )
            .map_err(meta)?;
        plan.defer(
            "close Agent preset API",
            Box::new(move || {
                Box::pin(async move {
                    list.close().await;
                    document.close().await;
                    Ok(())
                })
            }),
        )
    }
}
