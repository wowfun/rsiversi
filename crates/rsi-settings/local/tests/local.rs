use rsi_meta::{ResolvedFactory, Runtime, UpdateMode};
use rsi_settings::SettingsFactory;
use rsi_settings_local::LocalSettingsFactory;
use rsi_settings_protocol::{SettingsContract, SettingsError, SettingsSpec, ValidateWith};
use serde_json::{Value, json};
use std::fs;
use std::sync::Arc;

fn linked(id: &str, factory: Arc<dyn rsi_meta::PluginFactory>) -> ResolvedFactory {
    ResolvedFactory::linked(id, "test", UpdateMode::Replayable, factory)
}

#[tokio::test]
async fn local_write_preserves_unloaded_namespaces_and_detects_external_change() {
    let temporary = tempfile::tempdir().unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        fs::set_permissions(temporary.path(), fs::Permissions::from_mode(0o755)).unwrap();
    }
    let path = temporary.path().join("settings.json");
    fs::write(&path, br#"{"loaded":{"value":1},"unloaded":{"keep":true}}"#).unwrap();
    let runtime = Runtime::default();
    let provider = runtime
        .root()
        .apply(
            linked("rsi.settings.local", Arc::new(LocalSettingsFactory)),
            json!({"path":path}),
        )
        .await
        .unwrap();
    let service = runtime
        .root()
        .apply(
            linked("rsi.settings", Arc::new(SettingsFactory)),
            Value::Null,
        )
        .await
        .unwrap();
    let settings = runtime.root().lookup_local::<SettingsContract>().unwrap();
    let registration = settings
        .register(SettingsSpec {
            namespace: "loaded".into(),
            defaults: json!({}),
            base: json!({}),
            validator: Arc::new(ValidateWith(|_: &Value| Ok(()))),
        })
        .unwrap();
    registration
        .scope
        .replace(0, json!({"value":2}))
        .await
        .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        assert_eq!(
            fs::metadata(temporary.path()).unwrap().permissions().mode() & 0o777,
            0o755
        );
    }
    let document: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    assert_eq!(document["unloaded"], json!({"keep":true}));

    fs::write(&path, br#"{"loaded":{"value":3},"unloaded":{"keep":true}}"#).unwrap();
    assert_eq!(
        registration.scope.replace(1, json!({"value":4})).await,
        Err(SettingsError::ConcurrentDocumentChange)
    );
    assert_eq!(registration.scope.get().unwrap().value, json!({"value":2}));

    drop(registration);
    drop(settings);
    assert!(service.dispose().await.is_clean());
    assert!(provider.dispose().await.is_clean());
}

#[cfg(unix)]
#[tokio::test]
async fn preplaced_temporary_symlinks_cannot_overwrite_their_target() {
    use std::os::unix::fs::symlink;

    let temporary = tempfile::tempdir().unwrap();
    let path = temporary.path().join("settings.json");
    fs::write(&path, br#"{"loaded":{"value":1}}"#).unwrap();
    let victim = temporary.path().join("victim");
    fs::write(&victim, b"keep me").unwrap();
    for sequence in 0..512 {
        symlink(
            &victim,
            temporary.path().join(format!(
                ".settings.json.{}.{sequence}.tmp",
                std::process::id()
            )),
        )
        .unwrap();
    }

    let runtime = Runtime::default();
    let provider = runtime
        .root()
        .apply(
            linked("rsi.settings.local", Arc::new(LocalSettingsFactory)),
            json!({"path":path}),
        )
        .await
        .unwrap();
    let service = runtime
        .root()
        .apply(
            linked("rsi.settings", Arc::new(SettingsFactory)),
            Value::Null,
        )
        .await
        .unwrap();
    let registration = runtime
        .root()
        .lookup_local::<SettingsContract>()
        .unwrap()
        .register(SettingsSpec {
            namespace: "loaded".into(),
            defaults: json!({}),
            base: json!({}),
            validator: Arc::new(ValidateWith(|_: &Value| Ok(()))),
        })
        .unwrap();

    let _result = registration.scope.replace(0, json!({"value":2})).await;
    assert_eq!(fs::read(&victim).unwrap(), b"keep me");

    drop(registration);
    assert!(service.dispose().await.is_clean());
    assert!(provider.dispose().await.is_clean());
}

#[cfg(unix)]
#[tokio::test]
async fn preplaced_lock_symlink_is_rejected_without_chmodding_its_target() {
    use std::os::unix::fs::{PermissionsExt as _, symlink};

    let temporary = tempfile::tempdir().unwrap();
    let path = temporary.path().join("settings.json");
    fs::write(&path, br#"{"loaded":{"value":1}}"#).unwrap();
    let victim = temporary.path().join("victim");
    fs::write(&victim, b"keep me").unwrap();
    fs::set_permissions(&victim, fs::Permissions::from_mode(0o644)).unwrap();
    symlink(&victim, temporary.path().join(".settings.json.lock")).unwrap();

    let runtime = Runtime::default();
    let provider = runtime
        .root()
        .apply(
            linked("rsi.settings.local", Arc::new(LocalSettingsFactory)),
            json!({"path":path}),
        )
        .await
        .unwrap();
    let service = runtime
        .root()
        .apply(
            linked("rsi.settings", Arc::new(SettingsFactory)),
            Value::Null,
        )
        .await
        .unwrap();
    let settings = runtime.root().lookup_local::<SettingsContract>().unwrap();
    let registration = settings
        .register(SettingsSpec {
            namespace: "loaded".into(),
            defaults: json!({}),
            base: json!({}),
            validator: Arc::new(ValidateWith(|_: &Value| Ok(()))),
        })
        .unwrap();

    assert!(
        registration
            .scope
            .replace(0, json!({"value":2}))
            .await
            .is_err(),
        "a lock symlink must fail closed"
    );
    assert_eq!(
        fs::metadata(&victim).unwrap().permissions().mode() & 0o777,
        0o644
    );

    drop(registration);
    drop(settings);
    assert!(service.dispose().await.is_clean());
    assert!(provider.dispose().await.is_clean());
}
