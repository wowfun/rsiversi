//! Fair, bounded scheduling for optional per-Session context checkpoints.

use rsi_agent_composition_protocol::AgentCompositionPin;
use rsi_agent_context::{ContextLimits, ContextPage, ModelContextState};
use rsi_agent_session_protocol::SessionId;
use rsi_agent_turn_protocol::{ContextCheckpoint, TurnClaim, TurnExecution};
use std::collections::{BTreeMap, VecDeque};
use std::io::Write as _;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::Notify;

const MAXIMUM_PENDING_SESSIONS: usize = 256;
const CHECKPOINT_MAINTENANCE_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Clone, Debug)]
pub(super) struct CheckpointRequest {
    claim: TurnClaim,
    limits: ContextLimits,
    composition: AgentCompositionPin,
    budget: rsi_agent_context::ContextBudget,
}

impl CheckpointRequest {
    pub(super) const fn new(
        claim: TurnClaim,
        limits: ContextLimits,
        composition: AgentCompositionPin,
        budget: rsi_agent_context::ContextBudget,
    ) -> Self {
        Self {
            claim,
            limits,
            composition,
            budget,
        }
    }

    fn session_id(&self) -> &SessionId {
        self.claim.session_id()
    }
}

#[derive(Debug)]
struct SchedulerState {
    queue: VecDeque<SessionId>,
    pending: BTreeMap<SessionId, CheckpointRequest>,
    in_flight: Option<SessionId>,
    accepting: bool,
}

/// One writer queue that coalesces per Session and stays fair across Sessions.
#[derive(Debug)]
pub(super) struct CheckpointScheduler {
    state: Mutex<SchedulerState>,
    changed: Notify,
}

impl CheckpointScheduler {
    pub(super) fn new() -> Self {
        Self {
            state: Mutex::new(SchedulerState {
                queue: VecDeque::new(),
                pending: BTreeMap::new(),
                in_flight: None,
                accepting: true,
            }),
            changed: Notify::new(),
        }
    }

    pub(super) fn schedule(&self, request: CheckpointRequest) {
        let session_id = request.session_id().clone();
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if !state.accepting {
            return;
        }
        if let Some(pending) = state.pending.get_mut(&session_id) {
            *pending = request;
            return;
        }
        if state.in_flight.as_ref() == Some(&session_id) {
            state.pending.insert(session_id, request);
            return;
        }
        let tracked = state
            .pending
            .len()
            .saturating_add(usize::from(state.in_flight.is_some()));
        if tracked >= MAXIMUM_PENDING_SESSIONS {
            return;
        }
        state.pending.insert(session_id.clone(), request);
        state.queue.push_back(session_id);
        drop(state);
        self.changed.notify_one();
    }

    pub(super) fn close(&self) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.accepting = false;
        drop(state);
        self.changed.notify_waiters();
    }

    async fn next(&self) -> Option<CheckpointRequest> {
        loop {
            // Tokio tracks `notify_waiters` generations from future creation,
            // so closure between this line and the first poll is observed.
            let changed = self.changed.notified();
            {
                let mut state = self
                    .state
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                if let Some(session_id) = state.queue.pop_front() {
                    let request = state
                        .pending
                        .remove(&session_id)
                        .expect("checkpoint queue and pending map stay aligned");
                    debug_assert!(state.in_flight.is_none());
                    state.in_flight = Some(session_id);
                    return Some(request);
                }
                if !state.accepting {
                    return None;
                }
            }
            changed.await;
        }
    }

    fn finish(&self, session_id: &SessionId) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        debug_assert_eq!(state.in_flight.as_ref(), Some(session_id));
        state.in_flight = None;
        if state.pending.contains_key(session_id) {
            state.queue.push_back(session_id.clone());
        }
    }
}

struct InFlightCheckpoint {
    scheduler: Arc<CheckpointScheduler>,
    session_id: SessionId,
}

impl Drop for InFlightCheckpoint {
    fn drop(&mut self) {
        self.scheduler.finish(&self.session_id);
    }
}

pub(super) async fn run_checkpoint_writer(
    turns: Arc<dyn TurnExecution>,
    scheduler: Arc<CheckpointScheduler>,
) {
    while let Some(request) = scheduler.next().await {
        let in_flight = InFlightCheckpoint {
            scheduler: Arc::clone(&scheduler),
            session_id: request.session_id().clone(),
        };
        let result = save_checkpoint(&turns, &request).await;
        drop(in_flight);
        if let Err((phase, failure)) = result
            && failure != CheckpointFailure::Declined
        {
            let warning = format!(
                "{}\n",
                serde_json::json!({
                    "event": "executor_checkpoint_failed",
                    "session": request.session_id(),
                    "phase": phase,
                    "reason": failure
                })
            );
            let _ = std::io::stderr().lock().write_all(warning.as_bytes());
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum CheckpointFailure {
    Declined,
    Capacity,
    Storage,
    Invalid,
    Cancelled,
    Timeout,
}

pub(super) async fn save_checkpoint(
    turns: &Arc<dyn TurnExecution>,
    request: &CheckpointRequest,
) -> Result<(), (&'static str, CheckpointFailure)> {
    let deadline = tokio::time::Instant::now() + CHECKPOINT_MAINTENANCE_TIMEOUT;
    let checkpoint = tokio::time::timeout_at(deadline, rebuild_context_checkpoint(turns, request))
        .await
        .unwrap_or(Err(CheckpointFailure::Timeout))
        .map_err(|failure| ("rebuild", failure))?;
    if let Some(checkpoint) = checkpoint {
        tokio::time::timeout_at(
            deadline,
            turns.write_context_checkpoint(&request.claim, checkpoint),
        )
        .await
        .map_err(|_| ("write", CheckpointFailure::Timeout))?
        .map_err(|error| ("write", turn_outcome(&error)))?;
    }
    Ok(())
}

fn turn_outcome(error: &rsi_agent_turn_protocol::TurnError) -> CheckpointFailure {
    use rsi_agent_turn_protocol::TurnError;
    match error {
        TurnError::Capacity => CheckpointFailure::Capacity,
        TurnError::Cancelled => CheckpointFailure::Cancelled,
        TurnError::StaleClaim | TurnError::ShuttingDown | TurnError::EvidenceBudget => {
            CheckpointFailure::Declined
        }
        TurnError::Store(_) | TurnError::Flush(_) => CheckpointFailure::Storage,
        _ => CheckpointFailure::Invalid,
    }
}
fn context_outcome(error: &rsi_agent_context::ContextError) -> CheckpointFailure {
    match error {
        rsi_agent_context::ContextError::Capacity => CheckpointFailure::Capacity,
        _ => CheckpointFailure::Invalid,
    }
}

async fn seed_checkpoint(
    turns: &Arc<dyn TurnExecution>,
    request: &CheckpointRequest,
    fold: &mut ModelContextState,
) -> Result<bool, CheckpointFailure> {
    let Some(reader) = turns
        .open_checkpoint_fork_history(&request.claim)
        .await
        .map_err(|error| turn_outcome(&error))?
    else {
        return Ok(false);
    };
    for source in reader.sources() {
        fold.ingest(ContextPage::ForkSource(source))
            .map_err(|error| context_outcome(&error))?;
        let origin = source.history_origin().ok_or(CheckpointFailure::Invalid)?;
        let mut parent_cursor = origin.resolved_after_seq;
        while parent_cursor < origin.resolved_terminal_seq {
            let page = reader
                .read_facts(
                    source.session_id(),
                    parent_cursor,
                    rsi_agent_session_protocol::MAXIMUM_FACTS_PER_READ,
                )
                .await
                .map_err(|error| turn_outcome(&error))?;
            let Some(page) = page else { return Ok(false) };

            if page.through_parent_seq <= parent_cursor {
                return Err(CheckpointFailure::Invalid);
            }
            fold.ingest(ContextPage::ForkSeed(&page.facts))
                .map_err(|error| context_outcome(&error))?;
            parent_cursor = page.through_parent_seq;
        }
    }
    fold.ingest(ContextPage::FinishSeed)
        .map_err(|error| context_outcome(&error))?;
    Ok(true)
}

async fn rebuild_context_checkpoint(
    turns: &Arc<dyn TurnExecution>,
    request: &CheckpointRequest,
) -> Result<Option<ContextCheckpoint>, CheckpointFailure> {
    let mut fold = ModelContextState::open(
        request.composition.context_builder(),
        request.claim.header().clone(),
        request.limits,
        request.budget.clone(),
    )
    .map_err(|error| context_outcome(&error))?;
    let checkpoint = read_checkpoint(turns, request.claim.session_id(), &request.budget)
        .await
        .map_err(|error| turn_outcome(&error))?;
    let cached_through = if let Some(checkpoint) = checkpoint
        && let Some(restored) = checkpoint
            .restore_for(&fold)
            .map_err(|error| context_outcome(&error))?
    {
        fold = restored;
        checkpoint.through_seq
    } else {
        0
    };
    let mut cursor = cached_through;
    if cursor == 0
        && request.claim.header().history_origin().is_some()
        && !seed_checkpoint(turns, request, &mut fold).await?
    {
        return Ok(None);
    }
    loop {
        let page = turns
            .read_checkpoint_facts(
                &request.claim,
                cursor,
                rsi_agent_session_protocol::MAXIMUM_FACTS_PER_READ,
            )
            .await
            .map_err(|error| turn_outcome(&error))?;
        let Some(page) = page else { return Ok(None) };

        if page.through_seq <= cursor {
            if page.through_seq != cursor {
                return Err(CheckpointFailure::Invalid);
            }
            if cached_through != 0 && cursor == cached_through {
                return Ok(None);
            }
            break;
        }
        fold.ingest(ContextPage::Canonical(&page.facts))
            .map_err(|error| context_outcome(&error))?;
        cursor = page.through_seq;
    }

    let position = fold.position();
    Ok(Some(ContextCheckpoint {
        header_fingerprint: fold.header_fingerprint().to_owned(),
        through_seq: position.through_seq,
        fact_prefix_sha256: position.fact_prefix_sha256(),
        bytes: fold.checkpoint().map_err(|error| context_outcome(&error))?,
    }))
}

/// Admission travels into the actual Store worker, including after waiter cancellation.
pub(super) async fn read_checkpoint(
    turns: &Arc<dyn TurnExecution>,
    id: &SessionId,
    budget: &rsi_agent_context::ContextBudget,
) -> rsi_agent_turn_protocol::Result<Option<ContextCheckpoint>> {
    turns
        .read_context_checkpoint(id, budget.checkpoint_read())
        .await
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn bounded_evidence_refusal_is_a_decline() {
        assert_eq!(
            turn_outcome(&rsi_agent_turn_protocol::TurnError::EvidenceBudget),
            CheckpointFailure::Declined
        );
    }
    use rsi_agent_session_protocol::{AgentPresetId, FrozenAgentSettings, SessionHeader, TurnId};
    use rsi_agent_turn_protocol::TurnClaimIssuer;
    use rsi_ai_protocol::ModelRef;
    use rsi_sandbox::SandboxMode;

    fn request(session: &str, turn: usize) -> CheckpointRequest {
        let session_id = SessionId::new(session).unwrap();
        let turn_id = TurnId::new(format!("turn-{turn}")).unwrap();
        let header = SessionHeader::new_local(
            session_id.clone(),
            1,
            "/tmp",
            AgentPresetId::new("test-agent").unwrap(),
            FrozenAgentSettings::new(
                "test",
                "system",
                ModelRef::new("test", "model").unwrap(),
                SandboxMode::WorkspaceWrite,
                false,
            )
            .unwrap(),
        )
        .unwrap();
        CheckpointRequest::new(
            TurnClaimIssuer::new().issue(
                "executor".into(),
                u64::try_from(turn).unwrap(),
                session_id,
                turn_id,
                Arc::new(header),
                1,
                1,
                1,
            ),
            ContextLimits::default(),
            crate::tests::context_pin(),
            rsi_agent_context::ContextBudget::default(),
        )
    }

    #[tokio::test]
    async fn queued_updates_keep_their_original_fifo_position() {
        let scheduler = CheckpointScheduler::new();
        scheduler.schedule(request("session-a", 1));
        scheduler.schedule(request("session-b", 1));
        scheduler.schedule(request("session-a", 2));

        let first = scheduler.next().await.unwrap();
        assert_eq!(first.session_id().as_str(), "session-a");
        assert_eq!(first.claim.turn_id().as_str(), "turn-2");
        scheduler.finish(first.session_id());
        let second = scheduler.next().await.unwrap();
        assert_eq!(second.session_id().as_str(), "session-b");
    }

    #[tokio::test]
    async fn an_in_flight_update_requeues_once_behind_other_sessions() {
        let scheduler = CheckpointScheduler::new();
        scheduler.schedule(request("session-a", 1));
        scheduler.schedule(request("session-b", 1));
        let first = scheduler.next().await.unwrap();
        assert_eq!(first.session_id().as_str(), "session-a");
        scheduler.schedule(request("session-a", 2));
        scheduler.schedule(request("session-a", 3));
        scheduler.finish(first.session_id());

        let second = scheduler.next().await.unwrap();
        assert_eq!(second.session_id().as_str(), "session-b");
        scheduler.finish(second.session_id());
        let third = scheduler.next().await.unwrap();
        assert_eq!(third.session_id().as_str(), "session-a");
        assert_eq!(third.claim.turn_id().as_str(), "turn-3");
    }

    #[tokio::test]
    async fn capacity_counts_the_in_flight_key_and_still_accepts_its_update() {
        let scheduler = CheckpointScheduler::new();
        scheduler.schedule(request("session-active", 1));
        let active = scheduler.next().await.unwrap();
        for index in 0..MAXIMUM_PENDING_SESSIONS - 1 {
            scheduler.schedule(request(&format!("session-{index}"), 1));
        }
        scheduler.schedule(request("session-active", 2));
        scheduler.schedule(request("session-over-capacity", 1));
        scheduler.finish(active.session_id());
        scheduler.close();
        for index in 0..MAXIMUM_PENDING_SESSIONS - 1 {
            let queued = scheduler.next().await.unwrap();
            assert_eq!(queued.session_id().as_str(), format!("session-{index}"));
            scheduler.finish(queued.session_id());
        }
        let update = scheduler.next().await.unwrap();
        assert_eq!(update.session_id(), active.session_id());
        assert_eq!(update.claim.turn_id().as_str(), "turn-2");
        scheduler.finish(update.session_id());
        assert!(scheduler.next().await.is_none());
    }

    #[tokio::test]
    async fn close_drains_admitted_work_and_rejects_later_requests() {
        let scheduler = CheckpointScheduler::new();
        scheduler.schedule(request("session-a", 1));
        let active = scheduler.next().await.unwrap();
        scheduler.schedule(request("session-b", 1));
        scheduler.schedule(request("session-a", 2));
        scheduler.close();
        scheduler.schedule(request("session-c", 1));
        scheduler.finish(active.session_id());

        let second = scheduler.next().await.unwrap();
        assert_eq!(second.session_id().as_str(), "session-b");
        scheduler.finish(second.session_id());
        let third = scheduler.next().await.unwrap();
        assert_eq!(third.session_id().as_str(), "session-a");
        assert_eq!(third.claim.turn_id().as_str(), "turn-2");
        scheduler.finish(third.session_id());
        assert!(scheduler.next().await.is_none());
    }
}
