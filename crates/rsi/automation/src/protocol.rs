use rsi_browser::{BrowserPolicy, CheckSpec};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

pub(crate) const MAXIMUM_RULES: usize = 128;
pub(crate) fn timestamp(value: u64) -> Result<(), String> {
    if !(1..=253_402_300_799_999).contains(&value) {
        return Err("timestamp outside supported RFC3339 range".into());
    }
    Ok(())
}
/// Immutable standing-rule revision. Stable rule identities must not be recycled.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AutomationRule {
    pub id: String,
    pub revision: u64,
    pub enabled: bool,
    pub repository_id: u64,
    pub environment: String,
    pub preview_host_suffix: String,
    pub path_prefix: String,
    pub dependency_hosts: BTreeSet<String>,
    pub checks: CheckSpec,
    pub explore_on_failure: bool,
    pub authorized_catalog_digest: String,
    pub model: rsi_ai_protocol::ModelRef,
    pub turn_budget: rsi_agent_session_protocol::TurnBudget,
    pub max_rounds: u8,
}
impl AutomationRule {
    /// # Errors
    /// Rejects malformed values or values exceeding the owning protocol bounds.
    pub fn validate(&self) -> Result<(), String> {
        identity(&self.id)?;
        if self.revision == 0
            || self.repository_id == 0
            || self.environment.is_empty()
            || self.environment.len() > 128
            || self.environment.chars().any(char::is_control)
            || self.preview_host_suffix.len() > 253
            || self.preview_host_suffix.is_empty()
            || !self
                .preview_host_suffix
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'.' | b'-'))
        {
            return Err("invalid automation rule".into());
        }
        digest(&self.authorized_catalog_digest)?;
        self.turn_budget.validate().map_err(|e| e.to_string())?;
        if self.max_rounds == 0
            || self.max_rounds > 2
            || self.turn_budget.maximum_elapsed_ms() > 120_000
            || self.turn_budget.maximum_provider_attempts() > 8
            || self.turn_budget.maximum_tool_calls() > 16
            || self.turn_budget.maximum_generated_records() > 256
            || self.turn_budget.maximum_generated_record_bytes() > 1_048_576
        {
            return Err("standing exploration budget exceeds authorized bounds".into());
        }
        self.checks.validate()?;
        self.policy(&format!(
            "https://deployment.{}{}",
            self.preview_host_suffix, self.path_prefix
        ))?;
        Ok(())
    }
    /// # Errors
    /// Propagates validation and owning-service failures.
    pub fn policy(&self, entry_url: &str) -> Result<BrowserPolicy, String> {
        let policy = BrowserPolicy {
            entry_url: entry_url.into(),
            path_prefix: self.path_prefix.clone(),
            dependency_hosts: self.dependency_hosts.clone(),
        };
        policy.validate()?;
        let url = url::Url::parse(entry_url).map_err(|_| "invalid preview URL")?;
        let host = url.host_str().ok_or("missing preview host")?;
        if !host.ends_with(&format!(".{}", self.preview_host_suffix)) {
            return Err("preview requires deployment-specific allowed subdomain".into());
        }
        Ok(policy)
    }
}
pub(crate) fn identity(value: &str) -> Result<(), String> {
    if value.is_empty()
        || value.len() > 128
        || !value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_'))
    {
        return Err("invalid automation identity".into());
    }
    Ok(())
}
pub(crate) fn digest(value: &str) -> Result<(), String> {
    if value.len() != 64
        || !value
            .bytes()
            .all(|b| b.is_ascii_digit() || matches!(b, b'a'..=b'f'))
    {
        return Err("invalid SHA-256 digest".into());
    }
    Ok(())
}
/// Typed, authenticated external metadata; no executable instructions.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Deployment {
    pub repository_id: u64,
    pub deployment_id: u64,
    pub status_id: u64,
    pub deployment_created_ms: u64,
    pub status_created_ms: u64,
    pub environment: String,
    pub sha: String,
    pub url: String,
}
impl Deployment {
    /// # Errors
    /// Rejects malformed values or values exceeding the owning protocol bounds.
    pub fn validate(&self) -> Result<(), String> {
        timestamp(self.deployment_created_ms)?;
        timestamp(self.status_created_ms)?;
        if self.repository_id == 0
            || self.deployment_id == 0
            || self.status_id == 0
            || self.deployment_created_ms == 0
            || self.status_created_ms == 0
            || self.environment.is_empty()
            || self.environment.len() > 128
            || self.url.len() > 4096
            || !matches!(self.sha.len(), 40 | 64)
            || !self
                .sha
                .bytes()
                .all(|b| b.is_ascii_digit() || matches!(b, b'a'..=b'f'))
        {
            return Err("invalid deployment metadata".into());
        }
        Ok(())
    }
}
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AttemptState {
    Queued,
    Running,
    Passed,
    Failed,
    Unavailable,
    Cancelled,
    Interrupted,
    Superseded,
    CapacityRejected,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Attempt {
    pub id: u64,
    pub task_id: u64,
    pub rule: AutomationRule,
    pub deployment: Deployment,
    pub state: AttemptState,
    pub created_ms: u64,
    pub result: Option<rsi_browser::CheckResult>,
    pub session_id: Option<String>,
    pub report: Option<String>,
    #[serde(default)]
    pub exploration: ExplorationState,
}
impl Attempt {
    /// Whether checking or authorized exploration still owns unfinished work.
    pub fn work_pending(&self) -> bool {
        matches!(self.state, AttemptState::Queued | AttemptState::Running)
            || self.state == AttemptState::Failed
                && self.rule.explore_on_failure
                && matches!(
                    self.exploration,
                    ExplorationState::NotStarted
                        | ExplorationState::Starting
                        | ExplorationState::Running
                )
    }
}
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExplorationState {
    #[default]
    NotStarted,
    Starting,
    Running,
    Complete,
    Failed,
    Interrupted,
    Cancelled,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Receipt {
    pub delivery: String,
    pub task_id: u64,
    pub attempt_id: u64,
    pub duplicate: bool,
}
