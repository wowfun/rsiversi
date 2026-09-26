//! Agent-owned domain, pure commands and started-Tool mutations.
use crate::{
    SCHEDULE_DOMAIN, SCHEDULE_RESERVE, SCHEDULE_SETTLE, ScheduleController,
    ScheduleControllerContract, ScheduleRule, ScheduleState, request_id,
};
use async_trait::async_trait;
use rsi_agent_composition_protocol::{
    ContinuationCommand, ContributionError, ContributionKind, ContributionRegistrarContract,
    ContributionRegistration, ContributionResult, DomainDefinition, DomainForkPolicy, DomainHandle,
    DomainRegistrarContract, SessionCommand, SessionCommandContext, SessionCommandRegistration,
    SessionProjection, SessionProjectionContext, ValidatedDomainProposal,
};
use rsi_agent_session_protocol::{
    CommandArguments, ContributionId, DomainIdentity, DomainRequestId, DomainStateView, EffectId,
    MessageId, ProjectionValue, SessionCommandDescriptor,
};
use rsi_agent_turn_protocol::{
    AgentCallerAuthority, DomainMutation, TurnExecution, TurnExecutionContract,
};
use rsi_meta::{ActivationPlan, ConfigValue, MetaError, PluginFactory, PreparedActivation};
use rsi_tools_protocol::{
    ToolContent, ToolDefinition, ToolError, ToolExecution, ToolExecutor, ToolRegistrarContract,
    ToolRegistration, ToolResult, ToolScheduling, ToolTimeoutPolicy,
};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

/// Installs bounded Schedule Tools and its fork-reset domain.
#[derive(Clone, Copy, Debug, Default)]
pub struct ScheduleFactory;
#[derive(Debug)]
struct SchedulePlugin {
    state: DomainHandle<ScheduleState>,
}
impl SchedulePlugin {
    fn current<'a>(
        &self,
        domains: &'a [DomainStateView],
    ) -> ContributionResult<(&'a DomainStateView, ScheduleState)> {
        let view = domains
            .iter()
            .find(|v| v.snapshot.identity().id() == SCHEDULE_DOMAIN)
            .ok_or_else(|| invalid("Schedule domain is absent"))?;
        Ok((view, self.state.decode(&view.snapshot).map_err(invalid)?))
    }
}
/// Authenticated controller-selected UTC observation for one reserve proposal.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReserveSchedule {
    /// UTC time used to coalesce due occurrences.
    pub now_ms: u64,
}
/// Exact canonical input selected by the Host's terminal observation.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SettleSchedule {
    /// Previously allocated input identity.
    pub message_id: MessageId,
}
#[derive(Debug)]
struct Command {
    plugin: Arc<SchedulePlugin>,
    reserve: bool,
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
        if self.reserve {
            let args: ReserveSchedule =
                serde_json::from_value(arguments.value().clone()).map_err(invalid)?;
            let input = state.reserve(args.now_ms).map_err(invalid)?;
            if context.continuation_input.as_ref() != Some(&input)
                || context.request_id != request_id(input.round, "reserve").map_err(invalid)?
            {
                return Err(invalid(
                    "Schedule reserve differs from its frozen input or receipt",
                ));
            }
        } else {
            let args: SettleSchedule =
                serde_json::from_value(arguments.value().clone()).map_err(invalid)?;
            state.settle(&args.message_id).map_err(invalid)?;
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
impl SessionProjection for SchedulePlugin {
    async fn project(
        &self,
        context: &SessionProjectionContext,
        _: CancellationToken,
    ) -> ContributionResult<ProjectionValue> {
        ProjectionValue::encode(&self.current(context.domains())?.1).map_err(invalid)
    }
}
#[derive(Debug)]
struct ScheduleTool {
    plugin: Arc<SchedulePlugin>,
    turns: Arc<dyn TurnExecution>,
    host: Arc<dyn ScheduleController>,
    operation: &'static str,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Create {
    prompt: String,
    rule: ScheduleRule,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Resume {
    ids: Vec<DomainRequestId>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Delete {
    id: DomainRequestId,
}

impl ScheduleTool {
    fn mutate(
        &self,
        state: &mut ScheduleState,
        id: &DomainRequestId,
        arguments: serde_json::Value,
    ) -> rsi_tools_protocol::Result<()> {
        match self.operation {
            "schedule_create" => {
                let args: Create = serde_json::from_value(arguments)
                    .map_err(|e| ToolError::InvalidInput(e.to_string()))?;
                state
                    .create(
                        id.clone(),
                        args.prompt,
                        args.rule,
                        self.host.clock().now_ms(),
                    )
                    .map_err(ToolError::InvalidInput)?;
            }
            "schedule_resume" => {
                let args: Resume = serde_json::from_value(arguments)
                    .map_err(|e| ToolError::InvalidInput(e.to_string()))?;
                state.resume(&args.ids).map_err(ToolError::InvalidInput)?;
            }
            "schedule_delete" => {
                let args: Delete = serde_json::from_value(arguments)
                    .map_err(|e| ToolError::InvalidInput(e.to_string()))?;
                state.delete(&args.id).map_err(ToolError::InvalidInput)?;
            }
            _ => return Err(tool_error("unknown Schedule operation")),
        }
        Ok(())
    }
}

#[async_trait]
impl ToolExecutor for ScheduleTool {
    async fn execute(
        &self,
        arguments: serde_json::Value,
        execution: ToolExecution,
    ) -> rsi_tools_protocol::Result<ToolResult> {
        if execution.cancellation.is_cancelled() {
            return Err(ToolError::Cancelled);
        }
        let caller = execution
            .extension::<AgentCallerAuthority>()
            .ok_or_else(|| tool_error("Schedule requires Agent authority"))?;
        let effect = caller
            .tool_effect_id()
            .ok_or_else(|| tool_error("Schedule requires an exact started Tool"))?;
        let domains = self
            .turns
            .tool_settlement_domains(caller.claim(), effect)
            .await
            .map_err(tool_error)?;
        let (_, mut state) = self.plugin.current(&domains).map_err(tool_error)?;
        if self.operation == "schedule_list" {
            return result(
                json!({"state": state, "armed": self.host.armed(caller.session_id()), "failure": self.host.failure(caller.session_id())}),
            );
        }
        mutation_policy_guard(&domains)?;
        if !initial_human_root(self.turns.as_ref(), &caller)
            .await
            .map_err(tool_error)?
        {
            return Err(tool_error(
                "Schedule mutation requires an initial human-origin root Turn",
            ));
        }
        let epoch = self.host.epoch();
        if !epoch.is_open() {
            return Err(tool_error("Schedule Host generation is closed"));
        }
        let id = mutation_id(effect)?;
        self.mutate(&mut state, &id, arguments.clone())?;
        if !self
            .host
            .disarm_for_mutation(&epoch, caller.session_id())
            .await
            .map_err(tool_error)?
            || execution.cancellation.is_cancelled()
        {
            return Err(ToolError::Cancelled);
        }
        // The previous owner may have settled its reservation while disarming.
        let domains = self
            .turns
            .tool_settlement_domains(caller.claim(), effect)
            .await
            .map_err(tool_error)?;
        let policy = mutation_policy_guard(&domains)?;
        let (view, mut state) = self.plugin.current(&domains).map_err(tool_error)?;
        self.mutate(&mut state, &id, arguments)?;
        let mutation = DomainMutation {
            guards: vec![policy],
            require_uncancelled_turn: false,
            request_id: id.clone(),
            proposals: vec![
                self.plugin
                    .state
                    .propose(view.revision, &state)
                    .map_err(tool_error)?,
            ],
            facts: vec![],
        };
        let receipt = match self.turns.commit_tool_domains(&caller, mutation).await {
            Ok(receipt) => receipt,
            Err(rsi_agent_turn_protocol::TurnError::DomainOutcomeUnknown { .. }) => {
                return result(json!({"status":"persistence_unknown", "request_id":id}));
            }
            Err(error) => return Err(tool_error(error)),
        };
        let arming = if execution.cancellation.is_cancelled() || !epoch.is_open() {
            Ok(false)
        } else {
            self.host
                .arm_after_commit(
                    &epoch,
                    &caller,
                    view.snapshot.identity(),
                    &receipt,
                    execution.cancellation.clone(),
                )
                .await
        };
        let (armed, diagnostic) = match arming {
            Ok(armed) => (armed, None),
            Err(error) => (false, Some(error.chars().take(1024).collect::<String>())),
        };
        result(
            json!({"status":if armed {"created_armed"} else {"created_disarmed"}, "request_id":id, "state": state, "diagnostic": diagnostic}),
        )
    }
}
/// Deterministic request identity binds one domain mutation to its started effect.
///
/// # Errors
/// Returns an error if the derived identity violates the domain request contract.
pub fn mutation_id(effect: &EffectId) -> rsi_tools_protocol::Result<DomainRequestId> {
    use sha2::{Digest, Sha256};
    DomainRequestId::new(format!(
        "schedule-tool-{:x}",
        Sha256::digest(effect.as_str().as_bytes())
    ))
    .map_err(tool_error)
}
/// Checks the original acceptance; later human steering cannot promote an automatic Turn.
///
/// # Errors
/// Propagates claim expiry and failures reading canonical acceptance facts.
pub async fn initial_human_root(
    turns: &dyn TurnExecution,
    caller: &AgentCallerAuthority,
) -> rsi_agent_turn_protocol::Result<bool> {
    if caller.header().fork_origin().is_some() {
        return Ok(false);
    }
    Ok(
        rsi_agent_turn_protocol::initial_turn_input(turns, caller.claim())
            .await?
            .human,
    )
}
fn invalid(error: impl std::fmt::Display) -> ContributionError {
    ContributionError::Invalid(error.to_string())
}
fn tool_error(error: impl std::fmt::Display) -> ToolError {
    ToolError::Execution(error.to_string())
}
fn activation(error: impl std::fmt::Display) -> MetaError {
    MetaError::Activation(error.to_string())
}
fn result(value: serde_json::Value) -> rsi_tools_protocol::Result<ToolResult> {
    let text = value.to_string();
    ToolResult::new(value, vec![ToolContent::Text { text }], false)
}

#[async_trait]
impl PluginFactory for ScheduleFactory {
    fn prepare(&self, desired: &ConfigValue) -> rsi_meta::Result<PreparedActivation> {
        if !desired.is_null() {
            return Err(MetaError::InvalidInput(
                "Schedule configuration must be null".into(),
            ));
        }
        Ok(PreparedActivation::new(ConfigValue::Null)
            .requiring_local::<DomainRegistrarContract>()
            .requiring_local::<ContributionRegistrarContract>()
            .requiring_local::<ToolRegistrarContract>()
            .requiring_local::<TurnExecutionContract>()
            .requiring_local::<ScheduleControllerContract>())
    }
    async fn activate(&self, plan: ActivationPlan) -> rsi_meta::Result<()> {
        let definition = DomainDefinition::new(
            DomainIdentity::new(SCHEDULE_DOMAIN, 1).map_err(activation)?,
            &ScheduleState::default(),
            ScheduleState::validate,
        )
        .map_err(activation)?
        .with_fork_policy(DomainForkPolicy::ResetToInitial);
        let context = plan.context().registration_context()?;
        let (state, domain_lease) = definition
            .register(plan.local::<DomainRegistrarContract>()?.as_ref(), &context)
            .map_err(activation)?;
        let plugin = Arc::new(SchedulePlugin { state });
        let registrar = plan.local::<ContributionRegistrarContract>()?;
        let mut leases = Vec::new();
        for (id, name, reserve) in [
            (SCHEDULE_RESERVE, "schedule-reserve", true),
            (SCHEDULE_SETTLE, "schedule-settle", false),
        ] {
            let id = ContributionId::new(id).map_err(activation)?;
            let descriptor = SessionCommandDescriptor::new(
                id.clone(),
                name,
                "Internal Schedule transition",
                false,
            )
            .map_err(activation)?;
            let command = SessionCommandRegistration::new(
                descriptor,
                Arc::new(Command {
                    plugin: plugin.clone(),
                    reserve,
                }),
            )
            .continuation_only(if reserve {
                ContinuationCommand::Reserve
            } else {
                ContinuationCommand::Settle
            });
            leases.push(
                registrar
                    .register(
                        &context,
                        ContributionRegistration::new(id, 26, ContributionKind::Command(command)),
                    )
                    .map_err(activation)?,
            );
        }
        leases.push(
            registrar
                .register(
                    &context,
                    ContributionRegistration::new(
                        ContributionId::new("rsi.schedule.projection").map_err(activation)?,
                        26,
                        ContributionKind::Projection(plugin.clone()),
                    ),
                )
                .map_err(activation)?,
        );
        let turns = plan.local::<TurnExecutionContract>()?;
        let host = plan.local::<ScheduleControllerContract>()?;
        let mut tools = Vec::new();
        for (operation, description, schema) in tool_definitions() {
            tools.push(ToolRegistration {
                output: None,
                definition: ToolDefinition::new(operation, description, schema)
                    .map_err(activation)?
                    .with_scheduling(ToolScheduling::Exclusive),
                executor: Arc::new(ScheduleTool {
                    plugin: plugin.clone(),
                    turns: turns.clone(),
                    host: host.clone(),
                    operation,
                }),
                timeout: ToolTimeoutPolicy::Execution { timeout_ms: 30_000 },
            });
        }
        let tool_lease = plan
            .local::<ToolRegistrarContract>()?
            .register_batch(tools)
            .map_err(activation)?;
        plan.defer(
            "withdraw Schedule",
            Box::new(move || {
                Box::pin(async move {
                    tool_lease.retire().map_err(|e| e.to_string())?;
                    drop((leases, domain_lease));
                    Ok(())
                })
            }),
        )
    }
}

fn tool_definitions() -> [(&'static str, &'static str, serde_json::Value); 4] {
    [
        (
            "schedule_create",
            "Create and arm a finite reminder in this Session. Only an initial human-origin root Turn may create work. Maximum 16 reminders and 100 lifetime automatic parent rounds; restart requires explicit resume. Times are UTC Unix milliseconds. Recurrence is at least five minutes.",
            json!({"type":"object","properties":{"prompt":{"type":"string"},"rule":{"oneOf":[{"type":"object","properties":{"kind":{"const":"after"},"delay_ms":{"type":"integer","minimum":1}},"required":["kind","delay_ms"],"additionalProperties":false},{"type":"object","properties":{"kind":{"const":"at"},"at_ms":{"type":"integer","minimum":1}},"required":["kind","at_ms"],"additionalProperties":false},{"type":"object","properties":{"kind":{"const":"every"},"interval_ms":{"type":"integer","minimum":300_000}},"required":["kind","interval_ms"],"additionalProperties":false}]}},"required":["prompt","rule"],"additionalProperties":false}),
        ),
        (
            "schedule_list",
            "Read retained reminders, their lifetime allowance and live arming state. Reading never arms work.",
            json!({"type":"object","properties":{},"additionalProperties":false}),
        ),
        (
            "schedule_resume",
            "Explicitly resume selected pending reminder IDs without resetting the lifetime allowance. Requires an initial human-origin root Turn.",
            json!({"type":"object","properties":{"ids":{"type":"array","items":{"type":"string"},"minItems":1,"maxItems":16}},"required":["ids"],"additionalProperties":false}),
        ),
        (
            "schedule_delete",
            "Delete a reminder without refunding accepted rounds. Requires an initial human-origin root Turn.",
            json!({"type":"object","properties":{"id":{"type":"string"}},"required":["id"],"additionalProperties":false}),
        ),
    ]
}

fn mutation_policy_guard(
    domains: &[DomainStateView],
) -> rsi_tools_protocol::Result<rsi_agent_turn_protocol::DomainReadGuard> {
    let policy = domains
        .iter()
        .find(|view| view.snapshot.identity().id() == "rsi.plan-policy")
        .ok_or_else(|| tool_error("Schedule requires plan-policy state"))?;
    if policy.snapshot.state().value() == true {
        return Err(tool_error(
            "Schedule mutations are unavailable in plan mode",
        ));
    }
    Ok(rsi_agent_turn_protocol::DomainReadGuard {
        domain: policy.snapshot.identity().clone(),
        revision: policy.revision,
    })
}
