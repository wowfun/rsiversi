//! Pure reporting Tool with resident-Turn-owned exact retained results.
use async_trait::async_trait;
use rsi_agent_session_protocol::{OutputContract, REPORT_RESULT_TOOL};
use rsi_tools_protocol::{
    PreparedToolCall, RetainedToolResult, ToolCall, ToolDefinition, ToolError, ToolResult,
    ToolResultIdentity, ToolRuntime, ToolStart,
};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex},
};
use tokio_util::sync::CancellationToken;

#[derive(Debug)]
pub(super) struct ReportTools {
    inner: Arc<dyn ToolRuntime>,
    report: Arc<Report>,
}
#[derive(Debug)]
struct Report {
    owner: String,
    definition: ToolDefinition,
    contract: OutputContract,
    retained: Mutex<BTreeMap<ToolResultIdentity, ToolResult>>,
}
impl ReportTools {
    pub fn new(
        inner: Arc<dyn ToolRuntime>,
        owner: &str,
        contract: OutputContract,
    ) -> super::Result<Self> {
        if inner.program_role(REPORT_RESULT_TOOL).is_some() {
            return Err(super::AgentCompositionError::InvalidInput(
                "reserved report_result name is already registered".into(),
            ));
        }
        let definition = ToolDefinition::new(REPORT_RESULT_TOOL,
            "Submit the required structured result and finish this activation. A valid submission ends execution immediately. Correct validation errors and retry within the current budget.", contract.schema().clone())
            .map_err(|error| super::AgentCompositionError::InvalidInput(error.to_string()))?;
        Ok(Self {
            inner,
            report: Arc::new(Report {
                owner: format!("{:x}", Sha256::digest(owner.as_bytes())),
                definition,
                contract,
                retained: Mutex::new(BTreeMap::new()),
            }),
        })
    }
}
#[derive(Debug)]
struct PreparedReport {
    report: Arc<Report>,
    identity: ToolResultIdentity,
    arguments: serde_json::Value,
}
#[async_trait]
impl PreparedToolCall for PreparedReport {
    fn identity(&self) -> &ToolResultIdentity {
        &self.identity
    }
    async fn start(self: Box<Self>, start: ToolStart) -> rsi_tools_protocol::Result<ToolResult> {
        if start.cancellation.is_cancelled() {
            return Err(ToolError::Cancelled);
        }
        let mut retained = self
            .report
            .retained
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(result) = retained.get(&self.identity) {
            return Ok(result.clone());
        }
        // Report calls are exclusive; at most one unsettled result is expected.
        // The defensive finite admission also covers direct Runtime consumers.
        if retained.len() >= 8 {
            return Err(ToolError::Capacity);
        }
        let result = match self.report.contract.validate_value(&self.arguments) {
            Ok(()) => ToolResult::new(self.arguments, vec![], false)?,
            Err(error) => ToolResult::new(
                serde_json::json!({"error": error.to_string()}),
                vec![],
                true,
            )?,
        };
        retained.insert(self.identity.clone(), result.clone());
        Ok(result)
    }
}
#[async_trait]
impl ToolRuntime for ReportTools {
    fn program_role(&self, name: &str) -> Option<rsi_tools_protocol::ToolProgramRole> {
        if name == REPORT_RESULT_TOOL {
            Some(self.report.definition.program_role())
        } else {
            self.inner.program_role(name)
        }
    }
    fn program_roles(
        &self,
    ) -> std::collections::BTreeMap<String, rsi_tools_protocol::ToolProgramRole> {
        self.inner.program_roles()
    }
    fn definition(&self, name: &str) -> Option<ToolDefinition> {
        if name == REPORT_RESULT_TOOL {
            Some(self.report.definition.clone())
        } else {
            self.inner.definition(name)
        }
    }
    fn output_declarations(
        &self,
    ) -> std::collections::BTreeMap<String, rsi_tools_protocol::ToolOutputDeclaration> {
        self.inner.output_declarations()
    }
    fn definitions(&self) -> Vec<ToolDefinition> {
        let mut definitions = self.inner.definitions();
        definitions.push(self.report.definition.clone());
        definitions
    }
    fn prepare(
        &self,
        invocation_id: &str,
        call: ToolCall,
    ) -> rsi_tools_protocol::Result<Box<dyn PreparedToolCall>> {
        if call.name != REPORT_RESULT_TOOL {
            return self.inner.prepare(invocation_id, call);
        }
        call.validate()?;
        let request = serde_json::to_vec(&call)
            .map_err(|error| ToolError::InvalidInput(error.to_string()))?;
        let identity = ToolResultIdentity::new(
            &self.report.owner,
            invocation_id,
            &call.id,
            format!("{:x}", Sha256::digest(request)),
        )?;
        Ok(Box::new(PreparedReport {
            report: Arc::clone(&self.report),
            identity,
            arguments: call.arguments,
        }))
    }
    fn query(
        &self,
        identity: &ToolResultIdentity,
    ) -> rsi_tools_protocol::Result<RetainedToolResult> {
        if identity.owner_id() != self.report.owner {
            return self.inner.query(identity);
        }
        Ok(self
            .report
            .retained
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(identity)
            .cloned()
            .map_or(RetainedToolResult::Absent, RetainedToolResult::Returned))
    }
    async fn wait(
        &self,
        identity: &ToolResultIdentity,
        cancellation: CancellationToken,
    ) -> rsi_tools_protocol::Result<RetainedToolResult> {
        if identity.owner_id() != self.report.owner {
            return self.inner.wait(identity, cancellation).await;
        }
        // No await occurs inside start: its result is either absent or settled.
        self.query(identity)
    }
    fn commit(&self, identity: &ToolResultIdentity) -> rsi_tools_protocol::Result<()> {
        if identity.owner_id() != self.report.owner {
            return self.inner.commit(identity);
        }
        self.report
            .retained
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(identity);
        Ok(())
    }
}
