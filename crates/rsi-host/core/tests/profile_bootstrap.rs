use async_trait::async_trait;
use rsi_host::{
    HostBuilder, HostError, HostPaths, Profile, ProfileEntry, ProfileHealth, ProfilePatch,
};
use rsi_meta::{
    ActivationPlan, ConfigValue, FiberState, LocalContract, MetaError, PluginFactory,
    PreparedActivation, UpdateMode,
};
use rsi_meta_profile::ProfileError;
use serde_json::{Value, json};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

enum ScopedValue {}

impl LocalContract for ScopedValue {
    const KEY: &'static str = "test.scoped";
    type Service = AtomicUsize;
}

#[derive(Debug)]
struct Provider;

#[async_trait]
impl PluginFactory for Provider {
    fn prepare(&self, desired: &ConfigValue) -> rsi_meta::Result<PreparedActivation> {
        Ok(PreparedActivation::new(desired.clone()))
    }

    async fn activate(&self, plan: ActivationPlan) -> rsi_meta::Result<()> {
        let service = Arc::new(AtomicUsize::new(0));
        let _supply = plan.context().provide_local::<ScopedValue>(service)?;
        Ok(())
    }
}

#[derive(Debug)]
struct Consumer(Arc<AtomicUsize>);

#[async_trait]
impl PluginFactory for Consumer {
    fn prepare(&self, desired: &ConfigValue) -> rsi_meta::Result<PreparedActivation> {
        Ok(PreparedActivation::new(desired.clone()).requiring_local::<ScopedValue>())
    }

    async fn activate(&self, plan: ActivationPlan) -> rsi_meta::Result<()> {
        plan.local::<ScopedValue>()?.fetch_add(1, Ordering::SeqCst);
        self.0.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
}

#[derive(Debug)]
struct RecordConfig(Arc<Mutex<Vec<Value>>>);

#[async_trait]
impl PluginFactory for RecordConfig {
    fn prepare(&self, desired: &ConfigValue) -> rsi_meta::Result<PreparedActivation> {
        self.0.lock().unwrap().push(desired.clone());
        Ok(PreparedActivation::new(desired.clone()))
    }

    async fn activate(&self, _plan: ActivationPlan) -> rsi_meta::Result<()> {
        Ok(())
    }
}

#[derive(Debug)]
struct EffectProbe {
    live: Arc<AtomicUsize>,
    fail: bool,
}

#[async_trait]
impl PluginFactory for EffectProbe {
    fn prepare(&self, desired: &ConfigValue) -> rsi_meta::Result<PreparedActivation> {
        Ok(PreparedActivation::new(desired.clone()))
    }

    async fn activate(&self, plan: ActivationPlan) -> rsi_meta::Result<()> {
        if self.fail {
            return Err(MetaError::Activation("expected".to_owned()));
        }
        self.live.fetch_add(1, Ordering::SeqCst);
        let live = Arc::clone(&self.live);
        plan.defer(
            "release probe",
            Box::new(move || {
                Box::pin(async move {
                    live.fetch_sub(1, Ordering::SeqCst);
                    Ok(())
                })
            }),
        )?;
        Ok(())
    }
}

fn paths(root: &std::path::Path) -> HostPaths {
    HostPaths::new(root.join("config"), root.join("state"), root.join("cache")).unwrap()
}

#[tokio::test]
async fn host_maps_group_isolation_and_exposes_profile_control_without_root_mutation() {
    let temp = tempfile::tempdir().unwrap();
    let profile = temp.path().join("profile.toml");
    std::fs::write(
        &profile,
        r#"
format = 1
[[steps]]
kind = "group"
id = "scope"
[steps.isolation]
local = ["test.scoped"]
[[steps.nodes]]
kind = "plugin"
id = "provider"
plugin = "provider"
[[steps.nodes]]
kind = "plugin"
id = "consumer"
plugin = "consumer"
"#,
    )
    .unwrap();
    let activated = Arc::new(AtomicUsize::new(0));
    let mut builder = HostBuilder::new(paths(temp.path()));
    builder.register_local_contract::<ScopedValue>().unwrap();
    builder
        .register_linked("provider", "1", UpdateMode::Replayable, Arc::new(Provider))
        .unwrap();
    builder
        .register_linked(
            "consumer",
            "1",
            UpdateMode::Replayable,
            Arc::new(Consumer(Arc::clone(&activated))),
        )
        .unwrap();
    let running = builder.build().unwrap().start_file(&profile).await.unwrap();
    assert_eq!(activated.load(Ordering::SeqCst), 1);
    assert!(running.lookup_local::<ScopedValue>().is_none());
    assert_eq!(running.profile_status().health(), ProfileHealth::Converged);
    assert_eq!(running.profile_snapshot().nodes()[0].children().len(), 2);
    assert!(
        running
            .lookup_local::<rsi_host::ProfileControlContract>()
            .is_some()
    );
    let _ = running.shutdown().await;
}

#[tokio::test]
async fn linked_prefix_and_frozen_rhai_environment_execute_before_file_rows() {
    let temp = tempfile::tempdir().unwrap();
    let profile = temp.path().join("profile.toml");
    std::fs::write(
        &profile,
        "format = 1\n[[steps]]\nkind = \"plugin\"\nid = \"file\"\nplugin = \"record\"\nconfig = { layer = \"file\" }\n[[steps]]\nkind = \"plugin\"\nid = \"environment\"\nplugin = \"record\"\nconfig_rhai = \"#{ answer: defines.answer, platform: platform }\"\n",
    )
    .unwrap();
    let seen = Arc::new(Mutex::new(Vec::new()));
    let mut builder = HostBuilder::new(paths(temp.path()));
    builder.platform("frozen-test").unwrap();
    builder.define("answer", json!(42)).unwrap();
    builder
        .register_linked(
            "record",
            "1",
            UpdateMode::Replayable,
            Arc::new(RecordConfig(Arc::clone(&seen))),
        )
        .unwrap();
    builder
        .register_fragment(rsi_host::ProfileFragment::new(
            "base",
            [ProfileEntry::new(
                "linked",
                "record",
                json!({"layer": "linked"}),
            )],
        ))
        .unwrap();
    builder
        .register_launch_patch(ProfilePatch::ReplaceConfig {
            target: "file".into(),
            config: json!({"answer": 99, "layer": "launch"}),
        })
        .unwrap();
    let running = builder.build().unwrap().start_file(profile).await.unwrap();
    assert_eq!(
        *seen.lock().unwrap(),
        vec![
            json!({"layer": "linked"}),
            json!({"answer": 99, "layer": "launch"}),
            json!({"answer": 42, "platform": "frozen-test"})
        ]
    );
    let status = running.profile_status();
    let ids = status
        .target()
        .iter()
        .map(|target| target.id().as_str())
        .collect::<Vec<_>>();
    assert_eq!(ids, ["linked", "file", "environment"]);
    let _ = running.shutdown().await;
}

#[tokio::test]
async fn duplicate_or_missing_identity_fails_before_any_factory_prepare() {
    let temp = tempfile::tempdir().unwrap();
    let prepares = Arc::new(Mutex::new(Vec::new()));
    let mut builder = HostBuilder::new(paths(temp.path()));
    builder
        .register_linked(
            "record",
            "1",
            UpdateMode::Replayable,
            Arc::new(RecordConfig(Arc::clone(&prepares))),
        )
        .unwrap();
    let error = builder
        .build()
        .unwrap()
        .start(Profile::new([
            ProfileEntry::new("same", "record", Value::Null),
            ProfileEntry::new("same", "missing", Value::Null),
        ]))
        .await
        .unwrap_err();
    assert!(matches!(
        error,
        HostError::Profile(ProfileError::DuplicateInstance { .. })
    ));
    assert!(prepares.lock().unwrap().is_empty());
}

#[tokio::test]
async fn startup_failure_rolls_back_already_applied_children() {
    let temp = tempfile::tempdir().unwrap();
    let live = Arc::new(AtomicUsize::new(0));
    let mut builder = HostBuilder::new(paths(temp.path()));
    for (id, fail) in [("owned", false), ("failing", true)] {
        builder
            .register_linked(
                id,
                "1",
                UpdateMode::Replayable,
                Arc::new(EffectProbe {
                    live: Arc::clone(&live),
                    fail,
                }),
            )
            .unwrap();
    }
    let error = builder
        .build()
        .unwrap()
        .start(Profile::new([
            ProfileEntry::new("owned", "owned", Value::Null),
            ProfileEntry::new("failing", "failing", Value::Null),
        ]))
        .await
        .unwrap_err();
    assert!(matches!(error, HostError::Bootstrap(_)));
    assert_eq!(live.load(Ordering::SeqCst), 0);
    let _ = FiberState::Disposed;
}
