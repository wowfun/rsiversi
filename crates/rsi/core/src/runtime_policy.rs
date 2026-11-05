/// Maximum product transition, including resident Host and package verification.
pub const PRODUCT_TRANSITION_TIMEOUT: std::time::Duration = std::time::Duration::from_mins(3);

pub(crate) fn runtime_limits() -> rsi_meta::RuntimeLimits {
    let mut limits = rsi_meta::RuntimeLimits::default();
    limits.deadlines.transition = PRODUCT_TRANSITION_TIMEOUT;
    limits
}
