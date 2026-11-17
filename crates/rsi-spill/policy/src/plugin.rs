#[cfg(test)]
use crate::estimated_text_tokens;
use crate::{SpillPolicyConfig, retain_preview, retrieval_notice};
use async_trait::async_trait;
use rsi_agent_composition_protocol::{
    ContributionKind, ContributionRegistrarContract, ContributionRegistration,
    ToolAroundContributor, ToolContinuation,
};
use rsi_agent_session_protocol::ContributionId;
use rsi_meta::{ActivationPlan, ConfigValue, MetaError, PluginFactory, PreparedActivation};
use rsi_spill::{SpillScope, SpillStore, SpillStoreContract};
use rsi_tools_protocol::{ToolContent, ToolExecutionSession, ToolResult};
use std::sync::Arc;

/// Optional ordinary Agent text retention contribution.
#[derive(Clone, Debug, Default)]
pub struct SpillPolicyFactory;
#[async_trait]
impl PluginFactory for SpillPolicyFactory {
    fn prepare(&self, desired: &ConfigValue) -> rsi_meta::Result<PreparedActivation> {
        let config: SpillPolicyConfig = if desired.is_null() {
            SpillPolicyConfig::default()
        } else {
            serde_json::from_value(desired.clone())
                .map_err(|e| MetaError::InvalidInput(e.to_string()))?
        };
        let prepared = PreparedActivation::with_state(
            desired.clone(),
            config,
            std::mem::size_of::<SpillPolicyConfig>(),
        );
        Ok(if config.max_inline_tokens.is_some() {
            prepared
                .requiring_local::<SpillStoreContract>()
                .requiring_local::<ContributionRegistrarContract>()
        } else {
            prepared
        })
    }
    async fn activate(&self, mut plan: ActivationPlan) -> rsi_meta::Result<()> {
        let config = plan.take_state::<SpillPolicyConfig>()?;
        let Some(maximum) = config.max_inline_tokens else {
            return Ok(());
        };
        let policy = Policy {
            store: plan.local::<SpillStoreContract>()?,
            maximum,
        };
        let lease = plan
            .local::<ContributionRegistrarContract>()?
            .register(
                &plan.context().registration_context()?,
                ContributionRegistration::new(
                    ContributionId::new("rsi.spill.policy").expect("static identity"),
                    0,
                    ContributionKind::ToolAround(Arc::new(policy)),
                ),
            )
            .map_err(|e| MetaError::Activation(e.to_string()))?;
        plan.defer(
            "withdraw text retention",
            Box::new(move || {
                Box::pin(async move {
                    drop(lease);
                    Ok(())
                })
            }),
        )
    }
}
#[derive(Debug)]
struct Policy {
    store: Arc<dyn SpillStore>,
    maximum: usize,
}
#[async_trait]
impl ToolAroundContributor for Policy {
    async fn execute(&self, next: ToolContinuation<'_>) -> rsi_tools_protocol::Result<ToolResult> {
        if next.name() == "output_read" {
            return next.run().await;
        }
        let scope = next
            .execution()
            .extension::<ToolExecutionSession>()
            .map_or_else(
                || self.store.generation_scope(),
                |session| session.scope.clone(),
            );
        let result = next.run().await?;
        Ok(self.retain(scope, result).await)
    }
}
impl Policy {
    async fn retain(&self, scope: SpillScope, mut result: ToolResult) -> ToolResult {
        if result
            .content
            .iter()
            .any(|item| !matches!(item, ToolContent::Text { .. }))
        {
            return result;
        }
        let fallback;
        let content = if result.content.is_empty() {
            fallback = [ToolContent::Text {
                text: serde_json::to_string(&result.value).expect("validated JSON value"),
            }];
            fallback.as_slice()
        } else {
            result.content.as_slice()
        };
        let estimated: usize = content
            .iter()
            .map(|item| match item {
                ToolContent::Text { text } => {
                    4 + text
                        .chars()
                        .take(self.maximum.saturating_mul(4))
                        .count()
                        .div_ceil(4)
                }
                ToolContent::Image { .. } => unreachable!("mixed content returned above"),
            })
            .sum();
        if estimated <= self.maximum {
            return result;
        }
        let text = match content {
            [ToolContent::Text { text }] => std::borrow::Cow::Borrowed(text.as_str()),
            content => std::borrow::Cow::Owned(
                content
                    .iter()
                    .map(|item| match item {
                        ToolContent::Text { text } => text.as_str(),
                        ToolContent::Image { .. } => unreachable!("mixed content returned above"),
                    })
                    .collect::<Vec<_>>()
                    .join(""),
            ),
        };
        if result.spills.len() >= rsi_tools_protocol::MAXIMUM_TOOL_CONTENT_ITEMS {
            return result;
        }
        // A reference may cover only stderr or another part of the result.
        // Further shortening requires retaining this exact rendered text.
        let projected = rsi_spill::SpillRef {
            locator: rsi_spill::SpillLocator::new(&scope, [0; 16]),
            bytes: text.len() as u64,
        };
        let notice = result
            .spills
            .iter()
            .chain(std::iter::once(&projected))
            .map(retrieval_notice)
            .collect::<Vec<_>>()
            .join("\n\n");
        let Some(preview) = retain_preview(&text, &notice, self.maximum) else {
            return result;
        };
        if preview.len() >= text.len() {
            return result;
        }
        let Ok(Ok(reference)) = tokio::time::timeout(
            rsi_spill::PUBLICATION_WAIT,
            self.store.save_text(scope, &text),
        )
        .await
        else {
            return result;
        };
        result.content = vec![ToolContent::Text { text: preview }];
        result.spills.push(reference);
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rsi_spill::SpillReader as _;
    #[derive(Debug)]
    struct NoWrites;
    #[async_trait]
    impl rsi_spill::SpillReader for NoWrites {
        async fn read(
            &self,
            _: &rsi_spill::SpillLocator,
            _: u64,
            _: usize,
        ) -> rsi_spill::Result<rsi_spill::SpillPage> {
            panic!("retention must not read");
        }
    }
    #[async_trait]
    impl SpillStore for NoWrites {
        fn scope(&self, _: SpillScope) -> Arc<dyn rsi_spill::CaptureSink> {
            panic!("ineligible results must not create cache objects");
        }
        fn generation_scope(&self) -> SpillScope {
            SpillScope::new("test", "no-writes")
        }
        fn fence(&self, _: &SpillScope) {
            panic!("unexpected fence");
        }
        async fn cleanup(&self, _: &SpillScope) -> rsi_spill::Result<()> {
            panic!("unexpected cleanup");
        }
        async fn is_clean(&self, _: &SpillScope) -> rsi_spill::Result<bool> {
            panic!("unexpected lookup");
        }
    }

    #[tokio::test]
    async fn per_block_framing_cannot_expand_a_maximum_legal_result() {
        let original = ToolResult::new(
            serde_json::json!({}),
            vec![
                ToolContent::Text {
                    text: "x".repeat(16384)
                };
                256
            ],
            false,
        )
        .unwrap();
        let policy = Policy {
            store: Arc::new(NoWrites),
            maximum: 1_049_000,
        };
        assert_eq!(
            policy
                .retain(SpillScope::new("session", "framing"), original.clone())
                .await,
            original
        );
    }

    #[tokio::test]
    async fn partial_stream_references_do_not_authorize_losing_rendered_output() {
        let root = tempfile::tempdir().unwrap();
        let mut config = rsi_spill_local::SpillLocalConfig::new(root.path().join("spill"));
        config.minimum_free_bytes = 0;
        let store = rsi_spill_local::LocalSpillStore::open(config)
            .await
            .unwrap();
        let scope = SpillScope::new("session", "partial");
        let reference = store.save_text(scope.clone(), "stderr only").await.unwrap();
        let text = "stdout content".repeat(10_000);
        let mut original = ToolResult::new(
            serde_json::json!({}),
            vec![ToolContent::Text { text: text.clone() }],
            false,
        )
        .unwrap();
        original.spills.push(reference.clone());
        let policy = Policy {
            store: store.clone(),
            maximum: 12_500,
        };
        let retained = policy.retain(scope.clone(), original.clone()).await;
        let rendered = retained
            .content
            .iter()
            .map(|item| match item {
                ToolContent::Text { text } => text.clone(),
                ToolContent::Image { .. } => unreachable!(),
            })
            .chain(retained.spills.iter().map(retrieval_notice))
            .filter(|text| !text.is_empty())
            .collect::<Vec<_>>()
            .join("\n\n");
        assert!(estimated_text_tokens(&rendered) <= policy.maximum);
        assert_eq!(retained.spills.len(), 2);
        assert_eq!(retained.spills[0], reference);
        let saved = &retained.spills[1];
        let mut bytes = Vec::new();
        while bytes.len() < text.len() {
            bytes.extend(
                store
                    .read(&saved.locator, bytes.len() as u64, 65_536)
                    .await
                    .unwrap()
                    .bytes,
            );
        }
        assert_eq!(bytes, text.as_bytes());
        store.shutdown().await;
        assert_eq!(policy.retain(scope, original.clone()).await, original);
    }

    #[tokio::test]
    async fn images_mixed_results_and_impossible_notices_never_write() {
        let policy = Policy {
            store: Arc::new(NoWrites),
            maximum: 1,
        };
        let image: ToolContent = serde_json::from_value(serde_json::json!({
            "type":"image", "media":{"id":"a".repeat(64),"mime":"image/png","bytes":4,"width":1,"height":1}
        })).unwrap();
        let text = ToolContent::Text {
            text: "界🦀".repeat(1000),
        };
        for content in [vec![image.clone()], vec![text.clone(), image], vec![text]] {
            let original =
                ToolResult::new(serde_json::json!({"canonical":42}), content, false).unwrap();
            let result = policy
                .retain(SpillScope::new("session", "no-write"), original.clone())
                .await;
            assert_eq!(result, original);
        }
    }

    #[tokio::test]
    async fn missing_budget_registers_nothing_and_needs_no_store_or_registrar() {
        let runtime = rsi_meta::Runtime::default();
        runtime
            .root()
            .apply(
                rsi_meta::ResolvedFactory::linked(
                    "spill",
                    "test",
                    rsi_meta::UpdateMode::Replayable,
                    Arc::new(SpillPolicyFactory),
                ),
                serde_json::json!({}),
            )
            .await
            .unwrap();
        assert!(runtime.shutdown().await.is_clean());
    }
    #[tokio::test]
    async fn empty_content_budgets_the_context_json_fallback_without_changing_value() {
        let root = tempfile::tempdir().unwrap();
        let mut config = rsi_spill_local::SpillLocalConfig::new(root.path().join("spill"));
        config.minimum_free_bytes = 0;
        let store = rsi_spill_local::LocalSpillStore::open(config)
            .await
            .unwrap();
        let policy = Policy {
            store: store.clone(),
            maximum: 12_500,
        };
        let original = ToolResult::new(
            serde_json::json!({"value":"中🦀\"\\".repeat(15_000)}),
            vec![],
            false,
        )
        .unwrap();
        let expected = serde_json::to_string(&original.value).unwrap();
        let result = policy
            .retain(SpillScope::new("session", "fallback"), original.clone())
            .await;
        assert_eq!(result.value, original.value);
        assert_eq!(result.spills.len(), 1);
        let [ToolContent::Text { text }] = result.content.as_slice() else {
            panic!("bounded text");
        };
        assert!(estimated_text_tokens(text) <= 12_500);
        let mut bytes = Vec::new();
        while bytes.len() < expected.len() {
            bytes.extend_from_slice(
                &store
                    .read(&result.spills[0].locator, bytes.len() as u64, 65_536)
                    .await
                    .unwrap()
                    .bytes,
            );
        }
        assert_eq!(bytes, expected.as_bytes());
        store.shutdown().await;
        assert_eq!(
            policy
                .retain(SpillScope::new("session", "fallback"), original.clone())
                .await,
            original
        );
    }
    #[tokio::test]
    async fn multiple_text_blocks_retrieve_verbatim_and_keep_canonical_value() {
        let root = tempfile::tempdir().unwrap();
        let mut config = rsi_spill_local::SpillLocalConfig::new(root.path().join("spill"));
        config.minimum_free_bytes = 0;
        let store = rsi_spill_local::LocalSpillStore::open(config)
            .await
            .unwrap();
        let policy = Policy {
            store: store.clone(),
            maximum: 12_500,
        };
        let text = format!("HEAD{}TAIL", "中🦀".repeat(30_000));
        let split = text.char_indices().nth(30_001).unwrap().0;
        let original = ToolResult::new(
            serde_json::json!({"canonical":text}),
            vec![
                ToolContent::Text {
                    text: text[..split].into(),
                },
                ToolContent::Text {
                    text: text[split..].into(),
                },
            ],
            false,
        )
        .unwrap();
        let result = policy
            .retain(SpillScope::new("session", "fixture"), original.clone())
            .await;
        assert_eq!(result.value, original.value);
        assert_eq!(result.spills.len(), 1);
        let ToolContent::Text { text: preview } = &result.content[0] else {
            panic!("text");
        };
        assert!(estimated_text_tokens(preview) <= 12_500);
        assert!(preview.starts_with("HEAD"));
        assert!(preview.contains("TAIL"));
        let reference = &result.spills[0];
        let mut copied = Vec::new();
        while (copied.len() as u64) < reference.bytes {
            copied.extend_from_slice(
                &store
                    .read(&reference.locator, copied.len() as u64, 65_536)
                    .await
                    .unwrap()
                    .bytes,
            );
        }
        assert_eq!(copied, text.as_bytes());
        // Partial references cannot authorize further truncation after storage closes.
        store.shutdown().await;
        let mut captured = original.clone();
        captured.spills = result.spills.clone();
        let reused = policy
            .retain(SpillScope::new("session", "fixture"), captured.clone())
            .await;
        assert_eq!(reused, captured);
        let fallback = policy
            .retain(SpillScope::new("session", "fixture"), original.clone())
            .await;
        assert_eq!(fallback, original);
    }
}
