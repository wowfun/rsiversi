use crate::{ProtocolError, Result};
use serde::{Deserialize, Serialize};

/// Semantic domain selector, interpreted only by an explicitly installed target owner.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExportScope {
    /// Registered semantic scope, for example `session`.
    pub kind: String,
    /// Domain identifier, never a Context or Local key.
    pub key: String,
}
impl ExportScope {
    /// Bounds the selector without granting authority or resolving a domain object.
    pub fn validate(&self) -> Result<()> {
        if !crate::name_valid(&self.kind)
            || self.key.len() > 1024
            || self.key.chars().any(char::is_control)
        {
            return Err(ProtocolError("invalid export scope".into()));
        }
        Ok(())
    }
}
