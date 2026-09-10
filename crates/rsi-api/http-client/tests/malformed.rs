use futures_util::StreamExt;
use rsi_api_http_client::{HttpClient, HttpClientConfig};
use rsi_api_protocol::*;
use rsi_credentials_protocol::{CredentialRef, SecretValue};
use rsi_meta::Execution;
use std::time::Duration;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
};

const TOKEN: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
async fn receive(socket: &mut TcpStream) {
    let mut bytes = Vec::new();
    loop {
        let mut byte = [0];
        socket.read_exact(&mut byte).await.unwrap();
        bytes.push(byte[0]);
        assert!(bytes.len() < 4096);
        if bytes.ends_with(b"\r\n\r\n") {
            break;
        }
    }
    let text = String::from_utf8(bytes).unwrap();
    assert!(text.starts_with("POST /api/v1/"));
    let length: usize = text
        .lines()
        .find_map(|line| {
            line.to_ascii_lowercase()
                .strip_prefix("content-length: ")
                .map(str::to_owned)
        })
        .unwrap()
        .parse()
        .unwrap();
    assert!(length <= 1024);
    let mut body = vec![0; length];
    socket.read_exact(&mut body).await.unwrap();
}
async fn respond(
    socket: &mut TcpStream,
    status: u16,
    mime: &str,
    body: &[u8],
    declared: usize,
    extra: &str,
) {
    let headers = format!(
        "HTTP/1.1 {status} Response\r\nContent-Type: {mime}\r\nContent-Length: {declared}\r\nX-Rsi-Wire-Version: 1\r\nX-Rsi-Endpoint-Id: {}\r\nX-Rsi-Host-Epoch: {}\r\nConnection: close\r\n{extra}\r\n",
        EndpointId::from_bytes([2; 16]).as_str(),
        HostEpoch::from_bytes([3; 16]).as_str()
    );
    socket.write_all(headers.as_bytes()).await.unwrap();
    socket.write_all(body).await.unwrap();
    socket.shutdown().await.unwrap();
}
async fn fixture(
    operation: OperationSpec,
    status: u16,
    mime: &'static str,
    body: Vec<u8>,
    declared: usize,
    extra: &'static str,
) -> (HttpClient, tokio::task::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let origin = format!("http://{}", listener.local_addr().unwrap());
    let task = tokio::spawn(async move {
        for index in 0..3 {
            let (mut socket, _) = listener.accept().await.unwrap();
            receive(&mut socket).await;
            match index {
                0 => {
                    let bytes = serde_json::to_vec(&ConnectionDescription {
                        wire_version: 1,
                        endpoint_id: EndpointId::from_bytes([2; 16]),
                        host_epoch: HostEpoch::from_bytes([3; 16]),
                    })
                    .unwrap();
                    respond(
                        &mut socket,
                        200,
                        "application/json",
                        &bytes,
                        bytes.len(),
                        "",
                    )
                    .await;
                }
                1 => {
                    let bytes = serde_json::to_vec(&vec![
                        describe_operation(),
                        operations_operation(),
                        operation.clone(),
                    ])
                    .unwrap();
                    respond(
                        &mut socket,
                        200,
                        "application/json",
                        &bytes,
                        bytes.len(),
                        "",
                    )
                    .await;
                }
                _ => respond(&mut socket, status, mime, &body, declared, extra).await,
            }
        }
    });
    let client = HttpClient::connect(
        Execution::native(tokio::runtime::Handle::current()),
        HttpClientConfig {
            origin,
            endpoint_id: EndpointId::from_bytes([2; 16]),
            credential: CredentialRef::new("test", "device").unwrap(),
            tls_ca: None,
            allow_loopback_http: true,
        },
        SecretValue::new(TOKEN).unwrap(),
    )
    .await
    .unwrap();
    (client, task)
}
fn operation(effect: OperationEffect, class: OperationClass) -> OperationSpec {
    OperationSpec {
        id: OperationId::new("test", "operation", 1).unwrap(),
        effect,
        class,
        access: rsi_api_protocol::OperationAccess::Authenticated,
        encoding: RequestEncoding::Json,
        maximum_request_bytes: 128,
        maximum_response_bytes: 128,
    }
}

#[tokio::test]
async fn malformed_mutation_results_are_unknown_without_replay_and_reads_remain_failures() {
    for effect in [OperationEffect::Read, OperationEffect::Mutation] {
        for (status, mime, body, declared, extra) in [
            (200, "application/json", b"{".as_slice(), 1, ""),
            (200, "application/json", b"true".as_slice(), 5, ""),
            (
                200,
                "application/vnd.rsi.binary",
                b"short".as_slice(),
                5,
                "",
            ),
            (
                200,
                "application/json",
                b"true".as_slice(),
                4,
                "X-Rsi-Host-Epoch: 99999999999999999999999999999999\r\n",
            ),
            (
                200,
                "application/json",
                b"true".as_slice(),
                4,
                "Content-Encoding: gzip\r\n",
            ),
            (
                302,
                "application/json",
                b"{}".as_slice(),
                2,
                "Location: http://127.0.0.1:1/\r\n",
            ),
            (
                401,
                "application/json",
                br#"{"code":"capacity"}"#.as_slice(),
                19,
                "",
            ),
        ] {
            let operation = operation(effect, OperationClass::Data);
            let (client, peer) = fixture(
                operation.clone(),
                status,
                mime,
                body.to_vec(),
                declared,
                extra,
            )
            .await;
            let input = client.input_budget(operation.class).copy(b"{}").unwrap();
            let result = client.call(&operation, input).await;
            if effect == OperationEffect::Mutation {
                assert!(
                    matches!(result, Err(ApiError::OutcomeUnknown)),
                    "{result:?}"
                );
            } else {
                assert!(result.is_err());
            }
            client.close().await;
            peer.await.unwrap();
        }
    }
}

#[tokio::test]
async fn subscription_eof_trailing_frames_and_incomplete_errors_never_become_clean_end() {
    for body in [
        "event: item\ndata: true\n\n",
        "event: item\ndata: true\n\nevent: end\ndata: {}\n",
        "event: end\ndata: {}\n\nevent: item\ndata: true\n\n",
        "event: error\ndata: {\"code\":\"capacity\"}\n\n",
        "event: end\ndata: {}\n\nx",
    ] {
        let operation = operation(OperationEffect::Read, OperationClass::Subscription);
        let (client, peer) = fixture(
            operation.clone(),
            200,
            "text/event-stream",
            body.as_bytes().to_vec(),
            body.len(),
            "",
        )
        .await;
        let input = client.input_budget(operation.class).copy(b"{}").unwrap();
        let ApiOutput::Stream(mut stream) = client.call(&operation, input).await.unwrap() else {
            panic!("stream")
        };
        let results = tokio::time::timeout(Duration::from_secs(2), async {
            let mut results = Vec::new();
            while let Some(result) = stream.next().await {
                results.push(result);
            }
            results
        })
        .await
        .unwrap();
        assert!(
            results.last().is_some_and(Result::is_err),
            "false clean EOF for {body}"
        );
        client.close().await;
        peer.await.unwrap();
    }
}

#[tokio::test]
async fn valid_known_rejections_and_domain_errors_keep_their_meaning() {
    for (status, mime, body) in [
        (401, "application/json", r#"{"code":"unauthorized"}"#),
        (
            422,
            "application/vnd.rsi.domain-error+json",
            r#"{"code":"message_conflict"}"#,
        ),
    ] {
        let operation = operation(OperationEffect::Mutation, OperationClass::Data);
        let (client, peer) = fixture(
            operation.clone(),
            status,
            mime,
            body.as_bytes().to_vec(),
            body.len(),
            "",
        )
        .await;
        let input = client.input_budget(operation.class).copy(b"{}").unwrap();
        let result = client.call(&operation, input).await;
        if status == 401 {
            assert!(matches!(result, Err(ApiError::Unauthorized)));
        } else {
            let Err(ApiError::Domain(bytes)) = result else {
                panic!("domain")
            };
            assert_eq!(bytes.as_bytes(), body.as_bytes());
        }
        client.close().await;
        peer.await.unwrap();
    }
}
