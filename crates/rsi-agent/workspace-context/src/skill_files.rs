//! Skill directories authorize link targets independently of instruction roots.

use super::*;

#[derive(Debug)]
struct SkillDirectory {
    path: PathBuf,
    handle: Dir,
}

impl SkillDirectory {
    fn open(path: &Path) -> std::io::Result<Self> {
        let path = fs::canonicalize(path)?;
        #[cfg(unix)]
        let handle = open_absolute_directory_no_follow(&path)?;
        #[cfg(not(unix))]
        let handle = Dir::open_ambient_dir(&path, ambient_authority())?;
        Ok(Self { path, handle })
    }
}

#[derive(Clone, Debug)]
pub(super) struct SkillSource {
    pub(super) logical_path: PathBuf,
    resolved_path: PathBuf,
    directory: Arc<SkillDirectory>,
}

impl SkillSource {
    fn new(logical_path: PathBuf, directory: Arc<SkillDirectory>) -> Self {
        let name = logical_path.file_name().expect("discovered skill filename");
        Self {
            resolved_path: directory.path.join(name),
            logical_path,
            directory,
        }
    }

    pub(super) fn open(&self) -> std::io::Result<Option<File>> {
        open_directory_regular_file(
            &self.directory.handle,
            Path::new(
                self.logical_path
                    .file_name()
                    .expect("discovered skill filename"),
            ),
        )
    }

    fn retained_bytes(&self) -> Result<usize, WorkspaceContextError> {
        [
            self.logical_path.capacity(),
            self.resolved_path.capacity(),
            self.directory.path.capacity(),
            // Conservatively charge shared directory handles for every selection.
            std::mem::size_of::<Self>() + std::mem::size_of::<SkillDirectory>() + 512,
        ]
        .into_iter()
        .try_fold(0_usize, |sum, bytes| {
            sum.checked_add(bytes)
                .ok_or(WorkspaceContextError::Capacity)
        })
    }
}

#[derive(Default)]
pub(super) struct SkillDiscovery {
    selected: BTreeMap<String, SelectedSkill>,
    identities: BTreeSet<PathBuf>,
    inspected: usize,
}

impl SkillDiscovery {
    pub(super) fn into_selected(self) -> Vec<SelectedSkill> {
        self.selected.into_values().collect()
    }

    pub(super) fn scan(
        &mut self,
        root: &Path,
        project_root: Option<&Path>,
        observation: &mut Observation,
        budget: &mut SnapshotBudget,
    ) -> Result<(), WorkspaceContextError> {
        budget.check()?;
        let Some(directory) =
            optional_directory_result(SkillDirectory::open(root), root, observation)
        else {
            return Ok(());
        };
        let directory = Arc::new(directory);
        // Includes the bounded entry vector, logical paths and resolved-path scratch.
        let scratch = root
            .as_os_str()
            .len()
            .checked_add(directory.path.capacity())
            .and_then(|bytes| bytes.checked_add(1024))
            .and_then(|bytes| bytes.checked_mul((MAXIMUM_WORKSPACE_SKILL_ENTRIES + 1) * 4))
            .ok_or(WorkspaceContextError::Capacity)?;
        budget.reserve(scratch)?;
        let result = self.scan_directory(root, project_root, &directory, observation, budget);
        budget.release(scratch);
        result
    }

    fn scan_directory(
        &mut self,
        root: &Path,
        project_root: Option<&Path>,
        directory: &Arc<SkillDirectory>,
        observation: &mut Observation,
        budget: &mut SnapshotBudget,
    ) -> Result<(), WorkspaceContextError> {
        let entries = match directory.handle.entries() {
            Ok(entries) => entries,
            Err(error) => {
                observation.io(root, "list skill directory", &error);
                return Ok(());
            }
        };
        let remaining = MAXIMUM_WORKSPACE_SKILL_ENTRIES.saturating_sub(self.inspected);
        let mut retained = Vec::new();
        for (index, entry) in entries.take(remaining.saturating_add(1)).enumerate() {
            budget.check()?;
            if index == remaining {
                observation.fail(root, "skill entry scan limit exceeded");
                break;
            }
            self.inspected += 1;
            match entry.and_then(|entry| Ok((entry.file_name(), entry.file_type()?))) {
                Ok(entry) => retained.push(entry),
                Err(error) => observation.io(root, "read skill directory entry", &error),
            }
        }
        retained.sort_unstable_by(|left, right| left.0.cmp(&right.0));
        for (name, kind) in retained {
            budget.check()?;
            let logical_path = root.join(&name);
            let source = if kind.is_dir() || kind.is_symlink() {
                let target = directory.path.join(&name);
                if kind.is_symlink() {
                    let Some(metadata) = optional_directory_result(
                        fs::metadata(&target),
                        &logical_path,
                        observation,
                    ) else {
                        continue;
                    };
                    if !metadata.is_dir() {
                        continue;
                    }
                }
                let Some(target) = optional_directory_result(
                    SkillDirectory::open(&target),
                    &logical_path,
                    observation,
                ) else {
                    continue;
                };
                SkillSource::new(logical_path.join("SKILL.md"), Arc::new(target))
            } else if kind.is_file() && logical_path.extension().is_some_and(|ext| ext == "md") {
                SkillSource::new(logical_path, Arc::clone(directory))
            } else {
                continue;
            };
            self.select(source, project_root, observation, budget)?;
        }
        Ok(())
    }

    fn select(
        &mut self,
        source: SkillSource,
        project_root: Option<&Path>,
        observation: &mut Observation,
        budget: &mut SnapshotBudget,
    ) -> Result<(), WorkspaceContextError> {
        if self.identities.contains(&source.resolved_path) {
            return Ok(());
        }
        let Some(raw) = read_skill_metadata_prefix(&source, observation, &budget.cancellation)
        else {
            return Ok(());
        };
        let Some(skill) = parse_skill(source, project_root, &raw) else {
            return Ok(());
        };
        budget.reserve(skill.file.resolved_path.capacity() + std::mem::size_of::<PathBuf>() * 4)?;
        self.identities.insert(skill.file.resolved_path.clone());
        if !self.selected.contains_key(&skill.name) {
            budget.reserve(
                skill.file.retained_bytes()?
                    + skill.name.capacity() * 2
                    + skill.description.capacity()
                    + skill.source.capacity()
                    + std::mem::size_of::<SelectedSkill>() * 2,
            )?;
            self.selected.insert(skill.name.clone(), skill);
        }
        Ok(())
    }
}

fn optional_directory_result<T>(
    result: std::io::Result<T>,
    path: &Path,
    observation: &mut Observation,
) -> Option<T> {
    match result {
        Ok(value) => Some(value),
        Err(error) if optional_directory_missing(&error) => None,
        Err(error) => {
            observation.io(path, "resolve skill directory", &error);
            None
        }
    }
}

pub(super) fn optional_directory_missing(error: &std::io::Error) -> bool {
    matches!(
        error.kind(),
        std::io::ErrorKind::NotFound | std::io::ErrorKind::NotADirectory
    ) || directory_link_loop(error)
}

fn directory_link_loop(error: &std::io::Error) -> bool {
    #[cfg(unix)]
    {
        error.raw_os_error() == Some(libc::ELOOP)
    }
    #[cfg(windows)]
    {
        error.raw_os_error() == Some(1921)
    } // ERROR_CANT_RESOLVE_FILENAME
    #[cfg(not(any(unix, windows)))]
    {
        let _ = error;
        false
    }
}

#[cfg(test)]
pub(super) fn test_source() -> SkillSource {
    let temporary = tempfile::tempdir().unwrap();
    let directory = Arc::new(SkillDirectory::open(temporary.path()).unwrap());
    SkillSource::new(temporary.path().join("SKILL.md"), directory)
}

#[cfg(test)]
#[path = "skill_files/tests.rs"]
mod tests;
