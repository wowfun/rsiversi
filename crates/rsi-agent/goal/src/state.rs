//! Bounded semantic codec and effect-free state transitions.

use rsi_agent_session_protocol::{DomainRequestId, MessageId, TurnBudget, TurnId, TurnOutcome};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// The single current Goal; replacing a Goal does not rewrite its control history.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct GoalState {
    /// Absent until explicitly created.
    pub goal: Option<Goal>,
}

/// Frozen task and bounded current progress.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Goal {
    /// Caller-chosen logical identity, distinct for each newly created Goal.
    pub id: DomainRequestId,
    /// Requested result, limited to 8 KiB UTF-8.
    pub objective: String,
    /// Task constraints, limited to 4 KiB UTF-8.
    pub constraints: String,
    /// Explicit positive maximum automatic parent rounds.
    pub max_rounds: u64,
    /// Exact immutable Header budget shared by those rounds.
    pub turn_budget: TurnBudget,
    /// Allocations charged before message acceptance, without refunds.
    pub allocated_rounds: u64,
    /// Durable task phase; independent of live execution authorization.
    pub phase: GoalPhase,
    /// Bounded human-readable stopping explanation.
    pub reason: Option<String>,
    /// Latest allocation only; older allocations remain in durable controls.
    pub reservation: Option<GoalReservation>,
    /// Latest model claim, including its source Turn even when unverified.
    pub report: Option<GoalReport>,
}

/// Durable phase, never a live lease indicator.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum GoalPhase {
    /// Eligible for an explicitly armed controller to allocate another round.
    Active,
    /// Stopped by explicit pause, cancellation or model pause request.
    Paused,
    /// Execution failure, model blocker or allocation cap requires user action.
    Blocked,
    /// A completion claim's exact source Turn canonically completed.
    Completed,
}

/// Model report vocabulary; it contains no create, resume or budget operation.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum GoalReportKind {
    /// Evidence claims that the task is done; subject to Turn settlement.
    Complete,
    /// Further work requires an external change.
    Blocked,
    /// The model requests human intervention before continuing.
    Pause,
}

/// A model claim authenticated by the exact settled report Tool call.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct GoalReport {
    /// Requested disposition.
    pub kind: GoalReportKind,
    /// Nonempty bounded evidence or reason, at most 4 KiB UTF-8.
    pub evidence: String,
    /// Exact Turn containing the report Tool result.
    pub source_turn: TurnId,
}

/// Complete frozen input of one charged round.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct GoalReservation {
    /// One-based allocation number.
    pub round: u64,
    /// Deterministic identity bound to Goal and round.
    pub message_id: MessageId,
    /// Deterministic internal reserve receipt, absent only for the staged first baseline.
    pub request_id: Option<DomainRequestId>,
    /// Exact retry input; never regenerated after uncertain acceptance.
    pub input: String,
    /// SHA-256 of the exact UTF-8 input.
    pub input_sha256: String,
    /// Absent while acceptance, claim or terminal outcome remains unresolved.
    pub settlement: Option<RoundSettlement>,
}

/// Small outcome retained with the latest reservation.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum RoundSettlement {
    /// Explicitly abandoned before acceptance; no mailbox entry or refund is invented.
    Abandoned,
    /// Canonically discarded before claim; the allocation remains charged.
    Discarded,
    /// Canonical terminal classification; full diagnostics remain in Turn Facts.
    Turn {
        /// Exact admitted Turn.
        turn_id: TurnId,
        /// Terminal class governing continuation.
        outcome: RoundOutcome,
    },
}

/// Closed terminal classes, excluding potentially large result bodies.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RoundOutcome {
    /// Successful natural model termination.
    Completed,
    /// User or controller cancellation.
    Cancelled,
    /// Runtime or provider failure.
    Failed,
    /// Partial output followed by failure.
    PartialFailed,
    /// Unknown external effect after recovery.
    Interrupted,
    /// One immutable Turn budget exhausted.
    BudgetExceeded,
}

impl From<&TurnOutcome> for RoundOutcome {
    fn from(outcome: &TurnOutcome) -> Self {
        match outcome {
            TurnOutcome::Completed => Self::Completed,
            TurnOutcome::Cancelled => Self::Cancelled,
            TurnOutcome::Failed { .. } => Self::Failed,
            TurnOutcome::PartialFailed { .. } => Self::PartialFailed,
            TurnOutcome::Interrupted { .. } => Self::Interrupted,
            TurnOutcome::BudgetExceeded { .. } => Self::BudgetExceeded,
        }
    }
}

/// Checked aggregate allowances for automatic parent Turns only.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct GoalAllowance {
    /// Sum of parent elapsed-time limits in milliseconds.
    pub elapsed_ms: u64,
    /// Maximum parent provider attempts, including compaction.
    pub provider_attempts: u64,
    /// Maximum parent Tool calls.
    pub tool_calls: u64,
    /// Maximum parent generated records.
    pub generated_records: u64,
    /// Maximum parent generated record bytes.
    pub generated_record_bytes: u64,
}

impl GoalAllowance {
    /// Checks every dimension and multiplication before any Goal allocation.
    ///
    /// # Errors
    /// Rejects zero rounds, an invalid Turn budget or any aggregate overflow.
    pub fn new(rounds: u64, budget: &TurnBudget) -> Result<Self, String> {
        budget.validate().map_err(|error| error.to_string())?;
        if rounds == 0 {
            return Err("Goal max_rounds must be explicitly positive".into());
        }
        let multiply = |value: u64| {
            value
                .checked_mul(rounds)
                .ok_or_else(|| "Goal allowance overflow".to_owned())
        };
        Ok(Self {
            elapsed_ms: multiply(budget.maximum_elapsed_ms())?,
            provider_attempts: multiply(budget.maximum_provider_attempts())?,
            tool_calls: multiply(budget.maximum_tool_calls())?,
            generated_records: multiply(budget.maximum_generated_records())?,
            generated_record_bytes: multiply(budget.maximum_generated_record_bytes())?,
        })
    }
}

/// Explicit application state operations. These do not confer a live lease.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
pub enum GoalAction {
    /// Create a new bounded Goal, replacing only an inactive, settled predecessor.
    Create {
        /// Fresh logical identity.
        id: DomainRequestId,
        /// Desired result.
        objective: String,
        /// Additional task constraints.
        constraints: String,
        /// Explicit maximum round allocation.
        max_rounds: u64,
    },
    /// Re-enable eligibility without resetting allocations or frozen budget.
    Resume {
        /// Exact current Goal identity.
        id: DomainRequestId,
    },
    /// Stop future allocation; an existing Turn may finish.
    Pause {
        /// Exact current Goal identity.
        id: DomainRequestId,
    },
    /// Stop future allocation; the Host also cancels the exact automatic input.
    Cancel {
        /// Exact current Goal identity.
        id: DomainRequestId,
    },
}

impl GoalState {
    /// Decodes the exact Goal codec from an opaque durable domain snapshot.
    ///
    /// # Errors
    /// Rejects a different codec or any invalid bounded Goal state.
    pub fn decode(snapshot: &rsi_agent_session_protocol::DomainSnapshot) -> Result<Self, String> {
        if snapshot.identity().id() != crate::GOAL_DOMAIN || snapshot.identity().version() != 1 {
            return Err("Goal domain codec is unavailable".into());
        }
        let state: Self = serde_json::from_value(snapshot.state().value().clone())
            .map_err(|error| error.to_string())?;
        state.validate()?;
        Ok(state)
    }
    /// Validates durable data at the Goal codec boundary.
    ///
    /// # Errors
    /// Rejects unbounded text, inconsistent allocation data or unverified completion.
    pub fn validate(&self) -> Result<(), String> {
        if let Some(goal) = &self.goal {
            goal.validate()?;
        }
        Ok(())
    }

    /// Pure explicit action. Draft create stages its first allocation in the baseline.
    ///
    /// # Errors
    /// Rejects stale identities, invalid creation, completed resume or exhausted allocation.
    pub fn apply(
        &mut self,
        action: GoalAction,
        budget: &TurnBudget,
        draft: bool,
    ) -> Result<(), String> {
        let cancelling = matches!(&action, GoalAction::Cancel { .. });
        match action {
            GoalAction::Create {
                id,
                objective,
                constraints,
                max_rounds,
            } => {
                if self.goal.as_ref().is_some_and(|goal| {
                    goal.id == id || goal.phase == GoalPhase::Active || goal.unsettled()
                }) {
                    return Err("settle and stop the previous Goal before replacing it".into());
                }
                let mut goal = Goal {
                    id,
                    objective,
                    constraints,
                    max_rounds,
                    turn_budget: budget.clone(),
                    allocated_rounds: 0,
                    phase: GoalPhase::Active,
                    reason: None,
                    reservation: None,
                    report: None,
                };
                goal.validate()?;
                if draft {
                    goal.reserve()?;
                    if let Some(reservation) = &mut goal.reservation {
                        reservation.request_id = None;
                    }
                }
                self.goal = Some(goal);
            }
            GoalAction::Resume { id } => {
                let goal = self.current_mut(&id)?;
                if goal.phase == GoalPhase::Completed {
                    return Err("a completed Goal cannot resume".into());
                }
                if !goal.unsettled() && goal.allocated_rounds == goal.max_rounds {
                    return Err("all Goal rounds have been allocated".into());
                }
                goal.phase = GoalPhase::Active;
                goal.reason = None;
            }
            GoalAction::Pause { id } | GoalAction::Cancel { id } => {
                let goal = self.current_mut(&id)?;
                if goal.phase != GoalPhase::Completed {
                    goal.phase = GoalPhase::Paused;
                    goal.reason = Some("Paused by the user".into());
                }
                if draft
                    && cancelling
                    && let Some(reservation) = goal
                        .reservation
                        .as_ref()
                        .filter(|reservation| reservation.settlement.is_none())
                {
                    let message = reservation.message_id.clone();
                    goal.settle(&message, RoundSettlement::Abandoned)?;
                }
            }
        }
        self.validate()
    }

    /// Resolves exact Goal identity, rejecting stale controller commands.
    ///
    /// # Errors
    /// Rejects an absent Goal or a different logical identity.
    pub fn current_mut(&mut self, id: &DomainRequestId) -> Result<&mut Goal, String> {
        self.goal
            .as_mut()
            .filter(|goal| &goal.id == id)
            .ok_or_else(|| "Goal identity changed or is absent".into())
    }
}

impl Goal {
    fn validate(&self) -> Result<(), String> {
        safe_text(&self.objective, 8192, false)?;
        safe_text(&self.constraints, 4096, true)?;
        GoalAllowance::new(self.max_rounds, &self.turn_budget)?;
        if self.allocated_rounds > self.max_rounds
            || (self.allocated_rounds == 0) != self.reservation.is_none()
        {
            return Err("invalid Goal allocation state".into());
        }
        if let Some(reason) = &self.reason {
            safe_text(reason, 4096, false)?;
        }
        if let Some(report) = &self.report {
            safe_text(&report.evidence, 4096, false)?;
        }
        if let Some(reservation) = &self.reservation {
            let expected = self.make_reservation(reservation.round)?;
            if reservation.round != self.allocated_rounds
                || reservation.message_id != expected.message_id
                || (reservation.request_id.is_none() && reservation.round != 1)
                || (reservation.request_id.is_some()
                    && reservation.request_id != expected.request_id)
                || reservation.input != expected.input
                || reservation.input_sha256 != expected.input_sha256
            {
                return Err("Goal reservation does not match frozen task and round".into());
            }
        }
        if self.phase == GoalPhase::Completed && !self.verified_completion() {
            return Err("completed Goal lacks a matching successful source Turn".into());
        }
        Ok(())
    }

    /// Whether the latest charged input has no canonical disposition yet.
    pub fn unsettled(&self) -> bool {
        self.reservation
            .as_ref()
            .is_some_and(|reservation| reservation.settlement.is_none())
    }

    /// Binds an unadmitted draft baseline, or allocates the next charged round.
    ///
    /// # Errors
    /// Rejects stopped, already bound unresolved, exhausted or invalid Goal state.
    pub fn reserve(&mut self) -> Result<(), String> {
        if self.phase == GoalPhase::Active
            && let Some(reservation) = &mut self.reservation
            && reservation.settlement.is_none()
            && reservation.request_id.is_none()
        {
            reservation.request_id =
                Some(round_request_id(&self.id, reservation.round, "reserve")?);
            return self.validate();
        }
        if self.phase != GoalPhase::Active
            || self.unsettled()
            || self.allocated_rounds >= self.max_rounds
        {
            return Err("Goal cannot allocate another round".into());
        }
        let round = self
            .allocated_rounds
            .checked_add(1)
            .ok_or("Goal round overflow")?;
        let reservation = self.make_reservation(round)?;
        self.allocated_rounds = round;
        self.reservation = Some(reservation);
        self.report = None;
        self.validate()
    }

    fn make_reservation(&self, round: u64) -> Result<GoalReservation, String> {
        if round == 0 || round > self.max_rounds {
            return Err("invalid Goal round".into());
        }
        let identity = serde_json::to_vec(&(&self.id, round)).map_err(|error| error.to_string())?;
        let message_id = MessageId::new(format!("goal-{:x}", Sha256::digest(identity)))
            .map_err(|error| error.to_string())?;
        let input = format!(
            "Continue Goal {}. Automatic round {round}/{}.\nObjective:\n{}\nConstraints:\n{}\nWork toward the objective. Report completion with concrete evidence using report_goal, or report blocked/pause with a reason. The report does not override a failed Turn.",
            self.id, self.max_rounds, self.objective, self.constraints
        );
        Ok(GoalReservation {
            round,
            message_id,
            request_id: Some(round_request_id(&self.id, round, "reserve")?),
            input_sha256: format!("{:x}", Sha256::digest(input.as_bytes())),
            input,
            settlement: None,
        })
    }

    /// Records a claim only; a complete report never completes a running Turn.
    ///
    /// # Errors
    /// Rejects unbounded evidence or the absence of an unresolved round.
    pub fn record_report(&mut self, report: GoalReport) -> Result<(), String> {
        safe_text(&report.evidence, 4096, false)?;
        if !self.unsettled() {
            return Err("Goal has no unresolved round".into());
        }
        self.report = Some(report);
        Ok(())
    }

    /// Settles one exact reservation; failure classes always override model reports.
    ///
    /// # Errors
    /// Rejects a different reservation, conflicting settlement or invalid resulting state.
    pub fn settle(
        &mut self,
        message: &MessageId,
        settlement: RoundSettlement,
    ) -> Result<(), String> {
        let reservation = self
            .reservation
            .as_mut()
            .filter(|reservation| &reservation.message_id == message)
            .ok_or("Goal reservation changed")?;
        if let Some(previous) = &reservation.settlement {
            return if previous == &settlement {
                Ok(())
            } else {
                Err("Goal settlement conflict".into())
            };
        }
        reservation.settlement = Some(settlement.clone());
        match settlement {
            RoundSettlement::Abandoned => {
                self.phase = GoalPhase::Paused;
                self.reason =
                    Some("Automatic allocation was abandoned before input acceptance".into());
            }
            RoundSettlement::Discarded => {
                self.phase = GoalPhase::Paused;
                self.reason = Some("Automatic input was discarded before claim".into());
            }
            RoundSettlement::Turn {
                outcome: RoundOutcome::Cancelled,
                ..
            } => {
                self.phase = GoalPhase::Paused;
                self.reason = Some("Automatic Turn was cancelled".into());
            }
            RoundSettlement::Turn {
                outcome: RoundOutcome::Completed,
                turn_id,
            } => {
                if let Some(report) = self
                    .report
                    .as_ref()
                    .filter(|report| report.source_turn == turn_id)
                {
                    self.phase = match report.kind {
                        GoalReportKind::Complete => GoalPhase::Completed,
                        GoalReportKind::Blocked => GoalPhase::Blocked,
                        GoalReportKind::Pause => GoalPhase::Paused,
                    };
                    self.reason = Some(report.evidence.clone());
                } else if self.phase == GoalPhase::Active
                    && self.allocated_rounds == self.max_rounds
                {
                    self.phase = GoalPhase::Blocked;
                    self.reason = Some("All automatic rounds have been allocated without a verified completion report".into());
                }
            }
            RoundSettlement::Turn { outcome, .. } => {
                self.phase = GoalPhase::Blocked;
                self.reason = Some(format!(
                    "Automatic Turn ended with {outcome:?}; its model report is unverified"
                ));
            }
        }
        self.validate()
    }

    fn verified_completion(&self) -> bool {
        matches!((&self.report, self.reservation.as_ref().and_then(|reservation| reservation.settlement.as_ref())),
            (Some(GoalReport { kind: GoalReportKind::Complete, source_turn, .. }),
             Some(RoundSettlement::Turn { turn_id, outcome: RoundOutcome::Completed })) if source_turn == turn_id)
    }
}

impl GoalReservation {
    /// Copies the complete exact input into the Kernel's bounded admission value.
    pub fn input(&self, owner: &DomainRequestId) -> rsi_agent_session_protocol::ContinuationInput {
        rsi_agent_session_protocol::ContinuationInput {
            owner: owner.clone(),
            round: self.round,
            message_id: self.message_id.clone(),
            text: self.input.clone(),
        }
    }
}

/// Stable logical identity for one reserve or settlement, including after a lost reply.
///
/// # Errors
/// Rejects zero rounds or an unsupported operation name.
pub fn round_request_id(
    owner: &DomainRequestId,
    round: u64,
    operation: &str,
) -> Result<DomainRequestId, String> {
    if round == 0 || !matches!(operation, "reserve" | "settle") {
        return Err("invalid Goal round operation".into());
    }
    let identity =
        serde_json::to_vec(&(owner, round, operation)).map_err(|error| error.to_string())?;
    DomainRequestId::new(format!("goal-{operation}-{:x}", Sha256::digest(identity)))
        .map_err(|error| error.to_string())
}

pub(crate) fn safe_text(text: &str, maximum: usize, empty: bool) -> Result<(), String> {
    if text.len() > maximum
        || (!empty && text.trim().is_empty())
        || text
            .chars()
            .any(|character| character.is_control() && !matches!(character, '\n' | '\t'))
    {
        return Err(format!(
            "Goal text must be safe UTF-8 within {maximum} bytes"
        ));
    }
    Ok(())
}
