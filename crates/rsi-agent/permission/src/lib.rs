//! Session-owned permission selection.
#![forbid(unsafe_code)]
#![warn(missing_docs)]

use async_trait::async_trait;
use rsi_agent_composition_protocol::{
    ContributionError, ContributionKind, ContributionRegistrarContract, ContributionRegistration,
    ContributionResult, DomainHandle, DomainRegistrarContract, SessionCommand,
    SessionCommandContext, SessionCommandOutput, SessionCommandRegistration, SessionProjection,
    SessionProjectionContext, session_permission_domain,
};
use rsi_agent_session_protocol::{
    CommandArguments, ContributionId, ProjectionValue, SessionCommandDescriptor, SessionPermission,
};
use rsi_meta::{ActivationPlan, ConfigValue, MetaError, PluginFactory, PreparedActivation};
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

/// Contributes the Session domain and its structured permission command.
#[derive(Clone, Copy, Debug, Default)]
pub struct PermissionFactory;

#[derive(Debug)]
struct Select(DomainHandle<Option<SessionPermission>>);

#[async_trait]
impl SessionCommand for Select {
    async fn execute(
        &self,
        ctx: &SessionCommandContext,
        args: &CommandArguments,
        stop: CancellationToken,
    ) -> ContributionResult<SessionCommandOutput> {
        if stop.is_cancelled() {
            return Err(ContributionError::Closed);
        }
        let next = if let Some(name) = args.value().as_str() {
            SessionPermission::standard(name.trim()).map_err(invalid)?
        } else {
            serde::Deserialize::deserialize(args.value()).map_err(invalid)?
        };
        if SessionPermission::bounded_sandbox(&ctx.header, next.sandbox) != next.sandbox {
            return Err(invalid(
                "Permission selection exceeds the delegated sandbox",
            ));
        }
        let view = ctx
            .domains
            .iter()
            .find(|view| view.snapshot.identity().id() == SessionPermission::DOMAIN)
            .ok_or_else(|| invalid("Session permission domain missing"))?;
        Ok(vec![
            self.0
                .propose(view.revision, &Some(next))
                .map_err(invalid)?,
        ]
        .into())
    }
}

#[cfg(test)]
rsi_persistence_testkit::payload_contract!(
    persistence_permission,
    Option<SessionPermission>,
    SessionPermission::VERSION,
    format!(
        "tests/fixtures/persistence/permission-v{}.json",
        SessionPermission::VERSION
    ),
    {
        use rsi_agent_session_protocol::ApprovalPromptPolicy;
        use rsi_sandbox::SandboxMode;
        vec![
            None,
            Some(SessionPermission {
                preset: "danger-full-access".into(),
                sandbox: SandboxMode::DangerFullAccess,
                approval: ApprovalPromptPolicy::Never,
            }),
            Some(SessionPermission {
                preset: "workspace-write".into(),
                sandbox: SandboxMode::WorkspaceWrite,
                approval: ApprovalPromptPolicy::Ask,
            }),
        ]
    }
);

#[derive(serde::Serialize)]
struct Effective<'a> {
    preset: Option<&'a str>,
    sandbox: rsi_sandbox::SandboxMode,
    approval: rsi_agent_session_protocol::ApprovalPromptPolicy,
}

#[async_trait]
impl SessionProjection for Select {
    async fn project(
        &self,
        ctx: &SessionProjectionContext,
        _: CancellationToken,
    ) -> ContributionResult<ProjectionValue> {
        let view = ctx
            .domains()
            .iter()
            .find(|view| view.snapshot.identity().id() == SessionPermission::DOMAIN)
            .ok_or_else(|| invalid("Session permission domain missing"))?;
        let header = ctx.header();
        let selected = self.0.decode(&view.snapshot).map_err(invalid)?;
        let permission = selected.as_ref().or(header.settings().permission());
        ProjectionValue::encode(&Effective {
            preset: permission.map(|value| value.preset.as_str()),
            sandbox: SessionPermission::bounded_sandbox(
                header,
                permission.map_or(header.settings().sandbox(), |value| value.sandbox),
            ),
            approval: permission.map_or_else(
                || SessionPermission::initial_approval(header),
                |value| value.approval,
            ),
        })
        .map_err(invalid)
    }
}

fn invalid(error: impl std::fmt::Display) -> ContributionError {
    ContributionError::Invalid(error.to_string())
}

#[async_trait]
impl PluginFactory for PermissionFactory {
    fn prepare(&self, config: &ConfigValue) -> rsi_meta::Result<PreparedActivation> {
        if !config.is_null() {
            return Err(MetaError::InvalidInput(
                "Session permission takes no configuration".into(),
            ));
        }
        Ok(PreparedActivation::new(ConfigValue::Null)
            .requiring_local::<DomainRegistrarContract>()
            .requiring_local::<ContributionRegistrarContract>())
    }

    async fn activate(&self, plan: ActivationPlan) -> rsi_meta::Result<()> {
        let fail = |error: rsi_agent_composition_protocol::DomainError| {
            MetaError::Activation(error.to_string())
        };
        let definition = session_permission_domain().map_err(fail)?;
        let context = plan.context().registration_context()?;
        let (state, domain) = definition
            .register(plan.local::<DomainRegistrarContract>()?.as_ref(), &context)
            .map_err(fail)?;
        let id =
            ContributionId::new(SessionPermission::COMMAND).expect("static permission command");
        let descriptor = SessionCommandDescriptor::new(
            id.clone(),
            "permission",
            "Select this Session's permission preset",
            true,
            Some(rsi_agent_session_protocol::ComposerCommandInput::Text {
                attachments: false,
                hint: "<preset>".into(),
            }),
        )
        .map_err(|e| MetaError::Activation(e.to_string()))?;
        let registrar = plan.local::<ContributionRegistrarContract>()?;
        let selection = Arc::new(Select(state));
        let command = registrar
            .register(
                &context,
                ContributionRegistration::new(
                    id,
                    10,
                    ContributionKind::Command(SessionCommandRegistration::new(
                        descriptor,
                        selection.clone(),
                    )),
                ),
            )
            .map_err(|e| MetaError::Activation(e.to_string()))?;
        let view = registrar
            .register(
                &context,
                ContributionRegistration::new(
                    ContributionId::new(SessionPermission::VIEW).expect("static permission view"),
                    10,
                    ContributionKind::Projection(selection),
                ),
            )
            .map_err(|e| MetaError::Activation(e.to_string()))?;
        plan.defer(
            "withdraw Session permission",
            Box::new(move || {
                Box::pin(async move {
                    drop(command);
                    drop(view);
                    drop(domain);
                    Ok(())
                })
            }),
        )
    }
}
