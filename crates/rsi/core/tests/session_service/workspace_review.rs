use super::*;
use rsi_workspace_review_api::{Client, ConversationIdentity, Phase, Reply, Request, Scope};
use std::sync::atomic::{AtomicUsize, Ordering};
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
fn git(root: &std::path::Path, args: &[&str]) {
    let status = std::process::Command::new("/usr/bin/git")
        .current_dir(root)
        .args(args)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .status()
        .unwrap();
    assert!(status.success());
}
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[expect(
    clippy::too_many_lines,
    reason = "one public workflow retains exact source identities through mutation, retirement and cold recovery"
)]
async fn workspace_review_api_waits_for_real_shell_work_and_preserves_summaries_but_expires_diffs_after_restart()
 {
    let gate = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let command = format!(
        "printf 'after\\n' > tracked.txt; printf 'shell-created\\n' > shell.txt; mv rename.txt renamed.txt; rm deleted.txt; exec 3<>/dev/tcp/127.0.0.1/{}; printf ready >&3; IFS= read -r -u 3; printf 'settled\\n' >> shell.txt",
        gate.local_addr().unwrap().port()
    );
    let count = Arc::new(AtomicUsize::new(0));
    let requests = count.clone();
    let app=Router::new().route("/v1/chat/completions",post(move||{let count=count.clone();let command=command.clone();async move{let first=count.fetch_add(1,Ordering::SeqCst)==0;let delta=if first{serde_json::json!({"choices":[{"delta":{"role":"assistant","tool_calls":[{"index":0,"id":"review-bash","type":"function","function":{"name":"bash","arguments":serde_json::json!({"command":command,"timeout_ms":15000}).to_string()}}]},"finish_reason":null}]})}else{serde_json::json!({"choices":[{"delta":{"role":"assistant","content":"review complete"},"finish_reason":null}]})};let finish=serde_json::json!({"choices":[{"delta":{},"finish_reason":if first{"tool_calls"}else{"stop"}}]});Response::builder().header("content-type","text/event-stream").body(Body::from(format!("data: {delta}\n\ndata: {finish}\n\ndata: [DONE]\n\n"))).unwrap()}}));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let fixture = fixture(&format!("http://{}", listener.local_addr().unwrap()));
    let server = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    git(&fixture.workspace, &["init", "--quiet"]);
    git(&fixture.workspace, &["config", "user.name", "Review"]);
    git(
        &fixture.workspace,
        &["config", "user.email", "review@example.invalid"],
    );
    for (name, text) in [
        ("tracked.txt", "committed\n"),
        ("rename.txt", "same content\n"),
        ("deleted.txt", "deleted content\n"),
    ] {
        std::fs::write(fixture.workspace.join(name), text).unwrap();
    }
    git(&fixture.workspace, &["add", "."]);
    git(&fixture.workspace, &["commit", "-qm", "initial"]);
    std::fs::write(fixture.workspace.join("tracked.txt"), "dirty-before\n").unwrap();
    let index = std::fs::read(fixture.workspace.join(".git/index")).unwrap();
    let head = std::fs::read(fixture.workspace.join(".git/HEAD")).unwrap();
    let daemon = DaemonFixture::new(&fixture).await;
    let client = Client::new(daemon.connection.api_client()).unwrap();
    let settings = daemon.running.settings_access().unwrap();
    let current = settings.read("rsi.agent").await.unwrap();
    let mut value = current.value.clone();
    value["sandbox"] = "danger-full-access".into();
    value["require_approval"] = true.into();
    settings
        .replace("rsi.agent", &current.version(), value)
        .await
        .unwrap();
    drop(settings);
    let workspace = daemon
        .connection
        .workspace_registry()
        .get_or_create(&fixture.workspace)
        .await
        .unwrap();
    let scope = Scope {
        workspace: workspace.id.clone(),
        conversation: ConversationIdentity::Native(SessionId::new("review-source").unwrap()),
    };
    let handle = daemon
        .connection
        .session_service()
        .create(CreateSession {
            workspace_id: workspace.id,
            session_id: SessionId::new("review-source").unwrap(),
            agent_preset_id: None,
        })
        .await
        .unwrap();
    let mut interactions = handle.observe_interactions().await.unwrap();
    let turn = tokio::spawn({
        let handle = handle.clone();
        async move {
            run_message_to_terminal(&handle, "review-turn").await;
        }
    });
    let approval = tokio::time::timeout(std::time::Duration::from_secs(8), async {
        loop {
            let snapshot = interactions.next().await.unwrap().unwrap();
            if let Some(approval) = snapshot.approvals().first() {
                break approval.clone();
            }
        }
    })
    .await
    .unwrap();
    handle
        .answer_approval(
            &SessionId::new(approval.subject.session_id()).unwrap(),
            &approval.id,
            rsi_approval_protocol::ApprovalDecision::AllowOnce,
        )
        .await
        .unwrap();
    drop(interactions);
    let (mut socket, _) = tokio::time::timeout(std::time::Duration::from_secs(10), gate.accept())
        .await
        .unwrap()
        .unwrap();
    let mut ready = [0; 5];
    socket.read_exact(&mut ready).await.unwrap();
    assert_eq!(&ready, b"ready");
    let Reply::Summaries { items, .. } = client
        .call(Request::List {
            scope: scope.clone(),
            after: None,
        })
        .await
        .unwrap()
    else {
        panic!("summaries")
    };
    assert_eq!(items.len(), 1);
    assert_eq!(items[0].phase, Phase::Pending);
    let id = items[0].id.clone();
    assert!(
        client
            .call(Request::Files {
                scope: scope.clone(),
                id: id.clone(),
                offset: 0
            })
            .await
            .is_err(),
        "unsettled work cannot expose a final comparison"
    );
    std::fs::write(
        fixture.workspace.join("foreign.txt"),
        "concurrent unrelated writer\n",
    )
    .unwrap();
    socket.write_all(b"release\n").await.unwrap();
    drop(socket);
    turn.await.unwrap();
    let summary = tokio::time::timeout(std::time::Duration::from_secs(10), async {
        loop {
            let Reply::Summaries { items, .. } = client
                .call(Request::List {
                    scope: scope.clone(),
                    after: None,
                })
                .await
                .unwrap()
            else {
                panic!("summaries")
            };
            if items[0].phase != Phase::Pending {
                break items[0].clone();
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert_eq!(summary.phase, Phase::Complete, "{summary:?}");
    assert_eq!(summary.changed_files, 5);
    let Reply::Files { files, .. } = client
        .call(Request::Files {
            scope: scope.clone(),
            id: id.clone(),
            offset: 0,
        })
        .await
        .unwrap()
    else {
        panic!("files")
    };
    assert_eq!(files.len(), 5);
    assert!(files.iter().any(|f| f.path == "foreign.txt"));
    let diff_request = Request::Diff {
        scope: scope.clone(),
        id: id.clone(),
        path: "tracked.txt".into(),
        offset: 0,
    };
    let Reply::Diff { text, .. } = client.call(diff_request.clone()).await.unwrap() else {
        panic!("diff")
    };
    assert!(text.contains("-dirty-before"));
    assert!(text.contains("+after"));
    assert!(!text.contains("committed"));
    let Reply::Diff { text, .. } = client
        .call(Request::Diff {
            scope: scope.clone(),
            id: id.clone(),
            path: "shell.txt".into(),
            offset: 0,
        })
        .await
        .unwrap()
    else {
        panic!("shell diff")
    };
    assert!(text.contains("+settled"));
    let other = fixture.temporary.path().join("foreign-review-workspace");
    std::fs::create_dir(&other).unwrap();
    let other = daemon
        .connection
        .workspace_registry()
        .get_or_create(&other)
        .await
        .unwrap();
    assert!(
        client
            .call(Request::List {
                scope: Scope {
                    workspace: other.id,
                    conversation: scope.conversation.clone()
                },
                after: None
            })
            .await
            .is_err()
    );
    assert_eq!(
        std::fs::read(fixture.workspace.join(".git/index")).unwrap(),
        index
    );
    assert_eq!(
        std::fs::read(fixture.workspace.join(".git/HEAD")).unwrap(),
        head
    );
    assert_eq!(requests.load(Ordering::SeqCst), 2);
    drop((client, handle));
    daemon.shutdown().await;
    let daemon = DaemonFixture::new(&fixture).await;
    let client = Client::new(daemon.connection.api_client()).unwrap();
    let Reply::Summaries { epoch, items, .. } = client
        .call(Request::List { scope, after: None })
        .await
        .unwrap()
    else {
        panic!("summaries")
    };
    assert_eq!(items, vec![summary]);
    assert_ne!(epoch, items[0].epoch);
    assert!(
        matches!(client.call(diff_request).await.unwrap(),Reply::Expired{id:expired} if expired==id)
    );
    drop(client);
    daemon.shutdown().await;
    server.abort();
}
