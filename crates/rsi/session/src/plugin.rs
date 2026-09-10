use crate::LocalSessionService;
use async_trait::async_trait;
use rsi_agent_composition_protocol::AgentCompositionContract;
use rsi_agent_store_protocol::SessionStoreContract;
use rsi_agent_turn_protocol::{
    SessionCommandsContract, SessionProjectionsContract, TurnServiceContract,
};
use rsi_ai_protocol::{ImageCallContract, LanguageCallContract};
use rsi_media_protocol::MediaContract;
use rsi_meta::{ActivationPlan, ConfigValue, MetaError, PluginFactory, PreparedActivation};
use rsi_session_protocol::{
    AgentSettingsContract, SessionApprovalControlContract, SessionContract, SessionIngressContract,
    SessionReadContract,
};
use rsi_user_questions_protocol::UserQuestionsContract;
use rsi_workspace_protocol::WorkspaceRegistryContract;
use std::sync::Arc;

/// Ordinary owner of one native Session domain service generation.
#[derive(Clone, Debug, Default)]
pub struct SessionFactory;

#[async_trait]
impl PluginFactory for SessionFactory {
    fn prepare(&self, desired: &ConfigValue) -> rsi_meta::Result<PreparedActivation> {
        if !desired.is_null() {
            return Err(MetaError::InvalidInput(
                "Session configuration must be null".into(),
            ));
        }
        Ok(PreparedActivation::new(ConfigValue::Null)
            .requiring_local::<TurnServiceContract>()
            .requiring_local::<SessionCommandsContract>()
            .requiring_local::<SessionProjectionsContract>()
            .requiring_local::<SessionStoreContract>()
            .requiring_local::<AgentCompositionContract>()
            .requiring_local::<WorkspaceRegistryContract>()
            .requiring_local::<AgentSettingsContract>()
            .requiring_local::<LanguageCallContract>()
            .requiring_local::<ImageCallContract>()
            .requiring_local::<MediaContract>()
            .requiring_local::<SessionApprovalControlContract>()
            .requiring_local::<UserQuestionsContract>())
    }

    async fn activate(&self, plan: ActivationPlan) -> rsi_meta::Result<()> {
        let service = LocalSessionService::new(
            plan.context().runtime().execution().clone(),
            plan.local::<SessionCommandsContract>()?,
            plan.local::<SessionProjectionsContract>()?,
            plan.local::<TurnServiceContract>()?,
            plan.local::<SessionStoreContract>()?,
            plan.local::<AgentCompositionContract>()?,
            plan.local::<WorkspaceRegistryContract>()?,
            plan.local::<AgentSettingsContract>()?,
            plan.local::<LanguageCallContract>()?,
            plan.local::<ImageCallContract>()?,
            plan.local::<MediaContract>()?,
            plan.local::<SessionApprovalControlContract>()?,
        )
        .with_questions(Some(plan.local::<UserQuestionsContract>()?));
        let service = Arc::new(service);
        let cleanup = service.clone();
        plan.defer(
            "stop Session drafts",
            Box::new(move || {
                Box::pin(async move {
                    cleanup.stop().await;
                    Ok(())
                })
            }),
        )?;
        let supply = plan
            .context()
            .provide_local::<SessionContract>(service.clone())?;
        let ingress = plan
            .context()
            .provide_local::<SessionIngressContract>(service.clone())?;
        let reads = plan
            .context()
            .provide_local::<SessionReadContract>(service)?;
        plan.defer(
            "withdraw Session",
            Box::new(move || {
                Box::pin(async move {
                    drop(supply);
                    drop(ingress);
                    drop(reads);
                    Ok(())
                })
            }),
        )
    }
}
