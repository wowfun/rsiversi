use rsi_acp::{Error, Peer, StreamTransport};
use rsi_acp_protocol::{Message, RequestId};
use serde_json::{Value, json};
use std::time::Duration;
use tokio::io::{AsyncBufReadExt as _, AsyncWriteExt as _, BufReader, DuplexStream};
use tokio_util::sync::CancellationToken;

fn connection() -> (
    Peer,
    BufReader<tokio::io::ReadHalf<DuplexStream>>,
    tokio::io::WriteHalf<DuplexStream>,
) {
    let (local, remote) = tokio::io::duplex(65536);
    let (reader, writer) = tokio::io::split(local);
    let (remote_reader, remote_writer) = tokio::io::split(remote);
    (
        Peer::start(StreamTransport::new(reader, writer)),
        BufReader::new(remote_reader),
        remote_writer,
    )
}
async fn line(reader: &mut BufReader<tokio::io::ReadHalf<DuplexStream>>) -> Value {
    let mut text = String::new();
    tokio::time::timeout(Duration::from_secs(2), reader.read_line(&mut text))
        .await
        .unwrap()
        .unwrap();
    serde_json::from_str(&text).unwrap()
}
async fn send(writer: &mut tokio::io::WriteHalf<DuplexStream>, value: Value) {
    writer
        .write_all(&serde_json::to_vec(&value).unwrap())
        .await
        .unwrap();
    writer.write_all(b"\n").await.unwrap();
}

#[tokio::test]
async fn correlates_out_of_order_responses_and_drains_before_load_response() {
    let (mut peer, mut reader, mut writer) = connection();
    let port = peer.handle();
    let first = tokio::spawn({
        let port = port.clone();
        async move {
            port.request("first", &json!({}), CancellationToken::new())
                .await
                .unwrap()
        }
    });
    let a = line(&mut reader).await;
    let second = tokio::spawn({
        let port = port.clone();
        async move {
            port.request("second", &json!({}), CancellationToken::new())
                .await
                .unwrap()
        }
    });
    let b = line(&mut reader).await;
    send(
        &mut writer,
        json!({"jsonrpc":"2.0","id":b["id"],"result":{"value":2}}),
    )
    .await;
    send(
        &mut writer,
        json!({"jsonrpc":"2.0","id":a["id"],"result":{"value":1}}),
    )
    .await;
    assert_eq!(second.await.unwrap().result().unwrap()["value"], 2);
    assert_eq!(first.await.unwrap().result().unwrap()["value"], 1);
    send(
        &mut writer,
        json!({"jsonrpc":"2.0","id":"load-exact","method":"session/load","params":{}}),
    )
    .await;
    let incoming = peer.next().await.unwrap();
    let Message::Request { id, .. } = &incoming.message else {
        panic!("request");
    };
    assert_eq!(id, &RequestId::String("load-exact".into()));
    for ordinal in 0..3 {
        port.notify("session/update", &json!({"ordinal":ordinal}))
            .await
            .unwrap();
    }
    port.drain().await.unwrap();
    port.respond(id, Ok(&json!({}))).await.unwrap();
    for ordinal in 0..3 {
        assert_eq!(line(&mut reader).await["params"]["ordinal"], ordinal);
    }
    assert_eq!(line(&mut reader).await["id"], "load-exact");
    peer.close().await.unwrap();
}

#[tokio::test]
async fn cancelled_request_has_unknown_delivery_and_is_never_replayed() {
    let (peer, mut reader, _writer) = connection();
    let port = peer.handle();
    let cancellation = CancellationToken::new();
    let request = tokio::spawn({
        let port = port.clone();
        let cancellation = cancellation.clone();
        async move {
            port.request("session/prompt", &json!({}), cancellation)
                .await
        }
    });
    assert_eq!(line(&mut reader).await["method"], "session/prompt");
    cancellation.cancel();
    assert!(matches!(request.await.unwrap(), Err(Error::Closed)));
    assert!(port.is_closed());
    assert!(matches!(
        port.request("session/prompt", &json!({}), CancellationToken::new())
            .await,
        Err(Error::Closed)
    ));
    peer.close().await.unwrap();
    let mut extra = String::new();
    assert_eq!(reader.read_line(&mut extra).await.unwrap(), 0);
}

#[tokio::test]
async fn inbound_pressure_preserves_control_writes_but_still_bounds_incoming() {
    let (mut peer, mut reader, mut writer) = connection();
    let port = peer.handle();
    let mut retained = Vec::new();
    for index in 0..8 {
        let mut value = if index == 7 {
            json!({"jsonrpc":"2.0","method":"session/request_permission","id":"permission","params":{"text":""}})
        } else {
            json!({"jsonrpc":"2.0","method":"session/update","params":{"text":""}})
        };
        let overhead = serde_json::to_vec(&value).unwrap().len();
        value["params"]["text"] = json!("x".repeat(rsi_acp_protocol::MAX_FRAME_BYTES - overhead));
        assert_eq!(
            serde_json::to_vec(&value).unwrap().len(),
            rsi_acp_protocol::MAX_FRAME_BYTES
        );
        send(&mut writer, value).await;
        retained.push(peer.next().await.unwrap());
    }
    let response = json!({"outcome":{"outcome":"cancelled"}});
    let id = RequestId::String("permission".into());
    let (sent, reply) = tokio::join!(port.respond(&id, Ok(&response)), line(&mut reader));
    sent.unwrap();
    assert_eq!(reply["result"], response);
    let parameters = json!({"sessionId":"remote"});
    let (sent, cancel) = tokio::join!(
        port.notify("session/cancel", &parameters),
        line(&mut reader)
    );
    sent.unwrap();
    assert_eq!(cancel["method"], "session/cancel");
    assert!(!port.is_closed());
    send(
        &mut writer,
        json!({"jsonrpc":"2.0","method":"overflow","params":{}}),
    )
    .await;
    assert!(peer.next().await.is_none());
    assert_eq!(port.failure(), Some(Error::Capacity));
    drop(retained);
    peer.close().await.unwrap();
}

#[tokio::test]
async fn local_frame_rejection_is_inert_and_large_typed_frames_do_not_use_input_decoder() {
    let (peer, mut reader, mut writer) = connection();
    let port = peer.handle();
    assert!(matches!(
        port.request(
            "oversized",
            &json!({"text":"x".repeat(rsi_acp_protocol::MAX_FRAME_BYTES)}),
            CancellationToken::new()
        )
        .await,
        Err(Error::Protocol)
    ));
    assert!(
        !port.is_closed(),
        "a locally rejected, undispatched request cannot retire another request"
    );
    let values = json!({"items":(0..70000).map(|_|0).collect::<Vec<_>>()});
    let remote = async {
        let request = line(&mut reader).await;
        assert_eq!(request["params"]["items"].as_array().unwrap().len(), 70000);
        send(
            &mut writer,
            json!({"jsonrpc":"2.0","id":request["id"],"result":{}}),
        )
        .await;
    };
    let (result, ()) = tokio::join!(
        port.request("extension", &values, CancellationToken::new()),
        remote
    );
    result.unwrap();
    assert!(!port.is_closed());
    peer.close().await.unwrap();
}

#[tokio::test]
async fn duplicate_incoming_id_and_unknown_response_retire_the_peer() {
    for duplicate in [false, true] {
        let (mut peer, _reader, mut writer) = connection();
        let frame = if duplicate {
            json!({"jsonrpc":"2.0","id":9,"method":"session/prompt","params":{}})
        } else {
            json!({"jsonrpc":"2.0","id":9,"result":{}})
        };
        send(&mut writer, frame.clone()).await;
        if duplicate {
            let _first = peer.next().await.unwrap();
            send(&mut writer, frame).await;
        }
        assert!(
            tokio::time::timeout(Duration::from_secs(2), peer.next())
                .await
                .unwrap()
                .is_none()
        );
        assert!(peer.handle().is_closed());
        peer.close().await.unwrap();
    }
}

#[tokio::test]
async fn small_notification_count_is_bounded_independently_of_payload_bytes() {
    let (peer, _reader, mut writer) = connection();
    let frame = b"{\"jsonrpc\":\"2.0\",\"method\":\"session/update\",\"params\":{}}\n";
    assert!(frame.len() * 8193 < rsi_acp_protocol::MAX_DIRECTION_BYTES);
    writer.write_all(&frame.repeat(8193)).await.unwrap();
    tokio::time::timeout(Duration::from_secs(2), async {
        while !peer.handle().is_closed() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert_eq!(peer.handle().failure(), Some(Error::Capacity));
    peer.close().await.unwrap();
}

#[tokio::test]
async fn abandoned_permissions_remain_bounded_and_late_replies_release_slots() {
    let (mut peer, mut reader, mut writer) = connection();
    let port = peer.handle();
    let params = json!({"sessionId":"s","toolCall":{"toolCallId":"t"},"options":[{"optionId":"exact","name":"Once","kind":"allow_once"}]});
    let mut ids = Vec::new();
    for _ in 0..rsi_acp_protocol::MAX_PENDING {
        let mut pending = Box::pin(port.request_permission(&params));
        let frame = tokio::select! {
            result = &mut pending => panic!("unexpected reply: {result:?}"),
            frame = line(&mut reader) => frame,
        };
        port.drain().await.unwrap();
        assert!(futures_util::poll!(pending.as_mut()).is_pending());
        drop(pending);
        ids.push(frame["id"].clone());
        assert!(!port.is_closed());
    }
    assert!(matches!(
        port.request_permission(&params).await,
        Err(Error::Capacity)
    ));
    for id in ids {
        send(
            &mut writer,
            json!({"jsonrpc":"2.0","id":id,"result":{"outcome":{"outcome":"cancelled"}}}),
        )
        .await;
    }
    send(
        &mut writer,
        json!({"jsonrpc":"2.0","method":"settled","params":{}}),
    )
    .await;
    assert!(peer.next().await.is_some());
    assert!(!port.is_closed());
    let empty = json!({});
    let pending = port.request("ping", &empty, CancellationToken::new());
    let reply = async {
        let frame = line(&mut reader).await;
        send(
            &mut writer,
            json!({"jsonrpc":"2.0","id":frame["id"],"result":{}}),
        )
        .await;
    };
    let (result, ()) = tokio::join!(pending, reply);
    result.unwrap().result().unwrap();
    peer.close().await.unwrap();
}

#[tokio::test]
async fn reply_exposes_exact_preceding_update_horizon_before_consumer_drain() {
    let (mut peer, mut reader, mut writer) = connection();
    let port = peer.handle();
    let pending = tokio::spawn(async move {
        port.request("session/load", &json!({}), CancellationToken::new())
            .await
            .unwrap()
    });
    let request = line(&mut reader).await;
    for index in 0..3 {
        send(
            &mut writer,
            json!({"jsonrpc":"2.0","method":"session/update","params":{"index":index}}),
        )
        .await;
    }
    send(
        &mut writer,
        json!({"jsonrpc":"2.0","id":request["id"],"result":{}}),
    )
    .await;
    send(
        &mut writer,
        json!({"jsonrpc":"2.0","method":"later","params":{}}),
    )
    .await;
    let response = pending.await.unwrap();
    assert_eq!(response.preceding_messages(), 3);
    for ordinal in 1..=4 {
        assert_eq!(peer.next().await.unwrap().ordinal, ordinal);
    }
    peer.close().await.unwrap();
}

#[tokio::test]
async fn locally_rejected_response_keeps_the_exact_incoming_request() {
    let (mut peer, mut reader, mut writer) = connection();
    let port = peer.handle();
    send(
        &mut writer,
        json!({"jsonrpc":"2.0","id":"retry-response","method":"read","params":{}}),
    )
    .await;
    let incoming = peer.next().await.unwrap();
    let Message::Request { id, .. } = &incoming.message else {
        panic!("request")
    };
    assert_eq!(
        port.respond(
            id,
            Ok(&json!("x".repeat(rsi_acp_protocol::MAX_FRAME_BYTES)))
        )
        .await,
        Err(Error::Protocol)
    );
    assert!(!port.is_closed());
    port.respond(id, Ok(&json!({"ok":true}))).await.unwrap();
    assert_eq!(line(&mut reader).await["result"]["ok"], true);
    peer.close().await.unwrap();
}
