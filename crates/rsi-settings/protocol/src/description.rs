use crate::{Result, SettingsError, SettingsVersion, validate_namespace, validate_section};
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Maximum registered namespaces returned by one discovery page.
pub const MAXIMUM_SETTINGS_PAGE: usize = 64;
/// Maximum encoded bytes in a namespace's complete descriptive declaration.
pub const MAXIMUM_SETTINGS_METADATA_BYTES: usize = 64 * 1024;

/// When the namespace owner applies accepted values.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SettingsApply {
    /// The owner observes new values during its current lifetime.
    Live,
    /// Existing Sessions retain their captured settings.
    NewSession,
    /// Rebuild or restart the owning composition to apply changes.
    Restart,
}

/// Non-executable description supplied by the namespace owner.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SettingsMetadata {
    /// JSON Schema describing the resolved value; the registered validator owns acceptance.
    pub schema: Value,
    /// Application timing, independent of successful persistence.
    pub applies: SettingsApply,
    /// Explanation of the owner's application boundary.
    pub description: String,
    /// Object/array field paths whose non-secret values need discreet presentation.
    pub sensitive_fields: Vec<Vec<String>>,
}
impl SettingsMetadata {
    /// Checks declaration shape and retained wire bounds before registration or client exposure.
    pub fn validate(&self) -> Result<()> {
        if !self.schema.is_object() && !self.schema.is_boolean()
            || self.description.len() > 4096
            || self.sensitive_fields.len() > 64
            || self.sensitive_fields.iter().any(|path| {
                path.len() > 32
                    || path
                        .iter()
                        .any(|part| part.len() > 256 || part.chars().any(char::is_control))
            })
        {
            return Err(SettingsError::InvalidInput(
                "invalid Settings metadata".into(),
            ));
        }
        rsi_api_protocol::measure_json(self, MAXIMUM_SETTINGS_METADATA_BYTES)
            .map(|_| ())
            .map_err(|_| {
                SettingsError::InvalidInput("Settings metadata exceeds its byte bound".into())
            })
    }
}

/// One bounded lexical page over active registrations, not a registry snapshot.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SettingsPage {
    /// Strictly increasing active namespace names.
    pub namespaces: Vec<String>,
    /// Last returned name when more entries were present at capture.
    pub next: Option<String>,
}
impl SettingsPage {
    /// Checks response progress against the original bounded request.
    pub fn validate(&self, after: Option<&str>, limit: usize) -> Result<()> {
        validate_settings_page(after, limit)?;
        if self.namespaces.len() > limit
            || self.namespaces.windows(2).any(|pair| pair[0] >= pair[1])
            || self
                .namespaces
                .first()
                .is_some_and(|name| after.is_some_and(|after| name.as_str() <= after))
            || self.next.as_ref().is_some_and(|next| {
                self.namespaces.last() != Some(next) || self.namespaces.len() != limit
            })
        {
            return Err(SettingsError::InvalidInput(
                "invalid Settings discovery page".into(),
            ));
        }
        for name in &self.namespaces {
            validate_namespace(name)?;
        }
        Ok(())
    }
}

/// One registration's atomic descriptive view without its raw provider section.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SettingsDescription {
    /// Exact registered name requested by the caller.
    pub namespace: String,
    /// Registration identity and current committed revision at capture.
    pub version: SettingsVersion,
    /// Schema-owned default layer, before composition and user overlays.
    pub defaults: Value,
    /// Whether the current provider accepts changes.
    pub writable: bool,
    /// Non-executable owner declaration.
    pub metadata: SettingsMetadata,
}
impl SettingsDescription {
    /// Validates an exact returned description at the client boundary.
    pub fn validate(&self, namespace: &str) -> Result<()> {
        validate_namespace(namespace)?;
        if self.namespace != namespace {
            return Err(SettingsError::InvalidInput(
                "Settings description namespace mismatch".into(),
            ));
        }
        validate_section(&self.defaults)?;
        self.metadata.validate()
    }
}

/// Validates a lexical discovery request before retaining names or issuing I/O.
pub fn validate_settings_page(after: Option<&str>, limit: usize) -> Result<()> {
    if !(1..=MAXIMUM_SETTINGS_PAGE).contains(&limit) {
        return Err(SettingsError::InvalidInput(
            "invalid Settings discovery page limit".into(),
        ));
    }
    if let Some(after) = after {
        validate_namespace(after)?;
    }
    Ok(())
}
