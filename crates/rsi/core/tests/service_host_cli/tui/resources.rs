use super::*;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn skill_file_and_frozen_reference_workflows_use_real_tui_actions() {
    let (endpoint, state, provider) = gated_provider("skill_read").await;
    *state.arguments.lock().unwrap() = Some(serde_json::json!({"name": "guide"}));
    state.release.notify_one();
    let fixture = CliFixture::new(&endpoint);
    let skill = fixture.temporary.path().join("external-skills/guide");
    std::fs::create_dir_all(&skill).unwrap();
    std::fs::write(
        skill.join("SKILL.md"),
        "---\nname: guide\ndescription: PTY guide preview\n---\nSKILL_BODY_ONLY_PTY\n",
    )
    .unwrap();
    std::fs::create_dir(fixture.workspace.join(".git")).unwrap();
    let linked = fixture.workspace.join(".agents/skills/guide");
    std::fs::create_dir_all(linked.parent().unwrap()).unwrap();
    std::os::unix::fs::symlink(&skill, &linked).unwrap();
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
    {
        let requests = state.requests.lock().unwrap();
        assert_eq!(requests.len(), 2);
        assert!(
            requests[1]["messages"]
                .as_array()
                .unwrap()
                .iter()
                .any(|message| {
                    message["role"] == "tool"
                        && message["content"]
                            .as_str()
                            .is_some_and(|text| text.contains("SKILL_BODY_ONLY_PTY"))
                })
        );
    }
    terminal.send(b"\x10");
    terminal.select_menu("New session").await;
    terminal.absent("VISIBLE_SOURCE_PTY").await;
    terminal.send(b"/reference tui-resource-source\r");
    terminal.until("Captured through record").await;
    terminal.until("VISIBLE_SOURCE_PTY").await;
    terminal.capture();
    terminal.send(b"\r");
    terminal.select_menu("Add frozen reference to draft").await;
    terminal.until("Frozen reference added").await;
    terminal.send(b"\x10");
    terminal.select_menu("Draft references").await;
    terminal.select_menu("tui-resource-source").await;
    terminal.until("Captured through record").await;
    terminal.send(b"\r");
    terminal.select_menu("Remove from draft").await;
    terminal.until("Reference removed from draft").await;
    terminal.send(b"\x04");
    terminal.finish().await;
    provider.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[allow(clippy::too_many_lines)] // One isolated journey proves Home, attachment, session switching and restart share the intended mode lifetime.
async fn project_and_personal_skills_and_markdown_work_through_both_tui_entries() {
    async fn markdown_reply(
        State(requests): State<Arc<std::sync::Mutex<Vec<serde_json::Value>>>>,
        axum::Json(request): axum::Json<serde_json::Value>,
    ) -> Response {
        let count = {
            let mut requests = requests.lock().unwrap();
            requests.push(request);
            requests.len()
        };
        let body = concat!(
            "data: {\"choices\":[{\"delta\":{\"role\":\"assistant\",\"content\":\"# Heading\\n\\n**MARKDOWN_ANSWER** &amp; 中文\"},\"finish_reason\":null}]}\n\n",
            "data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}],\"usage\":{\"prompt_tokens\":2,\"completion_tokens\":3}}\n\n",
            "data: [DONE]\n\n"
        );
        let body = body.replace("&amp; 中文", &format!("&amp; 中文 REPLY_{count}"));
        Response::builder()
            .status(StatusCode::OK)
            .header("content-type", "text/event-stream")
            .body(Body::from(body))
            .unwrap()
    }
    for (launcher, daemon) in [(&["tui"][..], false), (&["--profile", "tui"][..], true)] {
        let requests = Arc::new(std::sync::Mutex::new(Vec::<serde_json::Value>::new()));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!("http://{}/v1", listener.local_addr().unwrap());
        let router = Router::new()
            .route("/v1/chat/completions", post(markdown_reply))
            .route("/v1/models", axum::routing::get(|| async {
                axum::Json(serde_json::json!({"data":[{"id":"setup-model","context_window_tokens":10000,"max_output_tokens":1000}]}))
            }))
            .with_state(requests.clone());
        let provider = tokio::spawn(async move {
            axum::serve(listener, router).await.unwrap();
        });
        let fixture = CliFixture::new(&endpoint);
        let skill = fixture.temporary.path().join("external-skills/guide");
        std::fs::create_dir_all(&skill).unwrap();
        std::fs::write(
            skill.join("SKILL.md"),
            "---\nname: guide\ndescription: PERSONAL_DOLLAR_GUIDE\n---\nHOME_SKILL_BODY\n",
        )
        .unwrap();
        let linked = fixture.temporary.path().join("home/.agents/skills");
        std::fs::create_dir_all(linked.parent().unwrap()).unwrap();
        std::os::unix::fs::symlink(skill.parent().unwrap(), &linked).unwrap();
        std::fs::create_dir(fixture.workspace.join(".git")).unwrap();
        std::fs::write(
            fixture.workspace.join("AGENTS.md"),
            "DEFAULT_PROJECT_AGENTS_PTY",
        )
        .unwrap();
        let project_skills = fixture.workspace.join(".agents/skills");
        std::fs::create_dir_all(&project_skills).unwrap();
        for name in ["eli5", "grilling"] {
            let target = fixture.temporary.path().join("external-skills").join(name);
            std::fs::create_dir_all(&target).unwrap();
            std::fs::write(target.join("SKILL.md"), format!("---\nname: {name}\ndescription: PROJECT_{name}_PREVIEW\n---\nEXTERNAL_PROJECT_{name}_BODY\n")).unwrap();
            let target = if name == "eli5" {
                std::path::PathBuf::from("../../../external-skills/eli5")
            } else {
                target
            };
            std::os::unix::fs::symlink(target, project_skills.join(name)).unwrap();
        }
        let settings_path = fixture.temporary.path().join("config/rsi/settings.json");
        std::fs::remove_file(&settings_path).unwrap();
        if daemon {
            fixture.assert_success(&["host", "start"]);
        }
        let mut terminal =
            TerminalClient::launch(&fixture, &["--session-id", "markdown-source"], launcher);
        terminal.until("No session attached").await;
        terminal.send(b"/markdown\r");
        terminal.until("Markdown rendering off").await;
        terminal.send(b"/login openai-compatible\r");
        terminal.until("API base URL").await;
        terminal.send(format!("{endpoint}\r").as_bytes());
        terminal.until("API key · masked").await;
        terminal.until("Enter reuses").await;
        terminal.send(b"fixture-secret\r");
        terminal.until("setup-model").await;
        terminal.send(b"\r");
        terminal.select_menu("Provider default").await;
        let settings = std::fs::read(&settings_path).unwrap();
        terminal.until("Ctrl+J adds a line").await;
        terminal.send(b"use $gu");
        terminal.until("PERSONAL_DOLLAR_GUIDE").await;
        terminal.send(b"\x1bOQ");
        terminal.until("HOME_SKILL_BODY").await;
        terminal.send(b"\x1b");
        terminal.absent("HOME_SKILL_BODY").await;
        terminal.send(b"\t");
        terminal.until("use $guide").await;
        terminal.send(b" $eli5 $grilling\r");
        terminal.until("**MARKDOWN_ANSWER** &amp;").await;
        terminal.send(b"/markdown on\r");
        terminal.until("Markdown rendering on").await;
        terminal.until("MARKDOWN_ANSWER & 中文").await;
        terminal.absent("**MARKDOWN_ANSWER**").await;
        assert_eq!(requests.lock().unwrap().len(), 1);
        assert!(
            requests.lock().unwrap()[0]
                .to_string()
                .contains("HOME_SKILL_BODY")
        );
        for marker in [
            "DEFAULT_PROJECT_AGENTS_PTY",
            "EXTERNAL_PROJECT_eli5_BODY",
            "EXTERNAL_PROJECT_grilling_BODY",
        ] {
            assert!(
                requests.lock().unwrap()[0].to_string().contains(marker),
                "{marker}"
            );
        }
        terminal.send(b"/markdown off\r");
        terminal.until("Markdown rendering off").await;
        terminal.until("**MARKDOWN_ANSWER** &amp;").await;
        terminal.send(b"/new\r");
        terminal.absent("MARKDOWN_ANSWER").await;
        terminal.send(b"/markdown\r");
        terminal.until("Markdown rendering on").await;
        assert_eq!(requests.lock().unwrap().len(), 1);
        terminal.send(b"/eli");
        terminal.until("PROJECT_eli5_PREVIEW").await;
        terminal.send(b"\x1bOQ");
        terminal.until("EXTERNAL_PROJECT_eli5_BODY").await;
        terminal.send(b"\x1b");
        terminal.absent("EXTERNAL_PROJECT_eli5_BODY").await;
        terminal.send(b"\t");
        terminal.until("/eli5").await;
        terminal.send(b"\r");
        terminal.until("REPLY_2").await;
        assert!(
            requests.lock().unwrap()[1]
                .to_string()
                .contains("EXTERNAL_PROJECT_eli5_BODY")
        );
        assert!(
            requests.lock().unwrap()[1]
                .to_string()
                .contains("DEFAULT_PROJECT_AGENTS_PTY")
        );
        terminal.send(b"\x04");
        terminal.finish().await;
        assert_eq!(std::fs::read(&settings_path).unwrap(), settings);
        let db = rusqlite::Connection::open(
            fixture
                .temporary
                .path()
                .join("state/rsi/agent/sessions.sqlite3"),
        )
        .unwrap();
        assert_eq!(
            db.query_row("SELECT COUNT(*) FROM sessions", [], |row| row
                .get::<_, i64>(0))
                .unwrap(),
            2
        );
        assert_eq!(
            db.query_row(
                "SELECT COUNT(*) FROM facts WHERE fact_json LIKE '%/markdown%'",
                [],
                |row| row.get::<_, i64>(0)
            )
            .unwrap(),
            0
        );
        drop(db);
        if daemon {
            fixture.assert_success(&["host", "restart"]);
        }
        let mut terminal =
            TerminalClient::launch(&fixture, &["--resume", "markdown-source"], launcher);
        terminal.until("MARKDOWN_ANSWER & 中文").await;
        terminal.absent("**MARKDOWN_ANSWER**").await;
        terminal.send(b"/skill grilling\r");
        terminal.until("REPLY_3").await;
        assert!(
            requests.lock().unwrap()[2]
                .to_string()
                .contains("EXTERNAL_PROJECT_grilling_BODY")
        );
        terminal.send(b"\x04");
        terminal.finish().await;
        assert_eq!(requests.lock().unwrap().len(), 3);
        if daemon {
            fixture.assert_success(&["host", "stop"]);
        }
        provider.abort();
    }
}
