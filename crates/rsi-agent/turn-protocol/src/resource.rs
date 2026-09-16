//! Independent finite Session resource reads without a Turn claim.

use async_trait::async_trait;
use rsi_agent_session_protocol::{SessionId, ValidatedResourceRequest, ValidatedResourceResponse};
use rsi_meta_contract::LocalContract;
use std::fmt;
use tokio_util::sync::CancellationToken;

/// Read-only generation selection; no Session hydration or execution is implied.
#[async_trait]
pub trait SessionResources: fmt::Debug + Send + Sync + 'static {
    /// Reads from the actual resident pin or a bounded cold composition preparation.
    /// Caller cancellation reports `Cancelled`; owner shutdown reports `ShuttingDown`.
    async fn read_resource(
        &self,
        session: &SessionId,
        request: ValidatedResourceRequest,
        cancellation: CancellationToken,
    ) -> crate::Result<ValidatedResourceResponse>;
}

/// Nominal Local resource read contract, separate from pure projections.
#[derive(Debug)]
pub struct SessionResourcesContract;
impl LocalContract for SessionResourcesContract {
    const KEY: &'static str = "rsi.agent.session.resources";
    type Service = dyn SessionResources;
}
