use super::*;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn workspace_review_tui_opens_dirty_baseline_diff_without_model_work() {
    let (endpoint, state, provider) = gated_provider("apply_patch").await;
    *state.arguments.lock().unwrap() = Some(
        serde_json::json!({"patch":"*** Begin Patch\n*** Update File: card.txt\n@@\n-before\n+after · 界\n*** End Patch\n"}),
    );
    state.release.notify_one();
    let fixture = CliFixture::new(&endpoint);
    let git = |args: &[&str]| {
        let output = std::process::Command::new("/usr/bin/git")
            .current_dir(&fixture.workspace)
            .args(args)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    };
    git(&["init", "--quiet"]);
    std::fs::write(fixture.workspace.join("card.txt"), "committed\n").unwrap();
    git(&["add", "."]);
    git(&[
        "-c",
        "user.name=Fixture",
        "-c",
        "user.email=fixture@example.invalid",
        "commit",
        "-qm",
        "fixture",
    ]);
    std::fs::write(fixture.workspace.join("card.txt"), "before\n").unwrap();
    let mut terminal = TerminalClient::start(&fixture, &["--session-id", "tui-review-source"]);
    terminal.capture_name = "workspace-review".into();
    terminal.until("Ctrl+J adds a line").await;
    terminal.send(b"Review interval patch\r");
    terminal.until("hello from daemon").await;
    terminal.send(b"\x10");
    terminal.select_menu("Workspace changes").await;
    terminal
        .until("Changes observed during an execution interval")
        .await;
    terminal.send(b"\r");
    terminal.select_menu("Review workspace changes").await;
    let end = Instant::now() + Duration::from_secs(15);
    while !terminal
        .screen
        .lock()
        .unwrap()
        .screen()
        .contents()
        .contains("Complete")
    {
        assert!(Instant::now() < end, "review capture never settled");
        terminal.send(b"\r");
        terminal.select_menu("Review workspace changes").await;
    }
    terminal.send(b"\r");
    terminal.select_menu("Open interval files").await;
    terminal.until("card.txt").await;
    terminal.send(b"\r");
    terminal.select_menu("Open file diff").await;
    terminal.until("-before").await;
    terminal.until("+after · 界").await;
    terminal.resize(PtySize {
        rows: 24,
        cols: 48,
        pixel_width: 0,
        pixel_height: 0,
    });
    terminal
        .until_screen("narrow diff repaint", |screen| {
            screen.contains("+after · 界")
                && screen.contains('┘')
                && !screen.contains("       ┌Detail")
        })
        .await;
    terminal.send(b"\x1b");
    terminal.until("Ctrl+J adds a line").await;
    terminal.send(b"\x04");
    terminal.finish().await;
    assert_eq!(state.requests.lock().unwrap().len(), 2);
    provider.abort();
}
