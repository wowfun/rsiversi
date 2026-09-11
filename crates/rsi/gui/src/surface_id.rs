use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::fmt;

/// Bounded stable document surface identity, independent of its attached Session.
#[derive(Clone, Copy, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct SurfaceId {
    bytes: [u8; 32],
    len: usize,
}
impl SurfaceId {
    /// Initial conversation surface.
    pub const MAIN: Self = Self::literal("main");
    /// Conventional optional comparison surface; other valid keys are also supported.
    pub const COMPARE: Self = Self::literal("compare");
    const fn literal(value: &str) -> Self {
        let mut bytes = [0; 32];
        let mut index = 0;
        while index < value.len() {
            bytes[index] = value.as_bytes()[index];
            index += 1;
        }
        Self {
            bytes,
            len: value.len(),
        }
    }
    /// Validates an external key without allocating unbounded retained identity data.
    pub fn parse(value: &str) -> Result<Self, String> {
        if value.is_empty()
            || value.len() > 32
            || !value.bytes().all(|byte| {
                byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'_' | b'-')
            })
        {
            return Err("Surface identity must contain 1..=32 lowercase letters, digits, underscores or hyphens".into());
        }
        Ok(Self::literal(value))
    }
    /// Borrows the exact validated key.
    ///
    /// # Panics
    /// Panics if the private ASCII representation invariant is violated.
    pub fn as_str(&self) -> &str {
        std::str::from_utf8(&self.bytes[..self.len]).expect("validated ASCII surface identity")
    }
}
impl fmt::Debug for SurfaceId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.as_str().fmt(f)
    }
}
impl fmt::Display for SurfaceId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}
impl Serialize for SurfaceId {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}
impl<'de> Deserialize<'de> for SurfaceId {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Self::parse(&String::deserialize(deserializer)?).map_err(serde::de::Error::custom)
    }
}
