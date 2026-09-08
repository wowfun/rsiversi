//! Plan mode as ordinary generation-owned state, commands, context, policy and views.

#![deny(unsafe_code)]
#![warn(missing_docs)]

use async_trait::async_trait;
use rsi_agent_composition_protocol::{
    ContextContributor, ContributionContext, ContributionError, ContributionInput,
    ContributionKind, ContributionOutput, ContributionRegistrarContract, ContributionRegistration,
    ContributionResult, DomainDefinition, DomainHandle, DomainRegistrarContract, SessionCommand,
    SessionCommandContext, SessionCommandRegistration, SessionProjection, SessionProjectionContext,
    ToolPolicy, ToolPolicyDecision, ToolPolicyRequest, ValidatedDomainProposal,
};
use rsi_agent_session_protocol::{
    CommandArguments, ContributionId, DomainIdentity, DomainStateView, ProjectionValue,
    SessionCommandDescriptor,
};
use rsi_meta::{ActivationPlan, ConfigValue, MetaError, PluginFactory, PreparedActivation};
use serde::{Deserialize, Serialize};
use std::{collections::BTreeSet, sync::Arc};
use tokio_util::sync::CancellationToken;

const DOMAIN: &str = "rsi.plan-policy";

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Config {
    allow_tools: Vec<String>,
}
impl Default for Config {
    fn default() -> Self {
        Self {
            allow_tools: vec!["ask_user".into(), "output_read".into()],
        }
    }
}
impl Config {
    fn parse(value: &ConfigValue) -> rsi_meta::Result<Self> {
        let mut config: Self = if value.is_null() {
            Self::default()
        } else {
            serde_json::from_value(value.clone())
                .map_err(|error| MetaError::InvalidInput(error.to_string()))?
        };
        if config.allow_tools.len() > 64 {
            return Err(MetaError::InvalidInput(
                "plan allowlist exceeds 64 Tool names".into(),
            ));
        }
        let mut names = BTreeSet::new();
        for name in &config.allow_tools {
            rsi_tools_protocol::ToolCall::validate_fields("plan-config", name, &ConfigValue::Null)
                .map_err(|error| MetaError::InvalidInput(error.to_string()))?;
            if !names.insert(name) {
                return Err(MetaError::InvalidInput(
                    "duplicate plan allowlist Tool".into(),
                ));
            }
        }
        config.allow_tools.sort();
        Ok(config)
    }
}

/// Agent-only factory requiring ordinary domain and contribution registrars.
#[derive(Clone, Copy, Debug, Default)]
pub struct PlanPolicyFactory;

#[derive(Debug)]
struct PlanPolicy {
    state: DomainHandle<bool>,
    allowed: Vec<String>,
}
#[derive(Serialize)]
struct View<'a> {
    enabled: bool,
    allow_tools: &'a [String],
}
impl PlanPolicy {
    fn current<'a>(
        &self,
        domains: &'a [DomainStateView],
    ) -> ContributionResult<(&'a DomainStateView, bool)> {
        let view = domains
            .iter()
            .find(|view| view.snapshot.identity().id() == DOMAIN)
            .ok_or_else(|| ContributionError::Invalid("plan state is missing".into()))?;
        Ok((view, self.state.decode(&view.snapshot).map_err(invalid)?))
    }
}
fn invalid(error: impl std::fmt::Display) -> ContributionError {
    ContributionError::Invalid(error.to_string())
}

#[async_trait]
impl SessionCommand for PlanPolicy {
    async fn execute(
        &self,
        context: &SessionCommandContext,
        arguments: &CommandArguments,
        cancellation: CancellationToken,
    ) -> ContributionResult<Vec<ValidatedDomainProposal>> {
        if cancellation.is_cancelled() {
            return Err(ContributionError::Closed);
        }
        let (view, previous) = self.current(&context.domains)?;
        let enabled = match arguments.value().as_str().map(str::trim) {
            Some("on") => true,
            Some("off") => false,
            Some("" | "toggle") => !previous,
            _ => {
                return Err(ContributionError::Invalid(
                    "usage: /plan [on|off|toggle]".into(),
                ));
            }
        };
        Ok(vec![
            self.state
                .propose(view.revision, &enabled)
                .map_err(invalid)?,
        ])
    }
}
#[async_trait]
impl ContextContributor for PlanPolicy {
    async fn contribute(
        &self,
        context: &ContributionContext,
        cancellation: CancellationToken,
    ) -> ContributionResult<ContributionOutput> {
        if cancellation.is_cancelled() {
            return Err(ContributionError::Closed);
        }
        let (_, enabled) = self.current(&context.domains)?;
        let text = if enabled {
            format!(
                "Plan mode is enabled. Explore and present a concrete plan for the user to review before execution. Only these Tools are allowed: {}. Existing sandbox and approval requirements still apply. The user can disable plan mode with /plan off.",
                self.allowed.join(", ")
            )
        } else {
            "Plan mode is disabled. Follow the user's request under the existing sandbox and approval requirements.".into()
        };
        Ok(ContributionOutput {
            inputs: vec![ContributionInput::context(text)],
            domains: Vec::new(),
        })
    }
}
#[async_trait]
impl ToolPolicy for PlanPolicy {
    async fn decide(
        &self,
        context: &ContributionContext,
        request: &ToolPolicyRequest<'_>,
        cancellation: CancellationToken,
    ) -> ContributionResult<ToolPolicyDecision> {
        if cancellation.is_cancelled() {
            return Err(ContributionError::Closed);
        }
        let (_, enabled) = self.current(&context.domains)?;
        Ok(
            if !enabled || self.allowed.iter().any(|name| name == request.name) {
                ToolPolicyDecision::Abstain
            } else {
                ToolPolicyDecision::Deny {
                    reason: format!(
                        "Plan mode does not allow Tool {}. The user can disable it with /plan off.",
                        request.name
                    ),
                }
            },
        )
    }
}
#[async_trait]
impl SessionProjection for PlanPolicy {
    async fn project(
        &self,
        context: &SessionProjectionContext,
        cancellation: CancellationToken,
    ) -> ContributionResult<ProjectionValue> {
        if cancellation.is_cancelled() {
            return Err(ContributionError::Closed);
        }
        let (_, enabled) = self.current(context.domains())?;
        ProjectionValue::encode(&View {
            enabled,
            allow_tools: &self.allowed,
        })
        .map_err(invalid)
    }
}

#[async_trait]
impl PluginFactory for PlanPolicyFactory {
    fn prepare(&self, desired: &ConfigValue) -> rsi_meta::Result<PreparedActivation> {
        let config = Config::parse(desired)?;
        let normalized = serde_json::to_value(&config)
            .map_err(|error| MetaError::InvalidInput(error.to_string()))?;
        let bytes = serde_json::to_vec(&normalized)
            .map_err(|error| MetaError::InvalidInput(error.to_string()))?
            .len();
        Ok(PreparedActivation::with_state(normalized, config, bytes)
            .requiring_local::<DomainRegistrarContract>()
            .requiring_local::<ContributionRegistrarContract>())
    }
    async fn activate(&self, mut plan: ActivationPlan) -> rsi_meta::Result<()> {
        let config: Config = plan.take_state()?;
        let definition = DomainDefinition::new(
            DomainIdentity::new(DOMAIN, 1).expect("static domain"),
            &false,
            |_| Ok(()),
        )
        .map_err(|error| MetaError::Activation(error.to_string()))?;
        let context = plan.context().registration_context()?;
        let (state, domain_lease) = definition
            .register(plan.local::<DomainRegistrarContract>()?.as_ref(), &context)
            .map_err(|error| MetaError::Activation(error.to_string()))?;
        let callback = Arc::new(PlanPolicy {
            state,
            allowed: config.allow_tools,
        });
        let command_id =
            ContributionId::new("rsi.plan-policy.command").expect("static contribution");
        let descriptor = SessionCommandDescriptor::new(
            command_id.clone(),
            "plan",
            "Set plan mode: /plan [on|off|toggle]",
            true,
        )
        .map_err(|error| MetaError::Activation(error.to_string()))?;
        let registrations = [
            (
                command_id,
                ContributionKind::Command(SessionCommandRegistration::new(
                    descriptor,
                    callback.clone(),
                )),
            ),
            (
                ContributionId::new("rsi.plan-policy.context").expect("static contribution"),
                ContributionKind::Context(callback.clone()),
            ),
            (
                ContributionId::new("rsi.plan-policy.tools").expect("static contribution"),
                ContributionKind::ToolPolicy(callback.clone()),
            ),
            (
                ContributionId::new("rsi.plan-policy.view").expect("static contribution"),
                ContributionKind::Projection(callback),
            ),
        ];
        let registrar = plan.local::<ContributionRegistrarContract>()?;
        let mut leases = Vec::new();
        for (id, kind) in registrations {
            leases.push(
                registrar
                    .register(&context, ContributionRegistration::new(id, 20, kind))
                    .map_err(|error| MetaError::Activation(error.to_string()))?,
            );
        }
        plan.defer(
            "withdraw plan policy",
            Box::new(move || {
                Box::pin(async move {
                    drop(leases);
                    drop(domain_lease);
                    Ok(())
                })
            }),
        )
    }
}
