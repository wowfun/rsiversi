//! Bounded initial-activation output contracts and exact durable result references.
use crate::{ActivationId, MessageId, Result, SessionError, SessionId, TurnId, validate_sha256};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

/// Maximum serialized schema bytes.
pub const MAXIMUM_OUTPUT_SCHEMA_BYTES: usize = 64 * 1024;
/// Maximum serialized model-authored schema bytes.
pub const MAXIMUM_MODEL_OUTPUT_SCHEMA_BYTES: usize = 8 * 1024;
/// Maximum serialized accepted value bytes.
pub const MAXIMUM_OUTPUT_VALUE_BYTES: usize = 256 * 1024;
/// Maximum UTF-8 preview bytes, before enclosing JSON encoding.
pub const MAXIMUM_OUTPUT_PREVIEW_BYTES: usize = 2 * 1024;
/// Complete encoded completion message reservation.
pub const MAXIMUM_COMPLETION_MESSAGE_BYTES: usize = 8 * 1024;
/// Reserved claim-local reporting tool.
pub const REPORT_RESULT_TOOL: &str = "report_result";

/// Validated finite, local-only Draft 7 object schema.
#[derive(Clone)]
pub struct OutputContract(std::sync::Arc<CompiledOutput>);
struct CompiledOutput {
    schema: Value,
    validator: std::sync::OnceLock<std::result::Result<jsonschema::Validator, ()>>,
    sha256: String,
}
impl std::fmt::Debug for OutputContract {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_tuple("OutputContract")
            .field(self.schema())
            .finish()
    }
}
impl PartialEq for OutputContract {
    fn eq(&self, other: &Self) -> bool {
        std::sync::Arc::ptr_eq(&self.0, &other.0)
            || (self.0.sha256 == other.0.sha256 && self.schema() == other.schema())
    }
}
impl Eq for OutputContract {}
impl Serialize for OutputContract {
    fn serialize<S: serde::Serializer>(
        &self,
        serializer: S,
    ) -> std::result::Result<S::Ok, S::Error> {
        self.schema().serialize(serializer)
    }
}
impl<'de> Deserialize<'de> for OutputContract {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> std::result::Result<Self, D::Error> {
        Self::new(Value::deserialize(d)?).map_err(serde::de::Error::custom)
    }
}
impl OutputContract {
    /// Admits model-authored finite schemas with no instruction annotations and at most 8 KiB.
    pub fn from_model_schema(schema: Value) -> Result<Self> {
        Self::admit(schema, MAXIMUM_MODEL_OUTPUT_SCHEMA_BYTES, false)
    }

    /// Validates the schema before child admission or provider dispatch.
    pub fn new(schema: Value) -> Result<Self> {
        Self::admit(schema, MAXIMUM_OUTPUT_SCHEMA_BYTES, true)
    }

    fn admit(schema: Value, maximum_bytes: usize, annotations: bool) -> Result<Self> {
        let encoded = bounded_json(&schema, maximum_bytes)?;
        if schema.get("type").and_then(Value::as_str) != Some("object") {
            return Err(invalid("output schema root must have type object"));
        }
        validate_schema(&schema, 0, &mut 64, annotations)?;
        jsonschema::draft7::meta::validate(&schema)
            .map_err(|_| invalid("invalid Draft 7 output schema"))?;
        Ok(Self(std::sync::Arc::new(CompiledOutput {
            schema,
            validator: std::sync::OnceLock::new(),
            sha256: encoded.sha256,
        })))
    }
    /// Real input schema used by the reporting Tool.
    pub fn schema(&self) -> &Value {
        &self.0.schema
    }
    /// Digest of the exact compact JSON schema bytes, including key order.
    pub fn sha256(&self) -> String {
        self.0.sha256.clone()
    }
    /// Validates one bounded value without fetching remote schemas.
    pub fn validate_value(&self, value: &Value) -> Result<()> {
        validate_structure(value, 0, &mut 100_000)?;
        crate::bounded_compact_json_len(value, MAXIMUM_OUTPUT_VALUE_BYTES)
            .map_err(|_| invalid("output JSON exceeds its byte limit"))?;
        self.validate_bounded_value(value)
    }
    fn validate_bounded_value(&self, value: &Value) -> Result<()> {
        let validator = self.0.validator.get_or_init(|| {
            jsonschema::options()
                .with_draft(jsonschema::Draft::Draft7)
                .build(&self.0.schema)
                .map_err(|_| ())
        });
        let validator = validator
            .as_ref()
            .map_err(|()| invalid("invalid Draft 7 output schema"))?;
        if let Some(error) = validator.iter_errors(value).next() {
            // Pointers describe the failed constraint without rendering instance data.
            let instance: String = error.instance_path.as_str().chars().take(256).collect();
            let schema: String = error.schema_path.as_str().chars().take(256).collect();
            return Err(invalid(&format!(
                "value at {instance:?} does not match output schema at {schema:?}"
            )));
        }
        Ok(())
    }
    /// Produces bounded metadata only after successful value validation.
    pub fn summarize(&self, value: &Value) -> Result<StructuredResultSummary> {
        let encoded = bounded_json(value, MAXIMUM_OUTPUT_VALUE_BYTES)?;
        self.validate_bounded_value(value)?;

        Ok(StructuredResultSummary {
            schema_sha256: self.sha256(),
            value_sha256: encoded.sha256,
            preview: encoded.preview,
        })
    }
}

/// Initial spawn input carrying the contract; follow-ups never inherit it.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InitialOutputContract {
    /// Exact initial accepted spawn message.
    pub message_id: MessageId,
    /// Bounded object schema.
    pub contract: OutputContract,
}

/// Small descriptor of a value held only in its `ToolResult` Fact.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StructuredResultSummary {
    /// Frozen schema SHA-256.
    pub schema_sha256: String,
    /// SHA-256 of the exact compact JSON value bytes, including key order.
    pub value_sha256: String,
    /// UTF-8 bounded display excerpt, not an independently parseable value.
    pub preview: String,
}
impl StructuredResultSummary {
    /// Validates metadata read from a durable boundary.
    pub fn validate(&self) -> Result<()> {
        validate_sha256("output schema digest", &self.schema_sha256)?;
        validate_sha256("output value digest", &self.value_sha256)?;
        if self.preview.len() > MAXIMUM_OUTPUT_PREVIEW_BYTES {
            return Err(invalid("output preview is too large"));
        }
        Ok(())
    }
}

/// Pure Tool settlement request, authoritative only after Kernel publication.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ToolConclusion {
    /// Optional schema-validated output descriptor.
    pub structured: Option<StructuredResultSummary>,
}
impl ToolConclusion {
    /// Revalidates its bounded durable representation.
    pub fn validate(&self) -> Result<()> {
        self.structured
            .as_ref()
            .map_or(Ok(()), StructuredResultSummary::validate)
    }
}

/// Exact immutable output locator, never a latest-result query.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentResultRef {
    /// Child which owns the result.
    pub child_session_id: SessionId,
    /// Exact activation whose final success authorizes reading it.
    pub activation_id: ActivationId,
    /// Exact producing Turn.
    pub turn_id: TurnId,
    /// Exact `ToolResult` Fact sequence.
    pub fact_seq: u64,
    /// Validated digest and display metadata.
    pub summary: StructuredResultSummary,
}
impl AgentResultRef {
    /// Revalidates the locator at durable/public boundaries.
    pub fn validate(&self) -> Result<()> {
        if self.fact_seq == 0 {
            return Err(invalid("output Fact sequence must be positive"));
        }
        self.summary.validate()
    }
}

fn invalid(message: &str) -> SessionError {
    SessionError::Invalid(message.into())
}
struct Encoded {
    sha256: String,
    preview: String,
}
fn validate_structure(value: &Value, depth: usize, remaining: &mut usize) -> Result<()> {
    *remaining = remaining
        .checked_sub(1)
        .ok_or_else(|| invalid("output JSON exceeds structural limits"))?;
    if depth > 64 {
        return Err(invalid("output JSON exceeds structural limits"));
    }
    match value {
        Value::Object(map) => {
            for value in map.values() {
                validate_structure(value, depth + 1, remaining)?;
            }
        }
        Value::Array(values) => {
            for value in values {
                validate_structure(value, depth + 1, remaining)?;
            }
        }
        _ => {}
    }
    Ok(())
}
fn bounded_json(value: &Value, maximum: usize) -> Result<Encoded> {
    struct Encoder {
        remaining: usize,
        hash: Sha256,
        preview: Vec<u8>,
    }
    impl std::io::Write for Encoder {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.remaining = self
                .remaining
                .checked_sub(bytes.len())
                .ok_or_else(|| std::io::Error::other("output JSON exceeds its byte limit"))?;
            self.hash.update(bytes);
            let count = bytes
                .len()
                .min(MAXIMUM_OUTPUT_PREVIEW_BYTES - self.preview.len());
            self.preview.extend_from_slice(&bytes[..count]);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    validate_structure(value, 0, &mut 100_000)?;
    let mut writer = Encoder {
        remaining: maximum,
        hash: Sha256::new(),
        preview: Vec::new(),
    };
    serde_json::to_writer(&mut writer, value)
        .map_err(|_| invalid("output JSON exceeds its byte limit"))?;
    // The complete encoding is UTF-8; only the bounded excerpt may split a codepoint.
    if let Err(error) = std::str::from_utf8(&writer.preview) {
        writer.preview.truncate(error.valid_up_to());
    }
    Ok(Encoded {
        sha256: format!("{:x}", writer.hash.finalize()),
        preview: String::from_utf8(writer.preview)
            .map_err(|_| invalid("output JSON encoding failed"))?,
    })
}
fn validate_schema_keyword(key: &str, annotations: bool) -> Result<()> {
    if !annotations && matches!(key, "title" | "description" | "default" | "examples") {
        return Err(invalid(
            "schema annotations are not accepted; put instructions in message",
        ));
    }
    if !matches!(
        key,
        "type"
            | "oneOf"
            | "properties"
            | "required"
            | "additionalProperties"
            | "items"
            | "enum"
            | "const"
            | "title"
            | "description"
            | "default"
            | "examples"
    ) {
        return Err(invalid("unsupported output schema keyword"));
    }
    Ok(())
}
fn validate_schema(
    schema: &Value,
    depth: usize,
    branches_left: &mut usize,
    annotations: bool,
) -> Result<()> {
    if depth > 32 {
        return Err(invalid("output schema nesting exceeds 32"));
    }
    let map = schema
        .as_object()
        .ok_or_else(|| invalid("schema must be an object"))?;
    for key in map.keys() {
        validate_schema_keyword(key, annotations)?;
    }
    for key in ["title", "description"] {
        if map.get(key).is_some_and(|value| !value.is_string()) {
            return Err(invalid("schema text annotation must be a string"));
        }
    }
    if let Some(branches) = map.get("oneOf") {
        if map.keys().any(|key| {
            !matches!(
                key.as_str(),
                "oneOf" | "title" | "description" | "default" | "examples"
            )
        }) {
            return Err(invalid("oneOf cannot have constraint siblings"));
        }
        let branches = branches
            .as_array()
            .filter(|branches| branches.len() >= 2)
            .ok_or_else(|| invalid("oneOf requires at least two schemas"))?;
        *branches_left = branches_left
            .checked_sub(branches.len())
            .ok_or_else(|| invalid("output schema exceeds 64 total oneOf branches"))?;
        for branch in branches {
            validate_schema(branch, depth + 1, branches_left, annotations)?;
        }
        return Ok(());
    }
    let kind = map
        .get("type")
        .map(|value| {
            value
                .as_str()
                .ok_or_else(|| invalid("schema type must be one string"))
        })
        .transpose()?;
    if kind.is_some_and(|kind| {
        !matches!(
            kind,
            "object" | "array" | "string" | "number" | "integer" | "boolean" | "null"
        )
    }) {
        return Err(invalid("unsupported schema type"));
    }
    validate_object_constraints(map, kind, depth, branches_left, annotations)?;
    if let Some(items) = map.get("items") {
        if kind != Some("array") {
            return Err(invalid("items requires array type"));
        }
        validate_schema(items, depth + 1, branches_left, annotations)?;
    }
    if let Some(values) = map.get("enum") {
        let values = values
            .as_array()
            .filter(|values| !values.is_empty())
            .ok_or_else(|| invalid("enum must be nonempty"))?;
        for value in values {
            scalar(kind, value)?;
        }
        if let Some(value) = map.get("const")
            && !values.contains(value)
        {
            return Err(invalid("const must belong to enum"));
        }
    }
    if let Some(value) = map.get("const") {
        scalar(kind, value)?;
    }
    Ok(())
}
fn validate_object_constraints(
    map: &serde_json::Map<String, Value>,
    kind: Option<&str>,
    depth: usize,
    branches_left: &mut usize,
    annotations: bool,
) -> Result<()> {
    if map.keys().any(|key| {
        matches!(
            key.as_str(),
            "properties" | "required" | "additionalProperties"
        ) && kind != Some("object")
    }) {
        return Err(invalid("object constraints require object type"));
    }
    if let Some(properties) = map.get("properties") {
        for property in properties
            .as_object()
            .ok_or_else(|| invalid("properties must be an object"))?
            .values()
        {
            validate_schema(property, depth + 1, branches_left, annotations)?;
        }
    }
    if let Some(required) = map.get("required") {
        let mut seen = std::collections::BTreeSet::new();
        for key in required
            .as_array()
            .ok_or_else(|| invalid("required must be an array"))?
        {
            let key = key
                .as_str()
                .ok_or_else(|| invalid("required names must be strings"))?;
            if !seen.insert(key)
                || map
                    .get("properties")
                    .and_then(|properties| properties.get(key))
                    .is_none()
            {
                return Err(invalid("required must name unique declared properties"));
            }
        }
    }
    if map
        .get("additionalProperties")
        .is_some_and(|value| !value.is_boolean())
    {
        return Err(invalid("additionalProperties must be boolean"));
    }
    Ok(())
}
fn scalar(kind: Option<&str>, value: &Value) -> Result<()> {
    let matches = match kind {
        Some("string") => value.is_string(),
        Some("boolean") => value.is_boolean(),
        Some("null") => value.is_null(),
        Some("number") => value.is_number(),
        Some("integer") => value.as_f64().is_some_and(|v| v.fract() == 0.0),
        None => !value.is_object() && !value.is_array(),
        _ => false,
    };
    if !matches {
        return Err(invalid("enum and const require type-correct scalar values"));
    }
    Ok(())
}

/// Small model-facing coordinates for reading a settled result.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentResultLocator {
    /// Exact child Session.
    pub child_session_id: SessionId,
    /// Exact successful activation.
    pub activation_id: ActivationId,
    /// Exact producing Turn.
    pub turn_id: TurnId,
    /// Exact `ToolResult` Fact.
    pub fact_seq: u64,
}
impl AgentResultRef {
    /// Coordinates; authoritative digests come from the committed Completion.
    pub fn locator(&self) -> AgentResultLocator {
        AgentResultLocator {
            child_session_id: self.child_session_id.clone(),
            activation_id: self.activation_id.clone(),
            turn_id: self.turn_id.clone(),
            fact_seq: self.fact_seq,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn decode_validates_schema_without_compiling_value_validator() {
        let contract: OutputContract =
            serde_json::from_str(r#"{"type":"object","properties":{"answer":{"type":"integer"}}}"#)
                .unwrap();
        assert!(contract.0.validator.get().is_none());
        let clone = contract.clone();
        clone.validate_value(&json!({"answer":42})).unwrap();
        assert!(contract.0.validator.get().is_some());
        assert!(
            serde_json::from_str::<OutputContract>(r#"{"type":"object","examples":42}"#).is_err()
        );
        let reordered: OutputContract =
            serde_json::from_str(r#"{"properties":{"answer":{"type":"integer"}},"type":"object"}"#)
                .unwrap();
        assert_ne!(contract.sha256(), reordered.sha256());
        assert_ne!(contract, reordered);
    }
    #[test]
    fn streaming_digest_and_preview_match_exact_json_bytes() {
        let contract = OutputContract::new(json!({"type":"object"})).unwrap();
        for text in ["界🦀".repeat(1000), "\u{0001}".repeat(1000), "short".into()] {
            let value = json!({"answer":text});
            let encoded = serde_json::to_string(&value).unwrap();
            let summary = contract.summarize(&value).unwrap();
            assert_eq!(
                summary.value_sha256,
                format!("{:x}", Sha256::digest(encoded.as_bytes()))
            );
            assert!(encoded.starts_with(&summary.preview));
            assert!(summary.preview.len() <= MAXIMUM_OUTPUT_PREVIEW_BYTES);
            assert!(
                encoded.len() <= MAXIMUM_OUTPUT_PREVIEW_BYTES
                    || summary.preview.len() >= MAXIMUM_OUTPUT_PREVIEW_BYTES - 3
            );
        }
        assert!(
            contract
                .summarize(&json!({"huge":"\u{0001}".repeat(MAXIMUM_OUTPUT_VALUE_BYTES / 2)}))
                .is_err()
        );
    }
    #[test]
    fn rejected_values_identify_paths_without_echoing_values() {
        let contract = OutputContract::new(
            json!({"type":"object","properties":{"answer":{"type":"integer"}}}),
        )
        .unwrap();
        let error = contract
            .validate_value(&json!({"answer":"private-value"}))
            .unwrap_err()
            .to_string();
        assert!(error.contains("/answer"), "{error}");
        assert!(error.contains("/properties/answer/type"), "{error}");
        assert!(!error.contains("private-value"));
        assert_eq!(serde_json::to_value(&contract).unwrap(), *contract.schema());
        assert_eq!(
            serde_json::from_value::<OutputContract>(contract.schema().clone()).unwrap(),
            contract
        );
    }
    #[test]
    fn combinator_work_is_bounded_across_the_schema() {
        let branches = vec![json!({"type":"integer"}); 65];
        assert!(
            OutputContract::new(
                json!({"type":"object","properties":{"answer":{"oneOf":branches}}})
            )
            .is_err()
        );
        let branches = vec![json!({"type":"integer"}); 33];
        assert!(OutputContract::new(json!({"type":"object","properties":{"a":{"oneOf":branches},"b":{"oneOf":branches}}})).is_err());
    }
    #[test]
    fn local_finite_schema_and_value_bounds_are_enforced() {
        let contract = OutputContract::new(json!({"type":"object", "properties":{"name":{"type":"string"},"answer":{"oneOf":[{"type":"integer"},{"type":"null"}]}},"required":["name","answer"],"additionalProperties":false})).unwrap();
        assert!(
            contract
                .validate_value(&json!({"name":"中文","answer":42}))
                .is_ok()
        );
        for value in [
            json!({"name":"x"}),
            json!({"name":3,"answer":null}),
            json!({"name":"x","answer":1.5}),
            json!({"name":"x","answer":null,"extra":true}),
        ] {
            assert!(contract.validate_value(&value).is_err());
        }
        for schema in [
            json!({"type":"object","$ref":"https://example.invalid/schema"}),
            json!({"type":"object","properties":{"x":{"pattern":".*"}}}),
            json!({"type":"object","oneOf":[{},{}]}),
            json!({"type":"object","required":["absent"]}),
            json!({"type":"object","properties":{"x":{"type":["string","null"]}}}),
            json!({"type":"object","additionalProperties":{}}),
        ] {
            assert!(OutputContract::new(schema).is_err());
        }
        assert!(
            contract
                .validate_value(
                    &json!({"name":"x".repeat(MAXIMUM_OUTPUT_VALUE_BYTES),"answer":null})
                )
                .is_err()
        );
        let summary = contract
            .summarize(&json!({"name":"界".repeat(2000),"answer":1}))
            .unwrap();
        assert!(summary.preview.len() <= MAXIMUM_OUTPUT_PREVIEW_BYTES);
        assert!(
            serde_json::from_str::<OutputContract>(r#"{"type":"object","$id":"evil"}"#).is_err()
        );
        assert!(
            OutputContract::new(
                json!({"type":"object","description":"x".repeat(MAXIMUM_OUTPUT_SCHEMA_BYTES)})
            )
            .is_err()
        );
    }
}
