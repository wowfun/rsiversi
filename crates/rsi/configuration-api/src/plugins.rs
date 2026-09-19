use super::{
    ApiError, ConfigurationClient, ConfigurationOperation, Deserialize, Result, Serialize, revision,
};

/// Bounded flat plugin status page request.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PluginStatusRequest {
    /// Explicit observation source.
    pub target: PluginStatusTarget,
    /// Offset in this observed pair of revisions.
    pub offset: usize,
    /// Number of rows, one through sixty-four.
    pub limit: usize,
}
impl Default for PluginStatusRequest {
    fn default() -> Self {
        Self {
            target: PluginStatusTarget::Host,
            offset: 0,
            limit: 32,
        }
    }
}
impl PluginStatusRequest {
    /// Checks bounds before any source observation.
    pub fn validate(&self) -> Result<()> {
        self.target.validate()?;
        if self.offset > 8192 || !(1..=64).contains(&self.limit) {
            return Err(ApiError::Invalid(
                "Invalid plugin status page bounds".into(),
            ));
        }
        Ok(())
    }
}
/// Source selection; current presets and resident Sessions are never interchangeable.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum PluginStatusTarget {
    /// Current Host desired and observed Profile.
    #[default]
    Host,
    /// Pure current source preview for one logical preset.
    Preset {
        /// Validated logical preset identity.
        id: String,
    },
    /// Exact Header-bound resident Session generation.
    Session {
        /// Correlation validated by the product Session read owner.
        target: rsi_session_protocol::SessionTarget,
    },
}
impl PluginStatusTarget {
    /// Checks all external identifiers before reading any source.
    pub fn validate(&self) -> Result<()> {
        match self {
            Self::Host => Ok(()),
            Self::Preset { id }
                if rsi_agent_session_protocol::AgentPresetId::new(id.as_str()).is_ok() =>
            {
                Ok(())
            }
            Self::Session { target } => target
                .validate()
                .map_err(|_| ApiError::Invalid("Invalid Session target".into())),
            Self::Preset { .. } => Err(ApiError::Invalid("Invalid preset target".into())),
        }
    }
}
/// Closed source availability; absence never triggers a generation build.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PluginAvailability {
    /// Requested evidence is available.
    #[default]
    Ready,
    /// Session is cold.
    NotResident,
    /// A separately admitted Session load is in progress.
    Loading,
    /// Source is broken or provider supplied no manifest.
    Unavailable,
}
/// Path-free category of a preset's winning root.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PluginPresetSource {
    /// Product system root.
    System,
    /// Explicit configured root.
    Configured,
    /// Writable user root.
    User,
}
/// Origin of the selected executable implementation.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PluginOrigin {
    /// Linked implementation.
    Linked,
    /// Native implementation.
    Native,
    /// No implementation observed or resolved.
    #[default]
    Unresolved,
}
/// Redacted actionable reason with a fixed, path-free explanation.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PluginDiagnostic {
    /// Missing portable service.
    MissingService,
    /// Missing nominal in-process service.
    MissingLocal,
    /// Published contract has an incompatible identity/version.
    ContractMismatch,
    /// Activation or retirement failed.
    LifecycleFailed,
}
impl PluginDiagnostic {
    /// Fixed instructions suitable for both native and Web clients.
    pub const fn guidance(self) -> &'static str {
        match self {
            Self::MissingService => {
                "Enable the required service in the owning Profile, then refresh."
            }
            Self::MissingLocal => {
                "Check that the owning Profile includes the required local provider."
            }
            Self::ContractMismatch => {
                "Use plugin and provider versions that implement the same contract."
            }
            Self::LifecycleFailed => {
                "Inspect the local Host diagnostics, correct the source, then refresh."
            }
        }
    }
}
/// Source correlation retained across bounded pagination.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PluginStatusContext {
    /// Echoed source selection.
    pub target: PluginStatusTarget,
    /// Observed availability.
    pub availability: PluginAvailability,
    /// Current preset or exact resident generation digest, when known.
    pub source_digest: Option<String>,
    /// Winning root class for a current preset preview only.
    pub preset_source: Option<PluginPresetSource>,
}
/// Safe aggregate Profile convergence category.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PluginHealth {
    /// A convergence attempt is active.
    Converging,
    /// The observed graph matches the accepted target.
    Converged,
    /// The observed graph is degraded.
    Degraded,
    /// An accepted source needs a process restart.
    RestartRequired,
    /// The Profile retired.
    Stopped,
}
/// Safe source-watcher category, without source locations or errors.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PluginWatcher {
    /// No watched source.
    Inactive,
    /// Latest probe succeeded.
    Healthy,
    /// A source probe failed.
    Faulted,
}
/// Actual observed lifecycle, independent of desired enablement.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PluginLifecycle {
    /// Waiting for dependencies.
    Pending,
    /// Staging resources.
    Loading,
    /// Published generation.
    Active,
    /// Failed activation or retirement.
    Failed,
    /// Retiring resources.
    Unloading,
    /// Finished teardown.
    Disposed,
}
/// Observed implementation name and lifecycle; no raw revision or diagnostic strings.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PluginObservation {
    /// Validated plugin identity.
    pub plugin: String,
    /// Host Profile lifecycle, or activation state captured in a resident Session manifest.
    pub state: PluginLifecycle,
}
/// One identity in the union of desired leaves and observed instances.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PluginStatusRow {
    /// Flat `InstanceId`, with no parent or scope topology.
    pub instance: String,
    /// Desired plugin, absent for a retained removed instance.
    pub desired_plugin: Option<String>,
    /// Effective desired enablement, including disabled ancestors.
    pub enabled: bool,
    /// Selected implementation origin.
    pub origin: PluginOrigin,
    /// Unique redacted lifecycle reasons.
    pub diagnostics: Vec<PluginDiagnostic>,
    /// Observation from the page target: Host Profile state or resident Session manifest.
    /// Session targets do not inspect current per-instance lifecycle; preset-only rows omit this.
    pub observed: Option<PluginObservation>,
}
/// Redacted bounded snapshot page. The two revisions never imply one atomic graph.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PluginStatusPage {
    /// Explicit source and pagination correlation.
    pub context: PluginStatusContext,
    /// Desired tree revision.
    pub desired_revision: String,
    /// Observed Profile-status revision.
    pub observed_revision: String,
    /// Aggregate convergence category.
    pub health: Option<PluginHealth>,
    /// Aggregate source watcher category.
    pub watcher: Option<PluginWatcher>,
    /// Echoed request offset.
    pub offset: usize,
    /// Total flat identities.
    pub total: usize,
    /// Exact next offset, when more rows exist.
    pub next_offset: Option<usize>,
    /// Ordered flat identities.
    pub plugins: Vec<PluginStatusRow>,
}
fn identifier(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 256
        && !value
            .chars()
            .any(|ch| ch.is_whitespace() || ch.is_control())
}
impl PluginStatusPage {
    /// Validates bounds, exact progress and redacted field identities.
    pub fn validate(&self, request: &PluginStatusRequest) -> Result<()> {
        request.validate()?;
        revision(&self.desired_revision)?;
        revision(&self.observed_revision)?;
        let next = self.offset.saturating_add(self.plugins.len());
        let host = matches!(request.target, PluginStatusTarget::Host);
        if self.context.target != request.target
            || self.health.is_some() != host
            || self.watcher.is_some() != host
            || self.context.source_digest.as_ref().is_some_and(|digest| {
                digest.len() != 64
                    || !digest
                        .bytes()
                        .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
            })
            || (self.context.availability != PluginAvailability::Ready && self.total != 0)
            || (!matches!(request.target, PluginStatusTarget::Preset { .. })
                && self.context.preset_source.is_some())
            || self.offset != request.offset
            || self.total > 8192
            || self.plugins.len() > request.limit
            || self.plugins.len() != request.limit.min(self.total.saturating_sub(self.offset))
            || self.next_offset != (next < self.total).then_some(next)
            || self
                .plugins
                .windows(2)
                .any(|pair| pair[0].instance >= pair[1].instance)
            || self.plugins.iter().any(|row| {
                row.diagnostics.len() > 4
                    || row.diagnostics.windows(2).any(|pair| pair[0] >= pair[1])
                    || !identifier(&row.instance)
                    || row
                        .desired_plugin
                        .as_ref()
                        .is_some_and(|plugin| !identifier(plugin))
                    || row
                        .observed
                        .as_ref()
                        .is_some_and(|observed| !identifier(&observed.plugin))
                    || (row.desired_plugin.is_none() && (row.enabled || row.observed.is_none()))
            })
        {
            return Err(ApiError::Invalid("Invalid plugin status response".into()));
        }
        Ok(())
    }
}
/// Host-owned finite observation source, invoked only under configuration admission.
#[async_trait::async_trait]
pub trait PluginStatusSource: std::fmt::Debug + Send + Sync + 'static {
    /// Produces one redacted page without preparing or executing a plugin.
    async fn plugins(&self, request: PluginStatusRequest) -> Result<PluginStatusPage>;
}
impl ConfigurationClient {
    /// Reads one grant-gated page without reusing Local Inspector authority.
    pub async fn plugins(&self, request: PluginStatusRequest) -> Result<PluginStatusPage> {
        request.validate()?;
        let page: PluginStatusPage = self.call(ConfigurationOperation::Plugins, &request).await?;
        page.validate(&request)?;
        Ok(page)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn page() -> PluginStatusPage {
        PluginStatusPage {
            context: PluginStatusContext::default(),
            desired_revision: "1".into(),
            observed_revision: "9".into(),
            health: Some(PluginHealth::Degraded),
            watcher: Some(PluginWatcher::Faulted),
            offset: 0,
            total: 1,
            next_offset: None,
            plugins: vec![PluginStatusRow {
                instance: "plugin".into(),
                desired_plugin: Some("fixture.plugin".into()),
                enabled: false,
                origin: PluginOrigin::Linked,
                diagnostics: vec![],
                observed: Some(PluginObservation {
                    plugin: "fixture.previous".into(),
                    state: PluginLifecycle::Unloading,
                }),
            }],
        }
    }
    #[test]
    fn pages_validate_progress_and_keep_desired_separate_from_observed() {
        let request = PluginStatusRequest::default();
        page().validate(&request).unwrap();
        let mut invalid = page();
        invalid.next_offset = Some(0);
        assert!(invalid.validate(&request).is_err());
        let mut invalid = page();
        invalid.offset = 1;
        assert!(invalid.validate(&request).is_err());
        let mut invalid = page();
        invalid.plugins.push(invalid.plugins[0].clone());
        invalid.total = 2;
        assert!(invalid.validate(&request).is_err());
        let mut invalid = page();
        invalid.plugins[0].instance = "raw diagnostic\n/private/path".into();
        assert!(invalid.validate(&request).is_err());
        let mut invalid = page();
        invalid.observed_revision = "01".into();
        assert!(invalid.validate(&request).is_err());
        assert!(
            PluginStatusRequest {
                offset: 8193,
                limit: 32,
                ..Default::default()
            }
            .validate()
            .is_err()
        );
        assert!(
            PluginStatusRequest {
                offset: 0,
                limit: 65,
                ..Default::default()
            }
            .validate()
            .is_err()
        );
        let mut invalid = serde_json::to_value(page()).unwrap();
        invalid["raw_error"] = serde_json::json!("secret");
        assert!(serde_json::from_value::<PluginStatusPage>(invalid).is_err());
    }
}
