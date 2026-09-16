//! Product-owned projection of actual Profile observations into the narrow grant API.
use super::{ApiError, InspectorSource, Result, Source};
use rsi_configuration_api::{
    PluginHealth, PluginLifecycle, PluginObservation, PluginStatusPage, PluginStatusRequest,
    PluginStatusRow, PluginStatusSource, PluginWatcher,
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
                },
            );
        }
        desired(node.children(), enabled, rows)?;
    }
    Ok(())
}
impl PluginStatusSource for Source {
    fn plugins(&self, request: PluginStatusRequest) -> Result<PluginStatusPage> {
        request.validate()?;
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
            rows.entry(instance.id().to_string())
                .or_insert_with(|| PluginStatusRow {
                    instance: instance.id().to_string(),
                    desired_plugin: None,
                    enabled: false,
                    observed: None,
                })
                .observed = Some(PluginObservation { plugin, state });
        }
        let total = rows.len();
        let plugins: Vec<_> = rows
            .into_values()
            .skip(request.offset)
            .take(request.limit)
            .collect();
        let next = request.offset + plugins.len();
        let page = PluginStatusPage {
            desired_revision: snapshot.revision().to_string(),
            observed_revision: status.revision().to_string(),
            health: match status.health() {
                ProfileHealth::Converging => PluginHealth::Converging,
                ProfileHealth::Converged => PluginHealth::Converged,
                ProfileHealth::Degraded => PluginHealth::Degraded,
                ProfileHealth::RestartRequired => PluginHealth::RestartRequired,
                ProfileHealth::Stopped => PluginHealth::Stopped,
            },
            watcher: match status.watcher() {
                WatcherHealth::Inactive => PluginWatcher::Inactive,
                WatcherHealth::Healthy => PluginWatcher::Healthy,
                WatcherHealth::Faulted => PluginWatcher::Faulted,
            },
            offset: request.offset,
            total,
            next_offset: (next < total).then_some(next),
            plugins,
        };
        page.validate(request)?;
        Ok(page)
    }
}
