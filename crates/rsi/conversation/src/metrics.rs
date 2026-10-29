//! Ordered usage reduction without history retention or model execution.

use rsi_agent_session_protocol::{
    EffectId, ModelEventPurpose, OutputBudgetTable, SessionFact, SessionFactBody, TurnId,
};
use rsi_ai_protocol::{
    LanguageEvent, ModelRef, PreparedLanguageSettings, ReasoningEffortId, TokenUsage,
};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// Recorded whole-Session timing; counts identify the evidence behind each sum.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SessionTiming {
    pub turns: u64,
    pub timed_turns: u64,
    pub total_ms: u64,
    pub timed_attempts: u64,
    pub model_ms: u64,
    pub first_token_attempts: u64,
    pub first_token_ms: u64,
    pub decode_ms: u64,
    pub decode_output_tokens: u64,
}

/// Reported facts of one completed or failed model attempt.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AttemptMetrics {
    pub turn: TurnId,
    pub effect: EffectId,
    pub model: ModelRef,
    pub config_generation: u64,
    pub reasoning_effort: Option<ReasoningEffortId>,
    pub usage: Option<TokenUsage>,
    pub elapsed_ms: Option<u64>,
    pub failed: bool,
    pub terminal_seq: u64,
}

/// Last finished ordinary request's observed input and captured capacity.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ContextUsage {
    description: rsi_ai_protocol::LanguageModelDescription,
    max_output_tokens: Option<u32>,
    input_tokens: u64,
    finished_seq: u64,
    #[serde(skip)]
    input_capacity_tokens: u32,
}
impl<'de> Deserialize<'de> for ContextUsage {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Wire {
            description: rsi_ai_protocol::LanguageModelDescription,
            max_output_tokens: Option<u32>,
            input_tokens: u64,
            finished_seq: u64,
        }
        let wire = Wire::deserialize(deserializer)?;
        Self::new(
            wire.description,
            wire.max_output_tokens,
            wire.input_tokens,
            wire.finished_seq,
        )
        .map_err(serde::de::Error::custom)
    }
}
impl ContextUsage {
    fn new(
        description: rsi_ai_protocol::LanguageModelDescription,
        max_output_tokens: Option<u32>,
        input_tokens: u64,
        finished_seq: u64,
    ) -> Result<Self, &'static str> {
        let input_capacity_tokens = description
            .profile()
            .input_capacity_tokens(max_output_tokens)
            .map_err(|_| "invalid captured request output capacity")?;
        Ok(Self {
            description,
            max_output_tokens,
            input_tokens,
            finished_seq,
            input_capacity_tokens,
        })
    }
    pub fn description(&self) -> &rsi_ai_protocol::LanguageModelDescription {
        &self.description
    }
    pub const fn input_tokens(&self) -> u64 {
        self.input_tokens
    }
    pub const fn finished_seq(&self) -> u64 {
        self.finished_seq
    }
    pub fn input_capacity(&self) -> u32 {
        self.input_capacity_tokens
    }
}

/// One Session's totals at a durable Fact cursor; child and inherited Facts are excluded by acquisition.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SessionMetrics {
    pub timing: SessionTiming,
    pub through_seq: u64,
    pub attempts: u64,
    pub reported_attempts: u64,
    pub failed_attempts: u64,
    pub tokens: TokenUsage,
    pub last_attempt: Option<AttemptMetrics>,
    pub last_context: Option<ContextUsage>,
    pub configured_cost: crate::ConfiguredCost,
}

impl SessionMetrics {
    /// Validates counters and their durable cursor relationships at API ingress.
    /// # Errors
    /// Rejects inconsistent counters, cursors or configured cost totals.
    pub fn validate(&self) -> Result<(), &'static str> {
        self.configured_cost.validate(self.attempts)?;
        if self.timing.timed_turns > self.timing.turns
            || self.timing.timed_attempts > self.attempts
            || self.timing.first_token_attempts > self.timing.timed_attempts
            || self.timing.decode_output_tokens > self.tokens.output_tokens()
            || self.timing.first_token_ms > self.timing.model_ms
            || self.timing.decode_ms > self.timing.model_ms
            || (self.timing.timed_turns == 0 && self.timing.total_ms != 0)
            || (self.timing.timed_attempts == 0 && self.timing.model_ms != 0)
            || (self.timing.first_token_attempts == 0 && self.timing.first_token_ms != 0)
            || (self.timing.decode_ms == 0 && self.timing.decode_output_tokens != 0)
        {
            return Err("invalid Session timing counters");
        }
        if self.reported_attempts > self.attempts
            || self.failed_attempts > self.attempts
            || self.last_attempt.as_ref().is_some_and(|attempt| {
                attempt.config_generation == 0
                    || attempt.terminal_seq == 0
                    || attempt.terminal_seq > self.through_seq
            })
            || self.last_context.as_ref().is_some_and(|context| {
                context.finished_seq == 0 || context.finished_seq > self.through_seq
            })
        {
            return Err("invalid Session metrics counters or cursor");
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Serialize)]
struct OpenAttempt {
    description: Option<rsi_ai_protocol::LanguageModelDescription>,
    turn: TurnId,
    effect: EffectId,
    model: ModelRef,
    config_generation: u64,
    settings: Option<PreparedLanguageSettings>,
    started_ms: Option<u64>,
    first_token_ms: Option<u64>,
    usage: Option<TokenUsage>,
    purpose: ModelEventPurpose,
    price_quote: Option<rsi_agent_session_protocol::PriceQuote>,
}

/// Bounded reducer of one Session's contiguous durable Fact stream.
#[derive(Clone, Debug, Default, Serialize)]
pub struct MetricsReducer {
    summary: SessionMetrics,
    open: Option<OpenAttempt>,
    output_budgets: OutputBudgetTable,
    turn_starts: BTreeMap<TurnId, u64>,
}

impl MetricsReducer {
    /// Starts a reduction with the immutable request caps of this Session.
    pub fn new(output_budgets: OutputBudgetTable) -> Self {
        Self {
            output_budgets,
            ..Self::default()
        }
    }
    /// Conservative cache charge for bounded scalar/vector/string state, without Facts.
    pub fn retained_bytes(&self) -> usize {
        std::mem::size_of::<Self>()
            + serde_json::to_vec(self)
                .map_or(usize::MAX / 2, |bytes| bytes.len())
                .saturating_mul(2)
    }
    pub fn summary(&self) -> &SessionMetrics {
        &self.summary
    }

    /// Applies a Fact atomically. Replayed cursors are inert; gaps and impossible grammar fail.
    /// # Errors
    /// Rejects a sequence gap, invalid request lifecycle or counter overflow.
    pub fn observe(&mut self, fact: &SessionFact) -> Result<(), &'static str> {
        if fact.seq() <= self.summary.through_seq {
            return Ok(());
        }
        if self.summary.through_seq.checked_add(1) != Some(fact.seq()) {
            return Err("metrics Fact sequence gap");
        }
        if !matches!(
            fact.body(),
            SessionFactBody::ModelIntent { .. }
                | SessionFactBody::ModelStarted { .. }
                | SessionFactBody::ModelAttemptSettled { .. }
                | SessionFactBody::TurnTerminal { .. }
                | SessionFactBody::TurnAccepted { .. }
                | SessionFactBody::MessageTurnAccepted { .. }
                | SessionFactBody::ImageRequested { .. }
        ) {
            self.summary.through_seq = fact.seq();
            return Ok(());
        }
        let mut next = self.clone();
        next.apply(fact)?;
        next.summary.through_seq = fact.seq();
        *self = next;
        Ok(())
    }

    fn model_event(
        &mut self,
        turn_id: &TurnId,
        effect_id: &EffectId,
        purpose: ModelEventPurpose,
        event: &LanguageEvent,
        seq: u64,
        timestamp_ms: u64,
    ) -> Result<(), &'static str> {
        let open = self.exact(turn_id, effect_id)?;
        if open.purpose != purpose {
            return Err("metrics model purpose mismatch");
        }
        if let LanguageEvent::ContentDelta { delta, .. } = event {
            let text = match delta {
                rsi_ai_protocol::ContentDelta::Text(text)
                | rsi_ai_protocol::ContentDelta::Reasoning(text)
                | rsi_ai_protocol::ContentDelta::ToolArguments(text) => text,
            };
            if !text.is_empty() {
                open.first_token_ms.get_or_insert(timestamp_ms);
            }
        } else if let LanguageEvent::Usage { usage } = event {
            if open.usage.replace(*usage).is_some() {
                return Err("duplicate attempt usage");
            }
            let quote = open.price_quote.clone();
            self.summary.configured_cost.usage(quote.as_ref(), *usage)?;
            let first = self.summary.reported_attempts == 0;
            self.summary.tokens = if first {
                *usage
            } else {
                sum(self.summary.tokens, *usage)?
            };
            self.summary.reported_attempts = add(self.summary.reported_attempts, 1)?;
        } else if matches!(
            event,
            LanguageEvent::Finished { .. } | LanguageEvent::Failed { .. }
        ) {
            let open = self.open.take().expect("exact model attempt");
            self.summary.timing.observe_attempt(&open, timestamp_ms)?;
            let failed = matches!(event, LanguageEvent::Failed { .. });
            if failed {
                self.summary.failed_attempts = add(self.summary.failed_attempts, 1)?;
            }
            if !failed && open.purpose == ModelEventPurpose::Conversation {
                self.summary.last_context = open
                    .usage
                    .zip(open.description)
                    .map(|(usage, description)| {
                        ContextUsage::new(
                            description,
                            self.output_budgets.cap(&open.model),
                            usage.input_tokens(),
                            seq,
                        )
                    })
                    .transpose()?;
            } else if !failed && open.purpose == ModelEventPurpose::ContextCompaction {
                self.summary.last_context = None;
            }
            self.summary.last_attempt = Some(AttemptMetrics {
                turn: open.turn,
                effect: open.effect,
                model: open.model,
                config_generation: open.config_generation,
                reasoning_effort: open
                    .settings
                    .and_then(|settings| settings.effective_reasoning_effort),
                usage: open.usage,
                elapsed_ms: open
                    .started_ms
                    .and_then(|start| timestamp_ms.checked_sub(start)),
                failed,
                terminal_seq: seq,
            });
        }
        Ok(())
    }

    #[allow(clippy::too_many_lines)] // One ordered reducer handles each stage of the same request.
    fn apply(&mut self, fact: &SessionFact) -> Result<(), &'static str> {
        match fact.body() {
            SessionFactBody::TurnAccepted { turn_id, .. }
            | SessionFactBody::MessageTurnAccepted { turn_id, .. }
            | SessionFactBody::ImageRequested { turn_id, .. } => {
                if !self.turn_starts.contains_key(turn_id) {
                    if self.turn_starts.len() == rsi_agent_session_protocol::MAXIMUM_LIVE_TURNS {
                        return Err("too many nonterminal Turns in Session timing");
                    }
                    self.summary.timing.turns = add(self.summary.timing.turns, 1)?;
                    self.turn_starts
                        .insert(turn_id.clone(), fact.timestamp_ms());
                }
            }
            SessionFactBody::ModelIntent {
                turn_id,
                effect_id,
                snapshot,
                purpose,
                price_quote,
                ..
            } => {
                if self.open.is_some() {
                    return Err("interleaved Session model attempts");
                }
                self.summary.attempts = add(self.summary.attempts, 1)?;
                self.summary.configured_cost.intent(price_quote.as_ref())?;
                self.open = Some(OpenAttempt {
                    description: rsi_ai_protocol::LanguageModelDescription::from_snapshot(snapshot)
                        .ok(),
                    price_quote: price_quote.clone(),
                    turn: turn_id.clone(),
                    effect: effect_id.clone(),
                    model: ModelRef::new(&snapshot.deployment_id, &snapshot.model)
                        .map_err(|_| "invalid prepared model")?,
                    config_generation: snapshot.config_generation,
                    settings: snapshot.language_settings.clone(),
                    started_ms: None,
                    first_token_ms: None,
                    usage: None,
                    purpose: purpose.event_purpose(),
                });
            }
            SessionFactBody::ModelStarted { turn_id, effect_id } => {
                let open = self.exact(turn_id, effect_id)?;
                if open.started_ms.replace(fact.timestamp_ms()).is_some() {
                    return Err("duplicate model start");
                }
            }
            SessionFactBody::ModelAttemptSettled {
                turn_id,
                effect_id,
                purpose,
                attempt,
            } => {
                let start = self
                    .exact(turn_id, effect_id)?
                    .started_ms
                    .unwrap_or(fact.timestamp_ms());
                for item in attempt.events() {
                    let (elapsed, event) = item.map_err(|_| "invalid model attempt evidence")?;
                    self.model_event(
                        turn_id,
                        effect_id,
                        *purpose,
                        &event,
                        fact.seq(),
                        start.saturating_add(elapsed),
                    )?;
                }
            }
            SessionFactBody::TurnTerminal { turn_id, .. } => {
                if self.open.as_ref().is_some_and(|open| &open.turn == turn_id) {
                    self.open = None;
                }
                if let Some(start) = self.turn_starts.remove(turn_id)
                    && let Some(elapsed) = fact.timestamp_ms().checked_sub(start)
                {
                    self.summary.timing.timed_turns = add(self.summary.timing.timed_turns, 1)?;
                    self.summary.timing.total_ms = add(self.summary.timing.total_ms, elapsed)?;
                }
            }
            _ => {}
        }
        Ok(())
    }

    fn exact(
        &mut self,
        turn: &TurnId,
        effect: &EffectId,
    ) -> Result<&mut OpenAttempt, &'static str> {
        self.open
            .as_mut()
            .filter(|open| &open.turn == turn && &open.effect == effect)
            .ok_or("model event has no matching metrics intent")
    }
}

impl SessionTiming {
    fn observe_attempt(&mut self, open: &OpenAttempt, end: u64) -> Result<(), &'static str> {
        let Some(start) = open.started_ms else {
            return Ok(());
        };
        let Some(elapsed) = end.checked_sub(start) else {
            return Ok(());
        };
        self.timed_attempts = add(self.timed_attempts, 1)?;
        self.model_ms = add(self.model_ms, elapsed)?;
        if let Some(first) = open
            .first_token_ms
            .filter(|first| *first >= start && *first <= end)
        {
            self.first_token_attempts = add(self.first_token_attempts, 1)?;
            self.first_token_ms = add(self.first_token_ms, first - start)?;
            if let Some(usage) = open.usage.filter(|_| end > first) {
                self.decode_ms = add(self.decode_ms, end - first)?;
                self.decode_output_tokens = add(self.decode_output_tokens, usage.output_tokens())?;
            }
        }
        Ok(())
    }
}

/// Checked aggregate without a fabricated cross-Session sequence or last request.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UsageTotals {
    pub attempts: u64,
    pub reported_attempts: u64,
    pub failed_attempts: u64,
    pub tokens: TokenUsage,
    pub configured_cost: crate::ConfiguredCost,
}
impl UsageTotals {
    /// Atomically adds one Session's own counters, including failed reported usage.
    /// # Errors
    /// Rejects invalid input counters, currency capacity or checked overflow.
    pub fn add_session(&mut self, summary: &SessionMetrics) -> Result<(), &'static str> {
        summary.validate()?;
        let mut next = self.clone();
        next.attempts = add(next.attempts, summary.attempts)?;
        next.failed_attempts = add(next.failed_attempts, summary.failed_attempts)?;
        if summary.reported_attempts > 0 {
            next.tokens = if next.reported_attempts == 0 {
                summary.tokens
            } else {
                sum(next.tokens, summary.tokens)?
            };
        }
        next.reported_attempts = add(next.reported_attempts, summary.reported_attempts)?;
        next.configured_cost.merge(&summary.configured_cost)?;
        next.validate()?;
        *self = next;
        Ok(())
    }
    /// # Errors
    /// Rejects inconsistent counters, cursors or configured cost totals.
    pub fn validate(&self) -> Result<(), &'static str> {
        self.configured_cost.validate(self.attempts)?;
        if self.failed_attempts > self.attempts || self.reported_attempts > self.attempts {
            return Err("invalid tree usage counters");
        }
        Ok(())
    }
}

fn add(a: u64, b: u64) -> Result<u64, &'static str> {
    a.checked_add(b).ok_or("Session usage overflow")
}
fn subset(a: Option<u64>, b: Option<u64>) -> Result<Option<u64>, &'static str> {
    a.zip(b).map(|(a, b)| add(a, b)).transpose()
}
fn sum(a: TokenUsage, b: TokenUsage) -> Result<TokenUsage, &'static str> {
    TokenUsage::new(
        add(a.input_tokens(), b.input_tokens())?,
        add(a.output_tokens(), b.output_tokens())?,
        subset(a.cache_read_tokens(), b.cache_read_tokens())?,
        subset(a.cache_write_tokens(), b.cache_write_tokens())?,
        subset(a.reasoning_tokens(), b.reasoning_tokens())?,
    )
    .map_err(|_| "Session token totals overflow")
}
