//! Closed, product-neutral observations of serialized Profile commands.
use super::{Controller, ProfileError, ReloadOutcome, Result};

/// Admission source of an executed command.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProfileAttemptOrigin {
    /// Explicit control reload.
    Manual,
    /// Coalesced source watcher notification.
    Watcher,
    /// Composition owner's input replacement.
    InputReplacement,
}

/// Terminal result independent of the currently observed graph health.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProfileAttemptOutcome {
    /// Candidate applied.
    Applied,
    /// Candidate was semantically equal.
    Unchanged,
    /// Candidate requires a restart.
    RestartRequired,
    /// Prior target reconstructed.
    RolledBack,
    /// Application and compensation failed.
    Degraded,
    /// Input admission failed in command order.
    Rejected,
    /// Preflight failed before graph mutation.
    Failed,
}

/// Safe operational failure category; contains no paths or executable messages.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProfileFailureKind {
    /// Source read or watch capture failed.
    Source,
    /// Profile compilation failed.
    Compile,
    /// Factory resolution failed.
    Resolve,
    /// Context or nominal contract binding failed.
    Bind,
    /// Factory preparation failed.
    Prepare,
    /// Factory application or lifecycle failed.
    Apply,
    /// Prior generation cleanup failed.
    Retire,
    /// Expected input revision was stale.
    InputConflict,
    /// Input could not preserve the running environment.
    IncompatibleInput,
    /// An explicit capacity bound was reached.
    Capacity,
    /// Controller no longer available.
    Stopped,
}

impl ProfileFailureKind {
    pub(super) fn from_error(error: &ProfileError) -> Self {
        match error {
            ProfileError::InputConflict { .. } => Self::InputConflict,
            ProfileError::IncompatibleInput(_) => Self::IncompatibleInput,
            ProfileError::Source { .. } => Self::Source,
            ProfileError::UnknownPlugin { .. } => Self::Resolve,
            ProfileError::UnknownLocalContract { .. } | ProfileError::UnknownLocalEvent { .. } => {
                Self::Bind
            }
            ProfileError::Meta(error) => match error {
                rsi_meta::MetaError::CapacityExhausted { .. }
                | rsi_meta::MetaError::Busy { .. }
                | rsi_meta::MetaError::PayloadTooLarge { .. } => Self::Capacity,
                rsi_meta::MetaError::RuntimeShuttingDown
                | rsi_meta::MetaError::RuntimeTerminal(_)
                | rsi_meta::MetaError::FiberDisposed { .. }
                | rsi_meta::MetaError::StaleContext { .. }
                | rsi_meta::MetaError::Cancelled => Self::Stopped,
                _ => Self::Bind,
            },
            ProfileError::Preparation { .. } => Self::Prepare,
            ProfileError::Application { .. }
            | ProfileError::UnexpectedDisposal { .. }
            | ProfileError::GenerationPending { .. } => Self::Apply,
            ProfileError::CapacityExceeded { .. } | ProfileError::Busy => Self::Capacity,
            ProfileError::Stopped => Self::Stopped,
            _ => Self::Compile,
        }
    }
}

/// Last completed command; exclusively published by the command worker.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProfileAttempt {
    /// Monotonic completed-command identity, independent of graph revision.
    pub sequence: u64,
    /// Executed command source.
    pub origin: ProfileAttemptOrigin,
    /// Closed terminal outcome.
    pub outcome: ProfileAttemptOutcome,
    /// Primary failure, if any.
    pub failure: Option<ProfileFailureKind>,
    /// Compensation failure, only when compensation failed.
    pub rollback_failure: Option<ProfileFailureKind>,
}

pub(super) struct ConvergenceFailure {
    pub kind: ProfileFailureKind,
    pub message: String,
}
impl From<ProfileError> for ConvergenceFailure {
    fn from(error: ProfileError) -> Self {
        Self {
            kind: ProfileFailureKind::from_error(&error),
            message: error.to_string(),
        }
    }
}

impl Controller {
    pub(super) fn finish_attempt(
        &self,
        origin: ProfileAttemptOrigin,
        result: &mut Result<ReloadOutcome>,
    ) {
        use ProfileAttemptOutcome as O;
        let (outcome, failure, rollback_failure) = match result.as_ref() {
            Ok(ReloadOutcome::Applied(_)) => (O::Applied, None, None),
            Ok(ReloadOutcome::Unchanged(_)) => (O::Unchanged, None, None),
            Ok(ReloadOutcome::RestartRequired(_)) => (O::RestartRequired, None, None),
            Ok(ReloadOutcome::RolledBack { error_kind, .. }) => {
                (O::RolledBack, Some(*error_kind), None)
            }
            Ok(ReloadOutcome::Degraded {
                error_kind,
                rollback_error_kind,
                ..
            }) => (O::Degraded, Some(*error_kind), Some(*rollback_error_kind)),
            Err(error) => (
                if matches!(
                    error,
                    ProfileError::InputConflict { .. } | ProfileError::IncompatibleInput(_)
                ) {
                    O::Rejected
                } else {
                    O::Failed
                },
                Some(ProfileFailureKind::from_error(error)),
                None,
            ),
        };
        let mut state = self.state.lock().expect("Profile state poisoned");
        let Some(state) = state.as_mut() else { return };
        let sequence = state.last_attempt.as_ref().map_or(1, |attempt| {
            attempt
                .sequence
                .checked_add(1)
                .expect("Profile attempt identity exhausted")
        });
        state.last_attempt = Some(ProfileAttempt {
            sequence,
            origin,
            outcome,
            failure,
            rollback_failure,
        });
        let status = self.publish_locked_state(state);
        if let Ok(outcome) = result {
            match outcome {
                ReloadOutcome::Applied(value)
                | ReloadOutcome::Unchanged(value)
                | ReloadOutcome::RestartRequired(value)
                | ReloadOutcome::RolledBack { status: value, .. }
                | ReloadOutcome::Degraded { status: value, .. } => *value = status,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn binding_retains_meta_capacity_and_shutdown_categories() {
        use rsi_meta::MetaError as M;
        for (error, expected) in [
            (
                M::CapacityExhausted {
                    resource: "bindings",
                },
                ProfileFailureKind::Capacity,
            ),
            (M::Busy { operation: "bind" }, ProfileFailureKind::Capacity),
            (
                M::PayloadTooLarge { maximum: 32 },
                ProfileFailureKind::Capacity,
            ),
            (M::RuntimeShuttingDown, ProfileFailureKind::Stopped),
            (
                M::RuntimeTerminal("failed".into()),
                ProfileFailureKind::Stopped,
            ),
            (M::Cancelled, ProfileFailureKind::Stopped),
            (
                M::InvalidInput("invalid binding".into()),
                ProfileFailureKind::Bind,
            ),
        ] {
            assert_eq!(
                ProfileFailureKind::from_error(&ProfileError::Meta(error)),
                expected
            );
        }
    }
}
