//! Native preset catalog reads; defaults and Draft selection retain their owners.
use super::{Empty, Never};
use rsi_agent_session_protocol::AgentPresetId;
use rsi_api_protocol::{
    ApiClient, ApiError, OperationAccess, OperationClass, OperationEffect, OperationId,
    OperationSpec, RequestEncoding, Result, call_json,
};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

/// Read-only operations against the actual composition catalog.
#[derive(Clone, Copy, Debug)]
pub enum AgentPresetsOperation {
    /// Ordered, path-free catalog with its current default.
    List,
    /// Exact native source of one healthy preset.
    Document,
}
impl AgentPresetsOperation {
    /// Returns the negotiated operation and encoded byte limits.
    ///
    /// # Panics
    /// Static operation names must be valid API identities.
    pub fn spec(self) -> OperationSpec {
        let (name, maximum_response_bytes) = match self {
            Self::List => ("list", 16 * 1024 * 1024),
            Self::Document => ("document", 4 * 1024 * 1024),
        };
        OperationSpec {
            protocol: rsi_api_protocol::wire_contract!(),
            id: OperationId::new("agent-presets", name, 1).expect("static operation"),
            access: OperationAccess::Authenticated,
            class: OperationClass::Data,
            effect: OperationEffect::Read,
            encoding: RequestEncoding::Json,
            maximum_request_bytes: 1024,
            maximum_response_bytes,
        }
    }
}

/// One catalog row; absence of `broken` proves compilation, not activation.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct AgentPresetRow {
    /// Exact catalog identity.
    pub id: AgentPresetId,
    /// Effective default at discovery time.
    pub is_default: bool,
    /// Optional display name from the owning metadata file.
    pub name: Option<String>,
    /// Optional description from the owning metadata file.
    pub description: Option<String>,
    /// Safe catalog diagnostic for a broken source.
    pub broken: Option<String>,
}

/// Fresh catalog in the source owner's precedence and display order.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct AgentPresetList {
    /// At most 256 rows, with unique identities.
    pub presets: Vec<AgentPresetRow>,
}

/// Native composition document format.
#[derive(Clone, Copy, Debug, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum AgentPresetFormat {
    /// RSI Profile TOML, not a DSH plugin entry list.
    Toml,
}

/// Exact, read-only native composition source.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct AgentPresetDocument {
    /// Requested identity.
    pub id: AgentPresetId,
    /// Language of the returned source.
    pub format: AgentPresetFormat,
    /// Complete source without conversion or truncation.
    pub content: String,
    /// Optional display name.
    pub name: Option<String>,
    /// Optional description.
    pub description: Option<String>,
}

/// Exact document lookup; identifiers are validated during decoding.
#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct AgentPresetDocumentRequest {
    /// Preset to read from the winning root.
    pub id: AgentPresetId,
}

/// Definite document lookup failures, distinct from transport availability.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(tag = "code", rename_all = "snake_case", deny_unknown_fields)]
pub enum AgentPresetDocumentError {
    /// No root supplied the requested identity.
    NotFound,
    /// The winning source could not be compiled or read consistently.
    Broken {
        /// Safe diagnostic from the owning catalog.
        reason: String,
    },
}

/// Typed catalog reads through the caller's existing API connection.
#[derive(Clone, Debug)]
pub struct AgentPresetsClient {
    api: Arc<dyn ApiClient>,
}
impl AgentPresetsClient {
    /// Uses this connection's origin and negotiated registrations.
    pub fn new(api: Arc<dyn ApiClient>) -> Self {
        Self { api }
    }

    /// Reads a fresh catalog without building a generation.
    pub async fn list(&self) -> Result<AgentPresetList> {
        let list: AgentPresetList = match call_json::<_, _, Never>(
            self.api.as_ref(),
            &AgentPresetsOperation::List.spec(),
            &Empty {},
        )
        .await?
        {
            Ok(list) => list,
            Err(never) => match never {},
        };
        let mut ids = std::collections::BTreeSet::new();
        if list.presets.len() > 256
            || list.presets.iter().any(|row| !ids.insert(&row.id))
            || list.presets.iter().filter(|row| row.is_default).count() > 1
        {
            return Err(ApiError::Invalid("invalid Agent preset catalog".into()));
        }
        Ok(list)
    }

    /// Reads an exact document or a definite catalog failure.
    pub async fn document(
        &self,
        id: &AgentPresetId,
    ) -> Result<std::result::Result<AgentPresetDocument, AgentPresetDocumentError>> {
        let result: std::result::Result<AgentPresetDocument, AgentPresetDocumentError> = call_json(
            self.api.as_ref(),
            &AgentPresetsOperation::Document.spec(),
            &AgentPresetDocumentRequest { id: id.clone() },
        )
        .await?;
        if result.as_ref().is_ok_and(|document| &document.id != id) {
            return Err(ApiError::Invalid(
                "Agent preset document identity differs".into(),
            ));
        }
        Ok(result)
    }
}
