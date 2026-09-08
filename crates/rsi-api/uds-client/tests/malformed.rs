#![cfg(unix)]
use futures_util::StreamExt;
use rsi_api_protocol::*;
use rsi_api_uds_client::{UdsClient, UdsClientConfig};
use rsi_meta::Execution;
use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::UnixListener,
};

fn operation(name: &str) -> OperationSpec {
    OperationSpec {
        id: OperationId::new("fault", name, 1).unwrap(),
        class: if name == "events" {
            OperationClass::Subscription
        } else {
            OperationClass::Data
        },
        effect: if name == "events" || name == "read" {
            OperationEffect::Read
        } else {
            OperationEffect::Mutation
        },
        access: rsi_api_protocol::OperationAccess::Authenticated,
        encoding: RequestEncoding::Json,
        maximum_request_bytes: 128,
        maximum_response_bytes: 128,
    }
}

#[tokio::test]
async fn lost_headers_malformed_replies_and_missing_stream_end_preserve_failure_without_replay() {
    let directory = tempfile::tempdir().unwrap();
    let config = UdsClientConfig {
        socket: directory.path().join("fault.sock"),
        endpoint_id: EndpointId::from_bytes([2; 16]),
        host_epoch: HostEpoch::from_bytes([3; 16]),
        compatibility: LocalCompatibilityKey::from_bytes([4; 32]),
    };
    let listener = UnixListener::bind(&config.socket).unwrap();
    let mut catalog = vec![describe_operation(), operations_operation()];
    catalog.extend(["lost", "json", "epoch", "read", "events"].map(operation));
    let expected = config.clone();
    let calls = Arc::new(Mutex::new(BTreeMap::<String, usize>::new()));
    let recorded = calls.clone();
    let server = tokio::spawn(async move {
        for _ in 0..7 {
            let (mut socket, _) = listener.accept().await.unwrap();
            let path = read_request(&mut socket).await;
            *recorded.lock().unwrap().entry(path.clone()).or_default() += 1;
            let name = path.split('/').nth(4).unwrap();
            if matches!(name, "lost" | "read") {
                continue;
            }
            let body = match name {
                "describe" => serde_json::to_vec(&ConnectionDescription {
                    wire_version: 1,
                    endpoint_id: expected.endpoint_id.clone(),
                    host_epoch: expected.host_epoch.clone(),
                })
                .unwrap(),
                "operations" => serde_json::to_vec(&catalog).unwrap(),
                "json" => b"{broken".to_vec(),
                "events" => b"event: item\ndata: true\n\n".to_vec(),
                "epoch" => b"true".to_vec(),
                _ => panic!("unexpected route {path}"),
            };
            let epoch = if name == "epoch" {
                HostEpoch::from_bytes([5; 16])
            } else {
                expected.host_epoch.clone()
            };
            let mime = if name == "events" {
                "text/event-stream"
            } else {
                "application/json"
            };
            let head = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: {mime}\r\nContent-Length: {}\r\nX-Rsi-Wire-Version: 1\r\nX-Rsi-Endpoint-Id: {}\r\nX-Rsi-Host-Epoch: {}\r\nConnection: close\r\n\r\n",
                body.len(),
                expected.endpoint_id.as_str(),
                epoch.as_str()
            );
            // Malformed metadata may make the client close before all writes finish.
            let _ = socket.write_all(head.as_bytes()).await;
            let _ = socket.write_all(&body).await;
            let _ = socket.shutdown().await;
        }
    });
    let client = UdsClient::connect(Execution::native(tokio::runtime::Handle::current()), config)
        .await
        .unwrap();
    for name in ["lost", "json", "epoch", "read", "events"] {
        let operation = operation(name);
        let result = client
            .call(
                &operation,
                client.input_budget(operation.class).copy(b"{}").unwrap(),
            )
            .await;
        if name == "events" {
            let ApiOutput::Stream(mut events) = result.unwrap() else {
                panic!("stream")
            };
            assert_eq!(
                events.next().await.unwrap().unwrap().json.as_bytes(),
                b"true"
            );
            assert!(events.next().await.unwrap().is_err());
        } else if name == "read" {
            assert!(matches!(result, Err(ApiError::Backend(_))));
        } else {
            assert!(
                matches!(result, Err(ApiError::OutcomeUnknown)),
                "{name}: {result:?}"
            );
        }
    }
    client.close().await;
    tokio::time::timeout(Duration::from_secs(3), server)
        .await
        .unwrap()
        .unwrap();
    let calls = calls.lock().unwrap();
    assert_eq!(calls.len(), 7);
    assert!(calls.values().all(|count| *count == 1));
}

async fn read_request(socket: &mut tokio::net::UnixStream) -> String {
    let mut request = Vec::new();
    let mut byte = [0];
    // Fixture requests are tiny and always have a fixed-length body.
    loop {
        socket.read_exact(&mut byte).await.unwrap();
        request.push(byte[0]);
        assert!(request.len() <= 4096);
        if request.ends_with(b"\r\n\r\n") {
            break;
        }
    }
    let request = String::from_utf8(request).unwrap();
    let path = request.split_whitespace().nth(1).unwrap().to_owned();
    let length = request
        .lines()
        .find_map(|line| {
            line.to_ascii_lowercase()
                .strip_prefix("content-length: ")
                .map(|value| value.trim().parse::<usize>().unwrap())
        })
        .unwrap();
    assert!(length <= 1024);
    let mut body = vec![0; length];
    socket.read_exact(&mut body).await.unwrap();
    path
}
