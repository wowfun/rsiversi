use super::*;

async fn completed(service: &dyn ExternalConversations, id: &ConversationId) {
    tokio::time::timeout(Duration::from_mins(3), async {
        loop {
            let view = service.view(id).await.unwrap();
            assert!(
                view.permissions.is_empty(),
                "fixture policy is never; unexpected interactive request"
            );
            match view.snapshot.status {
                Status::Completed => break,
                Status::Running => tokio::time::sleep(Duration::from_millis(50)).await,
                status => panic!("live DSH outcome: {status:?}"),
            }
        }
    })
    .await
    .expect("live DSH prompt deadline");
}
fn assert_pid_gone(path: &Path) {
    let pid = std::fs::read_to_string(path).unwrap();
    assert!(
        !Path::new("/proc").join(pid.trim()).exists(),
        "MCP peer was not reaped"
    );
}
async fn records(
    service: &dyn ExternalConversations,
    id: &ConversationId,
    epoch: u64,
) -> Vec<rsi_acp_protocol::observation::Record> {
    let mut result = vec![];
    let mut after = 0;
    loop {
        let page = service.page(id, epoch, after).await.unwrap();
        after = page.records.last().map_or(after, |record| record.sequence);
        result.extend(page.records);
        if !page.has_more {
            break;
        }
    }
    result
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "opt-in pinned DSH compiled runtime and real DeepSeek; run fixtures/rsi/acp/dsh-live.py"]
#[expect(
    clippy::too_many_lines,
    reason = "One opt-in interop lifecycle retains startup, live effects, resume and reaping evidence"
)]
async fn pinned_dsh_live_new_private_mcp_resume_without_load_and_reap() {
    let root = std::path::PathBuf::from(
        std::env::var("RSI_DSH_REPORT").expect("isolated report directory"),
    );
    std::fs::create_dir(&root).unwrap();
    let workspace = root.join("workspace");
    std::fs::create_dir(&workspace).unwrap();
    let runtime_root =
        std::path::PathBuf::from(std::env::var("RSI_DSH_RUNTIME").expect("pinned runtime"));
    let node = std::env::var("RSI_DSH_NODE").expect("absolute Node executable");
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../../fixtures/rsi/acp")
        .canonicalize()
        .unwrap();
    let patch = root.join("patch.yml");
    std::fs::write(&patch,format!("- id: llm-deepseek\n  config:\n    thinking: disabled\n    reasoningEffort: off\n    models: [{{id: deepseek-flash}}]\n- id: acp\n  config: {{provider: deepseek-official, model: deepseek-flash}}\n- id: sandbox-policy\n  config: {{mode: danger-full-access, workspaceRoot: {workspace}}}\n- id: fs-sandbox\n  config: {{cwd: {workspace}}}\n- id: approval\n  config: {{policy: never}}\n- id: session-persistence-jsonl\n  config: {{root: {sessions}, compression: none}}\n- id: session-log-deepseek\n  config: {{enabled: false}}\n",workspace=json!(workspace),sessions=json!(root.join("sessions")))).unwrap();
    let runtime = Runtime::default();
    activate(
        &runtime,
        "process",
        rsi_process_local::ProcessLocalFactory,
        json!({}),
    )
    .await;
    activate(
        &runtime,
        "sandbox",
        rsi_sandbox_local::SandboxLocalFactory::default(),
        json!({}),
    )
    .await;
    activate(&runtime, "credentials", Credentials, json!(null)).await;
    let mut environment = serde_json::Map::new();
    for (name, value) in [
        (
            "PATH",
            format!(
                "{}:/usr/bin:/bin",
                Path::new(&node).parent().unwrap().display()
            ),
        ),
        ("HOME", root.join("home").display().to_string()),
        ("DSH_HOME", root.join("dsh").display().to_string()),
        ("XDG_CONFIG_HOME", root.join("config").display().to_string()),
        ("XDG_STATE_HOME", root.join("state").display().to_string()),
        ("XDG_CACHE_HOME", root.join("cache").display().to_string()),
        ("NO_COLOR", "1".into()),
        (
            "RSI_DSH_PEER_PIDS",
            root.join("peer-pids").display().to_string(),
        ),
    ] {
        environment.insert(name.into(), json!({"kind":"literal","value":value}));
    }
    environment.insert(
        "DEEPSEEK_API_KEY".into(),
        json!({"kind":"credential","reference":{"owner":"rsi.acp","slot":"dsh-live"}}),
    );
    let config = json!({"directory":root.join("journal"),"endpoints":[{"id":"pinned-dsh","enabled":true,"cwd":workspace,"sandbox":"danger-full-access","launch":{"program":node,"arguments":["--import",fixture.join("dsh-observe.mjs"),runtime_root.join("node_modules/@deepseek-ai/dsh/lib/bin.js"),"--profile","acp","--patch",patch],"environment":environment},"mcp_servers":[{"name":"interop","launch":{"program":"/usr/bin/python3","arguments":["-u",fixture.join("mcp.py"),root.join("mcp-pid")],"environment":{"ACP_FIXTURE_SECRET":{"kind":"literal","value":"private-mcp-secret-fixture"},"ACP_FIXTURE_EMPTY":{"kind":"literal","value":""},"ACP_FIXTURE_CALLS":{"kind":"literal","value":root.join("mcp-calls.jsonl")}}}}]}]});
    let fiber = activate(&runtime, "dsh", rsi_acp_host::Factory, config).await;
    let service = runtime
        .root()
        .lookup_local::<ExternalConversationsContract>()
        .unwrap();
    let id = id("dsh-live");
    let began = std::time::Instant::now();
    let snapshot = service.start(id.clone(), "pinned-dsh").await.unwrap();
    let startup_ms = began.elapsed().as_millis();
    assert!(snapshot.capabilities.resume);
    assert!(!snapshot.capabilities.load);
    service.submit(&id,"This is an isolated integration test. Call the echo tool from the supplied MCP server with exactly DSH_MCP_VERIFIED. Then use bash to write the exact UTF-8 line dsh-live-ok to milestone.txt and read it back. Do not modify any other file. Reply DSH_LIVE_VERIFIED after both tool results are verified.").await.unwrap();
    completed(service.as_ref(), &id).await;
    assert_eq!(
        std::fs::read(workspace.join("milestone.txt")).unwrap(),
        b"dsh-live-ok\n"
    );
    let calls = std::fs::read_to_string(root.join("mcp-calls.jsonl")).unwrap();
    assert!(
        calls
            .lines()
            .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
            .any(
                |call| call["name"] == "echo" && call["arguments"]["message"] == "DSH_MCP_VERIFIED"
            )
    );
    let initial = records(service.as_ref(), &id, snapshot.epoch).await;
    assert!(initial.iter().any(|record| {
        record
            .value
            .as_ref()
            .is_some_and(|value| value.to_string().contains("DSH_LIVE_VERIFIED"))
    }));
    service.close(&id).await.unwrap();
    assert_reaped(&root);
    assert_pid_gone(&root.join("mcp-pid"));
    let resumed = service.reconnect(&id, Setup::Resume).await.unwrap();
    assert_eq!(resumed.epoch, snapshot.epoch);
    assert_eq!(resumed.remote, snapshot.remote);
    assert_eq!(
        records(service.as_ref(), &id, resumed.epoch).await.len(),
        initial.len(),
        "resume does not replay"
    );
    service.submit(&id,"Reply with DSH_RESUMED and the exact line you previously verified from milestone.txt. Do not use tools or change files.").await.unwrap();
    completed(service.as_ref(), &id).await;
    let final_records = records(service.as_ref(), &id, resumed.epoch).await;
    let text = final_records
        .iter()
        .skip(initial.len())
        .filter_map(|record| record.value.as_ref())
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join("\n");
    assert!(text.contains("DSH_RESUMED") && text.contains("dsh-live-ok"));
    service.close(&id).await.unwrap();
    assert_reaped(&root);
    assert_pid_gone(&root.join("mcp-pid"));
    assert!(fiber.dispose().await.is_clean());
    assert!(runtime.shutdown().await.is_clean());
    let bytes = std::fs::read(root.join("journal/observed.sqlite3")).unwrap();
    let key = std::env::var("RSI_DSH_LIVE_KEY").unwrap();
    assert!(
        !bytes
            .windows(key.len())
            .any(|window| window == key.as_bytes()),
        "credential leaked into journal"
    );
    let result = json!({"source_revision":"ddefc45fbc7f8e46dd73185e68295696d1297887","model":"deepseek-flash","startup_ms":startup_ms,"elapsed_ms":began.elapsed().as_millis(),"mcp_calls":calls.lines().count(),"file_bytes":12,"resume_replay_records":0,"initial_records":initial.len(),"final_records":final_records.len(),"load_advertised":false,"processes_reaped":true});
    std::fs::write(
        root.join("result.json"),
        serde_json::to_vec_pretty(&result).unwrap(),
    )
    .unwrap();
    println!("{result}");
}
