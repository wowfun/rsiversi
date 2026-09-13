use crate::{
    Goal, GoalReport, GoalReportKind,
    plugin::{GoalPlugin, invalid},
    state::safe_text,
};
use async_trait::async_trait;
use rsi_agent_composition_protocol::{
    ContributionContext, ContributionError, ContributionOutput, ContributionResult,
    PostToolContributor, ToolPolicy, ToolPolicyDecision, ToolPolicyRequest,
};
use rsi_agent_session_protocol::{DomainRequestId, SessionFact, SessionFactBody};
use rsi_tools_protocol::{ToolContent, ToolError, ToolExecution, ToolExecutor, ToolResult};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

#[derive(Debug)]
pub(crate) struct ReportTool;

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct ReportArguments {
    goal_id: DomainRequestId,
    kind: GoalReportKind,
    evidence: String,
}

impl ReportArguments {
    fn parse(value: serde_json::Value) -> Result<Self, String> {
        let args: Self = serde_json::from_value(value).map_err(|error| error.to_string())?;
        safe_text(&args.evidence, 4096, false)?;
        Ok(args)
    }
}

#[async_trait]
impl ToolExecutor for ReportTool {
    async fn execute(
        &self,
        arguments: serde_json::Value,
        execution: ToolExecution,
    ) -> rsi_tools_protocol::Result<ToolResult> {
        if execution.cancellation.is_cancelled() {
            return Err(ToolError::Cancelled);
        }
        let args = ReportArguments::parse(arguments).map_err(ToolError::InvalidInput)?;
        ToolResult::new(
            serde_json::json!({"goal_report": {"version": 1, "report": args}}),
            vec![ToolContent::Text { text: "Goal report received as a claim. Canonical Turn settlement determines its disposition.".into() }],
            false,
        )
    }
}

async fn owns_round(context: &ContributionContext, goal: &Goal) -> ContributionResult<bool> {
    let Some(reservation) = goal
        .reservation
        .as_ref()
        .filter(|reservation| reservation.settlement.is_none())
    else {
        return Ok(false);
    };
    let page = context
        .facts
        .read(context.accepted_fact_seq.saturating_sub(1), 1)
        .await?;
    Ok(page.facts.iter().any(|fact| fact.seq() == context.accepted_fact_seq
        && matches!(fact.body(), SessionFactBody::MessageTurnAccepted { turn_id, message_ids, .. }
            if turn_id == &context.turn_id && message_ids.as_slice() == [reservation.message_id.clone()])))
}

#[async_trait]
impl ToolPolicy for GoalPlugin {
    async fn decide(
        &self,
        context: &ContributionContext,
        request: &ToolPolicyRequest<'_>,
        cancellation: CancellationToken,
    ) -> ContributionResult<ToolPolicyDecision> {
        if cancellation.is_cancelled() {
            return Err(ContributionError::Closed);
        }
        if request.name != "report_goal" {
            return Ok(ToolPolicyDecision::Abstain);
        }
        let (_, state) = self.current(&context.domains)?;
        if let (Some(goal), Ok(args)) = (
            &state.goal,
            ReportArguments::parse(request.arguments.clone()),
        ) && goal.id == args.goal_id
            && owns_round(context, goal).await?
        {
            return Ok(ToolPolicyDecision::Abstain);
        }
        Ok(ToolPolicyDecision::Deny { reason: "report_goal requires the exact current automatic Goal round and bounded report arguments".into() })
    }
}

#[async_trait]
impl PostToolContributor for GoalPlugin {
    async fn contribute(
        &self,
        context: &ContributionContext,
        settled: &[Arc<SessionFact>],
        cancellation: CancellationToken,
    ) -> ContributionResult<ContributionOutput> {
        if cancellation.is_cancelled() {
            return Err(ContributionError::Closed);
        }
        let (view, mut state) = self.current(&context.domains)?;
        let Some(goal) = &mut state.goal else {
            return Ok(ContributionOutput::default());
        };
        if !owns_round(context, goal).await? {
            return Ok(ContributionOutput::default());
        }
        let mut reports = Vec::new();
        for fact in settled {
            if let SessionFactBody::ToolResult {
                turn_id,
                effect_id,
                identity,
                result,
            } = fact.body()
                && turn_id == &context.turn_id
                && !result.is_error
                && let Some(envelope) = result.value.get("goal_report")
                && envelope.get("version").and_then(serde_json::Value::as_u64) == Some(1)
                && let Some(report) = envelope.get("report")
                && let Ok(args) = ReportArguments::parse(report.clone())
                && args.goal_id == goal.id
            {
                reports.push((fact.seq(), effect_id.clone(), identity.clone(), args));
            }
        }
        if reports.is_empty() {
            return Ok(ContributionOutput::default());
        }
        let mut cursor = context.accepted_fact_seq;
        let mut authenticated = std::collections::BTreeSet::new();
        while cursor < context.horizon.fact_seq {
            if cancellation.is_cancelled() {
                return Err(ContributionError::Closed);
            }
            let page = context.facts.read(cursor, 128).await?;
            if page.through_seq <= cursor {
                return Err(invalid("Goal report history made no progress"));
            }
            for fact in page.facts {
                if let SessionFactBody::ToolIntent {
                    turn_id,
                    effect_id,
                    identity,
                    name,
                    arguments,
                    ..
                } = fact.body()
                    && turn_id == &context.turn_id
                    && name == "report_goal"
                {
                    for (seq, expected_effect, expected_identity, args) in &reports {
                        if fact.seq() < *seq
                            && effect_id == expected_effect
                            && identity == expected_identity
                            && serde_json::to_value(args).map_err(invalid)? == *arguments
                        {
                            authenticated.insert(*seq);
                        }
                    }
                }
            }
            cursor = page.through_seq;
        }
        let Some((_, _, _, args)) = reports
            .into_iter()
            .rev()
            .find(|(seq, ..)| authenticated.contains(seq))
        else {
            return Ok(ContributionOutput::default());
        };
        goal.record_report(GoalReport {
            kind: args.kind,
            evidence: args.evidence,
            source_turn: context.turn_id.clone(),
        })
        .map_err(invalid)?;
        Ok(ContributionOutput {
            inputs: Vec::new(),
            domains: vec![self.state.propose(view.revision, &state).map_err(invalid)?],
        })
    }
}

pub(crate) fn definition() -> rsi_tools_protocol::Result<rsi_tools_protocol::ToolDefinition> {
    use rsi_tools_protocol::{ToolDefinition, ToolScheduling};
    Ok(ToolDefinition::new("report_goal",
            "Report a Goal completion claim with concrete evidence, a blocker, or a pause request. Call this Tool alone as the final Tool call, after all work and checks; never batch it with another Tool. Only the current automatic Goal round may report. This does not arm execution, increase rounds, or override Turn failure.",
            serde_json::json!({"type":"object","properties":{
                "goal_id":{"type":"string","minLength":1,"maxLength":256},
                "kind":{"type":"string","enum":["complete","blocked","pause"]},
                "evidence":{"type":"string","minLength":1,"maxLength":4096}
            },"required":["goal_id","kind","evidence"],"additionalProperties":false}),
        )?.with_scheduling(ToolScheduling::ExclusiveFinal))
}
