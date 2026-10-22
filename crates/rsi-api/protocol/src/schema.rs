use crate::{ApiError, Result};
use serde::{Deserialize, Serialize};

/// Independently versioned wire contract, separate from executable identity.
#[derive(Clone, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ProtocolDescriptor {
    /// Stable owning interface name.
    pub name: String,
    /// Explicit semantic model revision.
    pub version: u32,
    /// SHA-256 of the generated normalized wire-contract schema.
    pub schema: String,
}
impl ProtocolDescriptor {
    /// Validates a descriptor at a decoded catalog or manifest boundary.
    pub fn validate(&self) -> Result<()> {
        if self.name.is_empty()
            || self.name.len() > 128
            || !self
                .name
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b"._-".contains(&b))
            || self.version == 0
            || self.schema.len() != 64
            || !self
                .schema
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        {
            return Err(ApiError::Invalid("invalid protocol descriptor".into()));
        }
        Ok(())
    }
}
/// Uses the invoking crate's generated owning wire contract.
#[macro_export]
macro_rules! wire_contract {
    () => {
        $crate::ProtocolDescriptor {
            name: env!("RSI_WIRE_NAME").into(),
            version: env!("RSI_WIRE_VERSION")
                .parse()
                .expect("generated protocol version"),
            schema: env!("RSI_WIRE_SCHEMA").into(),
        }
    };
}
/// Foundation connection/framing contract, also suitable for isolated transport fixtures.
pub fn connection_protocol() -> ProtocolDescriptor {
    crate::wire_contract!()
}

#[cfg(test)]
mod tests {
    #[test]
    fn descriptor_identity_matches_shared_corpus() {
        let cases: Vec<serde_json::Value> =
            serde_json::from_str(include_str!("../descriptor-conformance.json")).unwrap();
        for mut case in cases {
            let valid = case
                .as_object_mut()
                .unwrap()
                .remove("valid")
                .unwrap()
                .as_bool()
                .unwrap();
            case["schema"] = "a".repeat(64).into();
            let result = serde_json::from_value::<super::ProtocolDescriptor>(case.clone());
            assert_eq!(
                result.is_ok_and(|value| value.validate().is_ok()),
                valid,
                "{case}"
            );
        }
    }
}
