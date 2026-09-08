//! Independent canonical Media API endpoint and client plugins.

#![deny(unsafe_code)]
#![warn(missing_docs)]
#![allow(clippy::missing_errors_doc)]

mod client;
mod plugin;
mod wire;

pub use client::MediaClient;
pub use plugin::{MediaApiFactory, MediaClientFactory};

use async_trait::async_trait;
use rsi_api_protocol::{
    ApiContext, ApiError, ApiHandler, ApiMessage, ApiOutput, ApiRegistrar, ApiRegistration,
    ApiResponseCapacity, ByteBudget, RetainedBytes,
};
use rsi_media_protocol::{MAXIMUM_IMAGE_DESCRIPTOR_BYTES, Media, MediaRef};
use std::sync::Arc;
use wire::{Operation, map_failure};

#[derive(Debug)]
struct Handler {
    media: Arc<dyn Media>,
    operation: Operation,
    scratch: ByteBudget,
}
#[async_trait]
impl ApiHandler for Handler {
    async fn invoke(
        &self,
        _: ApiContext,
        input: RetainedBytes,
        output: ApiResponseCapacity,
    ) -> rsi_api_protocol::Result<ApiOutput> {
        let ApiResponseCapacity::Finite(mut capacity) = output else {
            return Err(ApiError::Backend("finite Media admission required".into()));
        };
        match self.operation {
            Operation::Import => {
                if input.is_empty() {
                    return Err(ApiError::Invalid("empty Media source".into()));
                }
                match self.media.import_image(input.into_bytes()).await {
                    Ok(reference) => {
                        reference.validate().map_err(|_| {
                            ApiError::Backend("invalid published Media reference".into())
                        })?;
                        Ok(ApiOutput::Reply(ApiMessage {
                            json: capacity.encode(&reference).map_err(encoding_failed)?,
                            binary: None,
                        }))
                    }
                    Err(error) => Err(ApiError::Domain(
                        capacity
                            .encode(&map_failure(error)?)
                            .map_err(encoding_failed)?,
                    )),
                }
            }
            Operation::Read => {
                let reference: MediaRef = serde_json::from_slice(input.as_bytes())
                    .map_err(|_| ApiError::Invalid("invalid canonical Media reference".into()))?;
                let _scratch = self.scratch.reserve(
                    usize::try_from(MAXIMUM_IMAGE_DESCRIPTOR_BYTES).expect("canonical image bound")
                        + 4097,
                )?;
                match self.media.read(&reference).await {
                    Ok(stored) => {
                        wire::validate_body(&reference, &stored)
                            .map_err(|_| ApiError::Backend("invalid provider Media body".into()))?;
                        let binary = capacity.split(stored.bytes.len())?.copy(&stored.bytes)?;
                        Ok(ApiOutput::Reply(ApiMessage {
                            json: capacity
                                .encode(&stored.reference)
                                .map_err(encoding_failed)?,
                            binary: Some(binary),
                        }))
                    }
                    Err(error) => Err(ApiError::Domain(
                        capacity
                            .encode(&map_failure(error)?)
                            .map_err(encoding_failed)?,
                    )),
                }
            }
        }
    }
}
fn encoding_failed(_: ApiError) -> ApiError {
    ApiError::Backend("Media result encoding failed".into())
}

/// Owns the independent import and read operation registrations.
#[derive(Debug)]
pub struct MediaApi {
    registrations: Vec<ApiRegistration>,
}
impl MediaApi {
    /// Registers domain-owned metadata and handlers against one Media service.
    pub fn register(
        registrar: &dyn ApiRegistrar,
        media: Arc<dyn Media>,
    ) -> rsi_api_protocol::Result<Self> {
        let scratch = ByteBudget::default();
        let mut registrations = Vec::new();
        for (operation, media) in Operation::ALL.into_iter().zip([media.clone(), media]) {
            registrations.push(registrar.register(
                operation.spec(),
                Arc::new(Handler {
                    media,
                    operation,
                    scratch: scratch.clone(),
                }),
            )?);
        }
        Ok(Self { registrations })
    }
    /// Retires both operations and waits for admitted publication work to settle.
    pub async fn close(self) {
        futures_util::future::join_all(self.registrations.into_iter().map(ApiRegistration::close))
            .await;
    }
}
