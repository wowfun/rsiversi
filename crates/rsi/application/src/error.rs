/// Standard application failure classified by process exit contract.
#[derive(Clone, Debug, Eq, thiserror::Error, PartialEq)]
pub enum RsiError {
    /// CLI, path, Profile, Settings, or Host bootstrap failure.
    #[error("{0}")]
    Boot(String),
    /// Accepted turn or runtime execution failure.
    #[error("{0}")]
    Run(String),
}

impl RsiError {
    /// Stable process exit code for this failure class.
    pub const fn exit_code(&self) -> u8 {
        match self {
            Self::Boot(_) => 2,
            Self::Run(_) => 1,
        }
    }
}
