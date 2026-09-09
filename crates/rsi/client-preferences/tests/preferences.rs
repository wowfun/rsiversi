use rsi_client_preferences::{ClientPreferencesFactory, NAMESPACE, Preferences};
use rsi_meta::{FiberState, PluginFactory, ResolvedFactory, Runtime, UpdateMode};
use rsi_settings_protocol::{SettingsAccessContract, SettingsApply, SettingsError};
use serde_json::{Value, json};
use std::sync::Arc;

async fn setup(value: Value) -> Runtime {
    let runtime = Runtime::default();
    for (id, factory) in [
        (
            "provider",
            Arc::new(rsi_settings_testkit::MemorySettingsProviderFactory::new(
                value,
            )) as Arc<dyn PluginFactory>,
        ),
        ("settings", Arc::new(rsi_settings::SettingsFactory)),
    ] {
        let fiber = runtime
            .root()
            .apply(
                ResolvedFactory::linked(id, "test", UpdateMode::Replayable, factory),
                Value::Null,
            )
            .await
            .unwrap();
        assert_eq!(fiber.snapshot().state, FiberState::Active);
    }
    runtime
}
fn factory() -> ResolvedFactory {
    ResolvedFactory::linked(
        "preferences",
        "test",
        UpdateMode::Replayable,
        Arc::new(ClientPreferencesFactory),
    )
}
#[tokio::test]
async fn ordinary_preferences_validate_persist_and_retire_without_changing_captured_values() {
    let runtime = setup(json!({})).await;
    let access = runtime
        .root()
        .lookup_local::<SettingsAccessContract>()
        .unwrap();
    assert_eq!(
        Preferences::load(access.as_ref()).await.unwrap(),
        Preferences::default()
    );
    let fiber = runtime.root().apply(factory(), Value::Null).await.unwrap();
    let frozen = Preferences::load(access.as_ref()).await.unwrap();
    let before = access.read(NAMESPACE).await.unwrap();
    let description = access.describe(NAMESPACE).await.unwrap();
    assert_eq!(description.metadata.applies, SettingsApply::Restart);
    assert!(
        description
            .metadata
            .description
            .contains("Reconnect Web or restart TUI")
    );
    for value in [
        json!({"web":{"enter_submit":"yes"}}),
        json!({"web":{"unknown":true}}),
        json!({"other":false}),
    ] {
        assert!(matches!(
            access.replace(NAMESPACE, &before.version(), value).await,
            Err(SettingsError::InvalidInput(_))
        ));
        assert_eq!(access.read(NAMESPACE).await.unwrap(), before);
    }
    access
        .replace(
            NAMESPACE,
            &before.version(),
            json!({"web":{"enter_submit":true}}),
        )
        .await
        .unwrap();
    assert!(!frozen.web.enter_submit);
    let next = Preferences::load(access.as_ref()).await.unwrap();
    assert!(next.web.enter_submit);
    assert!(next.tui.enter_submit);
    assert!(matches!(
        access.clear(NAMESPACE, &before.version()).await,
        Err(SettingsError::Conflict { .. })
    ));
    assert!(fiber.dispose().await.is_clean());
    assert!(matches!(
        access.read(NAMESPACE).await,
        Err(SettingsError::UnknownNamespace(_))
    ));
    assert_eq!(
        Preferences::load(access.as_ref()).await.unwrap(),
        Preferences::default()
    );
    let replacement = runtime.root().apply(factory(), Value::Null).await.unwrap();
    assert_eq!(replacement.snapshot().state, FiberState::Active);
    assert_eq!(Preferences::load(access.as_ref()).await.unwrap(), next);
    assert!(runtime.shutdown().await.is_clean());
}
#[tokio::test]
async fn malformed_stored_preferences_do_not_publish_a_namespace_owner() {
    let runtime = setup(json!({"rsi.client":{"tui":{"enter_submit":"no"}}})).await;
    let failed = runtime.root().apply(factory(), Value::Null).await.unwrap();
    assert!(
        matches!(failed.snapshot().state, FiberState::Failed(reason) if reason.contains("boolean"))
    );
    let access = runtime
        .root()
        .lookup_local::<SettingsAccessContract>()
        .unwrap();
    assert!(matches!(
        access.read(NAMESPACE).await,
        Err(SettingsError::UnknownNamespace(_))
    ));
    assert!(runtime.shutdown().await.is_clean());
}
