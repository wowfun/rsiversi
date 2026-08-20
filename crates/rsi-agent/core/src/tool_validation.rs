use std::collections::BTreeMap;

use jsonschema::{Retrieve, Uri};
use rsi_agent_protocol::ToolDefinition;
use serde_json::Value;

pub(crate) struct PreparedTool {
    pub(crate) validator: jsonschema::Validator,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ArgumentError {
    InvalidJson,
    LossyNumber,
    SchemaMismatch,
}

pub(crate) struct ValidatedArguments {
    pub(crate) canonical_json: String,
}

pub(crate) fn prepare_catalog(
    definitions: &[ToolDefinition],
) -> std::result::Result<BTreeMap<String, PreparedTool>, String> {
    let mut tools = BTreeMap::new();
    for definition in definitions {
        let validator = compile_schema(&definition.name, &definition.input_schema)?;
        let name = definition.name.clone();
        if tools.insert(name, PreparedTool { validator }).is_some() {
            return Err("tool catalog contains duplicate names".to_owned());
        }
    }
    Ok(tools)
}

pub(crate) fn validate_arguments(
    tool: &PreparedTool,
    raw: &str,
) -> std::result::Result<ValidatedArguments, ArgumentError> {
    let value = rsi_agent_protocol::parse_json_strict_f64(raw).map_err(|error| match error {
        rsi_agent_protocol::ProtocolError::LossyJsonNumber => ArgumentError::LossyNumber,
        _ => ArgumentError::InvalidJson,
    })?;
    tool.validator
        .validate(&value)
        .map_err(|_| ArgumentError::SchemaMismatch)?;
    // The strict parser inserts every object in key order. Number
    // normalization mutates only leaves, so direct serialization is already
    // the canonical provider value and avoids a second recursive clone.
    let canonical_json = serde_json::to_string(&value).map_err(|_| ArgumentError::InvalidJson)?;
    if canonical_json
        .chars()
        .take(rsi_agent_protocol::MAX_CONTENT_CHARS + 1)
        .count()
        > rsi_agent_protocol::MAX_CONTENT_CHARS
    {
        return Err(ArgumentError::InvalidJson);
    }
    Ok(ValidatedArguments { canonical_json })
}

fn compile_schema(
    name: &str,
    input_schema: &Value,
) -> std::result::Result<jsonschema::Validator, String> {
    jsonschema::draft202012::options()
        .with_retriever(RejectExternalSchemas)
        .should_validate_formats(true)
        .build(input_schema)
        .map_err(|error| format!("tool `{name}` schema cannot be compiled: {error}"))
}

#[derive(Debug)]
struct RejectExternalSchemas;

impl Retrieve for RejectExternalSchemas {
    fn retrieve(
        &self,
        _uri: &Uri<String>,
    ) -> std::result::Result<Value, Box<dyn std::error::Error + Send + Sync>> {
        Err("external JSON Schema references are disabled".into())
    }
}
