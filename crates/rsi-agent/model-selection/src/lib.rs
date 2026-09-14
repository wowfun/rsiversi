//! Durable Session selection, exposed through ordinary commands and projections.
#![forbid(unsafe_code)]
#![warn(missing_docs)]

use async_trait::async_trait;
use rsi_agent_composition_protocol::{
    ContributionError, ContributionKind, ContributionRegistrarContract, ContributionRegistration,
    ContributionResult, DomainDefinition, DomainForkPolicy, DomainHandle, DomainRegistrarContract,
    SessionCommand, SessionCommandContext, SessionCommandRegistration, SessionProjection,
    SessionProjectionContext, ValidatedDomainProposal,
};
use rsi_agent_session_protocol::{
    CommandArguments, ContributionId, DomainIdentity, DomainStateView, ModelSelection,
    ProjectionValue, SessionCommandDescriptor, SessionHeader,
};
use rsi_ai_protocol::{LanguageCall, LanguageCallContract};
use rsi_meta::{ActivationPlan, ConfigValue, MetaError, PluginFactory, PreparedActivation};
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

/// Durable domain identity; absence uses the Header baseline.
pub const DOMAIN: &str = "rsi.model-selection";
/// Ordinary command contribution identity used by application model pickers.
pub const COMMAND: &str = "rsi.model-selection.command";
/// Projection producer identity.
pub const VIEW: &str = "rsi.model-selection.view";

/// Resolves an explicit Turn override, the captured domain, or the frozen baseline.
/// An absent plugin is valid; a present invalid domain is never ignored.
///
/// # Errors
/// Rejects invalid selections, malformed domain state and unknown domain versions.
pub fn resolve_selection(
    header: &SessionHeader,
    domains: &[DomainStateView],
    explicit: Option<&ModelSelection>,
) -> ContributionResult<ModelSelection> {
    if let Some(selection) = explicit {
        selection.validate().map_err(invalid)?;
        return Ok(selection.clone());
    }
    if let Some(view) = domains
        .iter()
        .find(|view| view.snapshot.identity().id() == DOMAIN)
    {
        if view.snapshot.identity().version() != 1 {
            return Err(invalid("unsupported model selection domain version"));
        }
        let selection: Option<ModelSelection> =
            serde_json::from_value(view.snapshot.state().value().clone()).map_err(invalid)?;
        if let Some(selection) = selection {
            selection.validate().map_err(invalid)?;
            return Ok(selection);
        }
    }
    Ok(ModelSelection::baseline(header.settings()))
}

fn invalid(error: impl std::fmt::Display) -> ContributionError {
    ContributionError::Invalid(error.to_string())
}
#[allow(clippy::ref_option)] // Domain validators receive &T; this domain's T is Option<ModelSelection>.
fn validate(value: &Option<ModelSelection>) -> Result<(), String> {
    value.as_ref().map_or(Ok(()), |selection| {
        selection.validate().map_err(|e| e.to_string())
    })
}

/// Agent-only registration of selection state, command and typed view.
#[derive(Clone, Copy, Debug, Default)]
pub struct ModelSelectionFactory;
#[derive(Debug)]
struct Selection {
    state: DomainHandle<Option<ModelSelection>>,
    language: Arc<dyn LanguageCall>,
}
#[async_trait]
impl SessionCommand for Selection {
    async fn execute(
        &self,
        context: &SessionCommandContext,
        arguments: &CommandArguments,
        cancellation: CancellationToken,
    ) -> ContributionResult<Vec<ValidatedDomainProposal>> {
        if cancellation.is_cancelled() {
            return Err(ContributionError::Closed);
        }
        let selection: ModelSelection = match arguments.value() {
            serde_json::Value::String(text) => serde_json::from_str(text).map_err(invalid)?,
            value => serde_json::from_value(value.clone()).map_err(invalid)?,
        };
        selection.validate().map_err(invalid)?;
        self.language
            .describe(&selection.model)
            .map_err(invalid)?
            .profile()
            .reasoning_efforts()
            .resolve(selection.reasoning_effort.as_ref())
            .map_err(invalid)?;
        let view = context
            .domains
            .iter()
            .find(|view| view.snapshot.identity().id() == DOMAIN)
            .ok_or_else(|| invalid("model selection state is missing"))?;
        self.state.decode(&view.snapshot).map_err(invalid)?;
        Ok(vec![
            self.state
                .propose(view.revision, &Some(selection))
                .map_err(invalid)?,
        ])
    }
}
#[async_trait]
impl SessionProjection for Selection {
    async fn project(
        &self,
        context: &SessionProjectionContext,
        cancellation: CancellationToken,
    ) -> ContributionResult<ProjectionValue> {
        if cancellation.is_cancelled() {
            return Err(ContributionError::Closed);
        }
        ProjectionValue::encode(&resolve_selection(
            context.header(),
            context.domains(),
            None,
        )?)
        .map_err(invalid)
    }
}
#[async_trait]
impl PluginFactory for ModelSelectionFactory {
    fn prepare(&self, desired: &ConfigValue) -> rsi_meta::Result<PreparedActivation> {
        if !desired.is_null() {
            return Err(MetaError::InvalidInput(
                "model selection takes no configuration".into(),
            ));
        }
        Ok(PreparedActivation::new(desired.clone())
            .requiring_local::<DomainRegistrarContract>()
            .requiring_local::<ContributionRegistrarContract>()
            .requiring_local::<LanguageCallContract>())
    }
    async fn activate(&self, plan: ActivationPlan) -> rsi_meta::Result<()> {
        let definition = DomainDefinition::new(
            DomainIdentity::new(DOMAIN, 1).expect("static domain"),
            &None::<ModelSelection>,
            validate,
        )
        .map_err(|e| MetaError::Activation(e.to_string()))?
        .with_fork_policy(DomainForkPolicy::ResetToInitial);
        let context = plan.context().registration_context()?;
        let (state, domain_lease) = definition
            .register(plan.local::<DomainRegistrarContract>()?.as_ref(), &context)
            .map_err(|e| MetaError::Activation(e.to_string()))?;
        let callback = Arc::new(Selection {
            state,
            language: plan.local::<LanguageCallContract>()?,
        });
        let command = ContributionId::new(COMMAND).expect("static command");
        let descriptor = SessionCommandDescriptor::new(
            command.clone(),
            "model-selection",
            "Select the model and effort for the next assembled request",
            true,
        )
        .map_err(|e| MetaError::Activation(e.to_string()))?;
        let registrar = plan.local::<ContributionRegistrarContract>()?;
        let command_lease = registrar
            .register(
                &context,
                ContributionRegistration::new(
                    command,
                    10,
                    ContributionKind::Command(SessionCommandRegistration::new(
                        descriptor,
                        callback.clone(),
                    )),
                ),
            )
            .map_err(|e| MetaError::Activation(e.to_string()))?;
        let view_lease = registrar
            .register(
                &context,
                ContributionRegistration::new(
                    ContributionId::new(VIEW).expect("static view"),
                    10,
                    ContributionKind::Projection(callback),
                ),
            )
            .map_err(|e| MetaError::Activation(e.to_string()))?;
        plan.defer(
            "withdraw Session model selection",
            Box::new(move || {
                Box::pin(async move {
                    drop(view_lease);
                    drop(command_lease);
                    drop(domain_lease);
                    Ok(())
                })
            }),
        )
    }
}
