//! Workspace interpretation and durable last-good state belong to this ordinary plugin.

use super::*;
use rsi_agent_composition_protocol::{
    ContextContributor, ContributionContext, ContributionError, ContributionInput,
    ContributionKind, ContributionOutput, ContributionRegistrarContract, ContributionRegistration,
    ContributionResult, DomainDefinition, DomainHandle, DomainRegistrarContract,
};
use rsi_agent_session_protocol::{
    ContributionId, DomainIdentity, InputMessageSource, SessionFactBody, SessionId, TurnId,
};

const DOMAIN_ID: &str = "rsi.workspace-context";

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct State {
    instructions_sha256: Option<String>,
    skill_catalog_sha256: Option<String>,
    scanned_session: Option<SessionId>,
    scanned_turn: Option<TurnId>,
    through_fact_seq: u64,
}

fn validate_state(state: &State) -> Result<(), String> {
    if state.scanned_session.is_some() != state.scanned_turn.is_some() {
        return Err("workspace cursor requires both Session and Turn identity".into());
    }
    if state.scanned_session.is_none() && state.through_fact_seq != 0 {
        return Err("workspace cursor requires its owning Session".into());
    }
    for digest in [
        state.instructions_sha256.as_ref(),
        state.skill_catalog_sha256.as_ref(),
    ]
    .into_iter()
    .flatten()
    {
        if digest.len() != 64
            || !digest
                .bytes()
                .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
        {
            return Err("workspace state requires lowercase SHA-256 digests".into());
        }
    }
    Ok(())
}

#[derive(Debug)]
struct Contributor {
    source: Arc<dyn WorkspaceContext>,
    state: DomainHandle<State>,
}

#[async_trait]
impl ContextContributor for Contributor {
    async fn contribute(
        &self,
        context: &ContributionContext,
        _: CancellationToken,
    ) -> ContributionResult<ContributionOutput> {
        let view = context
            .domains
            .iter()
            .find(|view| view.snapshot.identity().id() == DOMAIN_ID)
            .ok_or_else(|| {
                ContributionError::Invalid("workspace domain state is missing".into())
            })?;
        let previous = self
            .state
            .decode(&view.snapshot)
            .map_err(contribution_error)?;
        let requests = pending_requests(context, &previous).await?;
        let snapshot = self
            .source
            .snapshot(&context.header, &requests)
            .await
            .map_err(contribution_error)?;
        if !snapshot.complete {
            return Ok(ContributionOutput::default());
        }
        let next = State {
            instructions_sha256: Some(snapshot.instructions_sha256.clone()),
            skill_catalog_sha256: Some(snapshot.skill_catalog_sha256.clone()),
            scanned_session: Some(context.header.session_id().clone()),
            scanned_turn: Some(context.turn_id.clone()),
            through_fact_seq: context.horizon.fact_seq,
        };
        let inputs = entered_inputs(&previous, snapshot);
        let domains = if next == previous {
            Vec::new()
        } else {
            vec![
                self.state
                    .propose(view.revision, &next)
                    .map_err(contribution_error)?,
            ]
        };
        Ok(ContributionOutput { inputs, domains })
    }
}

async fn pending_requests(
    context: &ContributionContext,
    state: &State,
) -> ContributionResult<WorkspaceSkillRequests> {
    let mut cursor = if state.scanned_session.as_ref() == Some(context.header.session_id())
        && state.scanned_turn.as_ref() == Some(&context.turn_id)
    {
        state.through_fact_seq
    } else {
        context.accepted_fact_seq.saturating_sub(1)
    };
    if cursor > context.horizon.fact_seq {
        return Err(ContributionError::Invalid(
            "workspace cursor exceeds its Session Fact horizon".into(),
        ));
    }
    let mut requests = WorkspaceSkillRequests::default();
    while cursor < context.horizon.fact_seq {
        let page = context.facts.read(cursor, 128).await?;
        if page.through_seq <= cursor || page.through_seq > context.horizon.fact_seq {
            return Err(ContributionError::Invalid(
                "workspace history reader did not advance within its horizon".into(),
            ));
        }
        for fact in page.facts {
            if fact.body().turn_id() != &context.turn_id {
                continue;
            }
            match fact.body() {
                SessionFactBody::TurnAccepted { text, .. } => {
                    requests.push_text(text).map_err(contribution_error)?;
                }
                SessionFactBody::InputMessageEntered {
                    source: InputMessageSource::Human { .. },
                    content,
                    ..
                } => {
                    requests.push_content(content).map_err(contribution_error)?;
                }
                _ => {}
            }
        }
        cursor = page.through_seq;
    }
    Ok(requests)
}

fn entered_inputs(current: &State, snapshot: WorkspaceContextSnapshot) -> Vec<ContributionInput> {
    let mut inputs = Vec::new();
    if current.instructions_sha256.as_deref() != Some(&snapshot.instructions_sha256)
        && (snapshot.instructions.is_some() || current.instructions_sha256.is_some())
    {
        inputs.push(ContributionInput::sourced(InputMessageSource::AgentInstructions {
            source: "workspace-baseline".into(), sha256: snapshot.instructions_sha256,
            replacement: true, tombstone: snapshot.instructions.is_none(),
        }, snapshot.instructions.unwrap_or_else(||
            "The complete workspace instruction baseline is empty; earlier workspace instructions no longer apply.".into())));
    }
    if current.skill_catalog_sha256.as_deref() != Some(&snapshot.skill_catalog_sha256)
        && (snapshot.skill_catalog.is_some() || current.skill_catalog_sha256.is_some())
    {
        inputs.push(ContributionInput::sourced(InputMessageSource::SkillCatalog { sha256: snapshot.skill_catalog_sha256 },
            snapshot.skill_catalog.unwrap_or_else(||
                "<available_skills>\n</available_skills>\nThis complete catalog replaces earlier skill names; no skills are currently available.".into())));
    }
    inputs.extend(snapshot.invocations.into_iter().map(|invocation| {
        ContributionInput::sourced(
            InputMessageSource::UserSkillInvocation {
                name: invocation.name,
                source: invocation.source,
            },
            invocation.text,
        )
    }));
    inputs
}

fn contribution_error(error: impl fmt::Display) -> ContributionError {
    ContributionError::Invalid(error.to_string())
}

/// Agent-only workspace context and state contribution over the global filesystem source.
#[derive(Clone, Copy, Debug, Default)]
pub struct WorkspaceContributorFactory;

#[async_trait]
impl PluginFactory for WorkspaceContributorFactory {
    fn prepare(&self, desired: &ConfigValue) -> rsi_meta::Result<PreparedActivation> {
        if !desired.is_null() {
            return Err(MetaError::InvalidInput(
                "workspace contributor configuration must be null".into(),
            ));
        }
        Ok(PreparedActivation::new(ConfigValue::Null)
            .requiring_local::<WorkspaceContextContract>()
            .requiring_local::<DomainRegistrarContract>()
            .requiring_local::<ContributionRegistrarContract>())
    }
    async fn activate(&self, plan: ActivationPlan) -> rsi_meta::Result<()> {
        let definition = DomainDefinition::new(
            DomainIdentity::new(DOMAIN_ID, 1).expect("static domain"),
            &State::default(),
            validate_state,
        )
        .map_err(|error| MetaError::Activation(error.to_string()))?;
        let context = plan.context().registration_context()?;
        let (state, domain_lease) = definition
            .register(plan.local::<DomainRegistrarContract>()?.as_ref(), &context)
            .map_err(|error| MetaError::Activation(error.to_string()))?;
        let contribution_lease = plan
            .local::<ContributionRegistrarContract>()?
            .register(
                &context,
                ContributionRegistration::new(
                    ContributionId::new(DOMAIN_ID).expect("static contribution"),
                    0,
                    ContributionKind::Context(Arc::new(Contributor {
                        source: plan.local::<WorkspaceContextContract>()?,
                        state,
                    })),
                ),
            )
            .map_err(|error| MetaError::Activation(error.to_string()))?;
        plan.defer(
            "withdraw workspace contribution",
            Box::new(move || {
                Box::pin(async move {
                    drop(contribution_lease);
                    drop(domain_lease);
                    Ok(())
                })
            }),
        )
    }
}
