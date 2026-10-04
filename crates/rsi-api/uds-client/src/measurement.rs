//! Opt-in bounded report measurements; never enabled by the product.
use std::sync::Mutex;
/// Timing in nanoseconds for one complete finite HTTP exchange.
#[derive(Clone, Debug, serde::Serialize)]
pub struct ExchangeTiming {
    /// Socket connection and Hyper setup, including actual peer UID verification.
    pub setup_ns: u128,
    /// First HTTP response head observed by the transport.
    pub response_head_ns: u128,
    /// Complete finite response decoded and admitted into retained output.
    pub complete_ns: u128,
}
#[derive(Debug, Default)]
pub(crate) struct Measurement(Mutex<Vec<ExchangeTiming>>);
impl Measurement {
    pub(crate) fn record(&self, timing: ExchangeTiming) {
        let mut samples = self
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if samples.len() < 4096 {
            samples.push(timing);
        }
    }
    pub(crate) fn take(&self) -> Vec<ExchangeTiming> {
        std::mem::take(
            &mut *self
                .0
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
        )
    }
}
