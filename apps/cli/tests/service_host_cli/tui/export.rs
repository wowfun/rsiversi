use super::*;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn fullscreen_export_quotes_paths_and_keeps_model_context_unchanged() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}", listener.local_addr().unwrap());
    let requests = Arc::new(std::sync::Mutex::new(Vec::<serde_json::Value>::new()));
    let router = Router::new()
        .route(
            "/v1/chat/completions",
            post(super::super::commands::capture),
        )
        .with_state(requests.clone());
    let provider = tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    let fixture = CliFixture::new(&endpoint);
    let mut terminal = TerminalClient::start(&fixture, &["--session-id", "tui-export"]);
    terminal.until("Ctrl+J adds a line").await;
    terminal.send(b"/export 'empty draft.json' -f json -i h,m\r");
    terminal.until("Exported").await;
    let draft: serde_json::Value =
        serde_json::from_slice(&std::fs::read(fixture.workspace.join("empty draft.json")).unwrap())
            .unwrap();
    assert_eq!(draft["messages"], serde_json::json!([]));
    assert!(requests.lock().unwrap().is_empty());
    terminal.send(b"hello for export\r");
    terminal.until("hello from daemon").await;
    terminal.send(b"/export 'nested/tui file.json' -f json -i m\r");
    tokio::time::timeout(Duration::from_secs(10), async {
        while !fixture.workspace.join("nested/tui file.json").is_file() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    let text = std::fs::read_to_string(fixture.workspace.join("nested/tui file.json")).unwrap();
    terminal
        .until_screen("second export notice", |screen| {
            screen.contains("Exported") && screen.contains("tui file.json")
        })
        .await;
    assert!(text.contains("hello from daemon") && !text.contains("/export"));
    assert_eq!(requests.lock().unwrap().len(), 1);
    terminal.send(b"/quit\r");
    terminal.finish().await;
    provider.abort();
}
