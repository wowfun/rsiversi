use crate::{
    AgentCompositionPin, AgentGenerationOwner, ContributionCatalog, ContributionKind,
    ContributionRegistration,
};
use async_trait::async_trait;
use rsi_tools_protocol::{AroundTool, NativeToolPlan, ToolBody, ToolExecution, ToolResult};
use std::{fmt, sync::Arc};

/// Native around-execution callback retained by the Agent composition generation.
#[async_trait]
pub trait ToolAroundContributor: fmt::Debug + Send + Sync + 'static {
    /// Awaits the remaining chain once, or returns an outcome without invoking it.
    ///
    /// # Errors
    /// Propagates callback or body failures to the Tool's retained outcome.
    async fn execute(&self, next: ToolContinuation<'_>) -> rsi_tools_protocol::Result<ToolResult>;
}

/// A borrowed native callback chain and its single-use admitted body.
#[derive(Debug)]
pub struct ToolContinuation<'a> {
    name: &'a str,
    remaining: &'a [ContributionRegistration],
    application: Option<Box<dyn AroundTool>>,
    body: ToolBody<'a>,
}

impl ToolContinuation<'_> {
    /// Exact name of the admitted registration.
    #[must_use]
    pub fn name(&self) -> &str {
        self.name
    }

    /// Immutable admitted arguments, without a second payload copy.
    #[must_use]
    pub fn arguments(&self) -> &serde_json::Value {
        self.body.arguments()
    }

    /// Exact invocation authority and cooperative cancellation.
    #[must_use]
    pub fn execution(&self) -> &ToolExecution {
        self.body.execution()
    }

    /// Consumes this continuation and awaits the remaining callbacks and body.
    ///
    /// # Errors
    /// Returns the callback or body failure, including cancellation before body entry.
    pub async fn run(mut self) -> rsi_tools_protocol::Result<ToolResult> {
        while let Some((entry, remaining)) = self.remaining.split_first() {
            self.remaining = remaining;
            if let ContributionKind::ToolAround(callback) = entry.kind() {
                return callback.execute(self).await;
            }
        }
        match self.application {
            Some(application) => application.execute(self.body).await,
            None => self.body.run().await,
        }
    }
}

#[derive(Debug)]
struct NativeToolChain {
    _generation: Arc<dyn AgentGenerationOwner>,
    name: String,
    catalog: ContributionCatalog,
    application: Option<Box<dyn AroundTool>>,
}

#[async_trait]
impl AroundTool for NativeToolChain {
    async fn withdrawn(&mut self) {
        match &mut self.application {
            Some(application) => application.withdrawn().await,
            None => std::future::pending::<()>().await,
        }
    }

    async fn execute(
        self: Box<Self>,
        body: ToolBody<'_>,
    ) -> rsi_tools_protocol::Result<ToolResult> {
        ToolContinuation {
            name: &self.name,
            remaining: self.catalog.entries(),
            application: self.application,
            body,
        }
        .run()
        .await
    }
}

impl AgentCompositionPin {
    /// Installs this generation's native wrappers around the existing execution plan.
    /// An empty native chain leaves the plan untouched and allocates nothing.
    pub fn wrap_tool_execution(&self, plan: &mut NativeToolPlan<'_>) {
        if self
            .contributions()
            .entries()
            .iter()
            .any(|entry| matches!(entry.kind(), ContributionKind::ToolAround(_)))
        {
            *plan.continuation = Some(Box::new(NativeToolChain {
                _generation: self.owner.clone(),
                name: plan.call.name.clone(),
                catalog: self.contributions().clone(),
                application: plan.continuation.take(),
            }));
        }
    }
}
