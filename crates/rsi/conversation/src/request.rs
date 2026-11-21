//! Partial request metadata for history windows, independent of lifetime totals.
use rsi_agent_session_protocol::{LanguageAttemptStage, ScheduledModelRetry, SessionFact, TurnId};
use rsi_ai_protocol::{LanguageEvent, ModelRef, TokenUsage};
use std::fmt::Write as _;

/// A failed request's durable reservation, with its owning Turn and failure kind.
#[derive(Clone, Debug)]
pub struct RecordedRetry {
    /// Turn whose closure ends the wait.
    pub turn: TurnId,
    /// Ordinal, delay and exact target effect.
    pub decision: ScheduledModelRetry,
    /// Typed reason from the originating failed attempt.
    pub reason: rsi_ai_protocol::ErrorKind,
    /// Provider-safe explanation admitted at the protocol boundary.
    pub explanation: Box<str>,
}

/// Outcome of a reservation in the loaded Turn history.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RetryState {
    /// Waiting for the reserved model effect.
    Scheduled,
    /// The reserved effect crossed its start boundary.
    Started,
    /// The owning Turn ended before that start.
    Cancelled,
}

impl RetryState {
    /// A later Turn terminal does not undo the start of the exact reserved effect.
    pub fn observed(started: bool, closed: bool) -> Self {
        if started {
            Self::Started
        } else if closed {
            Self::Cancelled
        } else {
            Self::Scheduled
        }
    }
}

/// Display facts of one exact effect. The caller owns its Session/Turn/effect key.
#[derive(Clone, Debug, Default)]
pub struct RequestPresentation {
    intent_seq: Option<u64>,
    model: Option<ModelRef>,
    effort: Option<String>,
    started: Option<u64>,
    terminal: Option<(u64, bool)>,
    elapsed_ms: Option<u64>,
    usage: Option<TokenUsage>,
    compaction: bool,
    retry_limit: Option<u8>,
    scheduled_retry: Option<Box<RecordedRetry>>,
}
impl RequestPresentation {
    /// Applies lifecycle facts; older backfill fills missing fields without reset.
    pub fn observe(&mut self, fact: &SessionFact) {
        let Some(attempt) = fact.body().language_attempt() else {
            return;
        };
        match attempt.stage {
            LanguageAttemptStage::Prepared(snapshot, purpose, _) => {
                self.intent_seq = Some(fact.seq());
                self.retry_limit = Some(snapshot.retry_policy.max_retries());
                self.model = Some(
                    ModelRef::new(&snapshot.deployment_id, &snapshot.model)
                        .expect("validated request intent"),
                );
                self.effort = snapshot
                    .language_settings
                    .as_ref()
                    .and_then(|settings| settings.effective_reasoning_effort.as_ref())
                    .map(ToString::to_string);
                self.compaction =
                    purpose == rsi_agent_session_protocol::ModelEventPurpose::ContextCompaction;
            }
            LanguageAttemptStage::Started => self.started = Some(fact.timestamp_ms()),
            LanguageAttemptStage::Settled(attempt, purpose) => {
                self.compaction =
                    purpose == rsi_agent_session_protocol::ModelEventPurpose::ContextCompaction;
                if let Some(retry) = attempt.scheduled_retry()
                    && let LanguageEvent::Failed { error, .. } = attempt.terminal()
                    && let Some(turn) = fact.body().turn_id()
                {
                    self.scheduled_retry = Some(Box::new(RecordedRetry {
                        turn: turn.clone(),
                        decision: retry.clone(),
                        reason: error.kind(),
                        explanation: error.safe_summary().into(),
                    }));
                }
                for (elapsed, event) in attempt.events().flatten() {
                    match event {
                        LanguageEvent::Usage { usage } => self.usage = Some(usage),
                        LanguageEvent::Finished { .. } | LanguageEvent::Failed { .. } => {
                            self.elapsed_ms = Some(elapsed);
                            self.terminal = Some((
                                fact.timestamp_ms(),
                                matches!(event, LanguageEvent::Failed { .. }),
                            ));
                        }
                        _ => {}
                    }
                }
            }
        }
    }
    /// Concise actual-request label; unknown usage is never printed as zero.
    pub fn title(&self) -> String {
        self.title_with_retry(RetryState::Scheduled)
    }
    /// Request title in a view which can correlate the later lifecycle facts.
    pub fn title_with_retry(&self, state: RetryState) -> String {
        let mut title = self
            .model
            .as_ref()
            .map(|route| route.model().to_owned())
            .unwrap_or_else(|| "Request · model not loaded".into());
        if self.compaction {
            title.push_str(" · compaction");
        }
        if let Some(effort) = &self.effort {
            let _ = write!(title, " · {effort}");
        }
        if let Some(usage) = self.usage {
            let _ = write!(
                title,
                " · {} in / {} out",
                usage.input_tokens(),
                usage.output_tokens()
            );
        } else if self.terminal.is_some() {
            title.push_str(" · usage unknown");
        }
        if let Some((end, failed)) = self.terminal {
            if let Some(ms) = self
                .elapsed_ms
                .or_else(|| self.started.and_then(|start| end.checked_sub(start)))
            {
                let _ = write!(title, " · {}.{:01}s", ms / 1000, (ms % 1000) / 100);
            }
            if failed {
                title.push_str(" · failed");
            }
        }
        if let Some(retry) = self.scheduled_retry.as_deref() {
            let number = retry.decision.number;
            let delay = retry.decision.delay_ms;
            let reason = retry.reason.code();
            let _ = match state {
                RetryState::Scheduled => write!(
                    title,
                    " · retry {number} scheduled after {delay} ms ({reason})"
                ),
                RetryState::Started => write!(
                    title,
                    " · retry {number} started (planned wait {delay} ms, {reason})"
                ),
                RetryState::Cancelled => write!(
                    title,
                    " · retry {number} cancelled (planned wait {delay} ms, {reason})"
                ),
            };
        }
        title
    }
    /// Retained heap bytes, in addition to the inline structure.
    pub fn owned_bytes(&self) -> usize {
        self.model
            .as_ref()
            .map_or(0, |route| route.deployment().len() + route.model().len())
            + self.effort.as_ref().map_or(0, String::capacity)
            + self.scheduled_retry.as_deref().map_or(0, |value| {
                std::mem::size_of_val(value)
                    + value.decision.effect_id.as_str().len()
                    + value.turn.as_str().len()
                    + value.explanation.len()
            })
    }
    /// The next effect reserved by this failure, its reason and the loaded policy limit.
    pub fn scheduled_retry(&self) -> Option<(&RecordedRetry, Option<u8>)> {
        Some((self.scheduled_retry.as_deref()?, self.retry_limit))
    }
    /// Whether the history window has observed a start for this exact effect.
    pub fn has_started(&self) -> bool {
        self.started.is_some()
    }
    /// Whether this request prepared a context summary.
    pub fn is_compaction(&self) -> bool {
        self.compaction
    }
    /// Inclusive counts from the settled provider stream.
    pub fn reported_usage(&self) -> Option<TokenUsage> {
        self.usage
    }
    /// Frozen provider deployment and model for this exact request.
    pub fn model_route(&self) -> Option<&ModelRef> {
        self.model.as_ref()
    }
    /// Exact intent source when that earlier history has been loaded.
    pub fn intent_seq(&self) -> Option<u64> {
        self.intent_seq
    }
}
