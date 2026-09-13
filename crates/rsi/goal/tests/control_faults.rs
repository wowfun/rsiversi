#![cfg(not(target_arch = "wasm32"))]

use async_trait::async_trait;
use rsi_agent_goal::{GoalAction, RoundSettlement};
use rsi_agent_session_protocol::{
    CommandRevision, ContinuationInput, ContinuationProvenance, DomainRequestId, MessageId,
    SessionCommandInvocation, SessionCommandReceipt, SessionId,
};
use rsi_agent_turn_protocol::{
    ContinuationBinding, ContinuationLease, DomainMutationReceipt, MessageReceipt,
};
use rsi_goal::{
    GoalControl, GoalController, GoalDriverStage, GoalError, GoalResult, GoalService, GoalSession,
    GoalSnapshot,
};
use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicUsize, Ordering},
};
use tokio::sync::Semaphore;
use tokio_util::sync::CancellationToken;

#[derive(Debug)]
struct LostCommand {
    id: SessionId,
    gated: bool,
    calls: AtomicUsize,
    entered: Semaphore,
    release: Semaphore,
    queried: Semaphore,
    committed: AtomicBool,
}
impl LostCommand {
    fn new(gated: bool) -> Arc<Self> {
        Arc::new(Self {
            id: SessionId::new("lost-control").unwrap(),
            gated,
            calls: AtomicUsize::new(0),
            entered: Semaphore::new(0),
            release: Semaphore::new(0),
            queried: Semaphore::new(0),
            committed: AtomicBool::new(false),
        })
    }
}
#[async_trait]
impl GoalSession for LostCommand {
    fn session_id(&self) -> &SessionId {
        &self.id
    }
    async fn application_command(
        &self,
        invocation: SessionCommandInvocation,
    ) -> GoalResult<SessionCommandReceipt> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        if self.committed.load(Ordering::SeqCst) {
            return Ok(SessionCommandReceipt::draft_changed(&invocation, "a".repeat(64)).unwrap());
        }
        self.entered.add_permits(1);
        if self.gated {
            self.release.acquire().await.unwrap().forget();
        }
        Err(GoalError::OutcomeUnknown(
            "fixture lost command reply".into(),
        ))
    }
    async fn command_status(
        &self,
        _: &DomainRequestId,
    ) -> GoalResult<Option<SessionCommandReceipt>> {
        self.queried.add_permits(1);
        Err(GoalError::Backend("fixture receipt read failed".into()))
    }
    async fn snapshot(&self) -> GoalResult<GoalSnapshot> {
        if self.committed.load(Ordering::SeqCst) {
            self.queried.add_permits(1);
            self.release.acquire().await.unwrap().forget();
            return Err(GoalError::Backend(
                "post-commit snapshot unavailable".into(),
            ));
        }
        panic!("unknown command cannot assert current state")
    }
    async fn arm(&self, _: ContinuationBinding) -> GoalResult<ContinuationLease> {
        panic!("unknown command cannot arm")
    }
    async fn retain_for_settlement(&self, _: ContinuationBinding) -> GoalResult<ContinuationLease> {
        panic!("unknown create has no settlement authority")
    }
    async fn internal_command(
        &self,
        _: &ContinuationLease,
        _: SessionCommandInvocation,
        _: Option<ContinuationInput>,
    ) -> GoalResult<DomainMutationReceipt> {
        panic!("unknown create cannot allocate")
    }
    async fn internal_status(
        &self,
        _: &ContinuationLease,
        _: &DomainRequestId,
    ) -> GoalResult<Option<DomainMutationReceipt>> {
        panic!("unknown create has no reservation")
    }
    async fn submit(
        &self,
        _: &ContinuationLease,
        _: ContinuationInput,
        _: ContinuationProvenance,
    ) -> GoalResult<MessageReceipt> {
        panic!("unknown create cannot submit")
    }
    async fn message_status(&self, _: &MessageId) -> GoalResult<Option<MessageReceipt>> {
        panic!("unknown create has no message")
    }
    async fn wait_round(&self, _: &MessageId, _: CancellationToken) -> GoalResult<RoundSettlement> {
        panic!("unknown create has no round")
    }
    async fn discard_if_pending(
        &self,
        _: &ContinuationLease,
        _: &MessageId,
    ) -> GoalResult<Option<MessageReceipt>> {
        panic!("unknown create has no pending input")
    }
    async fn cancel(&self, _: &MessageId) -> GoalResult<()> {
        panic!("unknown create has no claimed input")
    }
}
fn request() -> GoalControl {
    GoalControl {
        request_id: DomainRequestId::new("original-control").unwrap(),
        expected_revision: CommandRevision::Draft { revision: 0 },
        action: GoalAction::Create {
            id: DomainRequestId::new("goal").unwrap(),
            objective: "Verify work".into(),
            constraints: String::new(),
            max_rounds: 2,
        },
    }
}

#[tokio::test]
async fn shutdown_after_control_admission_retains_the_original_unknown_identity() {
    let service = GoalService::default();
    let session = LostCommand::new(true);
    let waiter = tokio::spawn({
        let service = service.clone();
        let session = session.clone();
        async move { service.control(session, request()).await }
    });
    session.entered.acquire().await.unwrap().forget();
    service.stop().await.unwrap();
    assert!(
        matches!(waiter.await.unwrap(), Err(GoalError::OutcomeUnknown(id)) if id == "original-control")
    );
    assert_eq!(session.calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn shutdown_or_read_failure_after_a_successful_command_cannot_be_a_known_rejection() {
    for shutdown in [false, true] {
        let service = GoalService::default();
        let session = LostCommand::new(false);
        session.committed.store(true, Ordering::SeqCst);
        let waiter = tokio::spawn({
            let service = service.clone();
            let session = session.clone();
            async move { service.control(session, request()).await }
        });
        session.queried.acquire().await.unwrap().forget();
        if shutdown {
            service.stop().await.unwrap();
        } else {
            session.release.add_permits(1);
        }
        assert!(
            matches!(waiter.await.unwrap(), Err(GoalError::OutcomeUnknown(id)) if id == "original-control")
        );
        assert_eq!(session.calls.load(Ordering::SeqCst), 1);
        if !shutdown {
            service.stop().await.unwrap();
        }
    }
}
fn disarmed(service: &GoalService, session: &LostCommand) {
    let state = service.status(&session.id).unwrap();
    assert!(!state.armed);
    assert_eq!(state.stage, GoalDriverStage::Disarmed);
    assert!(state.message_id.is_none());
}

#[tokio::test]
async fn stopped_host_reports_shutdown_for_all_goal_entrypoints() {
    let service = GoalService::default();
    let session = LostCommand::new(false);
    service.stop().await.unwrap();
    assert!(matches!(
        service.control(session.clone(), request()).await,
        Err(GoalError::ShuttingDown)
    ));
    assert!(matches!(
        service.observe(&session.id),
        Err(GoalError::ShuttingDown)
    ));
    assert!(matches!(
        service.status(&session.id),
        Err(GoalError::ShuttingDown)
    ));
    assert_eq!(session.calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn lost_control_and_receipt_read_never_arm_or_replay() {
    let service = GoalService::default();
    let session = LostCommand::new(false);
    assert!(
        matches!(service.control(session.clone(), request()).await, Err(GoalError::OutcomeUnknown(id)) if id == "original-control")
    );
    assert_eq!(session.calls.load(Ordering::SeqCst), 1);
    assert_eq!(session.queried.available_permits(), 1);
    disarmed(&service, &session);
    service.stop().await.unwrap();
}

#[tokio::test]
async fn dropping_control_waiter_leaves_the_owned_call_to_reconcile() {
    let service = GoalService::default();
    let session = LostCommand::new(true);
    let waiter = tokio::spawn({
        let service = service.clone();
        let session = session.clone();
        async move { service.control(session, request()).await }
    });
    session.entered.acquire().await.unwrap().forget();
    waiter.abort();
    assert!(waiter.await.unwrap_err().is_cancelled());
    session.release.add_permits(1);
    tokio::time::timeout(std::time::Duration::from_secs(1), session.queried.acquire())
        .await
        .unwrap()
        .unwrap()
        .forget();
    assert_eq!(session.calls.load(Ordering::SeqCst), 1);
    disarmed(&service, &session);
    service.stop().await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn expired_owned_control_returns_original_unknown_identity_and_releases_admission() {
    let service = GoalService::default();
    let session = LostCommand::new(true);
    let waiter = tokio::spawn({
        let service = service.clone();
        let session = session.clone();
        async move { service.control(session, request()).await }
    });
    session.entered.acquire().await.unwrap().forget();
    tokio::time::advance(std::time::Duration::from_secs(30)).await;
    assert!(
        matches!(waiter.await.unwrap(), Err(GoalError::OutcomeUnknown(id)) if id == "original-control")
    );
    assert_eq!(session.queried.available_permits(), 0);
    disarmed(&service, &session);
    session.release.add_permits(1);
    assert!(matches!(
        service.control(session.clone(), request()).await,
        Err(GoalError::OutcomeUnknown(_))
    ));
    assert_eq!(
        session.calls.load(Ordering::SeqCst),
        2,
        "second call was explicitly requested by the test"
    );
    service.stop().await.unwrap();
}
