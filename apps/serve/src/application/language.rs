use super::BoundSessions;
use futures_util::StreamExt;
use rsi_ai_protocol::{LanguageCallOrigin, LanguageEvent, LanguageRequest, ModelRef};
use rsi_extension::{Error, Result, body::Body};
use serde::Deserialize;
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Input {
    request: String,
    id: u64,
    model: ModelRef,
    session_id: Option<String>,
    purpose: Option<String>,
    body: Body,
}

impl BoundSessions {
    pub(super) async fn language(&self, payload: Value, stop: &CancellationToken) -> Result<Value> {
        let input: Input = serde_json::from_value(payload).map_err(|_| Error::Protocol)?;
        let rsi_api_protocol::CallOrigin::Device(caller) = self
            .routes
            .caller(&input.request)
            .map_err(|_| Error::Remote("request has retired".into()))?
        else {
            return Err(Error::Protocol);
        };
        let service = self
            .language
            .as_ref()
            .ok_or_else(|| Error::Remote("Language router unavailable".into()))?;
        let connection = self.connection.upgrade().ok_or(Error::Disconnected)?;
        let cancellation = caller.revoked.child_token();
        let _cancel = cancellation.clone().drop_guard();
        let run = async {
            let assembly = match &input.body {
                Body::Paged { .. } => Some(
                    self.submissions
                        .try_acquire()
                        .map_err(|_| Error::Capacity)?,
                ),
                Body::Inline { .. } => None,
            };
            let body: LanguageRequest = input
                .body
                .resolve(rsi_ai_protocol::MAX_REQUEST_BYTES, |token, offset| {
                    let payload = json!({"id":input.id,"token":token,"offset":offset});
                    let connection = &connection;
                    async move {
                        connection
                            .request("application.language.read", payload, stop.clone())
                            .await
                            .map_err(|e| Error::Remote(e.to_string()))
                    }
                })
                .await?;
            drop(assembly);
            let origin = LanguageCallOrigin {
                active_session: false,
                session_id: input.session_id,
                purpose: input.purpose,
            };
            let prepared = service
                .prepare_for(input.model, body, origin)
                .await
                .map_err(|e| Error::Remote(e.to_string()))?;
            let mut batches = prepared
                .start(cancellation.clone())
                .await
                .map_err(|e| Error::Remote(e.to_string()))?
                .ready_chunks(16);
            let mut reply = Value::Null;
            while let Some(batch) = batches.next().await {
                let events = batch
                    .into_iter()
                    .collect::<std::result::Result<Vec<_>, _>>()
                    .map_err(|e| Error::Remote(e.to_string()))?;
                let bytes = serde_json::to_vec(&events).map_err(|_| Error::Protocol)?;
                if bytes.len() <= rsi_extension::body::INLINE_BYTES {
                    if matches!(
                        events.last(),
                        Some(LanguageEvent::Finished { .. } | LanguageEvent::Failed { .. })
                    ) {
                        reply = json!({"events":events});
                        continue;
                    }
                    connection
                        .request(
                            "application.language.events",
                            json!({"id":input.id,"events":events}),
                            stop.clone(),
                        )
                        .await
                        .map_err(|e| Error::Remote(e.to_string()))?;
                } else {
                    for offset in (0..bytes.len()).step_by(rsi_extension::body::FRAGMENT_BYTES) {
                        let part = rsi_extension::body::fragment(&bytes, offset)?;
                        connection
                            .request(
                                "application.language.events",
                                json!({"id":input.id,"bytes":bytes.len(),"part":part}),
                                stop.clone(),
                            )
                            .await
                            .map_err(|e| Error::Remote(e.to_string()))?;
                    }
                }
            }
            Ok(reply)
        };
        tokio::select! {
            biased;
            () = cancellation.cancelled() => Err(Error::Cancelled),
            result = run => result,
        }
    }
}
