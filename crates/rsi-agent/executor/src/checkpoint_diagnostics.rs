//! Bounded, process-local checkpoint evidence; never an execution dependency.
use rsi_agent_session_protocol::{SessionFact, SessionId};
use serde::Serialize;
use std::{
    collections::{BTreeMap, VecDeque},
    io::Write,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

#[derive(Clone, Copy, Debug, Serialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub(super) enum Phase {
    Schedule,
    Rebuild,
    Write,
    Recovery,
}
#[derive(Clone, Copy, Debug, Serialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub(super) enum Outcome {
    Scheduled,
    Coalesced,
    AtCapacity,
    Closed,
    Ready,
    Declined,
    Capacity,
    Storage,
    Invalid,
    Cancelled,
    Timeout,
    Interrupted,
    Failed,
}
#[derive(Clone, Debug, Default, Serialize)]
pub(super) struct Scan {
    pub canonical_pages: u64,
    pub fork_pages: u64,
    pub facts: u64,
    pub encoded_bytes: u64,
    pub checkpoint_seq: u64,
    pub sequence_lag: u64,
}
impl Scan {
    pub fn page(&mut self, fork: bool, facts: &[Arc<SessionFact>]) {
        let pages = if fork {
            &mut self.fork_pages
        } else {
            &mut self.canonical_pages
        };
        *pages = pages.saturating_add(1);
        self.facts = self.facts.saturating_add(facts.len() as u64);
        for fact in facts {
            self.encoded_bytes = self.encoded_bytes.saturating_add(fact.encoded_len() as u64);
        }
    }
}
#[derive(Clone, Debug, Serialize)]
pub(super) struct Record {
    pub session: SessionId,
    pub phase: Phase,
    pub outcome: Outcome,
    pub elapsed_ms: u64,
    pub scan: Scan,
    pub successful_seq: Option<u64>,
    pub pending_sessions: usize,
    pub in_flight: bool,
}
#[derive(Debug, Default)]
struct State {
    counts: BTreeMap<Phase, BTreeMap<Outcome, u64>>,
    recent: Option<Record>,
    omitted: u64,
    last_emit: Option<Instant>,
    degraded: bool,
    successes: VecDeque<(SessionId, u64, Instant)>,
}
#[derive(Debug, Default)]
pub(super) struct Diagnostics(Mutex<State>);
impl Diagnostics {
    pub fn record(&self, record: Record) {
        let line = {
            let mut state = self
                .0
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if let Some(sequence) = record.successful_seq {
                state
                    .successes
                    .retain(|(session, _, _)| session != &record.session);
                if state.successes.len() == 64 {
                    state.successes.pop_front();
                }
                state
                    .successes
                    .push_back((record.session.clone(), sequence, Instant::now()));
            }
            let count = state
                .counts
                .entry(record.phase)
                .or_default()
                .entry(record.outcome)
                .or_default();
            *count = count.saturating_add(1);
            let degraded = !matches!(
                record.outcome,
                Outcome::Scheduled
                    | Outcome::Coalesced
                    | Outcome::Ready
                    | Outcome::Closed
                    | Outcome::Declined
            );
            if record.outcome == Outcome::Ready
                && matches!(record.phase, Phase::Write | Phase::Recovery)
            {
                state.degraded = false;
            }
            state.recent = Some(record);
            let now = Instant::now();
            let emit = (degraded && !state.degraded)
                || state
                    .last_emit
                    .is_some_and(|last| now.duration_since(last) >= Duration::from_secs(30));
            state.degraded |= degraded;
            if state.last_emit.is_none() {
                state.last_emit = Some(now);
            }
            if emit {
                Some(Self::line(&mut state, now, false))
            } else {
                state.omitted = state.omitted.saturating_add(1);
                None
            }
        };
        if let Some(line) = line {
            Self::write(&line);
        }
    }
    fn line(state: &mut State, now: Instant, final_summary: bool) -> Vec<u8> {
        let value = serde_json::json!({"event":"executor_checkpoint", "final":final_summary,
            "counts":state.counts,
            "recent":state.recent, "omitted_events":state.omitted,
            "last_success":state.recent.as_ref().and_then(|record| state.successes.iter().find(|(id,_,_)| id == &record.session)).map(|(id,seq,at)| serde_json::json!({"session":id,"sequence":seq,"age_ms":u64::try_from(now.duration_since(*at).as_millis()).unwrap_or(u64::MAX)}))});
        let mut bytes = serde_json::to_vec(&value).unwrap_or_default();
        if bytes.len() >= 4096 {
            bytes = b"{\"event\":\"executor_checkpoint\",\"record_omitted\":true}".to_vec();
        }
        bytes.push(b'\n');
        state.omitted = 0;
        state.last_emit = Some(now);
        bytes
    }
    fn write(line: &[u8]) {
        let _ = std::io::stderr().lock().write_all(line);
    }
    pub fn finish(&self) {
        let line = Self::line(
            &mut self
                .0
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
            Instant::now(),
            true,
        );
        Self::write(&line);
    }
    #[cfg(test)]
    pub fn count(&self, phase: Phase, outcome: Outcome) -> u64 {
        self.0
            .lock()
            .unwrap()
            .counts
            .get(&phase)
            .and_then(|counts| counts.get(&outcome))
            .copied()
            .unwrap_or(0)
    }
    #[cfg(test)]
    pub fn last(&self) -> Record {
        self.0.lock().unwrap().recent.as_ref().unwrap().clone()
    }
    pub fn operation(self: &Arc<Self>, session: &SessionId, phase: Phase) -> Operation {
        Operation {
            diagnostics: self.clone(),
            started: Instant::now(),
            record: Record {
                session: session.clone(),
                phase,
                outcome: Outcome::Interrupted,
                elapsed_ms: 0,
                scan: Scan::default(),
                successful_seq: None,
                pending_sessions: 0,
                in_flight: false,
            },
        }
    }
}
pub(super) struct Operation {
    diagnostics: Arc<Diagnostics>,
    started: Instant,
    pub record: Record,
}
impl Drop for Operation {
    fn drop(&mut self) {
        self.record.elapsed_ms =
            u64::try_from(self.started.elapsed().as_millis()).unwrap_or(u64::MAX);
        self.diagnostics.record(self.record.clone());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn routine_declines_preserve_the_next_degradation_edge() {
        let diagnostics = Arc::new(Diagnostics::default());
        let session = SessionId::new("health").unwrap();
        for outcome in [Outcome::Declined, Outcome::Storage] {
            let mut operation = diagnostics.operation(&session, Phase::Write);
            operation.record.outcome = outcome;
        }
        let state = diagnostics.0.lock().unwrap();
        assert_eq!(
            state.omitted, 0,
            "the first storage failure emits immediately"
        );
        assert!(state.degraded);
    }

    #[test]
    fn recovery_rearms_degradation_but_scheduling_and_rebuild_do_not() {
        let diagnostics = Arc::new(Diagnostics::default());
        let session = SessionId::new("health").unwrap();
        let record = |phase, outcome| {
            let mut operation = diagnostics.operation(&session, phase);
            operation.record.outcome = outcome;
        };
        record(Phase::Write, Outcome::Storage);
        record(Phase::Schedule, Outcome::Scheduled);
        record(Phase::Rebuild, Outcome::Ready);
        record(Phase::Write, Outcome::Storage);
        assert_eq!(diagnostics.0.lock().unwrap().omitted, 3);
        for phase in [Phase::Write, Phase::Recovery] {
            record(phase, Outcome::Ready);
            record(Phase::Write, Outcome::Storage);
            assert_eq!(diagnostics.0.lock().unwrap().omitted, 0);
        }
    }

    #[test]
    fn counts_distinguish_phases_and_failed_writes_retain_last_success() {
        let diagnostics = Arc::new(Diagnostics::default());
        let session = SessionId::new("test").unwrap();
        {
            let mut op = diagnostics.operation(&session, Phase::Recovery);
            op.record.outcome = Outcome::Ready;
        }
        {
            let mut op = diagnostics.operation(&session, Phase::Write);
            op.record.outcome = Outcome::Ready;
            op.record.successful_seq = Some(7);
        }
        {
            let mut op = diagnostics.operation(&session, Phase::Write);
            op.record.outcome = Outcome::Storage;
        }
        let mut state = diagnostics.0.lock().unwrap();
        let line = Diagnostics::line(&mut state, Instant::now(), true);
        let summary: serde_json::Value = serde_json::from_slice(&line).unwrap();
        assert_eq!(
            summary["counts"],
            serde_json::json!({
                "recovery":{"ready":1}, "write":{"ready":1,"storage":1}
            })
        );
        assert_eq!(summary["last_success"]["sequence"], 7);
    }

    #[test]
    fn records_counters_and_output_are_bounded() {
        let diagnostics = Arc::new(Diagnostics::default());
        for sequence in 1..=100 {
            let mut op = diagnostics.operation(&SessionId::new("test").unwrap(), Phase::Recovery);
            op.record.outcome = Outcome::Ready;
            op.record.scan.checkpoint_seq = sequence;
        }
        let mut state = diagnostics.0.lock().unwrap();
        assert_eq!(state.counts[&Phase::Recovery][&Outcome::Ready], 100);
        assert_eq!(state.omitted, 100);
        let line = Diagnostics::line(&mut state, Instant::now(), true);
        assert!(line.len() <= 4096);
        let summary = serde_json::from_slice::<serde_json::Value>(&line).unwrap();
        assert_eq!(summary["omitted_events"], 100);
        assert_eq!(summary["recent"]["scan"]["checkpoint_seq"], 100);
        assert_eq!(
            summary["counts"],
            serde_json::json!({"recovery":{"ready":100}})
        );
        state
            .counts
            .entry(Phase::Write)
            .or_default()
            .insert(Outcome::Ready, u64::MAX);
        drop(state);
        {
            let mut op = diagnostics.operation(&SessionId::new("test").unwrap(), Phase::Write);
            op.record.outcome = Outcome::Ready;
        }
        assert_eq!(diagnostics.count(Phase::Write, Outcome::Ready), u64::MAX);
    }
    #[test]
    fn dropped_operation_is_interrupted() {
        let diagnostics = Arc::new(Diagnostics::default());
        drop(diagnostics.operation(&SessionId::new("test").unwrap(), Phase::Rebuild));
        assert_eq!(diagnostics.count(Phase::Rebuild, Outcome::Interrupted), 1);
    }
}
