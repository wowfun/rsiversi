#![allow(clippy::wildcard_imports)] // This is one implementation partition of runtime.

use super::*;
use crate::Requirement;
use crate::plugin::{LocalRequirement, PreparedState};
use serde::Serialize;
use serde_json::Value;

impl Runtime {
    pub(super) fn normalize_config(
        factory: &preparation::RetainedFactory,
        desired: &RetainedConfig,
        limits: &PayloadLimits,
    ) -> Result<NormalizedConfig> {
        // Preparation is attempt-local. Always start from the immutable raw
        // desired value whose retained wrapper proves boundary validation; a
        // previous attempt's normalized output is not a valid input to a
        // later preparation.
        let prepared = match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            factory.prepare(desired.as_value())
        })) {
            Ok(prepared) => prepared?,
            Err(payload) => {
                drop_catching_unwind(payload);
                return Err(MetaError::InvalidConfig(
                    "plugin preparation panicked".to_owned(),
                ));
            }
        };
        let (config, requirements, local_requirements, state) = prepared.into_parts();
        let config = OwnedJsonValue::new(config);
        let encoded_bytes = Self::validate_config(config.as_value(), limits)?;
        Ok(NormalizedConfig {
            value: config,
            encoded_bytes,
            requirements,
            local_requirements,
            state,
        })
    }

    pub(super) fn validate_config(config: &ConfigValue, limits: &PayloadLimits) -> Result<usize> {
        validate_json_shape(config, limits.maximum_json_depth, limits.maximum_json_nodes)
            .map_err(MetaError::InvalidConfig)?;
        encoded_json_size_bounded(config, limits.maximum_config_bytes)
            .map_err(|error| MetaError::InvalidConfig(error.to_string()))
    }
}

pub(super) struct OwnedJsonValue {
    value: Option<Value>,
}

impl OwnedJsonValue {
    pub(super) fn new(value: Value) -> Self {
        Self { value: Some(value) }
    }

    pub(super) fn as_value(&self) -> &Value {
        self.value
            .as_ref()
            .expect("owned JSON value remains available until consumed")
    }

    pub(super) fn into_inner(mut self) -> Value {
        self.value
            .take()
            .expect("owned JSON value can only be consumed once")
    }
}

impl Drop for OwnedJsonValue {
    fn drop(&mut self) {
        if let Some(value) = self.value.take() {
            drop_json_value_iteratively(value);
        }
    }
}

fn drop_json_value_iteratively(value: Value) {
    enum Children {
        Array(std::vec::IntoIter<Value>),
        Object(serde_json::map::IntoIter),
    }

    impl Children {
        fn next(&mut self) -> Option<Value> {
            match self {
                Self::Array(values) => values.next(),
                Self::Object(values) => values.next().map(|(_, value)| value),
            }
        }
    }

    let mut current = Some(value);
    let mut parents = Vec::<Children>::new();
    loop {
        if let Some(value) = current.take() {
            match value {
                Value::Array(values) => parents.push(Children::Array(values.into_iter())),
                Value::Object(values) => parents.push(Children::Object(values.into_iter())),
                Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => {}
            }
        }

        let Some(children) = parents.last_mut() else {
            break;
        };
        if let Some(value) = children.next() {
            current = Some(value);
        } else {
            parents.pop();
        }
    }
}

pub(super) struct NormalizedConfig {
    pub(super) value: OwnedJsonValue,
    pub(super) encoded_bytes: usize,
    pub(super) requirements: Vec<Requirement>,
    pub(super) local_requirements: Vec<LocalRequirement>,
    pub(super) state: Option<PreparedState>,
}

fn validate_json_shape(
    value: &Value,
    maximum_depth: usize,
    maximum_nodes: usize,
) -> std::result::Result<(), String> {
    enum Pending<'value> {
        Value(&'value Value, usize),
        Array(std::slice::Iter<'value, Value>, usize),
        Object(serde_json::map::Values<'value>, usize),
    }

    let mut pending = vec![Pending::Value(value, 1_usize)];
    let mut nodes = 0_usize;
    while let Some(next) = pending.pop() {
        match next {
            Pending::Value(value, depth) => {
                if depth > maximum_depth {
                    return Err(format!(
                        "JSON nesting exceeds the configured depth limit of {maximum_depth}"
                    ));
                }
                nodes = nodes
                    .checked_add(1)
                    .ok_or_else(|| "JSON node count overflowed".to_owned())?;
                if nodes > maximum_nodes {
                    return Err(format!(
                        "JSON value exceeds the configured node limit of {maximum_nodes}"
                    ));
                }
                match value {
                    Value::Array(values) if !values.is_empty() => {
                        let child_depth = depth
                            .checked_add(1)
                            .ok_or_else(|| "JSON nesting depth overflowed".to_owned())?;
                        pending.push(Pending::Array(values.iter(), child_depth));
                    }
                    Value::Object(values) if !values.is_empty() => {
                        let child_depth = depth
                            .checked_add(1)
                            .ok_or_else(|| "JSON nesting depth overflowed".to_owned())?;
                        pending.push(Pending::Object(values.values(), child_depth));
                    }
                    Value::Null
                    | Value::Bool(_)
                    | Value::Number(_)
                    | Value::String(_)
                    | Value::Array(_)
                    | Value::Object(_) => {}
                }
            }
            Pending::Array(mut values, depth) => {
                if let Some(value) = values.next() {
                    pending.push(Pending::Array(values, depth));
                    pending.push(Pending::Value(value, depth));
                }
            }
            Pending::Object(mut values, depth) => {
                if let Some(value) = values.next() {
                    pending.push(Pending::Object(values, depth));
                    pending.push(Pending::Value(value, depth));
                }
            }
        }
    }
    Ok(())
}

pub(super) fn encoded_json_size_bounded<T: Serialize + ?Sized>(
    value: &T,
    maximum: usize,
) -> serde_json::Result<usize> {
    struct Counter {
        bytes: usize,
        maximum: usize,
    }

    impl std::io::Write for Counter {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            let next = self
                .bytes
                .checked_add(bytes.len())
                .ok_or_else(|| std::io::Error::other("JSON size overflowed"))?;
            if next > self.maximum {
                return Err(std::io::Error::other(format!(
                    "JSON encoding exceeds the configured {}-byte limit",
                    self.maximum
                )));
            }
            self.bytes = next;
            Ok(bytes.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    let mut counter = Counter { bytes: 0, maximum };
    serde_json::to_writer(&mut counter, value)?;
    Ok(counter.bytes)
}
