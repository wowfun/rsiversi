use super::*;
use async_trait::async_trait;
use rsi_acp_protocol::{
    observation::{Capabilities, ConversationId, Page, Record, RecordKind, Snapshot, Status},
    service::{self, ExternalConversations, Resident, Setup, View},
};
use rsi_api::ApiRegistry;
use rsi_api_protocol::{
    ApiClient, ApiDispatch as _, ApiError, ApiOutput, ByteBudget, CallOrigin,
    ConnectionDescription, EndpointId, HostEpoch, OperationClass, OperationSpec, RetainedBytes,
};
use serde_json::json;
use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicU64, Ordering},
};

#[derive(Debug, Default)]
struct Source {
    sends: AtomicU64,
    generation: AtomicU64,
}
fn snapshot() -> Snapshot {
    Snapshot {
        id: ConversationId::new("external").unwrap(),
        endpoint: "fixture".into(),
        cwd: "/host/workspace".into(),
        remote: Some("remote".into()),
        generation: 9_007_199_254_740_993,
        epoch: 1,
        status: Status::Ready,
        completion: None,
        capabilities: Capabilities::default(),
    }
}
#[async_trait]
impl ExternalConversations for Source {
    async fn endpoints(&self) -> service::Result<Vec<service::Endpoint>> {
        Ok(vec![service::Endpoint {
            id: "fixture".into(),
            enabled: true,
        }])
    }
    async fn residents(&self) -> service::Result<Vec<Resident>> {
        Ok(vec![])
    }
    async fn list(&self, _: Option<ConversationId>) -> service::Result<Vec<Snapshot>> {
        Ok(vec![snapshot()])
    }
    async fn view(&self, _: &ConversationId) -> service::Result<View> {
        Ok(View {
            snapshot: snapshot(),
            connected: true,
            permissions: vec![],
        })
    }
    async fn start(&self, _: ConversationId, _: &str) -> service::Result<Snapshot> {
        Ok(snapshot())
    }
    async fn reconnect(&self, _: &ConversationId, _: Setup) -> service::Result<Snapshot> {
        Ok(snapshot())
    }
    async fn submit(&self, _: &ConversationId, _: &str) -> service::Result<Snapshot> {
        self.sends.fetch_add(1, Ordering::SeqCst);
        Ok(snapshot())
    }
    async fn cancel(&self, _: &ConversationId) -> service::Result<Snapshot> {
        Ok(snapshot())
    }
    async fn close(&self, _: &ConversationId) -> service::Result<Snapshot> {
        Ok(snapshot())
    }
    async fn answer(
        &self,
        _: &ConversationId,
        generation: u64,
        _: &str,
        _: &str,
    ) -> service::Result<()> {
        self.generation.store(generation, Ordering::SeqCst);
        Ok(())
    }
    async fn page(&self, _: &ConversationId, epoch: u64, after: u64) -> service::Result<Page> {
        Ok(Page {
            records: vec![Record {
                sequence: after + 1,
                epoch,
                kind: RecordKind::User,
                bytes: 7,
                value: Some(json!("hello")),
            }],
            has_more: false,
        })
    }
    async fn window(
        &self,
        _: &ConversationId,
        _: u64,
        _: u64,
        _: usize,
    ) -> service::Result<Vec<u8>> {
        Ok(vec![0, 255, 128, 10])
    }
}
#[derive(Debug)]
struct Local {
    registry: Arc<ApiRegistry>,
    description: ConnectionDescription,
    operations: Vec<OperationSpec>,
    budget: ByteBudget,
    lose: AtomicBool,
    tamper: AtomicBool,
}
#[async_trait]
impl ApiClient for Local {
    fn description(&self) -> &ConnectionDescription {
        &self.description
    }
    fn operations(&self) -> &[OperationSpec] {
        &self.operations
    }
    fn input_budget(&self, _: OperationClass) -> ByteBudget {
        self.budget.clone()
    }
    async fn call(
        &self,
        operation: &OperationSpec,
        input: RetainedBytes,
    ) -> rsi_api_protocol::Result<ApiOutput> {
        let mut output = self
            .registry
            .admit(&operation.id, CallOrigin::Local)?
            .invoke(input)
            .await?;
        if self.lose.load(Ordering::Acquire) {
            return Err(ApiError::OutcomeUnknown);
        }
        if self.tamper.load(Ordering::Acquire)
            && let ApiOutput::Reply(reply) = &mut output
        {
            let mut value: serde_json::Value =
                serde_json::from_slice(reply.json.as_bytes()).unwrap();
            value["source"]["epoch"] = json!("2");
            reply.json = self.budget.encode(&value, 4 * 1024 * 1024).unwrap();
        }
        Ok(output)
    }
}
fn fixture() -> (Arc<Source>, Endpoint, Arc<Local>, Client) {
    let source = Arc::new(Source::default());
    let registry = Arc::new(ApiRegistry::new(rsi_meta::Execution::native(
        tokio::runtime::Handle::current(),
    )));
    let owner: Arc<dyn ExternalConversations> = source.clone();
    let endpoint = Endpoint::register(registry.as_ref(), &owner).unwrap();
    let local = Arc::new(Local {
        operations: registry.operations(),
        registry,
        description: ConnectionDescription {
            wire_version: 1,
            endpoint_id: EndpointId::from_bytes([2; 16]),
            host_epoch: HostEpoch::from_bytes([3; 16]),
        },
        budget: ByteBudget::new(8 * 1024 * 1024).unwrap(),
        lose: AtomicBool::new(false),
        tamper: AtomicBool::new(false),
    });
    let client = Client::new(local.clone()).unwrap();
    (source, endpoint, local, client)
}
#[tokio::test]
async fn exact_identity_and_large_generation_survive_api_while_unknown_submit_is_not_retried() {
    let (source, endpoint, local, client) = fixture();
    let id = snapshot().id;
    assert_eq!(
        client
            .start(id.clone(), "fixture")
            .await
            .unwrap()
            .generation,
        9_007_199_254_740_993
    );
    client
        .answer(
            &id,
            9_007_199_254_740_993,
            "permission-exact",
            "always-exact",
        )
        .await
        .unwrap();
    assert_eq!(
        source.generation.load(Ordering::SeqCst),
        9_007_199_254_740_993
    );
    local.lose.store(true, Ordering::Release);
    assert_eq!(
        client.submit(&id, "once").await.unwrap_err(),
        service::Error::Unknown
    );
    assert_eq!(source.sends.load(Ordering::SeqCst), 1);
    endpoint.close().await;
}
#[tokio::test]
async fn exact_history_binding_and_raw_window_reject_retargeted_responses() {
    let (_, endpoint, local, client) = fixture();
    let id = snapshot().id;
    assert_eq!(
        client.page(&id, 1, 3000).await.unwrap().records[0].sequence,
        3001
    );
    assert_eq!(
        client.window(&id, 1, 3001, 0).await.unwrap(),
        [0, 255, 128, 10]
    );
    local.tamper.store(true, Ordering::Release);
    assert_eq!(
        client.page(&id, 1, 3000).await.unwrap_err(),
        service::Error::Input
    );
    assert_eq!(
        client.window(&id, 1, 3001, 0).await.unwrap_err(),
        service::Error::Input
    );
    endpoint.close().await;
}
#[test]
fn page_admission_checks_complete_aggregate_and_sequence_order() {
    let record = Record {
        sequence: 1,
        epoch: 7,
        kind: RecordKind::User,
        bytes: 7,
        value: Some(json!("hello")),
    };
    assert!(
        validate::page(
            &Page {
                records: vec![record.clone(), record],
                has_more: false
            },
            7,
            0
        )
        .is_err()
    );
    let records = (1..=4)
        .map(|sequence| {
            let value = json!("x".repeat(65536));
            Record {
                sequence,
                epoch: 7,
                kind: RecordKind::User,
                bytes: 65538,
                value: Some(value),
            }
        })
        .collect();
    assert!(
        validate::page(
            &Page {
                records,
                has_more: false
            },
            7,
            0
        )
        .is_err()
    );
}
