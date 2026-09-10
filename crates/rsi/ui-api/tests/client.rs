use async_trait::async_trait;
use rsi_api_protocol::{
    ApiClient, ApiError, ApiMessage, ApiOutput, ByteBudget, ConnectionDescription, EndpointId,
    HostEpoch, OperationClass, OperationSpec, Result, RetainedBytes,
};
use rsi_ui::{
    ActionInput, ModelSnapshot, PresentationAction, PresentationIdentity, UiModel, UiReference,
    UiView,
};
use rsi_ui_api::{CatalogRequest, ExportScope, Invoke, Item, Observe, Selection, UiClient};
use std::{
    collections::VecDeque,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
};

#[derive(Debug)]
struct Remote {
    description: ConnectionDescription,
    operations: Vec<OperationSpec>,
    budget: ByteBudget,
    replies: Mutex<VecDeque<Result<ApiOutput>>>,
    calls: AtomicUsize,
}
#[async_trait]
impl ApiClient for Remote {
    fn description(&self) -> &ConnectionDescription {
        &self.description
    }
    fn operations(&self) -> &[OperationSpec] {
        &self.operations
    }
    fn input_budget(&self, _: OperationClass) -> ByteBudget {
        self.budget.clone()
    }
    async fn call(&self, _: &OperationSpec, _: RetainedBytes) -> Result<ApiOutput> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.replies.lock().unwrap().pop_front().unwrap()
    }
}
fn remote(replies: Vec<Result<ApiOutput>>) -> Arc<Remote> {
    Arc::new(Remote {
        description: ConnectionDescription {
            wire_version: 1,
            endpoint_id: EndpointId::from_bytes([1; 16]),
            host_epoch: HostEpoch::from_bytes([2; 16]),
        },
        operations: rsi_ui_api::operations().to_vec(),
        budget: ByteBudget::default(),
        replies: Mutex::new(replies.into()),
        calls: AtomicUsize::new(0),
    })
}
fn item(revision: u64) -> Item {
    Item {
        selection: 0,
        snapshot: ModelSnapshot {
            presentation: PresentationIdentity {
                reference: UiReference {
                    application: "app".into(),
                    target: "target".into(),
                    contribution: "source".into(),
                    name: "panel".into(),
                },
                epoch: "epoch".into(),
            },
            revision,
            model: UiModel::standard(UiView::default()).unwrap(),
        },
        ticket: Some("ticket".into()),
    }
}
fn message(value: &impl serde::Serialize) -> ApiMessage {
    ApiMessage {
        json: ByteBudget::default()
            .encode(value, rsi_ui_api::MAXIMUM_ITEM_BYTES)
            .unwrap(),
        binary: None,
    }
}
fn request() -> Observe {
    Observe {
        application: "app".into(),
        selections: vec![Selection {
            scope: ExportScope {
                kind: "session".into(),
                key: "session".into(),
            },
            bundle: "source".into(),
            surface: "panel".into(),
        }],
    }
}

#[tokio::test]
async fn changed_identity_backwards_revision_and_foreign_selection_close_the_stream() {
    let mut wrong_target = item(2);
    wrong_target.snapshot.presentation.reference.target = "foreign".into();
    let mut wrong_selection = item(2);
    wrong_selection.selection = 1;
    for invalid in [wrong_target, wrong_selection, item(1)] {
        let stream = futures_util::stream::iter(vec![
            Ok(message(&item(2))),
            Ok(message(&invalid)),
            Ok(message(&item(3))),
        ]);
        let client = UiClient::new(remote(vec![Ok(ApiOutput::Stream(Box::pin(stream)))])).unwrap();
        let mut observation = client.observe(&request()).await.unwrap();
        assert_eq!(
            observation
                .next()
                .await
                .unwrap()
                .unwrap()
                .item
                .snapshot
                .revision,
            2
        );
        assert!(matches!(
            observation.next().await,
            Err(ApiError::Invalid(_))
        ));
        assert!(observation.next().await.unwrap().is_none());
    }
}

#[tokio::test]
async fn malformed_action_reply_is_unknown_and_never_replayed() {
    let api = remote(vec![Ok(ApiOutput::Reply(message(&false)))]);
    let client = UiClient::new(api.clone()).unwrap();
    let shown = item(1);
    let request = Invoke {
        application: "app".into(),
        action: PresentationAction {
            presentation: shown.snapshot.presentation,
            revision: 1,
            action: "run".into(),
        },
        ticket: "ticket".into(),
        input: ActionInput::default(),
    };
    assert!(matches!(
        client.invoke(&request).await,
        Err(ApiError::OutcomeUnknown)
    ));
    assert_eq!(api.calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn binary_windows_and_catalog_order_are_validated_at_the_client() {
    let mut reply = message(&serde_json::json!({"bytes": 5}));
    reply.binary = Some(ByteBudget::default().copy(b"12345").unwrap());
    let client = UiClient::new(remote(vec![Ok(ApiOutput::Reply(reply)), Ok(ApiOutput::Reply(message(&serde_json::json!({"entries":[{"bundle":"b","surface":"s","title":"B"},{"bundle":"a","surface":"s","title":"A"}],"next":null}))))])).unwrap();
    let source = rsi_ui_api::Source {
        application: "app".into(),
        presentation: item(1).snapshot.presentation,
        revision: 1,
        name: "raw".into(),
        offset: 0,
        maximum: 4,
    };
    assert!(matches!(
        client.source(&source).await,
        Err(ApiError::Invalid(_))
    ));
    let request = CatalogRequest {
        scope: ExportScope {
            kind: "session".into(),
            key: "session".into(),
        },
        after: None,
        maximum: 64,
    };
    assert!(matches!(
        client.catalog(&request).await,
        Err(ApiError::Invalid(_))
    ));
}
