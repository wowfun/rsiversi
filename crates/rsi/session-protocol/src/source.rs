use crate::{Result, SessionError, SessionReadLease};
use async_trait::async_trait;
use rsi_execution::ExecutionLease;
use std::fmt;

/// One acquired Session source and its finite activity/authorization lifetime.
#[derive(Debug)]
pub struct SessionSourceLease {
    read: SessionReadLease,
    execution: ExecutionLease,
}
impl SessionSourceLease {
    /// Combines independently admitted owners only when they name the same location.
    pub fn new(read: SessionReadLease, execution: ExecutionLease) -> Result<Self> {
        if read.header().coordinates().location() != execution.binding().location() {
            return Err(SessionError::Api(rsi_api_protocol::ApiError::Unauthorized));
        }
        Ok(Self { read, execution })
    }
    /// Fixed Header of the admitted source.
    pub fn header(&self) -> &rsi_agent_session_protocol::SessionHeader {
        self.read.header()
    }
    /// Original lease to retain through source and process operations.
    pub fn execution(&self) -> &ExecutionLease {
        &self.execution
    }
    /// Session-provider retirement cancels subsequent source work.
    pub fn retiring(&self) -> &tokio_util::sync::CancellationToken {
        self.read.retiring()
    }
}
/// Server-only source owner already bound to one Session and actual authenticated caller.
#[async_trait]
pub trait SessionSource: fmt::Debug + Send + Sync + 'static {
    /// Checks current Use and acquires one finite source lifetime.
    async fn acquire(&self) -> Result<SessionSourceLease>;
}
/// Non-wire source capability published only in the actual bound Session target.
#[derive(Debug)]
pub struct SessionSourceContract;
impl rsi_meta_contract::LocalContract for SessionSourceContract {
    const KEY: &'static str = "rsi.session.source";
    type Service = dyn SessionSource;
}
