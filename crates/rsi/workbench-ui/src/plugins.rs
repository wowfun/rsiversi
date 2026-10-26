//! Shared finite plugin observations, independent of Local Inspector authority.
use super::{
    ActivationPlan, Arc, BoxFuture, ConfigValue, LocalContract, PluginFactory, PreparedActivation,
    Result, Work, error, prepare, watch,
};
use crate::PresentationState;
use async_trait::async_trait;
use rsi_configuration_api::{
    ConfigurationClient, ExaClient, ExaCredentialStatus, McpClient, McpCredentialStatus,
    McpCredentialTarget, McpRefreshRequest, McpStatus, PluginAvailability, PluginStatusPage,
    PluginStatusRequest, PluginStatusTarget,
};
use serde::{Deserialize, Serialize};
mod leaves;
mod mcp_ssh;
pub use mcp_ssh::{McpSshCommand, McpSshView};
mod ssh;
pub use leaves::{LeafCommand, LeafView};
pub use ssh::{SshCommand, SshView};

/// Explicit read/refresh and version-bound page navigation.
#[derive(Clone, Debug, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum PluginsCommand {
    /// Exact target/server MCP stdio administration.
    McpSsh {
        /// Explicit bounded operation.
        command: McpSshCommand,
    },
    /// Explicit SSH target management with Local-only trust controls.
    Ssh {
        /// Closed target operation.
        command: SshCommand,
    },
    /// Reviewed single-leaf Host source management, with separate explicit grants.
    Leaves {
        /// Closed bounded owner operation.
        command: LeafCommand,
    },
    /// Discard the old page and read current revisions.
    Refresh,
    /// Select a distinct observation source and discard old pagination.
    Select {
        /// Validated source identity.
        target: PluginStatusTarget,
    },
    /// Read the fixed Exa credential's redacted availability.
    ExaStatus,
    /// Save a credential without enabling Tools or submitting a search.
    ExaSet {
        /// Ephemeral zeroizing secret, never retained by a view.
        #[serde(deserialize_with = "secret")]
        secret: rsi_credentials_protocol::SecretValue,
    },
    /// Remove the fixed Exa credential independently of settings.
    ExaUnset,
    /// Read actual MCP connection observations.
    McpStatus,
    /// Apply saved HTTP settings and verify the selected connection; None selects all HTTP endpoints.
    McpRefresh {
        /// Exact server, or all HTTP endpoints.
        server: Option<String>,
    },
    /// Read one displayed exact credential binding.
    McpCredentialStatus {
        /// Expected current endpoint and reference.
        target: McpCredentialTarget,
    },
    /// Write one ephemeral secret with an independent receipt.
    McpCredentialSet {
        /// Expected current endpoint and reference.
        target: McpCredentialTarget,
        /// Zeroizing secret, excluded from all views.
        #[serde(deserialize_with = "secret")]
        secret: rsi_credentials_protocol::SecretValue,
    },
    /// Remove one exact configured credential with an independent receipt.
    McpCredentialUnset {
        /// Expected current endpoint and reference.
        target: McpCredentialTarget,
    },
    /// Read an adjacent page only from the displayed view.
    Page {
        /// Exact current view ticket.
        ticket: String,
        /// Bounded requested offset.
        offset: usize,
    },
}
/// Bounded projection retained by both product clients.
#[derive(Clone, Debug, Default, Serialize)]
pub struct PluginsView {
    /// Advisory exact commands available in the current operation catalog.
    pub actions: Vec<String>,
    /// Exact SSH MCP configuration and uncertainty state.
    pub mcp_ssh: McpSshView,
    /// Separate reviewed Host source state; never contains configuration values.
    pub leaves: LeafView,
    /// Redacted SSH targets and exact selected connection state.
    pub ssh: SshView,
    /// Whether this connection negotiated Exa credential management.
    pub exa_available: bool,
    /// Explicit redacted credential observation.
    pub exa_credential: Option<ExaCredentialStatus>,
    /// Independent safe Exa credential outcome.
    pub exa_notice: Option<String>,
    /// Fresh view identity; not an authorization token.
    pub ticket: String,
    /// Selected source, retained even when unavailable.
    pub target: PluginStatusTarget,
    /// Fixed guidance derived from closed source categories.
    pub guidance: Vec<String>,
    /// One complete page with independent desired and observed revisions.
    pub page: Option<PluginStatusPage>,
    /// Whether this connection negotiated the MCP workbench API.
    pub mcp_available: bool,
    /// Actual MCP configuration/connection observation.
    pub mcp: Option<McpStatus>,
    /// Safe MCP operation outcome, independent of plugin lifecycle status.
    pub mcp_notice: Option<String>,
    /// Explicit credential availability; no secret or local store path.
    pub mcp_credential: Option<McpCredentialStatus>,
    /// A bounded safe failure; a failed refresh clears stale observations.
    pub diagnostic: Option<String>,
}
#[derive(Debug, Default)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "independent optional capabilities, not mutually exclusive states"
)]
struct PluginCapabilities {
    actions: Vec<String>,
    mcp: bool,
    mcp_ssh: bool,
    exa: bool,
    leaves: bool,
    ssh: bool,
    can_trust: bool,
    can_grant: bool,
}
impl PluginCapabilities {
    fn new(catalog: &rsi_api_protocol::OperationCatalog) -> Self {
        use rsi_configuration_api::{
            ExaOperation as Exa, McpOperation as Mcp, mcp_ssh::Operation as McpSsh,
            ssh::Operation as Ssh,
        };
        let available = |specs: &[rsi_api_protocol::OperationSpec]| {
            PluginsFeature::available_in(catalog, specs)
        };
        let actions = [
            ("mcp_refresh", Mcp::Refresh.spec()),
            ("mcp_credential_status", Mcp::CredentialStatus.spec()),
            ("mcp_credential_set", Mcp::CredentialSet.spec()),
            ("mcp_credential_unset", Mcp::CredentialUnset.spec()),
            ("exa_set", Exa::Set.spec()),
            ("exa_unset", Exa::Unset.spec()),
            ("mcp_ssh.put", McpSsh::Put.spec()),
            ("mcp_ssh.remove", McpSsh::Remove.spec()),
            ("mcp_ssh.refresh", McpSsh::Refresh.spec()),
            ("ssh.put", Ssh::PutCandidate.spec()),
            ("ssh.connect", Ssh::Connect.spec()),
            ("ssh.disconnect", Ssh::Disconnect.spec()),
            ("ssh.resolve", Ssh::ResolveDirectory.spec()),
        ]
        .into_iter()
        .filter(|(_, spec)| available(std::slice::from_ref(spec)))
        .map(|(name, _)| name.to_owned())
        .collect();
        Self {
            actions,
            mcp: available(&[rsi_configuration_api::McpOperation::Status.spec()]),
            mcp_ssh: available(&[rsi_configuration_api::mcp_ssh::Operation::Get.spec()]),
            exa: available(&[rsi_configuration_api::ExaOperation::Status.spec()]),
            leaves: available(&[rsi_configuration_api::leaf::Operation::Catalog.spec()]),
            ssh: available(&[rsi_configuration_api::ssh::Operation::Catalog.spec()]),
            can_trust: available(&[rsi_configuration_api::ssh::Operation::ConfirmTrust.spec()]),
            can_grant: available(&[rsi_configuration_api::leaf::Operation::Grants.spec()]),
        }
    }
}
/// Ordinary workbench plugin state over the grant-gated Configuration API.
#[derive(Debug)]
pub struct PluginsFeature {
    api: Arc<dyn rsi_api_protocol::ApiClient>,
    capabilities: PresentationState<PluginCapabilities>,
    client: ConfigurationClient,
    mcp: Option<McpClient>,
    mcp_ssh: Option<rsi_configuration_api::mcp_ssh::Client>,
    exa: Option<ExaClient>,
    leaves: Option<rsi_configuration_api::leaf::Client>,
    ssh: Option<rsi_configuration_api::ssh::Client>,
    state: PresentationState<PluginsView>,
    work: Work,
}
impl PluginsFeature {
    fn available_in(
        catalog: &rsi_api_protocol::OperationCatalog,
        specs: &[rsi_api_protocol::OperationSpec],
    ) -> bool {
        specs.iter().all(|wanted| {
            catalog
                .operations()
                .iter()
                .any(|current| current.protocol == wanted.protocol && current.id == wanted.id)
        })
    }
    async fn discover(&self) -> Result<()> {
        let capabilities = self.api.operations().await.map_err(error)?;
        *self.capabilities.lock().expect("plugin capabilities") =
            PluginCapabilities::new(&capabilities);
        Ok(())
    }

    /// Captures the current finite read result.
    ///
    /// # Panics
    /// Panics if the internal state lock was poisoned by a prior panic.
    pub fn snapshot(&self) -> PluginsView {
        self.snapshot_if_changed(None).expect("uncached snapshot").1
    }

    /// Captures a coherent revision/view pair, cloning only when either owner changed.
    ///
    /// # Panics
    /// Panics if a previous mutation poisoned either presentation owner.
    pub fn snapshot_if_changed(
        &self,
        previous: Option<(u64, u64)>,
    ) -> Option<((u64, u64), PluginsView)> {
        let state = self.state.lock().expect("plugin view poisoned");
        let capabilities = self.capabilities.lock().expect("plugin capabilities");
        let revision = (state.content_revision(), capabilities.content_revision());
        if previous == Some(revision) {
            return None;
        }
        let mut view = state.clone();
        view.actions.clone_from(&capabilities.actions);
        view.mcp_available = capabilities.mcp;
        view.mcp_ssh.available = capabilities.mcp_ssh;
        view.exa_available = capabilities.exa;
        view.leaves.available = capabilities.leaves;
        view.ssh.available = capabilities.ssh;
        view.ssh.can_trust = capabilities.can_trust;
        view.leaves.can_grant = capabilities.can_grant
            && view.leaves.catalog.as_ref().is_some_and(|catalog| {
                matches!(
                    catalog.principal,
                    rsi_configuration_api::leaf::Principal::Local
                )
            });
        if let Some(page) = &view.page {
            view.guidance = page
                .plugins
                .iter()
                .flat_map(|row| &row.diagnostics)
                .map(|reason| reason.guidance().to_owned())
                .collect::<std::collections::BTreeSet<_>>()
                .into_iter()
                .collect();
            if let Some(attempt) = &page.last_attempt {
                view.guidance.push(attempt.guidance());
            }
            let availability = match page.context.availability {
                PluginAvailability::NotResident => Some(
                    "This Session has no resident generation. Normal resume may use the current preset.",
                ),
                PluginAvailability::Loading => {
                    Some("A Session load is in progress. Refresh after it finishes.")
                }
                PluginAvailability::Unavailable => Some(
                    "Composition evidence is unavailable. Check the preset source or local Host diagnostics.",
                ),
                PluginAvailability::Ready => None,
            };
            if let Some(message) = availability {
                view.guidance.push(message.into());
            }
        }
        Some((revision, view))
    }
    /// Subscribes to completed read results.
    pub fn changes(&self) -> watch::Receiver<u64> {
        self.work.changed.subscribe()
    }
    /// Admits one tracked read; page changes never combine different revisions.
    ///
    /// # Panics
    /// Panics if the internal state lock was poisoned by a prior panic.
    #[allow(clippy::too_many_lines)] // The closed workbench command dispatch shares one receipt and lifetime admission.
    pub fn command(self: &Arc<Self>, command: PluginsCommand) -> BoxFuture<'static, Result<()>> {
        let owner = self.clone();
        self.work.run(async move {
            if matches!(command, PluginsCommand::Refresh) {
                let _ = owner.discover().await;
            }
            if let PluginsCommand::McpSsh { command } = command {
                return owner.mcp_ssh_command(command).await;
            }
            if let PluginsCommand::Ssh { command } = command {
                return owner.ssh_command(command).await;
            }
            if let PluginsCommand::Leaves { command } = command {
                return owner.leaf_command(command).await;
            }
            if matches!(
                command,
                PluginsCommand::ExaStatus
                    | PluginsCommand::ExaSet { .. }
                    | PluginsCommand::ExaUnset
            ) {
                return owner.exa_command(command).await;
            }
            if !matches!(
                command,
                PluginsCommand::Refresh
                    | PluginsCommand::Select { .. }
                    | PluginsCommand::Page { .. }
            ) {
                return owner.mcp_command(command).await;
            }
            let refresh = matches!(command, PluginsCommand::Refresh);
            let (request, revisions) = match command {
                PluginsCommand::Refresh => (
                    PluginStatusRequest {
                        target: owner
                            .state
                            .lock()
                            .expect("plugin view poisoned")
                            .target
                            .clone(),
                        ..Default::default()
                    },
                    None,
                ),
                PluginsCommand::Select { target } => {
                    target.validate().map_err(error)?;
                    let mut state = owner.state.lock().expect("plugin view poisoned");
                    state.target = target.clone();
                    state.page = None;
                    state.guidance.clear();
                    (
                        PluginStatusRequest {
                            target,
                            ..Default::default()
                        },
                        None,
                    )
                }
                PluginsCommand::Page { ticket, offset } => {
                    let state = owner.state.lock().expect("plugin view poisoned");
                    let page = state
                        .page
                        .as_ref()
                        .filter(|_| state.ticket == ticket)
                        .ok_or("Plugin view changed; refresh it")?;
                    if page.next_offset != Some(offset) && page.offset.saturating_sub(32) != offset
                    {
                        return Err("Invalid plugin page transition".into());
                    }
                    (
                        PluginStatusRequest {
                            target: state.target.clone(),
                            offset,
                            limit: 32,
                        },
                        Some((
                            page.desired_revision.clone(),
                            page.observed_revision.clone(),
                            page.context.clone(),
                        )),
                    )
                }
                _ => unreachable!("MCP commands were handled above"),
            };
            let result = owner
                .client
                .plugins(request)
                .await
                .map_err(error)
                .and_then(|page| {
                    if revisions.is_some_and(|pair| {
                        pair != (
                            page.desired_revision.clone(),
                            page.observed_revision.clone(),
                            page.context.clone(),
                        )
                    }) {
                        Err(
                            "Plugin status changed while paging; refresh from the first page"
                                .into(),
                        )
                    } else {
                        Ok(page)
                    }
                });
            let mcp_available = owner.capabilities.lock().expect("plugin capabilities").mcp;
            let mcp = if refresh && mcp_available {
                if let Some(client) = &owner.mcp {
                    Some(client.status().await.map_err(error))
                } else {
                    None
                }
            } else {
                None
            };
            let mut state = owner.state.lock().expect("plugin view poisoned");
            if let Some(mcp) = mcp {
                state.mcp = mcp.as_ref().ok().cloned();
                state.mcp_notice = mcp.err();
                state.mcp_credential = None;
            }
            state.ticket = rsi_ui::fresh_identity("plugins")?;
            state.page = result.as_ref().ok().cloned();
            state.diagnostic = result.as_ref().err().cloned();
            result.map(|_| ())
        })
    }
    async fn mcp_command(&self, command: PluginsCommand) -> Result<()> {
        let client = self
            .mcp
            .as_ref()
            .ok_or("MCP is unavailable on this connection")?;
        let result = match command {
            PluginsCommand::McpStatus => {
                let result = client.status().await.map_err(error);
                self.state.lock().expect("plugin view poisoned").mcp =
                    result.as_ref().ok().cloned();
                result.map(|_| "MCP status refreshed".to_owned())
            }
            PluginsCommand::McpRefresh { server } => {
                let result = client
                    .refresh(&McpRefreshRequest { server })
                    .await
                    .map_err(error);
                let mut state = self.state.lock().expect("plugin view poisoned");
                match result {
                    Ok(result) => {
                        state.mcp = Some(result.status);
                        result.error.map_or_else(|| Ok("MCP connections verified. Existing conversations keep their saved schemas".to_owned()), |error| Err(error.to_string()))
                    }
                    Err(error) => {
                        state.mcp = None;
                        Err(error)
                    }
                }
            }
            PluginsCommand::McpCredentialStatus { target } => {
                let result = client.credential_status(&target).await.map_err(error);
                self.state
                    .lock()
                    .expect("plugin view poisoned")
                    .mcp_credential = result.as_ref().ok().cloned();
                result.map(|_| "Credential status refreshed".to_owned())
            }
            PluginsCommand::McpCredentialSet { target, secret } => {
                self.state
                    .lock()
                    .expect("plugin view poisoned")
                    .mcp_credential = None;
                client
                    .credential_set(&target, secret)
                    .await
                    .map_err(error)
                    .map(|_| "Credential saved. Refresh the MCP connection to verify it".to_owned())
            }
            PluginsCommand::McpCredentialUnset { target } => {
                self.state
                    .lock()
                    .expect("plugin view poisoned")
                    .mcp_credential = None;
                client
                    .credential_unset(&target)
                    .await
                    .map_err(error)
                    .map(|_| {
                        "Credential removed. Existing started calls were not replayed".to_owned()
                    })
            }
            _ => return Err("Invalid MCP command".into()),
        };
        let mut state = self.state.lock().expect("plugin view poisoned");
        state.ticket = rsi_ui::fresh_identity("plugins")?;
        state.mcp_notice = Some(match &result {
            Ok(message) | Err(message) => message.clone(),
        });
        result.map(|_| ())
    }
    async fn exa_command(&self, command: PluginsCommand) -> Result<()> {
        let client = self
            .exa
            .as_ref()
            .ok_or("Exa credential management is unavailable")?;
        self.state
            .lock()
            .expect("plugin view poisoned")
            .exa_credential = None;
        let result = match command {
            PluginsCommand::ExaStatus => {
                let result = client.status().await.map_err(error);
                self.state.lock().expect("plugin view poisoned").exa_credential = result.as_ref().ok().cloned();
                result.map(|_| "Exa credential status refreshed".to_owned())
            }
            PluginsCommand::ExaSet { secret } => client.set(secret).await.map_err(error).map(|_| "Exa credential saved. No search was submitted; enable web_search separately for new conversations".to_owned()),
            PluginsCommand::ExaUnset => client.unset().await.map_err(error).map(|_| "Exa credential removed. Tool settings are unchanged".to_owned()),
            _ => return Err("Invalid Exa credential command".into()),
        };
        let mut state = self.state.lock().expect("plugin view poisoned");
        state.ticket = rsi_ui::fresh_identity("plugins")?;
        state.exa_notice = Some(match &result {
            Ok(text) | Err(text) => text.clone(),
        });
        result.map(|_| ())
    }
}
/// Ordinary workbench plugin state over the grant-gated Configuration API.
#[derive(Debug)]
pub struct PluginsFeatureContract;
impl LocalContract for PluginsFeatureContract {
    const KEY: &'static str = "rsi.workbench.plugins";
    type Service = PluginsFeature;
}
/// Ordinary plugin diagnostics workbench factory.
#[derive(Clone, Debug, Default)]
pub struct PluginsFeatureFactory;
#[async_trait]
impl PluginFactory for PluginsFeatureFactory {
    fn prepare(&self, config: &ConfigValue) -> rsi_meta::Result<PreparedActivation> {
        prepare(config)
    }
    async fn activate(&self, plan: ActivationPlan) -> rsi_meta::Result<()> {
        let api = plan.local::<rsi_api_protocol::ApiClientContract>()?;
        let discovered = api.operations().await;
        let mut initial = PluginsView::default();
        let capabilities = match discovered {
            Ok(catalog) => catalog,
            Err(error) => {
                initial.diagnostic = Some(error.to_string());
                rsi_api_protocol::OperationCatalog::new(vec![]).expect("empty catalog")
            }
        };
        let feature = Arc::new(PluginsFeature {
            api: api.clone(),
            capabilities: PresentationState::new(PluginCapabilities::new(&capabilities)),
            client: ConfigurationClient::new(plan.local::<rsi_api_protocol::ApiClientContract>()?),
            mcp_ssh: Some(rsi_configuration_api::mcp_ssh::Client::new(api.clone())),
            mcp: Some(McpClient::new(
                plan.local::<rsi_api_protocol::ApiClientContract>()?,
            )),
            exa: Some(ExaClient::new(
                plan.local::<rsi_api_protocol::ApiClientContract>()?,
            )),
            leaves: Some(rsi_configuration_api::leaf::Client::new(
                plan.local::<rsi_api_protocol::ApiClientContract>()?,
            )),
            ssh: Some(rsi_configuration_api::ssh::Client::new(
                plan.local::<rsi_api_protocol::ApiClientContract>()?,
            )),
            state: PresentationState::new(initial),
            work: Work::new(plan.context().runtime().execution().clone()),
        });
        let supply = plan
            .context()
            .provide_local::<PluginsFeatureContract>(feature.clone())?;
        plan.defer(
            "drain plugin observations",
            Box::new(move || {
                Box::pin(async move {
                    drop(supply);
                    feature.work.close().await;
                    Ok(())
                })
            }),
        )?;
        Ok(())
    }
}
#[cfg(test)]
#[path = "plugins_tests.rs"]
mod tests;

fn secret<'de, D: serde::Deserializer<'de>>(
    decoder: D,
) -> std::result::Result<rsi_credentials_protocol::SecretValue, D::Error> {
    rsi_credentials_protocol::SecretValue::new(String::deserialize(decoder)?)
        .map_err(serde::de::Error::custom)
}
