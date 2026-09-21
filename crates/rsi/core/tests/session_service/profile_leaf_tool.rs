use super::*;
use axum::{Json, Router, body::Body, extract::State, response::Response, routing::post};
use std::collections::VecDeque;

#[derive(Default)]
struct Provider {
    calls: Mutex<VecDeque<Option<Value>>>,
    requests: Mutex<Vec<Value>>,
}
async fn respond(State(provider): State<Arc<Provider>>, Json(body): Json<Value>) -> Response {
    let number = {
        let mut requests = provider.requests.lock().unwrap();
        requests.push(body);
        requests.len()
    };
    let call = provider.calls.lock().unwrap().pop_front().flatten();
    let Some(call) = call else {
        return super::super::chat().await;
    };
    let call = json!({"choices":[{"delta":{"role":"assistant","tool_calls":[{"index":0,"id":format!("leaf-{number}"),"type":"function","function":{"name":"host_profile","arguments":call.to_string()}}]},"finish_reason":null}]});
    let finish = json!({"choices":[{"delta":{},"finish_reason":"tool_calls"}],"usage":{"prompt_tokens":2,"completion_tokens":1}});
    Response::builder()
        .header("content-type", "text/event-stream")
        .body(Body::from(format!(
            "data: {call}\n\ndata: {finish}\n\ndata: [DONE]\n\n"
        )))
        .unwrap()
}
impl Provider {
    fn queue(&self, call: Value) {
        self.calls.lock().unwrap().extend([Some(call), None]);
    }
    fn result(&self) -> String {
        self.requests.lock().unwrap().last().unwrap()["messages"]
            .as_array()
            .unwrap()
            .iter()
            .rev()
            .find(|message| message["role"] == "tool")
            .unwrap()["content"]
            .as_str()
            .unwrap()
            .to_owned()
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[expect(
    clippy::too_many_lines,
    reason = "One actual Session verifies caller authority, reviewed write and independent receipt"
)]
async fn profile_tool_uses_live_session_grants_and_the_same_review_and_receipt_owner() {
    let provider = Arc::new(Provider::default());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}", listener.local_addr().unwrap());
    let app = Router::new()
        .route("/v1/chat/completions", post(respond))
        .with_state(provider.clone());
    let server = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    let fixture = fixture(&endpoint);
    let sources = ProfileCatalog::new(fixture.paths.clone());
    let path = sources
        .copy_host(
            &HostProfileId::new("fixture").unwrap(),
            &HostProfileId::new("editable").unwrap(),
        )
        .unwrap();
    let original = std::fs::read(&path).unwrap();
    let running =
        RunningRsi::boot_host_profile(composition(fixture.paths.clone()), &host_profile(&fixture))
            .await
            .unwrap();
    let target = catalog(&running, CallOrigin::Local, "editable")
        .await
        .leaves
        .into_iter()
        .find(|leaf| leaf.target.leaf == "fixture-provider")
        .unwrap()
        .target;
    grant(
        &running,
        Grant {
            principal: Principal::Local,
            target: target.clone(),
            operation: ChangeKind::Disable,
        },
        true,
    )
    .await;
    let workspace = running
        .workspace_registry()
        .unwrap()
        .get_or_create(&fixture.workspace)
        .await
        .unwrap();
    let id = rsi_agent_session_protocol::SessionId::new("profile-agent").unwrap();
    let session = running
        .session_service()
        .unwrap()
        .create(rsi_session_protocol::CreateSession {
            workspace_id: workspace.id,
            session_id: id.clone(),
            agent_preset_id: None,
        })
        .await
        .unwrap();
    let preview = json!({"operation":"preview","request":{"target":target,"change":{"kind":"enabled","enabled":false}}});
    provider.queue(preview.clone());
    super::super::run_message_to_terminal(&session, "profile-tool-denied").await;
    assert!(
        provider.result().contains("Unauthorized"),
        "Local permission must not authorize an Agent tool"
    );
    assert_eq!(std::fs::read(&path).unwrap(), original);
    let scope = Grant {
        principal: Principal::Agent(id),
        target,
        operation: ChangeKind::Disable,
    };
    grant(&running, scope.clone(), true).await;
    provider.queue(preview);
    super::super::run_message_to_terminal(&session, "profile-tool-preview").await;
    let output: Value = serde_json::from_str(&provider.result()).unwrap();
    let preview: Preview = serde_json::from_value(output["data"].clone()).unwrap();
    assert_eq!(std::fs::read(&path).unwrap(), original);
    provider.queue(json!({"operation":"commit","request":commit(&preview)}));
    super::super::run_message_to_terminal(&session, "profile-tool-commit").await;
    let output: Value = serde_json::from_str(&provider.result()).unwrap();
    let receipt: Receipt = serde_json::from_value(output["data"].clone()).unwrap();
    assert!(matches!(
        receipt.outcome,
        Outcome::Saved {
            application: Application::NotSelected,
            ..
        }
    ));
    let saved = std::fs::read(&path).unwrap();
    assert_ne!(saved, original);
    assert!(matches!(
        call::<Receipt>(
            &running,
            CallOrigin::Local,
            Operation::Receipt,
            &ticket(&preview)
        )
        .await,
        Err(ApiError::Unauthorized)
    ));
    grant(&running, scope, false).await;
    provider.queue(json!({"operation":"receipt","ticket":ticket(&preview)}));
    super::super::run_message_to_terminal(&session, "profile-tool-receipt").await;
    let output: Value = serde_json::from_str(&provider.result()).unwrap();
    assert_eq!(output["data"]["preview"]["ticket"], preview.ticket);
    assert_eq!(std::fs::read(&path).unwrap(), saved);
    assert_eq!(provider.requests.lock().unwrap().len(), 8);
    assert!(running.shutdown().await.is_clean());
    server.abort();
}
