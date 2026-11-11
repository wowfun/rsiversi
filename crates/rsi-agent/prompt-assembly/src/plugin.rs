use super::{
    Arc, BTreeMap, ContributionId, ContributionResult, PromptContext, PromptInput, PromptRegistry,
    PromptRegistryContract, PromptSection, PromptText, TextDeclaration, TextProvider, async_trait,
    render,
};
use rsi_agent_composition_protocol::{AgentPromptScopeContract, PromptAssemblerContract};
use rsi_meta::{ActivationPlan, ConfigValue, MetaError, PluginFactory, PreparedActivation};
use serde::{Deserialize, Serialize};

/// Ordinary Agent-only prompt plugin. No process or Store owner is created.
#[derive(Debug, Default)]
pub struct PromptAssemblyFactory;

#[derive(Debug, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
struct Config {
    sections: Vec<SectionConfig>,
    contexts: Vec<ContextConfig>,
    variables: BTreeMap<String, String>,
    include_runtime_context: bool,
    workspace_context: bool,
    tool_order: Option<Vec<String>>,
}
impl Default for Config {
    fn default() -> Self {
        Self {
            sections: vec![],
            contexts: vec![],
            variables: BTreeMap::new(),
            include_runtime_context: true,
            workspace_context: true,
            tool_order: None,
        }
    }
}

#[derive(Debug, Deserialize, Serialize)]
struct TextConfig {
    name: ContributionId,
    order: f64,
    text: String,
    #[serde(default = "enabled")]
    interpolate: bool,
}
const fn enabled() -> bool {
    true
}
impl From<TextConfig> for TextDeclaration {
    fn from(config: TextConfig) -> Self {
        Self {
            name: config.name,
            order: config.order,
            text: PromptText::Literal(config.text.into()),
            interpolate: config.interpolate,
        }
    }
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct SectionConfig {
    #[serde(flatten)]
    declaration: TextConfig,
    #[serde(default)]
    complete: bool,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct ContextConfig {
    #[serde(flatten)]
    declaration: TextConfig,
    #[serde(default)]
    required: bool,
}

#[derive(Debug)]
struct WorkingDirectory;
impl TextProvider for WorkingDirectory {
    fn resolve(&self, input: &PromptInput<'_>) -> ContributionResult<Option<String>> {
        Ok(Some(format!(
            "Working directory: {}",
            input.header.canonical_cwd()
        )))
    }
}

fn activation(error: impl std::fmt::Display) -> MetaError {
    MetaError::Activation(error.to_string())
}

#[async_trait]
impl PluginFactory for PromptAssemblyFactory {
    fn prepare(&self, desired: &ConfigValue) -> rsi_meta::Result<PreparedActivation> {
        let config: Config = if desired.is_null() {
            Config::default()
        } else {
            serde_json::from_value(desired.clone())
                .map_err(|error| MetaError::InvalidInput(error.to_string()))?
        };
        render::validate_order(config.tool_order.as_deref())
            .map_err(|error| MetaError::InvalidInput(error.to_string()))?;
        let normalized = serde_json::to_value(&config)
            .map_err(|error| MetaError::InvalidInput(error.to_string()))?;
        let bytes = serde_json::to_vec(&normalized)
            .map_err(|error| MetaError::InvalidInput(error.to_string()))?
            .len();
        Ok(PreparedActivation::with_state(normalized, config, bytes)
            .requiring_local::<AgentPromptScopeContract>())
    }

    async fn activate(&self, mut plan: ActivationPlan) -> rsi_meta::Result<()> {
        let config = plan.take_state::<Config>()?;
        let scope = plan.local::<AgentPromptScopeContract>()?;
        let registry = Arc::new(
            PromptRegistry::new(
                scope.root.clone(),
                plan.context().runtime().limits().topology.maximum_fibers,
                config.tool_order,
            )
            .map_err(activation)?,
        );
        let owner = plan.context();
        let mut registrations = Vec::new();
        for section in config.sections {
            registrations.push(
                registry
                    .section(
                        owner,
                        PromptSection {
                            declaration: section.declaration.into(),
                            complete: section.complete,
                        },
                    )
                    .await
                    .map_err(activation)?,
            );
        }
        for context in config.contexts {
            registrations.push(
                registry
                    .context(
                        owner,
                        PromptContext {
                            declaration: context.declaration.into(),
                            required: context.required,
                        },
                    )
                    .await
                    .map_err(activation)?,
            );
        }
        for (name, text) in config.variables {
            registrations.push(
                registry
                    .variable(owner, name, PromptText::Literal(text.into()))
                    .await
                    .map_err(activation)?,
            );
        }
        if !config.include_runtime_context {
            registrations.push(
                registry
                    .suppress_runtime_context(owner)
                    .await
                    .map_err(activation)?,
            );
        }
        if config.workspace_context {
            registrations.push(
                registry
                    .context(
                        owner,
                        PromptContext {
                            declaration: TextDeclaration {
                                name: ContributionId::new("rsi.working-directory")
                                    .expect("static name"),
                                order: 100.0,
                                text: PromptText::Provider(Arc::new(WorkingDirectory)),
                                interpolate: false,
                            },
                            required: true,
                        },
                    )
                    .await
                    .map_err(activation)?,
            );
        }
        let authoring = owner.provide_local::<PromptRegistryContract>(registry.clone())?;
        let assembler = owner.provide_local::<PromptAssemblerContract>(registry)?;
        plan.defer(
            "withdraw native prompt services",
            Box::new(move || {
                Box::pin(async move {
                    drop((authoring, assembler));
                    drop(registrations);
                    Ok(())
                })
            }),
        )
    }
}
