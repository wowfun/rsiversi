//! Non-waking Session context injection through ordinary command publication.
#![forbid(unsafe_code)]
#![warn(missing_docs)]

use async_trait::async_trait;
use rsi_agent_composition_protocol::{
    CommandInput, ContributionError, ContributionKind, ContributionRegistrarContract,
    ContributionRegistration, ContributionResult, SessionCommand, SessionCommandContext,
    SessionCommandOutput, SessionCommandRegistration,
};
use rsi_agent_session_protocol::{
    AgentMessageContent, CommandArguments, ContributionId, MessageDelivery,
    SessionCommandDescriptor,
};
use rsi_meta::{ActivationPlan, ConfigValue, MetaError, PluginFactory, PreparedActivation};
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

mod seed;

/// Command contribution identity.
pub const COMMAND: &str = "rsi.context-input.command";
/// Discoverable command name.
pub const COMMAND_NAME: &str = "context-input";

/// Agent-only context commands and supplemental seed contribution.
#[derive(Clone, Copy, Debug, Default)]
pub struct ContextInputFactory;

#[derive(Debug)]
struct ContextInput(rsi_meta::Context);

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct Arguments {
    content: Vec<AgentMessageContent>,
    attribution: Option<rsi_agent_session_protocol::MessageAttribution>,
}

#[async_trait]
impl SessionCommand for ContextInput {
    async fn execute(
        &self,
        _: &SessionCommandContext,
        arguments: &CommandArguments,
        cancellation: CancellationToken,
    ) -> ContributionResult<SessionCommandOutput> {
        if cancellation.is_cancelled() {
            return Err(ContributionError::Closed);
        }
        let arguments: Arguments = serde::Deserialize::deserialize(arguments.value())
            .map_err(|error| ContributionError::Invalid(error.to_string()))?;
        let input = CommandInput {
            delivery: MessageDelivery::NextStep,
            content: arguments.content,
            attribution: arguments.attribution,
        };
        input.verify_attachments(&self.0, &cancellation).await?;
        Ok(SessionCommandOutput {
            inputs: vec![input],
            ..SessionCommandOutput::default()
        })
    }
}

#[async_trait]
impl PluginFactory for ContextInputFactory {
    fn prepare(&self, desired: &ConfigValue) -> rsi_meta::Result<PreparedActivation> {
        if !desired.is_null() {
            return Err(MetaError::InvalidInput(
                "context input takes no configuration".into(),
            ));
        }
        Ok(PreparedActivation::new(desired.clone())
            .requiring_local::<rsi_agent_composition_protocol::DomainRegistrarContract>()
            .requiring_local::<ContributionRegistrarContract>())
    }
    async fn activate(&self, plan: ActivationPlan) -> rsi_meta::Result<()> {
        seed::register(&plan)?;
        let id = ContributionId::new(COMMAND).expect("static command");
        let descriptor = SessionCommandDescriptor::new(
            id.clone(),
            COMMAND_NAME,
            "Hold context for the next request without waking the Session",
            true,
            None,
        )
        .map_err(|error| MetaError::Activation(error.to_string()))?;
        let lease = plan
            .local::<ContributionRegistrarContract>()?
            .register(
                &plan.context().registration_context()?,
                ContributionRegistration::new(
                    id,
                    10,
                    ContributionKind::Command(SessionCommandRegistration::new(
                        descriptor,
                        Arc::new(ContextInput(plan.context().clone())),
                    )),
                ),
            )
            .map_err(|error| MetaError::Activation(error.to_string()))?;
        plan.defer(
            "withdraw Session context input",
            Box::new(move || {
                Box::pin(async move {
                    drop(lease);
                    Ok(())
                })
            }),
        )
    }
}
