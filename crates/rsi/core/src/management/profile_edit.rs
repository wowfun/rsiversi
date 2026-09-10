use super::{
    AgentPresetManager, BTreeMap, ManagementOutput, ProfileCatalog, ProfileCommand, ProfileKind,
    ProfileOperationKind, RsiError, StandardComposition, application_profile_id, host_profile_id,
    report_error, standard_paths, write_json,
};
use rsi_meta_profile::{
    NodeChange, NodeChangeAspect, NodeChangeKind, ProfileSnapshot, SnapshotNode,
};
use serde_json::{Value, json};
use std::io::Read as _;
use std::os::unix::fs::OpenOptionsExt as _;

pub(super) async fn run(command: &ProfileCommand) -> u8 {
    match edit(command).await {
        Ok(value) => {
            let output = match command.output {
                ManagementOutput::Json => write_json(&value),
                ManagementOutput::Text => serde_json::to_string_pretty(&value)
                    .map_err(|_| RsiError::Boot("Profile edit output could not encode".into()))
                    .and_then(|text| rsi_terminal::write_text_line(&text)),
            };
            output.map_or_else(|error| report_error(&error), |()| 0)
        }
        Err(error) => report_error(&error),
    }
}

async fn edit(command: &ProfileCommand) -> rsi::Result<Value> {
    let proposed = read_source(&command.ids[1])?;
    let paths = standard_paths()?;
    let catalog = ProfileCatalog::new(paths.clone());
    #[cfg(target_os = "linux")]
    let coding = super::super::standard_coding_tools()?;
    #[cfg(not(target_os = "linux"))]
    let coding = None;
    let composition = StandardComposition::new(paths, BTreeMap::new(), coding);
    let presets = AgentPresetManager::open_standard_preview(&composition).await?;
    let result = (|| {
        let composition = composition.with_agent_presets(&presets)?;
        let host = match command.kind {
            ProfileKind::Host => composition.build_for_preview().map_err(edit_error)?,
            ProfileKind::Application => rsi::standard_application_host(composition, Vec::new())?.0,
        };
        let edit = match command.kind {
            ProfileKind::Host => {
                catalog.preview_host_edit(&host, &host_profile_id(&command.ids[0])?, &proposed)
            }
            ProfileKind::Application => catalog.preview_application_edit(
                &host,
                &application_profile_id(&command.ids[0])?,
                &proposed,
            ),
        }
        .map_err(edit_error)?;
        if command.operation == ProfileOperationKind::CommitEdit {
            if command.ids[2] != edit.review_digest() {
                return Err(RsiError::Boot(
                    "Profile edit differs from the reviewed digest; preview again".into(),
                ));
            }
            let receipt = edit.commit_once().map_err(edit_error)?;
            return Ok(json!({
                "version": 1, "type": "profile_edit_receipt", "path": receipt.path,
                "source": "published", "source_digest": receipt.source_digest,
                "directory_synced": receipt.directory_synced, "runtime": "not_requested",
            }));
        }
        let effective = edit.effective();
        Ok(json!({
            "version": 1, "type": "profile_edit_preview", "path": edit.path(),
            "review_digest": edit.review_digest(), "configuration_validation": "not_prepared",
            "source": {
                "before": std::str::from_utf8(edit.original_source()).ok(),
                "before_hex": std::str::from_utf8(edit.original_source()).is_err().then(|| hex::encode(edit.original_source())),
                "after": std::str::from_utf8(edit.proposed_source()).ok(),
            },
            "previous": effective.previous.as_ref().map(snapshot),
            "proposed": snapshot(&effective.proposed),
            "effective_changes": effective.changes.as_ref().map(|changes| changes.iter().map(change).collect::<Vec<_>>()),
            "factories": effective.leaves.iter().map(|leaf| json!({
                "instance_id": leaf.instance_id, "plugin_id": leaf.plugin_id,
                "identity": identity(&leaf.identity),
            })).collect::<Vec<_>>(),
            "sources": effective.sources.iter().map(|source| json!({
                "path": source.path, "sha256": hex::encode(source.sha256), "writable": source.path == edit.path(),
            })).collect::<Vec<_>>(),
        }))
    })();
    let shutdown = presets.shutdown().await;
    if !shutdown.is_clean() {
        // Publication may already have succeeded. Preserve its receipt and add cleanup status.
        return result.map(|mut value| {
            value["preview_cleanup"] = json!("degraded");
            value
        });
    }
    result
}

fn edit_error(error: impl std::fmt::Display) -> RsiError {
    RsiError::Boot(error.to_string())
}

fn read_source(path: &str) -> rsi::Result<Vec<u8>> {
    let maximum = rsi::MAXIMUM_PROFILE_DOCUMENT_BYTES;
    let file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)
        .map_err(edit_error)?;
    let metadata = file.metadata().map_err(edit_error)?;
    if !metadata.is_file() || metadata.len() > maximum as u64 {
        return Err(RsiError::Boot(
            "Profile edit input must be a bounded regular file".into(),
        ));
    }
    let mut bytes = Vec::new();
    file.take(maximum as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(edit_error)?;
    if bytes.len() > maximum {
        return Err(RsiError::Boot(
            "Profile edit input exceeds the document byte limit".into(),
        ));
    }
    Ok(bytes)
}

fn snapshot(snapshot: &ProfileSnapshot) -> Value {
    json!({ "source_digest": snapshot.source_digest(), "nodes": snapshot.nodes().iter().map(node).collect::<Vec<_>>() })
}
fn node(node: &SnapshotNode) -> Value {
    json!({ "id": node.id(), "plugin": node.plugin().map(rsi_meta::PluginId::as_str), "enabled": node.enabled(), "children": node.children().iter().map(self::node).collect::<Vec<_>>() })
}
fn change(change: &NodeChange) -> Value {
    let (kind, aspects) = match &change.kind {
        NodeChangeKind::Added => ("added", Vec::new()),
        NodeChangeKind::Removed => ("removed", Vec::new()),
        NodeChangeKind::Modified(aspects) => (
            "modified",
            aspects
                .iter()
                .map(|aspect| match aspect {
                    NodeChangeAspect::Kind => "kind",
                    NodeChangeAspect::Plugin => "plugin",
                    NodeChangeAspect::Parent => "parent",
                    NodeChangeAspect::Order => "order",
                    NodeChangeAspect::Enabled => "enabled",
                    NodeChangeAspect::Configuration => "configuration",
                    NodeChangeAspect::Isolation => "isolation",
                })
                .collect(),
        ),
    };
    json!({"id": change.id, "kind": kind, "aspects": aspects})
}
fn identity(identity: &rsi_meta::FactoryIdentity) -> Value {
    match identity {
        rsi_meta::FactoryIdentity::Linked { revision, .. } => {
            json!({"kind": "linked", "revision": revision})
        }
        rsi_meta::FactoryIdentity::Native { sha256, .. } => {
            json!({"kind": "native", "sha256": sha256})
        }
    }
}
