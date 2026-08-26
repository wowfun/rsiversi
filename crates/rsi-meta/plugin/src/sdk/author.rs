use super::{Activation, ProviderChannel};
use serde_json::Value;

/// One exact service dependency selected by a preparation attempt.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ServiceRequirement {
    pub(crate) key: String,
    pub(crate) contract: String,
    pub(crate) version: u64,
}

impl ServiceRequirement {
    pub fn new(key: impl Into<String>, contract: impl Into<String>, version: u64) -> Self {
        Self {
            key: key.into(),
            contract: contract.into(),
            version,
        }
    }

    pub fn key(&self) -> &str {
        &self.key
    }

    pub fn contract(&self) -> &str {
        &self.contract
    }

    pub const fn version(&self) -> u64 {
        self.version
    }
}

/// Single-use plugin state plus the normalized result of one preparation.
#[derive(Debug)]
pub struct Prepared<T> {
    pub(crate) normalized_config: Value,
    pub(crate) requirements: Vec<ServiceRequirement>,
    pub(crate) state: T,
    pub(crate) retained_bytes: u64,
}

impl<T> Prepared<T> {
    /// Declares the opaque state and its conservative retained-byte charge.
    ///
    /// Use zero only when `state` retains no bytes for the prepared attempt.
    pub fn new(normalized_config: Value, state: T, retained_bytes: u64) -> Self {
        Self {
            normalized_config,
            requirements: Vec::new(),
            state,
            retained_bytes,
        }
    }

    #[must_use]
    pub fn requiring(mut self, requirement: ServiceRequirement) -> Self {
        self.requirements.push(requirement);
        self
    }

    pub fn normalized_config(&self) -> &Value {
        &self.normalized_config
    }

    pub fn requirements(&self) -> &[ServiceRequirement] {
        &self.requirements
    }

    /// Returns the charge retained until this attempt is created or released.
    pub const fn retained_bytes(&self) -> u64 {
        self.retained_bytes
    }
}

/// Factory-side behavior exported through one ABI v2 plugin table.
pub trait NativePlugin: Default + Send + Sync + 'static {
    type Prepared: Send + 'static;
    type Instance: NativeInstance;

    fn identity(&self) -> Result<String, String>;
    fn prepare(&self, desired: &Value) -> Result<Prepared<Self::Prepared>, String>;
    fn create(&self, prepared: Self::Prepared) -> Result<Self::Instance, String>;
}

/// One created native instance. The SDK serializes these callbacks fail-fast.
pub trait NativeInstance: Send + 'static {
    fn activate(&mut self, activation: &mut Activation<'_>) -> Result<(), String>;
    fn serve(&mut self, port: &[u8], channel: &mut ProviderChannel<'_>) -> Result<(), String>;
}
