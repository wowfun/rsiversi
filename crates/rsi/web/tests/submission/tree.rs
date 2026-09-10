use super::*;
use rsi_agent_store_protocol::{StoreAgentDescendantStatus, StoreAgentSessionStatus};
use serde_json::{Value, json};
use sources::view;
use std::sync::atomic::Ordering;

async fn fixture() -> (
    Runtime,
    Arc<Backend>,
    Arc<Backend>,
    Arc<rsi_web::WebApplication>,
) {
    let (runtime, root, app) = sources::fixture().await;
    let plugin = runtime
        .root()
        .apply(
            ResolvedFactory::linked(
                "tree-ui",
                "fixture",
                UpdateMode::RestartRequired,
                Arc::new(rsi_session_tree_ui::SessionTreeUiFactory),
            ),
            Value::Null,
        )
        .await
        .unwrap();
    assert_eq!(plugin.snapshot().state, FiberState::Active);
    let root_id = root.header().await.unwrap().session_id().clone();
    let mut selected = None;
    for index in 0..40 {
        let id = SessionId::new(format!("child-{index:02}")).unwrap();
        let child = add_child(
            &root,
            id,
            root_id.clone(),
            &format!("task-{index:02}"),
            vec![index + 1],
        );
        if index == 0 {
            selected = Some(child);
        }
    }
    let child = selected.unwrap();
    let child_id = child.header().await.unwrap().session_id().clone();
    add_child(
        &root,
        SessionId::new("grandchild").unwrap(),
        child_id,
        "nested",
        vec![1, 1],
    );
    root.tree
        .lock()
        .unwrap()
        .sort_by(|left, right| left.status.session_id.cmp(&right.status.session_id));
    root.inspect().await.unwrap().validate().unwrap();
    for seq in 1..=130 {
        child.facts.lock().unwrap().push(text_fact(
            seq,
            if seq == 65 {
                format!("<script>literal</script>{}", "界".repeat(20_000))
            } else {
                format!("Record {seq}")
            },
        ));
    }
    app.command(&json!({"action":"open","pane":0,"session":root_id}).to_string())
        .await
        .unwrap();
    until(|| root.observations.load(Ordering::SeqCst) == 4).await;
    (runtime, root, child, app)
}
fn add_child(
    root: &Backend,
    id: SessionId,
    parent: SessionId,
    task: &str,
    path: Vec<u16>,
) -> Arc<Backend> {
    let backend = Arc::new(Backend::default());
    *backend.header.lock().unwrap() = Some(
        SessionHeader::new(
            id.clone(),
            1,
            "/tmp",
            AgentPresetId::new("test").unwrap(),
            FrozenAgentSettings::new(
                "test",
                "system",
                rsi_ai_protocol::ModelRef::new("test", "model").unwrap(),
                rsi_sandbox::SandboxMode::WorkspaceWrite,
                false,
            )
            .unwrap(),
        )
        .unwrap(),
    );
    root.children
        .lock()
        .unwrap()
        .insert(id.clone(), backend.clone());
    root.tree.lock().unwrap().push(StoreAgentDescendantStatus {
        status: StoreAgentSessionStatus {
            session_id: id,
            durable_control_seq: 1,
            has_open_turn: false,
            has_active_activation: false,
            has_waking_message: false,
        },
        parent_session_id: parent,
        path: AgentPath::new(path).unwrap(),
        task_name: task.into(),
    });
    backend
}
fn text_fact(seq: u64, text: String) -> SessionFact {
    SessionFact::new(
        seq,
        1,
        SessionFactBody::TurnAccepted {
            turn_id: TurnId::new(format!("turn-{seq}")).unwrap(),
            text,
            model: None,
            sandbox: rsi_sandbox::SandboxMode::WorkspaceWrite,
            require_approval: false,
        },
    )
    .unwrap()
}
async fn until(mut condition: impl FnMut() -> bool) {
    tokio::time::timeout(std::time::Duration::from_secs(3), async {
        while !condition() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
}
async fn open(app: &Arc<rsi_web::WebApplication>) {
    let pane = view(app)["panes"][0].clone();
    let surface = pane["ui_surfaces"]
        .as_array()
        .unwrap()
        .iter()
        .find(|surface| surface["title"] == "Agent tree")
        .unwrap();
    app.command(&json!({"action":"ui_surface","pane":0,"generation":pane["generation"],"reference":surface["reference"]}).to_string()).await.unwrap();
}
async fn click(app: &Arc<rsi_web::WebApplication>, label: &str) -> Value {
    let command = ui::button(&view(app)["ui_detail"], Some(label));
    app.command(&command.to_string()).await.unwrap();
    let detail = view(app)["ui_detail"].clone();
    assert!(detail["error"].is_null(), "{detail}");
    detail
}
fn shown(detail: &Value) -> String {
    detail["model"]["standard_view"].to_string()
}
fn source_text(detail: &Value) -> String {
    detail["model"]["standard_view"]["elements"]
        .as_array()
        .unwrap()
        .iter()
        .find(|element| element["kind"] == "code")
        .unwrap()["text"]
        .as_str()
        .unwrap()
        .into()
}

#[tokio::test]
async fn tree_paging_breadcrumbs_and_history_watermarks_use_no_child_observers() {
    let (runtime, root, child, app) = fixture().await;
    open(&app).await;
    let first = click(&app, "Inspect agent tree").await;
    assert!(shown(&first).contains("1–32 of 40"));
    assert!(shown(&first).contains("Idle"));
    let next = click(&app, "More children").await;
    assert!(shown(&next).contains("33–40 of 40"));
    click(&app, "First children").await;
    click(&app, "Inspect task-00").await;
    let nested = click(&app, "Inspect nested").await;
    assert!(shown(&nested).contains("Agent: task-00"));
    assert!(shown(&nested).contains("Agent: nested"));
    click(&app, "Agent: task-00").await;
    let page = click(&app, "Read conversation").await;
    assert!(shown(&page).contains("Through Fact 130"));
    assert!(shown(&page).contains("Fact 67"));
    child
        .facts
        .lock()
        .unwrap()
        .push(text_fact(131, "arrived after snapshot".into()));
    let earlier = click(&app, "Earlier history").await;
    assert!(shown(&earlier).contains("Through Fact 130"));
    assert!(shown(&earlier).contains("Fact 3"));
    click(&app, "Open Fact 65 fields").await;
    let first = click(&app, "TurnInput").await;
    let first_text = source_text(&first);
    assert!(first_text.starts_with("<script>literal</script>"));
    assert!(first_text.len() <= 16 * 1024);
    let next = click(&app, "Next field page").await;
    let next_text = source_text(&next);
    let complete = format!("<script>literal</script>{}", "界".repeat(20_000));
    assert_eq!(
        format!("{first_text}{next_text}"),
        complete[..first_text.len() + next_text.len()]
    );
    assert_eq!(
        source_text(&click(&app, "Previous field page").await),
        first_text
    );
    click(&app, "Agent: task-00").await;
    let latest = click(&app, "Read conversation").await;
    assert!(shown(&latest).contains("Through Fact 131"));
    assert_eq!(root.observations.load(Ordering::SeqCst), 4);
    assert!(
        root.children
            .lock()
            .unwrap()
            .values()
            .all(|child| child.observations.load(Ordering::SeqCst) == 0)
    );
    assert!(child.cancel.lock().unwrap().is_empty());
    assert!(runtime.shutdown().await.is_clean());
}

#[tokio::test]
async fn tree_actions_reject_wrong_reader_membership_cursors_and_close_exact_reads() {
    let (runtime, root, child, app) = fixture().await;
    open(&app).await;
    let initial = view(&app)["ui_detail"].clone();
    let mut request = ui::button(&initial, Some("Inspect agent tree"));
    let registry = runtime.root().lookup_local::<rsi_ui::UiContract>().unwrap();
    let reference = ui::reference(&initial, &request["name"]);
    let original = request["input"]["value"].clone();
    for value in [
        json!({"revision":"0","operation":original["operation"]}),
        json!({"revision":original["revision"],"operation":{"kind":"tree","selected":"outside","after":null}}),
        json!({"revision":original["revision"],"operation":{"kind":"history","selected":"child-00","before":"132","watermark":"130"}}),
    ] {
        request["input"]["value"] = value;
        assert!(
            registry
                .invoke(
                    &reference,
                    serde_json::from_value(request["input"].clone()).unwrap()
                )
                .await
                .is_err()
        );
    }
    assert!(child.history_requests.lock().unwrap().is_empty());
    click(&app, "Inspect agent tree").await;
    click(&app, "Inspect task-00").await;
    click(&app, "Read conversation").await;
    click(&app, "Open Fact 130 fields").await;
    child.block_source.store(true, Ordering::SeqCst);
    let command = ui::button(&view(&app)["ui_detail"], Some("TurnInput"));
    let waiting = app.command(&command.to_string());
    until(|| child.active_source.load(Ordering::SeqCst) == 1).await;
    app.command(r#"{"action":"close_detail"}"#).await.unwrap();
    waiting.await.unwrap();
    assert_eq!(child.active_source.load(Ordering::SeqCst), 0);
    assert!(view(&app)["ui_detail"].is_null());
    assert!(root.cancel.lock().unwrap().is_empty());
    assert!(child.cancel.lock().unwrap().is_empty());
    assert!(runtime.shutdown().await.is_clean());
}

#[tokio::test]
async fn maximum_tool_content_has_paged_fields_and_shared_failure_semantics() {
    let (runtime, _root, child, app) = fixture().await;
    child.facts.lock().unwrap().push(
        SessionFact::new(
            131,
            1,
            SessionFactBody::ToolResult {
                turn_id: TurnId::new("tool-turn").unwrap(),
                effect_id: EffectId::new("tool-effect").unwrap(),
                identity: rsi_tools_protocol::ToolResultIdentity::new(
                    "owner",
                    "invocation",
                    "call",
                    "a".repeat(64),
                )
                .unwrap(),
                result: rsi_tools_protocol::ToolResult::new(
                    json!({"exit_code":7}),
                    (0..256)
                        .map(|index| rsi_tools_protocol::ToolContent::Text {
                            text: format!("field {index}"),
                        })
                        .collect(),
                    false,
                )
                .unwrap(),
            },
        )
        .unwrap(),
    );
    open(&app).await;
    click(&app, "Inspect agent tree").await;
    click(&app, "Inspect task-00").await;
    assert!(shown(&click(&app, "Read conversation").await).contains("command failed"));
    let fields = click(&app, "Open Fact 131 fields").await;
    assert!(shown(&fields).contains("1–32 of 257"));
    let next = click(&app, "More fields").await;
    assert!(shown(&next).contains("33–64 of 257"));
    assert!(shown(&click(&app, "ToolText { index: 31 }").await).contains("field 31"));
    assert_eq!(child.observations.load(Ordering::SeqCst), 0);
    assert!(runtime.shutdown().await.is_clean());
}
