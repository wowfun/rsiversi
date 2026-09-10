use crate::{Commit, Observe, Offer, malformed, operation, operations};
use futures_util::StreamExt as _;
use rsi_api_protocol::{ApiClient, ApiError, ApiOutput, ApiStream, Result};
use std::sync::Arc;

/// Typed authority over one negotiated, authenticated connection.
#[derive(Clone, Debug)]
pub struct AssetsClient(Arc<dyn ApiClient>);
impl AssetsClient {
    /// Requires the exact policies of both operations.
    pub fn new(api: Arc<dyn ApiClient>) -> Result<Self> {
        if !operations()
            .iter()
            .all(|spec| api.operations().contains(spec))
        {
            return Err(ApiError::Unavailable);
        }
        Ok(Self(api))
    }
    async fn call(&self, commit: bool, request: &impl serde::Serialize) -> Result<ApiOutput> {
        let spec = operation(commit);
        let input = self
            .0
            .input_budget(spec.class)
            .encode(request, spec.maximum_request_bytes)?;
        self.0.call(&spec, input).await
    }
    /// Acquires observation admission; its first offer already owns a complete lease.
    pub async fn observe(&self, request: &Observe) -> Result<AssetObservation> {
        request.validate()?;
        let ApiOutput::Stream(stream) = self.call(false, request).await? else {
            return Err(malformed());
        };
        Ok(AssetObservation { stream })
    }
    /// Consumes the exact pending offer once; a missing response is unresolved.
    pub async fn commit(&self, request: &Commit) -> Result<()> {
        request.validate()?;
        match self.call(true, request).await? {
            ApiOutput::Reply(reply)
                if reply.binary.is_none() && reply.json.as_bytes() == b"true" =>
            {
                Ok(())
            }
            _ => Err(ApiError::OutcomeUnknown),
        }
    }
}
/// Dropping this stream releases remote generation ownership through the transport.
pub struct AssetObservation {
    stream: ApiStream,
}
impl AssetObservation {
    /// Receives one closed validated offer. EOF closes this ownership, even after a commit.
    pub async fn next(&mut self) -> Result<Offer> {
        let message = self.stream.next().await.ok_or(ApiError::Unavailable)??;
        if message.binary.is_some() {
            return Err(malformed());
        }
        let offer: Offer =
            serde_json::from_slice(message.json.as_bytes()).map_err(|_| malformed())?;
        offer.validate()?;
        Ok(offer)
    }
}

impl std::fmt::Debug for AssetObservation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AssetObservation").finish_non_exhaustive()
    }
}
