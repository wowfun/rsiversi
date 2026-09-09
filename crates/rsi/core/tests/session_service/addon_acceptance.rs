use super::*;
use rsi_agent_session_protocol::{CommandArguments, DomainRequestId, SessionCommandInvocation};
use rsi_session_protocol::SessionHandle;
use serde_json::{Value, json};
use std::collections::VecDeque;
use std::sync::atomic::Ordering;

#[path = "../../../../../fixtures/rsi/addon-workbench/addon.rs"]
mod workbench;

#[derive(Default)]
struct Provider {
    requests: Mutex<Vec<Value>>,
    calls: Mutex<VecDeque<Option<(&'static str, Value)>>>,
}
async fn respond(State(provider): State<Arc<Provider>>, Json(request): Json<Value>) -> Response {
    let call_id = {
        let mut requests = provider.requests.lock().unwrap();
        requests.push(request);
        format!("workbench-call-{}", requests.len())
    };
    let call = provider.calls.lock().unwrap().pop_front().flatten();
    let Some((name, arguments)) = call else {
        return chat().await;
    };
    let call = json!({"choices":[{"delta":{"role":"assistant","tool_calls":[{"index":0,"id":call_id,"type":"function","function":{"name":name,"arguments":arguments.to_string()}}]},"finish_reason":null}]});
    let finish = json!({"choices":[{"delta":{},"finish_reason":"tool_calls"}],"usage":{"prompt_tokens":2,"completion_tokens":1}});
    Response::builder()
        .status(StatusCode::OK)
        .header("content-type", "text/event-stream")
        .body(Body::from(format!(
            "data: {call}\n\ndata: {finish}\n\ndata: [DONE]\n\n"
        )))
        .unwrap()
}
async fn set_plan(handle: &Arc<dyn SessionHandle>, argument: &str, request: &str) {
    let commands = handle.commands().await.unwrap();
    handle
        .execute_command(SessionCommandInvocation {
            command: commands
                .commands()
                .iter()
                .find(|entry| entry.name() == "plan")
                .unwrap()
                .id()
                .clone(),
            request_id: DomainRequestId::new(request).unwrap(),
            expected_revision: commands.revision(),
            arguments: CommandArguments::new(argument.into()).unwrap(),
        })
        .await
        .unwrap();
}
async fn enabled(handle: &Arc<dyn SessionHandle>) -> bool {
    let snapshot = handle
        .observe_projections()
        .await
        .unwrap()
        .next()
        .await
        .unwrap()
        .unwrap();
    snapshot
        .snapshot()
        .entries()
        .iter()
        .find_map(|entry| {
            entry
                .view()
                .map(|view| view.value()["enabled"].as_bool().unwrap())
        })
        .unwrap()
}
async fn create(running: &RunningRsi, fixture: &Fixture, id: &str) -> Arc<dyn SessionHandle> {
    running
        .session_service()
        .unwrap()
        .create(CreateSession {
            workspace_id: running
                .workspace_registry()
                .unwrap()
                .get_or_create(&fixture.workspace)
                .await
                .unwrap()
                .id,
            session_id: SessionId::new(id).unwrap(),
            agent_preset_id: Some(rsi_agent_presets::AgentPresetId::new("workbench").unwrap()),
            workspace_trust: WorkspaceTrust::Untrusted,
        })
        .await
        .unwrap()
}
async fn echo(provider: &Provider, handle: &Arc<dyn SessionHandle>, id: &str, label: &str) {
    provider.calls.lock().unwrap().extend([
        Some(("fixture_echo", json!({"input":"public-addon"}))),
        None,
    ]);
    run_message_to_terminal(handle, id).await;
    let history = handle.history_before(None, 128).await.unwrap();
    let result = history
        .facts
        .iter()
        .rev()
        .find(|fact| matches!(fact.body(), SessionFactBody::ToolResult { .. }))
        .unwrap_or_else(|| panic!("echo {id} missing ToolResult: {:?}", history.facts));
    assert!(
        serde_json::to_string(result.body())
            .unwrap()
            .contains(&format!("\"label\":\"{label}\"")),
        "echo {id} expected {label}: {:?}",
        result.body()
    );
    assert!(
        provider.calls.lock().unwrap().is_empty(),
        "echo {id} ended before provider followup: {:?}",
        history.facts
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn independent_addon_owns_draft_state_policy_generations_and_cold_recovery() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}", listener.local_addr().unwrap());
    let provider = Arc::new(Provider::default());
    let router = Router::new()
        .route("/v1/chat/completions", post(respond))
        .with_state(provider.clone());
    let server = tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    let fixture = fixture(&endpoint);
    let preset = fixture.paths.config().join("agent-presets/workbench");
    std::fs::create_dir_all(&preset).unwrap();
    let path = preset.join("agent.profile.toml");
    std::fs::write(&path, workbench::profile("A")).unwrap();
    let evidence = Arc::new(workbench::Evidence::default());
    let assembly =
        composition(fixture.paths.clone()).with_addons(workbench::addons(evidence.clone()));
    let running = RunningRsi::boot(assembly.clone(), &fixture.profile)
        .await
        .unwrap();
    let old = prepare_durable_state(&running, &fixture, &provider, &evidence).await;
    let generation_a = generation(&old).await;
    provider
        .calls
        .lock()
        .unwrap()
        .push_back(Some(("fixture_echo", json!({"hold":true}))));
    let held = old.clone();
    let waiter = tokio::spawn(async move {
        run_message_to_terminal(&held, "held-a").await;
    });
    tokio::time::timeout(
        std::time::Duration::from_secs(5),
        evidence.entered.notified(),
    )
    .await
    .unwrap();
    std::fs::write(&path, workbench::profile("B")).unwrap();
    let new = create(&running, &fixture, "workbench-new").await;
    echo(&provider, &new, "new-b", "B").await;
    let generation_b = generation(&new).await;
    assert_ne!(generation_a, generation_b);
    assert_eq!(generation(&old).await, generation_a);
    assert_eq!(evidence.live_tools.load(Ordering::SeqCst), 2);
    evidence.release.notify_one();
    waiter.await.unwrap();
    assert_tool_label(&old, "A").await;
    assert!(!enabled(&old).await);
    let child = fork_completed_state(&running, &provider, &new).await;
    assert!(running.shutdown().await.is_clean());
    drop((old, new, running));
    assert_eq!(evidence.live_tools.load(Ordering::SeqCst), 0);
    let restarted = RunningRsi::boot(assembly, &fixture.profile).await.unwrap();
    let recovered = restarted
        .session_service()
        .unwrap()
        .attach(&SessionId::new("workbench-old").unwrap())
        .await
        .unwrap();
    assert!(!enabled(&recovered).await);
    assert_eq!(generation(&recovered).await, generation_b);
    let child = restarted
        .session_service()
        .unwrap()
        .attach(&child)
        .await
        .unwrap();
    assert!(!enabled(&child).await);
    echo(&provider, &recovered, "cold-b", "B").await;
    assert!(
        recovered
            .command_status(&DomainRequestId::new("durable-off").unwrap())
            .await
            .unwrap()
            .is_some()
    );
    assert!(restarted.shutdown().await.is_clean());
    drop((child, recovered, restarted));
    assert_eq!(evidence.live_tools.load(Ordering::SeqCst), 0);
    check_removed(&fixture).await;
    server.abort();
}

async fn prepare_durable_state(
    running: &RunningRsi,
    fixture: &Fixture,
    provider: &Provider,
    evidence: &workbench::Evidence,
) -> Arc<dyn SessionHandle> {
    check_settings(running).await;
    let old = create(running, fixture, "workbench-old").await;
    assert!(!enabled(&old).await);
    set_plan(&old, "on", "draft-on").await;
    assert!(enabled(&old).await);
    assert!(old.history_before(None, 8).await.unwrap().facts.is_empty());
    assert!(
        running
            .session_service()
            .unwrap()
            .list_recent(None, 16)
            .await
            .unwrap()
            .sessions
            .is_empty()
    );
    provider
        .calls
        .lock()
        .unwrap()
        .push_back(Some(("fixture_echo", json!({}))));
    run_message_to_terminal(&old, "denied").await;
    assert!(
        provider.requests.lock().unwrap()[0]
            .to_string()
            .contains("Plan mode is enabled")
    );
    assert_eq!(evidence.tool_calls.load(Ordering::SeqCst), 0);
    let history = old.history_before(None, 64).await.unwrap();
    assert!(
        history
            .facts
            .iter()
            .any(|fact| matches!(fact.body(), SessionFactBody::ToolRejected { .. }))
    );
    assert!(!history.facts.iter().any(|fact| matches!(
        fact.body(),
        SessionFactBody::ToolIntent { .. } | SessionFactBody::ToolStarted { .. }
    )));
    drop(history);
    set_plan(&old, "off", "durable-off").await;
    echo(provider, &old, "old-a", "A").await;
    old
}

async fn check_removed(fixture: &Fixture) {
    let removed = RunningRsi::boot(composition(fixture.paths.clone()), &fixture.profile)
        .await
        .unwrap();
    assert!(
        removed
            .settings_access()
            .unwrap()
            .describe("fixture.workbench")
            .await
            .is_err()
    );
    let workspace_id = removed
        .workspace_registry()
        .unwrap()
        .get_or_create(&fixture.workspace)
        .await
        .unwrap()
        .id;
    assert!(
        removed
            .session_service()
            .unwrap()
            .create(CreateSession {
                workspace_id,
                session_id: SessionId::new("removed-addon").unwrap(),
                agent_preset_id: Some(rsi_agent_presets::AgentPresetId::new("workbench").unwrap()),
                workspace_trust: WorkspaceTrust::Untrusted,
            })
            .await
            .is_err()
    );
    assert!(removed.shutdown().await.is_clean());
}

async fn generation(handle: &Arc<dyn SessionHandle>) -> String {
    handle
        .observe_projections()
        .await
        .unwrap()
        .next()
        .await
        .unwrap()
        .unwrap()
        .snapshot()
        .generation_sha256()
        .to_owned()
}
async fn assert_tool_label(handle: &Arc<dyn SessionHandle>, label: &str) {
    let history = handle.history_before(None, 128).await.unwrap();
    let value = history
        .facts
        .iter()
        .rev()
        .find_map(|fact| match fact.body() {
            SessionFactBody::ToolResult { result, .. } => Some(&result.value),
            _ => None,
        })
        .unwrap();
    assert_eq!(value["label"], label);
}
async fn fork_completed_state(
    running: &RunningRsi,
    provider: &Provider,
    parent: &Arc<dyn SessionHandle>,
) -> SessionId {
    set_plan(parent, "on", "idle-on-after-completed-turn").await;
    provider.calls.lock().unwrap().push_back(Some((
        "spawn_agent",
        json!({"task_name":"workbench-child","message":"child acceptance","fork_turns":"all"}),
    )));
    run_message_to_terminal(parent, "fork-workbench").await;
    let children = parent.inspect().await.unwrap().tree.descendants;
    assert_eq!(children.len(), 1);
    let id = children[0].status.session_id.clone();
    let child = running
        .session_service()
        .unwrap()
        .attach(&id)
        .await
        .unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        while child
            .inspect()
            .await
            .unwrap()
            .tree
            .session
            .has_active_activation
        {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    assert!(
        !enabled(&child).await,
        "fork uses completed-turn boundary, excluding later idle command"
    );
    assert!(enabled(parent).await);
    id
}

async fn check_settings(running: &RunningRsi) {
    let settings = running.settings_access().unwrap();
    assert!(
        settings
            .list(None, 64)
            .await
            .unwrap()
            .namespaces
            .iter()
            .any(|name| name == "fixture.workbench")
    );
    assert_eq!(
        settings
            .describe("fixture.workbench")
            .await
            .unwrap()
            .metadata
            .applies,
        rsi_settings_protocol::SettingsApply::Live
    );
    let old = settings.read("fixture.workbench").await.unwrap();
    assert!(
        settings
            .replace(
                "fixture.workbench",
                &old.version(),
                json!({"note":"x".repeat(33)})
            )
            .await
            .is_err()
    );
    let new = settings
        .replace(
            "fixture.workbench",
            &old.version(),
            json!({"note":"public settings"}),
        )
        .await
        .unwrap();
    assert_eq!(new.value["note"], "public settings");
}
