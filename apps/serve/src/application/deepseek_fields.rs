use async_trait::async_trait;
use bytes::Bytes;
use futures_util::{SinkExt, StreamExt};
use rsi_ai_deepseek::{AcceptedFields, DeepSeekFieldSource, FieldPreparation, PreparedFields};
use rsi_ai_protocol::LanguageCallOrigin;
use rsi_ai_transport::{MAX_PROVIDER_REQUEST_BODY_BYTES, TransportError};
use rsi_credentials_protocol::SecretValue;
use std::sync::{
    Arc, RwLock,
    atomic::{AtomicBool, Ordering},
};
use tokio::net::TcpStream;
use tokio_tungstenite::{
    WebSocketStream, client_async_with_config,
    tungstenite::{Message, client::IntoClientRequest, protocol::WebSocketConfig},
};
use tokio_util::sync::CancellationToken;

#[derive(Debug, Default)]
pub(super) struct Source(RwLock<Option<Arc<Endpoint>>>);

#[derive(Debug)]
struct Endpoint {
    port: u16,
    key: SecretValue,
    retired: CancellationToken,
    enabled: Arc<AtomicBool>,
}

impl Source {
    pub(super) fn publish(
        &self,
        port: u16,
        key: SecretValue,
        retired: CancellationToken,
        enabled: Arc<AtomicBool>,
    ) {
        *self.0.write().expect("field endpoint lock") = Some(Arc::new(Endpoint {
            port,
            key,
            retired,
            enabled,
        }));
    }
}

impl DeepSeekFieldSource for Source {
    fn current(&self) -> Option<Arc<dyn FieldPreparation>> {
        let endpoint = self.0.read().expect("field endpoint lock");
        let endpoint = endpoint.as_ref()?;
        (endpoint.enabled.load(Ordering::Acquire) && !endpoint.retired.is_cancelled())
            .then(|| endpoint.clone() as _)
    }
}

fn disconnected() -> TransportError {
    TransportError::new(
        "http.fields_disconnected",
        "Application request-field transaction disconnected",
    )
}

type Socket = WebSocketStream<TcpStream>;

#[async_trait]
impl FieldPreparation for Endpoint {
    async fn prepare(
        &self,
        origin: &LanguageCallOrigin,
        body: Bytes,
        stop: &CancellationToken,
    ) -> Result<PreparedFields, TransportError> {
        let exchange = async {
            let mut request = format!("ws://127.0.0.1:{}/rsi-model-fields", self.port)
                .into_client_request()
                .map_err(|_| disconnected())?;
            request.headers_mut().insert(
                "x-rsi-model-key",
                self.key
                    .expose_secret()
                    .parse()
                    .map_err(|_| disconnected())?,
            );
            let tcp = TcpStream::connect((std::net::Ipv4Addr::LOCALHOST, self.port))
                .await
                .map_err(|_| disconnected())?;
            let config = WebSocketConfig::default()
                .max_frame_size(Some(MAX_PROVIDER_REQUEST_BODY_BYTES))
                .max_message_size(Some(MAX_PROVIDER_REQUEST_BODY_BYTES));
            let (mut socket, _) = client_async_with_config(request, tcp, Some(config))
                .await
                .map_err(|_| disconnected())?;
            let origin = serde_json::json!({"sessionId":origin.session_id,"purpose":origin.purpose,"activeSession":origin.active_session});
            socket
                .feed(Message::text(origin.to_string()))
                .await
                .map_err(|_| disconnected())?;
            socket
                .send(Message::Binary(body))
                .await
                .map_err(|_| disconnected())?;
            let Some(Ok(Message::Binary(fields))) = socket.next().await else {
                return Err(disconnected());
            };
            let receipt = (fields.as_ref() != b"{}").then(|| {
                Box::new(Receipt(socket, self.retired.clone())) as Box<dyn AcceptedFields>
            });
            Ok(PreparedFields { fields, receipt })
        };
        tokio::select! { biased;
            () = stop.cancelled() => Err(TransportError::new("http.cancelled","Model call cancelled")),
            () = self.retired.cancelled() => Err(disconnected()),
            result = exchange => result,
        }
    }
}

struct Receipt(Socket, CancellationToken);
impl std::fmt::Debug for Receipt {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Application field receipt")
    }
}
#[async_trait]
impl AcceptedFields for Receipt {
    async fn accept(mut self: Box<Self>) -> Result<(), TransportError> {
        let exchange = async {
            self.0
                .send(Message::text("accept"))
                .await
                .map_err(|_| disconnected())?;
            match self.0.next().await {
                Some(Ok(Message::Text(reply))) if reply.as_str() == "ok" => Ok(()),
                _ => Err(disconnected()),
            }
        };
        tokio::select! {
            () = self.1.cancelled() => Err(disconnected()),
            result = exchange => result,
        }
    }
}
