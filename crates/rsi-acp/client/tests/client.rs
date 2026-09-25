use rsi_acp::{Peer, StreamTransport};
use rsi_acp_client::{Client, Setup};
use rsi_acp_journal::{Capabilities, ConversationId, Journal, Limits, Status};
use rsi_acp_protocol::Message;
use serde_json::json;
use std::time::Duration;

#[path = "client/configuration.rs"]
mod configuration;

async fn fixture(resume: bool) -> (tempfile::TempDir, Journal, Client, Peer) {
    let root = tempfile::tempdir().unwrap();
    let journal = Journal::open(root.path().to_owned(), Limits::default())
        .await
        .unwrap();
    let id = ConversationId::new("external-one").unwrap();
    journal
        .create(
            id.clone(),
            "fixture".into(),
            std::env::current_dir().unwrap().to_str().unwrap().into(),
        )
        .await
        .unwrap();
    let mut snapshot = journal.connect(&id).await.unwrap();
    if resume {
        snapshot = journal
            .bind(
                &id,
                snapshot.generation,
                "remote".into(),
                Capabilities::default(),
            )
            .await
            .unwrap();
    }
    let (left, right) = tokio::io::duplex(65536);
    let (read, write) = tokio::io::split(left);
    let client = Peer::start(StreamTransport::new(read, write));
    let (read, write) = tokio::io::split(right);
    let peer = Peer::start(StreamTransport::new(read, write));
    let client = Client::attach(client, journal.clone(), snapshot).unwrap();
    (root, journal, client, peer)
}

#[tokio::test]
async fn complete_replay_waits_for_journal_consumer_and_replaces_old_epoch() {
    let (_root, journal, client, mut peer) = fixture(true).await;
    let remote = tokio::spawn(async move {
        let port = peer.handle();
        while let Some(incoming) = peer.next().await {
            let Message::Request { id, method, .. } = incoming.message else {
                panic!("request")
            };
            match method.as_str() {
                "initialize" => port.respond(&id, Ok(&json!({"protocolVersion":1,"agentCapabilities":{"loadSession":true,"sessionCapabilities":{"close":{}}}}))).await.unwrap(),
                "session/load" => {
                    for index in 0..1200 {
                        port.notify("session/update", &json!({"sessionId":"remote","update":{"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":index.to_string()}}})).await.unwrap();
                    }
                    port.respond(&id, Ok(&json!({}))).await.unwrap();
                }
                "session/close" => { port.respond(&id, Ok(&json!({}))).await.unwrap(); break; }
                _ => panic!("unexpected method"),
            }
        }
        peer.close().await.unwrap();
    });
    let snapshot = client
        .initialize(Setup::Load, vec![], &[])
        .await
        .unwrap_or_else(|error| panic!("{error:?}; peer={:?}", client.handle().failure()));
    assert_eq!(snapshot.epoch, 2);
    let mut after = 0;
    let mut count = 0;
    loop {
        let page = journal
            .page(&snapshot.id, snapshot.epoch, after)
            .await
            .unwrap();
        count += page.records.len();
        after = page.records.last().unwrap().sequence;
        if !page.has_more {
            break;
        }
    }
    assert_eq!(count, 1200);
    let closed = client.close().await.unwrap();
    assert_eq!(closed.status, Status::Closed);
    tokio::time::timeout(Duration::from_secs(5), remote)
        .await
        .unwrap()
        .unwrap();
}

async fn ready(client: &Client, peer: &mut Peer) {
    let remote = async {
        let port = peer.handle();
        for method in ["initialize", "session/new"] {
            let Message::Request {
                id, method: actual, ..
            } = peer.next().await.unwrap().message
            else {
                panic!("request")
            };
            assert_eq!(method, actual);
            let result = if method == "initialize" {
                json!({"protocolVersion":1,"agentCapabilities":{}})
            } else {
                json!({"sessionId":"remote"})
            };
            port.respond(&id, Ok(&result)).await.unwrap();
        }
    };
    let (result, ()) = tokio::join!(client.initialize(Setup::New, vec![], &[]), remote);
    assert_eq!(result.unwrap().status, Status::Ready);
}

#[tokio::test]
async fn failed_load_preserves_visible_epoch_without_publishing_partial_replay() {
    let (_root, journal, client, mut peer) = fixture(true).await;
    let original = client.handle().snapshot();
    journal
        .append(
            &original.id,
            original.generation,
            rsi_acp_journal::RecordKind::User,
            json!({"prompt":[{"type":"text","text":"retained original"}]}),
        )
        .await
        .unwrap();
    let old = journal.page(&original.id, original.epoch, 0).await.unwrap();
    let remote = async {
        let port = peer.handle();
        let Message::Request { id, .. } = peer.next().await.unwrap().message else {
            panic!("initialize")
        };
        port.respond(
            &id,
            Ok(&json!({"protocolVersion":1,"agentCapabilities":{"loadSession":true}})),
        )
        .await
        .unwrap();
        let Message::Request { id, method, .. } = peer.next().await.unwrap().message else {
            panic!("load")
        };
        assert_eq!(method, "session/load");
        port.notify("session/update",&json!({"sessionId":"remote","update":{"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":"uncommitted replay"}}})).await.unwrap();
        port.respond(
            &id,
            Err(&json!({"code":-32603,"message":"fixture load rejected"})),
        )
        .await
        .unwrap();
    };
    let (result, ()) = tokio::join!(client.initialize(Setup::Load, vec![], &[]), remote);
    assert_eq!(result.unwrap_err(), rsi_acp_client::Error::Remote);
    let current = journal.get(&original.id).await.unwrap();
    assert_eq!(current.epoch, original.epoch);
    assert_eq!(
        serde_json::to_value(journal.page(&current.id, current.epoch, 0).await.unwrap()).unwrap(),
        serde_json::to_value(old).unwrap()
    );
    assert!(!client.handle().connected());
    client.close().await.unwrap();
    peer.close().await.unwrap();
}

#[tokio::test]
async fn unresponsive_cancel_has_a_deadline_and_never_claims_confirmed_settlement() {
    let (_root, journal, client, mut peer) = fixture(false).await;
    ready(&client, &mut peer).await;
    let handle = client.handle();
    handle
        .submit(vec![rsi_acp_protocol::schema::ContentBlock::Text(
            rsi_acp_protocol::schema::TextContent::new("held"),
        )])
        .await
        .unwrap();
    let Message::Request { method, .. } = peer.next().await.unwrap().message else {
        panic!("prompt")
    };
    assert_eq!(method, "session/prompt");
    tokio::time::pause();
    let cancel = handle.cancel();
    tokio::pin!(cancel);
    let incoming = tokio::select! {
        result=&mut cancel=>panic!("premature cancel: {result:?}"),
        incoming=peer.next()=>incoming.unwrap(),
    };
    assert!(
        matches!(incoming.message,Message::Notification{ref method,..}if method=="session/cancel")
    );
    tokio::time::advance(Duration::from_secs(31)).await;
    tokio::time::resume();
    assert_eq!(cancel.await.unwrap_err(), rsi_acp_client::Error::Unknown);
    observed(&handle, || !handle.connected()).await;
    assert_eq!(client.close().await.unwrap().status, Status::Unknown);
    assert_eq!(
        journal.get(&handle.snapshot().id).await.unwrap().status,
        Status::Unknown
    );
    assert!(peer.next().await.is_none());
    peer.close().await.unwrap();
}

async fn observed(handle: &rsi_acp_client::Handle, predicate: impl Fn() -> bool) {
    let mut changes = handle.observe();
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if predicate() {
                return;
            }
            changes.changed().await.unwrap();
        }
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn dropped_submission_waiter_retains_prompt_and_cancel_waits_for_remote_response() {
    use std::future::Future as _;
    let (_root, journal, client, mut peer) = fixture(false).await;
    ready(&client, &mut peer).await;
    let handle = client.handle();
    let prompt = vec![rsi_acp_protocol::schema::ContentBlock::Text(
        rsi_acp_protocol::schema::TextContent::new("work"),
    )];
    let mut submit = Box::pin(handle.submit(prompt));
    assert!(
        std::future::poll_fn(|cx| std::task::Poll::Ready(submit.as_mut().poll(cx)))
            .await
            .is_pending()
    );
    drop(submit);
    let Message::Request { id, method, .. } = peer.next().await.unwrap().message else {
        panic!("prompt")
    };
    assert_eq!(method, "session/prompt");
    assert_eq!(handle.snapshot().status, Status::Running);
    let cancellation = handle.cancel();
    tokio::pin!(cancellation);
    let incoming = tokio::select! {
        result = &mut cancellation => panic!("cancel returned before peer confirmation: {result:?}"),
        incoming = peer.next() => incoming.unwrap(),
    };
    assert!(
        matches!(incoming.message, Message::Notification { ref method, .. } if method == "session/cancel")
    );
    assert!(
        std::future::poll_fn(|cx| std::task::Poll::Ready(cancellation.as_mut().poll(cx)))
            .await
            .is_pending()
    );
    peer.handle()
        .respond(&id, Ok(&json!({"stopReason":"cancelled"})))
        .await
        .unwrap();
    let snapshot = cancellation.await.unwrap();
    assert_eq!(snapshot.status, Status::Cancelled);
    assert_eq!(
        snapshot.completion,
        Some(rsi_acp_journal::Completion::Cancelled)
    );
    assert_eq!(journal.get(&snapshot.id).await.unwrap(), snapshot);
    assert_eq!(client.close().await.unwrap().status, Status::Closed);
    assert!(peer.next().await.is_none());
    peer.close().await.unwrap();
}

#[tokio::test]
async fn all_four_peer_options_remain_exact_and_always_does_not_create_local_grant() {
    let (_root, _journal, client, mut peer) = fixture(false).await;
    ready(&client, &mut peer).await;
    let handle = client.handle();
    let generation = handle.snapshot().generation;
    for round in 0..2 {
        handle
            .submit(vec![rsi_acp_protocol::schema::ContentBlock::Text(
                rsi_acp_protocol::schema::TextContent::new("work"),
            )])
            .await
            .unwrap();
        let Message::Request { id, method, .. } = peer.next().await.unwrap().message else {
            panic!("prompt")
        };
        assert_eq!(method, "session/prompt");
        let params = json!({"sessionId":"remote","toolCall":{"toolCallId":format!("tool-{round}"),"title":"Requested tool"},"options":[
            {"optionId":"once-EXACT","name":"Once","kind":"allow_once"},
            {"optionId":"always-EXACT","name":"Always","kind":"allow_always"},
            {"optionId":"reject-EXACT","name":"Reject","kind":"reject_once"},
            {"optionId":"never-EXACT","name":"Never","kind":"reject_always"}
        ]});
        let port = peer.handle();
        let permission =
            tokio::spawn(async move { port.request_permission(&params).await.unwrap() });
        observed(&handle, || !handle.permissions().is_empty()).await;
        let choices = handle.permissions();
        assert_eq!(choices.len(), 1);
        assert_eq!(
            choices[0]
                .options
                .iter()
                .map(|option| option.kind.as_str())
                .collect::<Vec<_>>(),
            ["allow_once", "allow_always", "reject_once", "reject_always"]
        );
        assert_eq!(
            handle
                .answer(generation + 1, &choices[0].id, "always-EXACT")
                .await,
            Err(rsi_acp_client::Error::Stale)
        );
        assert_eq!(
            handle.answer(generation, &choices[0].id, "invented").await,
            Err(rsi_acp_client::Error::Input)
        );
        handle
            .answer(generation, &choices[0].id, "always-EXACT")
            .await
            .unwrap();
        let response = permission.await.unwrap();
        assert_eq!(
            response.result().unwrap()["outcome"]["optionId"],
            "always-EXACT"
        );
        assert_eq!(
            handle
                .answer(generation, &choices[0].id, "always-EXACT")
                .await,
            Err(rsi_acp_client::Error::Stale)
        );
        peer.handle()
            .respond(&id, Ok(&json!({"stopReason":"max_tokens"})))
            .await
            .unwrap();
        observed(&handle, || handle.snapshot().status == Status::Completed).await;
        assert_eq!(
            handle.snapshot().completion,
            Some(rsi_acp_journal::Completion::MaxTokens)
        );
    }
    client.close().await.unwrap();
    peer.close().await.unwrap();
}

#[tokio::test]
async fn peer_loss_keeps_unknown_and_never_resends_prompt() {
    let (_root, journal, client, mut peer) = fixture(false).await;
    ready(&client, &mut peer).await;
    let handle = client.handle();
    handle
        .submit(vec![rsi_acp_protocol::schema::ContentBlock::Text(
            rsi_acp_protocol::schema::TextContent::new("work"),
        )])
        .await
        .unwrap();
    let Message::Request { method, .. } = peer.next().await.unwrap().message else {
        panic!("prompt")
    };
    assert_eq!(method, "session/prompt");
    peer.close().await.unwrap();
    observed(&handle, || handle.snapshot().status == Status::Unknown).await;
    assert!(handle.submit(vec![]).await.is_err());
    let snapshot = client.close().await.unwrap();
    assert_eq!(snapshot.status, Status::Unknown);
    assert_eq!(
        journal.get(&snapshot.id).await.unwrap().status,
        Status::Unknown
    );
}

#[tokio::test]
async fn immediate_cancel_before_wire_admission_records_local_discard() {
    use std::future::Future as _;
    let (_root, _journal, client, mut peer) = fixture(false).await;
    ready(&client, &mut peer).await;
    let handle = client.handle();
    let mut submission =
        Box::pin(
            handle.submit(vec![rsi_acp_protocol::schema::ContentBlock::Text(
                rsi_acp_protocol::schema::TextContent::new("work"),
            )]),
        );
    assert!(
        std::future::poll_fn(|cx| std::task::Poll::Ready(submission.as_mut().poll(cx)))
            .await
            .is_pending()
    );
    // The owned task has been admitted but cannot run until this task yields.
    let snapshot = handle.cancel().await.unwrap();
    assert_eq!(snapshot.status, Status::Discarded);
    assert_eq!(snapshot.completion, None);
    submission.await.unwrap();
    client.close().await.unwrap();
    // No prompt or cancellation was put on the wire.
    assert!(peer.next().await.is_none());
    peer.close().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn setup_response_followed_by_eof_cannot_publish_ready_after_reader_settlement() {
    for iteration in 0..64 {
        let (_root, journal, client, mut peer) = fixture(false).await;
        let handle = client.handle();
        let remote = tokio::spawn(async move {
            let port = peer.handle();
            for method in ["initialize", "session/new"] {
                let Message::Request {
                    id, method: actual, ..
                } = peer.next().await.unwrap().message
                else {
                    panic!("request")
                };
                assert_eq!(actual, method);
                let value = if method == "initialize" {
                    json!({"protocolVersion":1,"agentCapabilities":{}})
                } else {
                    json!({"sessionId":"remote"})
                };
                port.respond(&id, Ok(&value)).await.unwrap();
            }
            peer.close().await.unwrap();
        });
        let _setup = client.initialize(Setup::New, vec![], &[]).await;
        remote.await.unwrap();
        observed(&handle, || !handle.connected()).await;
        // The reader owns EOF settlement, even when setup was already locally observed.
        observed(&handle, || handle.snapshot().status == Status::Unknown).await;
        assert_eq!(
            journal.get(&handle.snapshot().id).await.unwrap().status,
            Status::Unknown,
            "iteration {iteration}"
        );
        client.close().await.unwrap();
        journal.close().await.unwrap();
    }
}

#[tokio::test]
async fn failed_completion_journal_still_answers_pending_permissions() {
    let (_root, journal, client, mut peer) = fixture(false).await;
    ready(&client, &mut peer).await;
    let handle = client.handle();
    handle
        .submit(vec![rsi_acp_protocol::schema::ContentBlock::Text(
            rsi_acp_protocol::schema::TextContent::new("work"),
        )])
        .await
        .unwrap();
    let Message::Request { id, method, .. } = peer.next().await.unwrap().message else {
        panic!("prompt")
    };
    assert_eq!(method, "session/prompt");
    let port = peer.handle();
    let permission = tokio::spawn(async move {
        port.request_permission(&json!({"sessionId":"remote","toolCall":{"toolCallId":"pending","title":"Requested tool"},"options":[{"optionId":"once","name":"Once","kind":"allow_once"}]})).await
    });
    observed(&handle, || !handle.permissions().is_empty()).await;
    journal.close().await.unwrap();
    peer.handle()
        .respond(&id, Ok(&json!({"stopReason":"end_turn"})))
        .await
        .unwrap();
    let response = tokio::time::timeout(Duration::from_secs(2), permission)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(
        response.result().unwrap()["outcome"]["outcome"],
        "cancelled"
    );
    assert!(handle.permissions().is_empty());
    let _ = client.close().await;
    peer.close().await.unwrap();
}
