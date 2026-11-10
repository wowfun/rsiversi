use crate::AgentAdapter;
use async_trait::async_trait;
use rsi_agent_composition_protocol::{
    ContributionKind, ContributionRegistrarContract, ContributionRegistration,
    DomainRegistrarContract,
};
use rsi_agent_session_protocol::ContributionId;
use rsi_agent_store_protocol::SessionStoreContract;
use rsi_extension::ExtensionContract;
use rsi_meta::{ActivationPlan, ConfigValue, MetaError, PluginFactory, PreparedActivation};
use rsi_tools_protocol::{ToolDefinition, ToolRegistrarContract, ToolRegistration, ToolRunPolicy};
use serde::Deserialize;
use tokio_util::sync::CancellationToken;

fn meta(error: impl std::fmt::Display) -> MetaError {
    MetaError::Activation(error.to_string())
}
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Config {
    artifact_sha256: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Catalog {
    protocol: u32,
    tools: Vec<CatalogTool>,
    commands: Vec<rsi_agent_session_protocol::SessionCommandDescriptor>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CatalogTool {
    definition: ToolDefinition,
    interaction: Interaction,
}
impl CatalogTool {
    fn registration(self, adapter: &std::sync::Arc<AgentAdapter>) -> ToolRegistration {
        let (definition, run_policy) = match self.interaction {
            Interaction::Execution { timeout_ms } => (
                self.definition,
                ToolRunPolicy::execution(std::time::Duration::from_millis(timeout_ms)),
            ),
            Interaction::Human => (
                self.definition
                    .with_scheduling(rsi_tools_protocol::ToolScheduling::ExclusiveFinal),
                ToolRunPolicy::human_interaction(),
            ),
        };
        let executor = adapter.tool(definition.name().into());
        ToolRegistration {
            definition,
            output: None,
            executor,
            run_policy,
        }
    }
}
#[derive(Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum Interaction {
    Execution { timeout_ms: u64 },
    Human,
}

async fn catalog(connection: &rsi_extension::Connection) -> rsi_meta::Result<Catalog> {
    let reply = tokio::time::timeout(
        std::time::Duration::from_secs(10),
        connection.request(
            "agent.catalog",
            serde_json::Value::Null,
            CancellationToken::new(),
        ),
    )
    .await
    .map_err(meta)?
    .map_err(meta)?;
    let catalog: Catalog = serde_json::from_value(reply).map_err(meta)?;
    if catalog.protocol != 5
        || catalog.tools.is_empty()
        || catalog.tools.len() > 64
        || catalog.commands.len() > 64
    {
        return Err(meta("invalid extension catalog"));
    }
    Ok(catalog)
}

/// Ordinary Agent registration over one explicitly selected process generation.
#[derive(Debug, Default)]
pub struct AgentAdapterFactory;
#[async_trait]
impl PluginFactory for AgentAdapterFactory {
    fn prepare(&self, config: &ConfigValue) -> rsi_meta::Result<PreparedActivation> {
        let effective = config.clone();
        let config: Config = serde_json::from_value(config.clone()).map_err(meta)?;
        crate::definition(config.artifact_sha256.clone()).map_err(meta)?;
        Ok(PreparedActivation::with_state(effective, config, 0)
            .requiring_local::<ExtensionContract>()
            .requiring_local::<SessionStoreContract>()
            .requiring_local::<rsi_agent_turn_protocol::TurnExecutionContract>()
            .requiring_local::<rsi_user_questions_protocol::UserQuestionsContract>()
            .requiring_local::<DomainRegistrarContract>()
            .requiring_local::<ContributionRegistrarContract>()
            .requiring_local::<ToolRegistrarContract>())
    }
    async fn activate(&self, mut plan: ActivationPlan) -> rsi_meta::Result<()> {
        let config: Config = plan.take_state()?;
        let connection = plan.local::<ExtensionContract>()?;
        let catalog = catalog(&connection).await?;
        let registration = plan.context().registration_context()?;
        let (handle, domain) = crate::definition(config.artifact_sha256.clone())
            .map_err(meta)?
            .register(
                plan.local::<DomainRegistrarContract>()?.as_ref(),
                &registration,
            )
            .map_err(meta)?;
        let adapter = AgentAdapter::new(
            connection,
            plan.local::<SessionStoreContract>()?,
            plan.local::<rsi_agent_turn_protocol::TurnExecutionContract>()?,
            handle,
            config.artifact_sha256,
        )
        .map_err(meta)?;
        adapter
            .enable_questions(plan.local::<rsi_user_questions_protocol::UserQuestionsContract>()?)
            .map_err(meta)?;
        let tools = plan
            .local::<ToolRegistrarContract>()?
            .register_batch(
                catalog
                    .tools
                    .into_iter()
                    .map(|tool| tool.registration(&adapter))
                    .collect(),
            )
            .map_err(meta)?;
        let registrar = plan.local::<ContributionRegistrarContract>()?;
        let mut contributions = Vec::new();
        for descriptor in catalog.commands {
            let callback = adapter.command(descriptor.id().clone(), descriptor.name().into());
            contributions.push(
                registrar
                    .register(
                        &registration,
                        ContributionRegistration::new(
                            descriptor.id().clone(),
                            20,
                            ContributionKind::Command(
                                rsi_agent_composition_protocol::SessionCommandRegistration::new(
                                    descriptor, callback,
                                ),
                            ),
                        ),
                    )
                    .map_err(meta)?,
            );
        }
        for (id, kind) in session_contributions(adapter) {
            contributions.push(
                registrar
                    .register(
                        &registration,
                        ContributionRegistration::new(
                            ContributionId::new(id).map_err(meta)?,
                            20,
                            kind,
                        ),
                    )
                    .map_err(meta)?,
            );
        }
        plan.defer(
            "withdraw extension Agent registrations",
            Box::new(move || {
                Box::pin(async move {
                    tools.retire().map_err(|error| error.to_string())?;
                    drop(contributions);
                    drop(domain);
                    Ok(())
                })
            }),
        )
    }
}

fn session_contributions(
    adapter: std::sync::Arc<AgentAdapter>,
) -> [(&'static str, ContributionKind); 5] {
    [
        (
            "rsi.extension.before-step",
            ContributionKind::BeforeStep(adapter.clone()),
        ),
        (
            "rsi.extension.settlement",
            ContributionKind::ToolSettlement(adapter.clone()),
        ),
        (
            "rsi.extension.view",
            ContributionKind::Projection(adapter.clone()),
        ),
        (
            "rsi.extension.fork",
            ContributionKind::ForkTransform(adapter.clone()),
        ),
        (
            "rsi.extension.journal",
            ContributionKind::ResourceRead(adapter),
        ),
    ]
}
