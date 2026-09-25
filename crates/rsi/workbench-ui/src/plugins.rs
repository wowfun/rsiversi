//! Shared finite plugin observations, independent of Local Inspector authority.
use super::{
    ActivationPlan, Arc, BoxFuture, ConfigValue, LocalContract, Mutex, PluginFactory,
    PreparedActivation, Result, Work, error, meta, prepare, watch,
};
use async_trait::async_trait;
use rsi_configuration_api::{
    ConfigurationClient, ExaClient, ExaCredentialStatus, McpClient, McpCredentialStatus,
    McpCredentialTarget, McpRefreshRequest, McpStatus, PluginAvailability, PluginStatusPage,
    PluginStatusRequest, PluginStatusTarget,
};
use serde::{Deserialize, Serialize};
mod leaves;
pub use leaves::{LeafCommand, LeafView};

/// Explicit read/refresh and version-bound page navigation.
#[derive(Clone, Debug, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum PluginsCommand {
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
    /// Separate reviewed Host source state; never contains configuration values.
    pub leaves: LeafView,
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
/// Ordinary workbench plugin state over the grant-gated Configuration API.
#[derive(Debug)]
pub struct PluginsFeature {
    client: ConfigurationClient,
    mcp: Option<McpClient>,
    exa: Option<ExaClient>,
    leaves: Option<rsi_configuration_api::leaf::Client>,
    leaf_grants: bool,
    state: Mutex<PluginsView>,
    work: Work,
}
impl PluginsFeature {
    /// Captures the current finite read result.
    ///
    /// # Panics
    /// Panics if the internal state lock was poisoned by a prior panic.
    pub fn snapshot(&self) -> PluginsView {
        let mut view = self.state.lock().expect("plugin view poisoned").clone();
        view.mcp_available = self.mcp.is_some();
        view.exa_available = self.exa.is_some();
        view.leaves.available = self.leaves.is_some();
        view.leaves.can_grant = self.leaf_grants
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
        view
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
            let mcp = if refresh {
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
/// Nominal shared plugin status feature.
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
        let feature = Arc::new(PluginsFeature {
            client: ConfigurationClient::new(plan.local::<rsi_api_protocol::ApiClientContract>()?)
                .map_err(meta)?,
            mcp: McpClient::new(plan.local::<rsi_api_protocol::ApiClientContract>()?).ok(),
            exa: ExaClient::new(plan.local::<rsi_api_protocol::ApiClientContract>()?).ok(),
            leaves: rsi_configuration_api::leaf::Client::new(
                plan.local::<rsi_api_protocol::ApiClientContract>()?,
            )
            .ok(),
            leaf_grants: plan
                .local::<rsi_api_protocol::ApiClientContract>()?
                .operations()
                .contains(&rsi_configuration_api::leaf::Operation::Grants.spec()),
            state: Mutex::default(),
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
