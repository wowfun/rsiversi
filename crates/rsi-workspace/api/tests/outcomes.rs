use async_trait::async_trait;
use rsi_api::ApiRegistry;
use rsi_api_protocol::{
    ApiClient, ApiDispatch, ApiError, ApiOutput, ByteBudget, CallOrigin, ConnectionDescription,
    EndpointId, HostEpoch, OperationClass, OperationSpec, RetainedBytes,
};
use rsi_meta::Execution;
use rsi_workspace_api::{WorkspaceApi, WorkspaceClient};
use rsi_workspace_protocol::*;
use std::{
    path::Path,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
};

#[derive(Debug)]
struct Provider {
    writes: AtomicUsize,
    bad_record: bool,
}
#[async_trait]
impl WorkspaceRegistry for Provider {
    async fn get(&self, id: &WorkspaceId) -> Result<WorkspaceRecord> {
        Err(WorkspaceError::Unknown(id.clone()))
    }
    async fn list(&self, _: Option<WorkspaceCursor>, _: usize) -> Result<WorkspacePage> {
        Ok(WorkspacePage {
            records: vec![],
            next: None,
        })
    }
    async fn status(&self, id: &WorkspaceId) -> Result<WorkspaceStatus> {
        Err(WorkspaceError::Unknown(id.clone()))
    }
    async fn get_or_create(&self, path: &Path) -> Result<WorkspaceRecord> {
        self.writes.fetch_add(1, Ordering::SeqCst);
        if self.bad_record {
            Ok(WorkspaceRecord {
                id: WorkspaceId::parse("0".repeat(64)).unwrap(),
                path: path.into(),
            })
        } else {
            Err(WorkspaceError::Storage(
                "commit result was lost after write".into(),
            ))
        }
    }
    async fn delete_registration(&self, _: &WorkspaceId) -> Result<bool> {
        self.writes.fetch_add(1, Ordering::SeqCst);
        Err(WorkspaceError::Storage(
            "delete result was lost after write".into(),
        ))
    }
}
#[derive(Debug)]
struct Client {
    registry: Arc<ApiRegistry>,
    description: ConnectionDescription,
    operations: Vec<OperationSpec>,
}
#[async_trait]
impl ApiClient for Client {
    fn description(&self) -> &ConnectionDescription {
        &self.description
    }
    fn operations(&self) -> &[OperationSpec] {
        &self.operations
    }
    fn input_budget(&self, _: OperationClass) -> ByteBudget {
        ByteBudget::default()
    }
    async fn call(
        &self,
        operation: &OperationSpec,
        input: RetainedBytes,
    ) -> rsi_api_protocol::Result<ApiOutput> {
        self.registry
            .admit(&operation.id, CallOrigin::Local)?
            .invoke(input)
            .await
    }
}

#[tokio::test]
async fn commit_result_loss_and_invalid_registration_reply_stay_unknown_without_replay() {
    for bad_record in [false, true] {
        let registry = Arc::new(ApiRegistry::new(Execution::native(
            tokio::runtime::Handle::current(),
        )));
        let provider = Arc::new(Provider {
            writes: AtomicUsize::new(0),
            bad_record,
        });
        let endpoint = WorkspaceApi::register(registry.as_ref(), provider.clone()).unwrap();
        let connection = Arc::new(Client {
            operations: registry.operations(),
            registry: registry.clone(),
            description: ConnectionDescription {
                wire_version: 1,
                endpoint_id: EndpointId::from_bytes([1; 16]),
                host_epoch: HostEpoch::from_bytes([2; 16]),
            },
        });
        let client = WorkspaceClient::new(connection).unwrap();
        assert!(matches!(
            client.get_or_create(Path::new("relative")).await,
            Err(WorkspaceError::InvalidInput(_))
        ));
        assert_eq!(provider.writes.load(Ordering::SeqCst), 0);
        let path = std::env::current_dir().unwrap();
        assert!(matches!(
            client.get_or_create(&path).await,
            Err(WorkspaceError::Api(ApiError::OutcomeUnknown))
        ));
        assert_eq!(provider.writes.load(Ordering::SeqCst), 1);
        let id = WorkspaceId::parse("0".repeat(64)).unwrap();
        assert!(matches!(
            client.delete_registration(&id).await,
            Err(WorkspaceError::Api(ApiError::OutcomeUnknown))
        ));
        assert_eq!(provider.writes.load(Ordering::SeqCst), 2);
        endpoint.close().await;
        registry.close().await;
    }
}
