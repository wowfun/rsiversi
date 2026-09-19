//! Shared selection and bounded explicit reads; adapters supply caller provenance.

use super::*;
use rsi_agent_composition_protocol::{
    ContributionError, ContributionResult, SessionResourceReader,
};
use rsi_agent_session_protocol::{SessionResourceDescriptor, SessionResourceValue};
use rsi_agent_turn_protocol::AgentCallerAuthority;
use rsi_tools_protocol::{
    ToolContent, ToolDefinition, ToolError, ToolExecution, ToolExecutor, ToolRegistrarContract,
    ToolRegistration, ToolResult, ToolScheduling, ToolTimeoutPolicy,
};
use serde_json::{Value, json};

/// In-process caller classification; never accepted from a resource wire request.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SkillAudience {
    /// Explicit human discovery or preview.
    Human,
    /// Model Tool read authenticated by `AgentCallerAuthority`.
    Model,
}

pub(super) fn project_boundary(
    cwd: &Path,
    observation: &mut Observation,
) -> (Option<PathBuf>, bool) {
    let root = find_project_root(cwd, observation);
    let git_root = root.is_some();
    let root = root.or_else(|| Some(cwd.to_path_buf()));
    (root, git_root)
}

pub(super) fn discover_selected(
    config: &WorkspaceContextConfig,
    cwd: &Path,
    root: Option<&Path>,
    observation: &mut Observation,
    budget: &mut SnapshotBudget,
) -> Result<Vec<SelectedSkill>, WorkspaceContextError> {
    let mut discovery = SkillDiscovery::default();
    if let Some(root) = root {
        for directory in directories_between(root, cwd)?.into_iter().rev() {
            discovery.scan(
                &directory.join(".agents/skills"),
                Some(root),
                observation,
                budget,
            )?;
        }
    }
    for path in &config.user_skill_roots {
        discovery.scan(path, None, observation, budget)?;
    }
    Ok(discovery.into_selected())
}

fn descriptor(skill: &SelectedSkill) -> SessionResourceDescriptor {
    SessionResourceDescriptor {
        id: skill.name.clone(),
        name: skill.name.clone(),
        description: utf8_prefix(&skill.description, 4096).to_owned(),
        source: skill.source.clone(),
        media_type: "text/markdown".into(),
        model_readable: skill.model_invocable,
    }
}

pub(super) fn read_skills(
    config: &WorkspaceContextConfig,
    cwd: &Path,
    id: Option<&str>,
    audience: SkillAudience,
    mut budget: SnapshotBudget,
) -> Result<SessionResourceValue, WorkspaceContextError> {
    let mut observation = Observation::default();
    let (root, _) = project_boundary(cwd, &mut observation);
    let selected = discover_selected(config, cwd, root.as_deref(), &mut observation, &mut budget)?;
    if let Some(diagnostic) = observation.diagnostic {
        return Err(WorkspaceContextError::Failed(format!(
            "skill catalog could not be read completely; {diagnostic}; refresh to retry"
        )));
    }
    let eligible = |skill: &&SelectedSkill| match audience {
        SkillAudience::Human => skill.user_invocable,
        SkillAudience::Model => skill.model_invocable,
    };
    let value = if let Some(id) = id {
        let skill = selected
            .iter()
            .find(|skill| skill.name == id)
            .ok_or_else(|| {
                WorkspaceContextError::Invalid("skill was not found in the current catalog".into())
            })?;
        if !eligible(&skill) {
            let message = match audience {
                SkillAudience::Human => "skill does not allow user invocation",
                SkillAudience::Model => "skill does not allow model invocation",
            };
            return Err(WorkspaceContextError::Invalid(message.into()));
        }
        budget.reserve(
            MAXIMUM_WORKSPACE_CONTEXT_RENDERED_BYTES + MAXIMUM_WORKSPACE_CONTEXT_SOURCE_BYTES,
        )?;
        let invocation = read_skill_invocation(skill, &mut observation, &budget.cancellation);
        let invocation = invocation
            .filter(|_| observation.is_complete())
            .ok_or_else(|| {
                WorkspaceContextError::Failed(format!(
                    "skill changed or could not be read; {}; refresh to retry",
                    observation
                        .diagnostic
                        .as_deref()
                        .unwrap_or("invalid optional skill content"),
                ))
            })?;
        SessionResourceValue::Read {
            resource: descriptor(skill),
            text: invocation.text,
        }
    } else {
        let mut entries = Vec::new();
        for skill in selected.iter().filter(eligible) {
            budget.reserve(
                skill.name.len() * 2 + skill.description.len() + skill.source.len() + 512,
            )?;
            entries.push(descriptor(skill));
        }
        SessionResourceValue::List { entries }
    };
    budget.check()?;
    value
        .validate()
        .map_err(|error| WorkspaceContextError::Invalid(error.to_string()))?;
    Ok(value)
}

#[derive(Debug)]
pub(super) struct SkillResources(pub Arc<dyn WorkspaceContext>);
#[async_trait]
impl SessionResourceReader for SkillResources {
    async fn read(
        &self,
        header: &SessionHeader,
        id: Option<&str>,
        cancellation: CancellationToken,
    ) -> ContributionResult<SessionResourceValue> {
        self.0
            .skills(header, id, SkillAudience::Human, cancellation)
            .await
            .map_err(|error| match error {
                WorkspaceContextError::Closed => ContributionError::Closed,
                WorkspaceContextError::Capacity => ContributionError::Capacity,
                _ => ContributionError::Invalid(error.to_string()),
            })
    }
}

/// Ordinary Agent-only Tool adapter over the selected workspace source.
#[derive(Clone, Copy, Debug, Default)]
pub struct WorkspaceSkillToolsFactory;
#[async_trait]
impl PluginFactory for WorkspaceSkillToolsFactory {
    fn prepare(&self, desired: &ConfigValue) -> rsi_meta::Result<PreparedActivation> {
        if !desired.is_null() {
            return Err(MetaError::InvalidInput(
                "skill Tool configuration must be null".into(),
            ));
        }
        Ok(PreparedActivation::new(ConfigValue::Null)
            .requiring_local::<WorkspaceContextContract>()
            .requiring_local::<ToolRegistrarContract>())
    }
    async fn activate(&self, plan: ActivationPlan) -> rsi_meta::Result<()> {
        let definition = ToolDefinition::new("skill_read", "Read the current body of one exact model-invocable skill from the available skills catalog. Use name, never a filesystem path. User-only skills are unavailable.", json!({"type":"object","properties":{"name":{"type":"string","minLength":1,"maxLength":64}},"required":["name"],"additionalProperties":false})).map_err(meta)?.with_scheduling(ToolScheduling::ParallelSafe);
        let lease = plan
            .local::<ToolRegistrarContract>()?
            .register(ToolRegistration {
                definition,
                timeout: ToolTimeoutPolicy::Execution { timeout_ms: 30_000 },
                executor: Arc::new(SkillTool(plan.local::<WorkspaceContextContract>()?)),
            })
            .map_err(meta)?;
        plan.defer(
            "retire skill Tool",
            Box::new(move || {
                Box::pin(async move { lease.retire().map_err(|error| error.to_string()) })
            }),
        )
    }
}
fn meta(error: impl fmt::Display) -> MetaError {
    MetaError::Activation(error.to_string())
}
#[derive(Debug)]
struct SkillTool(Arc<dyn WorkspaceContext>);
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Arguments {
    name: String,
}
#[async_trait]
impl ToolExecutor for SkillTool {
    async fn execute(
        &self,
        arguments: Value,
        execution: ToolExecution,
    ) -> rsi_tools_protocol::Result<ToolResult> {
        let arguments: Arguments = serde_json::from_value(arguments)
            .map_err(|error| ToolError::InvalidInput(error.to_string()))?;
        let authority = execution
            .extension::<AgentCallerAuthority>()
            .ok_or_else(|| ToolError::InvalidInput("skill_read requires an Agent caller".into()))?;
        let value = self
            .0
            .skills(
                authority.header(),
                Some(&arguments.name),
                SkillAudience::Model,
                execution.cancellation.clone(),
            )
            .await;
        match value {
            Ok(SessionResourceValue::Read { resource, text }) => Ok(ToolResult {
                content: vec![ToolContent::Text { text }],
                value: json!({"version":1,"resource":resource}),
                is_error: false,
                enforcement: Vec::new(),
            }),
            Ok(_) => Err(ToolError::InvalidInput(
                "skill source returned a catalog for a body read".into(),
            )),
            Err(WorkspaceContextError::Closed) => Err(ToolError::Cancelled),
            Err(error) => Ok(ToolResult {
                content: vec![ToolContent::Text {
                    text: error.to_string(),
                }],
                value: json!({"version":1,"error":"skill_unavailable"}),
                is_error: true,
                enforcement: Vec::new(),
            }),
        }
    }
}
