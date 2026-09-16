//! Typed projection of schema-designated HTTP metadata; it grants no authority.
use crate::Result;
use base64::{Engine as _, engine::general_purpose::STANDARD};
use serde_json::Value;
use std::collections::BTreeSet;

/// Aggregate encoded custom HTTP parameter metadata ceiling.
pub const MAXIMUM_HTTP_PARAMETER_BYTES: usize = 16 * 1024;

#[derive(Clone, Debug)]
enum Primitive {
    String,
    Integer,
    Boolean,
}

/// Validated immutable annotation at a direct object-property path.
#[derive(Clone, Debug)]
pub struct HttpParameter {
    name: String,
    path: Vec<String>,
    kind: Primitive,
}
impl HttpParameter {
    /// Projects an optional argument value without interpreting any other schema keywords.
    pub fn project(&self, arguments: &Value) -> Result<Option<(String, String)>> {
        let mut value = arguments;
        for part in &self.path {
            let Some(next) = value.get(part) else {
                return Ok(None);
            };
            value = next;
        }
        let text = match self.kind {
            Primitive::String => value.as_str().map(str::to_owned),
            Primitive::Boolean => value.as_bool().map(|v| v.to_string()),
            Primitive::Integer => exact_integer(value).map(|value| value.to_string()),
        }
        .ok_or("Invalid MCP HTTP parameter value")?;
        let encoded = encode_header_value(&text);
        if encoded.len() > MAXIMUM_HTTP_PARAMETER_BYTES {
            return Err("MCP HTTP parameter exceeds 16 KiB".into());
        }
        Ok(Some((self.name.clone(), encoded)))
    }
}

fn exact_integer(value: &Value) -> Option<i64> {
    let raw = value.as_number()?.to_string();
    let (mantissa, exponent) = raw
        .split_once(['e', 'E'])
        .map_or(Some((raw.as_str(), 0_i32)), |(m, e)| {
            Some((m, e.parse::<i32>().ok()?))
        })?;
    let negative = mantissa.starts_with('-');
    let mantissa = mantissa.strip_prefix('-').unwrap_or(mantissa);
    let (whole, fraction) = mantissa.split_once('.').unwrap_or((mantissa, ""));
    let digits = format!("{whole}{fraction}");
    let mut digits = digits.trim_start_matches('0').to_owned();
    if digits.is_empty() {
        return Some(0);
    }
    let scale = exponent.checked_sub(i32::try_from(fraction.len()).ok()?)?;
    if scale < 0 {
        let remove = usize::try_from(scale.checked_neg()?).ok()?;
        let keep = digits.len().checked_sub(remove)?;
        if !digits[keep..].bytes().all(|byte| byte == b'0') {
            return None;
        }
        digits.truncate(keep);
    } else {
        let zeros = usize::try_from(scale).ok()?;
        if digits.len().checked_add(zeros)? > 16 {
            return None;
        }
        digits.extend(std::iter::repeat_n('0', zeros));
    }
    if digits.len() > 16 {
        return None;
    }
    let integer = digits.parse::<i64>().ok()?;
    if integer > 9_007_199_254_740_991 {
        return None;
    }
    Some(if negative { -integer } else { integer })
}

/// Encodes UTF-8, whitespace and literal sentinels per MCP's HTTP value rule.
pub fn encode_header_value(value: &str) -> String {
    if value.trim_matches([' ', '\t']) != value
        || !value
            .bytes()
            .all(|b| b == b'\t' || (0x20..=0x7e).contains(&b))
        || (value.starts_with("=?base64?") && value.ends_with("?="))
    {
        format!("=?base64?{}?=", STANDARD.encode(value))
    } else {
        value.to_owned()
    }
}

/// Admits at most 64 unique annotations; caller has bounded the complete schema.
pub fn http_parameters(schema: &Value) -> Result<Vec<HttpParameter>> {
    let mut parameters = Vec::new();
    visit(schema, Some(&[]), &mut BTreeSet::new(), &mut parameters, 0)?;
    Ok(parameters)
}

fn visit(
    schema: &Value,
    path: Option<&[String]>,
    names: &mut BTreeSet<String>,
    out: &mut Vec<HttpParameter>,
    depth: usize,
) -> Result<()> {
    if depth > 64 {
        return Err("MCP HTTP schema depth exceeded".into());
    }
    let Some(object) = schema.as_object() else {
        return Ok(());
    };
    if let Some(annotation) = object.get("x-mcp-header") {
        let path = path
            .filter(|path| !path.is_empty())
            .ok_or("MCP HTTP annotation is not on a direct property")?;
        let name = annotation
            .as_str()
            .filter(|name| {
                !name.is_empty()
                    && name.len() <= 128
                    && name
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(&b))
            })
            .ok_or("Invalid MCP HTTP parameter name")?;
        if out.len() == 64 || !names.insert(name.to_ascii_lowercase()) {
            return Err("MCP HTTP parameter count or name conflict".into());
        }
        let kind = match object.get("type").and_then(Value::as_str) {
            Some("string") => Primitive::String,
            Some("integer") => Primitive::Integer,
            Some("boolean") => Primitive::Boolean,
            _ => return Err("MCP HTTP parameter must have a primitive type".into()),
        };
        out.push(HttpParameter {
            name: format!("Mcp-Param-{name}"),
            path: path.to_vec(),
            kind,
        });
    }
    for (key, child) in object {
        match key.as_str() {
            "properties" | "patternProperties" | "$defs" | "definitions" | "dependentSchemas" => {
                if let Some(children) = child.as_object() {
                    for (name, schema) in children {
                        let next = path
                            .filter(|_| key == "properties" && !object.contains_key("$ref"))
                            .map(|path| {
                                let mut next = path.to_vec();
                                next.push(name.clone());
                                next
                            });
                        visit(schema, next.as_deref(), names, out, depth + 1)?;
                    }
                }
            }
            "items"
            | "prefixItems"
            | "contains"
            | "additionalProperties"
            | "unevaluatedProperties"
            | "unevaluatedItems"
            | "propertyNames"
            | "allOf"
            | "anyOf"
            | "oneOf"
            | "not"
            | "if"
            | "then"
            | "else" => {
                if let Some(children) = child.as_array() {
                    for child in children {
                        visit(child, None, names, out, depth + 1)?;
                    }
                } else {
                    visit(child, None, names, out, depth + 1)?;
                }
            }
            _ => {}
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn integer_projection_never_rounds_fractional_or_unsafe_json_numbers() {
        for (raw, expected) in [
            ("1.0", Some(1)),
            ("1e3", Some(1000)),
            ("-0.0", Some(0)),
            ("9007199254740991", Some(9_007_199_254_740_991)),
            ("9007199254740991.0000000000001", None),
            ("9007199254740992", None),
            ("1e99", None),
            ("1e-99", None),
        ] {
            assert_eq!(
                exact_integer(&serde_json::from_str::<Value>(raw).unwrap()),
                expected,
                "{raw}"
            );
        }
    }
    #[test]
    fn schema_paths_and_header_values_preserve_exact_meaning() {
        let schema = json!({"type":"object","properties":{"nested":{"type":"object","properties":{
            "region":{"type":"string","x-mcp-header":"Region"},"id":{"type":"integer","x-mcp-header":"Id"},"on":{"type":"boolean","x-mcp-header":"On"}
        }}}});
        let headers = http_parameters(&schema).unwrap();
        let values =
            json!({"nested":{"region":" 中文\r\n", "id":9_007_199_254_740_991_i64,"on":false}});
        let projected = headers
            .iter()
            .map(|h| h.project(&values).unwrap().unwrap())
            .collect::<Vec<_>>();
        assert!(projected.contains(&(
            "Mcp-Param-Region".into(),
            format!("=?base64?{}?=", STANDARD.encode(" 中文\r\n"))
        )));
        assert!(projected.contains(&("Mcp-Param-Id".into(), "9007199254740991".into())));
        assert!(projected.contains(&("Mcp-Param-On".into(), "false".into())));
        assert!(
            headers
                .iter()
                .all(|h| h.project(&json!({})).unwrap().is_none())
        );
        assert!(headers.iter().any(|h| {
            h.project(&json!({"nested":{"id":9_007_199_254_740_992_i64}}))
                .is_err()
        }));
        assert_eq!(
            encode_header_value("=?base64?literal?="),
            "=?base64?PT9iYXNlNjQ/bGl0ZXJhbD89?="
        );
        assert_eq!(encode_header_value("safe\tvalue"), "safe\tvalue");
    }
    #[test]
    fn invalid_annotation_locations_and_names_reject_the_definition() {
        for schema in [
            json!({"type":"string","x-mcp-header":"Root"}),
            json!({"type":"object","allOf":[{"properties":{"x":{"type":"string","x-mcp-header":"X"}}}]}),
            json!({"type":"object","properties":{"x":{"type":"number","x-mcp-header":"X"}}}),
            json!({"type":"object","properties":{"x":{"type":"string","x-mcp-header":"X\r\n"}}}),
            json!({"type":"object","properties":{"x":{"type":"string","x-mcp-header":"X"},"y":{"type":"string","x-mcp-header":"x"}}}),
        ] {
            assert!(http_parameters(&schema).is_err(), "{schema}");
        }
    }
}
