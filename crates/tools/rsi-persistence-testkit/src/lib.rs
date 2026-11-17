//! Shared schema and immutable writer evidence for owner-local persistence tests.
#![deny(unsafe_code)]
#![warn(missing_docs)]
use schemars::{JsonSchema, generate::SchemaSettings};
use serde_json::{Value, json};
use std::collections::BTreeMap;

/// Normalizes the actual serialization schema while preserving object field order.
/// Helper type names and schema annotations do not participate in the result.
///
/// # Panics
/// Panics if Schemars emits an unsupported reference or malformed schema node.
pub fn structural<T: JsonSchema>() -> Value {
    fn visit(
        value: &mut Value,
        definitions: &Value,
        identities: &mut BTreeMap<String, usize>,
        retained: &mut Vec<Value>,
    ) {
        match value {
            Value::Object(object) => {
                for annotation in ["title", "description", "examples", "$schema"] {
                    object.shift_remove(annotation);
                }
                if let Some(reference) = object.get_mut("$ref") {
                    let name = reference
                        .as_str()
                        .unwrap()
                        .strip_prefix("#/definitions/")
                        .unwrap();
                    let identity = if let Some(id) = identities.get(name) {
                        *id
                    } else {
                        let id = retained.len();
                        identities.insert(name.to_owned(), id);
                        retained.push(Value::Null);
                        let mut definition = definitions[name].clone();
                        assert!(!definition.is_null(), "missing schema definition {name}");
                        visit(&mut definition, definitions, identities, retained);
                        retained[id] = definition;
                        id
                    };
                    *reference = json!(format!("#/definitions/{identity}"));
                }
                for (keyword, child) in object {
                    match keyword.as_str() {
                        "properties" | "patternProperties" | "dependentSchemas" => {
                            for schema in child.as_object_mut().unwrap().values_mut() {
                                visit(schema, definitions, identities, retained);
                            }
                        }
                        "additionalProperties"
                        | "unevaluatedProperties"
                        | "items"
                        | "additionalItems"
                        | "unevaluatedItems"
                        | "allOf"
                        | "anyOf"
                        | "oneOf"
                        | "prefixItems"
                        | "not"
                        | "if"
                        | "then"
                        | "else"
                        | "contains"
                        | "propertyNames" => {
                            visit(child, definitions, identities, retained);
                        }
                        _ => {}
                    }
                }
            }
            Value::Array(values) => {
                for child in values {
                    visit(child, definitions, identities, retained);
                }
            }
            _ => {}
        }
    }
    let schema = SchemaSettings::draft07()
        .for_serialize()
        .into_generator()
        .into_root_schema_for::<T>();
    let mut root = serde_json::to_value(schema).unwrap();
    let definitions = root
        .as_object_mut()
        .unwrap()
        .remove("definitions")
        .unwrap_or_else(|| json!({}));
    let mut retained = Vec::new();
    visit(&mut root, &definitions, &mut BTreeMap::new(), &mut retained);
    json!({"root": root, "definitions": retained})
}

/// Restores a standard JSON Schema object from a normalized structural schema.
///
/// # Panics
/// Panics if the input was not produced by [`structural`].
pub fn validation_schema(structural: &Value) -> Value {
    let mut root = structural["root"].clone();
    let definitions: serde_json::Map<String, Value> = structural["definitions"]
        .as_array()
        .unwrap()
        .iter()
        .enumerate()
        .map(|(index, schema)| (index.to_string(), schema.clone()))
        .collect();
    root.as_object_mut()
        .unwrap()
        .insert("definitions".into(), Value::Object(definitions));
    root
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn fingerprint_ignores_rust_names_and_prose_but_preserves_field_order() {
        #[derive(JsonSchema)]
        #[allow(dead_code)]
        struct First {
            alpha: String,
            beta: u32,
        }
        /// Different documentation and a different type name.
        #[derive(JsonSchema)]
        #[allow(dead_code)]
        struct Renamed {
            alpha: String,
            beta: u32,
        }
        #[derive(JsonSchema)]
        #[allow(dead_code)]
        struct Reordered {
            beta: u32,
            alpha: String,
        }
        #[derive(JsonSchema)]
        #[allow(dead_code)]
        struct Parent {
            description: First,
            title: String,
        }
        #[derive(JsonSchema)]
        #[allow(dead_code)]
        struct OtherParent {
            description: Renamed,
            title: String,
        }
        assert_eq!(structural::<First>(), structural::<Renamed>());
        // Value equality ignores object order, whereas encoded bytes do not.
        assert_ne!(
            serde_json::to_vec(&structural::<First>()).unwrap(),
            serde_json::to_vec(&structural::<Reordered>()).unwrap()
        );
        let parent = structural::<Parent>();
        assert_eq!(parent, structural::<OtherParent>());
        assert!(parent["root"]["properties"].get("description").is_some());
        assert!(parent["root"]["properties"].get("title").is_some());
    }
}

/// Explicit capture and read-only verification of owner-local payload evidence.
pub mod payload;

/// Declares an owner-local read-only gate and an explicit immutable capture test.
/// The path is relative to the invoking crate; samples use its actual codec.
#[macro_export]
macro_rules! payload_contract {
    ($module:ident, $payload:ty, $version:expr, $path:expr, $samples:expr) => {
        $crate::payload_contract!(
            $module,
            $payload,
            $version,
            $path,
            $samples,
            |path: &std::path::Path, version: u32| {
                panic!(
                    "owner must exercise read/refusal for version {version}: {}",
                    path.display()
                );
            }
        );
    };
    ($module:ident, $payload:ty, $version:expr, $path:expr, $samples:expr, $legacy:expr) => {
        #[cfg(test)]
        mod $module {
            use super::*;
            fn path() -> std::path::PathBuf {
                std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join($path)
            }
            #[test]
            fn current_payload() {
                $crate::payload::verify_corpus::<$payload>(&path(), $version, $legacy);
            }
            #[test]
            #[ignore = "explicitly captures a new immutable codec fixture"]
            fn capture_current_payload() {
                let samples: Vec<$payload> = $samples;
                $crate::payload::capture(&path(), $version, &samples);
            }
        }
    };
}
