use async_trait::async_trait;
use rsi_acp::{
    Peer, PeerHandle, StreamTransport,
    server::{AgentBackend, Failure},
};
use rsi_acp_protocol::{Message, schema};
use serde::de::DeserializeOwned;
use serde_json::{Value, json};
use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicUsize, Ordering},
};
use tokio_util::sync::CancellationToken;

fn dto<T: DeserializeOwned>(value: Value) -> T {
    serde_json::from_value(value).unwrap()
}

#[derive(Debug, Default)]
struct DeliveredResponseEof {
    read_failure: Option<rsi_acp::Error>,
    reads: AtomicUsize,
    written: tokio::sync::Notify,
    eof: tokio::sync::Notify,
    finish_close: CancellationToken,
}
#[async_trait]
impl rsi_acp::Transport for DeliveredResponseEof {
    async fn read(&self) -> Result<Vec<u8>, rsi_acp::Error> {
        if self.reads.fetch_add(1, Ordering::SeqCst) == 0 {
            return Ok(b"{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"initialize\",\"params\":{\"protocolVersion\":1,\"clientCapabilities\":{}}}\n".to_vec());
        }
        self.eof.notified().await;
        self.read_failure.map_or_else(|| Ok(vec![]), Err)
    }
    async fn write(&self, bytes: &[u8]) -> Result<(), rsi_acp::Error> {
        // The caller has received a complete frame while the transport still owns
        // native write settlement. EOF retires that detached observation.
        let response: Value = serde_json::from_slice(bytes).unwrap();
        assert_eq!(response["id"], 1);
        self.written.notify_one();
        std::future::pending().await
    }
    async fn flush(&self) -> Result<(), rsi_acp::Error> {
        Ok(())
    }
    async fn close(&self) -> Result<(), rsi_acp::Error> {
        self.finish_close.cancelled().await;
        Ok(())
    }
}

async fn delivered_response_close(
    read_failure: Option<rsi_acp::Error>,
) -> Result<(), rsi_acp::Error> {
    let transport = Arc::new(DeliveredResponseEof {
        read_failure,
        ..DeliveredResponseEof::default()
    });
    let backend = Arc::new(Backend::default());
    let owner = tokio::spawn(rsi_acp::server::run(
        Peer::start(transport.clone()),
        backend.clone(),
        CancellationToken::new(),
    ));
    tokio::time::timeout(
        std::time::Duration::from_secs(2),
        transport.written.notified(),
    )
    .await
    .unwrap();
    transport.eof.notify_one();
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        while !backend.stopped.load(Ordering::SeqCst) {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    transport.finish_close.cancel();
    tokio::time::timeout(std::time::Duration::from_secs(2), owner)
        .await
        .unwrap()
        .unwrap()
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn clean_eof_during_delivered_response_observation_still_joins_healthy_shutdown() {
    assert_eq!(delivered_response_close(None).await, Ok(()));
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn driver_failure_during_delivered_response_observation_remains_unhealthy() {
    assert_eq!(
        delivered_response_close(Some(rsi_acp::Error::Closed)).await,
        Err(rsi_acp::Error::Closed)
    );
}
#[derive(Debug, Default)]
struct Backend {
    new_calls: AtomicUsize,
    configuration_calls: AtomicUsize,
    stopped: AtomicBool,
    active: AtomicBool,
    cancel_early: AtomicBool,
    cancelled: tokio::sync::Notify,
}
#[async_trait]
impl AgentBackend for Backend {
    async fn initialize(
        &self,
        _: schema::InitializeRequest,
    ) -> Result<schema::InitializeResponse, Failure> {
        Ok(dto(
            json!({"protocolVersion":1,"agentCapabilities":{"loadSession":true},"authMethods":[]}),
        ))
    }
    async fn new_session(
        &self,
        _: schema::NewSessionRequest,
    ) -> Result<schema::NewSessionResponse, Failure> {
        self.new_calls.fetch_add(1, Ordering::SeqCst);
        Ok(dto(json!({"sessionId":"fixture"})))
    }
    async fn load(
        &self,
        _: schema::LoadSessionRequest,
        peer: PeerHandle,
    ) -> Result<schema::LoadSessionResponse, Failure> {
        for index in 0..4 {
            peer.notify("session/update", &json!({"sessionId":"fixture","update":{"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":index.to_string()}}})).await.map_err(|_| Failure::Backend)?;
        }
        Ok(dto(json!({})))
    }
    async fn resume(
        &self,
        _: schema::ResumeSessionRequest,
    ) -> Result<schema::ResumeSessionResponse, Failure> {
        Ok(dto(json!({})))
    }
    async fn list(
        &self,
        _: schema::ListSessionsRequest,
    ) -> Result<schema::ListSessionsResponse, Failure> {
        Ok(dto(json!({"sessions":[]})))
    }
    async fn prompt(
        &self,
        request: schema::PromptRequest,
        _: PeerHandle,
    ) -> Result<schema::PromptResponse, Failure> {
        if !request.prompt.is_empty() {
            self.active.store(true, Ordering::SeqCst);
            self.cancelled.notified().await;
            self.active.store(false, Ordering::SeqCst);
            return Ok(dto(json!({"stopReason":"cancelled"})));
        }
        Ok(dto(json!({"stopReason":"end_turn"})))
    }
    async fn cancel(&self, _: schema::CancelNotification) -> Result<(), Failure> {
        if !self.active.load(Ordering::SeqCst) {
            self.cancel_early.store(true, Ordering::SeqCst);
        }
        self.cancelled.notify_one();
        Ok(())
    }
    async fn set_config_option(
        &self,
        request: schema::SetSessionConfigOptionRequest,
        peer: PeerHandle,
    ) -> Result<schema::SetSessionConfigOptionResponse, Failure> {
        self.configuration_calls.fetch_add(1, Ordering::SeqCst);
        let request = serde_json::to_value(request).unwrap();
        let options = json!([{"id":"model","name":"Model","type":"select","currentValue":request["value"],"options":[{"value":"a","name":"A"},{"value":"b","name":"B"}]}]);
        peer.notify("session/update", &json!({"sessionId":"fixture","update":{"sessionUpdate":"config_option_update","configOptions":options}}))
            .await.map_err(|_| Failure::Backend)?;
        Ok(dto(json!({"configOptions":options})))
    }
    async fn close(
        &self,
        _: schema::CloseSessionRequest,
    ) -> Result<schema::CloseSessionResponse, Failure> {
        Ok(dto(json!({})))
    }
    async fn shutdown(&self) -> Result<(), Failure> {
        self.stopped.store(true, Ordering::SeqCst);
        Ok(())
    }
}

async fn assert_configuration_update(
    port: &rsi_acp::PeerHandle,
    client: &mut Peer,
    backend: &Backend,
) {
    let request = |method: &'static str, params: Value| {
        let port = port.clone();
        async move {
            port.request(method, &params, CancellationToken::new())
                .await
                .unwrap()
        }
    };
    assert_eq!(
        request(
            "session/set_config_option",
            json!({"sessionId":"fixture","configId":"model","value":true})
        )
        .await
        .result()
        .unwrap_err()["code"],
        -32602
    );
    assert_eq!(backend.configuration_calls.load(Ordering::SeqCst), 0);
    let configured = request(
        "session/set_config_option",
        json!({"sessionId":"fixture","configId":"model","value":"b"}),
    )
    .await;
    assert_eq!(
        configured.result().unwrap()["configOptions"][0]["currentValue"],
        "b"
    );
    let event = client.next().await.unwrap();
    let Message::Notification { method, params } = event.message else {
        panic!("configuration update")
    };
    assert_eq!(method, "session/update");
    assert_eq!(params["update"]["sessionUpdate"], "config_option_update");
    assert_eq!(
        &params["update"]["configOptions"],
        &configured.result().unwrap()["configOptions"]
    );
    assert_eq!(backend.configuration_calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn raw_validation_precedes_backend_and_responses_follow_complete_updates() {
    let (left, right) = tokio::io::duplex(65536);
    let (read, write) = tokio::io::split(left);
    let mut client = Peer::start(StreamTransport::new(read, write));
    let (read, write) = tokio::io::split(right);
    let server = Peer::start(StreamTransport::new(read, write));
    let backend = Arc::new(Backend::default());
    let running = tokio::spawn(rsi_acp::server::run(
        server,
        backend.clone(),
        CancellationToken::new(),
    ));
    let port = client.handle();
    let request = |method: &'static str, params: Value| {
        let port = port.clone();
        async move {
            port.request(method, &params, CancellationToken::new())
                .await
                .unwrap()
        }
    };
    assert_eq!(
        request(
            "session/new",
            json!({"cwd":std::env::current_dir().unwrap(),"mcpServers":[]})
        )
        .await
        .result()
        .unwrap_err()["code"],
        -32002
    );
    request(
        "initialize",
        json!({"protocolVersion":1,"clientCapabilities":{}}),
    )
    .await
    .result()
    .unwrap();
    assert_eq!(
        request(
            "session/new",
            json!({"cwd":std::env::current_dir().unwrap(),"mcpServers":[{}]})
        )
        .await
        .result()
        .unwrap_err()["code"],
        -32602
    );
    assert_eq!(backend.new_calls.load(Ordering::SeqCst), 0);
    assert_eq!(
        request(
            "session/new",
            json!({"cwd":std::env::current_dir().unwrap(),"mcpServers":[]})
        )
        .await
        .result()
        .unwrap()["sessionId"],
        "fixture"
    );
    let load = tokio::spawn(request(
        "session/load",
        json!({"sessionId":"fixture","cwd":std::env::current_dir().unwrap(),"mcpServers":[]}),
    ));
    for index in 0..4 {
        let event = client.next().await.unwrap();
        let Message::Notification { params, .. } = event.message else {
            panic!("update");
        };
        assert_eq!(params["update"]["content"]["text"], index.to_string());
    }
    load.await.unwrap().result().unwrap();
    assert_configuration_update(&port, &mut client, &backend).await;
    assert_eq!(
        request("fs/read_text_file", json!({}))
            .await
            .result()
            .unwrap_err()["code"],
        -32601
    );
    client.close().await.unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(2), running)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert!(backend.stopped.load(Ordering::SeqCst));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn wire_order_admits_prompt_before_immediately_following_cancel() {
    use tokio::io::{AsyncBufReadExt as _, AsyncWriteExt as _};
    let (local, remote) = tokio::io::duplex(65536);
    let (local_read, local_write) = tokio::io::split(local);
    let (read, mut write) = tokio::io::split(remote);
    let mut read = tokio::io::BufReader::new(read);
    let backend = Arc::new(Backend::default());
    let task = tokio::spawn(rsi_acp::server::run(
        Peer::start(StreamTransport::new(local_read, local_write)),
        backend.clone(),
        CancellationToken::new(),
    ));
    write.write_all(b"{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"initialize\",\"params\":{\"protocolVersion\":1,\"clientCapabilities\":{}}}\n").await.unwrap();
    let mut line = String::new();
    read.read_line(&mut line).await.unwrap();
    for index in 2..1002 {
        let prompt = json!({"jsonrpc":"2.0","id":index,"method":"session/prompt","params":{"sessionId":"fixture","prompt":[{"type":"text","text":"wait"}]}});
        let cancel =
            json!({"jsonrpc":"2.0","method":"session/cancel","params":{"sessionId":"fixture"}});
        write
            .write_all(format!("{prompt}\n{cancel}\n").as_bytes())
            .await
            .unwrap();
        line.clear();
        tokio::time::timeout(std::time::Duration::from_secs(2), read.read_line(&mut line))
            .await
            .unwrap()
            .unwrap();
        assert!(
            !backend.cancel_early.load(Ordering::SeqCst),
            "cancel overtook prompt at iteration {index}"
        );
        let response: Value = serde_json::from_str(&line).unwrap();
        assert_eq!(response["result"]["stopReason"], "cancelled");
    }
    write.shutdown().await.unwrap();
    task.await.unwrap().unwrap();
}
