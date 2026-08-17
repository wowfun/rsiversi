//! Package-owned configuration validation and in-memory secret preparation.

use std::collections::BTreeSet;
use std::fmt;
use std::fs;
use std::io::Read;
use std::path::Path;

use serde_json::{Map, Value, json};
use thiserror::Error;

use super::{
    ContentHash, LoaderError, MAX_CONFIG_SCHEMA_BYTES, PluginPackage, read_file,
    validate_relative_path,
};

const DRAFT_2020_12: &str = "https://json-schema.org/draft/2020-12/schema";
const MAX_SECRET_FILE_BYTES: usize = 1024 * 1024;
const MAX_KEYRING_IDENTIFIER_BYTES: usize = 255;

mod schema;
pub use schema::PreparedConfigSchema;
use schema::{compile_validator, validate_compiled};

/// A validated plugin configuration whose secrets exist only in memory.
pub struct PreparedConfig {
    resolved: Value,
    redacted: Value,
    audit_hash: ContentHash,
}

impl PreparedConfig {
    /// Configuration passed to the trusted plugin during prepare.
    pub const fn resolved(&self) -> &Value {
        &self.resolved
    }

    /// Consumes this value and returns the resolved in-memory configuration.
    pub fn into_resolved(self) -> Value {
        self.resolved
    }

    /// Configuration safe to include in diagnostics and durable audit records.
    pub const fn redacted(&self) -> &Value {
        &self.redacted
    }

    /// SHA-256 of canonical unresolved configuration, including reference
    /// identity but never resolved secret bytes.
    pub const fn audit_hash(&self) -> ContentHash {
        self.audit_hash
    }
}

impl fmt::Debug for PreparedConfig {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PreparedConfig")
            .field("redacted", &self.redacted)
            .field("audit_hash", &self.audit_hash)
            .finish_non_exhaustive()
    }
}

/// Configuration preparation failed. Variants deliberately omit instance and
/// secret-source values so both `Display` and `Debug` are safe to log.
#[derive(Clone, Debug, Error, Eq, PartialEq)]
pub enum ConfigPrepareError {
    #[error("plugin package does not declare a configuration schema")]
    MissingSchema,
    #[error("plugin configuration schema path is unsafe")]
    UnsafeSchemaPath,
    #[error("cannot read plugin configuration schema")]
    SchemaRead,
    #[error("plugin configuration schema is not valid Draft 2020-12 JSON Schema")]
    InvalidSchema,
    #[error("plugin instance configuration is invalid at `{instance_path}`")]
    InvalidConfig { instance_path: String },
    #[error("plugin secret reference is invalid at `{instance_path}`")]
    InvalidSecretReference { instance_path: String },
    #[error("plugin secret source is unavailable at `{instance_path}`")]
    SecretUnavailable { instance_path: String },
    #[error("plugin secret file is unsafe at `{instance_path}`")]
    UnsafeSecretFile { instance_path: String },
    #[error("cannot encode canonical plugin configuration")]
    CanonicalEncoding,
}

/// Validates one instance configuration against its package schema, resolves
/// annotated secret references during prepare, and creates safe audit views.
///
/// A schema location marked with `"x-rsi-meta-secret": true` accepts exactly
/// one of these unresolved JSON shapes:
///
/// - `{"$secret":{"env":"NAME"}}`
/// - `{"$secret":{"file":"/absolute/private/path"}}`
/// - `{"$secret":{"keyring":{"service":"NAME","user":"NAME"}}}`
///
/// Plaintext at a marked location and `$secret` objects at unmarked locations
/// are rejected. External JSON Schema retrieval is always disabled. Packages
/// without `config_schema` accept only an empty object.
///
/// # Errors
///
/// Returns a redacted configuration, schema, secret-source, or input-file error.
pub fn prepare_config(
    package: &PluginPackage,
    instance_config: Value,
) -> Result<PreparedConfig, ConfigPrepareError> {
    let Some(schema_relative) = package.manifest().config_schema.as_deref() else {
        return prepare_config_with_schema(package, None, instance_config);
    };
    validate_relative_path("config_schema", schema_relative)
        .map_err(|_| ConfigPrepareError::UnsafeSchemaPath)?;
    let package_directory = package
        .manifest_path()
        .parent()
        .unwrap_or_else(|| Path::new("."));
    let schema_bytes = read_package_schema(package_directory, schema_relative)?;
    prepare_config_with_schema(package, Some(&schema_bytes), instance_config)
}

/// Validates configuration against schema bytes already captured by the
/// workspace host while checking its durable lock.
///
/// This avoids reopening a mutable package path between integrity validation
/// and configuration validation. Callers must pass the bytes whose digest they
/// checked for a package that declares `config_schema`.
#[doc(hidden)]
pub fn prepare_config_with_schema(
    package: &PluginPackage,
    schema_bytes: Option<&[u8]>,
    instance_config: Value,
) -> Result<PreparedConfig, ConfigPrepareError> {
    let schema = compile_config_schema(package, schema_bytes)?;
    prepare_config_with_compiled_schema(&schema, instance_config)
}

/// Parses and compiles the package schema once. External retrieval remains
/// disabled in both validators.
///
/// # Errors
///
/// Returns a schema contract or compilation error.
#[doc(hidden)]
pub fn compile_config_schema(
    package: &PluginPackage,
    schema_bytes: Option<&[u8]>,
) -> Result<PreparedConfigSchema, ConfigPrepareError> {
    let Some(schema_relative) = package.manifest().config_schema.as_deref() else {
        if schema_bytes.is_some() {
            return Err(ConfigPrepareError::MissingSchema);
        }
        return Ok(PreparedConfigSchema {
            schema: None,
            unresolved: None,
            resolved: None,
        });
    };
    validate_relative_path("config_schema", schema_relative)
        .map_err(|_| ConfigPrepareError::UnsafeSchemaPath)?;
    let schema_bytes = schema_bytes.ok_or(ConfigPrepareError::SchemaRead)?;
    let schema: Value =
        serde_json::from_slice(schema_bytes).map_err(|_| ConfigPrepareError::InvalidSchema)?;
    if schema.get("$schema").and_then(Value::as_str) != Some(DRAFT_2020_12)
        || jsonschema::draft202012::meta::validate(&schema).is_err()
    {
        return Err(ConfigPrepareError::InvalidSchema);
    }

    let reference_schema = reference_validation_schema(&schema);
    let unresolved = compile_validator(&reference_schema)?;
    let resolved = compile_validator(&schema)?;
    Ok(PreparedConfigSchema {
        schema: Some(schema),
        unresolved: Some(unresolved),
        resolved: Some(resolved),
    })
}

/// Validates and resolves one instance with a package-level compiled schema.
///
/// # Errors
///
/// Returns an invalid configuration, secret-source, or encoding error.
#[doc(hidden)]
pub fn prepare_config_with_compiled_schema(
    schema: &PreparedConfigSchema,
    instance_config: Value,
) -> Result<PreparedConfig, ConfigPrepareError> {
    let (Some(unresolved), Some(resolved_validator)) = (&schema.unresolved, &schema.resolved)
    else {
        if !instance_config
            .as_object()
            .is_some_and(serde_json::Map::is_empty)
        {
            return Err(ConfigPrepareError::MissingSchema);
        }
        let audit_bytes = serde_json::to_vec(&canonicalize(&instance_config))
            .map_err(|_| ConfigPrepareError::CanonicalEncoding)?;
        return Ok(PreparedConfig {
            resolved: instance_config.clone(),
            redacted: instance_config,
            audit_hash: ContentHash::digest(audit_bytes),
        });
    };

    validate_compiled(unresolved, &instance_config)?;

    let audit_bytes = serde_json::to_vec(&canonicalize(&instance_config))
        .map_err(|_| ConfigPrepareError::CanonicalEncoding)?;
    let audit_hash = ContentHash::digest(audit_bytes);
    let (resolved, redacted) = resolve_value(instance_config, "")?;
    validate_compiled(resolved_validator, &resolved)?;

    Ok(PreparedConfig {
        resolved,
        redacted,
        audit_hash,
    })
}

fn read_package_schema(
    package_directory: &Path,
    schema_relative: &Path,
) -> Result<Vec<u8>, ConfigPrepareError> {
    let requested_path = package_directory.join(schema_relative);
    let requested_metadata =
        fs::symlink_metadata(&requested_path).map_err(|_| ConfigPrepareError::SchemaRead)?;
    if requested_metadata.file_type().is_symlink() || !requested_metadata.file_type().is_file() {
        return Err(ConfigPrepareError::UnsafeSchemaPath);
    }
    let package_directory =
        fs::canonicalize(package_directory).map_err(|_| ConfigPrepareError::SchemaRead)?;
    let schema_path =
        fs::canonicalize(&requested_path).map_err(|_| ConfigPrepareError::SchemaRead)?;
    if !schema_path.starts_with(&package_directory) {
        return Err(ConfigPrepareError::UnsafeSchemaPath);
    }
    read_file(
        &schema_path,
        "read plugin configuration schema",
        MAX_CONFIG_SCHEMA_BYTES,
    )
    .map_err(|error| match error {
        LoaderError::UnsafeInputFile { .. } => ConfigPrepareError::UnsafeSchemaPath,
        _ => ConfigPrepareError::SchemaRead,
    })
}

fn reference_validation_schema(schema: &Value) -> Value {
    let mut next_internal_id = 0_u32;
    let mut transformed =
        transform_reference_validation_schema(schema, &mut next_internal_id, true);
    seal_local_reference_targets(schema, &mut transformed, &mut next_internal_id);
    transformed
}

fn seal_local_reference_targets(
    source: &Value,
    transformed: &mut Value,
    next_internal_id: &mut u32,
) {
    let mut pending = BTreeSet::new();
    collect_local_reference_targets(source, &mut pending);
    let mut targets = BTreeSet::new();
    while let Some(pointer) = pending.pop_first() {
        if !targets.insert(pointer.clone()) {
            continue;
        }
        if let Some(target) = source.pointer(&pointer) {
            collect_local_reference_targets(target, &mut pending);
        }
    }

    let mut targets: Vec<_> = targets.into_iter().collect();
    if targets.iter().all(String::is_empty) {
        return;
    }
    let Some(secret_free_id) = install_reference_target_guard(transformed, next_internal_id) else {
        return;
    };
    targets.sort_by_key(|pointer| pointer.matches('/').count());
    for pointer in targets {
        if pointer.is_empty() {
            continue;
        }
        let (Some(source_target), Some(target)) =
            (source.pointer(&pointer), transformed.pointer_mut(&pointer))
        else {
            continue;
        };
        let first_generated_id = *next_internal_id;
        let mut sealed =
            transform_reference_validation_schema(source_target, next_internal_id, true);
        replace_generated_secret_free_schemas(
            &mut sealed,
            first_generated_id,
            *next_internal_id,
            &secret_free_id,
        );
        *target = sealed;
    }
}

fn install_reference_target_guard(
    transformed: &mut Value,
    next_internal_id: &mut u32,
) -> Option<String> {
    let root = transformed.as_object_mut()?;
    let definitions = root
        .entry("$defs")
        .or_insert_with(|| Value::Object(Map::new()))
        .as_object_mut()?;
    let mut name = "__rsi_meta_secret_free_reference_target".to_owned();
    while definitions.contains_key(&name) {
        name.push('_');
    }
    let id = format!("urn:rsi-meta:internal:secret-free:{}", *next_internal_id);
    *next_internal_id += 1;
    definitions.insert(
        name,
        json!({
            "$id": id,
            "not": {
                "type": "object",
                "required": ["$secret"]
            },
            "additionalProperties": {"$ref": id},
            "items": {"$ref": id}
        }),
    );
    Some(id)
}

fn replace_generated_secret_free_schemas(
    schema: &mut Value,
    first_id: u32,
    next_id: u32,
    replacement_id: &str,
) {
    if let Value::Object(object) = schema
        && object
            .get("$id")
            .and_then(Value::as_str)
            .and_then(|id| id.rsplit(':').next())
            .and_then(|id| id.parse::<u32>().ok())
            .is_some_and(|id| (first_id..next_id).contains(&id))
    {
        *schema = json!({"$ref": replacement_id});
        return;
    }
    match schema {
        Value::Object(object) => {
            for child in object.values_mut() {
                replace_generated_secret_free_schemas(child, first_id, next_id, replacement_id);
            }
        }
        Value::Array(array) => {
            for child in array {
                replace_generated_secret_free_schemas(child, first_id, next_id, replacement_id);
            }
        }
        _ => {}
    }
}

fn collect_local_reference_targets(schema: &Value, targets: &mut BTreeSet<String>) {
    let Value::Object(object) = schema else {
        return;
    };
    for keyword in ["$ref", "$dynamicRef"] {
        if let Some(reference) = object.get(keyword).and_then(Value::as_str)
            && let Some(pointer) = local_reference_pointer(reference)
        {
            targets.insert(pointer);
        }
    }
    for keyword in ["$defs", "definitions", "properties", "patternProperties"] {
        if let Some(Value::Object(children)) = object.get(keyword) {
            for child in children.values() {
                collect_local_reference_targets(child, targets);
            }
        }
    }
    if let Some(Value::Object(children)) = object.get("dependentSchemas") {
        for child in children.values() {
            collect_local_reference_targets(child, targets);
        }
    }
    for keyword in [
        "additionalProperties",
        "unevaluatedProperties",
        "propertyNames",
        "items",
        "unevaluatedItems",
        "contains",
        "contentSchema",
        "not",
        "if",
        "then",
        "else",
    ] {
        if let Some(child) = object.get(keyword) {
            collect_local_reference_targets(child, targets);
        }
    }
    if let Some(Value::Array(children)) = object.get("prefixItems") {
        for child in children {
            collect_local_reference_targets(child, targets);
        }
    }
    for keyword in ["allOf", "anyOf", "oneOf"] {
        if let Some(Value::Array(children)) = object.get(keyword) {
            for child in children {
                collect_local_reference_targets(child, targets);
            }
        }
    }
}

fn local_reference_pointer(reference: &str) -> Option<String> {
    let fragment = reference.strip_prefix('#')?;
    if fragment.is_empty() {
        return Some(String::new());
    }
    if !fragment.starts_with('/') {
        return None;
    }
    percent_decode(fragment)
}

fn percent_decode(value: &str) -> Option<String> {
    let bytes = value.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' {
            let high = hex_value(*bytes.get(index + 1)?)?;
            let low = hex_value(*bytes.get(index + 2)?)?;
            decoded.push((high << 4) | low);
            index += 3;
        } else {
            decoded.push(bytes[index]);
            index += 1;
        }
    }
    String::from_utf8(decoded).ok()
}

const fn hex_value(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

fn transform_reference_validation_schema(
    schema: &Value,
    next_internal_id: &mut u32,
    seal_open_content: bool,
) -> Value {
    transform_reference_validation_schema_with_shared_properties(
        schema,
        next_internal_id,
        seal_open_content,
        &BTreeSet::new(),
    )
}

fn transform_reference_validation_schema_with_shared_properties(
    schema: &Value,
    next_internal_id: &mut u32,
    seal_open_content: bool,
    shared_secret_properties: &BTreeSet<String>,
) -> Value {
    match schema {
        Value::Bool(true) => {
            disallow_unannotated_reference(Map::new(), next_internal_id, seal_open_content)
        }
        Value::Bool(false) => Value::Bool(false),
        Value::Object(object)
            if object.get("x-rsi-meta-secret").and_then(Value::as_bool) == Some(true) =>
        {
            secret_reference_schema()
        }
        Value::Object(object) => {
            let delegates_to_reference =
                object.contains_key("$ref") || object.contains_key("$dynamicRef");
            let mut transformed = object.clone();
            for keyword in ["$defs", "definitions", "patternProperties"] {
                if let Some(Value::Object(children)) = transformed.get_mut(keyword) {
                    for child in children.values_mut() {
                        *child =
                            transform_reference_validation_schema(child, next_internal_id, true);
                    }
                }
            }
            if let Some(Value::Object(children)) = transformed.get_mut("properties") {
                for (property, child) in children {
                    let annotated =
                        child.get("x-rsi-meta-secret").and_then(Value::as_bool) == Some(true);
                    let transformed_child =
                        transform_reference_validation_schema(child, next_internal_id, true);
                    *child = if shared_secret_properties.contains(property) && !annotated {
                        allow_secret_reference(&transformed_child)
                    } else {
                        transformed_child
                    };
                }
            }
            if let Some(Value::Object(children)) = transformed.get_mut("dependentSchemas") {
                for child in children.values_mut() {
                    *child = transform_reference_validation_schema(child, next_internal_id, false);
                }
            }
            for keyword in [
                "additionalProperties",
                "unevaluatedProperties",
                "propertyNames",
                "items",
                "unevaluatedItems",
                "contains",
                "contentSchema",
            ] {
                if let Some(child) = transformed.get_mut(keyword) {
                    *child = transform_reference_validation_schema(child, next_internal_id, true);
                }
            }
            for keyword in ["not", "if", "then", "else"] {
                if let Some(child) = transformed.get_mut(keyword) {
                    *child = transform_reference_validation_schema(child, next_internal_id, false);
                }
            }
            if let Some(Value::Array(children)) = transformed.get_mut("prefixItems") {
                for child in children {
                    *child = transform_reference_validation_schema(child, next_internal_id, true);
                }
            }
            for keyword in ["allOf", "anyOf", "oneOf"] {
                if let Some(Value::Array(children)) = transformed.get_mut(keyword) {
                    let shared = if keyword == "allOf" {
                        direct_all_of_secret_properties(object)
                    } else {
                        BTreeSet::new()
                    };
                    for child in children {
                        *child = transform_reference_validation_schema_with_shared_properties(
                            child,
                            next_internal_id,
                            false,
                            &shared,
                        );
                    }
                }
            }
            if delegates_to_reference {
                Value::Object(transformed)
            } else {
                disallow_unannotated_reference(transformed, next_internal_id, seal_open_content)
            }
        }
        _ => schema.clone(),
    }
}

fn direct_all_of_secret_properties(schema: &Map<String, Value>) -> BTreeSet<String> {
    let mut properties = BTreeSet::new();
    let Some(Value::Array(branches)) = schema.get("allOf") else {
        return properties;
    };
    for branch in branches {
        collect_direct_secret_properties(branch, &mut properties);
    }
    properties
}

fn collect_direct_secret_properties(schema: &Value, properties: &mut BTreeSet<String>) {
    let Some(object) = schema.as_object() else {
        return;
    };
    if let Some(Value::Object(children)) = object.get("properties") {
        for (property, child) in children {
            if child.get("x-rsi-meta-secret").and_then(Value::as_bool) == Some(true) {
                properties.insert(property.clone());
            }
        }
    }
    if let Some(Value::Array(branches)) = object.get("allOf") {
        for branch in branches {
            collect_direct_secret_properties(branch, properties);
        }
    }
}

fn allow_secret_reference(schema: &Value) -> Value {
    json!({"anyOf": [schema, secret_reference_schema()]})
}

fn disallow_unannotated_reference(
    mut schema: Map<String, Value>,
    next_internal_id: &mut u32,
    seal_open_content: bool,
) -> Value {
    schema
        .entry("allOf")
        .or_insert_with(|| Value::Array(Vec::new()))
        .as_array_mut()
        .expect("JSON Schema allOf must be an array after metaschema validation")
        .push(json!({
            "not": {
                "type": "object",
                "required": ["$secret"]
            }
        }));
    if seal_open_content {
        schema
            .entry("unevaluatedProperties")
            .or_insert_with(|| recursive_secret_free_schema(next_internal_id));
        schema
            .entry("unevaluatedItems")
            .or_insert_with(|| recursive_secret_free_schema(next_internal_id));
    }
    Value::Object(schema)
}

fn recursive_secret_free_schema(next_internal_id: &mut u32) -> Value {
    let id = format!("urn:rsi-meta:internal:secret-free:{}", *next_internal_id);
    *next_internal_id += 1;
    json!({
        "$id": id,
        "$defs": {
            "node": {
                "not": {
                    "type": "object",
                    "required": ["$secret"]
                },
                "additionalProperties": {"$ref": "#/$defs/node"},
                "items": {"$ref": "#/$defs/node"}
            }
        },
        "$ref": "#/$defs/node"
    })
}

fn secret_reference_schema() -> Value {
    json!({
        "type": "object",
        "required": ["$secret"],
        "properties": {
            "$secret": {
                "type": "object",
                "oneOf": [
                    {
                        "required": ["env"],
                        "properties": {
                            "env": {
                                "type": "string",
                                "pattern": "^[A-Za-z_][A-Za-z0-9_]*$"
                            }
                        },
                        "additionalProperties": false
                    },
                    {
                        "required": ["file"],
                        "properties": {
                            "file": {"type": "string", "minLength": 1}
                        },
                        "additionalProperties": false
                    },
                    {
                        "required": ["keyring"],
                        "properties": {
                            "keyring": {
                                "type": "object",
                                "required": ["service", "user"],
                                "properties": {
                                    "service": {"type": "string", "minLength": 1, "maxLength": MAX_KEYRING_IDENTIFIER_BYTES},
                                    "user": {"type": "string", "minLength": 1, "maxLength": MAX_KEYRING_IDENTIFIER_BYTES}
                                },
                                "additionalProperties": false
                            }
                        },
                        "additionalProperties": false
                    }
                ]
            }
        },
        "additionalProperties": false
    })
}

#[allow(clippy::too_many_lines)] // One recursive walk produces resolved and redacted twins.
fn resolve_value(value: Value, instance_path: &str) -> Result<(Value, Value), ConfigPrepareError> {
    match value {
        Value::Object(mut object) if object.contains_key("$secret") => {
            let source = object
                .remove("$secret")
                .and_then(|value| value.as_object().cloned())
                .ok_or_else(|| ConfigPrepareError::InvalidSecretReference {
                    instance_path: instance_path.to_owned(),
                })?;
            if !object.is_empty() || source.len() != 1 {
                return Err(ConfigPrepareError::InvalidSecretReference {
                    instance_path: instance_path.to_owned(),
                });
            }
            if let Some(env_name) = source.get("env").and_then(Value::as_str) {
                if !valid_env_name(env_name) {
                    return Err(ConfigPrepareError::InvalidSecretReference {
                        instance_path: instance_path.to_owned(),
                    });
                }
                let secret =
                    std::env::var(env_name).map_err(|_| ConfigPrepareError::SecretUnavailable {
                        instance_path: instance_path.to_owned(),
                    })?;
                return Ok((
                    Value::String(secret),
                    json!({"$secret": {"env": "<redacted>"}}),
                ));
            }
            if let Some(path) = source.get("file").and_then(Value::as_str) {
                let secret = resolve_secret_file(Path::new(path), instance_path)?;
                return Ok((
                    Value::String(secret),
                    json!({"$secret": {"file": "<redacted>"}}),
                ));
            }
            if let Some(keyring) = source.get("keyring").and_then(Value::as_object) {
                let service = keyring
                    .get("service")
                    .and_then(Value::as_str)
                    .filter(|value| valid_keyring_identifier(value));
                let user = keyring
                    .get("user")
                    .and_then(Value::as_str)
                    .filter(|value| valid_keyring_identifier(value));
                let (Some(service), Some(user)) = (service, user) else {
                    return Err(ConfigPrepareError::InvalidSecretReference {
                        instance_path: instance_path.to_owned(),
                    });
                };
                if keyring.len() != 2 {
                    return Err(ConfigPrepareError::InvalidSecretReference {
                        instance_path: instance_path.to_owned(),
                    });
                }
                let entry = keyring::Entry::new(service, user).map_err(|_| {
                    ConfigPrepareError::SecretUnavailable {
                        instance_path: instance_path.to_owned(),
                    }
                })?;
                let secret =
                    entry
                        .get_password()
                        .map_err(|_| ConfigPrepareError::SecretUnavailable {
                            instance_path: instance_path.to_owned(),
                        })?;
                return Ok((
                    Value::String(secret),
                    json!({
                        "$secret": {
                            "keyring": {
                                "service": "<redacted>",
                                "user": "<redacted>"
                            }
                        }
                    }),
                ));
            }
            Err(ConfigPrepareError::InvalidSecretReference {
                instance_path: instance_path.to_owned(),
            })
        }
        Value::Object(object) => {
            let mut resolved = Map::new();
            let mut redacted = Map::new();
            for (key, child) in object {
                let child_path = pointer_push(instance_path, &key);
                let (resolved_child, redacted_child) = resolve_value(child, &child_path)?;
                resolved.insert(key.clone(), resolved_child);
                redacted.insert(key, redacted_child);
            }
            Ok((Value::Object(resolved), Value::Object(redacted)))
        }
        Value::Array(array) => {
            let mut resolved = Vec::with_capacity(array.len());
            let mut redacted = Vec::with_capacity(array.len());
            for (index, child) in array.into_iter().enumerate() {
                let child_path = pointer_push(instance_path, &index.to_string());
                let (resolved_child, redacted_child) = resolve_value(child, &child_path)?;
                resolved.push(resolved_child);
                redacted.push(redacted_child);
            }
            Ok((Value::Array(resolved), Value::Array(redacted)))
        }
        scalar => Ok((scalar.clone(), scalar)),
    }
}

fn valid_keyring_identifier(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= MAX_KEYRING_IDENTIFIER_BYTES
        && !value.chars().any(char::is_control)
}

fn valid_env_name(name: &str) -> bool {
    let mut bytes = name.bytes();
    matches!(bytes.next(), Some(first) if first.is_ascii_alphabetic() || first == b'_')
        && bytes.all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
}

#[cfg(unix)]
fn resolve_secret_file(path: &Path, instance_path: &str) -> Result<String, ConfigPrepareError> {
    use std::os::unix::fs::{MetadataExt, OpenOptionsExt};

    if !path.is_absolute() {
        return Err(ConfigPrepareError::InvalidSecretReference {
            instance_path: instance_path.to_owned(),
        });
    }
    let before = fs::symlink_metadata(path).map_err(|_| ConfigPrepareError::SecretUnavailable {
        instance_path: instance_path.to_owned(),
    })?;
    if before.file_type().is_symlink()
        || !before.file_type().is_file()
        || before.len() > MAX_SECRET_FILE_BYTES as u64
    {
        return Err(ConfigPrepareError::UnsafeSecretFile {
            instance_path: instance_path.to_owned(),
        });
    }
    let mut file = fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK)
        .open(path)
        .map_err(|_| ConfigPrepareError::SecretUnavailable {
            instance_path: instance_path.to_owned(),
        })?;
    let metadata = file
        .metadata()
        .map_err(|_| ConfigPrepareError::SecretUnavailable {
            instance_path: instance_path.to_owned(),
        })?;
    let mode = metadata.mode() & 0o7777;
    if !metadata.file_type().is_file()
        || metadata.dev() != before.dev()
        || metadata.ino() != before.ino()
        || metadata.uid() != rustix::process::geteuid().as_raw()
        || mode & !0o600 != 0
        || metadata.len() > MAX_SECRET_FILE_BYTES as u64
    {
        return Err(ConfigPrepareError::UnsafeSecretFile {
            instance_path: instance_path.to_owned(),
        });
    }
    let mut secret = Vec::with_capacity(
        usize::try_from(metadata.len())
            .unwrap_or(MAX_SECRET_FILE_BYTES)
            .min(MAX_SECRET_FILE_BYTES),
    );
    Read::by_ref(&mut file)
        .take(MAX_SECRET_FILE_BYTES as u64 + 1)
        .read_to_end(&mut secret)
        .map_err(|_| ConfigPrepareError::SecretUnavailable {
            instance_path: instance_path.to_owned(),
        })?;
    if secret.len() > MAX_SECRET_FILE_BYTES {
        return Err(ConfigPrepareError::UnsafeSecretFile {
            instance_path: instance_path.to_owned(),
        });
    }
    String::from_utf8(secret).map_err(|_| ConfigPrepareError::SecretUnavailable {
        instance_path: instance_path.to_owned(),
    })
}

#[cfg(not(unix))]
fn resolve_secret_file(_path: &Path, instance_path: &str) -> Result<String, ConfigPrepareError> {
    Err(ConfigPrepareError::UnsafeSecretFile {
        instance_path: instance_path.to_owned(),
    })
}

fn pointer_push(base: &str, segment: &str) -> String {
    format!("{base}/{}", segment.replace('~', "~0").replace('/', "~1"))
}

fn canonicalize(value: &Value) -> Value {
    match value {
        Value::Object(object) => {
            let mut keys: Vec<_> = object.keys().collect();
            keys.sort_unstable();
            let mut canonical = Map::new();
            for key in keys {
                canonical.insert(key.clone(), canonicalize(&object[key]));
            }
            Value::Object(canonical)
        }
        Value::Array(array) => Value::Array(array.iter().map(canonicalize).collect()),
        scalar => scalar.clone(),
    }
}
