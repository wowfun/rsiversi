//! Location identity and bounded target paths, without filesystem or access authority.
#![forbid(unsafe_code)]
#![warn(missing_docs)]
#![allow(clippy::missing_errors_doc)]

mod binding;
pub use binding::{ExecutionBinding, ExecutionPlanIdentity, ExecutionReview};

use serde::{Deserialize, Serialize};
use std::fmt;

/// Maximum explicit metadata locations: Local plus 256 target grant selections.
pub const MAXIMUM_EXPLICIT_LOCATIONS: usize = 257;

/// Bounded mechanical location filter, never execution or metadata access authority.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ExecutionLocations(Option<std::collections::BTreeSet<ExecutionLocation>>);
impl ExecutionLocations {
    /// Select every location; the product must independently authorize this scope.
    pub const fn all() -> Self {
        Self(None)
    }
    /// Select at most 257 distinct locations, including an empty selection.
    pub fn only(locations: std::collections::BTreeSet<ExecutionLocation>) -> Result<Self> {
        if locations.len() > MAXIMUM_EXPLICIT_LOCATIONS {
            return Err(CoordinateError::Locations);
        }
        Ok(Self(Some(locations)))
    }
    /// Returns whether the location participates in this mechanical query.
    pub fn contains(&self, location: &ExecutionLocation) -> bool {
        self.0
            .as_ref()
            .is_none_or(|locations| locations.contains(location))
    }
    /// Returns the explicit selection, or None for all locations.
    pub fn selected(&self) -> Option<&std::collections::BTreeSet<ExecutionLocation>> {
        self.0.as_ref()
    }
}

/// Stable identity assigned by the target owner, independent of connection epochs.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(transparent)]
pub struct ExecutionTargetId(String);

impl ExecutionTargetId {
    /// Validates a target identity before retaining externally decoded data.
    pub fn parse(value: impl Into<String>) -> Result<Self> {
        let value = value.into();
        if value.len() != 32
            || !value
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        {
            return Err(CoordinateError::Target);
        }
        Ok(Self(value))
    }

    /// Returns the stable registry identity, never a host or credential.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for ExecutionTargetId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for ExecutionTargetId {
    fn deserialize<D: serde::Deserializer<'de>>(
        deserializer: D,
    ) -> std::result::Result<Self, D::Error> {
        Self::parse(String::deserialize(deserializer)?).map_err(serde::de::Error::custom)
    }
}

/// Filesystem and execution location, without permission to use it.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum ExecutionLocation {
    /// The Service machine, regardless of the connecting client's platform.
    Local,
    /// One registry-owned SSH target; its connection is resolved separately.
    Ssh {
        /// Stable target identity.
        target: ExecutionTargetId,
    },
}

impl<'de> Deserialize<'de> for ExecutionLocation {
    fn deserialize<D: serde::Deserializer<'de>>(
        deserializer: D,
    ) -> std::result::Result<Self, D::Error> {
        // A tagged unit variant would silently accept additional object fields.
        #[derive(Deserialize)]
        #[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
        enum Wire {
            Local {},
            Ssh { target: ExecutionTargetId },
        }
        Ok(match Wire::deserialize(deserializer)? {
            Wire::Local {} => Self::Local,
            Wire::Ssh { target } => Self::Ssh { target },
        })
    }
}

/// A target-owned canonical directory spelling, without filesystem or grant authority.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
pub struct ExecutionCoordinates {
    location: ExecutionLocation,
    path: String,
}

impl ExecutionCoordinates {
    /// Validates coordinates already canonicalized by their owning filesystem.
    /// Lexical validation does not probe or authorize that filesystem.
    pub fn new(location: ExecutionLocation, path: impl Into<String>) -> Result<Self> {
        let path = path.into();
        if !rsi_workspace_path::is_normalized_absolute(&path)
            || (matches!(location, ExecutionLocation::Ssh { .. }) && !path.starts_with('/'))
        {
            return Err(CoordinateError::Path);
        }
        Ok(Self { location, path })
    }

    /// Returns the machine identity without resolving a provider.
    pub const fn location(&self) -> &ExecutionLocation {
        &self.location
    }

    /// Returns the canonical spelling in the target's path namespace.
    pub fn path(&self) -> &str {
        &self.path
    }
}

impl<'de> Deserialize<'de> for ExecutionCoordinates {
    fn deserialize<D: serde::Deserializer<'de>>(
        deserializer: D,
    ) -> std::result::Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Raw {
            location: ExecutionLocation,
            path: String,
        }
        let raw = Raw::deserialize(deserializer)?;
        Self::new(raw.location, raw.path).map_err(serde::de::Error::custom)
    }
}

/// Closed lexical input failures without echoing an external path or identity.
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum CoordinateError {
    /// Metadata query selection exceeds its bounded location count.
    #[error("execution location selection exceeds its bound")]
    Locations,
    /// Execution correlation metadata violates its location or generation invariants.
    #[error("invalid execution binding")]
    Binding,
    /// Target identity does not follow the registry identity grammar.
    #[error("invalid execution target identity")]
    Target,
    /// Target path is not bounded, normalized and absolute in its namespace.
    #[error("invalid execution location path")]
    Path,
}

/// Validated coordinate result.
pub type Result<T> = std::result::Result<T, CoordinateError>;

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn location_selections_bound_explicit_members_without_implying_authority() {
        let empty = ExecutionLocations::only(std::collections::BTreeSet::new()).unwrap();
        assert!(!empty.contains(&ExecutionLocation::Local));
        assert!(ExecutionLocations::all().contains(&ExecutionLocation::Local));
        let locations = (0..257)
            .map(|index| ExecutionLocation::Ssh {
                target: ExecutionTargetId::parse(format!("{index:032x}")).unwrap(),
            })
            .collect();
        let selected = ExecutionLocations::only(locations).unwrap();
        assert!(!selected.contains(&ExecutionLocation::Local));
        let mut locations = selected.selected().unwrap().clone();
        locations.insert(ExecutionLocation::Local);
        assert_eq!(
            ExecutionLocations::only(locations),
            Err(CoordinateError::Locations)
        );
    }

    #[test]
    fn decoding_validates_location_and_target_path_together_without_native_io() {
        let target = ExecutionTargetId::parse("a".repeat(32)).unwrap();
        let ssh = ExecutionLocation::Ssh { target };
        for path in [
            "relative",
            "/project/../outside",
            "/project/",
            "/project//child",
            "C:\\project",
            "/project\0",
        ] {
            assert!(
                ExecutionCoordinates::new(ssh.clone(), path).is_err(),
                "{path:?}"
            );
        }
        assert!(
            ExecutionCoordinates::new(ssh.clone(), format!("/{}", "x".repeat(16 * 1024))).is_err()
        );
        let remote = ExecutionCoordinates::new(ssh, "/not-a-local-directory/项目").unwrap();
        let encoded = serde_json::to_value(&remote).unwrap();
        assert_eq!(
            serde_json::from_value::<ExecutionCoordinates>(encoded.clone()).unwrap(),
            remote
        );
        let mut foreign = encoded;
        foreign["location"]["target"] = json!("B".repeat(32));
        assert!(serde_json::from_value::<ExecutionCoordinates>(foreign).is_err());
        for invalid in [
            json!({"location":{"kind":"local","target":"a".repeat(32)},"path":"/project"}),
            json!({"location":{"kind":"local"},"path":"/project","authority":true}),
            json!({"location":{"kind":"ssh","target":"a".repeat(32)},"path":"C:\\project"}),
        ] {
            assert!(serde_json::from_value::<ExecutionCoordinates>(invalid).is_err());
        }
        // A Linux reader treats a Windows Service's Local path as bounded text.
        assert!(ExecutionCoordinates::new(ExecutionLocation::Local, "C:\\project").is_ok());
    }

    #[test]
    fn equal_paths_at_distinct_locations_never_compare_equal() {
        let locations = [
            ExecutionLocation::Local,
            ExecutionLocation::Ssh {
                target: ExecutionTargetId::parse("a".repeat(32)).unwrap(),
            },
            ExecutionLocation::Ssh {
                target: ExecutionTargetId::parse("b".repeat(32)).unwrap(),
            },
        ];
        let coordinates: std::collections::BTreeSet<_> = locations
            .into_iter()
            .map(|location| ExecutionCoordinates::new(location, "/project").unwrap())
            .collect();
        assert_eq!(coordinates.len(), 3);
    }
}
