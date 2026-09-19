//! Product-owned projection of actual Profile observations into the narrow grant API.
use super::{ApiError, InspectorSource, Result, Source};
use rsi_agent_turn_protocol::ResidentComposition;
use rsi_configuration_api::{
    PluginAvailability, PluginDiagnostic, PluginHealth, PluginLifecycle, PluginObservation,
    PluginOrigin, PluginPresetSource, PluginStatusContext, PluginStatusPage, PluginStatusRequest,
    PluginStatusRow, PluginStatusSource, PluginStatusTarget, PluginWatcher,
};
use rsi_meta::FactoryIdentity;
use rsi_meta_profile::{ProfileHealth, ProfileInstanceState, SnapshotNode, WatcherHealth};
use std::collections::BTreeMap;

fn desired(
    nodes: &[SnapshotNode],
    parent_enabled: bool,
    rows: &mut BTreeMap<String, PluginStatusRow>,
) -> Result<()> {
    for node in nodes {
        let enabled = parent_enabled && node.enabled();
        if let Some(plugin) = node.plugin() {
            if rows.len() >= 8192 {
                return Err(ApiError::Capacity);
            }
            rows.insert(
                node.id().into(),
                PluginStatusRow {
                    instance: node.id().into(),
                    desired_plugin: Some(plugin.to_string()),
                    enabled,
                    observed: None,
                    origin: PluginOrigin::Unresolved,
                    diagnostics: vec![],
                },
            );
        }
        desired(node.children(), enabled, rows)?;
    }
    Ok(())
}
#[async_trait::async_trait]
impl PluginStatusSource for Source {
    async fn plugins(&self, request: PluginStatusRequest) -> Result<PluginStatusPage> {
        request.validate()?;
        match &request.target {
            PluginStatusTarget::Host => self.host_plugins(&request),
            PluginStatusTarget::Preset { id } => self.preset_plugins(&request, id).await,
            PluginStatusTarget::Session { target } => self.session_plugins(&request, target).await,
        }
    }
}
impl Source {
    fn host_plugins(&self, request: &PluginStatusRequest) -> Result<PluginStatusPage> {
        let (status, snapshot) = self.profile()?;
        let mut rows = BTreeMap::new();
        desired(snapshot.nodes(), true, &mut rows)?;
        for instance in status.observed() {
            if !rows.contains_key(instance.id().as_str()) && rows.len() >= 8192 {
                return Err(ApiError::Capacity);
            }
            let plugin = match instance.factory() {
                FactoryIdentity::Linked { plugin, .. } | FactoryIdentity::Native { plugin, .. } => {
                    plugin.to_string()
                }
            };
            let state = match instance.state() {
                ProfileInstanceState::Pending(_) => PluginLifecycle::Pending,
                ProfileInstanceState::Loading => PluginLifecycle::Loading,
                ProfileInstanceState::Active => PluginLifecycle::Active,
                ProfileInstanceState::Failed => PluginLifecycle::Failed,
                ProfileInstanceState::Unloading => PluginLifecycle::Unloading,
                ProfileInstanceState::Disposed => PluginLifecycle::Disposed,
            };
            let origin = match instance.factory() {
                FactoryIdentity::Linked { .. } => PluginOrigin::Linked,
                FactoryIdentity::Native { .. } => PluginOrigin::Native,
            };
            let mut diagnostics = match instance.state() {
                ProfileInstanceState::Pending(report) => report
                    .reasons
                    .iter()
                    .map(|reason| match reason {
                        rsi_meta::PendingReason::MissingService { .. } => {
                            PluginDiagnostic::MissingService
                        }
                        rsi_meta::PendingReason::MissingLocal { .. } => {
                            PluginDiagnostic::MissingLocal
                        }
                        rsi_meta::PendingReason::ContractMismatch { .. } => {
                            PluginDiagnostic::ContractMismatch
                        }
                    })
                    .collect::<Vec<_>>(),
                ProfileInstanceState::Failed => vec![PluginDiagnostic::LifecycleFailed],
                _ => Vec::new(),
            };
            diagnostics.sort_unstable();
            diagnostics.dedup();
            let row = rows
                .entry(instance.id().to_string())
                .or_insert_with(|| PluginStatusRow {
                    instance: instance.id().to_string(),
                    desired_plugin: None,
                    enabled: false,
                    observed: None,
                    origin,
                    diagnostics: vec![],
                });
            row.observed = Some(PluginObservation { plugin, state });
            row.origin = origin;
            row.diagnostics = diagnostics;
        }
        let total = rows.len();
        let plugins: Vec<_> = rows
            .into_values()
            .skip(request.offset)
            .take(request.limit)
            .collect();
        let next = request.offset + plugins.len();
        let page = PluginStatusPage {
            context: PluginStatusContext::default(),
            desired_revision: snapshot.revision().to_string(),
            observed_revision: status.revision().to_string(),
            health: Some(match status.health() {
                ProfileHealth::Converging => PluginHealth::Converging,
                ProfileHealth::Converged => PluginHealth::Converged,
                ProfileHealth::Degraded => PluginHealth::Degraded,
                ProfileHealth::RestartRequired => PluginHealth::RestartRequired,
                ProfileHealth::Stopped => PluginHealth::Stopped,
            }),
            watcher: Some(match status.watcher() {
                WatcherHealth::Inactive => PluginWatcher::Inactive,
                WatcherHealth::Healthy => PluginWatcher::Healthy,
                WatcherHealth::Faulted => PluginWatcher::Faulted,
            }),
            offset: request.offset,
            total,
            next_offset: (next < total).then_some(next),
            plugins,
        };
        page.validate(request)?;
        Ok(page)
    }
}

impl Source {
    async fn preset_plugins(
        &self,
        request: &PluginStatusRequest,
        id: &str,
    ) -> Result<PluginStatusPage> {
        let source = self
            .context
            .lookup_local::<rsi_agent_composition::AgentCompositionSourceContract>()
            .ok_or(ApiError::Unavailable)?
            .snapshot()
            .map_err(|_| ApiError::Unavailable)?;
        let id = rsi_agent_presets::AgentPresetId::new(id)
            .map_err(|_| ApiError::Invalid("Invalid preset target".into()))?;
        let roster = source
            .presets()
            .roster()
            .await
            .map_err(|_| ApiError::Unavailable)?;
        let row = roster
            .presets
            .iter()
            .find(|row| row.id == id)
            .ok_or(ApiError::Unavailable)?;
        let mut page = empty_page(request);
        page.context.preset_source = Some(match row.source {
            rsi_agent_presets::AgentPresetSource::System => PluginPresetSource::System,
            rsi_agent_presets::AgentPresetSource::Configured => PluginPresetSource::Configured,
            rsi_agent_presets::AgentPresetSource::User => PluginPresetSource::User,
        });
        let compiled = tokio::task::spawn_blocking(move || {
            let candidate = source.presets().compile(&id).ok()?;
            let manifest = source.manifest(&candidate.snapshot()).ok()?;
            Some((candidate.source_digest().to_owned(), manifest))
        })
        .await
        .map_err(|_| ApiError::Unavailable)?;
        if let Some((digest, manifest)) = compiled {
            page.context.source_digest = Some(digest);
            populate(&mut page, request, &manifest, false);
        } else {
            page.context.availability = PluginAvailability::Unavailable;
        }
        page.validate(request)?;
        Ok(page)
    }

    async fn session_plugins(
        &self,
        request: &PluginStatusRequest,
        target: &rsi_session_protocol::SessionTarget,
    ) -> Result<PluginStatusPage> {
        let reads = self
            .context
            .lookup_local::<rsi_session_protocol::SessionReadContract>()
            .ok_or(ApiError::Unavailable)?;
        let lease = reads
            .acquire(target)
            .await
            .map_err(|_| ApiError::Unavailable)?;
        let source = self
            .context
            .lookup_local::<rsi_agent_turn_protocol::SessionProjectionsContract>()
            .ok_or(ApiError::Unavailable)?;
        let mut page = empty_page(request);
        match source
            .resident_composition(&target.session_id)
            .map_err(|_| ApiError::Unavailable)?
        {
            ResidentComposition::NotResident => {
                page.context.availability = PluginAvailability::NotResident;
            }
            ResidentComposition::Loading => page.context.availability = PluginAvailability::Loading,
            ResidentComposition::Resident {
                header,
                source_digest,
                manifest,
            } => {
                if header.as_ref() != lease.header() {
                    return Err(ApiError::Unavailable);
                }
                page.context.source_digest = Some(source_digest);
                if let Some(manifest) = manifest {
                    populate(&mut page, request, &manifest, true);
                } else {
                    page.context.availability = PluginAvailability::Unavailable;
                }
            }
        }
        if lease.retiring().is_cancelled() {
            return Err(ApiError::Unavailable);
        }
        page.validate(request)?;
        Ok(page)
    }
}
fn empty_page(request: &PluginStatusRequest) -> PluginStatusPage {
    PluginStatusPage {
        context: PluginStatusContext {
            target: request.target.clone(),
            ..Default::default()
        },
        desired_revision: "0".into(),
        observed_revision: "0".into(),
        health: None,
        watcher: None,
        offset: request.offset,
        total: 0,
        next_offset: None,
        plugins: vec![],
    }
}
fn populate(
    page: &mut PluginStatusPage,
    request: &PluginStatusRequest,
    manifest: &rsi_agent_composition_protocol::CompositionManifest,
    resident: bool,
) {
    use rsi_agent_composition_protocol::CompositionOrigin;
    page.total = manifest.instances().len();
    page.plugins = manifest
        .instances()
        .iter()
        .skip(request.offset)
        .take(request.limit)
        .map(|row| PluginStatusRow {
            instance: row.instance.clone(),
            desired_plugin: Some(row.plugin.clone()),
            enabled: row.enabled,
            origin: match row.origin {
                CompositionOrigin::Linked => PluginOrigin::Linked,
                CompositionOrigin::Native => PluginOrigin::Native,
                CompositionOrigin::Unresolved => PluginOrigin::Unresolved,
            },
            diagnostics: vec![],
            observed: (resident && row.enabled).then(|| PluginObservation {
                plugin: row.plugin.clone(),
                state: PluginLifecycle::Active,
            }),
        })
        .collect();
    let next = request.offset + page.plugins.len();
    page.next_offset = (next < page.total).then_some(next);
}
