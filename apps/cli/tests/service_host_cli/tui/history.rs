use super::*;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn history_tui_discovers_saved_text_without_a_conversation_id() {
    let (endpoint, state, provider) = gated_provider("history_search").await;
    *state.arguments.lock().unwrap() = Some(
        serde_json::json!({"conversation":{"kind":"native","id":"global-history-source"},"request":{"operation":"search","query":"GLOBAL_NEEDLE"}}),
    );
    state.release.notify_one();
    let fixture = CliFixture::new(&endpoint);
    let mut terminal = TerminalClient::start(&fixture, &["--session-id", "global-history-source"]);
    terminal.capture_name = "history-global".into();
    terminal.until("Ctrl+J adds a line").await;
    terminal.send("GLOBAL_NEEDLE 中文🦀材料\r".as_bytes());
    terminal.until("hello from daemon").await;
    terminal.send(b"\x10");
    terminal.select_menu("New session").await;
    terminal.until("Ctrl+J adds a line").await;
    terminal.send(b"/history GLOBAL_NEEDLE\r");
    terminal.until("Saved history").await;
    for _ in 0..3 {
        terminal.send(b"\r");
        terminal.select_menu("Refresh saved source discovery").await;
        terminal.until("discovery complete").await;
        terminal.send(b"\r");
        terminal.select_menu("Search indexed text again").await;
        terminal.until("Saved history").await;
        if terminal
            .screen
            .lock()
            .unwrap()
            .screen()
            .contents()
            .contains("中文🦀材料")
        {
            break;
        }
    }
    terminal.until("中文🦀材料").await;
    terminal.capture();
    terminal.send(b"\r");
    terminal.select_menu("Open original").await;
    terminal.until("Verified original").await;
    terminal.capture();
    terminal.send(b"\r");
    terminal
        .select_menu("Freeze line 1: GLOBAL_NEEDLE 中文🦀材料")
        .await;
    terminal.until("GLOBAL_NEEDLE").await;
    terminal.send(b"\r");
    terminal.select_menu("Add frozen reference to draft").await;
    terminal.until("Frozen reference added").await;
    terminal.capture();
    terminal.send(b"\x10");
    terminal.select_menu("Draft references").await;
    terminal.select_menu("global-history-source").await;
    terminal.send(b"\r");
    terminal.select_menu("Remove from draft").await;
    terminal.until("Reference removed from draft").await;
    terminal.send(b"\x04");
    terminal.finish().await;
    assert_eq!(state.requests.lock().unwrap().len(), 2);
    provider.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn history_tui_indexes_opens_selects_and_freezes_an_original_without_model_work() {
    let (endpoint, state, provider) = gated_provider("history_search").await;
    *state.arguments.lock().unwrap() = Some(serde_json::json!({
        "conversation":{"kind":"native","id":"tui-history-source"},
        "request":{"operation":"search","query":"HISTORY_SOURCE"}
    }));
    state.release.notify_one();
    let fixture = CliFixture::new(&endpoint);
    let mut terminal = TerminalClient::start(&fixture, &["--session-id", "tui-history-source"]);
    terminal.capture_name = "history".into();
    terminal.until("Ctrl+J adds a line").await;
    terminal.send("HISTORY_SOURCE 中文🦀材料\r".as_bytes());
    terminal.until("hello from daemon").await;
    terminal.send(b"\x10");
    terminal.select_menu("New session").await;
    terminal.until("Ctrl+J adds a line").await;
    terminal.send(b"/history session:tui-history-source HISTORY_SOURCE\r");
    terminal.until("More indexing needed").await;
    terminal.capture();
    terminal.send(b"\r");
    terminal.select_menu("Index next batch").await;
    terminal.until("Caught up at this observation").await;
    terminal.send(b"\r");
    terminal.select_menu("Search indexed text").await;
    terminal.until("HISTORY_SOURCE").await;
    terminal.capture();
    terminal.send(b"\r");
    terminal.select_menu("Open original 1").await;
    terminal.until("Verified original").await;
    terminal.capture();
    terminal.send(b"\r");
    terminal
        .select_menu("Freeze line 1: HISTORY_SOURCE 中文🦀材料")
        .await;
    terminal.until("HISTORY_SOURCE").await;
    terminal.capture();
    terminal.send(b"\r");
    terminal.select_menu("Add frozen reference to draft").await;
    terminal.until("Frozen reference added").await;
    terminal.send(b"DRAFT_KEEP\x10");
    terminal.select_menu("Draft references").await;
    terminal.select_menu("tui-history-source").await;
    terminal.until("HISTORY_SOURCE").await;
    terminal.resize(PtySize {
        rows: 28,
        cols: 48,
        pixel_width: 0,
        pixel_height: 0,
    });
    terminal
        .until_screen("narrow selected reference", |screen| {
            screen.contains("HISTORY_SOURCE")
                && screen.contains("┘")
                && !screen.contains("       ┌Detail")
        })
        .await;
    terminal.capture();
    terminal.send(b"\r");
    terminal.select_menu("Remove from draft").await;
    terminal.until("Reference removed from draft").await;
    terminal.until("DRAFT_KEEP").await;
    terminal.send(b"\x15\x04");
    terminal.finish().await;
    let requests = state.requests.lock().unwrap();
    assert_eq!(requests.len(), 2);
    let tool = requests[1]["messages"]
        .as_array()
        .unwrap()
        .iter()
        .find(|message| message["role"] == "tool")
        .unwrap();
    assert!(
        tool["content"]
            .as_str()
            .unwrap()
            .contains("indexed_through"),
        "{tool}"
    );
    provider.abort();
}
