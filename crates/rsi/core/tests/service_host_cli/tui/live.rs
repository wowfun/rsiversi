//! Opt-in real-provider acceptance through the actual Linux PTY application.
use super::*;

async fn live_text(terminal: &mut TerminalClient, text: &str) {
    let deadline = Instant::now() + Duration::from_secs(150);
    loop {
        let visible = terminal
            .screen
            .lock()
            .unwrap()
            .screen()
            .contents()
            .contains(text);
        if visible {
            terminal.capture();
            return;
        }
        assert!(
            terminal.child.try_wait().unwrap().is_none(),
            "live TUI exited before the expected response"
        );
        if Instant::now() >= deadline {
            terminal.capture();
        }
        assert!(Instant::now() < deadline, "live TUI did not reach {text:?}");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires explicit DEEPSEEK_API_KEY and DEEPSEEK_MODEL; spends live quota"]
#[allow(clippy::too_many_lines)] // One opt-in PTY session covers actual effort, Todo, evidence and four geometries.
async fn live_deepseek_tui_effort_todo_request_evidence_and_resize() {
    let key = std::env::var("DEEPSEEK_API_KEY").expect("explicit live credential required");
    let model = std::env::var("DEEPSEEK_MODEL").expect("explicit live model required");
    let model_ref = rsi_ai_protocol::ModelRef::new("live", &model).unwrap();
    let fixture = CliFixture::new("https://api.deepseek.com");
    let profile = format!(
        r#"format=1
[[steps]]
kind="plugin"
id="live-provider"
plugin="rsi.ai.provider.deepseek"
[steps.config]
deployment="live"
endpoint="https://api.deepseek.com"
credential={{owner="rsi.ai.provider.deepseek",slot="default"}}
[steps.config.language_models.{model}]
context_window_tokens=1000000
default_output_reserve_tokens=4096
max_output_reserve_tokens=256000
[steps.config.reasoning_efforts.{model}]
supported=["off","low","high","max"]
default="off"
"#
    );
    std::fs::write(
        fixture
            .temporary
            .path()
            .join("config/rsi/host-profiles/fixture/host.profile.toml"),
        profile,
    )
    .unwrap();
    std::fs::write(fixture.temporary.path().join("config/rsi/settings.json"),serde_json::to_vec(&serde_json::json!({"rsi.agent":{"default_model":model_ref,"default_reasoning_effort":"off"}})).unwrap()).unwrap();
    TerminalClient::write_profile(&fixture, false);
    let mut terminal = TerminalClient::launch_environment(
        &fixture,
        &["--session-id", "live-tui-evidence"],
        &["--profile", "test-tui"],
        &[("DEEPSEEK_API_KEY", Some(key.as_str()))],
    );
    terminal.capture_name = "live-deepseek".into();
    terminal.until("Ctrl+J adds a line").await;
    terminal.until(&format!("{model} · off")).await;
    terminal.send(b"Use todo_write to record one completed task named 'Live task verified'. Then reply by joining TUI, LIVE, TODO and DONE with underscores. Do not use any other tool.\r");
    live_text(&mut terminal, "TUI_LIVE_TODO_DONE").await;
    terminal.until("in /").await;
    {
        let parser = terminal.screen.lock().unwrap();
        let contents = parser.screen().contents();
        let tool = contents
            .find("Called todo_write(1 tasks)")
            .expect("completed Todo summary");
        let answer = contents.find("TUI_LIVE_TODO_DONE").expect("final answer");
        let lines = contents.lines().collect::<Vec<_>>();
        let model_metadata = lines
            .iter()
            .filter(|line| line.starts_with("• ") && line.contains(&model))
            .collect::<Vec<_>>();
        assert_eq!(
            model_metadata.len(),
            1,
            "only final answer carries model metadata"
        );
        let metadata = contents.find(model_metadata[0].trim_end()).unwrap();
        assert!(tool < answer && answer < metadata);
        let tool_row = u16::try_from(
            parser
                .screen()
                .rows(0, 110)
                .position(|row| row.contains("Called todo_write(1 tasks)"))
                .unwrap(),
        )
        .unwrap();
        assert_eq!(parser.screen().cell(tool_row, 0).unwrap().contents(), "▸");
        assert_eq!(
            parser.screen().cell(tool_row, 0).unwrap().fgcolor(),
            vt100::Color::Idx(10)
        );
    }

    terminal.absent("Tasks 1/1").await;
    terminal.send(b"\x10");
    terminal.select_menu("Tasks").await;
    terminal.until("[x] Live task verified").await;
    terminal.send(b"\x1b");
    terminal.absent("[x] Live task verified").await;
    terminal.send(b"/effort\r");
    terminal.until("Reasoning effort").await;
    terminal.select_menu("high").await;
    terminal.until(&format!("{model} · high")).await;
    terminal.send(b"Compute 37 times 43. Reply only with the prefix TUI_LIVE followed by an underscore and the numeric answer. Do not call tools.\r");
    live_text(&mut terminal, "TUI_LIVE_1591").await;
    terminal.send(b"\x10");
    terminal.select_menu("Request usage").await;
    terminal.until("Session usage").await;
    terminal.until("input tokens").await;
    terminal.send(b"\x1b");
    terminal.absent("Session usage").await;
    terminal.send(b"\x10");
    terminal.select_menu("Inspect requests").await;
    terminal.until("Inspect request").await;
    terminal.send(b"\r");
    terminal.select_menu("System instructions").await;
    terminal.until("Request ").await;
    terminal.send(b"\x1b");
    terminal.absent("←/→ pages").await;
    for (cols, rows) in [(110, 35), (80, 24), (42, 12), (28, 9)] {
        terminal.resize(PtySize {
            rows,
            cols,
            pixel_width: 0,
            pixel_height: 0,
        });
        // A distinctive draft confirms a newly rendered Composer at the new size.
        terminal.send(b"\x15");
        terminal.send(format!("size-{cols}-{rows}").as_bytes());
        terminal.until(&format!("size-{cols}-{rows}")).await;
        terminal.capture();
    }
    terminal.send(b"\x15\x04");
    terminal.finish().await;
    assert!(
        !terminal
            .output
            .lock()
            .unwrap()
            .complete()
            .windows(key.len())
            .any(|window| window == key.as_bytes()),
        "credential reached terminal output"
    );
    let history = fixture.assert_success(&[
        "--profile",
        "test-cli",
        "--history",
        "live-tui-evidence",
        "--output",
        "jsonl",
    ]);
    let records = String::from_utf8(history.stdout)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
        .collect::<Vec<_>>();
    let facts = records
        .iter()
        .filter_map(|record| record.get("fact"))
        .collect::<Vec<_>>();
    // Decode actual durable Facts instead of inferring success from a visible prompt.
    let facts = facts
        .into_iter()
        .map(|fact| {
            serde_json::from_value::<rsi_agent_session_protocol::SessionFact>(fact.clone()).unwrap()
        })
        .collect::<Vec<_>>();
    let snapshots = facts
        .iter()
        .filter_map(|fact| {
            if let rsi_agent_session_protocol::SessionFactBody::ModelIntent {
                snapshot,
                evidence,
                ..
            } = fact.body()
            {
                Some((snapshot, evidence))
            } else {
                None
            }
        })
        .collect::<Vec<_>>();
    assert!(
        snapshots.len() >= 3,
        "Todo requires its own normal model follow-up"
    );
    assert!(
        snapshots
            .iter()
            .all(|(snapshot, evidence)| snapshot.model == model
                && matches!(
                    evidence,
                    rsi_agent_session_protocol::RequestEvidence::Available { .. }
                ))
    );
    let efforts = snapshots
        .iter()
        .map(|(snapshot, _)| {
            snapshot
                .language_settings
                .as_ref()
                .unwrap()
                .effective_reasoning_effort
                .as_ref()
                .unwrap()
                .as_str()
        })
        .collect::<Vec<_>>();
    assert_eq!(efforts[0], "off");
    assert_eq!(*efforts.last().unwrap(), "high");
    assert!(facts.iter().any(|fact|matches!(fact.body(),rsi_agent_session_protocol::SessionFactBody::ToolIntent {name,..} if name=="todo_write")));
    assert!(facts.iter().any(|fact| matches!(
        fact.body(),
        rsi_agent_session_protocol::SessionFactBody::ModelEvent {
            event: rsi_ai_protocol::LanguageEvent::Usage { .. },
            ..
        }
    )));
    if let Ok(directory) = std::env::var("RSI_TUI_PTY_REPORT") {
        let report = serde_json::json!({"model":model,"protocol":"deepseek-responses","requests":snapshots.len(),"efforts":efforts,"todo_tool":true,"request_evidence":true,"sizes":[[110,35],[80,24],[42,12],[28,9]],"platform":"Linux PTY","native_windows_macos":false});
        std::fs::write(
            std::path::Path::new(&directory).join("live-summary.json"),
            serde_json::to_vec_pretty(&report).unwrap(),
        )
        .unwrap();
    }
}
