//! Standard `RSIversi` Headless application composition and one-turn runner.

#![deny(unsafe_code)]
#![warn(missing_docs)]
#![allow(clippy::missing_errors_doc)]

mod composition;
mod run;
mod settings;

pub use composition::{StandardComposition, capture_standard_environment};
pub use run::{OutputMode, RunEvent, RunImageOptions, RunOptions, RunReport, SessionSelection};
pub use settings::{AgentSettings, AgentSettingsContract};

use rsi_host::{HostPaths, RunningHost};
use std::path::{Path, PathBuf};
use thiserror::Error;

/// Running standard Host and its application-facing operations.
#[derive(Debug)]
pub struct RunningRsi {
    host: RunningHost,
}

impl RunningRsi {
    /// Boots the standard immutable catalog from one required Profile file.
    pub async fn boot(composition: StandardComposition, profile_path: &Path) -> Result<Self> {
        let host = composition
            .build()
            .map_err(|error| RsiError::Boot(error.to_string()))?;
        let host = host
            .start_file(profile_path)
            .await
            .map_err(|error| RsiError::Boot(error.to_string()))?;
        if host.lookup_local::<AgentSettingsContract>().is_none() {
            let _outcome = host.shutdown().await;
            return Err(RsiError::Boot(
                "Headless Agent Settings did not become active".into(),
            ));
        }
        Ok(Self { host })
    }

    /// Returns the frozen Host paths.
    pub const fn paths(&self) -> &HostPaths {
        self.host.paths()
    }

    /// Submits trusted unscoped process-local work to the standard Jobs scheduler.
    ///
    /// Unscoped work is independent of Agent turn finalization and is cancelled
    /// only through its handle or Jobs generation retirement.
    pub fn submit_job(&self, spec: rsi_jobs::JobSpec) -> Result<rsi_jobs::JobHandle> {
        self.host
            .lookup_local::<rsi_jobs::JobsContract>()
            .ok_or_else(|| RsiError::Run("Jobs scheduler is unavailable".into()))?
            .submit(spec)
            .map_err(|error| RsiError::Run(error.to_string()))
    }

    /// Submits trusted process-local work owned by one exact Agent turn.
    ///
    /// Headless finalization cancels and boundedly joins only jobs bearing the
    /// matching session and turn identities before that turn becomes terminal.
    pub fn submit_turn_job(
        &self,
        session_id: &rsi_agent_session_protocol::SessionId,
        turn_id: &rsi_agent_session_protocol::TurnId,
        spec: rsi_jobs::JobSpec,
    ) -> Result<rsi_jobs::JobHandle> {
        let scope = composition::agent_turn_job_scope(session_id, turn_id)
            .map_err(|error| RsiError::Run(error.to_string()))?;
        self.host
            .lookup_local::<rsi_jobs::JobsContract>()
            .ok_or_else(|| RsiError::Run("Jobs scheduler is unavailable".into()))?
            .submit_scoped(scope, spec)
            .map_err(|error| RsiError::Run(error.to_string()))
    }

    /// Shuts down all Profile Fibers and process-local Jobs deterministically.
    pub async fn shutdown(&self) -> rsi_meta::ShutdownOutcome {
        self.host.shutdown().await
    }
}

/// Resolves standard XDG-style paths without searching for a Profile.
pub fn standard_paths() -> Result<HostPaths> {
    let config = optional_environment_path("XDG_CONFIG_HOME")?;
    let state = optional_environment_path("XDG_STATE_HOME")?;
    let cache = optional_environment_path("XDG_CACHE_HOME")?;
    let home = (config.is_none() || state.is_none() || cache.is_none())
        .then(|| environment_path("HOME"))
        .transpose()?;
    standard_paths_from(config, state, cache, home.as_deref())
}

fn standard_paths_from(
    config: Option<PathBuf>,
    state: Option<PathBuf>,
    cache: Option<PathBuf>,
    home: Option<&Path>,
) -> Result<HostPaths> {
    let fallback = |suffix: &str| {
        home.map(|home| home.join(suffix)).ok_or_else(|| {
            RsiError::Boot("required environment path `HOME` is not configured".into())
        })
    };
    let config = config.map_or_else(|| fallback(".config"), Ok)?.join("rsi");
    let state = state
        .map_or_else(|| fallback(".local/state"), Ok)?
        .join("rsi");
    let cache = cache.map_or_else(|| fallback(".cache"), Ok)?.join("rsi");
    HostPaths::new(config, state, cache).map_err(|error| RsiError::Boot(error.to_string()))
}

fn environment_path(name: &str) -> Result<PathBuf> {
    optional_environment_path(name)?.ok_or_else(|| {
        RsiError::Boot(format!(
            "required environment path `{name}` is not configured"
        ))
    })
}

fn optional_environment_path(name: &str) -> Result<Option<PathBuf>> {
    let Some(value) = std::env::var_os(name) else {
        return Ok(None);
    };
    let path = PathBuf::from(value);
    if !path.is_absolute() {
        return Err(RsiError::Boot(format!(
            "environment path `{name}` must be absolute"
        )));
    }
    Ok(Some(path))
}

/// Standard application failure classified by process exit contract.
#[derive(Clone, Debug, Eq, Error, PartialEq)]
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

/// Standard application result.
pub type Result<T> = std::result::Result<T, RsiError>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn complete_xdg_paths_do_not_require_home() {
        let paths = standard_paths_from(
            Some(PathBuf::from("/config")),
            Some(PathBuf::from("/state")),
            Some(PathBuf::from("/cache")),
            None,
        )
        .unwrap();

        assert_eq!(paths.config(), Path::new("/config/rsi"));
        assert_eq!(paths.state(), Path::new("/state/rsi"));
        assert_eq!(paths.cache(), Path::new("/cache/rsi"));
    }

    #[test]
    fn missing_xdg_path_requires_home_for_its_fallback() {
        assert!(matches!(
            standard_paths_from(
                Some(PathBuf::from("/config")),
                None,
                Some(PathBuf::from("/cache")),
                None,
            ),
            Err(RsiError::Boot(message)) if message.contains("HOME")
        ));
    }
}
