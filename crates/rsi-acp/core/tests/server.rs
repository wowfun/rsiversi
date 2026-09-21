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
struct Backend {
    new_calls: AtomicUsize,
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

#[tokio::test]
async fn raw_validation_precedes_backend_and_load_drains_complete_replay() {
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
