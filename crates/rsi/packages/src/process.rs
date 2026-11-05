use crate::{Result, invalid};
use serde::{Deserialize, Serialize};

/// Immutable path inside a payload of this package or one exact direct dependency.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PayloadPath {
    /// Omitted for the declaring package; otherwise a declared direct dependency.
    pub package: Option<String>,
    /// Named payload export.
    pub payload: String,
    /// Portable relative path, or empty for the payload root.
    pub path: String,
}
impl PayloadPath {
    fn validate(&self) -> Result<()> {
        if let Some(package) = &self.package {
            crate::identifier(package)?;
        }
        crate::identifier(&self.payload)?;
        if !self.path.is_empty() {
            crate::manifest::relative_payload(&self.path)?;
        }
        Ok(())
    }
}
/// One explicitly encoded argument; no shell or string-template evaluation occurs.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum ProcessArgument {
    /// Exact UTF-8 argument.
    Literal {
        /// Argument bytes, bounded to 16 KiB in aggregate.
        value: String,
    },
    /// Host-issued generation identity.
    Generation,
    /// Leased file or directory path.
    Path {
        /// Payload-relative location.
        location: PayloadPath,
    },
}
/// Non-executing descriptor consumed by an ordinary process-extension factory.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ProcessExtension {
    /// Current descriptor format is one.
    pub format: u32,
    /// Payload directory used as the working directory.
    pub cwd: PayloadPath,
    /// Complete argv after the operator-selected executable.
    pub arguments: Vec<ProcessArgument>,
}
impl ProcessExtension {
    /// Bounds and validates a descriptor before resolving any path.
    pub fn parse(bytes: &[u8]) -> Result<Self> {
        if bytes.len() > 64 * 1024 {
            return Err(invalid("process descriptor exceeds 64 KiB"));
        }
        let value: Self =
            serde_json::from_slice(bytes).map_err(|error| invalid(error.to_string()))?;
        if value.format != 1 || value.arguments.len() > 64 {
            return Err(invalid("process descriptor format or argument count"));
        }
        value.cwd.validate()?;
        let mut literal_bytes = 0_usize;
        for argument in &value.arguments {
            match argument {
                ProcessArgument::Literal { value } => {
                    literal_bytes = literal_bytes.saturating_add(value.len());
                    if value.contains('\0') || literal_bytes > 16 * 1024 {
                        return Err(invalid("process literal bounds"));
                    }
                }
                ProcessArgument::Path { location } => location.validate()?,
                ProcessArgument::Generation => {}
            }
        }
        Ok(value)
    }
}
