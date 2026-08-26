use std::fmt;

/// Failure returned by a host operation through the safe SDK.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SdkError {
    status: u32,
    diagnostic: String,
}

impl SdkError {
    pub(crate) fn new(status: u32, diagnostic: impl Into<String>) -> Self {
        Self {
            status,
            diagnostic: diagnostic.into(),
        }
    }

    pub const fn status(&self) -> u32 {
        self.status
    }

    pub fn diagnostic(&self) -> &str {
        &self.diagnostic
    }
}

impl fmt::Display for SdkError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "native host status {}: {}",
            self.status, self.diagnostic
        )
    }
}

impl std::error::Error for SdkError {}
