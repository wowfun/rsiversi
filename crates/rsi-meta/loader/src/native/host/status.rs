use super::HostFailure;
use crate::native::cap_table::CapError;
use crate::native::output_table::OutputError;
use rsi_meta_plugin::{
    STATUS_INVALID_ARGUMENT, STATUS_LIMIT_EXCEEDED, STATUS_PROTOCOL_ERROR, STATUS_STALE_CAPABILITY,
    STATUS_WRONG_CAPABILITY,
};

pub(super) fn output_error_status(error: OutputError) -> u32 {
    match error {
        OutputError::Invalid => STATUS_INVALID_ARGUMENT,
        OutputError::Stale => STATUS_STALE_CAPABILITY,
        OutputError::Protocol => STATUS_PROTOCOL_ERROR,
    }
}

impl HostFailure {
    pub(super) fn from_cap(error: CapError) -> Self {
        match error {
            CapError::Invalid => Self::new(STATUS_INVALID_ARGUMENT, "invalid capability"),
            CapError::Stale => Self::new(STATUS_STALE_CAPABILITY, "stale capability"),
            CapError::Protocol => Self::new(STATUS_PROTOCOL_ERROR, "capability already released"),
            CapError::Wrong | CapError::NotRetainable => {
                Self::new(STATUS_WRONG_CAPABILITY, "wrong capability kind or rights")
            }
            CapError::RefcountExhausted => {
                Self::new(STATUS_LIMIT_EXCEEDED, "capability reference limit exceeded")
            }
        }
    }
}
