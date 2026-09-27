use super::*;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn host_profile_tui_grants_prepares_saves_and_reconciles_with_or_without_native_session() {
    for configured in [true, false] {
        let fixture = CliFixture::new("http://127.0.0.1:1");
        let root = fixture.temporary.path().join("config/rsi");
        let original = std::fs::read(root.join("host-profiles/fixture/host.profile.toml")).unwrap();
        let directory = root.join("host-profiles/editable");
        std::fs::create_dir(&directory).unwrap();
        let path = directory.join("host.profile.toml");
        std::fs::write(&path, &original).unwrap();
        if !configured {
            std::fs::remove_file(root.join("settings.json")).unwrap();
        }
        let mut terminal = TerminalClient::start(&fixture, &[]);
        terminal.capture_name = format!("host-profiles-{configured}");
        let ready = if configured {
            "Ctrl+J adds a line"
        } else {
            "No default model"
        };
        terminal.until(ready).await;
        terminal.send(b"PROFILE_DRAFT_KEEP\x10");
        terminal
            .select_menu(if configured {
                "Host Profiles"
            } else {
                "/profiles"
            })
            .await;
        terminal.select_menu("Open Profile editable").await;
        terminal.select_menu("fixture-provider").await;
        terminal.until("Current grants: []").await;
        terminal.select_menu("Grant an exact change").await;
        terminal.select_menu("Grant Disable").await;
        terminal.select_menu("Grant to Local").await;
        terminal.until("Current grants: [Disable]").await;
        terminal.select_menu("Preview disable").await;
        terminal.until("Prepared Host Profile change").await;
        terminal.until("Review:").await;
        assert_eq!(std::fs::read(&path).unwrap(), original);
        terminal.capture();
        terminal.send(b"\r");
        terminal.select_menu("Save reviewed change").await;
        terminal.until("Source: saved").await;
        terminal.until("Current runtime: NotSelected").await;
        terminal.capture();
        let saved = std::fs::read(&path).unwrap();
        assert_ne!(saved, original);
        terminal.send(b"\r");
        terminal.select_menu("Query original receipt").await;
        terminal.until("Current runtime: NotSelected").await;
        assert_eq!(std::fs::read(&path).unwrap(), saved);
        terminal.send(b"\x1b");
        terminal.until("PROFILE_DRAFT_KEEP").await;
        terminal.send(b"\x10");
        terminal
            .select_menu(if configured {
                "Host Profiles"
            } else {
                "/profiles"
            })
            .await;
        terminal
            .select_menu("Recover original source receipts")
            .await;
        terminal.until("Read receipt ").await;
        terminal.send(b"\r");
        terminal.until("Source: saved").await;
        terminal.resize(PtySize {
            rows: 28,
            cols: 48,
            pixel_width: 0,
            pixel_height: 0,
        });
        terminal
            .until_screen("narrow Profile receipt", |screen| {
                screen.contains("Source: saved") && screen.contains("Current runtime: NotSelected")
            })
            .await;
        terminal.capture();
        terminal.send(b"\r");
        terminal.until("Query original receipt").await;
        terminal.send(b"\x1b");
        terminal.until("Enter actions").await;
        terminal.send(b"\x1b");
        terminal.until("PROFILE_DRAFT_KEEP").await;
        terminal.send(b"\x15\x04");
        terminal.finish().await;
        assert_eq!(std::fs::read(&path).unwrap(), saved);
    }
}
