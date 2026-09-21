//! Validated output metadata, independent of model and presentation protocols.

use crate::{Result, ToolContent, ToolError, ToolResult, validate_identifier, validate_json};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::{collections::BTreeMap, fmt, io::Write, marker::PhantomData, sync::Arc};

/// Maximum encoded bytes of one complete output declaration.
pub const MAXIMUM_TOOL_OUTPUT_DECLARATION_BYTES: usize = 64 * 1024;
/// Maximum declaration bytes, including names, in one immutable Tool catalog.
pub const MAXIMUM_TOOL_OUTPUT_CATALOG_BYTES: usize = 256 * 1024;

/// Immutable successful-output contract. Construction and decoding validate it.
#[derive(Clone)]
pub struct ToolOutputDeclaration(Arc<Declaration>);

struct Declaration {
    wire: WireDeclaration,
    validator: jsonschema::Validator,
    encoded_len: usize,
}

#[derive(Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct WireDeclaration {
    contract_id: String,
    version: u32,
    schema: Value,
    schema_digest: String,
}

impl ToolOutputDeclaration {
    /// Builds a declaration using the bounded schema dialect in the package contract.
    pub fn new(contract_id: impl Into<String>, version: u32, schema: Value) -> Result<Self> {
        let contract_id = contract_id.into();
        validate_identifier("tool output contract", &contract_id)?;
        if version == 0 {
            return Err(invalid("tool output contract version must be positive"));
        }
        validate_structure(&schema, 0, &mut 1024)?;
        encoded_len(&schema)?;
        validate_json("tool output schema", &schema)?;
        validate_schema(&schema)?;
        jsonschema::draft7::meta::validate(&schema)
            .map_err(|_| invalid("invalid tool output schema"))?;
        let schema = canonicalize(schema);
        let mut hash = Sha256::new();
        serde_json::to_writer(&mut hash, &schema)
            .map_err(|_| invalid("tool output schema encoding failed"))?;
        let wire = WireDeclaration {
            contract_id,
            version,
            schema_digest: format!("{:x}", hash.finalize()),
            schema,
        };
        let encoded_len = encoded_len(&wire)?;
        let validator = jsonschema::options()
            .with_draft(jsonschema::Draft::Draft7)
            .build(&wire.schema)
            .map_err(|_| invalid("invalid tool output schema"))?;
        Ok(Self(Arc::new(Declaration {
            wire,
            validator,
            encoded_len,
        })))
    }

    /// Stable author-declared contract identity.
    pub fn contract_id(&self) -> &str {
        &self.0.wire.contract_id
    }
    /// Positive author-declared contract version.
    pub fn version(&self) -> u32 {
        self.0.wire.version
    }
    /// Canonical schema with recursively ordered object keys.
    pub fn schema(&self) -> &Value {
        &self.0.wire.schema
    }
    /// Lowercase SHA-256 of the canonical compact schema.
    pub fn schema_digest(&self) -> &str {
        &self.0.wire.schema_digest
    }
    /// Complete compact declaration size, computed at its validation boundary.
    pub fn encoded_len(&self) -> usize {
        self.0.encoded_len
    }

    /// Checks a canonical value imported across an author or foreign boundary.
    /// Errors never include result values or schema annotations.
    pub fn validate_value(&self, value: &Value) -> Result<()> {
        validate_json("tool output value", value)?;
        // The pinned validator compares object constants in iteration order.
        // Normalize only the validation view; preserve the actual Tool result.
        if !self.0.validator.is_valid(&canonicalize(value.clone())) {
            return Err(invalid(
                "tool output value does not match its declared schema",
            ));
        }
        Ok(())
    }
}

impl fmt::Debug for ToolOutputDeclaration {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.wire.fmt(formatter)
    }
}
impl PartialEq for ToolOutputDeclaration {
    fn eq(&self, other: &Self) -> bool {
        self.0.wire == other.0.wire
    }
}
impl Serialize for ToolOutputDeclaration {
    fn serialize<S: serde::Serializer>(
        &self,
        serializer: S,
    ) -> std::result::Result<S::Ok, S::Error> {
        self.0.wire.serialize(serializer)
    }
}
impl<'de> Deserialize<'de> for ToolOutputDeclaration {
    fn deserialize<D: serde::Deserializer<'de>>(
        deserializer: D,
    ) -> std::result::Result<Self, D::Error> {
        let wire = WireDeclaration::deserialize(deserializer)?;
        let result = Self::new(wire.contract_id, wire.version, wire.schema)
            .map_err(serde::de::Error::custom)?;
        if result.schema_digest() != wire.schema_digest {
            return Err(serde::de::Error::custom(
                "tool output schema digest mismatch",
            ));
        }
        Ok(result)
    }
}

/// Author helper producing canonical JSON and model content from one Rust value.
/// The renderer must be pure; UI rendering remains a consumer of the declaration.
pub struct TypedToolOutput<T> {
    declaration: ToolOutputDeclaration,
    render: fn(&T) -> Vec<ToolContent>,
    marker: PhantomData<fn(T)>,
}
impl<T: Serialize> TypedToolOutput<T> {
    /// Binds a declaration and pure model renderer to the author's result type.
    pub fn new(declaration: ToolOutputDeclaration, render: fn(&T) -> Vec<ToolContent>) -> Self {
        Self {
            declaration,
            render,
            marker: PhantomData,
        }
    }
    /// Declaration to register with the Tool implementation.
    pub fn declaration(&self) -> &ToolOutputDeclaration {
        &self.declaration
    }
    /// Serializes and validates one successful result before rendering it.
    pub fn result(&self, value: &T) -> Result<ToolResult> {
        let canonical =
            serde_json::to_value(value).map_err(|_| invalid("tool output serialization failed"))?;
        self.declaration.validate_value(&canonical)?;
        ToolResult::new(canonical, (self.render)(value), false)
    }
}
impl<T> fmt::Debug for TypedToolOutput<T> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("TypedToolOutput")
            .field("declaration", &self.declaration)
            .finish_non_exhaustive()
    }
}

fn invalid(message: &str) -> ToolError {
    ToolError::InvalidInput(message.into())
}

fn validate_structure(value: &Value, depth: usize, remaining: &mut usize) -> Result<()> {
    if depth > 32 {
        return Err(invalid("tool output schema exceeds depth 32"));
    }
    *remaining = remaining
        .checked_sub(1)
        .ok_or_else(|| invalid("tool output schema exceeds 1024 JSON nodes"))?;
    match value {
        Value::Object(map) => {
            for value in map.values() {
                validate_structure(value, depth + 1, remaining)?;
            }
        }
        Value::Array(items) => {
            for value in items {
                validate_structure(value, depth + 1, remaining)?;
            }
        }
        _ => {}
    }
    Ok(())
}

fn validate_schema(schema: &Value) -> Result<()> {
    if schema.is_boolean() {
        return Ok(());
    }
    let map = schema
        .as_object()
        .ok_or_else(|| invalid("tool output schema must be an object or boolean"))?;
    for (key, value) in map {
        match key.as_str() {
            "type" | "required" | "enum" | "const" | "title" | "description" | "default"
            | "examples" => {}
            "properties" => {
                let properties = value
                    .as_object()
                    .ok_or_else(|| invalid("invalid tool output properties"))?;
                for schema in properties.values() {
                    validate_schema(schema)?;
                }
            }
            "items" | "additionalProperties" => validate_schema(value)?,
            _ => return Err(invalid("unsupported tool output schema keyword")),
        }
    }
    Ok(())
}

fn canonicalize(value: Value) -> Value {
    match value {
        Value::Object(map) => Value::Object(
            map.into_iter()
                .map(|(key, value)| (key, canonicalize(value)))
                .collect::<BTreeMap<_, _>>()
                .into_iter()
                .collect(),
        ),
        Value::Array(values) => Value::Array(values.into_iter().map(canonicalize).collect()),
        value => value,
    }
}

fn encoded_len(value: &impl Serialize) -> Result<usize> {
    struct Counter(usize);
    impl Write for Counter {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0 = self
                .0
                .checked_add(bytes.len())
                .filter(|length| *length <= MAXIMUM_TOOL_OUTPUT_DECLARATION_BYTES)
                .ok_or_else(|| std::io::Error::other("tool output declaration too large"))?;
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let mut counter = Counter(0);
    serde_json::to_writer(&mut counter, value)
        .map_err(|_| invalid("tool output declaration exceeds 64 KiB"))?;
    Ok(counter.0)
}
