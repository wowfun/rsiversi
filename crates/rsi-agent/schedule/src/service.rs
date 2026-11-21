//! Host-generation capability. A durable domain is never itself live authority.
use async_trait::async_trait;
use rsi_agent_session_protocol::{DomainIdentity, DomainRequestId, MessageId, SessionId};
use rsi_agent_turn_protocol::{AgentCallerAuthority, DomainMutationReceipt};
use rsi_meta::LocalContract;
use std::{fmt, sync::Arc};
use tokio_util::sync::CancellationToken;

/// Active operator deliveries by linked Session, released when the target goes idle.
pub type DeliveryHolds =
    std::collections::BTreeMap<SessionId, std::collections::BTreeSet<DomainRequestId>>;

/// Captured Host epoch, revoked on withdrawal.
#[derive(Clone, Debug)]
pub struct ScheduleEpoch {
    seal: Arc<()>,
    stopped: CancellationToken,
}
impl ScheduleEpoch {
    /// Creates a Host-owned epoch; safe Rust Local services are trusted peers.
    pub fn new(stopped: CancellationToken) -> Self {
        Self {
            seal: Arc::new(()),
            stopped,
        }
    }
    /// Exact Host generation identity, not a serializable credential.
    pub fn same_epoch(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.seal, &other.seal)
    }
    /// Whether this Host still admits new live work.
    pub fn is_open(&self) -> bool {
        !self.stopped.is_cancelled()
    }
}
/// Injected UTC clock and cancellation-aware wait, independent of Node.
#[async_trait]
pub trait ScheduleClock: fmt::Debug + Send + Sync + 'static {
    /// Current Unix time in milliseconds.
    fn now_ms(&self) -> u64;
    /// Waits until the absolute UTC deadline, or cancellation.
    async fn wait_until(&self, at_ms: u64, cancellation: CancellationToken);
}
/// Native Host service consumed by model-facing Schedule Tools.
#[async_trait]
pub trait ScheduleController: fmt::Debug + Send + Sync + 'static {
    /// Captures the current generation before durable mutation.
    fn epoch(&self) -> ScheduleEpoch;
    /// Supplies the exact clock shared by creation and timer driving.
    fn clock(&self) -> Arc<dyn ScheduleClock>;
    /// Reports process-local arming only; this never resumes execution.
    fn armed(&self, session: &SessionId) -> bool;
    /// Latest bounded driver or cleanup failure for this Session, if any.
    fn failure(&self, session: &SessionId) -> Option<String>;
    /// Reports changes to the execution holds shared by the task manager and admission.
    fn delivery_holds(&self) -> tokio::sync::watch::Receiver<DeliveryHolds>;
    /// Checks target availability and recovers a previous uncompleted delivery.
    async fn check_operator_run(
        &self,
        session: &SessionId,
        task: &DomainRequestId,
        previous: Option<&MessageId>,
    ) -> Result<(), String>;
    /// Tracks an acknowledged follow-up through the target Session's idle boundary.
    fn hold_delivery(&self, session: &SessionId, task: &DomainRequestId);
    /// Recomputes an existing future timer after an operator edit. An accepted
    /// delivery keeps its owner; this does not arm a disarmed Session.
    async fn refresh_pending(&self, session: &SessionId);
    /// Revokes and joins the previous owner before a validated human mutation is committed.
    /// A failed mutation leaves retained intent disarmed for explicit reconciliation.
    async fn disarm_for_mutation(
        &self,
        epoch: &ScheduleEpoch,
        session: &SessionId,
    ) -> Result<bool, String>;
    /// Reconciles an exact Tool mutation before replacing its live timer owner.
    async fn arm_after_commit(
        &self,
        epoch: &ScheduleEpoch,
        caller: &AgentCallerAuthority,
        domain: &DomainIdentity,
        receipt: &DomainMutationReceipt,
        cancellation: CancellationToken,
    ) -> Result<bool, String>;
}
/// Process-local Host Schedule capability, absent in portable/ACP compositions.
#[derive(Debug)]
pub struct ScheduleControllerContract;
impl LocalContract for ScheduleControllerContract {
    const KEY: &'static str = "rsi.schedule.controller";
    type Service = dyn ScheduleController;
}
