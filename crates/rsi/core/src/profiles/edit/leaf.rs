use super::*;
use rsi_meta_profile::SnapshotNode;
use serde_json::Value;
use toml_edit::{ArrayOfTables, DocumentMut, Item, Table};

/// A single literal change, borrowed so diagnostics never own secret configuration.
#[derive(Clone, Copy)]
pub enum HostLeafEdit<'a> {
    /// Replaces only the leaf's own enabled flag.
    Enabled(bool),
    /// Replaces only its complete JSON configuration, preserving exact numbers.
    Configuration(&'a Value),
}
impl std::fmt::Debug for HostLeafEdit<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Enabled(enabled) => f.debug_tuple("Enabled").field(enabled).finish(),
            Self::Configuration(_) => f.write_str("Configuration(<redacted>)"),
        }
    }
}
impl ProfileCatalog {
    pub(crate) fn host_edit_source(&self, id: &HostProfileId) -> Result<Vec<u8>, ProfileEditError> {
        reject_builtin_target(
            "Host Profile",
            id.as_str(),
            id.as_str() == STANDARD_HOST_PROFILE,
        )?;
        let path = self.host_path(id);
        let directory = open_absolute_directory_no_follow(
            path.parent().ok_or(ProfileEditError::InvalidSource)?,
        )?
        .into_std_file();
        Ok(read_root(&directory, &path)?.0)
    }
    /// Reviews one strict leaf override in the selected writable root.
    /// The caller owns preparation admission; no source changes until commit.
    pub fn preview_host_leaf_edit<'host>(
        &self,
        host: &'host Host,
        runtime: &rsi_meta::Runtime,
        id: &HostProfileId,
        instance: &str,
        change: HostLeafEdit<'_>,
    ) -> Result<ProfileEdit<'host>, ProfileEditError> {
        reject_builtin_target(
            "Host Profile",
            id.as_str(),
            id.as_str() == STANDARD_HOST_PROFILE,
        )?;
        if rsi_configuration_api::leaf::validate_leaf(instance).is_err() {
            return Err(ProfileEditError::NotLeaf);
        }
        let path = self.host_path(id);
        let directory = open_absolute_directory_no_follow(
            path.parent().ok_or(ProfileEditError::InvalidSource)?,
        )?
        .into_std_file();
        let original = read_root(&directory, &path)?.0;
        let mut override_step = Table::new();
        override_step.insert("kind", toml_edit::value("patch"));
        override_step.insert("target", toml_edit::value(instance));
        match change {
            HostLeafEdit::Enabled(value) => {
                override_step.insert("enabled", toml_edit::value(value));
            }
            HostLeafEdit::Configuration(value) => {
                rsi_configuration_api::leaf::validate_configuration(value)
                    .map_err(|_| ProfileEditError::ConfigurationBounds)?;
                let json = serde_json::to_string(value)
                    .map_err(|_| ProfileEditError::ConfigurationBounds)?;
                override_step.insert("config_json", toml_edit::value(json));
            }
        }
        let text = std::str::from_utf8(&original).map_err(|_| ProfileEditError::InvalidSource)?;
        let mut document = text
            .parse::<DocumentMut>()
            .map_err(|_| ProfileEditError::InvalidSource)?;
        if !document.contains_key("steps") {
            document.insert("steps", Item::ArrayOfTables(ArrayOfTables::new()));
        }
        let steps = document
            .get_mut("steps")
            .ok_or(ProfileEditError::InvalidSource)?;
        if let Some(steps) = steps.as_array_of_tables_mut() {
            steps.push(override_step);
        } else if let Some(steps) = steps.as_array_mut() {
            steps.push(override_step.into_inline_table());
        } else {
            return Err(ProfileEditError::InvalidSource);
        }
        let proposed = document.to_string();
        let mut edit = match self.preview_host_edit(host, id, proposed.as_bytes()) {
            Ok(edit) => edit,
            Err(error) => {
                // A missing patch target may fail compilation before a proposed
                // tree exists. Keep the public NotLeaf rejection on that path.
                let previous = host.preview_file_edit(&path, &original)?;
                if find(previous.proposed.nodes(), instance, None)
                    .is_none_or(|(leaf, _)| leaf.plugin().is_none())
                {
                    return Err(ProfileEditError::NotLeaf);
                }
                return Err(error);
            }
        };
        let previous = edit
            .effective
            .previous
            .as_ref()
            .ok_or(ProfileEditError::InvalidSource)?;
        let (leaf, blocked) =
            find(previous.nodes(), instance, None).ok_or(ProfileEditError::NotLeaf)?;
        let plugin = leaf.plugin().ok_or(ProfileEditError::NotLeaf)?.clone();
        if let (HostLeafEdit::Enabled(true), Some(parent)) = (change, blocked) {
            return Err(ProfileEditError::DisabledAncestor(parent.into()));
        }
        if edit.original != original {
            return Err(ProfileEditError::Conflict);
        }
        let prepared = host.prepare_file_edit(runtime, &path, proposed.as_bytes())?;
        if prepared != edit.effective {
            return Err(ProfileEditError::Conflict);
        }
        if let HostLeafEdit::Configuration(value) = change
            && !prepared
                .leaves
                .iter()
                .any(|leaf| leaf.instance_id == instance)
        {
            host.prepare_configuration(runtime, &plugin, value.clone())?;
        }
        edit.prepared = true;
        Ok(edit)
    }
}
fn find<'a>(
    nodes: &'a [SnapshotNode],
    target: &str,
    blocked: Option<&'a str>,
) -> Option<(&'a SnapshotNode, Option<&'a str>)> {
    for node in nodes {
        if node.id() == target {
            return Some((node, blocked));
        }
        if let Some(found) = find(
            node.children(),
            target,
            blocked.or_else(|| (!node.enabled()).then(|| node.id())),
        ) {
            return Some(found);
        }
    }
    None
}
