use super::*;
use futures_util::StreamExt as _;
use rsi_api_protocol::{
    ApiClient, ApiError, ApiMessage, ApiOutput, ByteBudget, ConnectionDescription, EndpointId,
    HostEpoch, OperationClass, OperationSpec, RetainedBytes,
};
use std::{
    collections::{BTreeMap, VecDeque},
    sync::Mutex,
};
#[derive(Debug)]
struct Api {
    operations: Vec<OperationSpec>,
    description: ConnectionDescription,
    calls: Mutex<Vec<serde_json::Value>>,
    invoked: Arc<tokio::sync::Notify>,
    replies: Mutex<VecDeque<rsi_api_protocol::Result<ApiOutput>>>,
}
#[async_trait]
impl ApiClient for Api {
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
        _: &OperationSpec,
        input: RetainedBytes,
    ) -> rsi_api_protocol::Result<ApiOutput> {
        if serde_json::from_slice::<serde_json::Value>(input.as_bytes())
            .unwrap()
            .get("ticket")
            .is_some()
        {
            self.invoked.notify_one();
        }
        self.calls
            .lock()
            .unwrap()
            .push(serde_json::from_slice(input.as_bytes()).unwrap());
        self.replies.lock().unwrap().pop_front().unwrap()
    }
}
fn message(value: &impl Serialize) -> ApiMessage {
    ApiMessage {
        json: ByteBudget::default()
            .encode(value, rsi_ui_api::MAXIMUM_ITEM_BYTES)
            .unwrap(),
        binary: None,
    }
}
fn item(revision: u64) -> rsi_ui_api::Item {
    rsi_ui_api::Item {
        selection: 0,
        snapshot: rsi_ui::ModelSnapshot {
            presentation: rsi_ui::PresentationIdentity {
                reference: rsi_ui::UiReference {
                    application: "fixture".into(),
                    target: "actual".into(),
                    contribution: "language".into(),
                    name: "query".into(),
                },
                epoch: "generation".into(),
            },
            revision,
            model: rsi_ui::UiModel::standard(UiView {
                title: "Language".into(),
                elements: vec![
                    UiElement::Input {
                        name: "path".into(),
                        label: "File".into(),
                        value: "src/main.rs".into(),
                        multiline: false,
                    },
                    UiElement::Button {
                        action: "query".into(),
                        label: "Find definition".into(),
                        value: serde_json::json!({"operation":"definition"}),
                    },
                ],
            })
            .unwrap(),
        },
        ticket: Some(format!("ticket-{revision}")),
    }
}
fn fixture(
    replies: Vec<rsi_api_protocol::Result<ApiOutput>>,
    invoked: Arc<tokio::sync::Notify>,
) -> (Reader, Arc<Api>) {
    let api = Arc::new(Api {
        operations: rsi_ui_api::operations().to_vec(),
        description: ConnectionDescription {
            wire_version: 1,
            endpoint_id: EndpointId::from_bytes([1; 16]),
            host_epoch: HostEpoch::from_bytes([2; 16]),
        },
        calls: Mutex::new(vec![]),
        invoked,
        replies: Mutex::new(replies.into()),
    });
    (
        Reader {
            revision: "reader".into(),
            scope: ExportScope {
                kind: "session".into(),
                key: "actual-session".into(),
            },
            client: UiClient::new(api.clone()).unwrap(),
            state: tokio::sync::Mutex::new(State::default()),
            stop: CancellationToken::new(),
        },
        api,
    )
}
fn responses(invoked: Arc<tokio::sync::Notify>) -> Vec<rsi_api_protocol::Result<ApiOutput>> {
    vec![
        Ok(ApiOutput::Reply(message(
            &serde_json::json!({"entries":[{"bundle":"language","surface":"query","title":"Code intelligence"}],"next":null}),
        ))),
        Ok(ApiOutput::Stream(Box::pin(
            futures_util::stream::once(async { Ok(message(&item(1))) })
                .chain(futures_util::stream::once(async move {
                    invoked.notified().await;
                    Ok(message(&item(2)))
                }))
                .chain(futures_util::stream::pending()),
        ))),
    ]
}
async fn opened(reader: &Reader) -> (State, UiView) {
    let mut state = State::default();
    read(
        reader,
        &mut state,
        Operation::List { after: None },
        BTreeMap::new(),
    )
    .await
    .unwrap();
    let view = read(
        reader,
        &mut state,
        Operation::Open {
            bundle: "language".into(),
            surface: "query".into(),
        },
        BTreeMap::new(),
    )
    .await
    .unwrap();
    (state, view)
}
fn action(view: &UiView, label: &str) -> Operation {
    let value = view
        .elements
        .iter()
        .find_map(|element| match element {
            UiElement::Button {
                label: actual,
                value,
                ..
            } if actual == label => Some(value.clone()),
            _ => None,
        })
        .unwrap();
    serde_json::from_value::<Request>(value).unwrap().operation
}
#[tokio::test]
async fn captured_scope_exact_ticket_and_close_without_forwarding_fields() {
    let invoked = Arc::new(tokio::sync::Notify::new());
    let mut replies = responses(invoked.clone());
    replies.push(Ok(ApiOutput::Reply(message(&true))));
    let (reader, api) = fixture(replies, invoked);
    let (mut state, view) = opened(&reader).await;
    let refreshed = read(
        &reader,
        &mut state,
        action(&view, "Find definition"),
        [("path".into(), "src/lib.rs".into())].into(),
    )
    .await
    .unwrap();
    assert_eq!(
        state.remote.as_ref().unwrap().item.item.snapshot.revision,
        2
    );
    {
        let calls = api.calls.lock().unwrap();
        assert_eq!(calls[0]["scope"]["key"], "actual-session");
        assert_eq!(calls[1]["selections"][0]["scope"]["key"], "actual-session");
        assert_eq!(calls[2]["ticket"], "ticket-1");
        assert_eq!(calls[2]["input"]["fields"]["path"], "src/lib.rs");
        assert_eq!(calls[2]["action"]["action"], "query");
    }
    read(
        &reader,
        &mut state,
        action(&refreshed, "Close service view"),
        [("path".into(), "discarded".into())].into(),
    )
    .await
    .unwrap();
    assert!(state.remote.is_none());
    assert_eq!(api.calls.lock().unwrap().len(), 3);
}
#[tokio::test]
async fn old_presentation_and_outcome_unknown_never_replay_ticket() {
    let invoked = Arc::new(tokio::sync::Notify::new());
    let mut replies = responses(invoked.clone());
    replies.push(Err(ApiError::OutcomeUnknown));
    let (reader, api) = fixture(replies, invoked);
    let (mut state, view) = opened(&reader).await;
    let Operation::Invoke {
        snapshot,
        action: invoke,
        value,
        ..
    } = action(&view, "Find definition")
    else {
        panic!()
    };
    assert!(matches!(
        read(
            &reader,
            &mut state,
            Operation::Invoke {
                application: "old".into(),
                snapshot,
                action: invoke,
                value
            },
            BTreeMap::new()
        )
        .await,
        Err(UiError::Retired)
    ));
    assert!(state.remote.is_some());
    assert_eq!(api.calls.lock().unwrap().len(), 2);
    assert!(
        read(
            &reader,
            &mut state,
            action(&view, "Find definition"),
            BTreeMap::new()
        )
        .await
        .is_err()
    );
    assert!(state.remote.is_none());
    assert!(matches!(
        read(
            &reader,
            &mut state,
            action(&view, "Find definition"),
            BTreeMap::new()
        )
        .await,
        Err(UiError::Retired)
    ));
    assert_eq!(api.calls.lock().unwrap().len(), 3);
}
#[tokio::test]
async fn unlisted_target_does_not_open_remote_observation() {
    let (reader, api) = fixture(vec![], Arc::new(tokio::sync::Notify::new()));
    assert!(
        read(
            &reader,
            &mut State::default(),
            Operation::Open {
                bundle: "foreign".into(),
                surface: "query".into()
            },
            BTreeMap::new()
        )
        .await
        .is_err()
    );
    assert!(api.calls.lock().unwrap().is_empty());
}

#[tokio::test]
async fn queued_model_change_is_shown_before_an_old_form_can_invoke() {
    let invoked = Arc::new(tokio::sync::Notify::new());
    let mut replies = responses(invoked.clone());
    replies[1] = Ok(ApiOutput::Stream(Box::pin(
        futures_util::stream::iter(vec![Ok(message(&item(1))), Ok(message(&item(2)))])
            .chain(futures_util::stream::pending()),
    )));
    replies.push(Ok(ApiOutput::Reply(message(&true))));
    let (reader, api) = fixture(replies, invoked);
    let (mut state, view) = opened(&reader).await;
    let changed = tokio::time::timeout(
        Duration::from_millis(100),
        read(
            &reader,
            &mut state,
            action(&view, "Find definition"),
            [("path".into(), "old-input.rs".into())].into(),
        ),
    )
    .await
    .expect("cached view must be reconciled without invoking")
    .unwrap();
    assert!(changed.elements.iter().any(
        |element| matches!(element,UiElement::Text{text} if text.contains("Service view changed"))
    ));
    assert_eq!(api.calls.lock().unwrap().len(), 2);
    assert_eq!(state.remote.unwrap().item.item.snapshot.revision, 2);
}
