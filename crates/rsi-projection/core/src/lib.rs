//! Disposable named projection registry plugin.

#![deny(unsafe_code)]
#![warn(missing_docs)]
#![allow(clippy::missing_errors_doc)]

use rsi_meta::{
    ActivationPlan, ConfigValue, LocalContract, MetaError, PluginFactory, PreparedActivation,
};
use serde_json::Value;
use std::collections::BTreeMap;
use std::fmt;
use std::sync::{Arc, Mutex, Weak};
use thiserror::Error;

const MAXIMUM_PROJECTION_BYTES: usize = 16 * 1024 * 1024;
const MAXIMUM_PROJECTION_UNITS: usize = 256;

/// Pure projection unit.
pub trait ProjectionUnit: fmt::Debug + Send + Sync + 'static {
    /// Derives one bounded value from the immutable input snapshot.
    fn project(&self, input: &Value) -> Result<Value>;
}

/// Exact-name projection registry.
pub trait ProjectionRegistry: fmt::Debug + Send + Sync + 'static {
    /// Registers one unit until the returned lease drops.
    fn register(&self, name: &str, unit: Arc<dyn ProjectionUnit>) -> Result<ProjectionLease>;
    /// Projects through a stable snapshot of all active units.
    fn project_all(&self, input: &Value) -> Result<BTreeMap<String, Value>>;
}

/// Nominal Local contract for [`ProjectionRegistry`].
#[derive(Debug)]
pub struct ProjectionRegistryContract;

impl LocalContract for ProjectionRegistryContract {
    const KEY: &'static str = "rsi.projection";
    type Service = dyn ProjectionRegistry;
}

/// Closed projection failure taxonomy.
#[derive(Clone, Debug, Error, Eq, PartialEq)]
pub enum ProjectionError {
    /// Malformed or out-of-bounds input/output.
    #[error("invalid projection value: {0}")]
    InvalidInput(String),
    /// Duplicate exact name.
    #[error("projection `{0}` is already registered")]
    Duplicate(String),
    /// Unit failure.
    #[error("projection `{name}` failed: {message}")]
    Unit {
        /// Exact unit name.
        name: String,
        /// Bounded failure message.
        message: String,
    },
}

/// Projection result.
pub type Result<T> = std::result::Result<T, ProjectionError>;

/// Opaque projection registration lease.
pub struct ProjectionLease {
    cleanup: Option<Box<dyn FnOnce() + Send + Sync + 'static>>,
}

impl fmt::Debug for ProjectionLease {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("ProjectionLease(..)")
    }
}

impl Drop for ProjectionLease {
    fn drop(&mut self) {
        if let Some(cleanup) = self.cleanup.take() {
            cleanup();
        }
    }
}

#[derive(Debug)]
struct Registry {
    state: Arc<State>,
}

#[derive(Debug)]
struct State {
    units: Mutex<BTreeMap<String, Arc<dyn ProjectionUnit>>>,
}

impl ProjectionRegistry for Registry {
    fn register(&self, name: &str, unit: Arc<dyn ProjectionUnit>) -> Result<ProjectionLease> {
        validate_name(name)?;
        let mut units = self
            .state
            .units
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if units.contains_key(name) {
            return Err(ProjectionError::Duplicate(name.into()));
        }
        if units.len() >= MAXIMUM_PROJECTION_UNITS {
            return Err(ProjectionError::InvalidInput(format!(
                "projection registry reached its {MAXIMUM_PROJECTION_UNITS}-unit bound"
            )));
        }
        units.insert(name.into(), unit);
        let state = Arc::downgrade(&self.state);
        let name = name.to_owned();
        Ok(ProjectionLease {
            cleanup: Some(Box::new(move || remove(&state, &name))),
        })
    }

    fn project_all(&self, input: &Value) -> Result<BTreeMap<String, Value>> {
        let _input_bytes = validate_value(input)?;
        let units = self
            .state
            .units
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .iter()
            .map(|(name, unit)| (name.clone(), Arc::clone(unit)))
            .collect::<Vec<_>>();
        let mut output = BTreeMap::new();
        let mut output_bytes = 2_usize;
        for (name, unit) in units {
            let value = unit.project(input).map_err(|error| ProjectionError::Unit {
                name: name.clone(),
                message: error.to_string(),
            })?;
            let value_bytes = validate_value(&value)?;
            let name_bytes = serde_json::to_vec(&name)
                .map_err(|error| ProjectionError::InvalidInput(error.to_string()))?
                .len();
            output_bytes = output_bytes
                .checked_add(usize::from(!output.is_empty()))
                .and_then(|bytes| bytes.checked_add(name_bytes))
                .and_then(|bytes| bytes.checked_add(1))
                .and_then(|bytes| bytes.checked_add(value_bytes))
                .ok_or_else(|| {
                    ProjectionError::InvalidInput("projection bytes overflowed".into())
                })?;
            if output_bytes > MAXIMUM_PROJECTION_BYTES {
                return Err(ProjectionError::InvalidInput(
                    "complete projection output is too large".into(),
                ));
            }
            output.insert(name, value);
        }
        Ok(output)
    }
}

fn remove(state: &Weak<State>, name: &str) {
    if let Some(state) = state.upgrade() {
        state
            .units
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(name);
    }
}

fn validate_name(name: &str) -> Result<()> {
    if name.is_empty() || name.len() > 256 {
        return Err(ProjectionError::InvalidInput(
            "projection name is empty or too large".into(),
        ));
    }
    Ok(())
}

fn validate_value(value: &Value) -> Result<usize> {
    let bytes = serde_json::to_vec(value)
        .map_err(|error| ProjectionError::InvalidInput(error.to_string()))?;
    if bytes.len() > MAXIMUM_PROJECTION_BYTES {
        return Err(ProjectionError::InvalidInput(
            "projection value is too large".into(),
        ));
    }
    Ok(bytes.len())
}

/// Ordinary factory for one Projection registry generation.
#[derive(Clone, Debug, Default)]
pub struct ProjectionFactory;

#[async_trait::async_trait]
impl PluginFactory for ProjectionFactory {
    fn prepare(&self, desired: &ConfigValue) -> rsi_meta::Result<PreparedActivation> {
        if !desired.is_null() && !desired.as_object().is_some_and(serde_json::Map::is_empty) {
            return Err(MetaError::InvalidInput(
                "Projection configuration must be null or empty".into(),
            ));
        }
        Ok(PreparedActivation::new(Value::Null))
    }

    async fn activate(&self, plan: ActivationPlan) -> rsi_meta::Result<()> {
        let registry: Arc<dyn ProjectionRegistry> = Arc::new(Registry {
            state: Arc::new(State {
                units: Mutex::new(BTreeMap::new()),
            }),
        });
        let supply = plan
            .context()
            .provide_local::<ProjectionRegistryContract>(registry)?;
        plan.defer(
            "withdraw Projection registry",
            Box::new(move || {
                Box::pin(async move {
                    drop(supply);
                    Ok(())
                })
            }),
        )
    }
}
