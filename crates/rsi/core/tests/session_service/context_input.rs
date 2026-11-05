use super::*;
use rsi_agent_session_protocol::{CommandArguments, DomainRequestId, SessionCommandInvocation};

async fn inject(
    handle: &Arc<dyn rsi_session_protocol::SessionHandle>,
    request: &str,
    text: &str,
    file: &rsi_media_protocol::FileRef,
) -> (
    SessionCommandInvocation,
    rsi_agent_session_protocol::SessionCommandReceipt,
) {
    invoke(handle, "context-input", request, serde_json::json!({
        "content":[{"type":"text","text":text},{"type":"file","file":file}],
        "attribution":{"id":format!("original:{request}"),"source":{"kind":"plugin:context","label":"来源"}}
    })).await
}

async fn command(
    handle: &Arc<dyn rsi_session_protocol::SessionHandle>,
    name: &str,
    request: &str,
    text: &str,
) -> (
    SessionCommandInvocation,
    rsi_agent_session_protocol::SessionCommandReceipt,
) {
    invoke(
        handle,
        name,
        request,
        if name == "context-input" {
            serde_json::json!({"content":[{"type":"text","text":text}],"attribution":{
                "id":format!("original:{request}"),"source":{"kind":"plugin:context","label":"来源"}
            }})
        } else {
            serde_json::json!({"text":text})
        },
    )
    .await
}

async fn invoke(
    handle: &Arc<dyn rsi_session_protocol::SessionHandle>,
    name: &str,
    request: &str,
    arguments: serde_json::Value,
) -> (
    SessionCommandInvocation,
    rsi_agent_session_protocol::SessionCommandReceipt,
) {
    let catalog = handle.commands().await.unwrap();
    let command = catalog
        .commands()
        .iter()
        .find(|command| command.name() == name)
        .unwrap();
    let invocation = SessionCommandInvocation {
        command: command.id().clone(),
        request_id: DomainRequestId::new(request).unwrap(),
        expected_revision: catalog.revision(),
        arguments: CommandArguments::new(arguments).unwrap(),
    };
    let receipt = handle.execute_command(invocation.clone()).await.unwrap();
    assert_eq!(
        handle.execute_command(invocation.clone()).await.unwrap(),
        receipt
    );
    (invocation, receipt)
}

async fn withdraw_draft(handle: &Arc<dyn rsi_session_protocol::SessionHandle>) {
    let (invocation, receipt) = command(
        handle,
        "context-input",
        "withdraw-draft",
        "DISCARDED_DRAFT_CONTEXT",
    )
    .await;
    let before = handle.draft_snapshot().await.unwrap();
    let request = rsi_session_protocol::DiscardDraftInputs {
        message_ids: vec![before.command_inputs[0].message.message_id.clone()],
    };
    let after = handle.discard_draft_inputs(request.clone()).await.unwrap();
    assert_eq!(after.header, before.header);
    assert_eq!(after.revision, before.revision + 1);
    assert!(after.command_inputs.is_empty());
    assert_eq!(handle.discard_draft_inputs(request).await.unwrap(), after);
    assert_eq!(handle.execute_command(invocation).await.unwrap(), receipt);
    assert_eq!(
        handle.draft_snapshot().await.unwrap(),
        after,
        "command retry cannot resurrect withdrawn context"
    );
}

async fn withdraw_durable(
    handle: &Arc<dyn rsi_session_protocol::SessionHandle>,
) -> rsi_agent_session_protocol::QueueMutationReceipt {
    use rsi_agent_session_protocol::{QueueMutation, QueueMutationRequest, QueueOperationId};
    let (invocation, command_receipt) = command(
        handle,
        "context-input",
        "withdraw-durable",
        "DISCARDED_DURABLE_CONTEXT",
    )
    .await;
    let snapshot = handle.inspect().await.unwrap();
    let input = &snapshot.pending[0];
    assert_eq!(
        input.source_kind,
        rsi_agent_session_protocol::AgentMessageSourceKind::Command
    );
    let request = QueueMutationRequest {
        operation_id: QueueOperationId::new("withdraw-command-input").unwrap(),
        slot_id: input.queue_slot.id.clone(),
        expected_message_id: input.message_id.clone(),
        mutation: QueueMutation::Withdraw,
    };
    let receipt = handle.mutate_queue(request.clone()).await.unwrap();
    assert_eq!(
        receipt.outcome,
        rsi_agent_session_protocol::QueueMutationOutcome::Withdrawn
    );
    assert_eq!(handle.mutate_queue(request).await.unwrap(), receipt);
    assert_eq!(
        handle.execute_command(invocation).await.unwrap(),
        command_receipt
    );
    assert!(handle.inspect().await.unwrap().pending.is_empty());
    receipt
}

async fn held_input_id(
    handle: &Arc<dyn rsi_session_protocol::SessionHandle>,
) -> rsi_agent_session_protocol::MessageId {
    let draft = handle.draft_snapshot().await.unwrap();
    assert_eq!(draft.command_inputs.len(), 1);
    assert!(draft.validate().is_ok());
    let input = &draft.command_inputs[0];
    assert_eq!(
        input.delivery,
        rsi_agent_session_protocol::MessageDelivery::NextStep
    );
    let rsi_agent_session_protocol::AgentMessageSource::Command { source } = &input.message.source
    else {
        panic!("draft command input changed its source")
    };
    assert_eq!(source.request_id.as_str(), "draft-boundary");
    assert_eq!(
        source.attribution.as_ref().unwrap().id,
        "original:draft-boundary"
    );
    assert_eq!(handle.draft_snapshot().await.unwrap(), draft);
    input.message.message_id.clone()
}

fn file_fixture(fixture: &Fixture) -> (rsi_media_protocol::FileRef, std::path::PathBuf) {
    use std::fmt::Write as _;
    let file_root = fixture.temporary.path().join("context-files");
    let digest = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";
    let object = file_root
        .join("files")
        .join("e3")
        .join(digest)
        .join("context.txt");
    std::fs::create_dir_all(object.parent().unwrap()).unwrap();
    std::fs::write(&object, []).unwrap();
    let file = rsi_media_protocol::FileRef::new(
        rsi_media_protocol::MediaId::new(digest).unwrap(),
        "context.txt",
        0,
    )
    .unwrap();
    let mut profile = std::fs::read_to_string(&fixture.profile).unwrap();
    write!(profile, "\n[[steps]]\nkind = \"plugin\"\nid = \"context-files\"\nplugin = \"rsi.media.attachments.local\"\nconfig = {{ root = {} }}\n", serde_json::to_string(&file_root).unwrap()).unwrap();
    std::fs::write(&fixture.profile, profile).unwrap();
    (file, object)
}

async fn verify_file_admission_and_receipt_retry(
    handle: &Arc<dyn rsi_session_protocol::SessionHandle>,
    file: &rsi_media_protocol::FileRef,
    object: &std::path::Path,
) {
    let before = handle.draft_snapshot().await.unwrap();
    let catalog = handle.commands().await.unwrap();
    let invalid = SessionCommandInvocation {
        command: catalog
            .commands()
            .iter()
            .find(|command| command.name() == "context-input")
            .unwrap()
            .id()
            .clone(),
        request_id: DomainRequestId::new("missing-file").unwrap(),
        expected_revision: catalog.revision(),
        arguments: CommandArguments::new(serde_json::json!({"content":[{"type":"file","file":{
            "id":file.id(),"name":"missing.txt","bytes":0
        }}]}))
        .unwrap(),
    };
    assert!(handle.execute_command(invalid).await.is_err());
    assert_eq!(handle.draft_snapshot().await.unwrap(), before);
    let (accepted, receipt) = inject(
        handle,
        "draft-boundary",
        "Side conversation boundary: first context",
        file,
    )
    .await;
    std::fs::remove_file(object).unwrap();
    assert_eq!(
        handle.execute_command(accepted).await.unwrap(),
        receipt,
        "receipt retries do not read the accepted file again"
    );
    std::fs::write(object, []).unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn context_input_is_non_waking_and_enters_once_before_the_human_question() {
    let requests = Arc::new(Mutex::new(Vec::new()));
    let received = requests.clone();
    let app = Router::new().route("/v1/chat/completions", post(move |Json(body): Json<serde_json::Value>| {
        received.lock().unwrap().push(body);
        async {
            Response::builder().status(200).header("content-type", "text/event-stream").body(Body::from(
                "data: {\"choices\":[{\"delta\":{\"role\":\"assistant\",\"content\":\"done\"},\"finish_reason\":null}]}\n\ndata: {\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}]}\n\ndata: [DONE]\n\n"
            )).unwrap()
        }
    }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let fixture = fixture(&format!("http://{}", listener.local_addr().unwrap()));
    let (file, object) = file_fixture(&fixture);
    let server = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    let running = RunningRsi::boot(composition(fixture.paths.clone()), &fixture.profile)
        .await
        .unwrap();
    let handle = running
        .session_service()
        .unwrap()
        .create(CreateSession {
            fork: None,
            workspace_id: running
                .workspace_registry()
                .unwrap()
                .get_or_create(&fixture.workspace)
                .await
                .unwrap()
                .id,
            session_id: SessionId::new("context-input").unwrap(),
            agent_preset_id: None,
        })
        .await
        .unwrap();
    withdraw_draft(&handle).await;
    verify_file_admission_and_receipt_retry(&handle, &file, &object).await;
    assert!(requests.lock().unwrap().is_empty());
    let draft_message_id = held_input_id(&handle).await;
    command(
        &handle,
        "context-seed",
        "draft-seed",
        &format!("{} Initial open-turn reference", "初始参考".repeat(2000)),
    )
    .await;
    assert_eq!(
        held_input_id(&handle).await,
        draft_message_id,
        "seed is not pending inbox work"
    );
    run_message_with_text_to_terminal(&handle, "first-question", "first-question").await;
    let entered = handle.history_before(None, 128).await.unwrap();
    assert!(entered.facts.iter().any(
        |fact| matches!(fact.body(), SessionFactBody::InputMessageClaimed {
        source: rsi_agent_session_protocol::InputMessageSource::Command {message_id,..},..
    } if message_id == &draft_message_id)
    ));
    let withdrawal = withdraw_durable(&handle).await;
    inject(
        &handle,
        "held-boundary",
        "Held context after publication",
        &file,
    )
    .await;
    command(
        &handle,
        "context-seed",
        "durable-seed",
        "Retained supplemental reference",
    )
    .await;
    assert!(running.shutdown().await.is_clean());
    drop(handle);
    assert_eq!(requests.lock().unwrap().len(), 1);
    let running = RunningRsi::boot(composition(fixture.paths.clone()), &fixture.profile)
        .await
        .unwrap();
    let handle = running
        .session_service()
        .unwrap()
        .attach(&SessionId::new("context-input").unwrap())
        .await
        .unwrap();
    assert_eq!(requests.lock().unwrap().len(), 1);
    run_message_with_text_to_terminal(&handle, "second-question", "second-question").await;
    assert_eq!(
        handle
            .queue_mutation_status(&withdrawal.operation_id)
            .await
            .unwrap(),
        Some(withdrawal)
    );
    assert_context_requests(&requests.lock().unwrap());
    assert_context_history(&handle).await;
    assert!(running.shutdown().await.is_clean());
    server.abort();
}

fn assert_context_requests(requests: &[serde_json::Value]) {
    assert_eq!(requests.len(), 2);
    assert!(
        !serde_json::to_string(requests)
            .unwrap()
            .contains("DISCARDED_")
    );
    assert_eq!(
        requests[0]["messages"]
            .to_string()
            .matches("Initial open-turn reference")
            .count(),
        1
    );
    let resumed = requests[1]["messages"].to_string();
    assert_eq!(resumed.matches("Initial open-turn reference").count(), 1);
    assert_eq!(
        resumed.matches("Retained supplemental reference").count(),
        1
    );
    for (index, context, question) in [
        (
            0,
            "Side conversation boundary: first context",
            "first-question",
        ),
        (1, "Held context after publication", "second-question"),
    ] {
        let messages = requests[index]["messages"].to_string();
        assert_eq!(
            messages.matches("context.txt").count(),
            2 * (index + 1),
            "each file is a name and a local path, including after restart"
        );
        assert_eq!(messages.matches(context).count(), 1);
        assert!(messages.find(context).unwrap() < messages.find(question).unwrap());
    }
}

async fn assert_context_history(handle: &Arc<dyn rsi_session_protocol::SessionHandle>) {
    let facts = handle.history_before(None, 128).await.unwrap();
    for fact in &facts.facts {
        if let SessionFactBody::InputMessageClaimed {
            source: rsi_agent_session_protocol::InputMessageSource::Command { source, .. },
            ..
        } = fact.body()
        {
            let attribution = source.attribution.as_ref().unwrap();
            assert_eq!(
                attribution.id,
                format!("original:{}", source.request_id.as_str())
            );
            assert_eq!(
                attribution.source,
                serde_json::json!({"kind":"plugin:context","label":"来源"})
            );
        }
    }
    assert_eq!(facts.facts.iter().filter(|fact| matches!(fact.body(),
        SessionFactBody::InputMessageClaimed {
            source: rsi_agent_session_protocol::InputMessageSource::PluginContext { contribution_id, .. }, ..
        } if contribution_id.as_str() == "rsi.context-seed.context"
    )).count(), 2, "each seed enters once, including across native restart");
    assert_eq!(
        facts
            .facts
            .iter()
            .filter(|fact| matches!(
                fact.body(),
                SessionFactBody::InputMessageClaimed {
                    source: rsi_agent_session_protocol::InputMessageSource::Command { .. },
                    ..
                }
            ))
            .count(),
        2
    );
}
