use crate::{CatalogPage, CatalogRequest, Invoke, Item, Observe, Source, protocol};
use futures_util::StreamExt as _;
use rsi_api_protocol::{ApiClient, ApiError, ApiOutput, ApiStream, Result, RetainedBytes};
use rsi_ui::PresentationIdentity;
use std::sync::Arc;

/// Stateless typed UI capability over one negotiated connection generation.
#[derive(Clone, Debug)]
pub struct UiClient(Arc<dyn ApiClient>);
impl UiClient {
    /// Requires the exact owning operation policies before exposing any call.
    pub fn new(api: Arc<dyn ApiClient>) -> Result<Self> {
        if !crate::operations()
            .iter()
            .all(|spec| api.operations().contains(spec))
        {
            return Err(ApiError::Unavailable);
        }
        Ok(Self(api))
    }
    async fn call(
        &self,
        name: &str,
        request: &(impl serde::Serialize + Sync),
    ) -> Result<ApiOutput> {
        let spec = protocol::operation(name);
        let input = self
            .0
            .input_budget(spec.class)
            .encode(request, spec.maximum_request_bytes)?;
        self.0.call(&spec, input).await
    }
    /// Reads a bounded logical declaration page without retaining its temporary target.
    pub async fn catalog(&self, request: &CatalogRequest) -> Result<CatalogPage> {
        if request.maximum == 0 || request.maximum > 64 {
            return Err(ApiError::Capacity);
        }
        let ApiOutput::Reply(reply) = self.call("catalog", request).await? else {
            return Err(malformed());
        };
        if reply.binary.is_some() {
            return Err(malformed());
        }
        let page: CatalogPage =
            serde_json::from_slice(reply.json.as_bytes()).map_err(|_| malformed())?;
        if page.entries.len() > request.maximum {
            return Err(malformed());
        }
        let mut previous = request.after.clone();
        for entry in &page.entries {
            crate::state::name(&entry.bundle)?;
            crate::state::name(&entry.surface)?;
            let cursor = crate::CatalogCursor {
                bundle: entry.bundle.clone(),
                surface: entry.surface.clone(),
            };
            if entry.title.len() > 256
                || previous
                    .as_ref()
                    .is_some_and(|previous| previous >= &cursor)
            {
                return Err(malformed());
            }
            previous = Some(cursor);
        }
        if page.next.is_some() && (page.entries.is_empty() || page.next != previous) {
            return Err(malformed());
        }
        Ok(page)
    }
    /// Opens one multiplexed observation; Drop closes its remote presentation ownership.
    pub async fn observe(&self, request: &Observe) -> Result<UiObservation> {
        if request.selections.is_empty() || request.selections.len() > 16 {
            return Err(ApiError::Capacity);
        }
        let ApiOutput::Stream(stream) = self.call("observe", request).await? else {
            return Err(malformed());
        };
        Ok(UiObservation {
            stream: Some(stream),
            identities: vec![None; request.selections.len()],
            revisions: vec![0; request.selections.len()],
        })
    }
    /// Sends this one-time ticket exactly once. A missing/malformed response is unresolved.
    pub async fn invoke(&self, request: &Invoke) -> Result<()> {
        match self.call("invoke", request).await? {
            ApiOutput::Reply(reply)
                if reply.binary.is_none() && reply.json.as_bytes() == b"true" =>
            {
                Ok(())
            }
            _ => Err(ApiError::OutcomeUnknown),
        }
    }
    /// Reads exact binary bytes from a declared source, preserving their transport budget.
    pub async fn source(&self, request: &Source) -> Result<RetainedBytes> {
        #[derive(serde::Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Length {
            bytes: usize,
        }

        if request.maximum == 0 || request.maximum > rsi_ui::MAXIMUM_INPUT_BYTES {
            return Err(ApiError::Capacity);
        }
        let ApiOutput::Reply(reply) = self.call("source", request).await? else {
            return Err(malformed());
        };
        let length: Length =
            serde_json::from_slice(reply.json.as_bytes()).map_err(|_| malformed())?;
        let binary = reply.binary.ok_or_else(malformed)?;
        if length.bytes != binary.len() || length.bytes > request.maximum {
            return Err(malformed());
        }
        Ok(binary)
    }
}
/// One validated incoming item and its retained encoded transport storage.
#[derive(Clone, Debug)]
pub struct UiItem {
    /// Validated model, displayed ticket and selection.
    pub item: Item,
    bytes: RetainedBytes,
}
impl UiItem {
    /// Immutable complete item JSON; clones keep the same transport reservation.
    pub fn bytes(&self) -> &RetainedBytes {
        &self.bytes
    }
}
/// One connection-bound stream; no hidden reconnect, polling loop or replay ledger.
pub struct UiObservation {
    stream: Option<ApiStream>,
    identities: Vec<Option<PresentationIdentity>>,
    revisions: Vec<u64>,
}
impl std::fmt::Debug for UiObservation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("UiObservation")
            .field("presentations", &self.identities.len())
            .field("closed", &self.stream.is_none())
            .finish_non_exhaustive()
    }
}
impl UiObservation {
    /// Receives and validates one full model; malformed input closes this observation.
    pub async fn next(&mut self) -> Result<Option<UiItem>> {
        let result = self.receive().await;
        if result.is_err() || matches!(result, Ok(None)) {
            self.stream = None;
        }
        result
    }
    async fn receive(&mut self) -> Result<Option<UiItem>> {
        let Some(stream) = &mut self.stream else {
            return Ok(None);
        };
        let Some(message) = stream.next().await else {
            return Ok(None);
        };
        let message = message?;
        if message.binary.is_some() || message.encoded_len() > crate::MAXIMUM_ITEM_BYTES {
            return Err(malformed());
        }
        let item: Item =
            serde_json::from_slice(message.json.as_bytes()).map_err(|_| malformed())?;
        item.snapshot.validate().map_err(|_| malformed())?;
        if item.selection >= self.identities.len()
            || item
                .ticket
                .as_ref()
                .is_some_and(|ticket| ticket.len() > 160 || ticket.is_empty())
        {
            return Err(malformed());
        }
        let identity = &mut self.identities[item.selection];
        if identity
            .as_ref()
            .is_some_and(|identity| *identity != item.snapshot.presentation)
            || item.snapshot.revision < self.revisions[item.selection]
        {
            return Err(malformed());
        }
        *identity = Some(item.snapshot.presentation.clone());
        self.revisions[item.selection] = item.snapshot.revision;
        Ok(Some(UiItem {
            item,
            bytes: message.json,
        }))
    }
}
fn malformed() -> ApiError {
    ApiError::Invalid("invalid UI response".into())
}
