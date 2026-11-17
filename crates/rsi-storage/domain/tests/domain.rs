use rsi_meta::{FiberHandle, ResolvedFactory, Runtime, UpdateMode};
use rsi_storage::{
    Address, BackendLease, Layout, RecordPolicy, StorageError, StorageFactory, StorageHubContract,
};
use rsi_storage_domain::{
    Domain, DomainFacility, DomainFacilityContract, DomainFactory, DomainSpec, Schema,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    future::Future,
    pin::pin,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    task::{Context, Poll, Waker},
};
use tokio::sync::broadcast::error::TryRecvError;
mod support;
use support::Backend;

struct Fixture {
    _runtime: Runtime,
    hub: FiberHandle,
    domains: FiberHandle,
    lease: BackendLease,
    facility: Arc<dyn DomainFacility>,
    backend: Arc<Backend>,
}
fn linked(id: &str, factory: Arc<dyn rsi_meta::PluginFactory>) -> ResolvedFactory {
    ResolvedFactory::linked(id, "test", UpdateMode::Replayable, factory)
}
fn spec() -> DomainSpec {
    DomainSpec {
        id: "domain".into(),
        backend: "memory".into(),
        maximum_records: 32,
        maximum_bytes: 4096,
        ..DomainSpec::default()
    }
}
fn schema() -> Schema {
    Schema::default().table::<Value>("rows").unwrap()
}
fn address(key: &str) -> Address {
    Address::record("rows", key).unwrap()
}
impl Fixture {
    async fn new() -> Self {
        let runtime = Runtime::default();
        let hub = runtime
            .root()
            .apply(linked("storage", Arc::new(StorageFactory)), Value::Null)
            .await
            .unwrap();
        let backend = Arc::new(Backend::default());
        let lease = runtime
            .root()
            .lookup_local::<StorageHubContract>()
            .unwrap()
            .register("memory", backend.clone())
            .unwrap();
        let domains = runtime
            .root()
            .apply(linked("domains", Arc::new(DomainFactory)), Value::Null)
            .await
            .unwrap();
        let facility = runtime
            .root()
            .lookup_local::<DomainFacilityContract>()
            .unwrap();
        Self {
            _runtime: runtime,
            hub,
            domains,
            lease,
            facility,
            backend,
        }
    }
    async fn open(&self) -> Domain {
        self.facility.open(spec(), schema()).await.unwrap()
    }
    async fn close(self) {
        assert!(self.domains.dispose().await.is_clean());
        drop(self.lease);
        assert!(self.hub.dispose().await.is_clean());
    }
}

#[tokio::test]
async fn batch_capacity_preserves_full_replacement_and_bounds_missing_deletions() {
    let fixture = Fixture::new().await;
    let domain = fixture
        .facility
        .open(
            DomainSpec {
                maximum_records: 2,
                ..spec()
            },
            schema(),
        )
        .await
        .unwrap();
    let table = domain.table::<Value>("rows").unwrap();
    table.put("old-a", json!(1)).await.unwrap();
    table.put("old-b", json!(2)).await.unwrap();
    assert_eq!(
        domain
            .batch(vec![
                table.put_change("new-a", json!(3)).unwrap(),
                table.put_change("new-b", json!(4)).unwrap(),
                rsi_storage_domain::Change::delete(address("old-a")),
                rsi_storage_domain::Change::delete(address("old-b")),
            ])
            .await
            .unwrap(),
        4
    );
    let before = domain.snapshot().unwrap().revision();
    let changes = (0..5)
        .map(|key| rsi_storage_domain::Change::delete(address(&format!("missing-{key}"))))
        .collect();
    assert!(matches!(domain.batch(changes).await,
        Err(StorageError::InvalidInput(message)) if message.contains("batch capacity")));
    assert!(matches!(table.delete_many(["a", "b", "c", "d", "e"]).await,
        Err(StorageError::InvalidInput(message)) if message.contains("batch capacity")));
    assert_eq!(table.delete_many(["new-a", "new-a"]).await.unwrap(), 1);
    assert_eq!(domain.snapshot().unwrap().revision(), before + 1);
    assert_eq!(*table.get("new-b").unwrap().unwrap(), json!(4));
    fixture.close().await;
}

#[tokio::test]
async fn failed_backend_write_never_changes_published_snapshot() {
    let fixture = Fixture::new().await;
    let domain = fixture.open().await;
    let table = domain.table::<Value>("rows").unwrap();
    table.put("one", json!({"value":1})).await.unwrap();
    let (before, mut events) = domain.snapshot_and_subscribe().unwrap();
    fixture
        .backend
        .state
        .reject_write
        .store(true, Ordering::SeqCst);
    assert!(matches!(
        table.put("two", json!({"value":2})).await,
        Err(StorageError::Io(_))
    ));
    assert_eq!(domain.snapshot().unwrap().revision(), before.revision());
    assert!(table.get("two").unwrap().is_none());
    assert!(matches!(events.try_recv(), Err(TryRecvError::Empty)));
    fixture
        .backend
        .state
        .reject_write
        .store(false, Ordering::SeqCst);
    assert!(matches!(
        table.put("large", json!("x".repeat(4096))).await,
        Err(StorageError::InvalidInput(_))
    ));
    assert_eq!(fixture.backend.values("domain").len(), 1);
    fixture.close().await;
}

#[tokio::test]
async fn cancelling_a_caller_after_backend_commit_cannot_split_the_domain_snapshot() {
    let fixture = Fixture::new().await;
    let domain = fixture.open().await;
    let table = domain.table::<Value>("rows").unwrap();
    fixture
        .backend
        .state
        .pause_after_write
        .store(true, Ordering::SeqCst);
    let write = tokio::spawn({
        let table = table.clone();
        async move { table.put("one", json!({"value":1})).await }
    });
    fixture.backend.state.entered.notified().await;
    assert!(fixture.backend.values("domain").contains_key("rows/one"));
    assert!(table.get("one").unwrap().is_none());
    write.abort();
    assert!(write.await.unwrap_err().is_cancelled());
    fixture.backend.state.release.notify_one();
    table.put("barrier", Value::Null).await.unwrap();
    assert_eq!(
        table.get("one").unwrap().unwrap().as_ref(),
        &json!({"value":1})
    );
    fixture.close().await;
}

#[tokio::test]
async fn close_rejects_new_writes_and_drains_cancelled_accepted_jobs_and_events() {
    let fixture = Fixture::new().await;
    let domain = fixture.open().await;
    let table = domain.table::<Value>("rows").unwrap();
    let (_, mut events) = domain.snapshot_and_subscribe().unwrap();
    fixture
        .backend
        .state
        .pause_write
        .store(true, Ordering::SeqCst);
    let first = tokio::spawn({
        let table = table.clone();
        async move { table.put("one", json!(1)).await }
    });
    fixture.backend.state.entered.notified().await;
    {
        let mut queued = pin!(table.put("two", json!(2)));
        assert!(matches!(
            queued
                .as_mut()
                .poll(&mut Context::from_waker(Waker::noop())),
            Poll::Pending
        ));
    }
    {
        let mut close = pin!(domain.close());
        assert!(matches!(
            close.as_mut().poll(&mut Context::from_waker(Waker::noop())),
            Poll::Pending
        ));
    }
    assert!(matches!(
        table.put("three", json!(3)).await,
        Err(StorageError::Closed)
    ));
    fixture.backend.state.release.notify_one();
    first.await.unwrap().unwrap();
    domain.close().await;
    assert_eq!(fixture.backend.values("domain").len(), 2);
    assert_eq!(events.try_recv().unwrap().revision, 1);
    assert_eq!(events.try_recv().unwrap().revision, 2);
    assert!(matches!(events.try_recv(), Err(TryRecvError::Closed)));
    assert!(matches!(domain.snapshot(), Err(StorageError::Closed)));
    let reopened = fixture.open().await;
    // A repeated close of the old authority must not release its replacement.
    domain.close().await;
    let shared = fixture.open().await;
    shared
        .table::<Value>("rows")
        .unwrap()
        .put("one", json!(4))
        .await
        .unwrap();
    assert_eq!(
        reopened
            .table::<Value>("rows")
            .unwrap()
            .snapshot()
            .unwrap()
            .len(),
        2
    );
    assert_eq!(
        *reopened
            .table::<Value>("rows")
            .unwrap()
            .get("one")
            .unwrap()
            .unwrap(),
        json!(4)
    );
    fixture.close().await;
}

#[tokio::test]
async fn concurrent_updates_use_queue_slot_values_and_missing_keys_do_not_write() {
    let fixture = Fixture::new().await;
    let domain = fixture
        .facility
        .open(spec(), Schema::default().table::<u64>("rows").unwrap())
        .await
        .unwrap();
    let table = domain.table::<u64>("rows").unwrap();
    table.put("count", 0).await.unwrap();
    let mut writes = Vec::new();
    for _ in 0..100 {
        let table = table.clone();
        writes.push(tokio::spawn(async move {
            table.update("count", |value| Ok(value + 1)).await.unwrap()
        }));
    }
    for write in writes {
        write.await.unwrap();
    }
    assert_eq!(*table.get("count").unwrap().unwrap(), 100);
    let committed = fixture.backend.state.writes.load(Ordering::SeqCst);
    assert!(matches!(
        table.update("missing", |value| Ok(value + 1)).await,
        Err(StorageError::InvalidInput(_))
    ));
    assert_eq!(table.delete_many(["missing", "missing"]).await.unwrap(), 0);
    assert_eq!(
        fixture.backend.state.writes.load(Ordering::SeqCst),
        committed
    );
    fixture.close().await;
}

#[tokio::test]
async fn cancelled_waiters_do_not_join_the_accepted_write_backlog() {
    static ENCODES: AtomicUsize = AtomicUsize::new(0);
    #[derive(Deserialize)]
    struct Counted(u64);
    impl Serialize for Counted {
        fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
            ENCODES.fetch_add(1, Ordering::SeqCst);
            serializer.serialize_u64(self.0)
        }
    }
    let fixture = Fixture::new().await;
    let domain = fixture
        .facility
        .open(spec(), Schema::default().table::<Counted>("rows").unwrap())
        .await
        .unwrap();
    let table = domain.table::<Counted>("rows").unwrap();
    fixture
        .backend
        .state
        .pause_write
        .store(true, Ordering::SeqCst);
    let first = tokio::spawn({
        let table = table.clone();
        async move { table.put("first", Counted(1)).await }
    });
    fixture.backend.state.entered.notified().await;
    for index in 0..8 {
        let key = format!("queued-{index}");
        let mut write = pin!(table.put(&key, Counted(index)));
        assert!(
            write
                .as_mut()
                .poll(&mut Context::from_waker(Waker::noop()))
                .is_pending()
        );
    }
    let mut waiting = pin!(table.put("waiting", Counted(9)));
    assert!(
        waiting
            .as_mut()
            .poll(&mut Context::from_waker(Waker::noop()))
            .is_pending()
    );
    assert_eq!(ENCODES.load(Ordering::SeqCst), 2);
    let closing = domain.close();
    assert!(matches!(
        waiting
            .as_mut()
            .poll(&mut Context::from_waker(Waker::noop())),
        Poll::Ready(Err(StorageError::Closed))
    ));
    fixture.backend.state.release.notify_one();
    first.await.unwrap().unwrap();
    closing.await;
    let values = fixture.backend.values("domain");
    assert_eq!(values.len(), 2);
    assert!(values.contains_key("rows/first"));
    assert!(values.contains_key("rows/queued-0"));
    fixture.close().await;
}

#[tokio::test]
async fn snapshots_share_values_and_lagged_subscribers_resnapshot_without_a_gap() {
    let fixture = Fixture::new().await;
    let domain = fixture.open().await;
    let table = domain.table::<Value>("rows").unwrap();
    table.put("one", json!(0)).await.unwrap();
    let stable = table.snapshot().unwrap();
    assert!(Arc::ptr_eq(
        &stable.entries().next().unwrap().1,
        &table.get("one").unwrap().unwrap()
    ));
    let (_, mut old_events) = domain.snapshot_and_subscribe().unwrap();
    for n in 1..=260 {
        table.put("one", json!(n)).await.unwrap();
    }
    assert!(matches!(
        old_events.try_recv(),
        Err(TryRecvError::Lagged(_))
    ));
    let (snapshot, mut events) = domain.snapshot_and_subscribe().unwrap();
    assert_eq!(
        snapshot
            .get::<Value>(&address("one"))
            .unwrap()
            .unwrap()
            .as_ref(),
        &json!(260)
    );
    table.put("one", json!(261)).await.unwrap();
    let event = events.try_recv().unwrap();
    assert_eq!(event.revision, snapshot.revision() + 1);
    assert_eq!(event.address, address("one"));
    assert_eq!(stable.entries().next().unwrap().1.as_ref(), &json!(0));
    // A receiver can immediately reenter reads and writes; no callback runs under
    // publication locks and delivery never waits for the receiver.
    table
        .update("one", |value| Ok(json!(value.as_u64().unwrap() + 1)))
        .await
        .unwrap();
    assert_eq!(table.get("one").unwrap().unwrap().as_ref(), &json!(262));
    fixture.close().await;
}

#[tokio::test]
async fn global_null_and_multi_table_batch_are_visible_together() {
    let fixture = Fixture::new().await;
    let schema = Schema::default()
        .table::<u64>("rows")
        .unwrap()
        .table::<String>("labels")
        .unwrap()
        .global::<Option<u64>>()
        .unwrap();
    let domain = fixture.facility.open(spec(), schema).await.unwrap();
    let rows = domain.table::<u64>("rows").unwrap();
    let labels = domain.table::<String>("labels").unwrap();
    let global = domain.global::<Option<u64>>().unwrap();
    assert!(global.get().unwrap().is_none());
    let (_, mut events) = domain.snapshot_and_subscribe().unwrap();
    domain
        .batch(vec![
            rows.put_change("../中", 7).unwrap(),
            labels.put_change("", "label".into()).unwrap(),
            global.change(None).unwrap(),
        ])
        .await
        .unwrap();
    let revision = events.try_recv().unwrap().revision;
    assert_eq!(events.try_recv().unwrap().revision, revision);
    assert_eq!(events.try_recv().unwrap().revision, revision);
    assert_eq!(*rows.get("../中").unwrap().unwrap(), 7);
    assert_eq!(labels.get("").unwrap().unwrap().as_str(), "label");
    assert_eq!(*global.get().unwrap().unwrap(), None);
    let published = global.set(None).await.unwrap();
    assert!(Arc::ptr_eq(&published, &global.get().unwrap().unwrap()));

    domain.close().await;
    let reopened = fixture
        .facility
        .open(
            spec(),
            Schema::default()
                .table::<u64>("rows")
                .unwrap()
                .table::<String>("labels")
                .unwrap()
                .global::<Option<u64>>()
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        *reopened
            .global::<Option<u64>>()
            .unwrap()
            .get()
            .unwrap()
            .unwrap(),
        None
    );
    fixture.close().await;
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Entry {
    value: u64,
    #[serde(default)]
    extra: Option<String>,
}

#[tokio::test]
async fn compatible_reads_never_restamp_and_current_units_accept_untouched_older_payloads() {
    let fixture = Fixture::new().await;
    fixture
        .backend
        .seed("domain", 1, address("old"), &json!({"value":1}));
    let mut upgraded = spec();
    upgraded.version = 2;
    upgraded.compatible_versions.insert(1);
    let schema = Schema::default().table::<Entry>("rows").unwrap();
    let domain = fixture
        .facility
        .open(upgraded.clone(), schema.clone())
        .await
        .unwrap();
    assert_eq!(
        fixture.backend.state.units.lock().unwrap()["domain"].version,
        Some(1)
    );
    let table = domain.table::<Entry>("rows").unwrap();
    assert_eq!(table.get("old").unwrap().unwrap().extra, None);
    table
        .put(
            "new",
            Entry {
                value: 2,
                extra: Some("new".into()),
            },
        )
        .await
        .unwrap();
    assert_eq!(
        fixture.backend.state.units.lock().unwrap()["domain"].version,
        Some(2)
    );
    assert_eq!(
        fixture.backend.values("domain")["rows/old"],
        json!({"value":1})
    );
    domain.close().await;
    upgraded.compatible_versions.clear();
    let reopened = fixture.facility.open(upgraded, schema).await.unwrap();
    assert_eq!(
        reopened
            .table::<Entry>("rows")
            .unwrap()
            .get("old")
            .unwrap()
            .unwrap()
            .value,
        1
    );
    fixture.close().await;
}

#[tokio::test]
async fn unsupported_atomic_batches_and_recovery_policy_fail_before_writing() {
    let fixture = Fixture::new().await;
    let recovery_schema = Schema::default()
        .table_with_policy::<Value>("rows", RecordPolicy::BackupAndSkip)
        .unwrap();
    assert!(matches!(
        fixture.facility.open(spec(), recovery_schema).await,
        Err(StorageError::Unsupported(_))
    ));
    assert_eq!(fixture.backend.state.loads.load(Ordering::SeqCst), 0);
    fixture.backend.state.atomic.store(false, Ordering::SeqCst);
    let mut specification = spec();
    specification.layout = Layout::PerRecord;
    let domain = fixture
        .facility
        .open(specification, schema())
        .await
        .unwrap();
    let table = domain.table::<Value>("rows").unwrap();
    table.put("one", json!(1)).await.unwrap();
    assert!(matches!(
        domain
            .batch(vec![
                table.put_change("two", json!(2)).unwrap(),
                table.put_change("three", json!(3)).unwrap()
            ])
            .await,
        Err(StorageError::Unsupported(_))
    ));
    assert_eq!(fixture.backend.values("domain").len(), 1);
    assert_eq!(table.delete_many(["one", "missing"]).await.unwrap(), 1);
    fixture.close().await;
}

#[tokio::test]
async fn unknown_outcome_fences_other_domains_on_the_same_backend_generation() {
    let fixture = Fixture::new().await;
    let first = fixture.open().await;
    let mut other = spec();
    other.id = "other".into();
    let second = fixture.facility.open(other, schema()).await.unwrap();
    fixture
        .backend
        .state
        .unknown_after_write
        .store(true, Ordering::SeqCst);
    assert!(matches!(
        first
            .table::<Value>("rows")
            .unwrap()
            .put("one", Value::Null)
            .await,
        Err(StorageError::OutcomeUnknown(_))
    ));
    assert!(matches!(
        second.snapshot(),
        Err(StorageError::RecoveryRequired)
    ));
    fixture.close().await;
}

static DECODES: AtomicUsize = AtomicUsize::new(0);
#[derive(Debug, Serialize)]
struct Counted {
    value: u64,
}
impl<'de> Deserialize<'de> for Counted {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Wire {
            value: u64,
        }
        DECODES.fetch_add(1, Ordering::SeqCst);
        let wire = Wire::deserialize(deserializer)?;
        Ok(Self { value: wire.value })
    }
}
#[tokio::test]
async fn business_codecs_decode_at_load_only_and_reject_invalid_durable_records() {
    let fixture = Fixture::new().await;
    fixture
        .backend
        .seed("domain", 1, address("one"), &json!({"value":1}));
    DECODES.store(0, Ordering::SeqCst);
    let schema = Schema::default().table::<Counted>("rows").unwrap();
    let domain = fixture.facility.open(spec(), schema.clone()).await.unwrap();
    let table = domain.table::<Counted>("rows").unwrap();
    assert_eq!(DECODES.load(Ordering::SeqCst), 1);
    table.get("one").unwrap();
    table.snapshot().unwrap();
    table.put("two", Counted { value: 2 }).await.unwrap();
    table
        .update("one", |value| {
            Ok(Counted {
                value: value.value + 1,
            })
        })
        .await
        .unwrap();
    assert_eq!(DECODES.load(Ordering::SeqCst), 1);
    domain.close().await;
    fixture
        .backend
        .seed("domain", 1, address("bad"), &json!({"value":"wrong"}));
    assert!(matches!(
        fixture.facility.open(spec(), schema).await,
        Err(StorageError::Corrupt(_))
    ));
    assert!(fixture.backend.values("domain").contains_key("rows/bad"));
    fixture.close().await;
}

#[tokio::test]
async fn domain_byte_admission_precedes_business_deserialization() {
    static DECODES: AtomicUsize = AtomicUsize::new(0);
    #[derive(Serialize)]
    struct Counted(String);
    impl<'de> Deserialize<'de> for Counted {
        fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
            DECODES.fetch_add(1, Ordering::SeqCst);
            String::deserialize(d).map(Self)
        }
    }
    let fixture = Fixture::new().await;
    fixture
        .backend
        .seed("domain", 1, address("one"), &json!("x".repeat(128)));
    assert!(matches!(
        fixture
            .facility
            .open(
                DomainSpec {
                    maximum_bytes: 64,
                    ..spec()
                },
                Schema::default().table::<Counted>("rows").unwrap()
            )
            .await,
        Err(StorageError::Corrupt(_))
    ));
    assert_eq!(DECODES.load(Ordering::SeqCst), 0);
    fixture.close().await;
}
