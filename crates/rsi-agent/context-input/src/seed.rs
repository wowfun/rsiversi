use super::{ContributionError, ContributionResult};
use async_trait::async_trait;
use rsi_agent_composition_protocol::{
    ContextContributor, ContributionContext, ContributionInput, ContributionKind,
    ContributionOutput, ContributionRegistrarContract, ContributionRegistration, DomainDefinition,
    DomainForkPolicy, DomainHandle, DomainReferencePolicy, DomainRegistrarContract, SessionCommand,
    SessionCommandContext, SessionCommandOutput, SessionCommandRegistration,
};
use rsi_agent_session_protocol::{
    CommandArguments, ContributionId, DomainIdentity, DomainStateView, SessionCommandDescriptor,
};
use rsi_meta::{ActivationPlan, MetaError};
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

const DOMAIN: &str = "rsi.context-seed";

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct Arguments {
    text: String,
}

#[derive(Debug)]
struct Seed(DomainHandle<String>);

fn invalid(error: impl std::fmt::Display) -> ContributionError {
    ContributionError::Invalid(error.to_string())
}

impl Seed {
    fn current<'a>(
        &self,
        domains: &'a [DomainStateView],
    ) -> ContributionResult<(&'a DomainStateView, String)> {
        let view = domains
            .iter()
            .find(|view| view.snapshot.identity().id() == DOMAIN)
            .ok_or_else(|| invalid("context seed state is missing"))?;
        Ok((view, self.0.decode(&view.snapshot).map_err(invalid)?))
    }
}

#[async_trait]
impl SessionCommand for Seed {
    async fn execute(
        &self,
        context: &SessionCommandContext,
        arguments: &CommandArguments,
        cancellation: CancellationToken,
    ) -> ContributionResult<SessionCommandOutput> {
        if cancellation.is_cancelled() {
            return Err(ContributionError::Closed);
        }
        let arguments: Arguments =
            serde_json::from_value(arguments.value().clone()).map_err(invalid)?;
        let (view, _) = self.current(&context.domains)?;
        Ok(vec![
            self.0
                .propose(view.revision, &arguments.text)
                .map_err(invalid)?,
        ]
        .into())
    }
}

#[async_trait]
impl ContextContributor for Seed {
    async fn contribute(
        &self,
        context: &ContributionContext,
        cancellation: CancellationToken,
    ) -> ContributionResult<ContributionOutput> {
        if cancellation.is_cancelled() {
            return Err(ContributionError::Closed);
        }
        let (view, text) = self.current(&context.domains)?;
        if text.is_empty() {
            return Ok(ContributionOutput::default());
        }
        Ok(ContributionOutput {
            inputs: vec![ContributionInput::context(text)],
            domains: vec![
                self.0
                    .propose(view.revision, &String::new())
                    .map_err(invalid)?,
            ],
        })
    }
}

pub(super) fn register(plan: &ActivationPlan) -> rsi_meta::Result<()> {
    let context = plan.context().registration_context()?;
    let (state, domain_lease) = DomainDefinition::new(
        DomainIdentity::new(DOMAIN, 1).expect("static domain"),
        &String::new(),
        |text| {
            if text.len() <= 64 * 1024 {
                Ok(())
            } else {
                Err("context seed exceeds 64 KiB".into())
            }
        },
        DomainReferencePolicy::None,
    )
    .map_err(|error| MetaError::Activation(error.to_string()))?
    .with_fork_policy(DomainForkPolicy::ResetToInitial)
    .register(plan.local::<DomainRegistrarContract>()?.as_ref(), &context)
    .map_err(|error| MetaError::Activation(error.to_string()))?;
    let callback = Arc::new(Seed(state));
    let command_id = ContributionId::new("rsi.context-seed.command").expect("static command");
    let descriptor = SessionCommandDescriptor::new(
        command_id.clone(),
        "context-seed",
        "Set supplemental reference context for the next request",
        true,
    )
    .map_err(|error| MetaError::Activation(error.to_string()))?;
    let registrar = plan.local::<ContributionRegistrarContract>()?;
    let mut leases = Vec::new();
    for (id, kind) in [
        (
            command_id,
            ContributionKind::Command(SessionCommandRegistration::new(
                descriptor,
                callback.clone(),
            )),
        ),
        (
            ContributionId::new("rsi.context-seed.context").expect("static contribution"),
            ContributionKind::Context(callback),
        ),
    ] {
        leases.push(
            registrar
                .register(&context, ContributionRegistration::new(id, 10, kind))
                .map_err(|error| MetaError::Activation(error.to_string()))?,
        );
    }
    plan.defer(
        "withdraw supplemental context",
        Box::new(move || {
            Box::pin(async move {
                drop(leases);
                drop(domain_lease);
                Ok(())
            })
        }),
    )
}
