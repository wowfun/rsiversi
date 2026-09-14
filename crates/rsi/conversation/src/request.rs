//! Partial request metadata for history windows, independent of lifetime totals.
use rsi_agent_session_protocol::{SessionFact, SessionFactBody};
use rsi_ai_protocol::{LanguageEvent, TokenUsage};
use std::fmt::Write as _;

/// Display facts of one exact effect. The caller owns its Session/Turn/effect key.
#[derive(Clone, Debug, Default)]
pub struct RequestPresentation {
    intent_seq: Option<u64>,
    model: Option<String>,
    effort: Option<String>,
    started: Option<u64>,
    terminal: Option<(u64, bool)>,
    usage: Option<TokenUsage>,
    compaction: bool,
}
impl RequestPresentation {
    /// Applies lifecycle facts; older backfill fills missing fields without reset.
    pub fn observe(&mut self, fact: &SessionFact) {
        match fact.body() {
            SessionFactBody::ModelIntent {
                snapshot, purpose, ..
            } => {
                self.intent_seq = Some(fact.seq());
                self.model = Some(snapshot.model.clone());
                self.effort = snapshot
                    .language_settings
                    .as_ref()
                    .and_then(|settings| settings.effective_reasoning_effort.as_ref())
                    .map(ToString::to_string);
                self.compaction = purpose.event_purpose()
                    == rsi_agent_session_protocol::ModelEventPurpose::ContextCompaction;
            }
            SessionFactBody::ModelStarted { .. } => self.started = Some(fact.timestamp_ms()),
            SessionFactBody::ModelEvent { event, .. } => match event {
                LanguageEvent::Usage { usage } => self.usage = Some(*usage),
                LanguageEvent::Finished { .. } => {
                    self.terminal = Some((fact.timestamp_ms(), false));
                }
                LanguageEvent::Failed { .. } => self.terminal = Some((fact.timestamp_ms(), true)),
                _ => {}
            },
            _ => {}
        }
    }
    /// Concise actual-request label; unknown usage is never printed as zero.
    pub fn title(&self) -> String {
        let mut title = self
            .model
            .clone()
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
            if let Some(ms) = self.started.and_then(|start| end.checked_sub(start)) {
                let _ = write!(title, " · {}.{:01}s", ms / 1000, (ms % 1000) / 100);
            }
            if failed {
                title.push_str(" · failed");
            }
        }
        title
    }
    /// Retained heap bytes, in addition to the inline structure.
    pub fn owned_bytes(&self) -> usize {
        self.model.as_ref().map_or(0, String::capacity)
            + self.effort.as_ref().map_or(0, String::capacity)
    }
    /// Exact intent source when that earlier history has been loaded.
    pub fn intent_seq(&self) -> Option<u64> {
        self.intent_seq
    }
}
