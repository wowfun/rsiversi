use super::{ConfigValue, LocalContract, LocalContractKey, Requirement};
use crate::{MetaError, Result};
use std::any::{Any, TypeId, type_name};
use std::fmt;

type OpaqueState = Box<dyn Any + Send + 'static>;

pub(crate) struct PreparedState {
    value: Option<OpaqueState>,
    retained_bytes: usize,
}

impl PreparedState {
    pub(super) fn new<T>(value: T, retained_bytes: usize) -> Self
    where
        T: Send + 'static,
    {
        Self {
            value: Some(Box::new(value)),
            retained_bytes,
        }
    }

    pub(crate) fn retained_bytes(&self) -> usize {
        self.retained_bytes
    }

    #[cfg(test)]
    pub(crate) fn new_for_test<T>(value: T) -> Self
    where
        T: Send + 'static,
    {
        Self::new(value, 0)
    }

    pub(super) fn take<T>(&mut self) -> Result<T>
    where
        T: Send + 'static,
    {
        let value = self
            .value
            .take()
            .ok_or(MetaError::PreparedStateUnavailable)?;
        match value.downcast::<T>() {
            Ok(value) => Ok(*value),
            Err(value) => {
                self.value = Some(value);
                Err(MetaError::PreparedStateTypeMismatch {
                    expected: type_name::<T>(),
                })
            }
        }
    }
}

impl Drop for PreparedState {
    fn drop(&mut self) {
        if let Some(value) = self.value.take() {
            crate::runtime::drop_catching_unwind(value);
        }
    }
}

/// Bounded configuration, exact requirements, and optional opaque state for
/// one activation attempt.
pub struct PreparedActivation {
    config: ConfigValue,
    requirements: Vec<Requirement>,
    local_requirements: Vec<LocalRequirement>,
    state: Option<PreparedState>,
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(crate) struct LocalRequirement {
    pub(crate) contract: TypeId,
    pub(crate) key: LocalContractKey,
}

impl fmt::Debug for PreparedActivation {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PreparedActivation")
            .field("config", &"<redacted>")
            .field("requirements", &self.requirements)
            .field("local_requirements", &self.local_requirements)
            .field("state", &self.state.as_ref().map(|_| "<redacted>"))
            .finish()
    }
}

impl PreparedActivation {
    /// Creates a prepared activation with no requirements or opaque state.
    pub fn new(config: ConfigValue) -> Self {
        Self {
            config,
            requirements: Vec::new(),
            local_requirements: Vec::new(),
            state: None,
        }
    }

    /// Creates a prepared activation owning one opaque attempt-local value.
    ///
    /// `retained_bytes` is a trusted safe-Rust factory declaration. It must
    /// include all memory retained solely by `state` and is checked against the
    /// Runtime's per-attempt prepared-state bound before activation. Core keeps
    /// that charge until the attempt retires, including after activation takes
    /// the value, because the plugin may move it into generation-owned state.
    pub fn with_state<T>(config: ConfigValue, state: T, retained_bytes: usize) -> Self
    where
        T: Send + 'static,
    {
        Self {
            config,
            requirements: Vec::new(),
            local_requirements: Vec::new(),
            state: Some(PreparedState::new(state, retained_bytes)),
        }
    }

    /// Appends one exact service requirement for this activation.
    #[must_use]
    pub fn requiring(mut self, requirement: Requirement) -> Self {
        self.requirements.push(requirement);
        self
    }

    /// Appends one exact nominal safe-Rust Local service requirement.
    #[must_use]
    pub fn requiring_local<C: LocalContract>(mut self) -> Self {
        self.local_requirements.push(LocalRequirement {
            contract: TypeId::of::<C>(),
            key: LocalContractKey::new(C::KEY),
        });
        self
    }

    /// Borrows the normalized configuration.
    pub fn config(&self) -> &ConfigValue {
        &self.config
    }

    /// Borrows the exact requirements selected by this attempt.
    pub fn requirements(&self) -> &[Requirement] {
        &self.requirements
    }

    pub(crate) fn into_parts(
        self,
    ) -> (
        ConfigValue,
        Vec<Requirement>,
        Vec<LocalRequirement>,
        Option<PreparedState>,
    ) {
        (
            self.config,
            self.requirements,
            self.local_requirements,
            self.state,
        )
    }
}
