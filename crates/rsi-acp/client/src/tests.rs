use super::*;
use rsi_acp::{Peer, StreamTransport};
use rsi_acp_journal::{Capabilities, Completion, Limits};
use serde_json::json;
use std::{
    future::Future,
    pin::Pin,
    task::{Context, Poll, Waker},
};
use tokio::io::{AsyncWrite, DuplexStream};

#[derive(Default)]
struct Gate {
    blocked: AtomicBool,
    close_fails: AtomicBool,
    entered: tokio::sync::Notify,
    writer: Mutex<Option<Waker>>,
}
impl Gate {
    fn release(&self) {
        self.blocked.store(false, Ordering::Release);
        if let Some(waker) = self.writer.lock().unwrap().take() {
            waker.wake();
        }
    }
}
struct Writer(tokio::io::WriteHalf<DuplexStream>, Arc<Gate>);
impl AsyncWrite for Writer {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        bytes: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        {
            let mut waker = self.1.writer.lock().unwrap();
            if self.1.blocked.load(Ordering::Acquire) {
                *waker = Some(cx.waker().clone());
                self.1.entered.notify_one();
                return Poll::Pending;
            }
        }
        Pin::new(&mut self.0).poll_write(cx, bytes)
    }
    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.0).poll_flush(cx)
    }
    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        if self.1.close_fails.load(Ordering::Acquire) {
            return Poll::Ready(Err(std::io::Error::other("fixture close failure")));
        }
        Pin::new(&mut self.0).poll_shutdown(cx)
    }
}

#[tokio::test]
async fn completed_prompt_survives_transport_cleanup_failure() {
    let (_root, client, peer, gate) = fixture().await;
    let state = client.state.clone();
    state
        .transition(Transition::Complete(Completion::EndTurn))
        .await
        .unwrap();
    gate.close_fails.store(true, Ordering::Release);
    assert!(matches!(client.close().await, Err(Error::Unknown)));
    assert_eq!(state.snapshot().status, Status::Completed);
    assert_eq!(
        state.journal.get(&state.id).await.unwrap().completion,
        Some(Completion::EndTurn)
    );
    peer.close().await.unwrap();
}

#[tokio::test]
async fn validated_completion_survives_permission_cleanup_rejection() {
    let (_root, client, mut peer, gate) = fixture().await;
    let state = client.state.clone();
    state.bind_target("remote").unwrap();
    state
        .transition(Transition::Bind("remote".into(), Capabilities::default()))
        .await
        .unwrap();
    let handle = client.handle();
    handle
        .submit(vec![rsi_acp_protocol::schema::ContentBlock::Text(
            rsi_acp_protocol::schema::TextContent::new("hello"),
        )])
        .await
        .unwrap();
    let rsi_acp_protocol::Message::Request { id, .. } = peer.next().await.unwrap().message else {
        panic!("prompt")
    };
    let port = peer.handle();
    let permission = tokio::spawn(async move {
        port.request_permission(&json!({"sessionId":"remote","toolCall":{"toolCallId":"t","title":"Read"},"options":[{"optionId":"exact","name":"Allow","kind":"allow_once"}]})).await
    });
    let mut changed = handle.observe();
    while handle.permissions().is_empty() {
        changed.changed().await.unwrap();
    }
    gate.blocked.store(true, Ordering::Release);
    queue_notification(&state).await;
    gate.entered.notified().await;
    for _ in 0..rsi_acp_protocol::MAX_PENDING {
        queue_notification(&state).await;
    }
    peer.handle()
        .respond(&id, Ok(&json!({"stopReason":"end_turn"})))
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(2), state.stop.cancelled())
        .await
        .unwrap();
    let operation = state.operation.lock().await;
    assert_eq!(state.snapshot().status, Status::Completed);
    assert_eq!(
        state.journal.get(&state.id).await.unwrap().completion,
        Some(Completion::EndTurn)
    );
    drop(operation);
    gate.release();
    client.close().await.unwrap();
    let _ = permission.await.unwrap();
    peer.close().await.unwrap();
}

#[tokio::test]
async fn prompt_response_is_observed_while_cancel_notification_is_blocked() {
    let (_root, client, mut peer, gate) = fixture().await;
    let state = client.state.clone();
    state.bind_target("remote").unwrap();
    state
        .transition(Transition::Bind("remote".into(), Capabilities::default()))
        .await
        .unwrap();
    client
        .handle()
        .submit(vec![rsi_acp_protocol::schema::ContentBlock::Text(
            rsi_acp_protocol::schema::TextContent::new("hello"),
        )])
        .await
        .unwrap();
    let rsi_acp_protocol::Message::Request { id, .. } = peer.next().await.unwrap().message else {
        panic!("prompt")
    };
    gate.blocked.store(true, Ordering::Release);
    state.active.lock().unwrap().as_ref().unwrap().cancel();
    gate.entered.notified().await; // cancellation write reached the transport
    peer.handle()
        .respond(&id, Ok(&json!({"stopReason":"cancelled"})))
        .await
        .unwrap();
    let operation = tokio::time::timeout(Duration::from_secs(2), state.operation.lock())
        .await
        .expect("completion must not wait on the blocked cancellation write");
    assert_eq!(state.snapshot().completion, Some(Completion::Cancelled));
    drop(operation);
    gate.release();
    client.close().await.unwrap();
    peer.close().await.unwrap();
}
async fn fixture() -> (tempfile::TempDir, Client, Peer, Arc<Gate>) {
    let root = tempfile::tempdir().unwrap();
    let journal = Journal::open(root.path().into(), Limits::default())
        .await
        .unwrap();
    let id = ConversationId::new("ordering").unwrap();
    journal
        .create(id.clone(), "fixture".into(), "/workspace".into())
        .await
        .unwrap();
    let snapshot = journal.connect(&id).await.unwrap();
    let (a, b) = tokio::io::duplex(65536);
    let (read, write) = tokio::io::split(a);
    let gate = Arc::new(Gate::default());
    let local = Peer::start(StreamTransport::new(read, Writer(write, gate.clone())));
    let (read, write) = tokio::io::split(b);
    (
        root,
        Client::attach(local, journal, snapshot).unwrap(),
        Peer::start(StreamTransport::new(read, write)),
        gate,
    )
}

#[tokio::test]
async fn transition_owner_orders_commit_and_publication_after_waiter_drop() {
    for completion_first in [false, true] {
        let (_root, client, peer, _) = fixture().await;
        let state = client.state.clone();
        state.status(Status::Running).await.unwrap();
        let (committed, commit) = tokio::sync::oneshot::channel();
        let (publish, publication) = tokio::sync::oneshot::channel();
        *state.publication_hook.lock().unwrap() = Some(transitions::PublicationHook {
            committed,
            publish: publication,
        });
        let owner = state.clone();
        let first = tokio::spawn(async move {
            owner
                .transition(if completion_first {
                    Transition::Complete(Completion::EndTurn)
                } else {
                    Transition::Disconnected
                })
                .await
        });
        commit.await.unwrap();
        assert_eq!(state.snapshot().status, Status::Running);
        first.abort();
        let _ = first.await;
        let owner = state.clone();
        let second = tokio::spawn(async move {
            owner
                .transition(if completion_first {
                    Transition::Disconnected
                } else {
                    Transition::Complete(Completion::EndTurn)
                })
                .await
        });
        publish.send(()).unwrap();
        second.await.unwrap().unwrap();
        let snapshot = state.snapshot();
        assert_eq!(snapshot.status, Status::Completed);
        assert_eq!(snapshot.completion, Some(Completion::EndTurn));
        assert_eq!(
            serde_json::to_value(&snapshot).unwrap(),
            serde_json::to_value(state.journal.get(&state.id).await.unwrap()).unwrap()
        );
        state.status(Status::Unknown).await.unwrap(); // late cancellation cannot overwrite completion
        assert_eq!(state.snapshot().status, Status::Completed);
        client.close().await.unwrap();
        peer.close().await.unwrap();
    }
}

#[tokio::test]
async fn setup_commit_rechecks_connection_before_publication() {
    let (_root, client, peer, _) = fixture().await;
    let state = client.state.clone();
    let (committed, commit) = tokio::sync::oneshot::channel();
    let (publish, publication) = tokio::sync::oneshot::channel();
    *state.publication_hook.lock().unwrap() = Some(transitions::PublicationHook {
        committed,
        publish: publication,
    });
    let owner = state.clone();
    let bind = tokio::spawn(async move {
        owner
            .transition(Transition::Bind(
                "remote".into(),
                rsi_acp_journal::Capabilities::default(),
            ))
            .await
    });
    commit.await.unwrap();
    state.stop.cancel();
    publish.send(()).unwrap();
    assert!(matches!(bind.await.unwrap(), Err(Error::Unknown)));
    client.close().await.unwrap();
    assert_eq!(state.snapshot().status, Status::Unknown);
    assert_eq!(
        serde_json::to_value(state.snapshot()).unwrap(),
        serde_json::to_value(state.journal.get(&state.id).await.unwrap()).unwrap()
    );
    peer.close().await.unwrap();
}

async fn queue_notification(state: &State) {
    let value = json!({});
    let future = state.port.notify("fixture", &value);
    tokio::pin!(future);
    std::future::poll_fn(|cx| {
        assert!(future.as_mut().poll(cx).is_pending());
        Poll::Ready(())
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn dropped_setup_waiter_retains_operation_and_close_joins_publication() {
    let (_root, client, mut peer, _) = fixture().await;
    let state = client.state.clone();
    let client = Arc::new(client);
    let (committed, commit) = tokio::sync::oneshot::channel();
    let (publish, publication) = tokio::sync::oneshot::channel();
    *state.publication_hook.lock().unwrap() = Some(transitions::PublicationHook {
        committed,
        publish: publication,
    });
    let caller = client.clone();
    let setup = tokio::spawn(async move { caller.initialize(Setup::New, vec![], &[]).await });
    for expected in ["initialize", "session/new"] {
        let rsi_acp_protocol::Message::Request { id, method, .. } =
            peer.next().await.unwrap().message
        else {
            panic!("setup request")
        };
        assert_eq!(method, expected);
        let result = if method == "initialize" {
            json!({"protocolVersion":1,"agentCapabilities":{}})
        } else {
            json!({"sessionId":"remote"})
        };
        peer.handle().respond(&id, Ok(&result)).await.unwrap();
    }
    commit.await.unwrap();
    setup.abort();
    let _ = setup.await;
    assert!(matches!(
        client.initialize(Setup::New, vec![], &[]).await,
        Err(Error::Busy)
    ));
    let client = Arc::try_unwrap(client).unwrap();
    let close = client.close();
    tokio::pin!(close);
    std::future::poll_fn(|cx| {
        assert!(close.as_mut().poll(cx).is_pending());
        Poll::Ready(())
    })
    .await;
    tokio::time::advance(std::time::Duration::from_secs(31)).await;
    std::future::poll_fn(|cx| {
        assert!(
            close.as_mut().poll(cx).is_pending(),
            "close must retain durable publication past its control deadline"
        );
        Poll::Ready(())
    })
    .await;
    publish.send(()).unwrap();
    assert_eq!(close.await.unwrap().status, Status::Unknown);
    assert_eq!(
        serde_json::to_value(state.snapshot()).unwrap(),
        serde_json::to_value(state.journal.get(&state.id).await.unwrap()).unwrap()
    );
    peer.close().await.unwrap();
}

#[tokio::test]
async fn partial_permission_cancellation_failure_retires_all_remaining_choices() {
    let (_root, client, peer, gate) = fixture().await;
    *client.state.active.lock().unwrap() = Some(CancellationToken::new());
    let mut requests = Vec::new();
    for id in 0..3 {
        let port = peer.handle();
        requests.push(tokio::spawn(async move { port.request_permission(&json!({"sessionId":"remote","toolCall":{"toolCallId":format!("t-{id}"),"title":"Read"},"options":[{"optionId":"exact","name":"Allow","kind":"allow_once"}]})).await }));
    }
    let handle = client.handle();
    let mut changed = handle.observe();
    while handle.permissions().len() < 3 {
        changed.changed().await.unwrap();
    }
    gate.blocked.store(true, Ordering::Release);
    queue_notification(&client.state).await;
    gate.entered.notified().await;
    for _ in 1..rsi_acp_protocol::MAX_PENDING {
        queue_notification(&client.state).await;
    }
    assert_eq!(
        incoming::cancel_permissions(&client.state).await,
        Err(Error::Unknown)
    );
    assert!(!handle.connected());
    assert!(handle.permissions().is_empty());
    gate.release();
    client.close().await.unwrap();
    for request in requests {
        let _ = request.await.unwrap();
    }
    peer.close().await.unwrap();
}

#[tokio::test]
async fn saturated_reply_queue_keeps_permission_and_dropped_answer_waiter_finishes_once() {
    let (_root, client, mut peer, gate) = fixture().await;
    *client.state.active.lock().unwrap() = Some(CancellationToken::new());
    let handle = client.handle();
    let port = peer.handle();
    let response = tokio::spawn(async move {
        port.request_permission(&json!({"sessionId":"remote","toolCall":{"toolCallId":"t","title":"Read"},"options":[{"optionId":"exact","name":"Allow","kind":"allow_once"}]})).await.unwrap()
    });
    let mut changed = handle.observe();
    while handle.permissions().is_empty() {
        changed.changed().await.unwrap();
    }
    let permission = handle.permissions()[0].clone();
    gate.blocked.store(true, Ordering::Release);
    queue_notification(&client.state).await;
    gate.entered.notified().await;
    for _ in 0..rsi_acp_protocol::MAX_PENDING {
        queue_notification(&client.state).await;
    }
    assert_eq!(
        handle
            .answer(client.state.generation, &permission.id, "exact")
            .await,
        Err(Error::Busy)
    );
    assert_eq!(handle.permissions().len(), 1);
    assert!(handle.connected());
    gate.release();
    client.state.port.drain().await.unwrap();
    gate.blocked.store(true, Ordering::Release);
    {
        let answer = handle.answer(client.state.generation, &permission.id, "exact");
        tokio::pin!(answer);
        std::future::poll_fn(|cx| {
            assert!(answer.as_mut().poll(cx).is_pending());
            Poll::Ready(())
        })
        .await;
        assert!(handle.permissions().is_empty());
    } // relinquish observation after admission, without aborting the flush owner
    gate.release();
    assert_eq!(
        response.await.unwrap().result().unwrap()["outcome"]["optionId"],
        "exact"
    );
    assert_eq!(
        handle
            .answer(client.state.generation, &permission.id, "exact")
            .await,
        Err(Error::Stale)
    );
    // Drain test-only notifications so the peer's retained inputs can be released.
    while peer.try_next().is_some() {}
    client.close().await.unwrap();
    peer.close().await.unwrap();
}

#[tokio::test]
async fn permission_timeout_retires_core_writer_even_when_client_reader_is_blocked() {
    let (_root, client, peer, gate) = fixture().await;
    *client.state.active.lock().unwrap() = Some(CancellationToken::new());
    let state = client.state.clone();
    let handle = client.handle();
    let port = peer.handle();
    let permission = tokio::spawn(async move {
        port.request_permission(&json!({"sessionId":"remote","toolCall":{"toolCallId":"t","title":"Read"},"options":[{"optionId":"exact","name":"Allow","kind":"allow_once"}]})).await
    });
    let mut changed = handle.observe();
    while handle.permissions().is_empty() {
        changed.changed().await.unwrap();
    }
    let choice = handle.permissions()[0].id.clone();
    gate.blocked.store(true, Ordering::Release);
    let port = peer.handle();
    let unsupported = tokio::spawn(async move {
        port.request("fixture/unsupported", &json!({}), CancellationToken::new())
            .await
    });
    gate.entered.notified().await; // reader is awaiting this unsupported reply's flush
    tokio::time::pause();
    let answering = handle.answer(state.generation, &choice, "exact");
    tokio::pin!(answering);
    std::future::poll_fn(|cx| {
        assert!(answering.as_mut().poll(cx).is_pending());
        Poll::Ready(())
    })
    .await;
    assert!(handle.permissions().is_empty());
    assert_eq!(answering.await, Err(Error::Unknown)); // paused time reaches CONTROL
    assert!(
        state.port.is_closed(),
        "client stop alone leaves the core writer alive"
    );
    assert_eq!(handle.failure(), Some(rsi_acp::Error::Closed));
    gate.release();
    assert!(
        permission.await.unwrap().is_err(),
        "timed out permission cannot flush later"
    );
    let _ = unsupported.await.unwrap();
    client.close().await.unwrap();
    peer.close().await.unwrap();
}

#[tokio::test]
async fn disconnect_discards_unpublished_replay_before_rollback_waiter_runs() {
    let (_root, client, peer, _) = fixture().await;
    let state = client.state.clone();
    state.transition(Transition::BeginReplay).await.unwrap();
    state
        .journal
        .append(
            &state.id,
            state.generation,
            rsi_acp_journal::RecordKind::Update,
            json!({"text":"unpublished"}),
        )
        .await
        .unwrap();
    state.transition(Transition::Disconnected).await.unwrap();
    // A second replay must not be needed to discard the unpublished epoch.
    let position = state.journal.get(&state.id).await.unwrap();
    assert_eq!(position.status, Status::Unknown);
    let records = state
        .journal
        .page(&state.id, position.epoch + 1, 0)
        .await
        .unwrap();
    assert!(
        records.records.is_empty(),
        "unpublished observations remained charged after disconnect"
    );
    client.close().await.unwrap();
    peer.close().await.unwrap();
}

#[tokio::test]
async fn replay_lost_during_durable_publication_refunds_unpublished_records() {
    let (_root, client, peer, _) = fixture().await;
    let state = client.state.clone();
    let (committed, commit) = tokio::sync::oneshot::channel();
    let (publish, publication) = tokio::sync::oneshot::channel();
    *state.publication_hook.lock().unwrap() = Some(transitions::PublicationHook {
        committed,
        publish: publication,
    });
    let owner = state.clone();
    let replay = tokio::spawn(async move { owner.transition(Transition::BeginReplay).await });
    commit.await.unwrap();
    state
        .journal
        .append(
            &state.id,
            state.generation,
            rsi_acp_journal::RecordKind::Update,
            json!({"text":"unpublished"}),
        )
        .await
        .unwrap();
    state.stop.cancel();
    publish.send(()).unwrap();
    assert!(matches!(replay.await.unwrap(), Err(Error::Unknown)));
    let position = state.journal.get(&state.id).await.unwrap();
    assert_eq!(position.status, Status::Unknown);
    assert!(
        state
            .journal
            .page(&state.id, position.epoch + 1, 0)
            .await
            .unwrap()
            .records
            .is_empty(),
        "setup loss must refund the durable but unpublished epoch"
    );
    client.close().await.unwrap();
    peer.close().await.unwrap();
}

#[tokio::test]
async fn close_interrupts_an_unsupported_response_under_backpressure() {
    let (_root, client, peer, gate) = fixture().await;
    client
        .state
        .transition(Transition::Bind("remote".into(), Capabilities::default()))
        .await
        .unwrap();
    gate.blocked.store(true, Ordering::Release);
    let port = peer.handle();
    let request = tokio::spawn(async move {
        port.request("fixture/unsupported", &json!({}), CancellationToken::new())
            .await
    });
    gate.entered.notified().await;
    let closed = tokio::time::timeout(Duration::from_secs(2), client.close())
        .await
        .expect("close must interrupt the blocked response")
        .unwrap();
    assert_eq!(closed.status, Status::Closed);
    gate.release();
    assert!(request.await.unwrap().is_err());
    peer.close().await.unwrap();
}

#[tokio::test]
async fn completion_closes_permission_admission_before_snapshot_publication() {
    let (_root, client, mut peer, _) = fixture().await;
    let state = client.state.clone();
    state.bind_target("remote").unwrap();
    state
        .transition(Transition::Bind("remote".into(), Capabilities::default()))
        .await
        .unwrap();
    let handle = client.handle();
    handle
        .submit(vec![rsi_acp_protocol::schema::ContentBlock::Text(
            rsi_acp_protocol::schema::TextContent::new("hello"),
        )])
        .await
        .unwrap();
    let rsi_acp_protocol::Message::Request { id, .. } = peer.next().await.unwrap().message else {
        panic!("prompt")
    };
    let port = peer.handle();
    let permission = tokio::spawn(async move {
        port.request_permission(&json!({"sessionId":"remote","toolCall":{"toolCallId":"t","title":"Read"},"options":[{"optionId":"exact","name":"Allow","kind":"allow_once"}]})).await
    });
    let mut changed = handle.observe();
    while handle.permissions().is_empty() {
        changed.changed().await.unwrap();
    }
    let choice = handle.permissions()[0].id.clone();
    let (committed, commit) = tokio::sync::oneshot::channel();
    let (publish, publication) = tokio::sync::oneshot::channel();
    *state.publication_hook.lock().unwrap() = Some(transitions::PublicationHook {
        committed,
        publish: publication,
    });
    peer.handle()
        .respond(&id, Ok(&json!({"stopReason":"end_turn"})))
        .await
        .unwrap();
    commit.await.unwrap();
    assert_eq!(state.snapshot().status, Status::Running);
    assert_eq!(
        handle.answer(state.generation, &choice, "exact").await,
        Err(Error::Stale)
    );
    publish.send(()).unwrap();
    let response = permission.await.unwrap().unwrap();
    assert_eq!(
        response.result().unwrap()["outcome"]["outcome"],
        "cancelled"
    );
    let late = peer.handle().request_permission(&json!({"sessionId":"remote","toolCall":{"toolCallId":"late","title":"Read"},"options":[{"optionId":"exact","name":"Allow","kind":"allow_once"}]})).await.unwrap();
    assert_eq!(late.result().unwrap()["outcome"]["outcome"], "cancelled");
    client.close().await.unwrap();
    peer.close().await.unwrap();
}

#[tokio::test]
async fn binding_publishes_replay_and_identity_together() {
    let (_root, client, peer, _) = fixture().await;
    let state = client.state.clone();
    state
        .transition(Transition::Bind("remote".into(), Capabilities::default()))
        .await
        .unwrap();
    let before = state.snapshot().epoch;
    state.transition(Transition::BeginReplay).await.unwrap();
    state
        .journal
        .append(
            &state.id,
            state.generation,
            rsi_acp_journal::RecordKind::Update,
            json!({"text":"replacement"}),
        )
        .await
        .unwrap();
    assert!(
        state
            .transition(Transition::Bind("wrong".into(), Capabilities::default()))
            .await
            .is_err()
    );
    assert_eq!(state.journal.get(&state.id).await.unwrap().epoch, before);
    let bound = state
        .transition(Transition::Bind("remote".into(), Capabilities::default()))
        .await
        .unwrap();
    assert_eq!(bound.status, Status::Ready);
    assert!(bound.epoch > before);
    assert_eq!(
        state.journal.get(&state.id).await.unwrap().epoch,
        bound.epoch
    );
    client.close().await.unwrap();
    peer.close().await.unwrap();
}

#[tokio::test]
async fn unsupported_reply_waits_for_capacity_and_is_sent_once() {
    let (_root, client, peer, gate) = fixture().await;
    *client.state.active.lock().unwrap() = Some(CancellationToken::new());
    let port = peer.handle();
    let response = tokio::spawn(async move {
        port.request_permission(&json!({"sessionId":"remote","toolCall":{"toolCallId":"t","title":"Read"},"options":[{"optionId":"exact","name":"Allow","kind":"allow_once"}]})).await.unwrap()
    });
    let handle = client.handle();
    let mut changed = handle.observe();
    while handle.permissions().is_empty() {
        changed.changed().await.unwrap();
    }
    // Reuse a wire-admitted request identity to drive the automatic-reply path
    // directly, after the consumer has handed off its ordinary permission owner.
    let id = {
        let mut permissions = client.state.permissions.lock().unwrap();
        let (_, pending) = permissions.pop_first().unwrap();
        let rsi_acp_protocol::Message::Request { id, .. } = pending.incoming.message else {
            panic!("request")
        };
        id
    };
    gate.blocked.store(true, Ordering::Release);
    queue_notification(&client.state).await;
    gate.entered.notified().await;
    for _ in 0..rsi_acp_protocol::MAX_PENDING {
        queue_notification(&client.state).await;
    }
    let mut answer = Box::pin(incoming::unsupported_answer(&client.state, &id));
    std::future::poll_fn(|cx| {
        assert!(
            answer.as_mut().poll(cx).is_pending(),
            "saturation must retain the unanswered request"
        );
        Poll::Ready(())
    })
    .await;
    assert!(handle.connected());
    gate.release();
    answer.await.unwrap();
    assert_eq!(
        response.await.unwrap().result().unwrap_err()["code"],
        -32601
    );
    assert!(handle.connected());
    client.close().await.unwrap();
    peer.close().await.unwrap();
}
