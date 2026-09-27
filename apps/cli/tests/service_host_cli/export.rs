use super::*;
use serde_json::Value;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn cli_export_stdout_file_latest_and_interactive_are_read_only() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}", listener.local_addr().unwrap());
    let requests = Arc::new(std::sync::Mutex::new(Vec::<Value>::new()));
    let router = Router::new()
        .route("/v1/chat/completions", post(super::commands::capture))
        .with_state(requests.clone());
    let provider = tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    let fixture = CliFixture::new(&endpoint);
    fixture.assert_success(&["host", "start", "--profile", "fixture"]);
    fixture.assert_success(&[
        "--profile",
        "test-headless",
        "export Unicode 界",
        "--session-id",
        "export-source",
    ]);
    let count = requests.lock().unwrap().len();
    assert_eq!(count, 1);
    let expected = fixture.assert_success(&[
        "--profile",
        "test-cli",
        "--export",
        "export-source",
        "-f",
        "json",
        "-i",
        "h,m,pie,lpr,last-provider-response",
    ]);
    let value: Value = serde_json::from_slice(&expected.stdout).unwrap();
    assert_eq!(value["header"]["session"]["session_id"], "export-source");
    assert!(value["messages"].to_string().contains("hello from daemon"));
    assert_eq!(value["last_provider_request"]["availability"], "available");
    let output = fixture.assert_success(&[
        "--profile",
        "test-cli",
        "--export",
        "latest",
        "-f",
        "json",
        "-i",
        "h,m,pie,lpr,last-provider-response",
        "-o",
        "nested/export file.json",
    ]);
    assert!(output.stdout.is_empty());
    assert_eq!(
        std::fs::read(fixture.workspace.join("nested/export file.json")).unwrap(),
        expected.stdout
    );
    for arguments in [
        vec!["--output", "jsonl"],
        vec!["--resume", "export-source"],
        vec!["--list"],
        vec!["--session-id", "other"],
    ] {
        let mut args = vec!["--profile", "test-cli", "--export", "latest"];
        args.extend(arguments);
        assert!(!fixture.run(&args).status.success());
    }
    let mut interactive = JsonClient::start(&fixture, &["--resume", "export-source"]);
    interactive.until(|v| v["type"] == "session").await;
    interactive
        .send(":export 'interactive file.json' -f json -i m\n")
        .await;
    interactive.until(|v| v["type"] == "export").await;
    interactive.send(":exit\n").await;
    interactive.finish().await;
    let value: Value = serde_json::from_slice(
        &std::fs::read(fixture.workspace.join("interactive file.json")).unwrap(),
    )
    .unwrap();
    assert!(value["messages"].to_string().contains("hello from daemon"));
    assert_eq!(
        requests.lock().unwrap().len(),
        count,
        "exports must not submit a model request"
    );
    fixture.assert_success(&["host", "stop"]);
    provider.abort();
}
