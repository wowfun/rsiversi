use async_trait::async_trait;
use rsi_meta::{ResolvedFactory, Runtime, UpdateMode};
use rsi_storage::{
    KvBackend, Result as StorageResult, StorageError, StorageFactory, StorageHubContract,
    StoredDomain,
};
use serde_json::Value;
use std::sync::Arc;

#[derive(Debug)]
struct NoopBackend;

#[async_trait]
impl KvBackend for NoopBackend {
    async fn load(&self, _domain: &str) -> StorageResult<Option<StoredDomain>> {
        Ok(None)
    }

    async fn put(
        &self,
        _domain: &str,
        _version: u32,
        _key: &str,
        _value: &Value,
    ) -> StorageResult<()> {
        Ok(())
    }

    async fn delete(&self, _domain: &str, _version: u32, _key: &str) -> StorageResult<()> {
        Ok(())
    }
}

#[tokio::test]
async fn backend_registration_is_exact_and_lease_owned() {
    let runtime = Runtime::default();
    let fiber = runtime
        .root()
        .apply(
            ResolvedFactory::linked(
                "rsi.storage",
                "test",
                UpdateMode::Replayable,
                Arc::new(StorageFactory),
            ),
            Value::Null,
        )
        .await
        .unwrap();
    let hub = runtime
        .root()
        .lookup_local::<StorageHubContract>()
        .expect("hub is active");
    let lease = hub.register("primary", Arc::new(NoopBackend)).unwrap();
    assert!(matches!(
        hub.register("primary", Arc::new(NoopBackend)),
        Err(StorageError::DuplicateBackend(name)) if name == "primary"
    ));
    assert!(hub.resolve("primary").is_ok());

    drop(lease);
    assert!(matches!(
        hub.resolve("primary"),
        Err(StorageError::BackendUnavailable(name)) if name == "primary"
    ));

    assert!(fiber.dispose().await.is_clean());
    assert!(
        runtime
            .root()
            .lookup_local::<StorageHubContract>()
            .is_none()
    );
}
