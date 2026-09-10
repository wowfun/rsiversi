//! Durable request-time sampling as an ordinary Agent contribution.

#![deny(unsafe_code)]
#![warn(missing_docs)]

use async_trait::async_trait;
use rsi_agent_composition_protocol::{
    ContextContributor, ContributionContext, ContributionError, ContributionInput,
    ContributionKind, ContributionOutput, ContributionRegistrarContract, ContributionRegistration,
    ContributionResult,
};
use rsi_agent_session_protocol::ContributionId;
use rsi_meta::{ActivationPlan, ConfigValue, MetaError, PluginFactory, PreparedActivation};
use std::{
    sync::Arc,
    time::{SystemTime, UNIX_EPOCH},
};
use time::{OffsetDateTime, format_description::well_known::Rfc3339};
use tokio_util::sync::CancellationToken;

/// Agent-only clock contribution; the default clock is sampled during execution.
#[derive(Clone, Copy, Debug)]
pub struct TimeContextFactory {
    clock: fn() -> SystemTime,
}

impl Default for TimeContextFactory {
    fn default() -> Self {
        Self::with_clock(SystemTime::now)
    }
}

impl TimeContextFactory {
    /// Selects a clock without sampling or acquiring any runtime authority.
    pub const fn with_clock(clock: fn() -> SystemTime) -> Self {
        Self { clock }
    }
}

#[derive(Debug)]
struct TimeContext(fn() -> SystemTime);

fn sample(clock: fn() -> SystemTime) -> ContributionResult<String> {
    let nanos = clock()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| {
            ContributionError::Invalid("time context clock predates the Unix epoch".into())
        })?
        .as_nanos();
    let nanos = i128::try_from(nanos)
        .map_err(|_| ContributionError::Invalid("time context clock is out of range".into()))?;
    let timestamp = OffsetDateTime::from_unix_timestamp_nanos(nanos)
        .map_err(|_| ContributionError::Invalid("time context clock is out of range".into()))?
        .format(&Rfc3339)
        .map_err(|_| ContributionError::Invalid("time context clock cannot be formatted".into()))?;
    Ok(format!(
        "Current time sampled before this model request: {timestamp} (UTC)."
    ))
}

#[async_trait]
impl ContextContributor for TimeContext {
    async fn contribute(
        &self,
        _: &ContributionContext,
        cancellation: CancellationToken,
    ) -> ContributionResult<ContributionOutput> {
        if cancellation.is_cancelled() {
            return Err(ContributionError::Closed);
        }
        Ok(ContributionOutput {
            inputs: vec![ContributionInput::context(sample(self.0)?)],
            domains: vec![],
        })
    }
}

#[async_trait]
impl PluginFactory for TimeContextFactory {
    fn prepare(&self, desired: &ConfigValue) -> rsi_meta::Result<PreparedActivation> {
        if !desired.is_null() {
            return Err(MetaError::InvalidInput(
                "time context configuration must be null".into(),
            ));
        }
        Ok(PreparedActivation::new(ConfigValue::Null)
            .requiring_local::<ContributionRegistrarContract>())
    }
    async fn activate(&self, plan: ActivationPlan) -> rsi_meta::Result<()> {
        let lease = plan
            .local::<ContributionRegistrarContract>()?
            .register(
                &plan.context().registration_context()?,
                ContributionRegistration::new(
                    ContributionId::new("rsi.time-context").expect("static contribution"),
                    10,
                    ContributionKind::Context(Arc::new(TimeContext(self.clock))),
                ),
            )
            .map_err(|error| MetaError::Activation(error.to_string()))?;
        plan.defer(
            "withdraw time context",
            Box::new(move || {
                Box::pin(async move {
                    drop(lease);
                    Ok(())
                })
            }),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn utc_sample_is_exact_and_rejects_an_invalid_clock() {
        assert_eq!(
            sample(|| UNIX_EPOCH + std::time::Duration::from_millis(1_000_123)).unwrap(),
            "Current time sampled before this model request: 1970-01-01T00:16:40.123Z (UTC)."
        );
        assert!(sample(|| UNIX_EPOCH - std::time::Duration::from_secs(1)).is_err());
    }
}
