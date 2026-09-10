//! Read-only completed-output API endpoint and client plugins.

#![deny(unsafe_code)]
#![warn(missing_docs)]
#![allow(clippy::missing_errors_doc)]

mod plugin;

pub use plugin::{OutputApiFactory, OutputClientFactory};

use async_trait::async_trait;
use rsi_api_protocol::{
    ApiClient, ApiContext, ApiError, ApiHandler, ApiMessage, ApiOutput, ApiRegistrar,
    ApiRegistration, ApiResponseCapacity, ByteBudget, OperationClass, OperationEffect, OperationId,
    OperationSpec, RequestEncoding, RetainedBytes,
};
use rsi_process::{
    MAXIMUM_OUTPUT_READ_BYTES, OutputPage, ProcessError, ProcessOutputCache, validate_output_read,
};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

fn operation() -> OperationSpec {
    OperationSpec {
        id: OperationId::new("output", "read", 1).expect("constant operation"),
        class: OperationClass::Data,
        effect: OperationEffect::Read,
        access: rsi_api_protocol::OperationAccess::Authenticated,
        encoding: RequestEncoding::Json,
        maximum_request_bytes: 256,
        maximum_response_bytes: MAXIMUM_OUTPUT_READ_BYTES + 1024,
    }
}
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Request {
    id: String,
    offset: u64,
    limit: usize,
}
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Metadata {
    id: String,
    offset: u64,
    next_offset: u64,
    total_bytes: u64,
}
#[derive(Deserialize, Serialize)]
#[serde(tag = "code", rename_all = "snake_case", deny_unknown_fields)]
enum Failure {
    Invalid,
    Capacity,
    ShuttingDown,
    Unavailable,
}

#[derive(Debug)]
struct OutputHandler {
    output: Arc<dyn ProcessOutputCache>,
    scratch: ByteBudget,
}
#[async_trait]
impl ApiHandler for OutputHandler {
    async fn invoke(
        &self,
        _: ApiContext,
        input: RetainedBytes,
        capacity: ApiResponseCapacity,
    ) -> rsi_api_protocol::Result<ApiOutput> {
        let request: Request = serde_json::from_slice(input.as_bytes())
            .map_err(|_| ApiError::Invalid("invalid output request".into()))?;
        validate_output_read(&request.id, request.limit)
            .map_err(|_| ApiError::Invalid("invalid output request".into()))?;
        let ApiResponseCapacity::Finite(mut capacity) = capacity else {
            return Err(ApiError::Backend("finite output admission required".into()));
        };
        let _scratch = self.scratch.reserve(request.limit)?;
        let page = match self
            .output
            .read(&request.id, request.offset, request.limit)
            .await
        {
            Ok(page) => page,
            Err(ProcessError::Api(error)) => return Err(error),
            Err(error) => {
                let failure = match error {
                    ProcessError::InvalidInput(_) => Failure::Invalid,
                    ProcessError::Capacity => Failure::Capacity,
                    ProcessError::ShuttingDown => Failure::ShuttingDown,
                    _ => Failure::Unavailable,
                };
                return Err(ApiError::Domain(capacity.encode(&failure)?));
            }
        };
        page.validate_for(&request.id, request.offset, request.limit)
            .map_err(|_| ApiError::Backend("invalid provider output page".into()))?;
        let binary = capacity.split(page.bytes.len())?.copy(&page.bytes)?;
        let metadata = Metadata {
            id: page.id,
            offset: page.offset,
            next_offset: page.next_offset,
            total_bytes: page.total_bytes,
        };
        Ok(ApiOutput::Reply(ApiMessage {
            json: capacity.encode(&metadata)?,
            binary: Some(binary),
        }))
    }
}

/// Registers the completed-output read without granting process execution authority.
pub fn register_output(
    registrar: &dyn ApiRegistrar,
    output: Arc<dyn ProcessOutputCache>,
) -> rsi_api_protocol::Result<ApiRegistration> {
    registrar.register(
        operation(),
        Arc::new(OutputHandler {
            output,
            scratch: ByteBudget::new(1024 * 1024)?,
        }),
    )
}

/// Read-only completed-output proxy using one negotiated API generation.
#[derive(Debug)]
pub struct OutputClient {
    api: Arc<dyn ApiClient>,
}
impl OutputClient {
    /// Requires exact read metadata before exposing the cache contract.
    pub fn new(api: Arc<dyn ApiClient>) -> rsi_api_protocol::Result<Self> {
        if !api.operations().contains(&operation()) {
            return Err(ApiError::Unavailable);
        }
        Ok(Self { api })
    }
}
#[async_trait]
impl ProcessOutputCache for OutputClient {
    async fn read(&self, id: &str, offset: u64, limit: usize) -> rsi_process::Result<OutputPage> {
        validate_output_read(id, limit)?;
        let operation = operation();
        let input = self
            .api
            .input_budget(operation.class)
            .encode(
                &Request {
                    id: id.into(),
                    offset,
                    limit,
                },
                operation.maximum_request_bytes,
            )
            .map_err(ProcessError::Api)?;
        let message = match self.api.call(&operation, input).await {
            Ok(ApiOutput::Reply(message)) => message,
            Ok(ApiOutput::Stream(_)) => return Err(malformed()),
            Err(ApiError::Domain(bytes)) => return Err(decode_failure(&bytes)?),
            Err(error) => return Err(ProcessError::Api(error)),
        };
        let metadata: Metadata =
            serde_json::from_slice(message.json.as_bytes()).map_err(|_| malformed())?;
        let bytes = message.binary.ok_or_else(malformed)?.into_bytes();
        let page = OutputPage {
            id: metadata.id,
            offset: metadata.offset,
            next_offset: metadata.next_offset,
            total_bytes: metadata.total_bytes,
            bytes,
        };
        page.validate_for(id, offset, limit)
            .map_err(|_| malformed())?;
        Ok(page)
    }
}
fn malformed() -> ProcessError {
    ProcessError::Api(ApiError::Invalid("invalid remote output page".into()))
}
fn decode_failure(bytes: &RetainedBytes) -> rsi_process::Result<ProcessError> {
    let failure: Failure = serde_json::from_slice(bytes.as_bytes()).map_err(|_| malformed())?;
    Ok(match failure {
        Failure::Invalid => ProcessError::InvalidInput("remote output rejected input".into()),
        Failure::Capacity => ProcessError::Capacity,
        Failure::ShuttingDown => ProcessError::ShuttingDown,
        Failure::Unavailable => ProcessError::Io("remote completed output unavailable".into()),
    })
}
