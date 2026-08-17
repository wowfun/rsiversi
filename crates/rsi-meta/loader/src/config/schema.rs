use jsonschema::{Retrieve, Uri};
use serde_json::Value;

use super::ConfigPrepareError;

/// One package schema parsed and compiled once for all mounted instances.
#[derive(Debug)]
pub struct PreparedConfigSchema {
    pub(super) schema: Option<Value>,
    pub(super) unresolved: Option<jsonschema::Validator>,
    pub(super) resolved: Option<jsonschema::Validator>,
}

impl PreparedConfigSchema {
    /// Parsed schema retained for inspection without reparsing package bytes.
    pub const fn schema(&self) -> Option<&Value> {
        self.schema.as_ref()
    }
}

pub(super) fn compile_validator(
    schema: &Value,
) -> Result<jsonschema::Validator, ConfigPrepareError> {
    jsonschema::draft202012::options()
        .with_retriever(RejectExternalSchemas)
        .should_validate_formats(true)
        .build(schema)
        .map_err(|_| ConfigPrepareError::InvalidSchema)
}

pub(super) fn validate_compiled(
    validator: &jsonschema::Validator,
    instance: &Value,
) -> Result<(), ConfigPrepareError> {
    validator
        .validate(instance)
        .map_err(|error| ConfigPrepareError::InvalidConfig {
            instance_path: error.instance_path.to_string(),
        })
}

#[derive(Debug)]
struct RejectExternalSchemas;

impl Retrieve for RejectExternalSchemas {
    fn retrieve(
        &self,
        _uri: &Uri<String>,
    ) -> Result<Value, Box<dyn std::error::Error + Send + Sync>> {
        Err("external JSON Schema references are disabled".into())
    }
}
