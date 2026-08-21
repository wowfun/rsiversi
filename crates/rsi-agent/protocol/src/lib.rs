//! Closed, bounded semantic DATA protocols for `rsi-agent` providers.
//!
//! The transport is owned by `rsi-meta`; this crate owns only the JSON payload
//! placed in one service DATA frame. Callers must use [`ToolsEnvelope::decode`]
//! at an untrusted boundary. AI model semantics are owned by `rsi-ai-protocol`.

use std::{
    collections::{BTreeMap, BTreeSet},
    io,
};

use serde::{Deserialize, Deserializer, Serialize, de};
use serde_json::{Map, Number, Value, value::RawValue};
use thiserror::Error;

pub const TOOLS_SERVICE_KEY: &str = "rsi.agent.tools";
pub const TOOLS_PROTOCOL: &str = TOOLS_SERVICE_KEY;
pub const WIRE_VERSION: u32 = 0;
pub const MAX_DATA_BYTES: usize = 768 * 1024;
pub const MAX_ID_BYTES: usize = 255;

/// Returns whether a value satisfies the shared durable and service identifier grammar.
#[must_use]
pub fn is_wire_identifier(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= MAX_ID_BYTES
        && value.bytes().all(|byte| byte.is_ascii_graphic())
}
pub const MAX_TOOLS: usize = 64;
pub const MAX_CATALOG_BYTES: usize = 256 * 1024;
pub const MAX_CONTENT_CHARS: usize = 64 * 1024;
pub const MAX_JSON_DEPTH: usize = 64;
pub const MAX_JSON_NODES: usize = 65_536;

pub const MAX_DESCRIPTION_CHARS: usize = 4 * 1024;
pub const MAX_ERROR_CODE_BYTES: usize = 64;
pub const MAX_ERROR_MESSAGE_CHARS: usize = 4 * 1024;

/// Provider-neutral declaration of one callable tool.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ToolDefinition {
    /// Stable callable name within the catalog.
    pub name: String,
    /// Model-facing explanation of the operation.
    pub description: String,
    /// JSON Schema for the tool's argument object.
    pub input_schema: Value,
}

impl ToolDefinition {
    fn validate(&self, field: &str) -> Result<(), ProtocolError> {
        require_tool_name(&format!("{field}.name"), &self.name)?;
        require_text(
            &format!("{field}.description"),
            &self.description,
            MAX_DESCRIPTION_CHARS,
            true,
        )?;
        if !matches!(self.input_schema, Value::Object(_) | Value::Bool(_)) {
            return invalid(
                format!("{field}.input_schema"),
                "must be an object or boolean JSON Schema",
            );
        }
        validate_json(&self.input_schema)?;
        Ok(())
    }
}

/// Stable semantic result presented to the model and transcript.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case", deny_unknown_fields)]
pub enum ToolResult {
    /// Successful JSON result.
    Ok {
        /// Arbitrary bounded JSON returned by the tool.
        value: Value,
    },
    /// Stable tool-level failure presented to the model and transcript.
    Error {
        /// Machine-readable failure code.
        code: String,
        /// Bounded human-readable failure summary.
        message: String,
    },
}

impl ToolResult {
    /// Validates the aggregate result without requiring a surrounding envelope.
    ///
    /// This is the durable-transcript validation seam: callers that already
    /// have a typed result should not manufacture unrelated request fields just
    /// to enforce result bounds.
    ///
    /// # Errors
    ///
    /// Returns [`ProtocolError::InvalidField`], [`ProtocolError::JsonLimit`], or
    /// [`ProtocolError::LossyJsonNumber`] when the result is outside the wire
    /// contract.
    pub fn validate(&self, field: &str) -> Result<(), ProtocolError> {
        match self {
            Self::Ok { value } => {
                validate_json(value)?;
                Ok(())
            }
            Self::Error { code, message } => {
                require_code(&format!("{field}.code"), code)?;
                require_text(
                    &format!("{field}.message"),
                    message,
                    MAX_ERROR_MESSAGE_CHARS,
                    true,
                )
            }
        }
    }
}

/// Stable service-level error payload.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WireError {
    /// Machine-readable service failure code.
    pub code: String,
    /// Bounded human-readable failure summary.
    pub message: String,
}

impl WireError {
    fn validate(&self, field: &str) -> Result<(), ProtocolError> {
        require_code(&format!("{field}.code"), &self.code)?;
        require_text(
            &format!("{field}.message"),
            &self.message,
            MAX_ERROR_MESSAGE_CHARS,
            true,
        )
    }
}

/// Payload of a successful tools catalog response.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ToolsCatalogResponse {
    /// Canonically ordered callable tool declarations.
    pub tools: Vec<ToolDefinition>,
}

impl ToolsCatalogResponse {
    /// Validates the aggregate catalog without requiring a surrounding envelope.
    ///
    /// # Errors
    ///
    /// Returns a field, JSON-complexity, number, or canonical-size error when
    /// the catalog is outside the wire contract.
    pub fn validate(&self, field: &str) -> Result<(), ProtocolError> {
        validate_catalog(&self.tools, field)
    }
}

/// Payload of one tool invocation request.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ToolsInvokeRequest {
    /// Call identifier correlated with the model's tool request.
    pub call_id: String,
    /// Exact tool name from the current catalog.
    pub name: String,
    /// Raw JSON argument text supplied by the model.
    pub arguments: String,
}

impl ToolsInvokeRequest {
    fn validate(&self) -> Result<(), ProtocolError> {
        require_identifier("call_id", &self.call_id)?;
        require_tool_name("name", &self.name)?;
        require_text("arguments", &self.arguments, MAX_CONTENT_CHARS, false)
    }
}

/// Payload of one tool invocation response.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ToolsInvokeResponse {
    /// Call identifier from the corresponding invocation request.
    pub call_id: String,
    /// Durable semantic tool result.
    pub result: ToolResult,
}

impl ToolsInvokeResponse {
    fn validate(&self) -> Result<(), ProtocolError> {
        require_identifier("call_id", &self.call_id)?;
        self.result.validate("result")
    }
}

/// Kind-specific body of a tools envelope.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum ToolsBody {
    CatalogRequest {},
    CatalogResponse(ToolsCatalogResponse),
    InvokeRequest(ToolsInvokeRequest),
    InvokeResponse(ToolsInvokeResponse),
    Error { error: WireError },
}

/// A closed, versioned tools-service envelope.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ToolsEnvelope {
    /// Exact protocol identity; must equal [`TOOLS_PROTOCOL`].
    pub protocol: String,
    /// Exact semantic wire version; only [`WIRE_VERSION`] is accepted.
    #[serde(deserialize_with = "deserialize_wire_version")]
    pub version: u32,
    /// Caller-assigned envelope correlation identifier.
    pub request_id: String,
    /// Kind-specific request or response payload.
    #[serde(flatten)]
    pub body: ToolsBody,
}

impl ToolsEnvelope {
    /// Constructs a catalog request with the current protocol header.
    pub fn catalog_request(request_id: impl Into<String>) -> Self {
        Self::new(request_id, ToolsBody::CatalogRequest {})
    }

    /// Constructs a catalog response with the current protocol header.
    pub fn catalog_response(request_id: impl Into<String>, response: ToolsCatalogResponse) -> Self {
        Self::new(request_id, ToolsBody::CatalogResponse(response))
    }

    /// Constructs an invocation request with the current protocol header.
    pub fn invoke_request(request_id: impl Into<String>, request: ToolsInvokeRequest) -> Self {
        Self::new(request_id, ToolsBody::InvokeRequest(request))
    }

    /// Constructs an invocation response with the current protocol header.
    pub fn invoke_response(request_id: impl Into<String>, response: ToolsInvokeResponse) -> Self {
        Self::new(request_id, ToolsBody::InvokeResponse(response))
    }

    /// Constructs a service error response with the current protocol header.
    pub fn error(request_id: impl Into<String>, error: WireError) -> Self {
        Self::new(request_id, ToolsBody::Error { error })
    }

    fn new(request_id: impl Into<String>, body: ToolsBody) -> Self {
        Self {
            protocol: TOOLS_PROTOCOL.to_owned(),
            version: WIRE_VERSION,
            request_id: request_id.into(),
            body,
        }
    }

    /// Validates the protocol header and every kind-specific field.
    ///
    /// # Errors
    ///
    /// Returns a protocol, version, identifier, or payload-bound error.
    pub fn validate(&self) -> Result<(), ProtocolError> {
        validate_header(
            &self.protocol,
            TOOLS_PROTOCOL,
            self.version,
            &self.request_id,
        )?;
        match &self.body {
            ToolsBody::CatalogRequest {} => Ok(()),
            ToolsBody::CatalogResponse(response) => response.validate("tools"),
            ToolsBody::InvokeRequest(request) => request.validate(),
            ToolsBody::InvokeResponse(response) => response.validate(),
            ToolsBody::Error { error } => error.validate("error"),
        }
    }

    /// Encodes one validated, recursively canonical JSON DATA payload.
    ///
    /// # Errors
    ///
    /// Returns a validation, serialization, or frame-size error.
    pub fn encode(&self) -> Result<Vec<u8>, ProtocolError> {
        self.validate()?;
        encode_canonical(self)
    }

    /// Decodes and validates one untrusted tools DATA payload.
    ///
    /// # Errors
    ///
    /// Returns a JSON, lossy-number, validation, or frame-size error.
    pub fn decode(bytes: &[u8]) -> Result<Self, ProtocolError> {
        decode_canonical(bytes, Self::validate)
    }
}

/// Recursively sorts object keys while enforcing bounded JSON complexity.
///
/// # Errors
///
/// Returns [`ProtocolError::JsonLimit`] when nesting or node count exceeds the
/// version-zero contract, or [`ProtocolError::LossyJsonNumber`] when a number
/// exists only through an enabled extended-precision representation.
pub fn canonicalize_json(value: &Value) -> Result<Value, ProtocolError> {
    let mut nodes = 0;
    canonicalize_json_at(value, 0, &mut nodes)
}

fn validate_json(value: &Value) -> Result<(), ProtocolError> {
    let mut nodes = 0;
    validate_json_at(value, 0, &mut nodes)
}

/// Encodes a recursively key-sorted JSON value.
///
/// # Errors
///
/// Returns a JSON-complexity, lossy-number, or serialization error.
pub fn canonical_json_bytes(value: &Value) -> Result<Vec<u8>, ProtocolError> {
    serde_json::to_vec(&canonicalize_json(value)?).map_err(ProtocolError::Json)
}

/// Parses exactly one JSON text without losing duplicate-key information.
///
/// The returned value is recursively key-sorted. Fractions, exponents, and
/// integers outside the native `i64`/`u64` domain use the finite machine number
/// represented by [`serde_json::Value`]. Unlike `serde_json::from_str::<Value>`,
/// this parser rejects duplicate object keys at every depth while the original
/// member stream is still observable. It also enforces [`MAX_JSON_DEPTH`] and
/// [`MAX_JSON_NODES`]. The input string is only borrowed, so a caller can retain
/// the original text independently.
///
/// # Errors
///
/// Returns [`ProtocolError::DuplicateJsonKey`] for a repeated object key,
/// [`ProtocolError::JsonLimit`] for a depth or node limit,
/// [`ProtocolError::LossyJsonNumber`] for a syntactically valid number outside
/// the finite native representation, or [`ProtocolError::Json`] for malformed
/// JSON or trailing data.
pub fn parse_json_strict(text: &str) -> Result<Value, ProtocolError> {
    StrictJsonParser::new(text.as_bytes()).parse(NumberPolicy::Normalize)
}

/// Parses strict JSON for a downstream consumer that evaluates every number as `f64`.
///
/// Every accepted number is replaced by its finite `f64` representation. A
/// number whose decimal value would change during that conversion is rejected
/// instead of being silently rounded before schema validation or dispatch.
///
/// # Errors
///
/// Returns the same syntax, duplicate-key, and complexity failures as
/// [`parse_json_strict`], plus [`ProtocolError::LossyJsonNumber`] when a number
/// has no exact finite `f64` representation.
pub fn parse_json_strict_f64(text: &str) -> Result<Value, ProtocolError> {
    StrictJsonParser::new(text.as_bytes()).parse(NumberPolicy::RequireF64Exact)
}

fn parse_json_exact(bytes: &[u8]) -> Result<Value, ProtocolError> {
    StrictJsonParser::new(bytes).parse(NumberPolicy::RequireExact)
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum NumberPolicy {
    Normalize,
    RequireExact,
    RequireF64Exact,
}

// This parser deliberately owns the small amount of JSON grammar needed at the
// trust boundary. Deserializing straight into `Value` would erase duplicate
// members before they can be rejected, while deserializing every token through
// `RawValue` would duplicate a substantially larger, feature-sensitive grammar.
// The differential corpus in `tests/contracts.rs` keeps this implementation
// aligned with serde_json for ordinary JSON syntax.
struct StrictJsonParser<'input> {
    input: &'input [u8],
    cursor: usize,
    nodes: usize,
}

impl<'input> StrictJsonParser<'input> {
    fn new(input: &'input [u8]) -> Self {
        Self {
            input,
            cursor: 0,
            nodes: 0,
        }
    }

    fn parse(mut self, number_policy: NumberPolicy) -> Result<Value, ProtocolError> {
        self.skip_whitespace();
        let value = self.parse_value(0, number_policy)?;
        self.skip_whitespace();
        if self.cursor != self.input.len() {
            return Err(json_syntax("trailing data after the JSON value"));
        }
        Ok(value)
    }

    fn parse_value(
        &mut self,
        depth: usize,
        number_policy: NumberPolicy,
    ) -> Result<Value, ProtocolError> {
        self.record_node(depth)?;
        match self.peek() {
            Some(b'n') => {
                self.consume_literal(b"null")?;
                Ok(Value::Null)
            }
            Some(b't') => {
                self.consume_literal(b"true")?;
                Ok(Value::Bool(true))
            }
            Some(b'f') => {
                self.consume_literal(b"false")?;
                Ok(Value::Bool(false))
            }
            Some(b'"') => self.parse_string().map(Value::String),
            Some(b'[') => self.parse_array(depth, number_policy),
            Some(b'{') => self.parse_object(depth, number_policy),
            Some(b'-' | b'0'..=b'9') => self.parse_number(number_policy).map(Value::Number),
            Some(_) => Err(json_syntax("unexpected character in JSON value")),
            None => Err(json_syntax("expected a JSON value")),
        }
    }

    fn parse_array(
        &mut self,
        depth: usize,
        number_policy: NumberPolicy,
    ) -> Result<Value, ProtocolError> {
        self.cursor += 1;
        self.skip_whitespace();
        let mut values = Vec::new();
        if self.consume_if(b']') {
            return Ok(Value::Array(values));
        }
        loop {
            values.push(self.parse_value(depth + 1, number_policy)?);
            self.skip_whitespace();
            if self.consume_if(b']') {
                return Ok(Value::Array(values));
            }
            self.expect(b',', "expected `,` or `]` after array element")?;
            self.skip_whitespace();
        }
    }

    fn parse_object(
        &mut self,
        depth: usize,
        number_policy: NumberPolicy,
    ) -> Result<Value, ProtocolError> {
        self.cursor += 1;
        self.skip_whitespace();
        let mut members = BTreeMap::new();
        if self.consume_if(b'}') {
            return Ok(Value::Object(Map::new()));
        }
        loop {
            if self.peek() != Some(b'"') {
                return Err(json_syntax("expected a JSON object key"));
            }
            let key = self.parse_string()?;
            if members.contains_key(&key) {
                return Err(ProtocolError::DuplicateJsonKey { key });
            }
            self.skip_whitespace();
            self.expect(b':', "expected `:` after JSON object key")?;
            self.skip_whitespace();
            let value = self.parse_value(depth + 1, number_policy)?;
            members.insert(key, value);
            self.skip_whitespace();
            if self.consume_if(b'}') {
                let object = members.into_iter().collect();
                return Ok(Value::Object(object));
            }
            self.expect(b',', "expected `,` or `}` after object member")?;
            self.skip_whitespace();
        }
    }

    fn parse_string(&mut self) -> Result<String, ProtocolError> {
        let start = self.cursor;
        self.cursor += 1;
        while let Some(byte) = self.peek() {
            match byte {
                b'"' => {
                    self.cursor += 1;
                    return serde_json::from_slice(&self.input[start..self.cursor])
                        .map_err(ProtocolError::Json);
                }
                b'\\' => {
                    self.cursor = self.cursor.saturating_add(2);
                    if self.cursor > self.input.len() {
                        return Err(json_syntax("unterminated JSON string escape"));
                    }
                }
                _ => self.cursor += 1,
            }
        }
        Err(json_syntax("unterminated JSON string"))
    }

    fn parse_number(&mut self, number_policy: NumberPolicy) -> Result<Number, ProtocolError> {
        let start = self.cursor;
        self.scan_number()?;
        let raw = std::str::from_utf8(&self.input[start..self.cursor])
            .expect("a JSON number token contains only ASCII");
        let represented = match number_policy {
            NumberPolicy::RequireF64Exact => raw.parse::<f64>().ok().and_then(Number::from_f64),
            NumberPolicy::Normalize | NumberPolicy::RequireExact => machine_number(raw),
        };
        let Some(represented) = represented else {
            return match number_policy {
                NumberPolicy::Normalize => Err(ProtocolError::LossyJsonNumber),
                NumberPolicy::RequireExact | NumberPolicy::RequireF64Exact => {
                    Err(ProtocolError::LossyJsonNumber)
                }
            };
        };
        if matches!(
            number_policy,
            NumberPolicy::RequireExact | NumberPolicy::RequireF64Exact
        ) && !decimal_values_equal(raw, &represented.to_string())
        {
            return Err(ProtocolError::LossyJsonNumber);
        }
        Ok(represented)
    }

    fn scan_number(&mut self) -> Result<(), ProtocolError> {
        self.consume_if(b'-');
        match self.peek() {
            Some(b'0') => {
                self.cursor += 1;
                if self.peek().is_some_and(|byte| byte.is_ascii_digit()) {
                    return Err(json_syntax("leading zero in JSON number"));
                }
            }
            Some(b'1'..=b'9') => {
                self.cursor += 1;
                self.consume_digits();
            }
            _ => return Err(json_syntax("expected digit in JSON number")),
        }
        if self.consume_if(b'.') {
            self.require_digit("expected digit after decimal point")?;
            self.consume_digits();
        }
        if self.peek().is_some_and(|byte| matches!(byte, b'e' | b'E')) {
            self.cursor += 1;
            if self.peek().is_some_and(|byte| matches!(byte, b'+' | b'-')) {
                self.cursor += 1;
            }
            self.require_digit("expected digit in number exponent")?;
            self.consume_digits();
        }
        Ok(())
    }

    fn require_digit(&mut self, reason: &'static str) -> Result<(), ProtocolError> {
        if self.peek().is_none_or(|byte| !byte.is_ascii_digit()) {
            return Err(json_syntax(reason));
        }
        self.cursor += 1;
        Ok(())
    }

    fn consume_digits(&mut self) {
        while self.peek().is_some_and(|byte| byte.is_ascii_digit()) {
            self.cursor += 1;
        }
    }

    fn consume_literal(&mut self, literal: &[u8]) -> Result<(), ProtocolError> {
        if self.input[self.cursor..].starts_with(literal) {
            self.cursor += literal.len();
            Ok(())
        } else {
            Err(json_syntax("invalid JSON literal"))
        }
    }

    fn record_node(&mut self, depth: usize) -> Result<(), ProtocolError> {
        if depth > MAX_JSON_DEPTH {
            return Err(ProtocolError::JsonLimit {
                reason: format!("nesting exceeds {MAX_JSON_DEPTH}"),
            });
        }
        self.nodes = self.nodes.saturating_add(1);
        if self.nodes > MAX_JSON_NODES {
            return Err(ProtocolError::JsonLimit {
                reason: format!("node count exceeds {MAX_JSON_NODES}"),
            });
        }
        Ok(())
    }

    fn expect(&mut self, byte: u8, reason: &'static str) -> Result<(), ProtocolError> {
        if self.consume_if(byte) {
            Ok(())
        } else {
            Err(json_syntax(reason))
        }
    }

    fn consume_if(&mut self, byte: u8) -> bool {
        if self.peek() == Some(byte) {
            self.cursor += 1;
            true
        } else {
            false
        }
    }

    fn skip_whitespace(&mut self) {
        while self
            .peek()
            .is_some_and(|byte| matches!(byte, b' ' | b'\n' | b'\r' | b'\t'))
        {
            self.cursor += 1;
        }
    }

    fn peek(&self) -> Option<u8> {
        self.input.get(self.cursor).copied()
    }
}

fn machine_number(raw: &str) -> Option<Number> {
    if !raw.bytes().any(|byte| matches!(byte, b'.' | b'e' | b'E')) {
        if raw.starts_with('-') {
            if let Ok(value) = raw.parse::<i64>() {
                return Some(value.into());
            }
        } else if let Ok(value) = raw.parse::<u64>() {
            return Some(value.into());
        }
    }
    raw.parse::<f64>()
        .ok()
        .and_then(Number::from_f64)
        .or_else(|| {
            if json_number_is_exact_zero(raw) {
                Number::from_f64(0.0)
            } else {
                None
            }
        })
}

fn json_syntax(reason: &'static str) -> ProtocolError {
    ProtocolError::Json(serde_json::Error::io(io::Error::new(
        io::ErrorKind::InvalidData,
        reason,
    )))
}

fn validate_json_at(value: &Value, depth: usize, nodes: &mut usize) -> Result<(), ProtocolError> {
    record_json_node(depth, nodes)?;
    match value {
        Value::Object(object) => {
            for value in object.values() {
                validate_json_at(value, depth + 1, nodes)?;
            }
        }
        Value::Array(array) => {
            for value in array {
                validate_json_at(value, depth + 1, nodes)?;
            }
        }
        Value::Number(number) => validate_json_number(number)?,
        Value::Null | Value::Bool(_) | Value::String(_) => {}
    }
    Ok(())
}

fn canonicalize_json_at(
    value: &Value,
    depth: usize,
    nodes: &mut usize,
) -> Result<Value, ProtocolError> {
    record_json_node(depth, nodes)?;
    match value {
        Value::Object(object) => {
            let mut keys: Vec<_> = object.keys().collect();
            keys.sort_unstable();
            let mut canonical = Map::new();
            for key in keys {
                canonical.insert(
                    key.clone(),
                    canonicalize_json_at(&object[key], depth + 1, nodes)?,
                );
            }
            Ok(Value::Object(canonical))
        }
        Value::Array(array) => array
            .iter()
            .map(|item| canonicalize_json_at(item, depth + 1, nodes))
            .collect::<Result<Vec<_>, _>>()
            .map(Value::Array),
        Value::Number(number) => {
            let raw = number.to_string();
            let represented = machine_number(&raw).ok_or(ProtocolError::LossyJsonNumber)?;
            if !decimal_values_equal(&raw, &represented.to_string()) {
                return Err(ProtocolError::LossyJsonNumber);
            }
            Ok(Value::Number(represented))
        }
        scalar => Ok(scalar.clone()),
    }
}

fn record_json_node(depth: usize, nodes: &mut usize) -> Result<(), ProtocolError> {
    if depth > MAX_JSON_DEPTH {
        return Err(ProtocolError::JsonLimit {
            reason: format!("nesting exceeds {MAX_JSON_DEPTH}"),
        });
    }
    *nodes = nodes.saturating_add(1);
    if *nodes > MAX_JSON_NODES {
        return Err(ProtocolError::JsonLimit {
            reason: format!("node count exceeds {MAX_JSON_NODES}"),
        });
    }
    Ok(())
}

fn encode_canonical<T: Serialize>(value: &T) -> Result<Vec<u8>, ProtocolError> {
    let serialized = serde_json::to_vec(value).map_err(ProtocolError::Json)?;
    require_frame_size(serialized.len())?;
    let canonical = parse_json_exact(&serialized)?;
    let bytes = serde_json::to_vec(&canonical).map_err(ProtocolError::Json)?;
    require_frame_size(bytes.len())?;
    Ok(bytes)
}

#[derive(Default)]
struct ByteCounter {
    bytes: usize,
}

impl io::Write for ByteCounter {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        self.bytes = self
            .bytes
            .checked_add(buffer.len())
            .ok_or_else(|| io::Error::other("serialized JSON length overflow"))?;
        Ok(buffer.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// Counts the exact compact JSON bytes emitted for a serializable value without
/// allocating an intermediate buffer.
///
/// # Errors
///
/// Returns [`ProtocolError::Json`] when serialization fails.
pub fn encoded_json_len(value: &impl Serialize) -> Result<usize, ProtocolError> {
    let mut counter = ByteCounter::default();
    serde_json::to_writer(&mut counter, value).map_err(ProtocolError::Json)?;
    Ok(counter.bytes)
}

fn decode_canonical<T>(
    bytes: &[u8],
    validate: impl FnOnce(&T) -> Result<(), ProtocolError>,
) -> Result<T, ProtocolError>
where
    T: StableEnvelope,
{
    require_frame_size(bytes.len())?;
    let mut canonical = parse_json_exact(bytes)?;
    let stable_values = T::take_stable_values(&mut canonical);
    let canonical = serde_json::to_vec(&canonical).map_err(ProtocolError::Json)?;
    let mut envelope: T = serde_json::from_slice(&canonical).map_err(ProtocolError::Json)?;
    envelope.restore_stable_values(stable_values)?;
    validate(&envelope)?;
    Ok(envelope)
}

// `serde_json` uses private marker objects to transport arbitrary-precision
// numbers and raw values through its data model. Those markers are an internal
// implementation detail and can collide with valid provider-owned object keys
// when another workspace crate enables the feature. Lift only the protocol's
// arbitrary JSON subtrees out before typed deserialization and restore them
// afterward; closed envelope fields still go through the derived DTO parser.
trait StableEnvelope: for<'de> Deserialize<'de> {
    fn take_stable_values(value: &mut Value) -> Vec<Value>;
    fn restore_stable_values(&mut self, values: Vec<Value>) -> Result<(), ProtocolError>;
}

impl StableEnvelope for ToolsEnvelope {
    fn take_stable_values(value: &mut Value) -> Vec<Value> {
        let mut values = Vec::new();
        let Some(envelope) = value.as_object_mut() else {
            return values;
        };
        match envelope.get("kind").and_then(Value::as_str) {
            Some("catalog_response") => {
                if let Some(tools) = envelope.get_mut("tools") {
                    take_catalog_values(tools, &mut values);
                }
            }
            Some("invoke_response") => {
                if let Some(result) = envelope.get_mut("result") {
                    take_tool_result_value(result, &mut values);
                }
            }
            _ => {}
        }
        values
    }

    fn restore_stable_values(&mut self, values: Vec<Value>) -> Result<(), ProtocolError> {
        let mut values = values.into_iter();
        match &mut self.body {
            ToolsBody::CatalogResponse(response) => {
                restore_catalog_values(&mut response.tools, &mut values)?;
            }
            ToolsBody::InvokeResponse(response) => {
                restore_tool_result_value(&mut response.result, &mut values)?;
            }
            _ => {}
        }
        require_no_stable_values(values)
    }
}

fn take_catalog_values(catalog: &mut Value, values: &mut Vec<Value>) {
    let Some(tools) = catalog.as_array_mut() else {
        return;
    };
    for tool in tools {
        let Some(tool) = tool.as_object_mut() else {
            continue;
        };
        if let Some(schema) = tool.get_mut("input_schema") {
            values.push(std::mem::replace(schema, Value::Bool(true)));
        }
    }
}

fn take_tool_result_value(result: &mut Value, values: &mut Vec<Value>) {
    let Some(result) = result.as_object_mut() else {
        return;
    };
    if result.get("status").and_then(Value::as_str) == Some("ok")
        && let Some(value) = result.get_mut("value")
    {
        values.push(std::mem::replace(value, Value::Null));
    }
}

fn restore_catalog_values(
    tools: &mut [ToolDefinition],
    values: &mut impl Iterator<Item = Value>,
) -> Result<(), ProtocolError> {
    for tool in tools {
        tool.input_schema = next_stable_value(values)?;
    }
    Ok(())
}

fn restore_tool_result_value(
    result: &mut ToolResult,
    values: &mut impl Iterator<Item = Value>,
) -> Result<(), ProtocolError> {
    if let ToolResult::Ok { value } = result {
        *value = next_stable_value(values)?;
    }
    Ok(())
}

fn next_stable_value(values: &mut impl Iterator<Item = Value>) -> Result<Value, ProtocolError> {
    values
        .next()
        .ok_or_else(|| json_syntax("missing preserved JSON value during envelope decoding"))
}

fn require_no_stable_values(mut values: impl Iterator<Item = Value>) -> Result<(), ProtocolError> {
    if values.next().is_some() {
        return Err(json_syntax(
            "unexpected preserved JSON value during envelope decoding",
        ));
    }
    Ok(())
}

fn validate_json_number(number: &Number) -> Result<(), ProtocolError> {
    if number.is_i64() || number.is_u64() {
        return Ok(());
    }
    let raw = number.to_string();
    let represented = machine_number(&raw).ok_or(ProtocolError::LossyJsonNumber)?;
    if !decimal_values_equal(&raw, &represented.to_string()) {
        return Err(ProtocolError::LossyJsonNumber);
    }
    Ok(())
}

#[derive(Debug, Eq, PartialEq)]
struct CanonicalDecimal {
    negative: bool,
    digits: String,
    exponent: i128,
}

fn decimal_values_equal(left: &str, right: &str) -> bool {
    canonical_decimal(left).is_some_and(|left| canonical_decimal(right) == Some(left))
}

fn canonical_decimal(text: &str) -> Option<CanonicalDecimal> {
    let (negative, unsigned) = text
        .strip_prefix('-')
        .map_or((false, text), |unsigned| (true, unsigned));
    let exponent_start = unsigned.find(['e', 'E']);
    let (mantissa, explicit_exponent) = exponent_start.map_or((unsigned, "0"), |index| {
        (&unsigned[..index], &unsigned[index + 1..])
    });

    let fraction_digits = mantissa
        .split_once('.')
        .map_or(0, |(_, fraction)| fraction.len());
    let digits = mantissa
        .bytes()
        .filter(u8::is_ascii_digit)
        .map(char::from)
        .collect::<String>();
    let without_leading = digits.trim_start_matches('0');
    if without_leading.is_empty() {
        return Some(CanonicalDecimal {
            negative: false,
            digits: "0".to_owned(),
            exponent: 0,
        });
    }
    let coefficient = without_leading.trim_end_matches('0');
    let trailing_zeros = without_leading.len().checked_sub(coefficient.len())?;
    let exponent = parse_decimal_exponent(explicit_exponent)?
        .checked_sub(i128::try_from(fraction_digits).ok()?)?
        .checked_add(i128::try_from(trailing_zeros).ok()?)?;
    Some(CanonicalDecimal {
        negative,
        digits: coefficient.to_owned(),
        exponent,
    })
}

fn parse_decimal_exponent(text: &str) -> Option<i128> {
    let (negative, magnitude) = text.strip_prefix('-').map_or_else(
        || (false, text.strip_prefix('+').unwrap_or(text)),
        |magnitude| (true, magnitude),
    );
    let magnitude = magnitude.trim_start_matches('0');
    if magnitude.is_empty() {
        return Some(0);
    }
    let magnitude = magnitude
        .parse::<i128>()
        .ok()?
        .checked_mul(if negative { -1 } else { 1 })?;
    Some(magnitude)
}

fn parse_wire_version(text: &str) -> Result<u32, ProtocolError> {
    text.parse::<u32>()
        .map_err(|_| json_syntax("wire version must be an unsigned JSON integer"))
}

fn json_number_is_exact_zero(text: &str) -> bool {
    let text = text.trim();
    if !text
        .as_bytes()
        .first()
        .is_some_and(|byte| matches!(byte, b'-' | b'0'..=b'9'))
    {
        return false;
    }
    text.trim_start_matches('-')
        .split(['e', 'E'])
        .next()
        .is_some_and(|significand| significand.bytes().all(|byte| matches!(byte, b'0' | b'.')))
}

fn validate_header(
    protocol: &str,
    expected: &'static str,
    version: u32,
    request_id: &str,
) -> Result<(), ProtocolError> {
    if protocol != expected {
        return Err(ProtocolError::UnsupportedProtocol {
            expected,
            found: protocol.to_owned(),
        });
    }
    if version != WIRE_VERSION {
        return Err(ProtocolError::UnsupportedVersion { found: version });
    }
    require_identifier("request_id", request_id)
}

fn deserialize_wire_version<'de, D>(deserializer: D) -> Result<u32, D::Error>
where
    D: Deserializer<'de>,
{
    let raw = Box::<RawValue>::deserialize(deserializer)?;
    parse_wire_version(raw.get()).map_err(de::Error::custom)
}

fn validate_catalog(tools: &[ToolDefinition], field: &str) -> Result<(), ProtocolError> {
    if tools.len() > MAX_TOOLS {
        return invalid(field, format!("contains more than {MAX_TOOLS} tools"));
    }
    let mut names = BTreeSet::new();
    for (index, tool) in tools.iter().enumerate() {
        tool.validate(&format!("{field}[{index}]"))?;
        if !names.insert(tool.name.as_str()) {
            return invalid(field, "contains duplicate tool names");
        }
    }
    // Canonical key ordering changes only member order, never compact JSON
    // length, so a counting writer enforces the aggregate bound without
    // allocating and reparsing a temporary catalog encoding.
    let encoded_bytes = encoded_json_len(&tools)?;
    if encoded_bytes > MAX_CATALOG_BYTES {
        return invalid(
            field,
            format!("canonical encoding exceeds {MAX_CATALOG_BYTES} bytes"),
        );
    }
    Ok(())
}

fn require_frame_size(actual: usize) -> Result<(), ProtocolError> {
    if actual > MAX_DATA_BYTES {
        return Err(ProtocolError::PayloadTooLarge {
            actual,
            maximum: MAX_DATA_BYTES,
        });
    }
    Ok(())
}

fn require_identifier(field: &str, value: &str) -> Result<(), ProtocolError> {
    if !is_wire_identifier(value) {
        return invalid(
            field,
            format!("must be 1..={MAX_ID_BYTES} non-whitespace printable ASCII bytes"),
        );
    }
    Ok(())
}

fn require_tool_name(field: &str, value: &str) -> Result<(), ProtocolError> {
    if value.is_empty()
        || value.len() > MAX_ID_BYTES
        || !value.bytes().enumerate().all(|(index, byte)| {
            byte.is_ascii_alphanumeric() || (index > 0 && matches!(byte, b'.' | b'_' | b'-'))
        })
    {
        return invalid(
            field,
            "must start with an ASCII alphanumeric and contain only ASCII alphanumerics, '.', '_', or '-'",
        );
    }
    Ok(())
}

fn require_code(field: &str, value: &str) -> Result<(), ProtocolError> {
    if value.is_empty()
        || value.len() > MAX_ERROR_CODE_BYTES
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
    {
        return invalid(field, "is outside the bounded error-code syntax");
    }
    Ok(())
}

fn require_text(
    field: &str,
    value: &str,
    maximum: usize,
    allow_empty: bool,
) -> Result<(), ProtocolError> {
    let mut characters = 0_usize;
    let mut contains_forbidden = false;
    for character in value.chars() {
        characters = characters.saturating_add(1);
        contains_forbidden |= character == '\0' || character == '\u{007f}';
    }
    if !allow_empty && characters == 0 {
        return invalid(field, "must not be empty");
    }
    if characters > maximum {
        return invalid(
            field,
            format!("must contain at most {maximum} Unicode scalar values"),
        );
    }
    if contains_forbidden {
        return invalid(field, "must not contain NUL or DEL");
    }
    Ok(())
}

fn invalid<T>(field: impl Into<String>, reason: impl Into<String>) -> Result<T, ProtocolError> {
    Err(ProtocolError::InvalidField {
        field: field.into(),
        reason: reason.into(),
    })
}

/// Rejection returned at the semantic protocol boundary.
#[derive(Debug, Error)]
pub enum ProtocolError {
    #[error("invalid protocol JSON: {0}")]
    Json(#[source] serde_json::Error),
    #[error("unsupported protocol `{found}`; expected `{expected}`")]
    UnsupportedProtocol {
        expected: &'static str,
        found: String,
    },
    #[error("unsupported protocol version {found}")]
    UnsupportedVersion { found: u32 },
    #[error("field `{field}` {reason}")]
    InvalidField { field: String, reason: String },
    #[error("JSON object contains duplicate key `{key}`")]
    DuplicateJsonKey { key: String },
    #[error("JSON value exceeds the version-zero complexity bound: {reason}")]
    JsonLimit { reason: String },
    #[error("JSON number cannot be represented without changing its decimal value")]
    LossyJsonNumber,
    #[error("semantic DATA payload is {actual} bytes; maximum is {maximum}")]
    PayloadTooLarge { actual: usize, maximum: usize },
}
