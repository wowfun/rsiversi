use super::*;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn skill_file_and_frozen_reference_workflows_use_real_tui_actions() {
    let (endpoint, provider) = provider().await;
    let fixture = CliFixture::new(&endpoint);
    let skill = fixture.temporary.path().join("config/rsi/skills/guide");
    std::fs::create_dir_all(&skill).unwrap();
    std::fs::write(
        skill.join("SKILL.md"),
        "---\nname: guide\ndescription: PTY guide preview\n---\nSKILL_BODY_ONLY_PTY\n",
    )
    .unwrap();
    let collision = fixture
        .temporary
        .path()
        .join("config/rsi/skills/model-selection");
    std::fs::create_dir_all(&collision).unwrap();
    std::fs::write(collision.join("SKILL.md"), "---\nname: model-selection\ndescription: Skill collides with hidden Session command\n---\nCOLLISION_PREVIEW\n").unwrap();
    std::fs::write(
        fixture.workspace.join("file with space.txt"),
        "FILE_BODY_ONLY_PTY 中文\n",
    )
    .unwrap();
    let mut terminal = TerminalClient::start(&fixture, &["--session-id", "tui-resource-source"]);
    terminal.capture_name = "resource-workflows".into();
    terminal.until("Ctrl+J adds a line").await;
    terminal.send(b"/model-s");
    terminal
        .until("Skill collides with hidden Session command")
        .await;
    terminal.send(b"\t");
    terminal.until("/skill model-selection").await;
    terminal.send(b"\x15");
    terminal.absent("/skill model-selection").await;
    terminal.send(b"/gu");
    terminal.until("PTY guide preview").await;
    terminal.send(b"\x1bOQ");
    terminal.until("SKILL_BODY_ONLY_PTY").await;
    terminal.capture();
    terminal.send(b"\x1b");
    terminal.absent("SKILL_BODY_ONLY_PTY").await;
    terminal.send(b"\t");
    terminal.until("/guide").await;
    terminal.send(b"\x15");
    terminal.absent("/guide").await;
    terminal.send(b"@");
    terminal.select_menu("file with space.txt").await;
    terminal.until("FILE_BODY_ONLY_PTY").await;
    terminal.capture();
    terminal.send(b"\r");
    terminal.select_menu("Insert path into draft").await;
    terminal.until("@\"file with space.txt\"").await;
    terminal.absent("FILE_BODY_ONLY_PTY").await;
    terminal.capture();
    terminal.send(b"\x15");
    terminal.absent("@\"file with space.txt\"").await;
    terminal.send(b"VISIBLE_SOURCE_PTY\r");
    terminal.until("hello from daemon").await;
    terminal.send(b"\x10");
    terminal.select_menu("New session").await;
    terminal.absent("VISIBLE_SOURCE_PTY").await;
    terminal.send(b"/reference tui-resource-source\r");
    terminal.until("Captured through Fact").await;
    terminal.until("VISIBLE_SOURCE_PTY").await;
    terminal.capture();
    terminal.send(b"\r");
    terminal.select_menu("Add frozen reference to draft").await;
    terminal.until("Frozen reference added").await;
    terminal.send(b"\x10");
    terminal.select_menu("Draft references").await;
    terminal.select_menu("tui-resource-source").await;
    terminal.until("Captured through Fact").await;
    terminal.send(b"\r");
    terminal.select_menu("Remove from draft").await;
    terminal.until("Reference removed from draft").await;
    terminal.send(b"\x04");
    terminal.finish().await;
    provider.abort();
}
