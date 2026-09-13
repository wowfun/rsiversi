use crate::{
    GOAL_COMMAND, GOAL_DOMAIN, GOAL_PROJECTION, GOAL_RESERVE, GOAL_SETTLE, GoalAction, GoalState,
    RoundSettlement, report::ReportTool,
};
use async_trait::async_trait;
use rsi_agent_composition_protocol::{
    ContextContributor, ContributionContext, ContributionError, ContributionInput,
    ContributionKind, ContributionOutput, ContributionRegistrarContract, ContributionRegistration,
    ContributionResult, DomainDefinition, DomainHandle, DomainRegistrarContract, SessionCommand,
    SessionCommandContext, SessionCommandRegistration, SessionProjection, SessionProjectionContext,
    ValidatedDomainProposal,
};
use rsi_agent_session_protocol::{
    CommandArguments, CommandRevision, ContributionId, DomainIdentity, DomainRequestId,
    DomainStateView, MessageId, ProjectionValue, SessionCommandDescriptor,
};
use rsi_meta::{ActivationPlan, ConfigValue, MetaError, PluginFactory, PreparedActivation};
use rsi_tools_protocol::{ToolRegistrarContract, ToolRegistration};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

/// Internal allocation arguments; the exact request identity lives in the command envelope.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ReserveGoal {
    /// Exact current Goal identity.
    pub id: DomainRequestId,
}

/// Internal canonical settlement arguments.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SettleGoal {
    /// Exact current Goal identity.
    pub id: DomainRequestId,
    /// Exact allocated message, never a guessed active Turn.
    pub message_id: MessageId,
    /// Disposition verified by the live controller against canonical records.
    pub settlement: RoundSettlement,
}

/// Agent-only ordinary plugin; deliberately holds no Turn or Kernel service.
#[derive(Clone, Copy, Debug, Default)]
pub struct GoalFactory;

#[derive(Debug)]
pub(crate) struct GoalPlugin {
    pub(crate) state: DomainHandle<GoalState>,
}

impl GoalPlugin {
    pub(crate) fn current<'a>(
        &self,
        domains: &'a [DomainStateView],
    ) -> ContributionResult<(&'a DomainStateView, GoalState)> {
        let view = domains
            .iter()
            .find(|view| view.snapshot.identity().id() == GOAL_DOMAIN)
            .ok_or_else(|| invalid("Goal domain is absent"))?;
        Ok((view, self.state.decode(&view.snapshot).map_err(invalid)?))
    }
}

pub(crate) fn invalid(error: impl std::fmt::Display) -> ContributionError {
    ContributionError::Invalid(error.to_string())
}

#[derive(Debug)]
enum Operation {
    Application,
    Reserve,
    Settle,
}

#[derive(Debug)]
struct Command {
    plugin: Arc<GoalPlugin>,
    operation: Operation,
}

#[async_trait]
impl SessionCommand for Command {
    async fn execute(
        &self,
        context: &SessionCommandContext,
        arguments: &CommandArguments,
        cancellation: CancellationToken,
    ) -> ContributionResult<Vec<ValidatedDomainProposal>> {
        if cancellation.is_cancelled() {
            return Err(ContributionError::Closed);
        }
        let (view, mut state) = self.plugin.current(&context.domains)?;
        match self.operation {
            Operation::Application => state
                .apply(
                    serde_json::from_value::<GoalAction>(arguments.value().clone())
                        .map_err(invalid)?,
                    context.header.settings().turn_budget(),
                    matches!(context.revision, CommandRevision::Draft { .. }),
                )
                .map_err(invalid)?,
            Operation::Reserve => {
                let args: ReserveGoal =
                    serde_json::from_value(arguments.value().clone()).map_err(invalid)?;
                let goal = state.current_mut(&args.id).map_err(invalid)?;
                goal.reserve().map_err(invalid)?;
                let reservation = goal
                    .reservation
                    .as_ref()
                    .ok_or_else(|| invalid("reserve returned no input"))?;
                if reservation.request_id.as_ref() != Some(&context.request_id) {
                    return Err(invalid(
                        "request identity differs from the proposed Goal reservation",
                    ));
                }
                let expected = reservation.input(&goal.id);
                if context.continuation_input.as_ref() != Some(&expected) {
                    return Err(invalid(
                        "continuation input differs from the proposed Goal reservation",
                    ));
                }
            }
            Operation::Settle => {
                let args: SettleGoal =
                    serde_json::from_value(arguments.value().clone()).map_err(invalid)?;
                state
                    .current_mut(&args.id)
                    .map_err(invalid)?
                    .settle(&args.message_id, args.settlement)
                    .map_err(invalid)?;
            }
        }
        Ok(vec![
            self.plugin
                .state
                .propose(view.revision, &state)
                .map_err(invalid)?,
        ])
    }
}

#[async_trait]
impl ContextContributor for GoalPlugin {
    async fn contribute(
        &self,
        context: &ContributionContext,
        cancellation: CancellationToken,
    ) -> ContributionResult<ContributionOutput> {
        if cancellation.is_cancelled() {
            return Err(ContributionError::Closed);
        }
        let (_, state) = self.current(&context.domains)?;
        let Some(goal) = state.goal else {
            return Ok(ContributionOutput::default());
        };
        let text = format!(
            "Current Goal {} ({:?}).\nObjective:\n{}\nConstraints:\n{}\nAllocated automatic parent rounds: {}/{}; remaining allocations: {}. A complete report requires concrete evidence and is verified only after the source Turn completes. Use report_goal only during the automatic Goal round, alone as the final Tool call after all work and checks; never batch it with another Tool. Reports cannot resume execution or increase this allowance.",
            goal.id,
            goal.phase,
            goal.objective,
            goal.constraints,
            goal.allocated_rounds,
            goal.max_rounds,
            goal.max_rounds - goal.allocated_rounds
        );
        Ok(ContributionOutput {
            inputs: vec![ContributionInput::context(text)],
            domains: Vec::new(),
        })
    }
}

#[async_trait]
impl SessionProjection for GoalPlugin {
    async fn project(
        &self,
        context: &SessionProjectionContext,
        cancellation: CancellationToken,
    ) -> ContributionResult<ProjectionValue> {
        if cancellation.is_cancelled() {
            return Err(ContributionError::Closed);
        }
        let (_, state) = self.current(context.domains())?;
        ProjectionValue::encode(&state).map_err(invalid)
    }
}

#[async_trait]
impl PluginFactory for GoalFactory {
    fn prepare(&self, desired: &ConfigValue) -> rsi_meta::Result<PreparedActivation> {
        if !desired.is_null() {
            return Err(MetaError::InvalidInput(
                "Goal configuration must be null".into(),
            ));
        }
        Ok(PreparedActivation::with_state(ConfigValue::Null, (), 0)
            .requiring_local::<DomainRegistrarContract>()
            .requiring_local::<ContributionRegistrarContract>()
            .requiring_local::<ToolRegistrarContract>())
    }
    async fn activate(&self, mut plan: ActivationPlan) -> rsi_meta::Result<()> {
        let (): () = plan.take_state()?;
        let definition = DomainDefinition::new(
            DomainIdentity::new(GOAL_DOMAIN, 1).map_err(activation)?,
            &GoalState::default(),
            GoalState::validate,
        )
        .map_err(activation)?;
        let context = plan.context().registration_context()?;
        let (state, domain_lease) = definition
            .register(plan.local::<DomainRegistrarContract>()?.as_ref(), &context)
            .map_err(activation)?;
        let plugin = Arc::new(GoalPlugin { state });
        let registrar = plan.local::<ContributionRegistrarContract>()?;
        let mut leases = Vec::new();
        for (id, name, operation, internal) in [
            (GOAL_COMMAND, "goal", Operation::Application, false),
            (GOAL_RESERVE, "goal-reserve", Operation::Reserve, true),
            (GOAL_SETTLE, "goal-settle", Operation::Settle, true),
        ] {
            let id = ContributionId::new(id).map_err(activation)?;
            let descriptor = SessionCommandDescriptor::new(
                id.clone(),
                name,
                "Control a bounded Goal with explicit structured arguments",
                !internal,
            )
            .map_err(activation)?;
            let mut command = SessionCommandRegistration::new(
                descriptor,
                Arc::new(Command {
                    plugin: plugin.clone(),
                    operation,
                }),
            );
            if internal {
                command = command.continuation_only(if name == "goal-reserve" {
                    rsi_agent_composition_protocol::ContinuationCommand::Reserve
                } else {
                    rsi_agent_composition_protocol::ContinuationCommand::Settle
                });
            }
            leases.push(
                registrar
                    .register(
                        &context,
                        ContributionRegistration::new(id, 25, ContributionKind::Command(command)),
                    )
                    .map_err(activation)?,
            );
        }
        for (id, kind) in [
            (
                "rsi.goal.context",
                ContributionKind::Context(plugin.clone()),
            ),
            (
                "rsi.goal.report",
                ContributionKind::PostTool(plugin.clone()),
            ),
            (
                "rsi.goal.report-policy",
                ContributionKind::ToolPolicy(plugin.clone()),
            ),
            (GOAL_PROJECTION, ContributionKind::Projection(plugin)),
        ] {
            leases.push(
                registrar
                    .register(
                        &context,
                        ContributionRegistration::new(
                            ContributionId::new(id).map_err(activation)?,
                            25,
                            kind,
                        ),
                    )
                    .map_err(activation)?,
            );
        }
        let definition = crate::report::definition().map_err(activation)?;
        let tool_lease = plan
            .local::<ToolRegistrarContract>()?
            .register_batch(vec![ToolRegistration {
                definition,
                executor: Arc::new(ReportTool),
                timeout: rsi_tools_protocol::ToolTimeoutPolicy::Execution { timeout_ms: 1000 },
            }])
            .map_err(activation)?;
        plan.defer(
            "withdraw Goal contributions",
            Box::new(move || {
                Box::pin(async move {
                    tool_lease.retire().map_err(|error| error.to_string())?;
                    drop(leases);
                    drop(domain_lease);
                    Ok(())
                })
            }),
        )
    }
}

fn activation(error: impl std::fmt::Display) -> MetaError {
    MetaError::Activation(error.to_string())
}
