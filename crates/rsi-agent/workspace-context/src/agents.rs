//! File discovery and parsing, shared by human previews and fresh spawn resolution.
use super::*;
use rsi_agent_session_protocol::{
    DelegationRole, ModelSelection, SpawnRoleReference, SpawnRoleSeed,
};

const MAXIMUM_FILES: usize = 32;
const MAXIMUM_FILE_BYTES: usize = 64 * 1024;

/// One winning definition, including a visible diagnostic for malformed sources.
#[derive(Clone, Debug)]
pub struct WorkspaceAgentDefinition {
    /// Filename-derived role name.
    pub name: String,
    /// Description, or a bounded failure diagnostic.
    pub description: String,
    /// Logical file locator for display only.
    pub source: String,
    /// Complete original Markdown for human preview.
    pub text: String,
    /// Validated definition; malformed files stay visible but cannot execute.
    pub seed: Option<SpawnRoleSeed>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Frontmatter {
    description: String,
    model: Option<rsi_ai_protocol::ModelRef>,
    reasoning_effort: Option<rsi_ai_protocol::ReasoningEffortId>,
    allow: Option<BTreeSet<String>>,
    #[serde(default)]
    deny: BTreeSet<String>,
}
fn parse(name: &str, source: &str, raw: &str) -> Result<(String, SpawnRoleSeed), String> {
    let (yaml, body) = split_skill(raw).ok_or("expected YAML frontmatter followed by Markdown")?;
    let metadata: Frontmatter =
        yaml_serde::from_str(yaml).map_err(|error| format!("invalid frontmatter: {error}"))?;
    let description = metadata.description.trim();
    if description.is_empty()
        || description.len() > 1024
        || description.chars().any(char::is_control)
        || body.trim().is_empty()
    {
        return Err("description must be 1..1024 plain bytes and body must be nonempty".into());
    }
    if metadata.model.is_none() && metadata.reasoning_effort.is_some() {
        return Err("reasoning_effort requires model".into());
    }
    let seed = SpawnRoleSeed {
        reference: SpawnRoleReference {
            provider: "rsi.agents".into(),
            name: name.into(),
        },
        role: DelegationRole {
            name: name.into(),
            persona: Some(body.trim().into()),
            allow: metadata.allow,
            deny: metadata.deny,
        },
        model: metadata.model.map(|model| ModelSelection {
            model,
            reasoning_effort: metadata.reasoning_effort,
        }),
        source: source.into(),
        sha256: digest(raw),
    };
    seed.validate().map_err(|error| error.to_string())?;
    Ok((description.into(), seed))
}
fn failure(path: &Path, error: impl fmt::Display) -> WorkspaceContextError {
    let mut observation = Observation::default();
    let reason = format!("agent source: {error}");
    observation.fail(path, &reason.escape_debug().to_string());
    WorkspaceContextError::Failed(observation.diagnostic.expect("recorded failure"))
}

#[expect(
    clippy::too_many_lines,
    reason = "Discovery retains one bounded directory and file ownership sequence"
)]
pub(super) fn read_agents(
    config: &WorkspaceContextConfig,
    cwd: &Path,
    id: Option<&str>,
    reserved_names: &BTreeSet<String>,
    mut budget: SnapshotBudget,
) -> Result<Vec<WorkspaceAgentDefinition>, WorkspaceContextError> {
    let mut observation = Observation::default();
    let (root, _) = skills::project_boundary(cwd, &mut observation);
    if let Some(error) = observation.diagnostic {
        return Err(WorkspaceContextError::Failed(error));
    }
    let mut roots = match root {
        Some(root) => directories_between(&root, cwd)?
            .into_iter()
            .rev()
            .map(|path| path.join(".agents/agents"))
            .collect::<Vec<_>>(),
        None => Vec::new(),
    };
    roots.extend(config.user_agent_roots.iter().cloned());
    let mut selected = BTreeMap::new();
    let mut visited = BTreeSet::new();
    let mut listed = 0;
    let mut inspected = 0;
    for root in roots {
        budget.check()?;
        let canonical = match fs::canonicalize(&root) {
            Ok(path) => path,
            Err(error) if skill_files::optional_directory_missing(&error) => continue,
            Err(error) => return Err(failure(&root, error)),
        };
        #[cfg(unix)]
        let dir = open_absolute_directory_no_follow(&canonical);
        #[cfg(not(unix))]
        let dir = Dir::open_ambient_dir(&canonical, ambient_authority());
        let dir = match dir {
            Ok(dir) => dir,
            Err(error) if skill_files::optional_directory_missing(&error) => continue,
            Err(error) => return Err(failure(&root, error)),
        };
        let mut names = Vec::new();
        for entry in dir.entries().map_err(|error| failure(&root, error))? {
            budget.check()?;
            inspected += 1;
            if inspected > MAXIMUM_WORKSPACE_SKILL_ENTRIES {
                return Err(failure(
                    &root,
                    format!("agent scan exceeds {MAXIMUM_WORKSPACE_SKILL_ENTRIES} entries"),
                ));
            }
            let entry = entry.map_err(|error| failure(&root, error))?;
            if entry
                .file_type()
                .map_err(|error| failure(&root, error))?
                .is_file()
            {
                names.push(entry.file_name());
            }
        }
        names.sort();
        for filename in names {
            let file_path = Path::new(&filename);
            if file_path.extension().is_none_or(|ext| ext != "md") {
                continue;
            }
            let Some(name) = file_path.file_stem().and_then(|name| name.to_str()) else {
                continue;
            };
            if !valid_skill_name(name) {
                continue;
            }
            if id.is_some_and(|id| id != name) || !visited.insert(name.to_owned()) {
                continue;
            }
            let in_prefix = listed < MAXIMUM_FILES;
            listed += usize::from(in_prefix);
            if !in_prefix && !reserved_names.contains(name) {
                continue;
            }
            let path = root.join(&filename);
            let source = path
                .to_string_lossy()
                .chars()
                .flat_map(char::escape_default)
                .collect::<String>();
            if reserved_names.contains(name) {
                selected.insert(
                    name.into(),
                    WorkspaceAgentDefinition {
                        name: name.into(),
                        description: "Unavailable: name conflicts with an inline role".into(),
                        source,
                        text: String::new(),
                        seed: None,
                    },
                );
                continue;
            }
            // Account for retained original text/persona and YAML decoding scratch.
            budget.reserve(MAXIMUM_FILE_BYTES * 4)?;
            let loaded = (|| -> Result<String, String> {
                let mut file = open_directory_regular_file(&dir, file_path)
                    .map_err(|e| e.to_string())?
                    .ok_or("agent file changed or is not a regular file")?;
                let before = file.metadata().map_err(|e| e.to_string())?;
                if before.len() > MAXIMUM_FILE_BYTES as u64 {
                    return Err("agent file exceeds 64 KiB".into());
                }
                let mut raw = String::new();
                file.by_ref()
                    .take(MAXIMUM_FILE_BYTES as u64 + 1)
                    .read_to_string(&mut raw)
                    .map_err(|e| e.to_string())?;
                budget.check().map_err(|e| e.to_string())?;
                let after = file.metadata().map_err(|e| e.to_string())?;
                if raw.len() > MAXIMUM_FILE_BYTES
                    || before.len() != after.len()
                    || before.modified().ok() != after.modified().ok()
                {
                    return Err("agent file changed while being read".into());
                }
                Ok(raw)
            })();
            let (text, parsed) = match loaded {
                Ok(raw) => {
                    let result = parse(name, &source, &raw);
                    (raw, result)
                }
                Err(error) => (String::new(), Err(error)),
            };
            let (description, seed) = match parsed {
                Ok((description, seed)) => (description, Some(seed)),
                Err(error) => (format!("Unavailable: {}", utf8_prefix(&error, 1000)), None),
            };
            selected.insert(
                name.to_owned(),
                WorkspaceAgentDefinition {
                    name: name.into(),
                    description,
                    source,
                    text,
                    seed,
                },
            );
        }
    }
    budget.check()?;
    Ok(selected.into_values().collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn source_failures_escape_and_bound_paths_and_reasons() {
        let path = PathBuf::from(format!("bad\n\u{7f}{}", "界".repeat(4096)));
        let WorkspaceContextError::Failed(text) = failure(&path, "denied\n\0") else {
            panic!("expected failure")
        };
        assert!(text.len() <= 2048);
        assert!(!text.chars().any(char::is_control));
        assert!(text.contains("bad\\n\\u{7f}"));
        assert!(text.contains("denied\\n\\0"));
    }
}
