//! Ordinary provider preserving the existing `ContextFold` projection.

use crate::{
    ContextBuilderIdentity, ContextFold, ContextInit, ContextLimits, ContextPage, ContextPosition,
    ModelContextBuilder, ModelContextBuilderContract, ModelContextCursor, Result,
};
use async_trait::async_trait;
use rsi_ai_protocol::LanguageRequest;
use rsi_meta::{ActivationPlan, ConfigValue, MetaError, PluginFactory, PreparedActivation};
use rsi_tools_protocol::ToolDefinition;
use sha2::{Digest, Sha256};
use std::sync::Arc;

/// Default pure builder over the existing Fact-to-Language fold.
#[derive(Debug)]
pub struct DefaultContextBuilder {
    identity: ContextBuilderIdentity,
}

impl Default for DefaultContextBuilder {
    fn default() -> Self {
        Self {
            identity: ContextBuilderIdentity::new(
                "rsi.agent.context.default",
                "2.4.0",
                hex::encode(Sha256::digest(b"null")),
            )
            .expect("static builder identity is valid"),
        }
    }
}

impl ModelContextBuilder for DefaultContextBuilder {
    fn identity(&self) -> &ContextBuilderIdentity {
        &self.identity
    }
    fn open(&self, init: ContextInit<'_>) -> Result<Box<dyn ModelContextCursor>> {
        let mut fold = match init.checkpoint {
            Some(bytes) => ContextFold::from_checkpoint(init.header, init.limits, bytes)?,
            None => ContextFold::with_limits(init.header, init.limits)?,
        };
        fold.enable_semantic(init.identity)?;
        Ok(Box::new(DefaultCursor {
            fold,
            limits: init.limits,
        }))
    }
}

#[derive(Debug)]
struct DefaultCursor {
    fold: ContextFold,
    limits: ContextLimits,
}
impl ModelContextCursor for DefaultCursor {
    fn ingest(&mut self, page: ContextPage<'_>) -> Result<()> {
        match page {
            ContextPage::Canonical(facts) => self.fold.apply(facts),
            ContextPage::ClaimVisible { facts, through_seq } => {
                self.fold.apply_page(facts, through_seq)
            }
            ContextPage::ForkSeed(facts) => self.fold.apply_seed_page(facts),
            ContextPage::FinishSeed => self.fold.finish_seed(),
        }
    }
    fn build(&self, tools: Vec<ToolDefinition>) -> Result<LanguageRequest> {
        self.fold.request(self.limits, tools)
    }
    fn plan_compaction(
        &self,
        model: &rsi_ai_protocol::ModelRef,
        profile: &rsi_ai_protocol::LanguageProfile,
        force: Option<rsi_agent_session_protocol::CompactionTrigger>,
        shrink: bool,
    ) -> Result<Option<crate::PlannedCompaction>> {
        self.fold.plan_compaction(model, profile, force, shrink)
    }
    fn summary_installed(&self, effect: &rsi_agent_session_protocol::EffectId) -> bool {
        self.fold.summary_installed(effect)
    }
    fn checkpoint(&self) -> Result<Arc<[u8]>> {
        self.fold.checkpoint_bytes()
    }
    fn position(&self) -> ContextPosition {
        ContextPosition {
            through_seq: self.fold.through_seq(),
            fact_prefix_digest: self.fold.fact_prefix_digest,
        }
    }
}

/// Explicit Agent contribution providing the default builder; accepts null config.
#[derive(Debug, Default)]
pub struct DefaultContextBuilderFactory;

#[async_trait]
impl PluginFactory for DefaultContextBuilderFactory {
    fn prepare(&self, config: &ConfigValue) -> rsi_meta::Result<PreparedActivation> {
        if !config.is_null() {
            return Err(MetaError::InvalidInput(
                "default context builder configuration must be null".into(),
            ));
        }
        Ok(PreparedActivation::new(ConfigValue::Null))
    }
    async fn activate(&self, plan: ActivationPlan) -> rsi_meta::Result<()> {
        let supply = plan
            .context()
            .provide_local::<ModelContextBuilderContract>(Arc::new(
                DefaultContextBuilder::default(),
            ))?;
        plan.defer(
            "withdraw default context builder",
            Box::new(move || {
                Box::pin(async move {
                    drop(supply);
                    Ok(())
                })
            }),
        )
    }
}
