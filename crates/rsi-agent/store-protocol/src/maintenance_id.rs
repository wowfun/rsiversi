use crate::{Result, StoreError};
use serde::{Deserialize, Serialize};
use std::fmt;

/// Caller-allocated identity of one durable maintenance operation.
/// Uses the shared bounded identifier grammar, independently of Session identity.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(transparent)]
pub struct MaintenanceOperationId(String);

impl MaintenanceOperationId {
    /// Validates an identity before it can enter a request or failure receipt.
    pub fn new(value: impl Into<String>) -> Result<Self> {
        let value = value.into();
        rsi_agent_session_protocol::validate_identifier("maintenance operation", &value)
            .map_err(|error| StoreError::Invalid(error.to_string()))?;
        Ok(Self(value))
    }
    /// Returns the original identity.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}
impl<'de> Deserialize<'de> for MaintenanceOperationId {
    fn deserialize<D: serde::Deserializer<'de>>(decoder: D) -> std::result::Result<Self, D::Error> {
        Self::new(String::deserialize(decoder)?).map_err(serde::de::Error::custom)
    }
}
impl fmt::Display for MaintenanceOperationId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}
impl AsRef<str> for MaintenanceOperationId {
    fn as_ref(&self) -> &str {
        self.as_str()
    }
}
impl std::str::FromStr for MaintenanceOperationId {
    type Err = StoreError;
    fn from_str(value: &str) -> Result<Self> {
        Self::new(value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn construction_and_decode_share_the_identifier_boundary() {
        for value in ["", "../foreign", "with space", &"a".repeat(257)] {
            assert!(MaintenanceOperationId::new(value).is_err());
            assert!(
                serde_json::from_value::<MaintenanceOperationId>(serde_json::json!(value)).is_err()
            );
        }
        let id = MaintenanceOperationId::new("purge-1").unwrap();
        assert_eq!(
            serde_json::to_value(&id).unwrap(),
            serde_json::json!("purge-1")
        );
        assert_eq!(
            serde_json::from_value::<MaintenanceOperationId>(serde_json::json!("purge-1")).unwrap(),
            id
        );
    }
}
