use async_trait::async_trait;
use rsi_application::{ProfileCatalogSource, ScopedProfile};
use rsi_host::{Host, HostBuilder, Profile, ProfileEntry, ProfileFragment, ProfileProgram};
use rsi_meta::{
    ActivationPlan, ConfigValue, LocalContract, PluginFactory, PreparedActivation, Runtime,
    UpdateMode,
};
use std::sync::{Arc, Mutex};
use tokio::sync::watch;

#[derive(Debug)]
struct Label;
impl LocalContract for Label {
    const KEY: &'static str = "fixture.catalog.label";
    type Service = String;
}
#[derive(Debug)]
struct Leaf(&'static str);
#[async_trait]
impl PluginFactory for Leaf {
    fn prepare(&self, config: &ConfigValue) -> rsi_meta::Result<PreparedActivation> {
        Ok(PreparedActivation::new(config.clone()))
    }
    async fn activate(&self, plan: ActivationPlan) -> rsi_meta::Result<()> {
        plan.context()
            .provide_local::<Label>(Arc::new(self.0.into()))?;
        Ok(())
    }
}
fn host(label: &'static str) -> Arc<Host> {
    let mut builder = HostBuilder::without_paths("fixture");
    builder.register_local_contract::<Label>().unwrap();
    builder
        .register_linked("leaf", label, UpdateMode::Replayable, Arc::new(Leaf(label)))
        .unwrap();
    builder
        .register_fragment(ProfileFragment::new(
            "fixture",
            [ProfileEntry::new("leaf", "leaf", ConfigValue::Null)],
        ))
        .unwrap();
    Arc::new(builder.build().unwrap())
}
#[derive(Debug)]
struct Source {
    current: Mutex<Option<Arc<Host>>>,
    after_capture: Mutex<Option<Arc<Host>>>,
    changed: watch::Sender<u64>,
    captures: std::sync::atomic::AtomicUsize,
}
impl Source {
    fn publish(&self, host: Option<Arc<Host>>) {
        *self.current.lock().unwrap() = host;
        self.changed.send_modify(|value| *value += 1);
    }
}
impl ProfileCatalogSource for Source {
    fn snapshot(&self) -> rsi_host::Result<Arc<Host>> {
        self.captures
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let result = self
            .current
            .lock()
            .unwrap()
            .clone()
            .ok_or_else(|| rsi_host::HostError::Bootstrap("source unavailable".into()));
        if let Some(next) = self.after_capture.lock().unwrap().take() {
            self.publish(Some(next));
        }
        result
    }
    fn changes(&self) -> watch::Receiver<u64> {
        self.changed.subscribe()
    }
}
async fn until(check: impl Fn() -> bool) {
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        while !check() {
            tokio::time::sleep(std::time::Duration::from_millis(1)).await;
        }
    })
    .await
    .expect("catalog converges");
}

#[tokio::test]
async fn catalog_capture_race_recovery_and_retirement_use_the_same_profile() {
    let source = Arc::new(Source {
        current: Mutex::new(Some(host("old"))),
        after_capture: Mutex::new(Some(host("new"))),
        changed: watch::channel(0).0,
        captures: std::sync::atomic::AtomicUsize::new(0),
    });
    let weak = Arc::downgrade(&source);
    let runtime = Runtime::default();
    let profile = ScopedProfile::start_following(
        source.clone(),
        &runtime.root(),
        ProfileProgram::from_profile(Profile::default()),
    )
    .await
    .unwrap();
    until(|| {
        profile
            .lookup_local::<Label>()
            .is_some_and(|label| label.as_str() == "new")
    })
    .await;
    let label = profile.lookup_local::<Label>().unwrap();
    let input_revision = profile.updater().input_revision();
    source.publish(None);
    until(|| profile.catalog_diagnostic().is_some()).await;
    assert!(Arc::ptr_eq(
        &label,
        &profile.lookup_local::<Label>().unwrap()
    ));
    assert_eq!(profile.updater().input_revision(), input_revision);
    source.publish(Some(host("last")));
    until(|| {
        profile
            .lookup_local::<Label>()
            .is_some_and(|label| label.as_str() == "last")
    })
    .await;
    until(|| profile.catalog_diagnostic().is_none()).await;
    drop(source);
    // Parent-driven Meta retirement must stop the follower even while the public
    // ScopedProfile handle remains alive outside the retiring ownership tree.
    assert!(runtime.shutdown().await.is_clean());
    until(|| weak.upgrade().is_none()).await;
    assert_eq!(
        profile.profile_status().health(),
        rsi_host::ProfileHealth::Stopped
    );
}

#[derive(Debug)]
struct GatedLeaf {
    entered: tokio::sync::Notify,
    release: tokio::sync::Notify,
}
#[async_trait]
impl PluginFactory for GatedLeaf {
    fn prepare(&self, config: &ConfigValue) -> rsi_meta::Result<PreparedActivation> {
        Ok(PreparedActivation::new(config.clone()))
    }
    async fn activate(&self, plan: ActivationPlan) -> rsi_meta::Result<()> {
        self.entered.notify_one();
        self.release.notified().await;
        plan.context()
            .provide_local::<Label>(Arc::new("intermediate".into()))?;
        Ok(())
    }
}

#[tokio::test]
async fn pending_catalog_retries_after_a_concurrent_owner_input_commit() {
    let source = Arc::new(Source {
        current: Mutex::new(Some(host("old"))),
        after_capture: Mutex::new(None),
        changed: watch::channel(0).0,
        captures: std::sync::atomic::AtomicUsize::new(0),
    });
    let runtime = Runtime::default();
    let program = ProfileProgram::from_profile(Profile::default());
    let profile = ScopedProfile::start_following(source.clone(), &runtime.root(), program.clone())
        .await
        .unwrap();
    let gate = Arc::new(GatedLeaf {
        entered: tokio::sync::Notify::new(),
        release: tokio::sync::Notify::new(),
    });
    let mut builder = HostBuilder::without_paths("fixture");
    builder.register_local_contract::<Label>().unwrap();
    builder
        .register_linked("leaf", "intermediate", UpdateMode::Replayable, gate.clone())
        .unwrap();
    builder
        .register_fragment(ProfileFragment::new(
            "fixture",
            [ProfileEntry::new("leaf", "leaf", ConfigValue::Null)],
        ))
        .unwrap();
    let input = builder.build().unwrap().profile_input(program).unwrap();
    let updater = profile.updater();
    let pending = updater.submit(updater.input_revision(), input).unwrap();
    gate.entered.notified().await;
    let captured = source.captures.load(std::sync::atomic::Ordering::SeqCst);
    source.publish(Some(host("latest")));
    until(|| source.captures.load(std::sync::atomic::Ordering::SeqCst) > captured).await;
    // The in-progress owner command holds revision N while the follower queues N.
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    gate.release.notify_one();
    pending.wait().await.unwrap();
    until(|| {
        profile
            .lookup_local::<Label>()
            .is_some_and(|label| label.as_str() == "latest")
    })
    .await;
    assert!(profile.catalog_diagnostic().is_none());
    assert!(profile.shutdown().await.is_clean());
    assert!(runtime.shutdown().await.is_clean());
}
