//! Fair, bounded scheduling for optional per-Session context checkpoints.

use crate::checkpoint_diagnostics::{Diagnostics, Outcome, Phase, Scan};
use rsi_agent_composition_protocol::AgentCompositionPin;
use rsi_agent_context::{ContextLimits, ContextPage, ModelContextState};
use rsi_agent_session_protocol::SessionId;
use rsi_agent_turn_protocol::{ContextCheckpoint, TurnClaim, TurnExecution};
use std::collections::{BTreeMap, VecDeque};
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
    pub(super) diagnostics: Arc<Diagnostics>,
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
            diagnostics: Arc::new(Diagnostics::default()),
        }
    }

    pub(super) fn schedule(&self, request: CheckpointRequest) -> Outcome {
        let mut operation = self
            .diagnostics
            .operation(request.session_id(), Phase::Schedule);
        let outcome = self.schedule_inner(request);
        {
            let state = self
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            operation.record.pending_sessions = state.pending.len();
            operation.record.in_flight = state.in_flight.is_some();
        }
        operation.record.outcome = outcome;
        outcome
    }

    fn schedule_inner(&self, request: CheckpointRequest) -> Outcome {
        let session_id = request.session_id().clone();
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if !state.accepting {
            return Outcome::Closed;
        }
        if let Some(pending) = state.pending.get_mut(&session_id) {
            *pending = request;
            return Outcome::Coalesced;
        }
        if state.in_flight.as_ref() == Some(&session_id) {
            state.pending.insert(session_id, request);
            return Outcome::Coalesced;
        }
        let tracked = state
            .pending
            .len()
            .saturating_add(usize::from(state.in_flight.is_some()));
        if tracked >= MAXIMUM_PENDING_SESSIONS {
            return Outcome::AtCapacity;
        }
        state.pending.insert(session_id.clone(), request);
        state.queue.push_back(session_id);
        drop(state);
        self.changed.notify_one();
        Outcome::Scheduled
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
    struct Summary(Arc<Diagnostics>);
    impl Drop for Summary {
        fn drop(&mut self) {
            self.0.finish();
        }
    }
    let _summary = Summary(scheduler.diagnostics.clone());
    while let Some(request) = scheduler.next().await {
        let in_flight = InFlightCheckpoint {
            scheduler: Arc::clone(&scheduler),
            session_id: request.session_id().clone(),
        };
        let deadline = tokio::time::Instant::now() + CHECKPOINT_MAINTENANCE_TIMEOUT;
        let mut operation = scheduler
            .diagnostics
            .operation(request.session_id(), Phase::Rebuild);
        {
            let state = scheduler
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            operation.record.pending_sessions = state.pending.len();
            operation.record.in_flight = state.in_flight.is_some();
        }
        match tokio::time::timeout_at(
            deadline,
            rebuild_context_checkpoint(&turns, &request, &mut operation.record.scan),
        )
        .await
        {
            Err(_) => operation.record.outcome = Outcome::Timeout,
            Ok(Err(outcome)) => operation.record.outcome = outcome,
            Ok(Ok(None)) => operation.record.outcome = Outcome::Declined,
            Ok(Ok(Some(checkpoint))) => {
                operation.record.outcome = Outcome::Ready;
                let mut write = scheduler
                    .diagnostics
                    .operation(request.session_id(), Phase::Write);
                write.record.scan = operation.record.scan.clone();
                write.record.pending_sessions = operation.record.pending_sessions;
                write.record.in_flight = operation.record.in_flight;
                operation = write;
                let sequence = checkpoint.through_seq;
                operation.record.outcome = match tokio::time::timeout_at(
                    deadline,
                    turns.write_context_checkpoint(&request.claim, checkpoint),
                )
                .await
                {
                    Err(_) => Outcome::Timeout,
                    Ok(Err(error)) => turn_outcome(&error),
                    Ok(Ok(false)) => Outcome::Declined,
                    Ok(Ok(true)) => {
                        operation.record.successful_seq = Some(sequence);
                        Outcome::Ready
                    }
                };
            }
        }
        drop(in_flight);
        {
            let state = scheduler
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            operation.record.pending_sessions = state.pending.len();
            operation.record.in_flight = state.in_flight.is_some();
        }
    }
}

fn turn_outcome(error: &rsi_agent_turn_protocol::TurnError) -> Outcome {
    use rsi_agent_turn_protocol::TurnError;
    match error {
        TurnError::Capacity => Outcome::Capacity,
        TurnError::Cancelled => Outcome::Cancelled,
        TurnError::StaleClaim | TurnError::ShuttingDown | TurnError::EvidenceBudget => {
            Outcome::Declined
        }
        TurnError::Store(_) | TurnError::Flush(_) => Outcome::Storage,
        _ => Outcome::Invalid,
    }
}
fn context_outcome(error: &rsi_agent_context::ContextError) -> Outcome {
    match error {
        rsi_agent_context::ContextError::Capacity => Outcome::Capacity,
        _ => Outcome::Invalid,
    }
}

async fn rebuild_context_checkpoint(
    turns: &Arc<dyn TurnExecution>,
    request: &CheckpointRequest,
    scan: &mut Scan,
) -> Result<Option<ContextCheckpoint>, Outcome> {
    let mut fold = ModelContextState::open(
        request.composition.context_builder(),
        request.claim.header().clone(),
        request.limits,
        request.budget.clone(),
    )
    .map_err(|error| context_outcome(&error))?;
    let mut cursor = 0;
    let mut restored_checkpoint = false;
    let checkpoint = read_checkpoint(turns, request.claim.session_id(), &request.budget)
        .await
        .map_err(|error| turn_outcome(&error))?;
    if let Some(checkpoint) = checkpoint {
        let candidate =
            restore_candidate(&fold, &checkpoint.bytes).map_err(|error| context_outcome(&error))?;
        if let Some(restored) = candidate
            && restored.position().through_seq == checkpoint.through_seq
            && restored.position().fact_prefix_sha256() == checkpoint.fact_prefix_sha256
            && request
                .claim
                .header()
                .fingerprint()
                .is_ok_and(|fingerprint| fingerprint == checkpoint.header_fingerprint)
        {
            fold = restored;
            cursor = checkpoint.through_seq;
            restored_checkpoint = true;
            scan.checkpoint_seq = cursor;
        }
    }
    if !restored_checkpoint {
        let origin = request.claim.header().fork_origin();
        if let Some(origin) = origin {
            let mut parent_cursor = origin.resolved_after_seq;
            loop {
                let page = turns
                    .read_checkpoint_fork_facts(
                        &request.claim,
                        parent_cursor,
                        rsi_agent_session_protocol::MAXIMUM_FACTS_PER_READ,
                    )
                    .await
                    .map_err(|error| turn_outcome(&error))?;
                let Some(page) = page else { return Ok(None) };
                scan.page(true, &page.facts);
                if page.through_parent_seq <= parent_cursor {
                    if parent_cursor != page.terminal_parent_seq {
                        return Err(Outcome::Invalid);
                    }
                    break;
                }
                fold.ingest(ContextPage::ForkSeed(&page.facts))
                    .map_err(|error| context_outcome(&error))?;
                parent_cursor = page.through_parent_seq;
            }
            fold.ingest(ContextPage::FinishSeed)
                .map_err(|error| context_outcome(&error))?;
        }
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
        scan.page(false, &page.facts);
        if page.through_seq <= cursor {
            if page.through_seq != cursor {
                return Err(Outcome::Invalid);
            }
            break;
        }
        fold.ingest(ContextPage::Canonical(&page.facts))
            .map_err(|error| context_outcome(&error))?;
        cursor = page.through_seq;
    }
    scan.sequence_lag = cursor.saturating_sub(scan.checkpoint_seq);
    Ok(Some(ContextCheckpoint {
        header_fingerprint: request
            .claim
            .header()
            .fingerprint()
            .map_err(|_| Outcome::Invalid)?,
        through_seq: fold.position().through_seq,
        fact_prefix_sha256: fold.position().fact_prefix_sha256(),
        bytes: fold.checkpoint().map_err(|error| context_outcome(&error))?,
    }))
}

/// Corrupt or incompatible caches fall back; pressure must preserve its refusal
/// instead of initiating another expensive materialization from raw history.
pub(super) fn restore_candidate(
    state: &ModelContextState,
    bytes: &[u8],
) -> rsi_agent_context::Result<Option<ModelContextState>> {
    match state.restored(bytes) {
        Ok(restored) => Ok(Some(restored)),
        Err(error @ rsi_agent_context::ContextError::Capacity) => Err(error),
        Err(_) => Ok(None),
    }
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
            Outcome::Declined
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
        assert_eq!(
            scheduler.schedule(request("session-a", 1)),
            Outcome::Scheduled
        );
        assert_eq!(
            scheduler.schedule(request("session-b", 1)),
            Outcome::Scheduled
        );
        assert_eq!(
            scheduler.schedule(request("session-a", 2)),
            Outcome::Coalesced
        );

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
        assert_eq!(
            scheduler.schedule(request("session-a", 1)),
            Outcome::Scheduled
        );
        assert_eq!(
            scheduler.schedule(request("session-b", 1)),
            Outcome::Scheduled
        );
        let first = scheduler.next().await.unwrap();
        assert_eq!(first.session_id().as_str(), "session-a");
        assert_eq!(
            scheduler.schedule(request("session-a", 2)),
            Outcome::Coalesced
        );
        assert_eq!(
            scheduler.schedule(request("session-a", 3)),
            Outcome::Coalesced
        );
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
        assert_eq!(
            scheduler.schedule(request("session-active", 1)),
            Outcome::Scheduled
        );
        let active = scheduler.next().await.unwrap();
        for index in 0..MAXIMUM_PENDING_SESSIONS - 1 {
            assert_eq!(
                scheduler.schedule(request(&format!("session-{index}"), 1)),
                Outcome::Scheduled
            );
        }
        assert_eq!(
            scheduler.schedule(request("session-active", 2)),
            Outcome::Coalesced
        );
        assert_eq!(
            scheduler.schedule(request("session-over-capacity", 1)),
            Outcome::AtCapacity
        );
        assert_eq!(
            scheduler
                .diagnostics
                .count(Phase::Schedule, Outcome::AtCapacity),
            1
        );
        scheduler.finish(active.session_id());
    }

    #[tokio::test]
    async fn close_drains_admitted_work_and_rejects_later_requests() {
        let scheduler = CheckpointScheduler::new();
        assert_eq!(
            scheduler.schedule(request("session-a", 1)),
            Outcome::Scheduled
        );
        let active = scheduler.next().await.unwrap();
        assert_eq!(
            scheduler.schedule(request("session-b", 1)),
            Outcome::Scheduled
        );
        assert_eq!(
            scheduler.schedule(request("session-a", 2)),
            Outcome::Coalesced
        );
        scheduler.close();
        assert_eq!(scheduler.schedule(request("session-c", 1)), Outcome::Closed);
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
