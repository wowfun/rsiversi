use crate::wire::{Failure, Operation, validate_body};
use async_trait::async_trait;
use rsi_api_protocol::{ApiClient, ApiError, ApiOutput, RetainedBytes};
use rsi_media_protocol::{Media, MediaError, MediaRef, StoredMedia};
use std::sync::Arc;

/// Canonical Media proxy retaining received byte ownership.
#[derive(Debug)]
pub struct MediaClient {
    api: Arc<dyn ApiClient>,
}
impl MediaClient {
    /// Requires the exact import/read contracts before exposing Media capability.
    pub fn new(api: Arc<dyn ApiClient>) -> rsi_api_protocol::Result<Self> {
        if Operation::ALL
            .iter()
            .any(|operation| !api.operations().contains(&operation.spec()))
        {
            return Err(ApiError::Unavailable);
        }
        Ok(Self { api })
    }
}
#[async_trait]
impl Media for MediaClient {
    async fn import_image(&self, source: bytes::Bytes) -> rsi_media_protocol::Result<MediaRef> {
        let spec = Operation::Import.spec();
        if source.is_empty() || source.len() > spec.maximum_request_bytes {
            return Err(MediaError::InvalidInput(
                "API Media source must contain 1 byte through 64 MiB".into(),
            ));
        }
        let input = self
            .api
            .input_budget(spec.class)
            .copy(&source)
            .map_err(MediaError::Api)?;
        match self.api.call(&spec, input).await {
            Ok(ApiOutput::Reply(message)) if message.binary.is_none() => {
                serde_json::from_slice(message.json.as_bytes())
                    .map_err(|_| MediaError::Api(ApiError::OutcomeUnknown))
            }
            Ok(_) => Err(MediaError::Api(ApiError::OutcomeUnknown)),
            Err(ApiError::Domain(bytes)) => Err(failure(&bytes, true)?),
            Err(error) => Err(MediaError::Api(error)),
        }
    }
    async fn read(&self, reference: &MediaRef) -> rsi_media_protocol::Result<StoredMedia> {
        reference.validate()?;
        let spec = Operation::Read.spec();
        let input = self
            .api
            .input_budget(spec.class)
            .encode(reference, spec.maximum_request_bytes)
            .map_err(MediaError::Api)?;
        let message = match self.api.call(&spec, input).await {
            Ok(ApiOutput::Reply(message)) => message,
            Ok(ApiOutput::Stream(_)) => return Err(malformed()),
            Err(ApiError::Domain(bytes)) => {
                let error = failure(&bytes, false)?;
                if matches!(&error, MediaError::NotFound(id) if *id != reference.id) {
                    return Err(malformed());
                }
                return Err(error);
            }
            Err(error) => return Err(MediaError::Api(error)),
        };
        let stored = StoredMedia {
            reference: serde_json::from_slice(message.json.as_bytes()).map_err(|_| malformed())?,
            bytes: message.binary.ok_or_else(malformed)?.into_bytes(),
        };
        validate_body(reference, &stored).map_err(|_| malformed())?;
        Ok(stored)
    }
}
fn malformed() -> MediaError {
    MediaError::Api(ApiError::Invalid("invalid remote Media body".into()))
}
fn failure(bytes: &RetainedBytes, mutation: bool) -> rsi_media_protocol::Result<MediaError> {
    let failure: Failure = serde_json::from_slice(bytes.as_bytes()).map_err(|_| {
        if mutation {
            MediaError::Api(ApiError::OutcomeUnknown)
        } else {
            malformed()
        }
    })?;
    Ok(failure.into_error())
}
