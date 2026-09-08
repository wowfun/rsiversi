//! Independent read-only extension view capture.

use async_trait::async_trait;
use rsi_agent_session_protocol::{SessionId, SessionProjectionSnapshot};
use rsi_meta_contract::LocalContract;
use std::fmt;

/// Coalesced requery hints with no durable payload or execution capability.
pub type SessionProjectionChanges = std::pin::Pin<Box<dyn futures_util::Stream<Item = ()> + Send>>;

/// Read-only generation selection and complete derived snapshot capture.
#[async_trait]
pub trait SessionProjections: fmt::Debug + Send + Sync + 'static {
    /// Subscribes before capture; includes future publication and ends on retirement.
    fn watch_projection_changes(
        &self,
        session_id: &SessionId,
    ) -> crate::Result<SessionProjectionChanges>;
    /// Reads a consistent current cut without hydrating or claiming the Session.
    async fn projection_snapshot(
        &self,
        session_id: &SessionId,
    ) -> crate::Result<SessionProjectionSnapshot>;
}

/// Nominal Local contract, independently consumable from execution and commands.
#[derive(Debug)]
pub struct SessionProjectionsContract;
impl LocalContract for SessionProjectionsContract {
    const KEY: &'static str = "rsi.agent.session.projections";
    type Service = dyn SessionProjections;
}
