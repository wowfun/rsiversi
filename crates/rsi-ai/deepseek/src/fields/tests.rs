use super::*;
use crate::{DeepSeekAdapter, DeepSeekConfig, DeepSeekProtocol};
use rsi_ai_protocol::{DispatchStatus, ErrorKind, LanguageModelLimits, LanguageRequest, Message};
use rsi_ai_provider::MissingMediaResolver;
use rsi_ai_testkit::{complete_language, language_context};
use rsi_credentials_protocol::{CredentialSource, ResolvedCredential, SecretValue};
use rsi_meta::{
    ActivationPlan, ConfigValue, PluginFactory, PreparedActivation, ResolvedFactory, Runtime,
    UpdateMode,
};
use std::sync::{
    Mutex,
    atomic::{AtomicBool, AtomicUsize, Ordering},
};

#[tokio::test]
#[ignore = "requires an explicit DEEPSEEK_API_KEY; two official requests spend live quota"]
async fn official_endpoints_accept_the_native_segmented_request_body() {
    use rsi_ai_transport::{ReqwestTransport, collect_body};
    use serde_json::json;

    let key = SecretValue::new(std::env::var("DEEPSEEK_API_KEY").expect("explicit credential"))
        .expect("valid credential");
    let transport = ReqwestTransport::new().unwrap();
    let message = json!({"role":"user","content":"Reply OK."});
    let fields = Bytes::from(serde_json::to_vec(&json!({"dsh_session_log":{
        "version":1,"sessionFormatVersion":4,"session":{"version":4,"id":"synthetic","createdAt":0},
        "afterSeq":-1,"throughSeq":0,"events":[{"seq":0,"time":0,"type":"turn/start","data":{"turn":1}}]
    }})).unwrap());
    for (path, mut body, output) in [
        (
            "/responses",
            json!({"input":[message],"max_output_tokens":64,"reasoning":{"effort":"none"}}),
            "output",
        ),
        (
            "/chat/completions",
            json!({"messages":[message],"max_tokens":64,"thinking":{"type":"disabled"}}),
            "choices",
        ),
    ] {
        body["model"] = json!("deepseek-flash");
        let request = HttpRequest::new(
            "POST".parse().unwrap(),
            format!("https://api.deepseek.com{path}"),
        )
        .unwrap()
        .bearer_auth(&key)
        .unwrap()
        .header(
            "content-type".parse().unwrap(),
            "application/json".parse().unwrap(),
        )
        .unwrap();
        let outgoing = extend(
            request,
            serde_json::to_vec(&body).unwrap().into(),
            fields.clone(),
        )
        .unwrap();
        let (status, body) = tokio::time::timeout(std::time::Duration::from_secs(90), async {
            let response = transport
                .execute(outgoing, CancellationToken::new())
                .await
                .unwrap();
            (
                response.status,
                collect_body(response.body, 512 * 1024).await.unwrap(),
            )
        })
        .await
        .expect("provider timed out");
        assert!((200..300).contains(&status), "{path} HTTP {status}");
        let result: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert!(
            result[output]
                .as_array()
                .is_some_and(|items| !items.is_empty())
        );
        println!("{path} HTTP {status}, received model output");
    }
}

#[derive(Debug, Default)]
struct Contributor {
    enabled: AtomicBool,
    collide: AtomicBool,
    confirmed: Arc<AtomicUsize>,
    stall_acceptance: bool,
}

#[derive(Debug)]
struct Source(Arc<Contributor>);
impl DeepSeekFieldSource for Source {
    fn current(&self) -> Option<Arc<dyn FieldPreparation>> {
        self.0
            .enabled
            .load(Ordering::Relaxed)
            .then(|| self.0.clone() as _)
    }
}

#[async_trait]
impl PluginFactory for Source {
    fn prepare(&self, desired: &ConfigValue) -> rsi_meta::Result<PreparedActivation> {
        Ok(PreparedActivation::new(desired.clone()))
    }
    async fn activate(&self, plan: ActivationPlan) -> rsi_meta::Result<()> {
        plan.context()
            .provide_local::<DeepSeekFieldSourceContract>(Arc::new(Source(self.0.clone())))?;
        Ok(())
    }
}

#[async_trait]
impl FieldPreparation for Contributor {
    async fn prepare(
        &self,
        origin: &LanguageCallOrigin,
        base: Bytes,
        _: &CancellationToken,
    ) -> Result<PreparedFields, TransportError> {
        assert_eq!(origin.session_id.as_deref(), Some("s"));
        let body: serde_json::Value = serde_json::from_slice(&base).unwrap();
        assert!(body.get("input").is_some() || body.get("messages").is_some());
        let name = if self.collide.load(Ordering::Relaxed) {
            "model"
        } else {
            "unmodified_plugin_field"
        };
        Ok(PreparedFields {
            fields: serde_json::to_vec(
                &serde_json::json!({(name):self.confirmed.load(Ordering::Relaxed)}),
            )
            .unwrap()
            .into(),
            receipt: Some(Box::new(Receipt {
                confirmed: self.confirmed.clone(),
                stall: self.stall_acceptance,
            })),
        })
    }
}

#[derive(Debug)]
struct Receipt {
    confirmed: Arc<AtomicUsize>,
    stall: bool,
}
#[async_trait]
impl AcceptedFields for Receipt {
    async fn accept(self: Box<Self>) -> Result<(), TransportError> {
        if self.stall {
            std::future::pending::<()>().await;
        }
        self.confirmed.fetch_add(1, Ordering::Relaxed);
        Ok(())
    }
}

#[derive(Debug, Default)]
struct Wire(Mutex<Vec<(String, serde_json::Value)>>);
#[async_trait]
impl HttpTransport for Wire {
    async fn execute(
        &self,
        mut request: HttpRequest,
        _: CancellationToken,
    ) -> Result<HttpResponse, TransportError> {
        let body = serde_json::from_slice(&request.take_json_bytes().await?).unwrap();
        let mut requests = self.0.lock().unwrap();
        requests.push((request.url().path().to_owned(), body));
        Ok(HttpResponse {
            status: match requests.len() {
                2 => 503,
                3 => 201,
                4 => 204,
                _ => 200,
            },
            headers: Default::default(),
            body: Box::pin(stream::empty()),
        })
    }
}

#[tokio::test]
async fn cancellation_releases_a_pending_field_receipt_after_http_acceptance() {
    use futures_util::{FutureExt, poll};

    let runtime = Runtime::default();
    let contributor = Arc::new(Contributor {
        enabled: AtomicBool::new(true),
        stall_acceptance: true,
        ..Contributor::default()
    });
    let _source = runtime
        .root()
        .apply(
            ResolvedFactory::linked(
                "fields",
                "test",
                UpdateMode::Replayable,
                Arc::new(Source(contributor.clone())),
            ),
            ConfigValue::Null,
        )
        .await
        .unwrap();
    let wire = Arc::new(Wire::default());
    let transport = Transport {
        inner: wire.clone(),
        context: runtime.root(),
    };
    let request = HttpRequest::new("POST".parse().unwrap(), "http://localhost/responses")
        .unwrap()
        .body(Bytes::from_static(br#"{"input":[]}"#));
    let origin = LanguageCallOrigin {
        session_id: Some("s".into()),
        ..LanguageCallOrigin::default()
    };
    let stop = CancellationToken::new();
    let mut send = Box::pin(transport.execute_language(request, &origin, stop.clone()));
    assert!(poll!(&mut send).is_pending());
    assert_eq!(wire.0.lock().unwrap().len(), 1);
    stop.cancel();
    let response = send
        .now_or_never()
        .expect("cancellation must release the pending receipt")
        .unwrap();
    assert_eq!(response.status, 200);
    assert_eq!(contributor.confirmed.load(Ordering::Relaxed), 0);
}

#[tokio::test]
async fn either_protocol_confirms_at_headers_and_preserves_the_pending_prefix_after_503() {
    for (protocol, path) in [
        (DeepSeekProtocol::Responses, "/responses"),
        (DeepSeekProtocol::ChatCompletions, "/chat/completions"),
    ] {
        let runtime = Runtime::default();
        let contributor = Arc::new(Contributor::default());
        let wire = Arc::new(Wire::default());
        let _source = runtime
            .root()
            .apply(
                ResolvedFactory::linked(
                    "fields",
                    "test",
                    UpdateMode::Replayable,
                    Arc::new(Source(contributor.clone())),
                ),
                ConfigValue::Null,
            )
            .await
            .unwrap();
        let adapter = DeepSeekAdapter::new(
            DeepSeekConfig::with_endpoint("http://127.0.0.1:1")
                .unwrap()
                .with_protocol(protocol)
                .with_default_output_tokens("fixture", 1024)
                .unwrap()
                .with_model_profile(
                    "fixture",
                    LanguageModelLimits::new(65536, 1024, 2048).unwrap(),
                )
                .unwrap(),
            Arc::new(Transport {
                inner: wire.clone(),
                context: runtime.root(),
            }),
        );
        let run = || {
            let context = language_context(
                "deepseek",
                "fixture",
                "fixture",
                Some(ResolvedCredential {
                    secret: SecretValue::new("loopback").unwrap(),
                    source: CredentialSource::Keyring,
                }),
                Arc::new(MissingMediaResolver),
                0,
            )
            .with_origin(LanguageCallOrigin {
                active_session: true,
                session_id: Some("s".into()),
                purpose: None,
            });
            complete_language(
                &adapter,
                context,
                "fixture",
                LanguageRequest::new(vec![Message::user_text("synthetic").unwrap()]).unwrap(),
            )
        };
        // Every received response lacks its SSE body, including the two accepted calls.
        assert!(run().await.is_err());
        contributor.enabled.store(true, Ordering::Relaxed);
        assert!(run().await.is_err());
        assert_eq!(contributor.confirmed.load(Ordering::Relaxed), 0);
        assert!(run().await.is_err());
        assert_eq!(contributor.confirmed.load(Ordering::Relaxed), 1);
        assert!(run().await.is_err());
        contributor.collide.store(true, Ordering::Relaxed);
        let collision = run().await.unwrap_err();
        let collision = collision.provider_error().unwrap();
        assert_eq!(collision.kind(), ErrorKind::InvalidRequest);
        assert_eq!(collision.dispatch_status(), DispatchStatus::NotDispatched);
        let captured = wire.0.lock().unwrap();
        assert!(captured.iter().all(|(url, _)| url == path));
        let starts: Vec<_> = captured
            .iter()
            .map(|(_, body)| {
                body.get("unmodified_plugin_field")
                    .and_then(serde_json::Value::as_u64)
            })
            .collect();
        assert_eq!(starts, [None, Some(0), Some(0), Some(1)]);
        assert_eq!(contributor.confirmed.load(Ordering::Relaxed), 2);
    }
}
