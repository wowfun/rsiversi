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
    assert_eq!(description.metadata.applies, SettingsApply::Live);
    assert!(description.metadata.description.contains("apply live"));
    for value in [
        json!({"web":{"enter_submit":"yes"}}),
        json!({"web":{"unknown":true}}),
        json!({"other":false}),
        json!({"appearance":{"theme":"automatic"}}),
        json!({"appearance":{"content_font_size":11}}),
        json!({"appearance":{"content_font_size":18}}),
        json!({"appearance":{"content_font_size":14.5}}),
        json!({"appearance":{"content_font_size":"14"}}),
        json!({"tui":{"enter_submit":true}}),
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
            json!({"web":{"submit_key":"mod_enter","busy_submit":"steer"},"appearance":{"theme":"dark","content_font_size":17}}),
        )
        .await
        .unwrap();
    assert_eq!(
        frozen.web.submit_key,
        rsi_client_preferences::SubmitKey::Enter
    );
    let next = Preferences::load(access.as_ref()).await.unwrap();
    assert_eq!(
        next.web.submit_key,
        rsi_client_preferences::SubmitKey::ModEnter
    );
    assert_eq!(next.appearance.theme, rsi_client_preferences::Theme::Dark);
    assert_eq!(next.appearance.content_font_size, 17);
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
    let runtime = setup(json!({"rsi.client":{"web":{"enter_submit":"no"}}})).await;
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

#[tokio::test]
async fn legacy_keys_migrate_once_preserving_appearance_and_new_preferences() {
    for web in [
        json!({"enter_submit":true}),
        json!({"enter_submit":false}),
        json!({}),
        json!({"submit_key":"mod_enter","busy_submit":"steer"}),
    ] {
        let runtime = setup(
            json!({"rsi.client":{"web":web,"appearance":{"theme":"dark","content_font_size":16}}}),
        )
        .await;
        let owner = runtime.root().apply(factory(), Value::Null).await.unwrap();
        assert_eq!(owner.snapshot().state, FiberState::Active);
        let access = runtime
            .root()
            .lookup_local::<SettingsAccessContract>()
            .unwrap();
        let first = Preferences::load(access.as_ref()).await.unwrap();
        assert_eq!(first.appearance.theme, rsi_client_preferences::Theme::Dark);
        assert_eq!(first.appearance.content_font_size, 16);
        assert_eq!(
            first.web.submit_key,
            if web.get("submit_key").is_some() {
                rsi_client_preferences::SubmitKey::ModEnter
            } else {
                rsi_client_preferences::SubmitKey::Enter
            }
        );
        assert!(owner.dispose().await.is_clean());
        let next = runtime.root().apply(factory(), Value::Null).await.unwrap();
        assert_eq!(next.snapshot().state, FiberState::Active);
        assert_eq!(Preferences::load(access.as_ref()).await.unwrap(), first);
        assert!(runtime.shutdown().await.is_clean());
    }
    let runtime = setup(json!({"rsi.client":{"appearance":{"theme":"light"}}})).await;
    let owner = runtime.root().apply(factory(), Value::Null).await.unwrap();
    assert_eq!(owner.snapshot().state, FiberState::Active);
    let access = runtime
        .root()
        .lookup_local::<SettingsAccessContract>()
        .unwrap();
    let value = Preferences::load(access.as_ref()).await.unwrap();
    assert_eq!(value.web, Preferences::default().web);
    assert_eq!(value.appearance.theme, rsi_client_preferences::Theme::Light);
    assert!(runtime.shutdown().await.is_clean());
}
