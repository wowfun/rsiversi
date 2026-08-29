use async_trait::async_trait;
use rsi_meta::{ResolvedFactory, Runtime, UpdateMode};
use rsi_storage::{KvBackend, Result as StorageResult, StorageError, StorageFactory, StoredDomain};
use rsi_storage_domain::{DomainFacilityContract, DomainFactory, DomainSpec};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use tokio::sync::Notify;

#[derive(Debug, Default)]
struct Backend {
    state: Mutex<Option<StoredDomain>>,
    reject_writes: Mutex<bool>,
}

#[derive(Debug, Default)]
struct CommitThenPauseBackend {
    state: Mutex<Option<StoredDomain>>,
    committed: Notify,
    release: Notify,
}

#[async_trait]
impl KvBackend for CommitThenPauseBackend {
    async fn load(&self, _domain: &str) -> StorageResult<Option<StoredDomain>> {
        Ok(self.state.lock().unwrap().clone())
    }

    async fn put(
        &self,
        _domain: &str,
        version: u32,
        key: &str,
        value: &Value,
    ) -> StorageResult<()> {
        {
            let mut state = self.state.lock().unwrap();
            state
                .get_or_insert_with(|| StoredDomain {
                    version,
                    records: BTreeMap::new(),
                })
                .records
                .insert(key.to_owned(), value.clone());
        }
        self.committed.notify_one();
        self.release.notified().await;
        Ok(())
    }

    async fn delete(&self, _domain: &str, _version: u32, key: &str) -> StorageResult<()> {
        if let Some(state) = self.state.lock().unwrap().as_mut() {
            state.records.remove(key);
        }
        self.committed.notify_one();
        self.release.notified().await;
        Ok(())
    }
}

#[async_trait]
impl KvBackend for Backend {
    async fn load(&self, _domain: &str) -> StorageResult<Option<StoredDomain>> {
        Ok(self.state.lock().unwrap().clone())
    }

    async fn put(
        &self,
        _domain: &str,
        version: u32,
        key: &str,
        value: &Value,
    ) -> StorageResult<()> {
        if *self.reject_writes.lock().unwrap() {
            return Err(StorageError::Io("injected failure".into()));
        }
        let mut state = self.state.lock().unwrap();
        let domain = state.get_or_insert_with(|| StoredDomain {
            version,
            records: BTreeMap::new(),
        });
        domain.records.insert(key.to_owned(), value.clone());
        Ok(())
    }

    async fn delete(&self, _domain: &str, _version: u32, key: &str) -> StorageResult<()> {
        if *self.reject_writes.lock().unwrap() {
            return Err(StorageError::Io("injected failure".into()));
        }
        if let Some(state) = self.state.lock().unwrap().as_mut() {
            state.records.remove(key);
        }
        Ok(())
    }
}

#[tokio::test]
async fn failed_backend_write_never_changes_published_snapshot() {
    let runtime = Runtime::default();
    let hub_fiber = runtime
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
        .lookup_local::<rsi_storage::StorageHubContract>()
        .unwrap();
    let backend = Arc::new(Backend::default());
    let lease = hub.register("memory", backend.clone()).unwrap();
    let domain_fiber = runtime
        .root()
        .apply(
            ResolvedFactory::linked(
                "rsi.storage.domain",
                "test",
                UpdateMode::Replayable,
                Arc::new(DomainFactory),
            ),
            Value::Null,
        )
        .await
        .unwrap();
    let facility = runtime
        .root()
        .lookup_local::<DomainFacilityContract>()
        .unwrap();
    let domain = facility
        .open(DomainSpec {
            id: "workspace".into(),
            backend: "memory".into(),
            version: 1,
            maximum_records: 4,
            maximum_bytes: 64,
        })
        .await
        .unwrap();

    domain.put("one", json!({"value": 1})).await.unwrap();
    *backend.reject_writes.lock().unwrap() = true;
    assert!(domain.put("two", json!({"value": 2})).await.is_err());
    assert_eq!(
        domain.snapshot().await,
        BTreeMap::from([("one".into(), json!({"value": 1}))])
    );
    *backend.reject_writes.lock().unwrap() = false;
    assert!(matches!(
        domain.put("large", Value::String("x".repeat(64))).await,
        Err(StorageError::InvalidInput(message)) if message.contains("aggregate byte bound")
    ));
    assert_eq!(
        backend.state.lock().unwrap().as_ref().unwrap().records,
        BTreeMap::from([("one".into(), json!({"value": 1}))])
    );

    drop(domain);
    assert!(domain_fiber.dispose().await.is_clean());
    drop(lease);
    assert!(hub_fiber.dispose().await.is_clean());
}

#[tokio::test]
async fn cancelling_a_caller_after_backend_commit_cannot_split_the_domain_snapshot() {
    let runtime = Runtime::default();
    let hub_fiber = runtime
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
        .lookup_local::<rsi_storage::StorageHubContract>()
        .unwrap();
    let backend = Arc::new(CommitThenPauseBackend::default());
    let lease = hub.register("pausing", backend.clone()).unwrap();
    let domain_fiber = runtime
        .root()
        .apply(
            ResolvedFactory::linked(
                "rsi.storage.domain",
                "test",
                UpdateMode::Replayable,
                Arc::new(DomainFactory),
            ),
            Value::Null,
        )
        .await
        .unwrap();
    let facility = runtime
        .root()
        .lookup_local::<DomainFacilityContract>()
        .unwrap();
    let domain = facility
        .open(DomainSpec {
            id: "workspace".into(),
            backend: "pausing".into(),
            version: 1,
            maximum_records: 4,
            maximum_bytes: 1024,
        })
        .await
        .unwrap();
    let writing = domain.clone();
    let write = tokio::spawn(async move { writing.put("one", json!({"value": 1})).await });
    backend.committed.notified().await;
    write.abort();
    backend.release.notify_one();

    tokio::time::timeout(std::time::Duration::from_millis(250), async {
        loop {
            if domain.snapshot().await.contains_key("one") {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("domain-owned commit must publish the live snapshot");

    drop(domain);
    assert!(domain_fiber.dispose().await.is_clean());
    drop(lease);
    assert!(hub_fiber.dispose().await.is_clean());
}
