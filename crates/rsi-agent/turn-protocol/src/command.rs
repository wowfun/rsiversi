//! Session-owned command dispatch and canonical result lookup.

use crate::{DomainMutationReceipt, PreparedResumeSession, Result};
use async_trait::async_trait;
use rsi_agent_session_protocol::{
    DomainRequestId, SessionCommandInvocation, SessionCommandsView, SessionId,
};
use rsi_meta_contract::LocalContract;
use std::fmt;

/// Trusted process-local command service; transports add their own authentication.
#[async_trait]
pub trait SessionCommands: fmt::Debug + Send + Sync + 'static {
    /// Lists commands from the exact generation selected by this resume authority.
    async fn list(&self, session: PreparedResumeSession) -> Result<SessionCommandsView>;
    /// Executes one immutable request through bounded callback and atomic commit admission.
    async fn execute(
        &self,
        session: PreparedResumeSession,
        invocation: SessionCommandInvocation,
    ) -> Result<DomainMutationReceipt>;
    /// Reads a canonical command receipt; absence never authorizes implicit replay.
    async fn query(
        &self,
        session_id: &SessionId,
        request_id: &DomainRequestId,
    ) -> Result<Option<DomainMutationReceipt>>;
}

/// Nominal Local contract, independently consumable from execution services.
#[derive(Debug)]
pub struct SessionCommandsContract;
impl LocalContract for SessionCommandsContract {
    const KEY: &'static str = "rsi.agent.session.commands";
    type Service = dyn SessionCommands;
}
