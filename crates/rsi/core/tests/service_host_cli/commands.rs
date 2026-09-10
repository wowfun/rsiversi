use super::*;
use serde_json::{Value, json};

pub(super) fn result(output: &Output) -> Value {
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .find(|value| value["type"] == "command_result")
        .expect("command result event")["data"]
        .clone()
}
pub(super) async fn capture(
    State(requests): State<Arc<std::sync::Mutex<Vec<Value>>>>,
    axum::Json(request): axum::Json<Value>,
) -> Response {
    requests.lock().unwrap().push(request);
    chat().await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn headless_commands_freeze_draft_before_model_and_query_durable_receipt_after_restart() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}", listener.local_addr().unwrap());
    let requests = Arc::new(std::sync::Mutex::new(Vec::<Value>::new()));
    let router = Router::new()
        .route("/v1/chat/completions", post(capture))
        .with_state(requests.clone());
    let provider = tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    let fixture = CliFixture::new(&endpoint);
    fixture.assert_success(&["host", "start", "--profile", "fixture"]);
    let list = result(&fixture.assert_success(&[
        "--profile",
        "test-headless",
        "--commands",
        "--session-id",
        "command-session",
        "--output",
        "jsonl",
    ]));
    assert_eq!(list["revision"], json!({"kind":"draft","revision":0}));
    let descriptor = list["commands"]
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry["name"] == "plan")
        .unwrap();
    let on = json!({"command":descriptor["id"],"request_id":"plan-on","expected_revision":list["revision"],"arguments":"on"});
    let output = fixture.assert_success(&[
        "--profile",
        "test-headless",
        "--command",
        &on.to_string(),
        "inspect plan",
        "--session-id",
        "command-session",
        "--output",
        "jsonl",
    ]);
    assert_eq!(
        result(&output)["outcome"],
        json!({"kind":"draft_changed","revision":1})
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("hello from daemon"));
    assert_plan_request(&requests);
    let list = result(&fixture.assert_success(&[
        "--profile",
        "test-headless",
        "--commands",
        "--resume",
        "command-session",
        "--output",
        "jsonl",
    ]));
    assert_eq!(list["revision"]["kind"], "durable");
    let off = json!({"command":descriptor["id"],"request_id":"plan-off","expected_revision":list["revision"],"arguments":"off"});
    let receipt = result(&fixture.assert_success(&[
        "--profile",
        "test-headless",
        "--command",
        &off.to_string(),
        "--resume",
        "command-session",
        "--output",
        "jsonl",
    ]));
    assert_eq!(receipt["outcome"]["kind"], "committed");
    let again = result(&fixture.assert_success(&[
        "--profile",
        "test-headless",
        "--command",
        &off.to_string(),
        "--resume",
        "command-session",
        "--output",
        "jsonl",
    ]));
    assert_eq!(again, receipt);
    fixture.assert_success(&["host", "stop"]);
    fixture.assert_success(&["host", "start", "--profile", "fixture"]);
    let recovered = result(&fixture.assert_success(&[
        "--profile",
        "test-headless",
        "--command-status",
        "plan-off",
        "--resume",
        "command-session",
        "--output",
        "jsonl",
    ]));
    assert_eq!(recovered, receipt);
    assert_eq!(
        requests.lock().unwrap().len(),
        1,
        "command operations never start a provider"
    );
    fixture.assert_success(&["host", "stop"]);
    provider.abort();
}

fn assert_plan_request(requests: &std::sync::Mutex<Vec<Value>>) {
    let requests = requests.lock().unwrap();
    assert_eq!(requests.len(), 1);
    assert!(requests[0].to_string().contains("Plan mode is enabled"));
}
