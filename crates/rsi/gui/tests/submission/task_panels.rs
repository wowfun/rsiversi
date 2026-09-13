use super::*;
use futures_util::StreamExt;
use rsi_agent_goal::{GoalAction, GoalState};
use rsi_goal::{GoalControl, GoalControlReceipt, GoalDriverStage, GoalLiveState};
use rsi_session_protocol::SessionError;
use serde_json::{Value, json};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

#[path = "task_panels/tests.rs"]
mod tests;

#[derive(Debug)]
pub(super) struct Scenario {
    enabled: AtomicBool,
    state: Mutex<GoalState>,
    controls: Mutex<Vec<GoalControl>>,
    changed: tokio::sync::watch::Sender<u64>,
    live: Mutex<GoalLiveState>,
    live_changed: tokio::sync::watch::Sender<()>,
    block_reply: AtomicBool,
    reply_entered: tokio::sync::Notify,
    reply_release: tokio::sync::Notify,
    waiting_observed: tokio::sync::Notify,
    block_projections: AtomicBool,
    projection_entered: tokio::sync::Notify,
    projection_release: tokio::sync::Notify,
    unknown: AtomicBool,
    receipt_unavailable: AtomicBool,
    receipt_queries: Mutex<Vec<DomainRequestId>>,
    jobs_active: AtomicBool,
    jobs_reads: Mutex<Vec<TurnJobsRequest>>,
    block_jobs: AtomicBool,
    reading_jobs: AtomicUsize,
    projections: ProjectionRetention,
    jobs: JobsRetention,
}
impl Default for Scenario {
    fn default() -> Self {
        Self {
            enabled: AtomicBool::new(false),
            state: Mutex::default(),
            controls: Mutex::default(),
            changed: tokio::sync::watch::channel(0).0,
            live: Mutex::default(),
            live_changed: tokio::sync::watch::channel(()).0,
            block_reply: AtomicBool::new(false),
            reply_entered: tokio::sync::Notify::new(),
            reply_release: tokio::sync::Notify::new(),
            waiting_observed: tokio::sync::Notify::new(),
            block_projections: AtomicBool::new(false),
            projection_entered: tokio::sync::Notify::new(),
            projection_release: tokio::sync::Notify::new(),
            unknown: AtomicBool::new(false),
            receipt_unavailable: AtomicBool::new(false),
            receipt_queries: Mutex::default(),
            jobs_active: AtomicBool::new(false),
            jobs_reads: Mutex::default(),
            block_jobs: AtomicBool::new(false),
            reading_jobs: AtomicUsize::new(0),
            projections: ProjectionRetention::default(),
            jobs: JobsRetention::default(),
        }
    }
}
impl Scenario {
    pub(super) fn record_query(&self, request: &DomainRequestId) -> bool {
        self.receipt_queries.lock().unwrap().push(request.clone());
        self.receipt_unavailable.load(Ordering::SeqCst)
    }
    pub(super) fn enabled(&self) -> bool {
        self.enabled.load(Ordering::SeqCst)
    }
    pub(super) fn jobs_active(&self) -> bool {
        self.jobs_active.load(Ordering::SeqCst)
    }
    pub(super) fn live(&self) -> rsi_session_protocol::Result<GoalLiveState> {
        if !self.enabled() {
            return Err(SessionError::NotFound("Goal fixture".into()));
        }
        Ok(self.live.lock().unwrap().clone())
    }
    pub(super) async fn reply(&self, captured: GoalLiveState) -> GoalLiveState {
        if self.block_reply.swap(false, Ordering::SeqCst) {
            self.reply_entered.notify_one();
            self.reply_release.notified().await;
        }
        captured
    }
    pub(super) async fn control(
        &self,
        backend: &Backend,
        request: GoalControl,
    ) -> rsi_session_protocol::Result<GoalControlReceipt> {
        let header = backend.header().await?;
        let revision = *self.changed.borrow();
        self.controls.lock().unwrap().push(request.clone());
        if request.expected_revision != (CommandRevision::Draft { revision }) {
            return Err(SessionError::CommandRevisionConflict {
                expected: request.expected_revision,
                actual: CommandRevision::Draft { revision },
            });
        }
        self.state
            .lock()
            .unwrap()
            .apply(
                request.action.clone(),
                header.settings().turn_budget(),
                true,
            )
            .map_err(SessionError::Invalid)?;
        let armed = matches!(
            request.action,
            GoalAction::Create { .. } | GoalAction::Resume { .. }
        );
        let live = GoalLiveState {
            armed,
            stage: if armed {
                GoalDriverStage::Reserving
            } else {
                GoalDriverStage::Disarmed
            },
            ..GoalLiveState::default()
        };
        *self.live.lock().unwrap() = live.clone();
        let invocation = request.invocation().unwrap();
        let command =
            SessionCommandReceipt::draft_changed(&invocation, header.fingerprint().unwrap())
                .unwrap();
        *backend.command_receipt.lock().unwrap() = Some(command.clone());
        self.changed.send_replace(revision + 1);
        self.live_changed.send_replace(());
        if self.unknown.load(Ordering::SeqCst) {
            return Err(SessionError::CommandOutcomeUnknown {
                request_id: request.request_id,
            });
        }
        let live = self.reply(live).await;
        Ok(GoalControlReceipt { command, live })
    }
    pub(super) fn observe_goal(self: &Arc<Self>) -> rsi_session_protocol::Result<GoalStream> {
        let initial = self.live()?;
        let stream = futures_util::stream::unfold(
            (self.clone(), self.live_changed.subscribe(), false),
            |(owner, mut changed, delivered_waiting)| async move {
                // A subsequent poll proves the consumer processed the preceding item.
                if delivered_waiting {
                    owner.waiting_observed.notify_one();
                }
                changed.changed().await.ok()?;
                changed.borrow_and_update();
                let value = owner.live();
                let delivered_waiting = value
                    .as_ref()
                    .is_ok_and(|live| live.stage == GoalDriverStage::Waiting);
                Some((value, (owner, changed, delivered_waiting)))
            },
        );
        Ok(Box::pin(
            futures_util::stream::iter([Ok(initial)]).chain(stream),
        ))
    }
    fn snapshot(&self, header: &SessionHeader) -> rsi_session_protocol::Result<ProjectionSnapshot> {
        self.projections.reserve_capture()?.retain(
            SessionProjectionSnapshot::new(
                header.session_id().clone(),
                header.fingerprint().unwrap(),
                "b".repeat(64),
                ProjectionCursor::Draft {
                    revision: *self.changed.borrow(),
                },
                vec![ProjectionEntry::value(
                    ContributionId::new(rsi_agent_goal::GOAL_PROJECTION).unwrap(),
                    ProjectionValue::encode(&*self.state.lock().unwrap()).unwrap(),
                )],
            )
            .unwrap(),
        )
    }
    pub(super) fn projections(
        self: &Arc<Self>,
        header: SessionHeader,
    ) -> rsi_session_protocol::Result<ProjectionStream> {
        let initial = self.snapshot(&header)?;
        let stream = futures_util::stream::unfold(
            (self.clone(), header, self.changed.subscribe()),
            |(owner, header, mut changed)| async move {
                changed.changed().await.ok()?;
                changed.borrow_and_update();
                if owner.block_projections.swap(false, Ordering::SeqCst) {
                    owner.projection_entered.notify_one();
                    owner.projection_release.notified().await;
                }
                Some((owner.snapshot(&header), (owner, header, changed)))
            },
        );
        Ok(Box::pin(
            futures_util::stream::iter([Ok(initial)]).chain(stream),
        ))
    }
    pub(super) async fn jobs(
        &self,
        backend: &Backend,
        request: TurnJobsRequest,
    ) -> rsi_session_protocol::Result<JobsSnapshot> {
        self.jobs_reads.lock().unwrap().push(request.clone());
        if self.block_jobs.load(Ordering::SeqCst) {
            struct Active<'a>(&'a AtomicUsize);
            impl Drop for Active<'_> {
                fn drop(&mut self) {
                    self.0.fetch_sub(1, Ordering::SeqCst);
                }
            }
            self.reading_jobs.fetch_add(1, Ordering::SeqCst);
            let _active = Active(&self.reading_jobs);
            std::future::pending::<()>().await;
        }
        assert_eq!(request.turn_id.as_str(), "jobs-turn");
        let header = backend.header().await?;
        let next = request.after.is_some();
        self.jobs.reserve_capture()?.retain(TurnJobsPage {
            session_id: header.session_id().clone(), header_sha256: header.fingerprint().unwrap(), turn_id: request.turn_id, generation: 7, after: request.after,
            jobs: vec![serde_json::from_value(json!({"id":if next {"job-b"} else {"job-a"},"name":"compile","producer":"bash","status":"running","requires_report":true,"reported":false,"terminal":null,"output_retained":true})).unwrap()], has_more: !next,
        })
    }
}
