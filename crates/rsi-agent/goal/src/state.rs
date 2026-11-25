//! Bounded semantic codec and effect-free state transitions.

use rsi_agent_session_protocol::{
    ContinuationInput, DomainRequestId, MessageId, TurnId, TurnOutcome,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// The single current Goal; replacing a Goal does not rewrite its control history.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
pub struct GoalState {
    /// Absent until explicitly created.
    pub goal: Option<Goal>,
}

/// Current task specification and its charged progress.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
pub struct Goal {
    /// Caller-chosen logical identity, distinct for each newly created Goal.
    pub id: DomainRequestId,
    /// Requested result, limited to 8 KiB UTF-8.
    pub objective: String,
    /// Task constraints, limited to 4 KiB UTF-8.
    pub constraints: String,
    /// Explicit positive maximum automatic parent rounds.
    pub max_rounds: u64,
    /// Allocations charged atomically with message acceptance, without refunds.
    pub allocated_rounds: u64,
    /// Durable task phase; independent of live execution authorization.
    pub phase: GoalPhase,
    /// Bounded human-readable stopping explanation.
    pub reason: Option<String>,
    /// Stable code supplied by the policy that explicitly blocked this Goal.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub block_code: Option<String>,
    /// Latest allocation only; older allocations remain in durable controls.
    pub reservation: Option<GoalReservation>,
    /// Latest model claim, including its source Turn even when unverified.
    pub report: Option<GoalReport>,
    /// Authority declaring explicit completion, independent of any model report.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completed_by: Option<GoalCompleter>,
}

/// Source of an explicit completion decision.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
pub enum GoalCompleter {
    /// The Host operator requested completion.
    Owner,
    /// A participant with access to the addressed Session requested completion.
    Participant,
    /// A trusted application identifies the Cordis entry invoking the public Goal service.
    Plugin {
        /// Loader package or module name.
        name: String,
    },
}

impl GoalCompleter {
    fn validate(&self) -> Result<(), String> {
        if let Self::Plugin { name } = self
            && (name.is_empty() || name.len() > 256 || !name.bytes().all(|b| b.is_ascii_graphic()))
        {
            return Err("invalid completing plugin identity".into());
        }
        Ok(())
    }
}

/// Durable phase, never a live lease indicator.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(test, derive(schemars::JsonSchema))]
pub enum GoalPhase {
    /// Eligible for an explicitly armed controller to allocate another round.
    Active,
    /// Stopped by explicit pause, cancellation or model pause request.
    Paused,
    /// Execution failure, model blocker or allocation cap requires user action.
    Blocked,
    /// An explicit completion was accepted or a reported completion was verified.
    Completed,
}

/// Model report vocabulary; it contains no create, resume or budget operation.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(test, derive(schemars::JsonSchema))]
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
#[cfg_attr(test, derive(schemars::JsonSchema))]
pub struct GoalReport {
    /// Requested disposition.
    pub kind: GoalReportKind,
    /// Nonempty bounded evidence or reason, at most 4 KiB UTF-8.
    pub evidence: String,
    /// Exact Turn containing the report Tool result.
    pub source_turn: TurnId,
}

/// Identity, evidence and disposition of one charged round.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
pub struct GoalReservation {
    /// One-based allocation number.
    pub round: u64,
    /// Reissues of this charged round after an edit invalidated unclaimed input.
    #[serde(default, skip_serializing_if = "initial_attempt")]
    pub attempt: u64,
    /// Deterministic identity bound to Goal and round.
    pub message_id: MessageId,
    /// Deterministic internal reserve receipt, absent only for the staged first baseline.
    pub request_id: Option<DomainRequestId>,
    /// SHA-256 of the exact UTF-8 input.
    pub input_sha256: String,
    /// The current specification was edited after this round was allocated.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub edited: bool,
    /// Absent while acceptance, claim or terminal outcome remains unresolved.
    pub settlement: Option<RoundSettlement>,
}

/// Newly prepared Kernel input with its exact reservation identity and digest.
#[derive(Debug)]
pub struct PreparedGoalRound {
    attempt: u64,
    /// Complete input moved to the Kernel upon admission.
    pub input: ContinuationInput,
    /// Deterministic identity for the reserve command.
    pub request_id: DomainRequestId,
    /// SHA-256 of the input text for receipt reconciliation.
    pub input_sha256: String,
}

/// Small outcome retained with the latest reservation.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
pub enum RoundSettlement {
    /// Explicitly abandoned before acceptance; no mailbox entry or refund is invented.
    Abandoned,
    /// Canonically discarded before claim; the allocation remains charged.
    Discarded,
    /// A queued continuation lost its revision before claim. When recorded by
    /// the armed controller for an edit, its charged round can be reissued.
    ContinuationDisarmed,
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
#[cfg_attr(test, derive(schemars::JsonSchema))]
pub enum RoundOutcome {
    /// Successful natural model termination.
    Completed,
    /// Input preparation declined the request.
    Blocked,
    /// User or controller cancellation.
    Cancelled,
    /// Runtime or provider failure.
    Failed,
    /// Partial output followed by failure.
    PartialFailed,
    /// Unknown external effect after recovery.
    Interrupted,
    /// The model reached its requested output token limit.
    MaxTokens,
}

impl From<&TurnOutcome> for RoundOutcome {
    fn from(outcome: &TurnOutcome) -> Self {
        match outcome {
            TurnOutcome::Completed => Self::Completed,
            TurnOutcome::Blocked => Self::Blocked,
            TurnOutcome::Cancelled => Self::Cancelled,
            TurnOutcome::Failed { .. } => Self::Failed,
            TurnOutcome::PartialFailed { .. } => Self::PartialFailed,
            TurnOutcome::Interrupted { .. } => Self::Interrupted,
            TurnOutcome::MaxTokens => Self::MaxTokens,
        }
    }
}

/// Default maximum automatic parent rounds.
pub const DEFAULT_MAX_ROUNDS: u64 = 256;
/// Largest integer represented exactly by all supported clients.
pub const MAXIMUM_GOAL_ROUNDS: u64 = 9_007_199_254_740_991;

/// Validates a positive safe-integer round count.
///
/// # Errors
/// Rejects zero and values above the cross-client integer bound.
pub fn validate_max_rounds(rounds: u64) -> Result<(), String> {
    if rounds == 0 || rounds > MAXIMUM_GOAL_ROUNDS {
        return Err("Goal max_rounds must be a positive safe integer".into());
    }
    Ok(())
}

/// Explicit application state operations. These do not confer a live lease.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
pub enum GoalAction {
    /// Create a new bounded Goal in an empty slot or after explicit completion.
    Create {
        /// Fresh logical identity.
        id: DomainRequestId,
        /// Desired result.
        objective: String,
        /// Additional task constraints.
        constraints: String,
        /// Explicit maximum round allocation.
        #[serde(default)]
        max_rounds: Option<u64>,
    },
    /// Update the objective, the allocation limit, or both without resetting progress.
    Edit {
        /// Exact current Goal identity.
        id: DomainRequestId,
        /// Replacement objective.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        objective: Option<String>,
        /// Replacement positive round limit.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        max_rounds: Option<u64>,
    },
    /// Remove the current Goal and retain its historical events.
    Clear {
        /// Exact current Goal identity.
        id: DomainRequestId,
    },
    /// Re-enable eligibility without resetting allocated rounds.
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
    /// A trusted policy stops future allocation at its observed revision.
    Block {
        /// Exact current Goal identity.
        id: DomainRequestId,
        /// Stable lower-kebab-case category.
        code: String,
        /// Trimmed explanation displayed to the user.
        message: String,
    },
    /// Mark a live, paused or blocked Goal complete and stop further allocations.
    Complete {
        /// Exact current Goal identity.
        id: DomainRequestId,
        /// Decision source established by the calling application.
        source: GoalCompleter,
    },
}

/// Original application publication time in milliseconds, representable by all clients.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(try_from = "u64")]
pub struct GoalPublicationTime(u64);

impl TryFrom<u64> for GoalPublicationTime {
    type Error = &'static str;

    fn try_from(value: u64) -> Result<Self, Self::Error> {
        if value > (1 << 53) - 1 {
            return Err("Goal publication time exceeds the client integer range");
        }
        Ok(Self(value))
    }
}

/// Arguments bound by the native application command receipt.
#[derive(Deserialize)]
pub(crate) struct GoalCommandArguments {
    #[serde(rename = "published_at_ms")]
    _published_at: Option<GoalPublicationTime>,
    #[serde(flatten)]
    pub action: GoalAction,
}

#[test]
fn published_goal_action_checks_authored_time_and_retains_strict_action_fields() {
    let action = serde_json::json!({"action":"pause","id":"goal","published_at_ms":0});
    assert!(matches!(
        GoalCommandArguments::deserialize(&action).unwrap().action,
        GoalAction::Pause { .. }
    ));
    let mut bad = action.clone();
    bad["published_at_ms"] = serde_json::json!(1u64 << 53);
    assert!(GoalCommandArguments::deserialize(&bad).is_err());
    let mut bad = action;
    bad["unrecognized"] = true.into();
    assert!(GoalCommandArguments::deserialize(&bad).is_err());
}

impl GoalState {
    /// Decodes the exact Goal codec from an opaque durable domain snapshot.
    ///
    /// # Errors
    /// Rejects a different codec or any invalid bounded Goal state.
    pub fn decode(snapshot: &rsi_agent_session_protocol::DomainSnapshot) -> Result<Self, String> {
        if snapshot.identity().id() != crate::GOAL_DOMAIN
            || snapshot.identity().version() != crate::GOAL_CODEC_VERSION
        {
            return Err("Goal domain codec is unavailable".into());
        }
        let state =
            Self::deserialize(snapshot.state().value()).map_err(|error| error.to_string())?;
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

    /// Pure explicit action. Creation never allocates an automatic round.
    ///
    /// # Errors
    /// Rejects stale identities, invalid creation, completed resume or exhausted allocation.
    pub fn apply(
        &mut self,
        action: GoalAction,
        default_max_rounds: u64,
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
                if self
                    .goal
                    .as_ref()
                    .is_some_and(|goal| goal.id == id || goal.phase != GoalPhase::Completed)
                {
                    return Err(
                        "complete or clear the previous Goal before creating another".into(),
                    );
                }
                let goal = Goal {
                    id,
                    objective,
                    constraints,
                    max_rounds: max_rounds.unwrap_or(default_max_rounds),
                    allocated_rounds: 0,
                    phase: GoalPhase::Active,
                    reason: None,
                    block_code: None,
                    reservation: None,
                    report: None,
                    completed_by: None,
                };
                goal.validate()?;
                self.goal = Some(goal);
                return Ok(());
            }
            GoalAction::Clear { id } => {
                self.current_mut(&id)?;
                self.goal = None;
            }
            GoalAction::Edit {
                id,
                objective,
                max_rounds,
            } => {
                let goal = self.current_mut(&id)?;
                if objective.is_none() && max_rounds.is_none() {
                    return Err("Goal edit requires an objective or maximum rounds".into());
                }
                let previous_limit =
                    max_rounds.map(|value| std::mem::replace(&mut goal.max_rounds, value));
                let previous_objective =
                    objective.map(|value| std::mem::replace(&mut goal.objective, value));
                let previous_report = if goal.unsettled() {
                    goal.report.take()
                } else {
                    None
                };
                let previous_edit = goal
                    .reservation
                    .as_mut()
                    .map(|r| std::mem::replace(&mut r.edited, true));
                if let Err(error) = goal.validate() {
                    if let Some(value) = previous_limit {
                        goal.max_rounds = value;
                    }
                    if let Some(value) = previous_objective {
                        goal.objective = value;
                    }
                    if let Some(value) = previous_report {
                        goal.report = Some(value);
                    }
                    if let Some((reservation, edited)) =
                        goal.reservation.as_mut().zip(previous_edit)
                    {
                        reservation.edited = edited;
                    }
                    return Err(error);
                }
                return Ok(());
            }
            GoalAction::Resume { id } => {
                let goal = self.current_mut(&id)?;
                if goal.phase == GoalPhase::Completed {
                    return Err("a completed Goal cannot resume".into());
                }
                if !goal.has_round_capacity() {
                    return Err("all Goal rounds have been allocated".into());
                }
                goal.phase = GoalPhase::Active;
                goal.reason = None;
                goal.block_code = None;
            }
            GoalAction::Block { id, code, message } => {
                let goal = self.current_mut(&id)?;
                if goal.phase != GoalPhase::Active {
                    return Err("only an active Goal can be blocked".into());
                }
                goal.validate()?;
                goal.validate_block(&code, &message)?;
                goal.phase = GoalPhase::Blocked;
                goal.reason = Some(message);
                goal.block_code = Some(code);
                return Ok(());
            }
            GoalAction::Complete { id, source } => {
                source.validate()?;
                let goal = self.current_mut(&id)?;
                if goal.phase == GoalPhase::Completed {
                    return Err("the Goal is already complete".into());
                }
                goal.phase = GoalPhase::Completed;
                goal.reason = None;
                goal.block_code = None;
                goal.completed_by = Some(source);
            }
            GoalAction::Pause { id } | GoalAction::Cancel { id } => {
                let goal = self.current_mut(&id)?;
                if goal.phase == GoalPhase::Active {
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
    /// Policy reason used when no further automatic round can be allocated.
    pub fn round_limit_reason(&self) -> (&'static str, String) {
        (
            "round-limit",
            format!(
                "Goal reached its configured limit of {} rounds.",
                self.max_rounds
            ),
        )
    }

    fn validate_block(&self, code: &str, message: &str) -> Result<(), String> {
        if !code.as_bytes().first().is_some_and(u8::is_ascii_lowercase)
            || !code.split('-').all(|part| {
                !part.is_empty()
                    && part
                        .bytes()
                        .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit())
            })
        {
            return Err("Goal block code must be lower-kebab-case".into());
        }
        safe_text(message, 4096, false)?;
        if message.trim() != message {
            return Err("Goal block message must be trimmed".into());
        }
        // The other bounded fields use under 48 KiB even with maximal JSON escaping.
        // Test exceptionally long policy codes against the actual view envelope.
        if code.len() > rsi_agent_session_protocol::MAXIMUM_PROJECTION_VALUE_BYTES / 4 {
            let mut value = serde_json::json!({"goal":self});
            let goal = &mut value["goal"];
            goal["phase"] = "blocked".into();
            goal["reason"] = message.into();
            goal["block_code"] = code.into();
            if self.unsettled() {
                // Space for a later model claim and settlement of the already admitted round.
                let turn = "t".repeat(rsi_agent_session_protocol::MAXIMUM_AGENT_IDENTIFIER_BYTES);
                goal["report"] = serde_json::json!({
                    "kind":"complete", "evidence":"\\".repeat(4096), "source_turn":turn
                });
                goal["reservation"]["settlement"] = serde_json::json!({
                    "type":"turn", "outcome":"partial_failed", "turn_id":turn
                });
            }
            rsi_agent_session_protocol::ProjectionValue::new(value).map_err(|e| e.to_string())?;
        }
        Ok(())
    }

    fn validate(&self) -> Result<(), String> {
        self.validate_fields()?;
        let Some(reservation) = &self.reservation else {
            return Ok(());
        };
        let digest = &reservation.input_sha256;
        if digest.len() != 64
            || !digest
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        {
            return Err("invalid Goal input digest".into());
        }
        let round = reservation.round;
        let valid_request = match &reservation.request_id {
            Some(id) => *id == round_request_id(&reservation.message_id, "reserve")?,
            None => round == 1 && reservation.attempt == 0,
        };
        if round != self.allocated_rounds
            || !valid_request
            || reservation.message_id
                != self.round_message_id(round, reservation.attempt, digest)?
            || (reservation.settlement == Some(RoundSettlement::ContinuationDisarmed)
                && !reservation.edited)
            || (!reservation.edited
                && *digest != format!("{:x}", Sha256::digest(self.round_text(round).as_bytes())))
        {
            return Err("Goal reservation does not match frozen task and round".into());
        }
        Ok(())
    }

    fn validate_fields(&self) -> Result<(), String> {
        safe_text(&self.objective, 8192, false)?;
        safe_text(&self.constraints, 4096, true)?;
        validate_max_rounds(self.max_rounds)?;
        if self.allocated_rounds > MAXIMUM_GOAL_ROUNDS
            || (self.allocated_rounds > self.max_rounds
                && self.reservation.as_ref().is_none_or(|r| !r.edited))
            || (self.allocated_rounds == 0) != self.reservation.is_none()
        {
            return Err("invalid Goal allocation state".into());
        }
        if let Some(code) = &self.block_code {
            let message = self
                .reason
                .as_deref()
                .filter(|_| self.phase == GoalPhase::Blocked)
                .ok_or("an explicit blocker requires a blocked Goal with a reason")?;
            self.validate_block(code, message)?;
        } else if let Some(reason) = &self.reason {
            safe_text(reason, 4096, false)?;
        }
        if let Some(report) = &self.report {
            safe_text(&report.evidence, 4096, false)?;
        }
        if let Some(source) = &self.completed_by {
            if self.phase != GoalPhase::Completed {
                return Err("explicit completion requires a completed Goal".into());
            }
            source.validate()?;
        } else if self.phase == GoalPhase::Completed && !self.verified_completion() {
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

    fn reissue(&self) -> Option<&GoalReservation> {
        self.reservation
            .as_ref()
            .filter(|r| r.settlement == Some(RoundSettlement::ContinuationDisarmed))
    }

    /// Whether a new round or the revised input of an unclaimed round fits the cap.
    pub fn has_round_capacity(&self) -> bool {
        self.allocated_rounds < self.max_rounds
            || (self.allocated_rounds == self.max_rounds && self.reissue().is_some())
    }

    /// Proposes the next allocation for atomic publication with its input.
    ///
    /// # Errors
    /// Rejects stopped, already bound unresolved, exhausted or invalid Goal state.
    pub fn reserve(&mut self) -> Result<String, String> {
        let prepared = self.next_reservation()?;
        let input = prepared.input;
        self.allocated_rounds = input.round;
        self.reservation = Some(GoalReservation {
            round: input.round,
            attempt: prepared.attempt,
            message_id: input.message_id,
            request_id: Some(prepared.request_id),
            input_sha256: prepared.input_sha256,
            edited: false,
            settlement: None,
        });
        self.report = None;
        Ok(input.text)
    }

    /// Prepares one frozen input without modifying the current allocation or model report.
    ///
    /// # Errors
    /// Rejects invalid Goal fields, a stopped Goal, an unsettled input or an exhausted budget.
    pub fn next_reservation(&self) -> Result<PreparedGoalRound, String> {
        if self.phase != GoalPhase::Active || self.unsettled() || !self.has_round_capacity() {
            return Err("Goal cannot allocate another round".into());
        }
        self.validate_fields()?;
        let (round, attempt) = match self.reissue() {
            Some(previous) => (
                previous.round,
                previous
                    .attempt
                    .checked_add(1)
                    .ok_or("Goal attempt sequence exhausted")?,
            ),
            None => (self.allocated_rounds + 1, 0),
        };
        self.make_reservation(round, attempt)
    }

    fn round_message_id(
        &self,
        round: u64,
        attempt: u64,
        input_sha256: &str,
    ) -> Result<MessageId, String> {
        let identity = serde_json::to_vec(&(&self.id, round, attempt, input_sha256))
            .map_err(|error| error.to_string())?;
        MessageId::new(format!("goal-{:x}", Sha256::digest(identity)))
            .map_err(|error| error.to_string())
    }

    fn round_text(&self, round: u64) -> String {
        format!(
            "Continue Goal {}. Automatic round {round}/{}.\nObjective:\n{}\nConstraints:\n{}\nCheck current results. Verify the whole objective before marking it complete. Leave unfinished work active. Follow the configured Goal tool policy for blockers.",
            self.id, self.max_rounds, self.objective, self.constraints
        )
    }

    fn make_reservation(&self, round: u64, attempt: u64) -> Result<PreparedGoalRound, String> {
        let input = self.round_text(round);
        let input_sha256 = format!("{:x}", Sha256::digest(input.as_bytes()));
        let message_id = self.round_message_id(round, attempt, &input_sha256)?;
        Ok(PreparedGoalRound {
            attempt,
            request_id: round_request_id(&message_id, "reserve")?,
            input_sha256,
            input: ContinuationInput {
                owner: self.id.clone(),
                round,
                message_id,
                text: input,
            },
        })
    }

    /// Records a claim only; a complete report never completes a running Turn.
    ///
    /// # Errors
    /// Rejects unbounded evidence or the absence of an unresolved round.
    pub fn record_report(&mut self, report: GoalReport) -> Result<(), String> {
        safe_text(&report.evidence, 4096, false)?;
        if self
            .reservation
            .as_ref()
            .is_none_or(|r| r.settlement.is_some() || r.edited)
        {
            return Err("Goal has no unresolved round for the current specification".into());
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
        let edited = reservation.edited;
        if settlement == RoundSettlement::ContinuationDisarmed
            && (!edited || self.phase != GoalPhase::Active)
        {
            return Err("reissuing input requires an edited active Goal".into());
        }
        reservation.settlement = Some(settlement.clone());
        if self.completed_by.is_some()
            || self.block_code.is_some()
            || (self.phase == GoalPhase::Paused
                && matches!(settlement, RoundSettlement::Turn { .. })
                && !self.verified_completion())
        {
            return self.validate();
        }
        match settlement {
            RoundSettlement::ContinuationDisarmed => {}
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
                    .filter(|report| !edited && report.source_turn == turn_id)
                {
                    self.phase = match report.kind {
                        GoalReportKind::Complete => GoalPhase::Completed,
                        GoalReportKind::Blocked => GoalPhase::Blocked,
                        GoalReportKind::Pause => GoalPhase::Paused,
                    };
                    self.reason = Some(report.evidence.clone());
                } else if self.phase == GoalPhase::Active
                    && self.allocated_rounds >= self.max_rounds
                {
                    self.phase = GoalPhase::Blocked;
                    let (code, reason) = self.round_limit_reason();
                    self.reason = Some(reason);
                    self.block_code = Some(code.into());
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

    /// Returns the model report only after its exact source Turn canonically completed.
    /// All report dispositions require successful settlement; failed claims remain in `report`.
    pub fn verified_report(&self) -> Option<&GoalReport> {
        let report = self.report.as_ref()?;
        match self.reservation.as_ref()?.settlement.as_ref()? {
            RoundSettlement::Turn {
                turn_id,
                outcome: RoundOutcome::Completed,
            } if turn_id == &report.source_turn => Some(report),
            _ => None,
        }
    }

    fn verified_completion(&self) -> bool {
        self.verified_report()
            .is_some_and(|report| report.kind == GoalReportKind::Complete)
    }
}

fn initial_attempt(attempt: &u64) -> bool {
    *attempt == 0
}

/// Stable receipt identity for a single frozen message attempt.
///
/// # Errors
/// Rejects an unsupported operation or an oversized caller-supplied identity.
pub fn round_request_id(message: &MessageId, operation: &str) -> Result<DomainRequestId, String> {
    if !matches!(operation, "reserve" | "settle") {
        return Err("invalid Goal round operation".into());
    }
    DomainRequestId::new(format!("{operation}-{message}")).map_err(|error| error.to_string())
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

#[cfg(test)]
rsi_persistence_testkit::payload_contract!(
    persistence_goal,
    GoalState,
    crate::GOAL_CODEC_VERSION,
    format!(
        "tests/fixtures/persistence/goal-v{}.json",
        crate::GOAL_CODEC_VERSION
    ),
    {
        let mut state = GoalState::default();
        let mut samples = vec![state.clone()];
        state
            .apply(
                GoalAction::Create {
                    id: DomainRequestId::new("fixture").unwrap(),
                    objective: "Verify 中🦀".into(),
                    constraints: "synthetic".into(),
                    max_rounds: Some(3),
                },
                3,
                false,
            )
            .unwrap();
        samples.push(state.clone());
        state.goal.as_mut().unwrap().reserve().unwrap();
        samples.push(state.clone());
        let turn = TurnId::new("turn").unwrap();
        let goal = state.goal.as_mut().unwrap();
        let message = goal.reservation.as_ref().unwrap().message_id.clone();
        goal.report = Some(GoalReport {
            kind: GoalReportKind::Complete,
            evidence: "validated".into(),
            source_turn: turn.clone(),
        });
        goal.settle(
            &message,
            RoundSettlement::Turn {
                turn_id: turn,
                outcome: RoundOutcome::Completed,
            },
        )
        .unwrap();
        samples.push(state);

        let mut blocked = samples[1].clone();
        blocked
            .apply(
                GoalAction::Block {
                    id: DomainRequestId::new("fixture").unwrap(),
                    code: "needs-input".into(),
                    message: "Choose a workspace".into(),
                },
                3,
                false,
            )
            .unwrap();
        samples.push(blocked);

        let mut edited = samples[1].clone();
        edited.goal.as_mut().unwrap().reserve().unwrap();
        edited
            .apply(
                GoalAction::Edit {
                    id: DomainRequestId::new("fixture").unwrap(),
                    objective: Some("Review the revised objective".into()),
                    max_rounds: Some(20),
                },
                3,
                false,
            )
            .unwrap();
        samples.push(edited.clone());
        let queued = edited.goal.as_mut().unwrap();
        let message = queued.reservation.as_ref().unwrap().message_id.clone();
        queued
            .settle(&message, RoundSettlement::ContinuationDisarmed)
            .unwrap();
        samples.push(edited.clone());
        edited.goal.as_mut().unwrap().reserve().unwrap();
        samples.push(edited);

        for source in [
            GoalCompleter::Owner,
            GoalCompleter::Participant,
            GoalCompleter::Plugin {
                name: "@fixture/goal".into(),
            },
        ] {
            let mut explicit = samples[1].clone();
            explicit
                .apply(
                    GoalAction::Complete {
                        id: DomainRequestId::new("fixture").unwrap(),
                        source,
                    },
                    3,
                    false,
                )
                .unwrap();
            samples.push(explicit);
        }

        let samples: Vec<GoalState> = samples;
        for sample in &samples {
            sample.validate().unwrap();
        }
        samples
    },
    |path, version| {
        use rsi_agent_session_protocol::{DomainIdentity, DomainSnapshot, DomainStateValue};
        let fixture: serde_json::Value =
            serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
        for sample in fixture["payload"]["samples"].as_array().unwrap() {
            let value = serde_json::from_str(sample.as_str().unwrap()).unwrap();
            let snapshot = DomainSnapshot::new(
                DomainIdentity::new(crate::GOAL_DOMAIN, version).unwrap(),
                DomainStateValue::new(value).unwrap(),
            );
            assert_eq!(
                GoalState::decode(&snapshot).unwrap_err(),
                "Goal domain codec is unavailable"
            );
        }
    }
);
