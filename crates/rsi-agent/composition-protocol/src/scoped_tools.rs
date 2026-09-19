//! Claim-local discovery and preparation over an unchanged generation runtime.
use async_trait::async_trait;
use rsi_tools_protocol::{
    PreparedToolCall, RetainedToolResult, ToolCall, ToolDefinition, ToolError, ToolResultIdentity,
    ToolRuntime,
};
use std::{collections::BTreeSet, sync::Arc};
use tokio_util::sync::CancellationToken;

#[derive(Debug)]
pub(super) struct ScopedTools {
    pub inner: Arc<dyn ToolRuntime>,
    pub allowed: BTreeSet<String>,
}
#[async_trait]
impl ToolRuntime for ScopedTools {
    fn definitions(&self) -> Vec<ToolDefinition> {
        self.inner
            .definitions()
            .into_iter()
            .filter(|tool| self.allowed.contains(tool.name()))
            .collect()
    }
    fn prepare(
        &self,
        invocation_id: &str,
        call: ToolCall,
    ) -> rsi_tools_protocol::Result<Box<dyn PreparedToolCall>> {
        if !self.allowed.contains(&call.name) {
            return Err(ToolError::Unknown(call.name));
        }
        self.inner.prepare(invocation_id, call)
    }
    fn query(
        &self,
        identity: &ToolResultIdentity,
    ) -> rsi_tools_protocol::Result<RetainedToolResult> {
        self.inner.query(identity)
    }
    async fn wait(
        &self,
        identity: &ToolResultIdentity,
        cancellation: CancellationToken,
    ) -> rsi_tools_protocol::Result<RetainedToolResult> {
        self.inner.wait(identity, cancellation).await
    }
    fn commit(&self, identity: &ToolResultIdentity) -> rsi_tools_protocol::Result<()> {
        self.inner.commit(identity)
    }
}
