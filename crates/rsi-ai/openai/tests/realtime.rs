use std::{
    collections::VecDeque,
    fmt,
    sync::{Arc, Mutex},
    time::Duration,
};

use async_trait::async_trait;
use rsi_ai::{ModelRef, Registry};
use rsi_ai_auth::{CredentialManager, CredentialRequirement, SecretValue};
use rsi_ai_openai::{
    OpenAiConfig, OpenAiRealtimeAdapter, RealtimeDialer, RealtimeJsonSocket, TokioRealtimeDialer,
};
use rsi_ai_protocol::{RealtimeCommand, RealtimeEvent, RealtimeRequest};
use rsi_ai_provider::{AbortSignal, AdapterFuture, ProviderRegistration};
use serde_json::{Value, json};

#[derive(Clone, Debug)]
struct FakeDialer {
    state: Arc<Mutex<FakeState>>,
}

#[derive(Debug)]
struct FakeState {
    incoming: VecDeque<Value>,
    sent: Vec<Value>,
    url: Option<String>,
    authenticated: bool,
    closed: bool,
}

impl FakeDialer {
    fn new(incoming: Vec<Value>) -> Self {
        Self {
            state: Arc::new(Mutex::new(FakeState {
                incoming: incoming.into(),
                sent: Vec::new(),
                url: None,
                authenticated: false,
                closed: false,
            })),
        }
    }
}

impl RealtimeDialer for FakeDialer {
    fn connect(
        &self,
        url: String,
        credential: SecretValue,
        _abort: AbortSignal,
    ) -> AdapterFuture<Result<Box<dyn RealtimeJsonSocket>, rsi_ai_protocol::AiError>> {
        let state = Arc::clone(&self.state);
        Box::pin(async move {
            {
                let mut guard = state.lock().expect("fake socket lock");
                guard.url = Some(url);
                guard.authenticated = credential.expose() == "realtime-secret";
            }
            Ok(Box::new(FakeSocket { state }) as Box<dyn RealtimeJsonSocket>)
        })
    }
}

struct FakeSocket {
    state: Arc<Mutex<FakeState>>,
}

impl fmt::Debug for FakeSocket {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("FakeSocket(..)")
    }
}

#[async_trait]
impl RealtimeJsonSocket for FakeSocket {
    async fn send_json(&mut self, value: Value) -> Result<(), rsi_ai_protocol::AiError> {
        self.state
            .lock()
            .expect("fake socket lock")
            .sent
            .push(value);
        Ok(())
    }

    async fn next_json(&mut self) -> Result<Option<Value>, rsi_ai_protocol::AiError> {
        Ok(self
            .state
            .lock()
            .expect("fake socket lock")
            .incoming
            .pop_front())
    }

    async fn close(&mut self) -> Result<(), rsi_ai_protocol::AiError> {
        self.state.lock().expect("fake socket lock").closed = true;
        Ok(())
    }
}

#[tokio::test]
async fn realtime_maps_live_commands_and_provider_close_without_replay() {
    let dialer = FakeDialer::new(vec![
        json!({"type":"session.created", "session":{"id":"session-1"}}),
        json!({"type":"response.output_audio_transcript.delta", "response_id":"response-1", "delta":"hello"}),
        json!({"type":"response.output_audio.delta", "response_id":"response-1", "delta":"AAE="}),
        json!({"type":"response.output_audio.delta", "response_id":"response-1", "delta":"AgM="}),
    ]);
    let config = OpenAiConfig::new("http://127.0.0.1:1").expect("loopback config");
    let adapter = OpenAiRealtimeAdapter::new(config, Arc::new(dialer.clone()));
    let registry = Registry::builder(
        CredentialManager::builder()
            .with_explicit("openai", "realtime-secret")
            .expect("credential")
            .build(),
    )
    .register(
        ProviderRegistration::builder("openai", "openai")
            .expect("registration")
            .with_credential(
                CredentialRequirement::new("openai", ["OPENAI_API_KEY"]).expect("requirement"),
            )
            .with_realtime(adapter)
            .build()
            .expect("provider"),
    )
    .expect("register")
    .build()
    .expect("registry");

    let mut session = registry
        .realtime(ModelRef::new("openai", "gpt-realtime").expect("model"))
        .expect("realtime")
        .connect(
            RealtimeRequest::new("alloy")
                .expect("request")
                .with_instructions("be concise")
                .expect("instructions"),
        )
        .await
        .expect("connect");
    assert!(matches!(
        session.next_event().await.expect("started"),
        Some(RealtimeEvent::SessionStarted { .. })
    ));
    session
        .send(RealtimeCommand::AppendAudio {
            sequence: 1,
            bytes: vec![0, 1],
        })
        .await
        .expect("append audio");
    assert!(matches!(
        session.next_event().await.expect("text"),
        Some(RealtimeEvent::OutputTextDelta { .. })
    ));
    assert!(matches!(
        session.next_event().await.expect("first audio"),
        Some(RealtimeEvent::OutputAudioChunk { sequence: 1, .. })
    ));
    assert!(matches!(
        session.next_event().await.expect("second audio"),
        Some(RealtimeEvent::OutputAudioChunk { sequence: 2, .. })
    ));
    assert!(matches!(
        session.next_event().await.expect("closed"),
        Some(RealtimeEvent::Closed { .. })
    ));

    let state = dialer.state.lock().expect("fake socket lock");
    assert!(state.authenticated);
    assert!(
        state
            .url
            .as_deref()
            .is_some_and(|url| url.starts_with("ws://127.0.0.1:1/v1/realtime?model="))
    );
    assert_eq!(state.sent[0]["type"], "session.update");
    assert_eq!(state.sent[0]["session"]["type"], "realtime");
    assert_eq!(
        state.sent[0]["session"]["output_modalities"],
        json!(["audio"])
    );
    assert_eq!(
        state.sent[0]["session"]["audio"]["input"]["format"]["type"],
        "audio/pcm"
    );
    assert_eq!(
        state.sent[0]["session"]["audio"]["input"]["format"]["rate"],
        24_000
    );
    assert_eq!(
        state.sent[0]["session"]["audio"]["output"]["voice"],
        "alloy"
    );
    assert_eq!(state.sent[1]["type"], "input_audio_buffer.append");
    assert_eq!(state.sent[1]["audio"], "AAE=");
}

#[tokio::test]
async fn production_realtime_socket_observes_abort_after_connect() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("listen");
    let address = listener.local_addr().expect("address");
    let server = tokio::spawn(async move {
        let (socket, _) = listener.accept().await.expect("accept");
        let _socket = tokio_tungstenite::accept_async(socket)
            .await
            .expect("websocket handshake");
        std::future::pending::<()>().await;
    });
    let abort = AbortSignal::new();
    let mut socket = TokioRealtimeDialer::default()
        .connect(
            format!("ws://{address}/v1/realtime?model=test"),
            SecretValue::new("realtime-secret").expect("secret"),
            abort.clone(),
        )
        .await
        .expect("connect");

    abort.abort();
    let error = tokio::time::timeout(Duration::from_millis(250), socket.next_json())
        .await
        .expect("socket observes cancellation")
        .expect_err("cancelled socket");
    assert_eq!(error.kind(), rsi_ai_protocol::ErrorKind::Cancelled);
    server.abort();
}

#[tokio::test]
async fn production_realtime_dialer_bounds_a_stalled_handshake() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("listen");
    let address = listener.local_addr().expect("address");
    let server = tokio::spawn(async move {
        let (_socket, _) = listener.accept().await.expect("accept");
        std::future::pending::<()>().await;
    });
    let dialer =
        TokioRealtimeDialer::with_connect_timeout(Duration::from_millis(50)).expect("dialer");
    let error = dialer
        .connect(
            format!("ws://{address}/v1/realtime?model=test"),
            SecretValue::new("realtime-secret").expect("secret"),
            AbortSignal::new(),
        )
        .await
        .expect_err("handshake timeout");
    assert_eq!(error.kind(), rsi_ai_protocol::ErrorKind::Timeout);
    server.abort();
}
