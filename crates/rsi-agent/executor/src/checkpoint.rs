//! Fair, bounded scheduling for optional per-Session context checkpoints.

use rsi_agent_context::{ContextFold, ContextLimits};
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
}

impl CheckpointRequest {
    pub(super) const fn new(claim: TurnClaim, limits: ContextLimits) -> Self {
        Self { claim, limits }
    }

    fn session_id(&self) -> &SessionId {
        self.claim.session_id()
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum ScheduleOutcome {
    Scheduled,
    Coalesced,
    AtCapacity,
    Closed,
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

    pub(super) fn schedule(&self, request: CheckpointRequest) -> ScheduleOutcome {
        let session_id = request.session_id().clone();
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if !state.accepting {
            return ScheduleOutcome::Closed;
        }
        if let Some(pending) = state.pending.get_mut(&session_id) {
            *pending = request;
            return ScheduleOutcome::Coalesced;
        }
        if state.in_flight.as_ref() == Some(&session_id) {
            state.pending.insert(session_id, request);
            return ScheduleOutcome::Coalesced;
        }
        let tracked = state
            .pending
            .len()
            .saturating_add(usize::from(state.in_flight.is_some()));
        if tracked >= MAXIMUM_PENDING_SESSIONS {
            return ScheduleOutcome::AtCapacity;
        }
        state.pending.insert(session_id.clone(), request);
        state.queue.push_back(session_id);
        drop(state);
        self.changed.notify_one();
        ScheduleOutcome::Scheduled
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
        let _in_flight = InFlightCheckpoint {
            scheduler: Arc::clone(&scheduler),
            session_id: request.session_id().clone(),
        };
        let deadline = tokio::time::Instant::now() + CHECKPOINT_MAINTENANCE_TIMEOUT;
        if let Ok(Some(checkpoint)) =
            tokio::time::timeout_at(deadline, rebuild_context_checkpoint(&turns, &request)).await
        {
            let _ignored = tokio::time::timeout_at(
                deadline,
                turns.write_context_checkpoint(&request.claim, checkpoint),
            )
            .await;
        }
    }
}

async fn rebuild_context_checkpoint(
    turns: &Arc<dyn TurnExecution>,
    request: &CheckpointRequest,
) -> Option<ContextCheckpoint> {
    let mut fold = ContextFold::with_limits(request.claim.header().clone(), request.limits).ok()?;
    let mut cursor = 0;
    let mut restored_checkpoint = false;
    if let Ok(Some(checkpoint)) = turns
        .read_context_checkpoint(request.claim.session_id())
        .await
        && let Ok(restored) = ContextFold::from_checkpoint(
            request.claim.header().clone(),
            request.limits,
            &checkpoint.bytes,
        )
        && restored.through_seq() == checkpoint.through_seq
        && restored.fact_prefix_sha256() == checkpoint.fact_prefix_sha256
        && request
            .claim
            .header()
            .fingerprint()
            .is_ok_and(|fingerprint| fingerprint == checkpoint.header_fingerprint)
    {
        fold = restored;
        cursor = checkpoint.through_seq;
        restored_checkpoint = true;
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
                    .ok()??;
                if page.through_parent_seq <= parent_cursor {
                    if parent_cursor != page.terminal_parent_seq {
                        return None;
                    }
                    break;
                }
                fold.apply_seed_page(&page.facts).ok()?;
                parent_cursor = page.through_parent_seq;
            }
            fold.finish_seed().ok()?;
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
            .ok()??;
        if page.through_seq <= cursor {
            if page.through_seq != cursor {
                return None;
            }
            break;
        }
        fold.apply(&page.facts).ok()?;
        cursor = page.through_seq;
    }
    Some(ContextCheckpoint {
        header_fingerprint: request.claim.header().fingerprint().ok()?,
        through_seq: fold.through_seq(),
        fact_prefix_sha256: fold.fact_prefix_sha256(),
        bytes: fold.checkpoint_bytes().ok()?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use rsi_agent_session_protocol::{AgentPresetId, FrozenAgentSettings, SessionHeader, TurnId};
    use rsi_agent_turn_protocol::TurnClaimIssuer;
    use rsi_ai_protocol::ModelRef;
    use rsi_sandbox::SandboxMode;

    fn request(session: &str, turn: usize) -> CheckpointRequest {
        let session_id = SessionId::new(session).unwrap();
        let turn_id = TurnId::new(format!("turn-{turn}")).unwrap();
        let header = SessionHeader::new(
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
        )
    }

    #[tokio::test]
    async fn queued_updates_keep_their_original_fifo_position() {
        let scheduler = CheckpointScheduler::new();
        assert_eq!(
            scheduler.schedule(request("session-a", 1)),
            ScheduleOutcome::Scheduled
        );
        assert_eq!(
            scheduler.schedule(request("session-b", 1)),
            ScheduleOutcome::Scheduled
        );
        assert_eq!(
            scheduler.schedule(request("session-a", 2)),
            ScheduleOutcome::Coalesced
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
            ScheduleOutcome::Scheduled
        );
        assert_eq!(
            scheduler.schedule(request("session-b", 1)),
            ScheduleOutcome::Scheduled
        );
        let first = scheduler.next().await.unwrap();
        assert_eq!(first.session_id().as_str(), "session-a");
        assert_eq!(
            scheduler.schedule(request("session-a", 2)),
            ScheduleOutcome::Coalesced
        );
        assert_eq!(
            scheduler.schedule(request("session-a", 3)),
            ScheduleOutcome::Coalesced
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
            ScheduleOutcome::Scheduled
        );
        let active = scheduler.next().await.unwrap();
        for index in 0..MAXIMUM_PENDING_SESSIONS - 1 {
            assert_eq!(
                scheduler.schedule(request(&format!("session-{index}"), 1)),
                ScheduleOutcome::Scheduled
            );
        }
        assert_eq!(
            scheduler.schedule(request("session-active", 2)),
            ScheduleOutcome::Coalesced
        );
        assert_eq!(
            scheduler.schedule(request("session-over-capacity", 1)),
            ScheduleOutcome::AtCapacity
        );
        scheduler.finish(active.session_id());
    }

    #[tokio::test]
    async fn close_drains_admitted_work_and_rejects_later_requests() {
        let scheduler = CheckpointScheduler::new();
        assert_eq!(
            scheduler.schedule(request("session-a", 1)),
            ScheduleOutcome::Scheduled
        );
        let active = scheduler.next().await.unwrap();
        assert_eq!(
            scheduler.schedule(request("session-b", 1)),
            ScheduleOutcome::Scheduled
        );
        assert_eq!(
            scheduler.schedule(request("session-a", 2)),
            ScheduleOutcome::Coalesced
        );
        scheduler.close();
        assert_eq!(
            scheduler.schedule(request("session-c", 1)),
            ScheduleOutcome::Closed
        );
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
