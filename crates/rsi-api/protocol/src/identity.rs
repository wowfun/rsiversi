use crate::{ApiError, Result};
use serde::{Deserialize, Serialize};

macro_rules! identity {
    ($name:ident, $bytes:literal, $doc:literal) => {
        #[doc = $doc]
        #[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Hash, Serialize)]
        #[serde(transparent)]
        pub struct $name(String);

        impl $name {
            /// Validates the canonical fixed-width lowercase hexadecimal representation.
            pub fn parse(value: impl Into<String>) -> Result<Self> {
                let value = value.into();
                if value.len() != $bytes * 2
                    || !value
                        .bytes()
                        .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
                {
                    return Err(ApiError::Invalid(format!(
                        "{} must be {} lowercase hexadecimal bytes",
                        stringify!($name),
                        $bytes * 2
                    )));
                }
                Ok(Self(value))
            }
            /// Constructs an identity from entropy supplied by its platform owner.
            pub fn from_bytes(bytes: [u8; $bytes]) -> Self {
                Self(hex::encode(bytes))
            }
            /// Allocates an unpredictable identity from native OS entropy.
            #[cfg(not(target_family = "wasm"))]
            pub fn generate() -> Result<Self> {
                let mut bytes = [0; $bytes];
                getrandom::fill(&mut bytes)
                    .map_err(|_| ApiError::Backend("OS entropy failed".into()))?;
                Ok(Self::from_bytes(bytes))
            }
            /// Borrows the exact non-secret identity.
            pub fn as_str(&self) -> &str {
                &self.0
            }
        }
        impl<'de> Deserialize<'de> for $name {
            fn deserialize<D: serde::Deserializer<'de>>(
                deserializer: D,
            ) -> std::result::Result<Self, D::Error> {
                Self::parse(String::deserialize(deserializer)?).map_err(serde::de::Error::custom)
            }
        }
    };
}

identity!(
    EndpointId,
    16,
    "Stable deployment identity persisted across restarts and code upgrades."
);
identity!(
    HostEpoch,
    16,
    "Identity of one running Host generation, independent of wire/build versions."
);
identity!(
    DeviceId,
    16,
    "Opaque identity allocated by an authenticated device registration owner."
);

identity!(
    LocalCompatibilityKey,
    32,
    "Opaque local deployment-selection fence, distinct from peer authentication."
);
