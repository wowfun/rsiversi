use async_trait::async_trait;
use bytes::Bytes;
use futures_util::stream;
use rsi_ai_protocol::LanguageCallOrigin;
use rsi_ai_transport::{
    HttpRequest, HttpResponse, HttpTransport, MAX_PROVIDER_REQUEST_BODY_BYTES, TransportError,
};
use serde::de::IgnoredAny;
use std::{collections::BTreeMap, fmt::Debug, sync::Arc};
use tokio_util::sync::CancellationToken;

#[cfg(test)]
mod tests;

/// An application's live request registry, selected only for this provider.
#[derive(Debug)]
pub struct DeepSeekFieldSourceContract;
impl rsi_meta::LocalContract for DeepSeekFieldSourceContract {
    const KEY: &'static str = "rsi.ai.deepseek.request-fields";
    type Service = dyn DeepSeekFieldSource;
}

/// Pins a currently contributing generation without opening IPC on the default-off path.
pub trait DeepSeekFieldSource: Send + Sync + Debug {
    fn current(&self) -> Option<Arc<dyn FieldPreparation>>;
}

/// Prepares fields from the exact serialized JSON, excluding URL and credential headers.
#[async_trait]
pub trait FieldPreparation: Send + Sync + Debug {
    async fn prepare(
        &self,
        origin: &LanguageCallOrigin,
        body: Bytes,
        stop: &CancellationToken,
    ) -> Result<PreparedFields, TransportError>;
}

/// One JSON object of additional fields and its HTTP acceptance callback.
pub struct PreparedFields {
    pub fields: Bytes,
    pub receipt: Option<Box<dyn AcceptedFields>>,
}

impl std::fmt::Debug for PreparedFields {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PreparedFields")
            .field("bytes", &self.fields.len())
            .field("receipt", &self.receipt.is_some())
            .finish()
    }
}

/// Dropping this receipt abandons an uncertain prefix.
#[async_trait]
pub trait AcceptedFields: Send + Debug {
    async fn accept(self: Box<Self>) -> Result<(), TransportError>;
}

#[derive(Debug)]
pub(super) struct Transport {
    pub inner: Arc<dyn HttpTransport>,
    pub context: rsi_meta::Context,
}

fn invalid_fields() -> TransportError {
    TransportError::new(
        "http.invalid_request_fields",
        "Invalid or conflicting DeepSeek request fields",
    )
}

fn extend(request: HttpRequest, base: Bytes, fields: Bytes) -> Result<HttpRequest, TransportError> {
    // Both envelopes are compact JSON objects. Parse names without allocating their values,
    // including inline images. Only the inner commas and braces change on the wire.
    let keys = |bytes: &[u8]| {
        serde_json::from_slice::<BTreeMap<String, IgnoredAny>>(bytes).map_err(|_| invalid_fields())
    };
    let added = keys(&fields)?;
    if added.is_empty() {
        return Ok(request.body(base));
    }
    if base.last() != Some(&b'}') || fields.first() != Some(&b'{') || fields.last() != Some(&b'}') {
        return Err(invalid_fields());
    }
    let original = keys(&base)?;
    if added.keys().any(|key| original.contains_key(key)) {
        return Err(invalid_fields());
    }
    let base_size = if original.is_empty() {
        0
    } else {
        base.len() - 1
    };
    if base_size.saturating_add(fields.len()) > MAX_PROVIDER_REQUEST_BODY_BYTES {
        return Err(TransportError::new(
            "http.invalid_request_fields",
            "Extended DeepSeek request exceeds the body limit",
        ));
    }
    if original.is_empty() {
        return Ok(request.body(fields));
    }
    Ok(request.body_stream(Box::pin(stream::iter([
        Ok(base.slice(..base.len() - 1)),
        Ok(Bytes::from_static(b",")),
        Ok(fields.slice(1..)),
    ]))))
}

#[async_trait]
impl HttpTransport for Transport {
    async fn execute(
        &self,
        request: HttpRequest,
        stop: CancellationToken,
    ) -> Result<HttpResponse, TransportError> {
        self.inner.execute(request, stop).await
    }

    async fn execute_language(
        &self,
        mut request: HttpRequest,
        origin: &LanguageCallOrigin,
        stop: CancellationToken,
    ) -> Result<HttpResponse, TransportError> {
        let Some(fields) = self
            .context
            .lookup_local::<DeepSeekFieldSourceContract>()
            .and_then(|source| source.current())
        else {
            return self.inner.execute(request, stop).await;
        };
        let base = tokio::select! { biased;
            () = stop.cancelled() => return Err(TransportError::new("http.cancelled", "Model call cancelled")),
            bytes = request.take_json_bytes() => bytes?,
        };
        let prepared = fields.prepare(origin, base.clone(), &stop).await?;
        let outgoing = extend(request, base, prepared.fields)?;
        let response = self.inner.execute(outgoing, stop).await?;
        if (200..300).contains(&response.status)
            && let Some(receipt) = prepared.receipt
            && let Err(error) = receipt.accept().await
        {
            eprintln!(
                "DeepSeek request field acknowledgement failed: {}",
                error.code()
            );
        }
        Ok(response)
    }
}
