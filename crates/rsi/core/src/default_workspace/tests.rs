use super::*;
use rsi_workspace_protocol::{
    ExecutionLocation, WorkspaceCursor, WorkspaceId, WorkspaceName, WorkspaceOrderSeed,
    WorkspacePage, WorkspaceStatus,
};
use std::{path::Path, sync::atomic::AtomicUsize};

#[derive(Debug)]
struct FaultyRead {
    inner: Arc<dyn WorkspaceRegistry>,
    fail_at: usize,
    reads: AtomicUsize,
}
#[async_trait]
impl WorkspaceRegistry for FaultyRead {
    async fn get(&self, id: &WorkspaceId) -> rsi_workspace_protocol::Result<WorkspaceRecord> {
        self.inner.get(id).await
    }
    async fn list(
        &self,
        after: Option<WorkspaceCursor>,
        limit: usize,
    ) -> rsi_workspace_protocol::Result<WorkspacePage> {
        if self.reads.fetch_add(1, Ordering::SeqCst) + 1 == self.fail_at {
            return Err(WorkspaceError::Storage("injected read failure".into()));
        }
        self.inner.list(after, limit).await
    }
    async fn order_seed(&self) -> rsi_workspace_protocol::Result<WorkspaceOrderSeed> {
        self.inner.order_seed().await
    }
    async fn register_at(
        &self,
        location: &ExecutionLocation,
        path: &Path,
    ) -> rsi_workspace_protocol::Result<WorkspaceRecord> {
        self.inner.register_at(location, path).await
    }
    async fn status(&self, id: &WorkspaceId) -> rsi_workspace_protocol::Result<WorkspaceStatus> {
        self.inner.status(id).await
    }
    async fn rename(
        &self,
        id: &WorkspaceId,
        name: WorkspaceName,
    ) -> rsi_workspace_protocol::Result<WorkspaceRecord> {
        self.inner.rename(id, name).await
    }
    async fn delete_registration(&self, id: &WorkspaceId) -> rsi_workspace_protocol::Result<bool> {
        self.inner.delete_registration(id).await
    }
}

#[derive(Debug)]
struct FaultyReceipt {
    inner: Arc<dyn Domain>,
    fail: AtomicBool,
}
#[async_trait]
impl Domain for FaultyReceipt {
    fn spec(&self) -> &DomainSpec {
        self.inner.spec()
    }
    fn ensure_available(&self) -> Result<(), rsi_storage::StorageError> {
        self.inner.ensure_available()
    }
    async fn snapshot(
        &self,
    ) -> Result<std::collections::BTreeMap<String, serde_json::Value>, rsi_storage::StorageError>
    {
        self.inner.snapshot().await
    }
    async fn put(
        &self,
        key: &str,
        value: serde_json::Value,
    ) -> Result<(), rsi_storage::StorageError> {
        if self.fail.swap(false, Ordering::SeqCst) {
            return Err(rsi_storage::StorageError::Io(
                "injected receipt failure".into(),
            ));
        }
        self.inner.put(key, value).await
    }
    async fn delete(&self, key: &str) -> Result<bool, rsi_storage::StorageError> {
        self.inner.delete(key).await
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[expect(
    clippy::too_many_lines,
    reason = "one read and receipt failure recovery lifecycle"
)]
async fn registry_read_failure_is_known_and_retry_does_not_duplicate_initialization() {
    for fail_at in [1, 2, 3] {
        let temp = tempfile::tempdir().unwrap();
        let root = rsi_files_native_fs::resolve_absolute_root_alias(temp.path(), false).unwrap();
        let profile = root.join("profile.toml");
        std::fs::write(&profile, "format = 1\nsteps = []\n").unwrap();
        let host = crate::StandardComposition::new(
            rsi_host::HostPaths::new(root.join("config"), root.join("state"), root.join("cache"))
                .unwrap(),
            std::collections::BTreeMap::new(),
            None,
            crate::ApplicationCatalogMetadata::default(),
        )
        .build()
        .unwrap()
        .start_file(&profile)
        .await
        .unwrap();
        let mut activation = host.subscribe_profile();
        tokio::time::timeout(
            std::time::Duration::from_secs(30),
            activation.wait_for(|_| {
                host.lookup_local::<WorkspaceRegistryContract>().is_some()
                    && host.lookup_local::<WorkspaceIngressContract>().is_some()
                    && host.lookup_local::<SessionStoreContract>().is_some()
                    && host.lookup_local::<ConfigurationAccessContract>().is_some()
                    && host.lookup_local::<DomainFacilityContract>().is_some()
            }),
        )
        .await
        .expect("default workspace dependencies did not activate")
        .expect("Host profile ended before activation");
        let registry = host.lookup_local::<WorkspaceRegistryContract>().unwrap();
        let domain = host
            .lookup_local::<DomainFacilityContract>()
            .unwrap()
            .open(DomainSpec {
                id: "test.default-workspace".into(),
                backend: "base".into(),
                version: 1,
                maximum_records: 1,
                maximum_bytes: 128 * 1024,
            })
            .await
            .unwrap();
        std::fs::create_dir(root.join("Documents")).unwrap();
        let mut owner = Arc::new(Owner {
            documents: Some(root.join("Documents")),
            domain: Arc::new(FaultyReceipt {
                inner: domain.clone(),
                fail: AtomicBool::new(fail_at == 3),
            }),
            workspace: Arc::new(FaultyRead {
                inner: registry.clone(),
                fail_at: if fail_at == 3 { usize::MAX } else { fail_at },
                reads: AtomicUsize::new(0),
            }),
            ingress: host.lookup_local::<WorkspaceIngressContract>().unwrap(),
            store: host.lookup_local::<SessionStoreContract>().unwrap(),
            access: host.lookup_local::<ConfigurationAccessContract>().unwrap(),
            receipt: Arc::new(Mutex::new(None)),
            closed: AtomicBool::new(false),
            execution: rsi_meta::Execution::native(tokio::runtime::Handle::current()),
        });
        let failure = owner
            .clone()
            .initialize(CallOrigin::Local)
            .await
            .unwrap_err();
        if fail_at == 3 {
            assert_eq!(failure, ApiError::OutcomeUnknown);
            assert_eq!(registry.list(None, 10).await.unwrap().records.len(), 1);
            // Model process restart before receipt persistence; Windows ancestors may
            // be captured with a different case than the registry's physical path.
            #[cfg(windows)]
            {
                Arc::get_mut(&mut owner).unwrap().documents = Some(root.join("dOCUMENTS"));
            }
        } else {
            assert!(
                matches!(failure, ApiError::Backend(ref text) if text.contains("injected read failure")),
                "{failure:?}"
            );
            assert!(registry.list(None, 10).await.unwrap().records.is_empty());
        }
        assert!(domain.snapshot().await.unwrap().is_empty());
        let one = owner
            .clone()
            .initialize(CallOrigin::Local)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        Arc::get_mut(&mut owner).unwrap().receipt = Arc::new(Mutex::new(None));
        let two = owner
            .initialize(CallOrigin::Local)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert_eq!(one, two);
        assert_eq!(registry.list(None, 10).await.unwrap().records.len(), 1);
        assert_eq!(domain.snapshot().await.unwrap().len(), 1);
        assert!(host.shutdown().await.is_clean());
    }
}
