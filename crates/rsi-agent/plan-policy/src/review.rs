use super::{PlanPolicy, invalid};
use async_trait::async_trait;
use rsi_agent_composition_protocol::{
    ContributionResult, ToolSettlement, ToolSettlementContext, ToolSettlementContributor,
};
use rsi_agent_session_protocol::{
    DomainRevision, DomainStateView, EffectId, SessionFactBody, SessionId, ToolConclusion,
};
use rsi_agent_turn_protocol::{AgentCallerAuthority, TurnExecution};
use rsi_tools_protocol::{
    ToolContent, ToolError, ToolExecution, ToolExecutor, ToolLaneParkingAuthority, ToolResult,
};
use rsi_user_questions_protocol::{
    ClosedReview, Question, QuestionRequest, ReviewChoice, UserQuestions,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::sync::Arc;

pub(super) const REVIEW_DOMAIN: &str = "rsi.plan-review";
pub(super) const WRITE: &str = "plan_write";
pub(super) const REQUEST: &str = "request_plan_execution";
const MAXIMUM_PLAN_BYTES: usize = 32 * 1024;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct PlanRef {
    session_id: SessionId,
    effect_id: EffectId,
    sha256: String,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Document {
    title: String,
    body: String,
}
impl Document {
    fn validate(&self) -> Result<(), String> {
        if self.title.trim().is_empty()
            || self.body.trim().is_empty()
            || serde_json::to_vec(self).map_err(|e| e.to_string())?.len() > MAXIMUM_PLAN_BYTES
        {
            return Err("plan requires a nonempty title and body within 32 KiB encoded".into());
        }
        Ok(())
    }
    fn reference(&self, session_id: SessionId, effect_id: EffectId) -> PlanRef {
        PlanRef {
            session_id,
            effect_id,
            sha256: format!(
                "{:x}",
                Sha256::digest(serde_json::to_vec(self).expect("document serialization"))
            ),
        }
    }
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct SavedPlan {
    plan_ref: PlanRef,
    document: Document,
}
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum Decision {
    ApproveExecute,
    RequestChanges,
    Decline,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ReviewReceipt {
    plan_ref: PlanRef,
    mode_revision: DomainRevision,
    review_revision: DomainRevision,
    decision: Decision,
    feedback: Option<String>,
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ReviewState {
    plan: Option<SavedPlan>,
    last_review: Option<ReviewReceipt>,
}
impl ReviewState {
    pub(super) fn validate(&self) -> Result<(), String> {
        if let Some(saved) = &self.plan {
            saved.document.validate()?;
            if saved.plan_ref
                != saved.document.reference(
                    saved.plan_ref.session_id.clone(),
                    saved.plan_ref.effect_id.clone(),
                )
            {
                return Err("saved plan digest mismatch".into());
            }
        }
        if let Some(receipt) = &self.last_review
            && (self.plan.as_ref().map(|p| &p.plan_ref) != Some(&receipt.plan_ref)
                || receipt.feedback.as_ref().is_some_and(|s| s.len() > 4096))
        {
            return Err("review does not match the saved plan or exceeds feedback limit".into());
        }
        Ok(())
    }
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RequestArguments {
    plan_ref: PlanRef,
}

impl PlanPolicy {
    pub(super) fn review_current<'a>(
        &self,
        domains: &'a [DomainStateView],
    ) -> ContributionResult<(&'a DomainStateView, ReviewState)> {
        let view = domains
            .iter()
            .find(|v| v.snapshot.identity().id() == REVIEW_DOMAIN)
            .ok_or_else(|| invalid("plan review state is missing"))?;
        Ok((view, self.review.decode(&view.snapshot).map_err(invalid)?))
    }
}
impl ToolSettlementContributor for PlanPolicy {
    fn settle(&self, context: &ToolSettlementContext<'_>) -> ContributionResult<ToolSettlement> {
        let SessionFactBody::ToolIntent {
            name,
            arguments,
            effect_id,
            ..
        } = context.intent.body()
        else {
            return Err(invalid("plan settlement requires exact ToolIntent"));
        };
        if context.result.is_error || (name != WRITE && name != REQUEST) {
            return Ok(ToolSettlement::default());
        }
        if context.header.fork_origin().is_some() {
            return Err(invalid("plan Tools are root-only"));
        }
        let (review_view, mut state) = self.review_current(context.domains)?;
        if name == WRITE {
            let document: Document = serde_json::from_value(arguments.clone()).map_err(invalid)?;
            document.validate().map_err(invalid)?;
            let saved = SavedPlan {
                plan_ref: document
                    .reference(context.header.session_id().clone(), effect_id.clone()),
                document,
            };
            let actual: SavedPlan =
                serde_json::from_value(context.result.value.clone()).map_err(invalid)?;
            if actual != saved {
                return Err(invalid("saved plan differs from its exact ToolIntent"));
            }
            state = ReviewState {
                plan: Some(saved),
                last_review: None,
            };
            return Ok(ToolSettlement {
                require_uncancelled_turn: false,
                domains: vec![
                    self.review
                        .propose(review_view.revision, &state)
                        .map_err(invalid)?,
                ],
                conclusion: None,
            });
        }
        let args: RequestArguments = serde_json::from_value(arguments.clone()).map_err(invalid)?;
        let receipt: ReviewReceipt =
            serde_json::from_value(context.result.value.clone()).map_err(invalid)?;
        let (mode_view, enabled) = self.current(context.domains)?;
        if !enabled
            || context.header.initial_output().is_some()
            || receipt.mode_revision != mode_view.revision
            || receipt.review_revision != review_view.revision
            || receipt.plan_ref != args.plan_ref
            || state.plan.as_ref().map(|p| &p.plan_ref) != Some(&receipt.plan_ref)
        {
            return Err(invalid(
                "plan review is stale or incompatible with this Session",
            ));
        }
        state.last_review = Some(receipt.clone());
        let mut domains = vec![
            self.review
                .propose(review_view.revision, &state)
                .map_err(invalid)?,
        ];
        if receipt.decision == Decision::ApproveExecute {
            domains.push(
                self.state
                    .propose(mode_view.revision, &false)
                    .map_err(invalid)?,
            );
        }
        Ok(ToolSettlement {
            require_uncancelled_turn: true,
            domains,
            conclusion: (receipt.decision == Decision::Decline)
                .then_some(ToolConclusion { structured: None }),
        })
    }
}

#[derive(Debug)]
pub(super) struct PlanTool {
    pub policy: Arc<PlanPolicy>,
    pub turns: Arc<dyn TurnExecution>,
    pub questions: Arc<dyn UserQuestions>,
    pub write: bool,
}
fn tool_error(error: impl std::fmt::Display) -> ToolError {
    ToolError::Execution(error.to_string())
}
fn result(value: impl Serialize) -> rsi_tools_protocol::Result<ToolResult> {
    let value = serde_json::to_value(value).map_err(tool_error)?;
    ToolResult::new(
        value.clone(),
        vec![ToolContent::Text {
            text: value.to_string(),
        }],
        false,
    )
}
#[async_trait]
impl ToolExecutor for PlanTool {
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
            .ok_or_else(|| tool_error("plan Tool requires Agent authority"))?;
        if caller.header().fork_origin().is_some() {
            return Err(tool_error("plan Tools are root-only"));
        }
        let effect = caller
            .tool_effect_id()
            .ok_or_else(|| tool_error("plan Tool requires a started effect"))?;
        if self.write {
            let document: Document = serde_json::from_value(arguments)
                .map_err(|e| ToolError::InvalidInput(e.to_string()))?;
            document.validate().map_err(ToolError::InvalidInput)?;
            return result(SavedPlan {
                plan_ref: document.reference(caller.session_id().clone(), effect.clone()),
                document,
            });
        }
        if caller.header().initial_output().is_some() {
            return Err(tool_error(
                "structured-output Sessions cannot request plan execution",
            ));
        }
        let args: RequestArguments = serde_json::from_value(arguments)
            .map_err(|e| ToolError::InvalidInput(e.to_string()))?;
        let domains = self
            .turns
            .tool_settlement_domains(caller.claim(), effect)
            .await
            .map_err(tool_error)?;
        let (mode, enabled) = self.policy.current(&domains).map_err(tool_error)?;
        let (review, state) = self.policy.review_current(&domains).map_err(tool_error)?;
        let saved = state
            .plan
            .ok_or_else(|| tool_error("save a plan before requesting execution"))?;
        if !enabled || args.plan_ref != saved.plan_ref {
            return Err(tool_error("plan mode must be enabled and plan_ref current"));
        }
        let binding = format!(
            "{:x}",
            Sha256::digest(
                serde_json::to_vec(&(effect, &args.plan_ref, mode.revision, review.revision))
                    .map_err(tool_error)?
            )
        );
        let request = review_question(&caller, &saved, binding)?;
        request.validate().map_err(tool_error)?;
        let parking = execution
            .extension::<ToolLaneParkingAuthority>()
            .ok_or_else(|| tool_error("plan review requires exclusive-final admission"))?;
        let waiting = self
            .turns
            .park_human_wait(caller.claim(), (*parking).clone())
            .await
            .map_err(tool_error)?;
        let answer = self
            .questions
            .ask(request.clone(), execution.cancellation.clone())
            .await;
        waiting
            .resume(execution.cancellation.clone())
            .await
            .map_err(tool_error)?;
        if execution.cancellation.is_cancelled() {
            return Err(ToolError::Cancelled);
        }
        let answer = answer.map_err(tool_error)?;
        answer.validate_for(&request).map_err(tool_error)?;
        let answer = answer
            .review
            .ok_or_else(|| tool_error("typed review answer is required"))?;
        let decision = match answer.choice_id.as_str() {
            "approve_execute" => Decision::ApproveExecute,
            "request_changes" => Decision::RequestChanges,
            "decline" => Decision::Decline,
            _ => return Err(tool_error("unknown plan review decision")),
        };
        result(ReviewReceipt {
            plan_ref: saved.plan_ref,
            mode_revision: mode.revision,
            review_revision: review.revision,
            decision,
            feedback: answer.feedback,
        })
    }
}

pub(super) fn definitions()
-> rsi_tools_protocol::Result<Vec<(rsi_tools_protocol::ToolDefinition, bool)>> {
    use rsi_tools_protocol::{ToolDefinition, ToolScheduling};
    use serde_json::json;
    Ok(vec![
        (ToolDefinition::new(WRITE, "Save a concrete plan for human review. Returns the exact plan_ref. The encoded title and body together must fit 32 KiB; saving does not authorize execution.", json!({"type":"object","properties":{"title":{"type":"string","minLength":1},"body":{"type":"string","minLength":1}},"required":["title","body"],"additionalProperties":false}))?.with_scheduling(ToolScheduling::Exclusive), true),
        (ToolDefinition::new(REQUEST, "Ask the human to approve the exact saved plan. Requires plan mode and plan_ref from plan_write. Approval atomically exits plan mode; request_changes retains it; decline ends this turn.", json!({"type":"object","properties":{"plan_ref":{"type":"object","properties":{"session_id":{"type":"string"},"effect_id":{"type":"string"},"sha256":{"type":"string"}},"required":["session_id","effect_id","sha256"],"additionalProperties":false}},"required":["plan_ref"],"additionalProperties":false}))?.with_scheduling(ToolScheduling::ExclusiveFinal), false),
    ])
}

fn review_question(
    caller: &AgentCallerAuthority,
    saved: &SavedPlan,
    binding: String,
) -> rsi_tools_protocol::Result<QuestionRequest> {
    let mut entropy = [0_u8; 16];
    getrandom::fill(&mut entropy).map_err(tool_error)?;
    let request = QuestionRequest {
        id: format!("plan-review-{:032x}", u128::from_le_bytes(entropy)),
        session_id: caller.session_id().to_string(),
        turn_id: caller.turn_id().to_string(),
        questions: vec![Question {
            id: "plan".into(),
            prompt: format!("{}\n\n{}", saved.document.title, saved.document.body),
            options: vec![],
        }],
        review: Some(ClosedReview {
            binding,
            choices: [
                ("approve_execute", "Approve and execute"),
                ("request_changes", "Request changes"),
                ("decline", "Decline and end turn"),
            ]
            .into_iter()
            .map(|(id, label)| ReviewChoice {
                id: id.into(),
                label: label.into(),
            })
            .collect(),
        }),
    };
    Ok(request)
}
