use super::*;
use rsi_agent_store_protocol::SessionStore as _;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn fullscreen_queue_editor_preserves_draft_and_withdraws_the_successor() {
    let (endpoint, state, provider) = gated_provider("bash").await;
    let fixture = CliFixture::new(&endpoint);
    let mut terminal = TerminalClient::start(&fixture, &["--session-id", "tui-queue-edit"]);
    terminal.capture_name = "queue-edit".into();
    terminal.until("Ctrl+J adds a line").await;
    terminal.send(b"held active work\r");
    tokio::time::timeout(Duration::from_secs(20), state.requested.notified())
        .await
        .unwrap();
    terminal.send(b"queued original\r");
    tokio::time::sleep(Duration::from_millis(250)).await;
    terminal.send(b"ordinary retained draft\x10");
    terminal.select_menu("Pending inputs").await;
    terminal.until("Pending inputs").await;
    terminal.send(b"\r");
    terminal.until("queued original").await;
    terminal.send(b"\r");
    terminal.select_menu("Edit").await;
    terminal.select_menu("Edit text block 1").await;
    terminal.until("Queued input").await;
    terminal.send(b"\x15queued replacement");
    terminal.until("queued replacement").await;
    terminal.capture();
    terminal.send(b"\r");
    terminal.absent("Queued input").await;
    terminal.until("ordinary retained draft").await;
    terminal.send(b"\x10");
    terminal.select_menu("Pending inputs").await;
    terminal.until("Pending inputs").await;
    terminal.send(b"\r");
    terminal.until("queued replacement").await;
    terminal.capture();
    terminal.send(b"\r");
    terminal.select_menu("Withdraw").await;
    terminal.send(b"\x1b");
    tokio::time::sleep(Duration::from_millis(80)).await;
    terminal.until("ordinary retained draft").await;
    terminal.send(b"\x03");
    terminal.until("Stop accepted").await;
    terminal.until("ordinary retained draft").await;
    terminal.send(b"\x15\x04");
    terminal.finish().await;
    assert_eq!(
        state.requests.lock().unwrap().len(),
        1,
        "withdrawn successor never reaches a provider"
    );
    let store =
        rsi_agent_store_sqlite::SqliteStore::open(fixture.temporary.path().join("state/rsi/agent"))
            .unwrap();
    let session = rsi_agent_session_protocol::SessionId::new("tui-queue-edit").unwrap();
    let controls = store.read_controls(&session, 0, 64).await.unwrap();
    let serialized = serde_json::to_string(&controls.records).unwrap();
    assert!(
        serialized.contains("message_successor") && serialized.contains("queue_mutation_recorded")
    );
    assert!(serialized.contains("queued original") && serialized.contains("queued replacement"));
    assert_eq!(
        store
            .read_agent_mailbox(&session, None)
            .await
            .unwrap()
            .pending_count,
        0
    );
    if let Ok(report) = std::env::var("RSI_TUI_PTY_REPORT") {
        std::fs::write(
            std::path::Path::new(&report).join("queue-controls.json"),
            serialized,
        )
        .unwrap();
    }
    provider.abort();
}
