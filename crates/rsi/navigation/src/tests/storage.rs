use super::*;
use rsi_meta::{ResolvedFactory, Runtime, UpdateMode};
use rsi_storage_testkit::{BackendFactory, MemoryBackend};
use std::sync::atomic::{AtomicUsize, Ordering};

pub(super) struct FixtureOwner {
    pub owner: Arc<Navigation>,
    pub _storage: Arc<TestDomain>,
}
impl std::ops::Deref for FixtureOwner {
    type Target = Arc<Navigation>;
    fn deref(&self) -> &Self::Target {
        &self.owner
    }
}
impl std::ops::DerefMut for FixtureOwner {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.owner
    }
}

pub(super) struct TestDomain {
    pub domain: Domain,
    pub backend: Arc<MemoryBackend>,
    _runtime: Runtime,
    consumed: AtomicUsize,
}
impl TestDomain {
    pub async fn new(spec: DomainSpec) -> Arc<Self> {
        Self::open(spec, Arc::new(MemoryBackend::default()))
            .await
            .unwrap()
    }
    pub async fn open(
        spec: DomainSpec,
        backend: Arc<MemoryBackend>,
    ) -> std::result::Result<Arc<Self>, rsi_storage::StorageError> {
        let runtime = Runtime::default();
        for (name, factory) in [
            (
                "storage",
                Arc::new(rsi_storage::StorageFactory) as Arc<dyn PluginFactory>,
            ),
            (
                "backend",
                Arc::new(BackendFactory::new(&spec.backend, backend.clone())),
            ),
            ("domains", Arc::new(rsi_storage_domain::DomainFactory)),
        ] {
            let fiber = runtime
                .root()
                .apply(
                    ResolvedFactory::linked(name, "test", UpdateMode::Replayable, factory),
                    ConfigValue::Null,
                )
                .await
                .unwrap();
            assert_eq!(fiber.snapshot().state, rsi_meta::FiberState::Active);
        }
        let domain = runtime
            .root()
            .lookup_local::<DomainFacilityContract>()
            .unwrap()
            .open(spec, domain_schema().unwrap())
            .await?;
        Ok(Arc::new(Self {
            domain,
            backend,
            _runtime: runtime,
            consumed: AtomicUsize::new(0),
        }))
    }
    pub fn writes(&self) -> usize {
        self.backend.state.writes.load(Ordering::SeqCst)
    }
    pub fn document(&self) -> Option<serde_json::Value> {
        (self.writes() != self.consumed.load(Ordering::SeqCst))
            .then(|| serde_json::to_value(load_document(&self.domain).unwrap()).unwrap())
    }
    pub fn take_document(&self) -> Option<serde_json::Value> {
        let document = self.document();
        self.consumed.store(self.writes(), Ordering::SeqCst);
        document
    }
    pub async fn seed_document(&self, document: &Document) {
        let mut changes = vec![
            self.domain
                .global::<Revision>()
                .unwrap()
                .change(Revision {
                    revision: document.revision,
                })
                .unwrap(),
        ];
        let records = self.domain.table::<StoredMetadata>("metadata").unwrap();
        for (id, row) in &document.records {
            changes.push(records.put_change(id.as_str(), row.clone()).unwrap());
        }
        let orders = self.domain.table::<WorkspaceAccount>("orders").unwrap();
        for (id, order) in &document.workspace_orders {
            changes.push(orders.put_change(id.as_str(), order.clone()).unwrap());
        }
        self.domain.batch(changes).await.unwrap();
        self.backend.state.writes.store(0, Ordering::SeqCst);
        self.consumed.store(0, Ordering::SeqCst);
        self.backend.state.last_write.lock().unwrap().clear();
    }
}

#[tokio::test]
async fn navigation_requires_atomic_batches_before_publication() {
    let backend = Arc::new(MemoryBackend::default());
    backend.state.atomic.store(false, Ordering::SeqCst);
    let mut spec = domain_spec("fixture".into());
    spec.layout = rsi_storage::Layout::PerRecord;
    let fixture = TestDomain::open(spec, backend).await.unwrap();
    assert!(
        matches!(load_document(&fixture.domain), Err(ApiError::Invalid(message)) if message.contains("atomic"))
    );
    assert_eq!(fixture.writes(), 0);
    fixture.domain.close().await;
}

#[tokio::test]
async fn one_metadata_edit_commits_two_records_and_preserves_old_snapshots() {
    let mut owner = owner().await;
    let fixture = TestDomain::new(owner.domain.spec().clone()).await;
    Arc::get_mut(&mut owner).unwrap().domain = fixture.domain.clone();
    let changed = SessionId::new("row-001").unwrap();
    let untouched = SessionId::new("row-002").unwrap();
    let mut document = Document::default();
    for id in [&changed, &untouched] {
        document
            .edit(
                id,
                &SessionMetadata {
                    title: Some(id.to_string()),
                    ..Default::default()
                },
            )
            .unwrap();
    }
    fixture.seed_document(&document).await;
    owner.state.lock().unwrap().document = Arc::new(document);
    let before = owner.document();
    let old_row = fixture
        .domain
        .table::<StoredMetadata>("metadata")
        .unwrap()
        .get(untouched.as_str())
        .unwrap()
        .unwrap();
    fixture
        .backend
        .state
        .pause_write
        .store(true, Ordering::SeqCst);
    let pending = owner
        .replace(
            CallOrigin::Local,
            changed.clone(),
            &before.revision.to_string(),
            SessionMetadata {
                title: Some("updated".into()),
                ..Default::default()
            },
        )
        .unwrap();
    fixture.backend.state.entered.notified().await;
    assert!(Arc::ptr_eq(&owner.document(), &before));
    fixture.backend.state.release.notify_one();
    pending.await.unwrap();
    assert_eq!(
        before.records[&changed].title.as_deref(),
        Some(changed.as_str())
    );
    assert_eq!(
        owner.document().records[&changed].title.as_deref(),
        Some("updated")
    );
    let new_row = fixture
        .domain
        .table::<StoredMetadata>("metadata")
        .unwrap()
        .get(untouched.as_str())
        .unwrap()
        .unwrap();
    assert!(Arc::ptr_eq(&old_row, &new_row));
    {
        let writes = fixture.backend.state.last_write.lock().unwrap();
        assert_eq!(writes.len(), 2);
        assert!(writes.iter().any(|change| change.address.is_global()));
        assert!(
            writes
                .iter()
                .any(|change| change.address.table() == "metadata"
                    && change.address.key() == changed.as_str())
        );
    }
    owner.close().await;
}
