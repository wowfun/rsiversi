use super::*;
use rsi_settings_protocol::{
    Result as SettingsResult, SettingsDocument, SettingsProvider, SettingsProviderContract,
};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

#[derive(Debug, Default)]
struct Storage {
    document: Mutex<SettingsDocument>,
    offline: AtomicBool,
    reads: AtomicUsize,
    writes: AtomicUsize,
}

#[async_trait]
impl SettingsProvider for Storage {
    fn writable(&self) -> bool {
        true
    }

    async fn load(&self) -> SettingsResult<SettingsDocument> {
        self.reads.fetch_add(1, Ordering::SeqCst);
        if self.offline.load(Ordering::SeqCst) {
            return Err(SettingsError::Io("storage offline".into()));
        }
        Ok(self.document.lock().unwrap().clone())
    }

    async fn compare_and_set(
        &self,
        namespace: &str,
        expected: Option<&Value>,
        replacement: Option<&Value>,
    ) -> SettingsResult<Option<Value>> {
        assert_eq!(expected, None);
        self.document
            .lock()
            .unwrap()
            .insert(namespace.into(), replacement.unwrap().clone());
        self.writes.fetch_add(1, Ordering::SeqCst);
        self.offline.store(true, Ordering::SeqCst);
        Err(SettingsError::OutcomeUnknown("lost acknowledgment".into()))
    }
}

#[derive(Debug)]
struct StoragePlugin(Arc<Storage>);

#[async_trait]
impl PluginFactory for StoragePlugin {
    fn prepare(&self, _: &ConfigValue) -> rsi_meta::Result<PreparedActivation> {
        Ok(PreparedActivation::new(Value::Null))
    }

    async fn activate(&self, plan: ActivationPlan) -> rsi_meta::Result<()> {
        let supply = plan
            .context()
            .provide_local::<SettingsProviderContract>(self.0.clone())?;
        plan.defer(
            "withdraw test storage",
            Box::new(move || {
                Box::pin(async move {
                    drop(supply);
                    Ok(())
                })
            }),
        )
    }
}

#[tokio::test]
async fn next_batch_recovers_a_committed_setting_after_lost_acknowledgment() {
    let runtime = rsi_meta::Runtime::default();
    let ctx = runtime.root();
    let storage = Arc::new(Storage::default());
    for (name, factory) in [
        (
            "test.storage",
            Arc::new(StoragePlugin(storage.clone())) as Arc<dyn PluginFactory>,
        ),
        ("test.settings", Arc::new(rsi_settings::SettingsFactory)),
    ] {
        ctx.apply(
            ResolvedFactory::linked(name, "test", UpdateMode::Replayable, factory),
            Value::Null,
        )
        .await
        .unwrap();
    }
    let limits = Parallelism::attach(&ctx, 2).await.unwrap();
    let scope = ctx
        .lookup_local::<SettingsContract>()
        .unwrap()
        .scope("rsi.agent-loop")
        .unwrap();
    assert_eq!(limits.limit().await.unwrap(), 2);
    assert!(matches!(
        scope
            .replace(0, json!({"max_parallel_tool_calls":10}))
            .await,
        Err(SettingsError::OutcomeUnknown(_))
    ));
    assert!(limits.limit().await.is_err());
    storage.offline.store(false, Ordering::SeqCst);
    assert_eq!(limits.limit().await.unwrap(), 10);
    let reads = storage.reads.load(Ordering::SeqCst);
    assert_eq!(limits.limit().await.unwrap(), 10);
    assert_eq!(storage.reads.load(Ordering::SeqCst), reads);
    assert_eq!(storage.writes.load(Ordering::SeqCst), 1);
}
