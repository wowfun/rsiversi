use super::sources::{fixture, view};
use super::*;
use rsi_settings_protocol::*;
use serde_json::json;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

#[derive(Debug)]
pub(super) struct Fixture {
    blocked: AtomicBool,
    active: AtomicUsize,
    calls: AtomicUsize,
    writes: AtomicUsize,
    read_only: AtomicBool,
    changed_scope: AtomicBool,
    snapshot: Mutex<SettingsSnapshot>,
}
impl Default for Fixture {
    fn default() -> Self {
        Self {
            blocked: AtomicBool::new(false),
            active: AtomicUsize::new(0),
            calls: AtomicUsize::new(0),
            writes: AtomicUsize::new(0),
            read_only: AtomicBool::new(false),
            changed_scope: AtomicBool::new(false),
            snapshot: Mutex::new(SettingsSnapshot {
                scope_id: SettingsScopeId::parse("0".repeat(32)).unwrap(),
                revision: 0,
                value: json!({"enabled":true}),
            }),
        }
    }
}
impl Fixture {
    async fn reading(&self) {
        self.calls.fetch_add(1, Ordering::SeqCst);
        if self.blocked.load(Ordering::SeqCst) {
            struct Active<'a>(&'a AtomicUsize);
            impl Drop for Active<'_> {
                fn drop(&mut self) {
                    self.0.fetch_sub(1, Ordering::SeqCst);
                }
            }
            self.active.fetch_add(1, Ordering::SeqCst);
            let _active = Active(&self.active);
            std::future::pending::<()>().await;
        }
    }
}
#[async_trait]
impl SettingsAccess for Fixture {
    async fn list(
        &self,
        after: Option<&str>,
        limit: usize,
    ) -> rsi_settings_protocol::Result<SettingsPage> {
        self.reading().await;
        let names: Vec<_> = (0..66)
            .map(|index| format!("fixture.{index:02}"))
            .filter(|name| after.is_none_or(|after| name.as_str() > after))
            .collect();
        let next = (names.len() > limit).then(|| names[limit - 1].clone());
        Ok(SettingsPage {
            namespaces: names.into_iter().take(limit).collect(),
            next,
        })
    }
    async fn describe(
        &self,
        namespace: &str,
    ) -> rsi_settings_protocol::Result<SettingsDescription> {
        self.reading().await;
        Ok(SettingsDescription {
            namespace: namespace.into(),
            version: self.snapshot.lock().unwrap().version(),
            defaults: json!({"enabled":true}),
            writable: !self.read_only.load(Ordering::SeqCst),
            metadata: SettingsMetadata {
                schema: json!({"type":"object","properties":{"enabled":{"type":"boolean"}}}),
                applies: SettingsApply::Restart,
                description: "Fixture restarts".into(),
                sensitive_fields: vec![vec!["reference".into()]],
            },
        })
    }
    async fn read(&self, namespace: &str) -> rsi_settings_protocol::Result<SettingsSnapshot> {
        if namespace == rsi_client_preferences::NAMESPACE {
            return Err(SettingsError::UnknownNamespace(namespace.into()));
        }
        self.reading().await;
        let mut snapshot = self.snapshot.lock().unwrap().clone();
        if self.changed_scope.load(Ordering::SeqCst) {
            snapshot.scope_id = SettingsScopeId::parse("1".repeat(32)).unwrap();
        }
        Ok(snapshot)
    }
    async fn replace(
        &self,
        _: &str,
        expected: &SettingsVersion,
        value: serde_json::Value,
    ) -> rsi_settings_protocol::Result<SettingsSnapshot> {
        self.writes.fetch_add(1, Ordering::SeqCst);
        let mut snapshot = self.snapshot.lock().unwrap();
        assert_eq!(&snapshot.version(), expected);
        snapshot.revision += 1;
        snapshot.value = value;
        Ok(snapshot.clone())
    }
    async fn clear(
        &self,
        _: &str,
        _: &SettingsVersion,
    ) -> rsi_settings_protocol::Result<SettingsSnapshot> {
        unreachable!()
    }
}

#[tokio::test]
async fn settings_pages_descriptions_and_read_only_editor_preserve_cas() {
    let (runtime, backend, app) = fixture().await;
    app.command(r#"{"action":"settings_list"}"#).await.unwrap();
    let first = view(&app)["settings_catalog"].clone();
    assert_eq!(first["page"]["namespaces"].as_array().unwrap().len(), 64);
    let next = json!({"action":"settings_next","ticket":first["ticket"]}).to_string();
    app.command(&next).await.unwrap();
    let second = view(&app)["settings_catalog"].clone();
    assert_eq!(
        second["page"]["namespaces"],
        json!(["fixture.64", "fixture.65"])
    );
    let calls = backend.settings.calls.load(Ordering::SeqCst);
    app.command(&next).await.unwrap();
    assert_eq!(view(&app)["settings_catalog"], second);
    assert_eq!(backend.settings.calls.load(Ordering::SeqCst), calls);
    app.command(r#"{"action":"settings_read","namespace":"fixture.00"}"#)
        .await
        .unwrap();
    let editor = view(&app)["settings"].clone();
    assert_eq!(editor["description"]["metadata"]["applies"], "restart");
    assert_eq!(editor["description"]["defaults"], json!({"enabled":true}));
    let save =
        json!({"action":"settings_save","ticket":editor["ticket"],"text":"{\"enabled\":false}"})
            .to_string();
    app.command(&save).await.unwrap();
    assert_eq!(
        view(&app)["settings"]["description"]["version"]["revision"],
        1
    );
    assert!(app.command(&save).await.is_err());
    assert_eq!(backend.settings.writes.load(Ordering::SeqCst), 1);
    backend.settings.read_only.store(true, Ordering::SeqCst);
    app.command(r#"{"action":"settings_read","namespace":"fixture.00"}"#)
        .await
        .unwrap();
    let editor = view(&app)["settings"].clone();
    assert_eq!(editor["description"]["writable"], false);
    assert!(
        app.command(
            &json!({"action":"settings_save","ticket":editor["ticket"],"text":"{}"}).to_string()
        )
        .await
        .is_err()
    );
    assert_eq!(backend.settings.writes.load(Ordering::SeqCst), 1);
    assert!(runtime.shutdown().await.is_clean());
}

#[tokio::test]
async fn closing_settings_cancels_reads_and_registration_changes_stay_in_their_detail() {
    let (runtime, backend, app) = fixture().await;
    for command in [
        r#"{"action":"settings_list"}"#,
        r#"{"action":"settings_read","namespace":"fixture.00"}"#,
    ] {
        backend.settings.blocked.store(true, Ordering::SeqCst);
        let old = app.command(command);
        tokio::time::timeout(std::time::Duration::from_secs(1), async {
            while backend.settings.active.load(Ordering::SeqCst) != 1 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        app.command(r#"{"action":"close_detail"}"#).await.unwrap();
        old.await.unwrap();
        assert_eq!(backend.settings.active.load(Ordering::SeqCst), 0);
        assert!(view(&app)["settings_catalog"].is_null());
        assert!(view(&app)["settings"].is_null());
        assert_eq!(view(&app)["notice"], "");
    }
    backend.settings.blocked.store(false, Ordering::SeqCst);
    backend.settings.changed_scope.store(true, Ordering::SeqCst);
    app.command(r#"{"action":"settings_read","namespace":"fixture.00"}"#)
        .await
        .unwrap();
    assert!(view(&app)["settings"].is_null());
    assert!(
        view(&app)["settings_catalog"]["error"]
            .as_str()
            .unwrap()
            .contains("registration changed")
    );
    assert_eq!(view(&app)["notice"], "");
    assert!(runtime.shutdown().await.is_clean());
}
