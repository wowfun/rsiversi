use async_trait::async_trait;
use rsi_meta::{
    ActivationPlan, FactoryIdentity, FiberState, PluginFactory, PluginId, PreparedActivation,
    ResolvedFactory, Result, Runtime, UpdateMode,
};
use serde_json::Value;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

#[derive(Debug)]
struct IdentityFreeFactory {
    activated: Arc<AtomicBool>,
}

#[async_trait]
impl PluginFactory for IdentityFreeFactory {
    fn prepare(&self, desired: &Value) -> Result<PreparedActivation> {
        Ok(PreparedActivation::new(desired.clone()))
    }

    async fn activate(&self, _plan: ActivationPlan) -> Result<()> {
        self.activated.store(true, Ordering::Release);
        Ok(())
    }
}

#[tokio::test]
async fn caller_resolves_immutable_factory_identity_before_plugin_execution() {
    let activated = Arc::new(AtomicBool::new(false));
    let runtime = Runtime::default();
    let resolved = ResolvedFactory::linked(
        PluginId::new("test.resolved-factory"),
        "revision-7",
        UpdateMode::Replayable,
        Arc::new(IdentityFreeFactory {
            activated: Arc::clone(&activated),
        }),
    );

    let fiber = runtime.root().apply(resolved, Value::Null).await.unwrap();

    assert_eq!(fiber.snapshot().state, FiberState::Active);
    assert_eq!(
        fiber.snapshot().factory,
        FactoryIdentity::Linked {
            plugin: PluginId::new("test.resolved-factory"),
            revision: "revision-7".to_owned(),
        }
    );
    assert!(activated.load(Ordering::Acquire));
    assert!(runtime.shutdown().await.is_complete());
}

#[tokio::test]
async fn malformed_factory_provenance_is_rejected_before_plugin_execution() {
    for identity in [
        FactoryIdentity::linked("test.invalid", ""),
        FactoryIdentity::native("test.invalid", "not-a-sha256"),
    ] {
        let activated = Arc::new(AtomicBool::new(false));
        let runtime = Runtime::default();
        let result = runtime
            .root()
            .apply(
                ResolvedFactory::new(
                    identity,
                    UpdateMode::Replayable,
                    Arc::new(IdentityFreeFactory {
                        activated: Arc::clone(&activated),
                    }),
                ),
                Value::Null,
            )
            .await;
        assert!(result.is_err());
        assert!(!activated.load(Ordering::Acquire));
        assert!(runtime.shutdown().await.is_complete());
    }
}
