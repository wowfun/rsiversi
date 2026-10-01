//! Complete template discovery and bounded server-owned URI expansion.

use crate::{Result, name};
use iri_string::template::UriTemplateStr;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};

/// Values supported by RFC 6570; numbers, booleans and nested collections are rejected.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum TemplateParameter {
    /// A scalar string.
    String(String),
    /// An ordered list of strings.
    List(Vec<String>),
    /// A map, expanded in deterministic key order.
    Map(BTreeMap<String, String>),
}

/// Validated, bounded parameters. Missing declared variables remain undefined.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct TemplateParameters(BTreeMap<String, TemplateParameter>);

impl TemplateParameters {
    /// Validates untrusted tool input independently of the tool's JSON schema.
    pub fn from_value(value: Value) -> Result<Self> {
        if serde_json::to_vec(&value)
            .map_err(|_| "Invalid template parameters")?
            .len()
            > 65536
        {
            return Err("MCP template parameters exceed 64 KiB".into());
        }
        let values: BTreeMap<String, TemplateParameter> = serde_json::from_value(value)
            .map_err(|_| "Template parameters must contain strings, string lists or string maps")?;
        if values.len() > 32 {
            return Err("MCP template parameters exceed 32 variables".into());
        }
        let mut leaves = 0usize;
        for (key, value) in &values {
            if key.len() > 256 || iri_string::template::context::VarName::new(key).is_err() {
                return Err("Invalid MCP template variable name".into());
            }
            let valid = match value {
                TemplateParameter::String(value) => {
                    leaves += 1;
                    value.len() <= 4096
                }
                TemplateParameter::List(values) => {
                    leaves += values.len();
                    values.iter().all(|value| value.len() <= 4096)
                }
                TemplateParameter::Map(values) => {
                    leaves += values.len() * 2;
                    values
                        .iter()
                        .all(|(key, value)| key.len() <= 4096 && value.len() <= 4096)
                }
            };
            if !valid || leaves > 256 {
                return Err("MCP template parameter leaves exceed their bound".into());
            }
        }
        Ok(Self(values))
    }
}

/// Complete resource-template metadata; extra fields have no execution semantics.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct McpResourceTemplate {
    /// RFC 6570 template interpreted only by its configured MCP server.
    pub uri_template: String,
    /// Human-readable catalog name.
    pub name: String,
    /// Attributed external description.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Optional advertised resource content type.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mime_type: Option<String>,
    /// Preserved descriptive metadata.
    #[serde(flatten)]
    pub extensions: BTreeMap<String, Value>,
}

/// Discovery state is distinct from an enabled but empty catalog.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case", deny_unknown_fields)]
pub enum TemplateCatalog {
    /// The operator has not authorized parameterized resource access.
    #[default]
    Disabled,
    /// The server explicitly reported `MethodNotFound` or no resource capability.
    Unsupported,
    /// A complete, validated template catalog, possibly empty.
    Available {
        /// All templates; partial discovery is never published.
        templates: Vec<McpResourceTemplate>,
    },
}

impl TemplateCatalog {
    /// Returns the complete enabled catalog without conflating its state.
    pub fn entries(&self) -> &[McpResourceTemplate] {
        match self {
            Self::Available { templates } => templates,
            Self::Disabled | Self::Unsupported => &[],
        }
    }

    /// Validates finite, uniquely identified templates before freezing a catalog.
    pub fn validate(&self) -> Result<()> {
        if self.entries().len() > crate::MAXIMUM_RESOURCES {
            return Err("MCP template catalog exceeds its bound".into());
        }
        let mut identities = BTreeSet::new();
        for template in self.entries() {
            template.validate()?;
            if !identities.insert(&template.uri_template) {
                return Err("Duplicate MCP resource template".into());
            }
        }
        Ok(())
    }
}

struct BoundedUri(String);
impl std::fmt::Write for BoundedUri {
    fn write_str(&mut self, text: &str) -> std::fmt::Result {
        if text.len() > 4096 - self.0.len() {
            return Err(std::fmt::Error);
        }
        self.0.push_str(text);
        Ok(())
    }
}

impl McpResourceTemplate {
    /// Expands only declared variables with URI escaping into at most 4096 bytes.
    /// The returned URI is data for this MCP server, never a local Files path.
    pub fn expand(&self, parameters: &TemplateParameters) -> Result<String> {
        use iri_string::{
            spec::UriSpec,
            template::simple_context::{SimpleContext, Value as ContextValue},
        };
        let template =
            UriTemplateStr::new(&self.uri_template).map_err(|_| "Invalid MCP resource template")?;
        let declared = template
            .variables()
            .map(|name| name.as_str())
            .collect::<BTreeSet<_>>();
        let mut context = SimpleContext::new();
        for (key, value) in &parameters.0 {
            if !declared.contains(key.as_str()) {
                return Err("Undeclared MCP template parameter".into());
            }
            let value = match value {
                TemplateParameter::String(value) => ContextValue::String(value.clone()),
                TemplateParameter::List(value) => ContextValue::List(value.clone()),
                TemplateParameter::Map(value) => {
                    ContextValue::Assoc(value.iter().map(|(k, v)| (k.clone(), v.clone())).collect())
                }
            };
            context.insert(key, value);
        }
        let mut output = BoundedUri(String::new());
        template
            .expand_dynamic::<UriSpec, _, _>(&mut output, &mut context)
            .map_err(|_| "MCP URI expansion failed or exceeds 4096 bytes")?;
        Ok(output.0)
    }

    /// Checks complete metadata and syntax without expanding or contacting a server.
    pub fn validate(&self) -> Result<()> {
        if !name(&self.uri_template, 4096)
            || !name(&self.name, 256)
            || self.description.as_ref().is_some_and(|v| !name(v, 4096))
            || self.mime_type.as_ref().is_some_and(|v| !name(v, 128))
            || UriTemplateStr::new(&self.uri_template).is_err()
        {
            return Err("Invalid MCP resource template".into());
        }
        Ok(())
    }
}
