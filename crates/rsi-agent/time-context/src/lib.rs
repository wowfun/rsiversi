//! Durable request-time sampling as an ordinary Agent contribution.

#![deny(unsafe_code)]
#![warn(missing_docs)]

use async_trait::async_trait;
use rsi_agent_composition_protocol::{
    BeforeStepContext, BeforeStepContributor, BeforeStepOutput, ContributionError,
    ContributionInput, ContributionKind, ContributionOutput, ContributionRegistrarContract,
    ContributionRegistration, ContributionResult,
};
use rsi_agent_session_protocol::ContributionId;
use rsi_meta::{ActivationPlan, ConfigValue, MetaError, PluginFactory, PreparedActivation};
use serde::{Deserialize, Serialize};
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
struct TimeContext {
    clock: fn() -> SystemTime,
    config: Config,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
struct Config {
    refresh_interval_ms: u64,
}
impl Default for Config {
    fn default() -> Self {
        Self {
            refresh_interval_ms: 600_000,
        }
    }
}

impl TimeContext {
    fn contribution(&self, previous: Option<u64>) -> ContributionResult<ContributionOutput> {
        let elapsed = (self.clock)().duration_since(UNIX_EPOCH).map_err(|_| {
            ContributionError::Invalid("time context clock predates the Unix epoch".into())
        })?;
        let nanos = i128::try_from(elapsed.as_nanos())
            .map_err(|_| ContributionError::Invalid("time context clock is out of range".into()))?;
        let timestamp = OffsetDateTime::from_unix_timestamp_nanos(nanos)
            .map_err(|_| ContributionError::Invalid("time context clock is out of range".into()))?;
        if previous.is_some_and(|last| {
            elapsed
                .as_millis()
                .checked_sub(u128::from(last))
                .is_some_and(|age| age < u128::from(self.config.refresh_interval_ms))
        }) {
            return Ok(ContributionOutput::default());
        }
        let timestamp = timestamp.format(&Rfc3339).map_err(|_| {
            ContributionError::Invalid("time context clock cannot be formatted".into())
        })?;
        Ok(ContributionOutput {
            inputs: vec![ContributionInput::context(format!(
                "Current time sampled before this model request: {timestamp} (UTC)."
            ))],
            domains: vec![],
        })
    }
}

#[async_trait]
impl BeforeStepContributor for TimeContext {
    async fn prepare(
        &self,
        context: &BeforeStepContext<'_>,
        cancellation: CancellationToken,
    ) -> ContributionResult<BeforeStepOutput> {
        if cancellation.is_cancelled() {
            return Err(ContributionError::Closed);
        }
        Ok(BeforeStepOutput {
            contribution: self.contribution(context.previous_input_at_ms)?,
            ..BeforeStepOutput::default()
        })
    }
}

#[async_trait]
impl PluginFactory for TimeContextFactory {
    fn prepare(&self, desired: &ConfigValue) -> rsi_meta::Result<PreparedActivation> {
        let config: Config = if desired.is_null() {
            Config::default()
        } else {
            serde_json::from_value(desired.clone())
                .map_err(|error| MetaError::InvalidInput(error.to_string()))?
        };
        let normalized = serde_json::to_value(&config)
            .map_err(|error| MetaError::InvalidInput(error.to_string()))?;
        Ok(
            PreparedActivation::with_state(normalized, config, std::mem::size_of::<Config>())
                .requiring_local::<ContributionRegistrarContract>(),
        )
    }
    async fn activate(&self, mut plan: ActivationPlan) -> rsi_meta::Result<()> {
        let config = plan.take_state::<Config>()?;
        let lease = plan
            .local::<ContributionRegistrarContract>()?
            .register(
                &plan.context().registration_context()?,
                ContributionRegistration::new(
                    ContributionId::new("rsi.time-context").expect("static contribution"),
                    10,
                    ContributionKind::BeforeStep(Arc::new(TimeContext {
                        clock: self.clock,
                        config,
                    })),
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
    fn plugin(interval: u64) -> TimeContext {
        TimeContext {
            clock: || UNIX_EPOCH + std::time::Duration::from_millis(1_000_123),
            config: Config {
                refresh_interval_ms: interval,
            },
        }
    }

    #[test]
    fn utc_sample_is_exact_and_rejects_an_invalid_clock() {
        let mut batch = rsi_agent_composition_protocol::ContributionBatch::default();
        batch
            .append(
                &ContributionId::new("rsi.time-context").unwrap(),
                &rsi_agent_session_protocol::TurnId::new("turn").unwrap(),
                &rsi_agent_session_protocol::StepId::new("step").unwrap(),
                plugin(600_000).contribution(None).unwrap(),
            )
            .unwrap();
        let (facts, domains) = batch.into_parts();
        assert!(domains.is_empty());
        assert!(
            matches!(&facts[..], [rsi_agent_session_protocol::SessionFactBody::InputMessageClaimed { content, .. }]
            if matches!(&content[..], [rsi_agent_session_protocol::AgentMessageContent::Text { text }]
                if text == "Current time sampled before this model request: 1970-01-01T00:16:40.123Z (UTC)."))
        );
        let invalid = TimeContext {
            clock: || UNIX_EPOCH - std::time::Duration::from_secs(1),
            ..plugin(600_000)
        };
        assert!(invalid.contribution(None).is_err());
    }

    #[test]
    fn refresh_uses_entered_input_and_handles_boundary_zero_and_clock_rollback() {
        let clock = plugin(600_000);
        for previous in [None, Some(400_123), Some(1_000_124)] {
            assert_eq!(clock.contribution(previous).unwrap().inputs.len(), 1);
        }
        for previous in [Some(400_124), Some(1_000_123)] {
            assert!(clock.contribution(previous).unwrap().inputs.is_empty());
        }
        assert_eq!(
            plugin(0)
                .contribution(Some(1_000_123))
                .unwrap()
                .inputs
                .len(),
            1
        );
        // Merely computing a proposal cannot start the interval.
        assert_eq!(clock.contribution(None).unwrap().inputs.len(), 1);
        assert_eq!(clock.contribution(None).unwrap().inputs.len(), 1);
    }

    #[test]
    fn configuration_defaults_and_unsigned_interval_are_admitted_at_prepare() {
        let factory = TimeContextFactory::default();
        for value in [
            ConfigValue::Null,
            serde_json::json!({}),
            serde_json::json!({"refresh_interval_ms": 0}),
            serde_json::json!({"refresh_interval_ms": u64::MAX}),
        ] {
            factory.prepare(&value).unwrap();
        }
        assert_eq!(Config::default().refresh_interval_ms, 600_000);
        for value in [
            serde_json::json!({"refresh_interval_ms": -1}),
            serde_json::json!({"refresh_interval_ms": 0.5}),
            serde_json::json!({"unknown": true}),
        ] {
            assert!(factory.prepare(&value).is_err());
        }
    }
}
