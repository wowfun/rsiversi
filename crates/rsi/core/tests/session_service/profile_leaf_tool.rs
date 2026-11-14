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
async fn profile_tool_uses_turn_authority_with_stateless_reviews_and_direct_results() {
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
    let sources = ProfileCatalog::new(
        fixture.paths.clone(),
        rsi::ApplicationCatalogMetadata::default(),
    );
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
    let settings = running.settings_access().unwrap();
    let current = settings.read("rsi.agent").await.unwrap();
    let mut value = current.value.clone();
    value["sandbox"] = json!("danger-full-access");
    value["require_approval"] = json!(false);
    settings
        .replace("rsi.agent", &current.version(), value)
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
            scope: rsi_configuration_api::leaf::GrantScope::Profile {
                target: target.clone(),
                operation: ChangeKind::Disable,
            },
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
            fork: None,
            workspace_id: workspace.id.clone(),
            session_id: id.clone(),
            agent_preset_id: None,
        })
        .await
        .unwrap();
    let preview = json!({"operation":"preview","request":{"target":target,"change":{"kind":"enabled","enabled":false}}});
    provider.queue(preview);
    super::super::run_message_to_terminal(&session, "profile-tool-preview").await;
    let output: Value = serde_json::from_str(&provider.result()).unwrap();
    let preview: Preview = serde_json::from_value(output["data"].clone()).unwrap();
    assert_eq!(std::fs::read(&path).unwrap(), original);
    for index in 0..5 {
        provider.queue(json!({"operation":"preview","request":{"target":target,"change":{"kind":"enabled","enabled":false}}}));
        super::super::run_message_to_terminal(&session, &format!("preview-{index}")).await;
        let output: Value = serde_json::from_str(&provider.result()).unwrap();
        assert_eq!(output["data"]["digest"], preview.digest);
        assert!(output["data"].get("ticket").is_none());
    }
    let human: Preview = call(
        &running,
        CallOrigin::Local,
        Operation::Preview,
        &PreviewRequest {
            target: target.clone(),
            change: Change::Enabled { enabled: false },
        },
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(human, preview);
    provider.queue(json!({"operation":"commit","request":commit(&preview)}));
    super::super::run_message_to_terminal(&session, "profile-tool-commit").await;
    let output: Value = serde_json::from_str(&provider.result()).unwrap();
    let receipt: ChangeResult = serde_json::from_value(output["data"].clone()).unwrap();
    assert!(matches!(
        receipt.outcome,
        Outcome::Saved {
            application: Application::NotSelected,
            ..
        }
    ));
    let saved = std::fs::read(&path).unwrap();
    assert_ne!(saved, original);
    let grants: Grants = call(&running, CallOrigin::Local, Operation::Grants, &json!({}))
        .await
        .unwrap()
        .unwrap();
    assert!(
        grants
            .scopes
            .iter()
            .all(|grant| !matches!(grant.principal, Principal::Agent(_)))
    );

    let current = settings.read("rsi.agent").await.unwrap();
    let mut value = current.value.clone();
    value["sandbox"] = json!("read-only");
    settings
        .replace("rsi.agent", &current.version(), value)
        .await
        .unwrap();
    let other = running
        .session_service()
        .unwrap()
        .create(rsi_session_protocol::CreateSession {
            fork: None,
            workspace_id: workspace.id,
            session_id: rsi_agent_session_protocol::SessionId::new("other-profile-agent").unwrap(),
            agent_preset_id: None,
        })
        .await
        .unwrap();
    provider.queue(json!({"operation":"catalog","query":{"profile":"editable"}}));
    run_approved(
        &other,
        "profile-tool-denied",
        rsi_approval_protocol::ApprovalDecision::Deny,
    )
    .await;
    assert!(!provider.result().contains("\"operation\":\"catalog\""));
    provider.queue(json!({"operation":"catalog","query":{"profile":"editable"}}));
    run_approved(
        &other,
        "profile-tool-approved",
        rsi_approval_protocol::ApprovalDecision::AllowOnce,
    )
    .await;
    let output: Value = serde_json::from_str(&provider.result()).unwrap();
    assert_eq!(output["operation"], "catalog");
    assert!(
        output["data"]["leaves"]
            .as_array()
            .unwrap()
            .iter()
            .all(|leaf| leaf["allowed"].as_array().unwrap().len() == 3)
    );
    assert_eq!(std::fs::read(&path).unwrap(), saved);
    drop(settings);
    assert!(running.shutdown().await.is_clean());
    server.abort();
}

async fn run_approved(
    handle: &Arc<dyn rsi_session_protocol::SessionHandle>,
    message: &'static str,
    decision: rsi_approval_protocol::ApprovalDecision,
) {
    use futures_util::StreamExt as _;
    let mut interactions = handle.observe_interactions().await.unwrap();
    let turn = tokio::spawn({
        let handle = handle.clone();
        async move { super::super::run_message_to_terminal(&handle, message).await }
    });
    let approval = tokio::time::timeout(std::time::Duration::from_secs(8), async {
        loop {
            let snapshot = interactions.next().await.unwrap().unwrap();
            if let Some(approval) = snapshot.approvals().first() {
                break approval.clone();
            }
        }
    })
    .await
    .expect("Host Profile Tool must reach native approval before executing");
    assert!(
        handle
            .answer_approval(
                &rsi_agent_session_protocol::SessionId::new(approval.subject.session_id()).unwrap(),
                &approval.id,
                decision,
            )
            .await
            .unwrap()
    );
    turn.await.unwrap();
}
