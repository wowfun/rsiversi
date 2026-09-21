//! Optional effect-interval observation, independent of durable Turn settlement.
use crate::ControlledWorkStatus;
use async_trait::async_trait;
use rsi_agent_session_protocol::{SessionHeader, TurnId};
use rsi_meta_contract::LocalContract;
use std::{fmt, sync::Arc};
use tokio_util::sync::CancellationToken;

/// Immutable claim coordinates without execution or mutation authority.
#[derive(Clone, Debug)]
pub struct ExecutionObservationStart {
    /// Immutable source Header.
    pub header: SessionHeader,
    /// Exact Turn.
    pub turn: TurnId,
    /// Process-local claim generation.
    pub claim: u64,
    /// Acceptance Fact coordinate.
    pub accepted_seq: u64,
    /// Live source cut before observation.
    pub live_seq: u64,
    /// Prior execution or an unavailable initial-history proof prevents a full baseline claim.
    pub recovered: bool,
}

/// Evidence supplied after the claim drive and bounded cleanup observation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ExecutionObservationEnd {
    /// Begin returned before its deadline, before any effect was entered.
    pub begin_completed: bool,
    /// Exact-generation controlled work sample, independent of durable outcome.
    pub controlled_work: ControlledWorkStatus,
}

/// An admitted observation retains its own resources through actual completion.
#[async_trait]
pub trait ExecutionInterval: fmt::Debug + Send + Sync + 'static {
    /// Capture a baseline. Failure details belong to the observation owner.
    async fn begin(&self, cancellation: CancellationToken);
    /// Capture and publish final evidence; never changes the durable Turn outcome.
    async fn end(&self, evidence: ExecutionObservationEnd, cancellation: CancellationToken);
}

/// Explicit composition seam for non-authoritative effect-interval evidence.
#[async_trait]
pub trait ExecutionObserver: fmt::Debug + Send + Sync + 'static {
    /// Admit before entering effect owners. No claim mutation authority is transferred.
    async fn observe(
        &self,
        start: ExecutionObservationStart,
        cancellation: CancellationToken,
    ) -> Result<Arc<dyn ExecutionInterval>, String>;
}

/// Nominal Local dependency enabled explicitly by the Executor configuration.
#[derive(Debug)]
pub struct ExecutionObserverContract;
impl LocalContract for ExecutionObserverContract {
    const KEY: &'static str = "rsi.agent.execution.observer";
    type Service = dyn ExecutionObserver;
}
