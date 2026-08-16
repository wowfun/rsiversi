use std::fmt;
use std::pin::Pin;
use std::sync::Arc;

use anyhow::Result;
use async_trait::async_trait;
use futures_util::Stream;

use crate::protocol::{
    CommandEnvelope, CommandOutcomeEnvelope, EventEnvelope, GraphRevision, InstanceId,
    ServiceOpenRequest, StreamEnvelope, rejected, validate_outcome,
};

pub type SharedHost = Arc<dyn HostApi>;
pub type HostEventStream = Pin<Box<dyn Stream<Item = Result<EventEnvelope>> + Send + 'static>>;
pub type BoxHostServiceStream = Box<dyn HostServiceStream>;

#[async_trait]
pub trait HostServiceStream: fmt::Debug + Send + 'static {
    fn provider(&self) -> &InstanceId;

    async fn send(&mut self, payload: &[u8]) -> Result<()>;

    async fn grant_credit(&mut self, bytes: u64) -> Result<()>;

    async fn recv(&mut self) -> Option<Result<StreamEnvelope>>;

    async fn half_close(&mut self) -> Result<()>;

    async fn cancel(&mut self, reason: String) -> Result<()>;
}

/// The only composition-facing seam used by the transport modules.
///
/// The production implementation delegates to `rsi_meta::CompositionHost`.
/// Tests use an in-memory implementation so framing/auth failures cannot mutate
/// the composition graph accidentally.
#[async_trait]
pub trait HostApi: fmt::Debug + Send + Sync + 'static {
    async fn submit(&self, command: CommandEnvelope) -> Result<CommandOutcomeEnvelope>;

    async fn subscribe(&self, after_cursor: u64) -> Result<HostEventStream>;

    fn graph_revision(&self) -> GraphRevision;

    fn token_generation(&self) -> u64;

    fn open_service(&self, _request: ServiceOpenRequest) -> Result<BoxHostServiceStream> {
        anyhow::bail!("service streams are unavailable on this host")
    }

    async fn shutdown(&self) -> Result<()>;

    async fn wait_terminated(&self) -> Result<()> {
        Ok(())
    }

    /// Waits indefinitely for host termination without initiating it.
    ///
    /// Transport-only test hosts are inert by default; the production adapter
    /// overrides this to observe the registry task.
    async fn monitor_terminated(&self) -> Result<()> {
        std::future::pending().await
    }
}

#[async_trait]
impl HostServiceStream for rsi_meta::ServiceStream {
    fn provider(&self) -> &InstanceId {
        self.provider()
    }

    async fn send(&mut self, payload: &[u8]) -> Result<()> {
        self.send(payload).await.map_err(Into::into)
    }

    async fn grant_credit(&mut self, bytes: u64) -> Result<()> {
        self.grant_credit(bytes).await.map_err(Into::into)
    }

    async fn recv(&mut self) -> Option<Result<StreamEnvelope>> {
        self.recv().await.map(|result| result.map_err(Into::into))
    }

    async fn half_close(&mut self) -> Result<()> {
        self.half_close().await.map_err(Into::into)
    }

    async fn cancel(&mut self, reason: String) -> Result<()> {
        self.cancel(reason).await.map_err(Into::into)
    }
}

pub async fn submit_with_rejection(
    host: &dyn HostApi,
    command: CommandEnvelope,
) -> CommandOutcomeEnvelope {
    let command_id = command.command_id.clone();
    match host.submit(command).await {
        Ok(result) if result.command_id == command_id && validate_outcome(&result).is_ok() => {
            result
        }
        Ok(result) => rejected(
            command_id,
            host.graph_revision(),
            "invalid_host_outcome",
            format!(
                "host returned an invalid result envelope for command_id {:?}",
                result.command_id
            ),
        ),
        Err(error) => {
            let (code, message, details) = match error.downcast_ref::<rsi_meta::HostError>() {
                Some(rsi_meta::HostError::OperationRejected {
                    code,
                    message,
                    details,
                }) => (code.clone(), message.clone(), details.clone()),
                Some(rsi_meta::HostError::CommandIdConflict { .. }) => (
                    "operation_id_conflict".to_owned(),
                    format!("{error:#}"),
                    std::collections::BTreeMap::new(),
                ),
                _ => (
                    "host_error".to_owned(),
                    format!("{error:#}"),
                    std::collections::BTreeMap::new(),
                ),
            };
            let mut outcome = rejected(command_id, host.graph_revision(), code, message);
            if let crate::protocol::CommandOutcome::Rejected {
                details: outcome_details,
                ..
            } = &mut outcome.payload
            {
                *outcome_details = details;
            }
            outcome
        }
    }
}
