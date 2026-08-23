#![allow(clippy::wildcard_imports)] // This is one implementation partition of runtime.

use super::*;

/// Immutable capacity and deadline policy enforced by one Runtime.
#[derive(Clone, Debug)]
pub struct RuntimeLimits {
    /// Maximum registered Fibers, including pending and unloading Fibers.
    pub maximum_fibers: usize,
    /// Maximum published service slots.
    pub maximum_services: usize,
    /// Maximum staged and published event listeners.
    pub maximum_event_listeners: usize,
    /// Maximum cleanup effects owned by one Fiber generation.
    pub maximum_effects_per_fiber: usize,
    /// Maximum encoded bytes in one service frame, event value, or overlay.
    pub maximum_frame_bytes: usize,
    /// Maximum encoded bytes in input and normalized plugin configuration.
    pub maximum_config_bytes: usize,
    /// Maximum distinct Fibers reconciled concurrently by the worker.
    pub maximum_concurrent_reconciliations: usize,
    /// Maximum admitted live service calls across the Runtime.
    pub maximum_concurrent_service_calls: usize,
    /// Bounded capacity of each request or ordinary response channel.
    pub channel_capacity: usize,
    /// Complete activation or reconfiguration deadline.
    pub transition_timeout: Duration,
    /// Complete service-stream deadline from admission.
    pub service_call_timeout: Duration,
    /// Complete event-dispatch deadline from admission.
    pub event_callback_timeout: Duration,
    /// One deadline shared by all root disposals during shutdown.
    pub shutdown_timeout: Duration,
}

impl Default for RuntimeLimits {
    fn default() -> Self {
        Self {
            maximum_fibers: 4_096,
            maximum_services: 4_096,
            maximum_event_listeners: 16_384,
            maximum_effects_per_fiber: 4_096,
            maximum_frame_bytes: 1024 * 1024,
            maximum_config_bytes: 1024 * 1024,
            maximum_concurrent_reconciliations: 32,
            maximum_concurrent_service_calls: 1_024,
            channel_capacity: 32,
            transition_timeout: Duration::from_secs(30),
            service_call_timeout: Duration::from_mins(1),
            event_callback_timeout: Duration::from_mins(1),
            shutdown_timeout: Duration::from_secs(90),
        }
    }
}

impl RuntimeLimits {
    pub(super) fn validate(&self) -> Result<()> {
        if self.maximum_fibers == 0
            || self.maximum_services == 0
            || self.maximum_event_listeners == 0
            || self.maximum_effects_per_fiber == 0
            || self.maximum_frame_bytes == 0
            || self.maximum_config_bytes == 0
            || self.maximum_concurrent_reconciliations == 0
            || self.maximum_concurrent_service_calls == 0
            || self.channel_capacity == 0
            || self.transition_timeout.is_zero()
            || self.service_call_timeout.is_zero()
            || self.event_callback_timeout.is_zero()
            || self.shutdown_timeout.is_zero()
        {
            return Err(MetaError::InvalidInput(
                "runtime capacity limits must be nonzero".to_owned(),
            ));
        }
        if self.shutdown_timeout < self.transition_timeout
            || self.shutdown_timeout < self.service_call_timeout
            || self.shutdown_timeout < self.event_callback_timeout
        {
            return Err(MetaError::InvalidInput(
                "shutdown timeout must cover transition, service, and event deadlines".to_owned(),
            ));
        }
        Ok(())
    }
}
