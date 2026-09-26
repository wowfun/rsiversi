use super::*;
use serde_json::{Value, json};
pub(super) async fn provider() -> (
    String,
    Arc<std::sync::Mutex<Vec<Value>>>,
    tokio::task::JoinHandle<()>,
) {
    let requests = Arc::new(std::sync::Mutex::new(Vec::<Value>::new()));
    let received = requests.clone();
    let app = Router::new().route("/v1/chat/completions", post(move |axum::Json(body): axum::Json<Value>| {
        received.lock().unwrap().push(body.clone());
        async move {
            let messages = body["messages"].as_array().unwrap();
            let saved = messages.iter().find(|m| m["tool_call_id"] == "save");
            let reviewed = messages.iter().any(|m| m["tool_call_id"] == "review");
            let call = if reviewed { None } else if let Some(saved) = saved {
                let saved: Value = serde_json::from_str(saved["content"].as_str().unwrap()).unwrap();
                Some(("review", "request_plan_execution", json!({"plan_ref":saved["plan_ref"]})))
            } else { Some(("save", "plan_write", json!({"title":"Review terminal plan","body":"Inspect source, verify the result."}))) };
            let tool = call.is_some();
            let delta = if let Some((id, name, args)) = call { json!({"role":"assistant","tool_calls":[{"index":0,"id":id,"type":"function","function":{"name":name,"arguments":args.to_string()}}]}) } else { json!({"role":"assistant","content":"PLAN_REVIEW_DONE"}) };
            let first = json!({"choices":[{"delta":delta,"finish_reason":null}]});
            let last = json!({"choices":[{"delta":{},"finish_reason":if tool {"tool_calls"} else {"stop"}}],"usage":{"prompt_tokens":10,"completion_tokens":5}});
            Response::builder().status(200).header("content-type","text/event-stream").body(Body::from(format!("data: {first}\n\ndata: {last}\n\ndata: [DONE]\n\n"))).unwrap()
        }
    }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}", listener.local_addr().unwrap());
    let task = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    (endpoint, requests, task)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn cli_closed_plan_review_sends_typed_choice_and_feedback() {
    let (endpoint, requests, task) = provider().await;
    let fixture = CliFixture::new(&endpoint);
    let mut client = JsonClient::start(&fixture, &["--session-id", "cli-plan-review"]);
    client.send("/plan on\n").await;
    client.until(|v| v["type"] == "command_result").await;
    client.send("Save and review a plan\n").await;
    let pending = client
        .until(|v| {
            v["type"] == "interactions"
                && v["data"]["questions"]
                    .as_array()
                    .is_some_and(|q| !q.is_empty())
        })
        .await;
    let request = &pending["data"]["questions"][0];
    let id = request["id"].as_str().unwrap();
    client.send(&format!(":answer {id}\n")).await;
    let prompt = client.until(|v| v["type"] == "answer_prompt").await;
    assert_eq!(
        prompt["data"]["review"]["choices"]
            .as_array()
            .unwrap()
            .len(),
        3
    );
    client.send("1 checked in CLI\n").await;
    client.until(|v| v["type"] == "question_answer").await;
    client.until(|v| v["type"] == "outcome").await;
    client.send(":exit\n").await;
    client.finish().await;
    let requests = requests.lock().unwrap();
    assert_eq!(requests.len(), 3);
    assert!(
        requests[2]["messages"]
            .to_string()
            .contains("checked in CLI")
    );
    assert!(
        requests[2]["messages"]
            .to_string()
            .contains("Plan mode is disabled")
    );
    task.abort();
}
