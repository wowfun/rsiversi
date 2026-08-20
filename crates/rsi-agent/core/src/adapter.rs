use async_trait::async_trait;
use rsi_agent_protocol::{
    ToolsBody, ToolsCatalogResponse, ToolsEnvelope, ToolsInvokeRequest, ToolsInvokeResponse,
};
use rsi_ai_meta::{AiService, ClientControl, MetaIncoming, MetaServiceStream, ServerControl};
use rsi_ai_protocol::{LanguageAssembler, LanguageOutput, LanguageRequest};
use rsi_meta::{
    CompositionHost, InstanceId, ServiceKey, ServiceOpenRequest, ServiceStream, StreamKind,
};
use std::sync::Arc;

use crate::{Failure, FailureKind, SessionId};

fn stream_credit_bytes(tool: bool) -> u64 {
    let response_count = if tool {
        crate::MAX_TOOL_CALLS_PER_TURN + 1
    } else {
        usize::try_from(crate::MAX_STEPS).expect("model step limit fits usize")
    };
    let bytes = rsi_agent_protocol::MAX_DATA_BYTES
        .checked_mul(response_count)
        .expect("bounded response credit fits usize");
    u64::try_from(bytes).expect("bounded response credit fits u64")
}

pub(crate) struct PortBundle {
    pub(crate) model: Box<dyn ModelPort>,
    pub(crate) tools: Box<dyn ToolPort>,
}

/// Exact bytes rederived from committed state after the writer acknowledged
/// the request event. Construction is kept beside the runner's commit barrier.
#[derive(Clone)]
pub(crate) struct CommittedModelRequest {
    request_id: String,
    model: String,
    canonical_json: Arc<str>,
}

pub(crate) struct PreparedModelCall {
    request: CommittedModelRequest,
    snapshot: rsi_ai_meta::PreparedCallSnapshot,
}

impl PreparedModelCall {
    pub(crate) fn new(
        request: CommittedModelRequest,
        snapshot: rsi_ai_meta::PreparedCallSnapshot,
    ) -> Self {
        Self { request, snapshot }
    }

    pub(crate) fn snapshot(&self) -> &rsi_ai_meta::PreparedCallSnapshot {
        &self.snapshot
    }

    #[cfg(test)]
    pub(crate) fn request(&self) -> &CommittedModelRequest {
        &self.request
    }
}

impl CommittedModelRequest {
    pub(crate) fn new(request_id: String, model: String, canonical_json: Arc<str>) -> Self {
        Self {
            request_id,
            model,
            canonical_json,
        }
    }

    pub(crate) fn request_id(&self) -> &str {
        &self.request_id
    }

    pub(crate) fn bytes(&self) -> &[u8] {
        self.canonical_json.as_bytes()
    }

    pub(crate) fn model(&self) -> &str {
        &self.model
    }
}

pub(crate) struct ValidatedAssistantMessage(LanguageOutput);

impl ValidatedAssistantMessage {
    #[cfg(test)]
    pub(crate) fn validate(message: LanguageOutput) -> std::result::Result<Self, PortError> {
        message
            .validate()
            .map_err(|error| PortError::model_protocol(error.to_string()))?;
        Ok(Self(message))
    }

    fn from_output(output: LanguageOutput) -> Self {
        Self(output)
    }

    pub(crate) fn into_inner(self) -> LanguageOutput {
        self.0
    }
}

pub(crate) struct ValidatedToolCatalog(ToolsCatalogResponse);

impl ValidatedToolCatalog {
    #[cfg(test)]
    pub(crate) fn validate(
        mut response: ToolsCatalogResponse,
    ) -> std::result::Result<Self, PortError> {
        response
            .validate("catalog")
            .map_err(|error| PortError::tool_protocol(error.to_string()))?;
        for tool in &mut response.tools {
            tool.input_schema = rsi_agent_protocol::canonicalize_json(&tool.input_schema)
                .map_err(|error| PortError::tool_protocol(error.to_string()))?;
        }
        Ok(Self(response))
    }

    fn from_decoded_envelope(response: ToolsCatalogResponse) -> Self {
        Self(response)
    }

    pub(crate) fn into_inner(self) -> ToolsCatalogResponse {
        self.0
    }
}

pub(crate) struct ValidatedToolResponse(ToolsInvokeResponse);

impl ValidatedToolResponse {
    #[cfg(test)]
    pub(crate) fn validate(
        response: ToolsInvokeResponse,
        expected_call_id: &str,
    ) -> std::result::Result<Self, PortError> {
        if response.call_id != expected_call_id {
            return Err(PortError::tool_protocol("invoke response call_id mismatch"));
        }
        response
            .result
            .validate("result")
            .map_err(|error| PortError::tool_protocol(error.to_string()))?;
        let mut response = response;
        if let rsi_agent_protocol::ToolResult::Ok { value } = &mut response.result {
            *value = rsi_agent_protocol::canonicalize_json(value)
                .map_err(|error| PortError::tool_protocol(error.to_string()))?;
        }
        Ok(Self(response))
    }

    fn from_decoded_envelope(response: ToolsInvokeResponse) -> Self {
        Self(response)
    }

    pub(crate) fn into_inner(self) -> ToolsInvokeResponse {
        self.0
    }
}

pub(crate) trait PortFactory: Send + Sync {
    fn open(&self, session_id: &SessionId) -> std::result::Result<PortBundle, PortError>;
}

#[async_trait]
pub(crate) trait ModelPort: Send {
    fn provider(&self) -> &str;
    async fn initialize(&mut self) -> std::result::Result<(), PortError>;
    async fn prepare(
        &mut self,
        request: &CommittedModelRequest,
    ) -> std::result::Result<PreparedModelCall, PortError>;
    async fn start(
        &mut self,
        prepared: PreparedModelCall,
    ) -> std::result::Result<ValidatedAssistantMessage, PortError>;
    async fn finish(&mut self) -> std::result::Result<(), PortError>;
}

#[async_trait]
pub(crate) trait ToolPort: Send {
    fn provider(&self) -> &str;
    async fn initialize(&mut self) -> std::result::Result<(), PortError>;
    async fn catalog(&mut self) -> std::result::Result<ValidatedToolCatalog, PortError>;
    async fn invoke(
        &mut self,
        request: ToolsInvokeRequest,
    ) -> std::result::Result<ValidatedToolResponse, PortError>;
    async fn finish(&mut self) -> std::result::Result<(), PortError>;
}

#[derive(Debug)]
pub(crate) struct PortError {
    pub(crate) failure: Failure,
    pub(crate) retry: Option<Box<rsi_ai_protocol::AiError>>,
}

impl PortError {
    fn model_unavailable(message: impl Into<String>) -> Self {
        Self {
            failure: Failure::new(FailureKind::ModelUnavailable, message),
            retry: None,
        }
    }

    pub(crate) fn model_ai(error: rsi_ai_protocol::AiError, output_observed: bool) -> Self {
        Self {
            failure: Failure::new(FailureKind::ModelUnavailable, error.safe_summary()),
            retry: (!output_observed).then(|| Box::new(error)),
        }
    }

    pub(crate) fn retry_error(&self) -> Option<&rsi_ai_protocol::AiError> {
        self.retry.as_deref()
    }

    fn model_protocol(message: impl Into<String>) -> Self {
        Self {
            failure: Failure::new(FailureKind::ModelProtocol, message),
            retry: None,
        }
    }

    fn tool_unavailable(message: impl Into<String>) -> Self {
        Self {
            failure: Failure::new(FailureKind::ToolUnavailable, message),
            retry: None,
        }
    }

    fn tool_protocol(message: impl Into<String>) -> Self {
        Self {
            failure: Failure::new(FailureKind::ToolProtocol, message),
            retry: None,
        }
    }
}

pub(crate) struct CompositionPortFactory {
    composition: CompositionHost,
    consumer: InstanceId,
}

impl CompositionPortFactory {
    pub(crate) fn new(composition: CompositionHost, consumer: InstanceId) -> Self {
        Self {
            composition,
            consumer,
        }
    }
}

impl PortFactory for CompositionPortFactory {
    fn open(&self, _session_id: &SessionId) -> std::result::Result<PortBundle, PortError> {
        let tools = self
            .composition
            .open_service(ServiceOpenRequest {
                consumer: self.consumer.clone(),
                service: ServiceKey::new(rsi_agent_protocol::TOOLS_SERVICE_KEY),
            })
            .map_err(|error| PortError::tool_unavailable(error.to_string()))?;
        Ok(PortBundle {
            model: Box::new(StreamModelPort::new(
                self.composition.clone(),
                self.consumer.clone(),
            )),
            tools: Box::new(StreamToolPort::new(tools)),
        })
    }
}

struct StreamModelPort {
    provider: String,
    composition: CompositionHost,
    consumer: InstanceId,
    stream: Option<MetaServiceStream>,
}

impl StreamModelPort {
    fn new(composition: CompositionHost, consumer: InstanceId) -> Self {
        Self {
            provider: "uninitialized".to_owned(),
            composition,
            consumer,
            stream: None,
        }
    }

    fn stream(&mut self) -> std::result::Result<&mut MetaServiceStream, PortError> {
        self.stream
            .as_mut()
            .ok_or_else(|| PortError::model_unavailable("language service is not initialized"))
    }
}

#[async_trait]
impl ModelPort for StreamModelPort {
    fn provider(&self) -> &str {
        &self.provider
    }

    async fn initialize(&mut self) -> std::result::Result<(), PortError> {
        let stream = MetaServiceStream::open(
            &self.composition,
            self.consumer.clone(),
            AiService::Language,
        )
        .await
        .map_err(|error| PortError::model_unavailable(error.to_string()))?;
        self.provider = stream.provider().to_string();
        self.stream = Some(stream);
        Ok(())
    }

    async fn prepare(
        &mut self,
        request: &CommittedModelRequest,
    ) -> std::result::Result<PreparedModelCall, PortError> {
        let language = serde_json::from_slice::<LanguageRequest>(request.bytes())
            .map_err(|error| PortError::model_protocol(error.to_string()))?;
        let call_id = request.request_id().to_owned();
        let model = request.model().to_owned();
        let stream = self.stream()?;
        stream
            .send_control(&ClientControl::PrepareLanguage {
                call_id: call_id.clone(),
                model,
                request: language,
            })
            .await
            .map_err(|error| PortError::model_unavailable(error.to_string()))?;
        let snapshot = match stream
            .recv()
            .await
            .map_err(|error| PortError::model_unavailable(error.to_string()))?
        {
            Some(MetaIncoming::Control(ServerControl::Prepared {
                call_id: received,
                snapshot,
            })) if received == call_id => snapshot,
            Some(MetaIncoming::Control(ServerControl::Failed { error, .. })) => {
                return Err(PortError::model_ai(error, false));
            }
            _ => {
                return Err(PortError::model_protocol(
                    "language prepare returned an unexpected frame",
                ));
            }
        };
        Ok(PreparedModelCall::new(request.clone(), snapshot))
    }

    async fn start(
        &mut self,
        prepared: PreparedModelCall,
    ) -> std::result::Result<ValidatedAssistantMessage, PortError> {
        let call_id = prepared.request.request_id().to_owned();
        let stream = self.stream()?;
        stream
            .send_control(&ClientControl::Start {
                call_id: call_id.clone(),
            })
            .await
            .map_err(|error| PortError::model_unavailable(error.to_string()))?;
        let mut assembler = LanguageAssembler::new();
        let mut output_observed = false;
        loop {
            match stream
                .recv()
                .await
                .map_err(|error| PortError::model_unavailable(error.to_string()))?
            {
                Some(MetaIncoming::Control(ServerControl::LanguageEvent {
                    call_id: received,
                    event,
                })) if received == call_id => {
                    output_observed |= !matches!(
                        event,
                        rsi_ai_protocol::LanguageEvent::Usage { .. }
                            | rsi_ai_protocol::LanguageEvent::Finished { .. }
                            | rsi_ai_protocol::LanguageEvent::Failed { .. }
                    );
                    let terminal = matches!(
                        event,
                        rsi_ai_protocol::LanguageEvent::Finished { .. }
                            | rsi_ai_protocol::LanguageEvent::Failed { .. }
                    );
                    assembler
                        .push(&event)
                        .map_err(|error| PortError::model_protocol(error.to_string()))?;
                    if terminal {
                        break;
                    }
                }
                Some(MetaIncoming::Control(ServerControl::Failed { error, .. })) => {
                    return Err(PortError::model_ai(error, output_observed));
                }
                _ => {
                    return Err(PortError::model_protocol(
                        "language generation returned an unexpected frame",
                    ));
                }
            }
        }
        let output = assembler.finish().map_err(|failure| match failure {
            rsi_ai_protocol::LanguageAssemblyError::Provider { error, partial } => {
                let output_observed = !partial.content.is_empty()
                    || !partial.sources.is_empty()
                    || !partial.warnings.is_empty();
                PortError::model_ai(error, output_observed)
            }
            rsi_ai_protocol::LanguageAssemblyError::Protocol(error) => {
                PortError::model_protocol(error.to_string())
            }
        })?;
        Ok(ValidatedAssistantMessage::from_output(output))
    }

    async fn finish(&mut self) -> std::result::Result<(), PortError> {
        let stream = self.stream()?;
        stream
            .half_close()
            .await
            .map_err(|error| PortError::model_unavailable(error.to_string()))?;
        match stream
            .recv()
            .await
            .map_err(|error| PortError::model_unavailable(error.to_string()))?
        {
            Some(MetaIncoming::End) => Ok(()),
            Some(MetaIncoming::Cancel { reason }) => Err(PortError::model_unavailable(reason)),
            Some(MetaIncoming::Control(_) | MetaIncoming::BlobChunk { .. }) => Err(
                PortError::model_protocol("language stream emitted data while closing"),
            ),
            None => Err(PortError::model_unavailable("language stream closed")),
        }
    }
}

struct StreamToolPort {
    provider: String,
    stream: ServiceStream,
    sequence: u64,
}

impl StreamToolPort {
    fn new(stream: ServiceStream) -> Self {
        Self {
            provider: stream.provider().to_string(),
            stream,
            sequence: 0,
        }
    }

    fn request_id(&mut self, operation: &str) -> String {
        self.sequence += 1;
        format!("{operation}-{}", self.sequence)
    }
}

#[async_trait]
impl ToolPort for StreamToolPort {
    fn provider(&self) -> &str {
        &self.provider
    }

    async fn initialize(&mut self) -> std::result::Result<(), PortError> {
        initialize_stream(&mut self.stream, true).await
    }

    async fn catalog(&mut self) -> std::result::Result<ValidatedToolCatalog, PortError> {
        let request_id = self.request_id("catalog");
        let request = ToolsEnvelope::catalog_request(request_id.clone());
        let bytes = request
            .encode()
            .map_err(|error| PortError::tool_protocol(error.to_string()))?;
        self.stream
            .send(&bytes)
            .await
            .map_err(|error| PortError::tool_unavailable(error.to_string()))?;
        let response = ToolsEnvelope::decode(&recv_data(&mut self.stream, true).await?)
            .map_err(|error| PortError::tool_protocol(error.to_string()))?;
        if response.request_id != request_id {
            return Err(PortError::tool_protocol(
                "catalog response request_id mismatch",
            ));
        }
        match response.body {
            ToolsBody::CatalogResponse(response) => {
                Ok(ValidatedToolCatalog::from_decoded_envelope(response))
            }
            ToolsBody::Error { error } => Err(PortError::tool_unavailable(format!(
                "{}: {}",
                error.code, error.message
            ))),
            _ => Err(PortError::tool_protocol(
                "tools provider returned a non-catalog response",
            )),
        }
    }

    async fn invoke(
        &mut self,
        request: ToolsInvokeRequest,
    ) -> std::result::Result<ValidatedToolResponse, PortError> {
        let request_id = self.request_id("invoke");
        let expected_call_id = request.call_id.clone();
        let envelope = ToolsEnvelope::invoke_request(request_id.clone(), request);
        let bytes = envelope
            .encode()
            .map_err(|error| PortError::tool_protocol(error.to_string()))?;
        self.stream
            .send(&bytes)
            .await
            .map_err(|error| PortError::tool_unavailable(error.to_string()))?;
        let response = ToolsEnvelope::decode(&recv_data(&mut self.stream, true).await?)
            .map_err(|error| PortError::tool_protocol(error.to_string()))?;
        if response.request_id != request_id {
            return Err(PortError::tool_protocol(
                "invoke response request_id mismatch",
            ));
        }
        match response.body {
            ToolsBody::InvokeResponse(response) if response.call_id == expected_call_id => {
                Ok(ValidatedToolResponse::from_decoded_envelope(response))
            }
            ToolsBody::InvokeResponse(_) => {
                Err(PortError::tool_protocol("invoke response call_id mismatch"))
            }
            ToolsBody::Error { error } => Err(PortError::tool_unavailable(format!(
                "{}: {}",
                error.code, error.message
            ))),
            _ => Err(PortError::tool_protocol(
                "tools provider returned a non-invoke response",
            )),
        }
    }

    async fn finish(&mut self) -> std::result::Result<(), PortError> {
        finish_stream(&mut self.stream, true).await
    }
}

async fn initialize_stream(
    stream: &mut ServiceStream,
    tool: bool,
) -> std::result::Result<(), PortError> {
    let envelope = stream
        .recv()
        .await
        .ok_or_else(|| unavailable(tool, "service stream closed before initial credit"))?
        .map_err(|error| unavailable(tool, error.to_string()))?;
    if envelope.kind != StreamKind::Credit || envelope.credit_bytes.is_none() {
        return Err(protocol(tool, "service did not begin with a credit frame"));
    }
    stream
        .grant_credit(stream_credit_bytes(tool))
        .await
        .map_err(|error| unavailable(tool, error.to_string()))
}

async fn recv_data(
    stream: &mut ServiceStream,
    tool: bool,
) -> std::result::Result<Vec<u8>, PortError> {
    loop {
        let envelope = stream
            .recv()
            .await
            .ok_or_else(|| unavailable(tool, "service stream closed before response"))?
            .map_err(|error| unavailable(tool, error.to_string()))?;
        match envelope.kind {
            StreamKind::Credit => {}
            StreamKind::Data => {
                return envelope
                    .data
                    .ok_or_else(|| protocol(tool, "DATA frame has no bytes"));
            }
            StreamKind::Cancel | StreamKind::End => {
                return Err(unavailable(tool, "service ended before response"));
            }
            _ => return Err(protocol(tool, "unexpected service stream frame")),
        }
    }
}

async fn finish_stream(
    stream: &mut ServiceStream,
    tool: bool,
) -> std::result::Result<(), PortError> {
    stream
        .half_close()
        .await
        .map_err(|error| unavailable(tool, error.to_string()))?;
    loop {
        let envelope = stream
            .recv()
            .await
            .ok_or_else(|| unavailable(tool, "service stream closed without END"))?
            .map_err(|error| unavailable(tool, error.to_string()))?;
        match envelope.kind {
            StreamKind::Credit => {}
            StreamKind::End => return Ok(()),
            StreamKind::Cancel => return Err(unavailable(tool, "provider cancelled stream")),
            _ => return Err(protocol(tool, "unexpected frame while closing stream")),
        }
    }
}

fn unavailable(tool: bool, message: impl Into<String>) -> PortError {
    if tool {
        PortError::tool_unavailable(message)
    } else {
        PortError::model_unavailable(message)
    }
}

fn protocol(tool: bool, message: impl Into<String>) -> PortError {
    if tool {
        PortError::tool_protocol(message)
    } else {
        PortError::model_protocol(message)
    }
}

#[cfg(test)]
mod tests {
    use rsi_agent_protocol::{ToolResult, ToolsInvokeResponse};

    use super::*;

    #[test]
    fn validated_tool_seam_rejects_invalid_fake_values() {
        assert!(
            ValidatedToolCatalog::validate(ToolsCatalogResponse {
                tools: vec![
                    rsi_agent_protocol::ToolDefinition {
                        name: "duplicate".to_owned(),
                        description: "one".to_owned(),
                        input_schema: serde_json::json!({"type":"object"}),
                    },
                    rsi_agent_protocol::ToolDefinition {
                        name: "duplicate".to_owned(),
                        description: "two".to_owned(),
                        input_schema: serde_json::json!({"type":"object"}),
                    },
                ],
            })
            .is_err()
        );

        assert!(
            ValidatedToolResponse::validate(
                ToolsInvokeResponse {
                    call_id: "wrong-call".to_owned(),
                    result: ToolResult::Error {
                        code: "bad".to_owned(),
                        message: "rejected".to_owned(),
                    },
                },
                "expected-call",
            )
            .is_err()
        );
        assert!(
            ValidatedToolResponse::validate(
                ToolsInvokeResponse {
                    call_id: "expected-call".to_owned(),
                    result: ToolResult::Error {
                        code: String::new(),
                        message: "rejected".to_owned(),
                    },
                },
                "expected-call",
            )
            .is_err()
        );
    }

    #[test]
    fn validated_tool_seam_canonicalizes_arbitrary_json() {
        let catalog = ValidatedToolCatalog::validate(ToolsCatalogResponse {
            tools: vec![rsi_agent_protocol::ToolDefinition {
                name: "ordered".to_owned(),
                description: "ordered schema".to_owned(),
                input_schema: serde_json::json!({"z":{"b":1,"a":2},"a":0}),
            }],
        })
        .expect("valid catalog")
        .into_inner();
        assert_eq!(
            catalog.tools[0]
                .input_schema
                .as_object()
                .expect("schema object")
                .keys()
                .map(String::as_str)
                .collect::<Vec<_>>(),
            ["a", "z"]
        );
        assert_eq!(
            catalog.tools[0].input_schema["z"]
                .as_object()
                .expect("nested schema object")
                .keys()
                .map(String::as_str)
                .collect::<Vec<_>>(),
            ["a", "b"]
        );

        let response = ValidatedToolResponse::validate(
            ToolsInvokeResponse {
                call_id: "expected-call".to_owned(),
                result: ToolResult::Ok {
                    value: serde_json::json!({"z":{"b":1,"a":2},"a":0}),
                },
            },
            "expected-call",
        )
        .expect("valid response")
        .into_inner();
        let ToolResult::Ok { value } = response.result else {
            panic!("expected successful result")
        };
        assert_eq!(
            value
                .as_object()
                .expect("result object")
                .keys()
                .map(String::as_str)
                .collect::<Vec<_>>(),
            ["a", "z"]
        );
        assert_eq!(
            value["z"]
                .as_object()
                .expect("nested result object")
                .keys()
                .map(String::as_str)
                .collect::<Vec<_>>(),
            ["a", "b"]
        );
    }
}
