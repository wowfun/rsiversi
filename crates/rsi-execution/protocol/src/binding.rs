use super::{CoordinateError, ExecutionCoordinates, ExecutionLocation, Result};
use rsi_api_protocol::HostEpoch;
use serde::{Deserialize, Serialize};

/// Exact immutable display/correlation metadata; it cannot recreate a lease.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(try_from = "BindingWire")]
pub struct ExecutionBinding {
    host_epoch: HostEpoch,
    location: ExecutionLocation,
    target_revision: u64,
    connection_epoch: u64,
    provider_generation: u64,
    lease_generation: u64,
}
impl ExecutionBinding {
    /// Returns the issuing Host lifetime, distinct across Service restarts.
    pub const fn host_epoch(&self) -> &HostEpoch {
        &self.host_epoch
    }
    /// Returns the selected location without interpreting its path namespace.
    pub const fn location(&self) -> &ExecutionLocation {
        &self.location
    }
    /// Returns the exact target configuration revision; Local uses zero.
    pub const fn target_revision(&self) -> u64 {
        self.target_revision
    }
    /// Returns the pinned connection epoch; Local uses zero.
    pub const fn connection_epoch(&self) -> u64 {
        self.connection_epoch
    }
    /// Returns the process-local backend generation.
    pub const fn provider_generation(&self) -> u64 {
        self.provider_generation
    }
    /// Returns this admission owner's process-local lease generation.
    pub const fn lease_generation(&self) -> u64 {
        self.lease_generation
    }
}

/// Immutable prepared-plan correlation for approval evidence, not execution authority.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(try_from = "PlanWire")]
pub struct ExecutionPlanIdentity {
    binding: ExecutionBinding,
    sequence: u64,
}
impl ExecutionPlanIdentity {
    /// Returns the exact provider, target and admission-generation binding.
    pub const fn binding(&self) -> &ExecutionBinding {
        &self.binding
    }
    /// Returns the plan's unique ordinal in this lease.
    pub const fn sequence(&self) -> u64 {
        self.sequence
    }
}

impl ExecutionBinding {
    /// Validates non-authorizing correlation metadata before retaining it.
    pub fn new(
        host_epoch: HostEpoch,
        location: ExecutionLocation,
        target_revision: u64,
        connection_epoch: u64,
        provider_generation: u64,
        lease_generation: u64,
    ) -> Result<Self> {
        let valid = match location {
            ExecutionLocation::Local => target_revision == 0 && connection_epoch == 0,
            ExecutionLocation::Ssh { .. } => target_revision != 0 && connection_epoch != 0,
        };
        if !valid || provider_generation == 0 || lease_generation == 0 {
            return Err(CoordinateError::Binding);
        }
        Ok(Self {
            host_epoch,
            location,
            target_revision,
            connection_epoch,
            provider_generation,
            lease_generation,
        })
    }
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct BindingWire {
    host_epoch: HostEpoch,
    location: ExecutionLocation,
    target_revision: u64,
    connection_epoch: u64,
    provider_generation: u64,
    lease_generation: u64,
}
impl TryFrom<BindingWire> for ExecutionBinding {
    type Error = CoordinateError;
    fn try_from(wire: BindingWire) -> Result<Self> {
        Self::new(
            wire.host_epoch,
            wire.location,
            wire.target_revision,
            wire.connection_epoch,
            wire.provider_generation,
            wire.lease_generation,
        )
    }
}
impl ExecutionPlanIdentity {
    /// Binds a positive plan ordinal to its exact lease metadata.
    pub fn new(binding: ExecutionBinding, sequence: u64) -> Result<Self> {
        if sequence == 0 {
            return Err(CoordinateError::Binding);
        }
        Ok(Self { binding, sequence })
    }
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PlanWire {
    binding: ExecutionBinding,
    sequence: u64,
}
impl TryFrom<PlanWire> for ExecutionPlanIdentity {
    type Error = CoordinateError;
    fn try_from(wire: PlanWire) -> Result<Self> {
        Self::new(wire.binding, wire.sequence)
    }
}

/// Exact location and optional prepared process metadata for a review, never authority.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(try_from = "ReviewWire")]
pub struct ExecutionReview {
    binding: ExecutionBinding,
    plan_sequence: Option<std::num::NonZeroU64>,
    workspace: String,
}
impl ExecutionReview {
    /// Creates bounded review metadata for the same lease and target workspace.
    pub fn new(
        binding: ExecutionBinding,
        plan_sequence: Option<std::num::NonZeroU64>,
        workspace: String,
    ) -> Result<Self> {
        ExecutionCoordinates::new(binding.location.clone(), workspace.clone())?;
        Ok(Self {
            binding,
            plan_sequence,
            workspace,
        })
    }
    /// Returns the exact issuing lease metadata.
    pub const fn binding(&self) -> &ExecutionBinding {
        &self.binding
    }
    /// Returns the prepared plan ordinal when this effect prepares a process.
    pub const fn plan_sequence(&self) -> Option<std::num::NonZeroU64> {
        self.plan_sequence
    }
    /// Returns the reviewed workspace boundary in the selected target namespace.
    pub fn workspace(&self) -> &str {
        &self.workspace
    }
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ReviewWire {
    binding: ExecutionBinding,
    plan_sequence: Option<std::num::NonZeroU64>,
    workspace: String,
}
impl TryFrom<ReviewWire> for ExecutionReview {
    type Error = CoordinateError;
    fn try_from(wire: ReviewWire) -> Result<Self> {
        Self::new(wire.binding, wire.plan_sequence, wire.workspace)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn decoded_reviews_preserve_location_and_reject_forged_generation_shapes() {
        let binding = ExecutionBinding::new(
            HostEpoch::from_bytes([1; 16]),
            ExecutionLocation::Local,
            0,
            0,
            1,
            2,
        )
        .unwrap();
        let review =
            ExecutionReview::new(binding, std::num::NonZeroU64::new(3), "/workspace".into())
                .unwrap();
        let value = serde_json::to_value(&review).unwrap();
        assert_eq!(
            serde_json::from_value::<ExecutionReview>(value.clone()).unwrap(),
            review
        );
        for (path, invalid) in [
            ("target_revision", json!(1)),
            ("connection_epoch", json!(1)),
            ("provider_generation", json!(0)),
            ("lease_generation", json!(0)),
            ("authority", json!(true)),
        ] {
            let mut changed = value.clone();
            changed["binding"][path] = invalid;
            assert!(serde_json::from_value::<ExecutionReview>(changed).is_err());
        }
        let mut changed = value.clone();
        changed["plan_sequence"] = json!(0);
        assert!(serde_json::from_value::<ExecutionReview>(changed).is_err());
        let mut changed = value.clone();
        changed["workspace"] = json!("relative");
        assert!(serde_json::from_value::<ExecutionReview>(changed).is_err());
        let mut changed = value;
        changed["binding"]["location"] = json!({"kind":"ssh","target":"a".repeat(32)});
        assert!(serde_json::from_value::<ExecutionReview>(changed.clone()).is_err());
        changed["binding"]["target_revision"] = json!(1);
        changed["binding"]["connection_epoch"] = json!(1);
        assert_ne!(
            serde_json::from_value::<ExecutionReview>(changed).unwrap(),
            review
        );
    }
}
