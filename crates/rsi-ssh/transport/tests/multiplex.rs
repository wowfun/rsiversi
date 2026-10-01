use rsi_ssh_protocol::frame::{
    Frame, FrameHeader, FrameKind, HEADER_BYTES, MAXIMUM_FRAGMENT_BYTES, MAXIMUM_MESSAGE_BYTES,
};
use rsi_ssh_transport::{Connection, Control, Error, Incoming, RequestKind, Role, StreamId};
use std::{
    future::Future,
    pin::Pin,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    task::{Context, Poll, Waker},
    time::Duration,
};
use tokio::{
    io::{AsyncReadExt, AsyncWrite, AsyncWriteExt, DuplexStream},
    time::timeout,
};

const EPOCH: u64 = 37;
fn pair() -> (Connection, Connection, Incoming) {
    let (left, right) = tokio::io::duplex(1024);
    let (lr, lw) = tokio::io::split(left);
    let (rr, rw) = tokio::io::split(right);
    let (client, _) = Connection::start(lr, lw, Role::Client, EPOCH).unwrap();
    let (helper, incoming) = Connection::start(rr, rw, Role::Helper, EPOCH).unwrap();
    (client, helper, incoming)
}
fn raw(role: Role) -> (Connection, Incoming, DuplexStream) {
    let (left, right) = tokio::io::duplex(1024);
    let (read, write) = tokio::io::split(left);
    let (connection, incoming) = Connection::start(read, write, role, EPOCH).unwrap();
    (connection, incoming, right)
}
async fn read_frame(wire: &mut DuplexStream) -> Frame {
    let mut header = [0; HEADER_BYTES];
    wire.read_exact(&mut header).await.unwrap();
    let header = FrameHeader::decode(&header, EPOCH).unwrap();
    let mut bytes = vec![0; header.body_len()];
    wire.read_exact(&mut bytes).await.unwrap();
    header.with_body(bytes).unwrap()
}
async fn send_frame(wire: &mut DuplexStream, frame: Frame) {
    wire.write_all(&frame.encode_header(EPOCH).unwrap())
        .await
        .unwrap();
    wire.write_all(frame.body()).await.unwrap();
}
async fn bounded<F: Future>(future: F) -> F::Output {
    timeout(Duration::from_secs(2), future)
        .await
        .expect("operation failed to make progress")
}

#[tokio::test(start_paused = true)]
async fn full_streams_and_abandoned_ordinary_calls_preserve_reserved_controls_and_heartbeats() {
    let (client, helper, mut incoming) = pair();
    let mut receivers = Vec::new();
    let mut blocked_writers = Vec::new();
    for slot in 0..8 {
        let id = StreamId::new(slot, 1).unwrap();
        let mut sender = helper.open_sender(id).unwrap();
        receivers.push(client.open_receiver(id).unwrap());
        // Four tiny frames spend all credits, despite consuming only four bytes.
        for byte in 0..4 {
            bounded(sender.send(vec![byte], false)).await.unwrap();
        }
        blocked_writers.push(tokio::spawn(
            async move { sender.send(vec![4], false).await },
        ));
    }
    let mut calls = Vec::new();
    let mut requests = Vec::new();
    for _ in 0..32 {
        let connection = client.clone();
        calls.push(tokio::spawn(async move { connection.call(vec![1]).await }));
        requests.push(bounded(incoming.next()).await.unwrap());
    }
    for call in calls {
        call.abort();
        let _ = call.await;
    }
    assert_eq!(client.call(vec![2]).await.unwrap_err(), Error::Capacity);
    assert!(blocked_writers.iter().all(|writer| !writer.is_finished()));

    let mut heartbeat = helper.heartbeats();
    bounded(client.heartbeat()).await.unwrap();
    heartbeat.changed().await.unwrap();
    assert_eq!(*heartbeat.borrow_and_update(), 1);
    for control in [
        Control::Resize {
            process: 4,
            columns: 100,
            rows: 40,
        },
        Control::Terminate { process: 4 },
    ] {
        let connection = client.clone();
        let call = tokio::spawn(async move { connection.control(control).await });
        let request = bounded(incoming.next()).await.unwrap();
        assert_eq!(request.kind(), RequestKind::Control(control));
        request.reply(b"ok".to_vec()).unwrap();
        assert_eq!(bounded(call).await.unwrap().unwrap().as_bytes(), b"ok");
    }
    assert!(blocked_writers.iter().all(|writer| !writer.is_finished()));
    for receiver in &mut receivers {
        assert_eq!(
            bounded(receiver.next()).await.unwrap().unwrap().bytes,
            vec![0]
        );
    }
    for writer in blocked_writers {
        bounded(writer).await.unwrap().unwrap();
    }
    // Remote replies settle abandoned calls; only then can their slots be reused.
    for request in requests {
        request.reply(vec![3]).unwrap();
    }
    bounded(client.heartbeat()).await.unwrap();
    let connection = client.clone();
    let next = tokio::spawn(async move { connection.call(vec![9]).await });
    bounded(incoming.next())
        .await
        .unwrap()
        .reply(vec![10])
        .unwrap();
    assert_eq!(bounded(next).await.unwrap().unwrap().as_bytes(), &[10]);
}

#[tokio::test(start_paused = true)]
async fn fragmented_payloads_round_trip_at_the_bound_while_stream_eof_remains_ordered() {
    let (client, helper, mut incoming) = pair();
    let id = StreamId::new(5, 17).unwrap();
    let mut sender = helper.open_sender(id).unwrap();
    let mut receiver = client.open_receiver(id).unwrap();
    let connection = client.clone();
    let call =
        tokio::spawn(async move { connection.call(vec![0x35; MAXIMUM_MESSAGE_BYTES]).await });
    let bytes = vec![0x7b; MAXIMUM_FRAGMENT_BYTES];
    sender.send(bytes.clone(), false).await.unwrap();
    sender.send(vec![1, 2], true).await.unwrap();
    assert_eq!(sender.send(vec![3], false).await, Err(Error::Invalid));
    let request = bounded(incoming.next()).await.unwrap();
    assert_eq!(request.payload(), vec![0x35; MAXIMUM_MESSAGE_BYTES]);
    request.reply(vec![0x57; MAXIMUM_MESSAGE_BYTES]).unwrap();
    assert_eq!(
        bounded(call).await.unwrap().unwrap().as_bytes(),
        vec![0x57; MAXIMUM_MESSAGE_BYTES]
    );
    let first = bounded(receiver.next()).await.unwrap().unwrap();
    assert!(!first.eof);
    assert_eq!(first.bytes, bytes);
    let last = receiver.next().await.unwrap().unwrap();
    assert!(last.eof);
    assert_eq!(last.bytes, [1, 2]);
    assert_eq!(receiver.next().await.unwrap(), None);
    helper.retire_stream(id).unwrap();
    client.retire_stream(id).unwrap();
    assert_eq!(client.open_receiver(id).unwrap_err(), Error::Invalid);
    assert_eq!(receiver.next().await.unwrap_err(), Error::Invalid);
    let fresh = StreamId::new(5, 18).unwrap();
    let mut fresh_sender = helper.open_sender(fresh).unwrap();
    let mut fresh_receiver = client.open_receiver(fresh).unwrap();
    fresh_sender.send(vec![], true).await.unwrap();
    assert!(fresh_receiver.next().await.unwrap().unwrap().eof);
}

#[tokio::test(start_paused = true)]
async fn retirement_distinguishes_never_dispatched_calls_from_unverified_dispatched_effects() {
    let (client, _, _wire) = raw(Role::Client);
    let mut queued = Box::pin(client.call(vec![1]));
    std::future::poll_fn(|cx| {
        assert!(queued.as_mut().poll(cx).is_pending());
        Poll::Ready(())
    })
    .await;
    client.close();
    assert_eq!(queued.await.unwrap_err(), Error::Closed);

    let (client, _, mut wire) = raw(Role::Client);
    let connection = client.clone();
    let started = tokio::spawn(async move { connection.call(b"effect".to_vec()).await });
    let request = bounded(read_frame(&mut wire)).await;
    assert_eq!(request.body(), b"effect");
    // Even a syntactically valid partial reply proves no complete outcome.
    send_frame(
        &mut wire,
        Frame::new(
            EPOCH,
            FrameKind::Reply,
            request.header().identity(),
            0,
            false,
            b"partial".to_vec(),
        )
        .unwrap(),
    )
    .await;
    drop(wire);
    assert_eq!(
        bounded(started).await.unwrap().unwrap_err(),
        Error::OutcomeUnknown
    );
}

#[tokio::test(start_paused = true)]
async fn a_reply_before_the_complete_request_is_dispatched_cannot_certify_an_outcome() {
    let (client, _, mut wire) = raw(Role::Client);
    let connection = client.clone();
    let call = tokio::spawn(async move { connection.call(vec![1; MAXIMUM_MESSAGE_BYTES]).await });
    let first = bounded(read_frame(&mut wire)).await;
    assert!(!first.header().is_final());
    send_frame(
        &mut wire,
        Frame::new(
            EPOCH,
            FrameKind::Reply,
            first.header().identity(),
            0,
            true,
            b"premature".to_vec(),
        )
        .unwrap(),
    )
    .await;
    assert_eq!(
        bounded(call).await.unwrap().unwrap_err(),
        Error::OutcomeUnknown
    );
    bounded(client.settled()).await;
}

#[tokio::test(start_paused = true)]
async fn heartbeat_and_control_preempt_the_next_ordinary_fragment_after_an_inflight_frame() {
    let (client, _, mut wire) = raw(Role::Client);
    let connection = client.clone();
    let call = tokio::spawn(async move { connection.call(vec![1; MAXIMUM_MESSAGE_BYTES]).await });
    assert_eq!(
        bounded(read_frame(&mut wire)).await.header().kind(),
        FrameKind::Request
    );
    let mut control = Box::pin(client.control(Control::Terminate { process: 5 }));
    let mut heartbeat = Box::pin(client.heartbeat());
    std::future::poll_fn(|cx| {
        assert!(control.as_mut().poll(cx).is_pending());
        assert!(heartbeat.as_mut().poll(cx).is_pending());
        Poll::Ready(())
    })
    .await;
    let mut ordinary = 0;
    let mut heartbeat_seen = false;
    let control_id = loop {
        let frame = bounded(read_frame(&mut wire)).await;
        match frame.header().kind() {
            FrameKind::Request => {
                ordinary += 1;
                assert!(ordinary <= 1);
            }
            FrameKind::Heartbeat => {
                heartbeat_seen = true;
                send_frame(
                    &mut wire,
                    Frame::new(
                        EPOCH,
                        FrameKind::HeartbeatAck,
                        frame.header().identity(),
                        0,
                        true,
                        vec![],
                    )
                    .unwrap(),
                )
                .await;
            }
            FrameKind::ControlRequest => {
                assert!(heartbeat_seen);
                break frame.header().identity();
            }
            other => panic!("unexpected frame {other:?}"),
        }
    };
    send_frame(
        &mut wire,
        Frame::new(EPOCH, FrameKind::ControlReply, control_id, 0, true, vec![7]).unwrap(),
    )
    .await;
    bounded(heartbeat).await.unwrap();
    assert_eq!(bounded(control).await.unwrap().as_bytes(), &[7]);
    client.close();
    assert_eq!(
        bounded(call).await.unwrap().unwrap_err(),
        Error::OutcomeUnknown
    );
    bounded(client.settled()).await;
}

#[tokio::test(start_paused = true)]
async fn disconnect_never_forges_eof_or_accepts_blocked_stream_writes() {
    let (client, helper, _incoming) = pair();
    let id = StreamId::new(2, 1).unwrap();
    let mut sender = helper.open_sender(id).unwrap();
    let mut receiver = client.open_receiver(id).unwrap();
    for _ in 0..4 {
        sender.send(vec![1], false).await.unwrap();
    }
    let writer = tokio::spawn(async move { sender.send(vec![2], false).await });
    bounded(client.heartbeat()).await.unwrap();
    assert!(!writer.is_finished());
    helper.close();
    bounded(client.closed()).await;
    assert_eq!(receiver.next().await.unwrap_err(), Error::Closed);
    assert_eq!(bounded(writer).await.unwrap(), Err(Error::Closed));
}

#[tokio::test(start_paused = true)]
async fn receive_budget_covers_delivered_requests_and_is_independent_of_controls() {
    let (client, _helper, mut incoming) = pair();
    let mut calls = Vec::new();
    let mut retained = Vec::new();
    for _ in 0..4 {
        let connection = client.clone();
        calls.push(tokio::spawn(async move {
            connection.call(vec![5; MAXIMUM_MESSAGE_BYTES]).await
        }));
        retained.push(bounded(incoming.next()).await.unwrap());
    }
    let connection = client.clone();
    let control =
        tokio::spawn(async move { connection.control(Control::Terminate { process: 1 }).await });
    bounded(incoming.next())
        .await
        .unwrap()
        .reply(vec![1])
        .unwrap();
    bounded(control).await.unwrap().unwrap();
    // A fifth byte cannot bypass the budget simply because earlier requests were delivered.
    assert_eq!(
        bounded(client.call(vec![1])).await.unwrap_err(),
        Error::OutcomeUnknown
    );
    for call in calls {
        assert_eq!(
            bounded(call).await.unwrap().unwrap_err(),
            Error::OutcomeUnknown
        );
    }
    drop(retained);
}

#[tokio::test(start_paused = true)]
async fn wrong_epoch_unknown_controls_and_out_of_order_fragments_close_before_dispatch() {
    for case in 0..4 {
        let (helper, mut incoming, mut wire) = raw(Role::Helper);
        match case {
            0 => {
                let frame = Frame::new(EPOCH + 1, FrameKind::Request, 2, 0, true, vec![1]).unwrap();
                // Send only the foreign header: the reader must not await its body.
                wire.write_all(&frame.encode_header(EPOCH + 1).unwrap())
                    .await
                    .unwrap();
            }
            1 => {
                send_frame(
                    &mut wire,
                    Frame::new(
                        EPOCH,
                        FrameKind::ControlRequest,
                        3,
                        0,
                        true,
                        br#"{"operation":"execute","command":"touch marker"}"#.to_vec(),
                    )
                    .unwrap(),
                )
                .await;
            }
            2 => {
                send_frame(
                    &mut wire,
                    Frame::new(
                        EPOCH,
                        FrameKind::ControlRequest,
                        3,
                        0,
                        true,
                        br#"{"operation":"resize","process":1,"columns":0,"rows":30}"#.to_vec(),
                    )
                    .unwrap(),
                )
                .await;
            }
            _ => {
                send_frame(
                    &mut wire,
                    Frame::new(EPOCH, FrameKind::Request, 2, 1, true, vec![1]).unwrap(),
                )
                .await;
            }
        }
        bounded(helper.closed()).await;
        assert!(incoming.next().await.is_none());
    }
}

#[tokio::test(start_paused = true)]
async fn replayed_credit_or_credit_for_unsent_data_retires_the_connection() {
    for sequence in [1, 2] {
        let (client, _, mut wire) = raw(Role::Client);
        let id = StreamId::new(0, 1).unwrap();
        let _sender = client.open_sender(id).unwrap();
        send_frame(
            &mut wire,
            Frame::new(
                EPOCH,
                FrameKind::Credit,
                id.raw(),
                1,
                true,
                4_u32.to_be_bytes().to_vec(),
            )
            .unwrap(),
        )
        .await;
        send_frame(
            &mut wire,
            Frame::new(
                EPOCH,
                FrameKind::Credit,
                id.raw(),
                sequence,
                true,
                1_u32.to_be_bytes().to_vec(),
            )
            .unwrap(),
        )
        .await;
        bounded(client.closed()).await;
    }
}

#[tokio::test(start_paused = true)]
async fn frames_already_in_flight_for_retired_streams_do_not_touch_reused_slots() {
    let (helper, _, mut wire) = raw(Role::Helper);
    let old = StreamId::new(0, 1).unwrap();
    let mut abandoned = helper.open_receiver(old).unwrap();
    assert_eq!(
        bounded(read_frame(&mut wire)).await.header().kind(),
        FrameKind::Credit
    );
    helper.retire_stream(old).unwrap();
    let fresh = StreamId::new(0, 2).unwrap();
    let mut receiver = helper.open_receiver(fresh).unwrap();
    assert_eq!(
        bounded(read_frame(&mut wire)).await.header().identity(),
        fresh.raw()
    );
    send_frame(
        &mut wire,
        Frame::new(
            EPOCH,
            FrameKind::Data,
            old.raw(),
            0,
            false,
            b"retired".to_vec(),
        )
        .unwrap(),
    )
    .await;
    send_frame(
        &mut wire,
        Frame::new(
            EPOCH,
            FrameKind::Credit,
            old.raw(),
            1,
            true,
            4_u32.to_be_bytes().to_vec(),
        )
        .unwrap(),
    )
    .await;
    send_frame(
        &mut wire,
        Frame::new(
            EPOCH,
            FrameKind::Data,
            fresh.raw(),
            0,
            true,
            b"current".to_vec(),
        )
        .unwrap(),
    )
    .await;
    assert_eq!(
        bounded(receiver.next()).await.unwrap().unwrap().bytes,
        b"current"
    );
    assert_eq!(abandoned.next().await.unwrap_err(), Error::Invalid);
    // A future unregistered generation still cannot create a stream from the wire.
    let future = StreamId::new(0, 3).unwrap();
    send_frame(
        &mut wire,
        Frame::new(EPOCH, FrameKind::Data, future.raw(), 0, true, vec![]).unwrap(),
    )
    .await;
    bounded(helper.closed()).await;
}

#[tokio::test(start_paused = true)]
async fn dropping_an_unanswered_request_settles_pending_client_work_as_unknown() {
    let (client, _helper, mut incoming) = pair();
    let connection = client.clone();
    let call = tokio::spawn(async move { connection.call(vec![1]).await });
    drop(bounded(incoming.next()).await.unwrap());
    assert_eq!(
        bounded(call).await.unwrap().unwrap_err(),
        Error::OutcomeUnknown
    );
}

#[derive(Debug, Default)]
struct FlushGate {
    open: AtomicBool,
    waiter: std::sync::Mutex<Option<Waker>>,
}
impl FlushGate {
    fn open(&self) {
        self.open.store(true, Ordering::Release);
        if let Some(waiter) = self.waiter.lock().unwrap().take() {
            waiter.wake();
        }
    }
}
#[derive(Debug)]
struct GatedFlush<W> {
    inner: W,
    gate: Arc<FlushGate>,
}
impl<W: AsyncWrite + Unpin> AsyncWrite for GatedFlush<W> {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        bytes: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        Pin::new(&mut self.inner).poll_write(cx, bytes)
    }
    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        *self.gate.waiter.lock().unwrap() = Some(cx.waker().clone());
        if self.gate.open.load(Ordering::Acquire) {
            Pin::new(&mut self.inner).poll_flush(cx)
        } else {
            Poll::Pending
        }
    }
    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.inner).poll_shutdown(cx)
    }
}

#[tokio::test(start_paused = true)]
async fn a_reply_visible_to_the_peer_returns_its_request_slot_before_local_flush_finishes() {
    let (left, right) = tokio::io::duplex(1024);
    let (lr, lw) = tokio::io::split(left);
    let (rr, rw) = tokio::io::split(right);
    let gate = Arc::new(FlushGate::default());
    let (client, _) = Connection::start(lr, lw, Role::Client, EPOCH).unwrap();
    let (_helper, mut incoming) = Connection::start(
        rr,
        GatedFlush {
            inner: rw,
            gate: gate.clone(),
        },
        Role::Helper,
        EPOCH,
    )
    .unwrap();
    let mut calls = Vec::new();
    let mut requests = Vec::new();
    for _ in 0..32 {
        let connection = client.clone();
        calls.push(tokio::spawn(async move { connection.call(vec![1]).await }));
        requests.push(bounded(incoming.next()).await.unwrap());
    }
    requests.remove(0).reply(vec![2]).unwrap();
    bounded(calls.remove(0)).await.unwrap().unwrap();
    let connection = client.clone();
    let replacement = tokio::spawn(async move { connection.call(vec![3]).await });
    let request = bounded(incoming.next())
        .await
        .expect("peer already consumed the preceding reply");
    assert_eq!(request.payload(), &[3]);
    gate.open();
    request.reply(vec![4]).unwrap();
    bounded(replacement).await.unwrap().unwrap();
    for request in requests {
        request.reply(vec![2]).unwrap();
    }
    for call in calls {
        bounded(call).await.unwrap().unwrap();
    }
}

#[tokio::test(start_paused = true)]
async fn unanswered_requests_expire_even_after_waiter_loss_and_live_heartbeats() {
    let (client, helper, mut incoming) = pair();
    let connection = client.clone();
    let abandoned = tokio::spawn(async move { connection.call(vec![42]).await });
    let retained = bounded(incoming.next()).await.unwrap();
    abandoned.abort();
    let _ = abandoned.await;
    for _ in 0..5 {
        bounded(client.heartbeat()).await.unwrap();
        tokio::time::advance(Duration::from_secs(5)).await;
        assert!(!client.is_closed());
    }
    tokio::time::advance(Duration::from_secs(6)).await;
    tokio::task::yield_now().await;
    assert!(
        client.is_closed(),
        "an unanswered admitted request must retire its epoch"
    );
    bounded(client.settled()).await;
    drop(retained);
    helper.close();
}

#[tokio::test(start_paused = true)]
async fn peer_reply_certifies_request_before_local_flush_completes() {
    let (left, right) = tokio::io::duplex(1024);
    let (lr, lw) = tokio::io::split(left);
    let (rr, rw) = tokio::io::split(right);
    let gate = Arc::new(FlushGate::default());
    let (client, _) = Connection::start(
        lr,
        GatedFlush {
            inner: lw,
            gate: gate.clone(),
        },
        Role::Client,
        EPOCH,
    )
    .unwrap();
    let (_helper, mut incoming) = Connection::start(rr, rw, Role::Helper, EPOCH).unwrap();
    let calling = client.clone();
    let waiting = tokio::spawn(async move { calling.call(vec![1, 2, 3]).await });
    let request = bounded(incoming.next()).await.unwrap();
    assert_eq!(request.payload(), &[1, 2, 3]);
    assert!(!gate.open.load(Ordering::Acquire));
    request.reply(vec![4]).unwrap();
    assert_eq!(bounded(waiting).await.unwrap().unwrap().as_bytes(), &[4]);
    gate.open();
    client.close();
    client.settled().await;
}

#[tokio::test(start_paused = true)]
async fn abandoned_heartbeat_expires_without_peer_eof_or_further_calls() {
    let (client, _, mut wire) = raw(Role::Client);
    let waiter = tokio::spawn({
        let client = client.clone();
        async move { client.heartbeat().await }
    });
    assert_eq!(
        read_frame(&mut wire).await.header().kind(),
        FrameKind::Heartbeat
    );
    waiter.abort();
    assert!(waiter.await.unwrap_err().is_cancelled());
    assert_eq!(client.heartbeat().await.unwrap_err(), Error::Capacity);
    tokio::time::advance(Duration::from_secs(30)).await;
    bounded(client.closed()).await;
    bounded(client.settled()).await;
    assert_eq!(client.heartbeat().await.unwrap_err(), Error::Closed);
}

#[tokio::test(start_paused = true)]
async fn request_parity_and_completed_identity_replay_are_rejected_per_class() {
    for (kind, identity, body) in [
        (FrameKind::Request, 4, vec![1]),
        (
            FrameKind::ControlRequest,
            5,
            serde_json::to_vec(&Control::Terminate { process: 1 }).unwrap(),
        ),
    ] {
        for invalid in [identity - 2, identity, identity + 1] {
            let (helper, mut incoming, mut wire) = raw(Role::Helper);
            send_frame(
                &mut wire,
                Frame::new(EPOCH, kind, identity, 0, true, body.clone()).unwrap(),
            )
            .await;
            let call = bounded(incoming.next()).await.unwrap();
            call.reply(vec![1]).unwrap();
            read_frame(&mut wire).await;
            send_frame(
                &mut wire,
                Frame::new(EPOCH, kind, invalid, 0, true, body.clone()).unwrap(),
            )
            .await;
            bounded(helper.closed()).await;
            assert!(incoming.next().await.is_none());
        }
    }
}
