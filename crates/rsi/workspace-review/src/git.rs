use rsi_files_protocol::{FileKind, Files, FilesBinding, FilesCaller, FilesError, RelativePath};
use rsi_process::{ManagedProcess, Process, ProcessSpec};
use rsi_sandbox::{ProcessRequest, ProcessStdio, Sandbox, SandboxMode, WorkspaceReadRequest};
use rsi_workspace_review_api::{FileChange, Omission, OmissionKind};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
    sync::Arc,
};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};
use tokio_util::sync::CancellationToken;

pub(super) type Result<T> = std::result::Result<T, OmissionKind>;
const FILE_BYTES: usize = 4 * 1024 * 1024;
const CAPTURE_BYTES: usize = 128 * 1024 * 1024;
const INTERVAL_SCRATCH: u32 = 512 * 1024 * 1024;
#[derive(Debug)]
pub(super) struct Git {
    pub process: Arc<dyn Process>,
    pub sandbox: Arc<dyn Sandbox>,
    pub files: Arc<dyn Files>,
    pub program: PathBuf,
    pub quota: Arc<Semaphore>,
    pub tasks: tokio_util::task::TaskTracker,
}
#[derive(Debug)]
pub(super) struct Scratch {
    pub directory: Option<tempfile::TempDir>,
    pub tasks: tokio_util::task::TaskTracker,
    pub permits: Vec<OwnedSemaphorePermit>,
    pub charged: u32,
}
impl Scratch {
    pub fn path(&self) -> &Path {
        self.directory.as_ref().expect("live scratch").path()
    }
    fn reserve(&mut self, quota: &Arc<Semaphore>, bytes: usize) -> Result<()> {
        // Git loose-object overhead, index/tree names and compression expansion are
        // charged conservatively, before feeding any original bytes to Git.
        let bytes = u32::try_from(bytes).map_err(|_| OmissionKind::Limit)?;
        if self
            .charged
            .checked_add(bytes)
            .is_none_or(|n| n > INTERVAL_SCRATCH)
        {
            return Err(OmissionKind::Capacity);
        }
        let permit = quota
            .clone()
            .try_acquire_many_owned(bytes)
            .map_err(|_| OmissionKind::Capacity)?;
        self.charged += bytes;
        self.permits.push(permit);
        Ok(())
    }
}
#[derive(Debug, Default)]
pub(super) struct Capture {
    files: BTreeMap<String, (String, u32)>,
    excluded: BTreeSet<String>,
    listed_complete: bool,
    pub omissions: Vec<Omission>,
}
impl Capture {
    fn omit(&mut self, reason: OmissionKind) {
        if let Some(row) = self.omissions.iter_mut().find(|row| row.kind == reason) {
            row.count = row.count.saturating_add(1);
        } else {
            self.omissions.push(Omission {
                kind: reason,
                count: 1,
            });
        }
        self.omissions.sort_by_key(|row| row.kind);
    }
    fn known(&self, path: &str) -> bool {
        !self.excluded.contains(path) && (self.listed_complete || self.files.contains_key(path))
    }
}
struct Terminate(ManagedProcess);
impl Drop for Terminate {
    fn drop(&mut self) {
        self.0.terminate();
    }
}
impl Git {
    async fn run(
        &self,
        cwd: &Path,
        scratch: Option<&Path>,
        args: &[&str],
        input: Vec<u8>,
        stop: &CancellationToken,
    ) -> Result<Vec<u8>> {
        if stop.is_cancelled() {
            return Err(OmissionKind::Deadline);
        }
        let mut arguments: Vec<String> = [
            "-c",
            "core.fsmonitor=false",
            "-c",
            "core.untrackedCache=false",
            "-c",
            "core.hooksPath=/dev/null",
            "-c",
            "core.quotePath=false",
            "-c",
            "gc.auto=0",
        ]
        .into_iter()
        .map(str::to_owned)
        .collect();
        arguments.extend(args.iter().map(|s| (*s).to_owned()));
        let process = self
            .sandbox
            .confine(ProcessRequest {
                stdio: ProcessStdio::Pipes,
                mode: if scratch.is_some() {
                    SandboxMode::WorkspaceWrite
                } else {
                    SandboxMode::ReadOnly
                },
                program: self.program.clone(),
                arguments,
                cwd: cwd.to_owned(),
                workspace: cwd.to_owned(),
            })
            .await
            .map_err(|_| OmissionKind::Git)?;
        let mut environment: Vec<_> = [
            ("PATH", "/usr/bin:/bin"),
            ("LC_ALL", "C"),
            ("GIT_CONFIG_NOSYSTEM", "1"),
            ("GIT_CONFIG_GLOBAL", "/dev/null"),
            ("GIT_CONFIG_SYSTEM", "/dev/null"),
            ("GIT_ATTR_NOSYSTEM", "1"),
            ("GIT_OPTIONAL_LOCKS", "0"),
            ("GIT_TERMINAL_PROMPT", "0"),
            ("GIT_LITERAL_PATHSPECS", "1"),
        ]
        .into_iter()
        .map(|(k, v)| (k.into(), v.into()))
        .collect();
        if let Some(scratch) = scratch {
            environment.push((
                "GIT_DIR".into(),
                scratch.join("repository").into_os_string(),
            ));
            environment.push((
                "GIT_INDEX_FILE".into(),
                scratch.join("index").into_os_string(),
            ));
            environment.push(("HOME".into(), scratch.as_os_str().to_owned()));
        }
        let process = self
            .process
            .spawn(ProcessSpec {
                process,
                stdin: input,
                environment,
                stdout_max_bytes: FILE_BYTES,
                stderr_max_bytes: 8192,
                termination_grace_ms: 200,
            })
            .map_err(|_| OmissionKind::Git)?;
        let guard = Terminate(process.clone());
        let outcome = tokio::select! {biased;()=stop.cancelled()=>{process.terminate();let _=process.wait().await;return Err(OmissionKind::Deadline);},outcome=process.wait()=>outcome.map_err(|_|OmissionKind::Git)?};
        let output = process
            .stdout()
            .read_from(0)
            .map_err(|_| OmissionKind::Git)?;
        drop(guard);
        if outcome.exit_code != Some(0) {
            return Err(OmissionKind::Git);
        }
        if output.lossy || output.oldest_offset != 0 || output.next_offset > FILE_BYTES as u64 {
            return Err(OmissionKind::Limit);
        }
        Ok(output.bytes)
    }
    pub async fn initialize(&self, base: &Path, stop: &CancellationToken) -> Result<Scratch> {
        let mut scratch = Scratch {
            directory: None,
            tasks: self.tasks.clone(),
            permits: vec![],
            charged: 0,
        };
        scratch.reserve(&self.quota, 4 * 1024 * 1024)?;
        scratch.directory = Some(
            crate::scratch_root::interval_directory(base).map_err(|_| OmissionKind::Capacity)?,
        );
        let path = scratch.path().to_owned();
        self.run(
            &path,
            Some(&path),
            &["init", "--bare", "--quiet", "--template=", "repository"],
            vec![],
            stop,
        )
        .await?;
        Ok(scratch)
    }
    #[expect(
        clippy::too_many_lines,
        reason = "one bounded capture keeps admission, reads and exclusions in order"
    )]
    pub async fn capture(
        &self,
        workspace: &Path,
        scratch: &mut Scratch,
        stop: &CancellationToken,
    ) -> Capture {
        let mut capture = Capture::default();
        let listing = match self
            .run(
                workspace,
                None,
                &[
                    "ls-files",
                    "-z",
                    "--cached",
                    "--others",
                    "--exclude-standard",
                    "--",
                    ".",
                ],
                vec![],
                stop,
            )
            .await
        {
            Ok(bytes) => bytes,
            Err(reason) => {
                capture.omit(if reason == OmissionKind::Git {
                    OmissionKind::NonGit
                } else {
                    reason
                });
                return capture;
            }
        };
        let mut names = BTreeSet::new();
        let mut retained = 0;
        capture.listed_complete = true;
        for bytes in listing.split(|b| *b == 0).filter(|s| !s.is_empty()) {
            let Ok(path) = std::str::from_utf8(bytes) else {
                capture.omit(OmissionKind::Unreadable);
                continue;
            };
            if rsi_workspace_review_api::path(path).is_err() {
                capture.omit(OmissionKind::Unreadable);
                continue;
            }
            if names.contains(path) {
                continue;
            }
            if names.len() >= 10_000 || retained + path.len() > 1024 * 1024 {
                capture.listed_complete = false;
                capture.omit(OmissionKind::Limit);
                break;
            }
            retained += path.len();
            names.insert(path.to_owned());
        }
        let caller = FilesCaller::default();
        let scope = self
            .sandbox
            .workspace_read(WorkspaceReadRequest {
                mode: SandboxMode::ReadOnly,
                cwd: workspace.to_owned(),
                workspace: workspace.to_owned(),
            })
            .await;
        if scope.is_err() {
            capture.listed_complete = false;
            capture.omit(OmissionKind::Unreadable);
            return capture;
        }
        let binding = FilesBinding::new(
            caller.clone(),
            "workspace-review",
            "1",
            workspace.to_owned(),
        )
        .expect("trusted canonical workspace");
        let mode_root = match ModeRoot::open(workspace).await {
            Ok(root) => root,
            Err(reason) => {
                capture.listed_complete = false;
                capture.omit(reason);
                return capture;
            }
        };
        let mut bytes = 0;
        let mut batch = Vec::new();
        let mut batch_bytes = 0;
        for path in names {
            if stop.is_cancelled() {
                capture.listed_complete = false;
                capture.omit(OmissionKind::Deadline);
                break;
            }
            let Ok(relative) = RelativePath::new(path.as_bytes()) else {
                capture.excluded.insert(path);
                capture.omit(OmissionKind::Unreadable);
                continue;
            };
            let opened = match self
                .files
                .open(binding.clone(), relative, FileKind::File, stop.clone())
                .await
            {
                Ok(file) => file,
                // Git also lists tracked deletions. Files open maps absence to
                // Unavailable; it is an observed deletion, not an unreadable file.
                Err(FilesError::Unavailable) => continue,
                Err(_) => {
                    capture.excluded.insert(path);
                    capture.omit(OmissionKind::Unreadable);
                    continue;
                }
            };
            let result = async {
                let length = usize::try_from(opened.length).map_err(|_| OmissionKind::Limit)?;
                if length > FILE_BYTES || bytes + length > CAPTURE_BYTES {
                    return Err(OmissionKind::Limit);
                }
                bytes += length;
                let mode = file_mode(&mode_root, &path).await?;
                let data = self.read_file(&binding, &opened, stop).await?;
                if data.contains(&0) || std::str::from_utf8(&data).is_err() {
                    return Err(OmissionKind::Binary);
                }
                scratch.reserve(
                    &self.quota,
                    length.saturating_mul(2) + 4096 + path.len() * 128,
                )?;
                Ok((data, mode))
            }
            .await;
            let _ = self.files.release(&binding, &opened.token);
            match result {
                Ok((data, mode)) => {
                    batch_bytes += data.len();
                    batch.push((path, data, mode));
                    if batch.len() >= 64 || batch_bytes >= 4 * 1024 * 1024 {
                        self.import(scratch, &mut capture, std::mem::take(&mut batch), stop)
                            .await;
                        batch_bytes = 0;
                    }
                }
                Err(reason) => {
                    capture.excluded.insert(path);
                    capture.omit(reason);
                }
            }
        }
        if !batch.is_empty() {
            self.import(scratch, &mut capture, batch, stop).await;
        }
        self.files.release_caller(&caller);
        capture
    }
    async fn import(
        &self,
        scratch: &Scratch,
        capture: &mut Capture,
        batch: Vec<(String, Vec<u8>, u32)>,
        stop: &CancellationToken,
    ) {
        use std::io::Write as _;
        let mut input = Vec::new();
        for (index, (_, data, _)) in batch.iter().enumerate() {
            let mark = index + 1;
            writeln!(input, "blob\nmark :{mark}\ndata {}", data.len()).expect("Vec write");
            input.extend_from_slice(data);
            writeln!(input, "\nget-mark :{mark}").expect("Vec write");
        }
        input.extend_from_slice(b"done\n");
        let result = self
            .run(
                scratch.path(),
                Some(scratch.path()),
                &["fast-import", "--quiet", "--done"],
                input,
                stop,
            )
            .await
            .and_then(|output| {
                let ids = output
                    .split(|byte| *byte == b'\n')
                    .filter(|line| !line.is_empty())
                    .map(object_id)
                    .collect::<Result<Vec<_>>>()?;
                if ids.len() != batch.len() {
                    return Err(OmissionKind::Git);
                }
                Ok(ids)
            });
        match result {
            Ok(ids) => {
                for ((path, _, mode), oid) in batch.into_iter().zip(ids) {
                    capture.files.insert(path, (oid, mode));
                }
            }
            Err(reason) => {
                for (path, _, _) in batch {
                    capture.excluded.insert(path);
                    capture.omit(reason);
                }
            }
        }
    }
    async fn read_file(
        &self,
        binding: &FilesBinding,
        opened: &rsi_files_protocol::OpenedFile,
        stop: &CancellationToken,
    ) -> Result<Vec<u8>> {
        let mut bytes =
            Vec::with_capacity(usize::try_from(opened.length).map_err(|_| OmissionKind::Limit)?);
        while (bytes.len() as u64) < opened.length {
            if stop.is_cancelled() {
                return Err(OmissionKind::Deadline);
            }
            let page = self
                .files
                .read(
                    binding.clone(),
                    opened.token.clone(),
                    bytes.len() as u64,
                    65536,
                    stop.clone(),
                )
                .await
                .map_err(|_| OmissionKind::Unreadable)?;
            let chunk = hex::decode(page.bytes_hex).map_err(|_| OmissionKind::Unreadable)?;
            if page.total != opened.length
                || page.offset != bytes.len() as u64
                || chunk.is_empty()
                || bytes.len() + chunk.len() > FILE_BYTES
            {
                return Err(OmissionKind::Unreadable);
            }
            bytes.extend(chunk);
        }
        self.files
            .describe(binding, &opened.token)
            .map_err(|_| OmissionKind::Unreadable)?;
        Ok(bytes)
    }
    async fn tree(
        &self,
        scratch: &Scratch,
        files: &BTreeMap<String, (String, u32)>,
        included: &BTreeSet<String>,
        stop: &CancellationToken,
    ) -> Result<String> {
        self.run(
            scratch.path(),
            Some(scratch.path()),
            &["read-tree", "--empty"],
            vec![],
            stop,
        )
        .await?;
        let mut entries = Vec::new();
        for (path, (oid, mode)) in files {
            if included.contains(path) {
                entries.extend_from_slice(format!("{mode:o} {oid}\t{path}\0").as_bytes());
            }
        }
        if entries.len() > FILE_BYTES {
            return Err(OmissionKind::Limit);
        }
        self.run(
            scratch.path(),
            Some(scratch.path()),
            &["update-index", "-z", "--index-info"],
            entries,
            stop,
        )
        .await?;
        object_id(
            &self
                .run(
                    scratch.path(),
                    Some(scratch.path()),
                    &["write-tree"],
                    vec![],
                    stop,
                )
                .await?,
        )
    }
    pub async fn compare(
        &self,
        scratch: &Scratch,
        before: &Capture,
        after: &Capture,
        stop: &CancellationToken,
    ) -> Result<Comparison> {
        let included: BTreeSet<_> = before
            .files
            .keys()
            .chain(after.files.keys())
            .filter(|p| before.known(p) && after.known(p))
            .cloned()
            .collect();
        let old = self.tree(scratch, &before.files, &included, stop).await?;
        let new = self.tree(scratch, &after.files, &included, stop).await?;
        let bytes = self
            .run(
                scratch.path(),
                Some(scratch.path()),
                &[
                    "diff-tree",
                    "--no-commit-id",
                    "--numstat",
                    "-z",
                    "-r",
                    "-M",
                    "--no-ext-diff",
                    "--no-textconv",
                    &old,
                    &new,
                ],
                vec![],
                stop,
            )
            .await?;
        Ok(Comparison {
            before: old,
            after: new,
            files: numstat(&bytes)?,
        })
    }
    pub async fn diff(
        &self,
        scratch: &Scratch,
        comparison: &Comparison,
        file: &FileChange,
        stop: &CancellationToken,
    ) -> Result<String> {
        let mut args = vec![
            "diff-tree",
            "--no-commit-id",
            "-p",
            "-r",
            "-M",
            "--no-color",
            "--no-ext-diff",
            "--no-textconv",
            &comparison.before,
            &comparison.after,
            "--",
            &file.path,
        ];
        if let Some(previous) = &file.previous_path {
            args.push(previous);
        }
        String::from_utf8(
            self.run(scratch.path(), Some(scratch.path()), &args, vec![], stop)
                .await?,
        )
        .map_err(|_| OmissionKind::Binary)
    }
}
#[derive(Debug)]
pub(super) struct Comparison {
    pub before: String,
    pub after: String,
    pub files: Vec<FileChange>,
}
fn object_id(bytes: &[u8]) -> Result<String> {
    let value = std::str::from_utf8(bytes)
        .map_err(|_| OmissionKind::Git)?
        .trim();
    if !matches!(value.len(), 40 | 64)
        || !value
            .bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
    {
        return Err(OmissionKind::Git);
    }
    Ok(value.into())
}
fn numstat(bytes: &[u8]) -> Result<Vec<FileChange>> {
    let mut parts = bytes.split(|b| *b == 0);
    let mut files = Vec::new();
    while let Some(line) = parts.next() {
        if line.is_empty() {
            continue;
        }
        let value = std::str::from_utf8(line).map_err(|_| OmissionKind::Git)?;
        let mut fields = value.splitn(3, '\t');
        let added = fields
            .next()
            .ok_or(OmissionKind::Git)?
            .parse()
            .map_err(|_| OmissionKind::Binary)?;
        let removed = fields
            .next()
            .ok_or(OmissionKind::Git)?
            .parse()
            .map_err(|_| OmissionKind::Binary)?;
        let name = fields.next().ok_or(OmissionKind::Git)?;
        let (path, previous_path) = if name.is_empty() {
            let old = std::str::from_utf8(parts.next().ok_or(OmissionKind::Git)?)
                .map_err(|_| OmissionKind::Git)?;
            let new = std::str::from_utf8(parts.next().ok_or(OmissionKind::Git)?)
                .map_err(|_| OmissionKind::Git)?;
            (new.to_owned(), Some(old.to_owned()))
        } else {
            (name.to_owned(), None)
        };
        let file = FileChange {
            path,
            previous_path,
            added,
            removed,
        };
        file.validate().map_err(|_| OmissionKind::Git)?;
        if files.len() >= 20_000 {
            return Err(OmissionKind::Limit);
        }
        files.push(file);
    }
    files.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(files)
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let directory = self.directory.take();
        let permits = std::mem::take(&mut self.permits);
        self.tasks.spawn_blocking(move || {
            drop(directory);
            drop(permits);
        });
    }
}
async fn file_mode(root: &ModeRoot, path: &str) -> Result<u32> {
    #[cfg(unix)]
    {
        let root = root.0.clone();
        let path = path.to_owned();
        tokio::task::spawn_blocking(move || {
            use std::os::unix::fs::PermissionsExt as _;
            let file = rsi_files_native_fs::open_relative_file_no_follow(&root, Path::new(&path))
                .map_err(|_| OmissionKind::Unreadable)?;
            let mode = file
                .metadata()
                .map_err(|_| OmissionKind::Unreadable)?
                .permissions()
                .mode();
            Ok(if mode & 0o111 == 0 {
                0o100_644
            } else {
                0o100_755
            })
        })
        .await
        .map_err(|_| OmissionKind::Unreadable)?
    }
    #[cfg(not(unix))]
    {
        let _ = (root, path);
        Ok(0o100_644)
    }
}

struct ModeRoot(#[cfg(unix)] Arc<cap_std::fs::Dir>);
impl ModeRoot {
    async fn open(workspace: &Path) -> Result<Self> {
        #[cfg(unix)]
        {
            let workspace = workspace.to_owned();
            tokio::task::spawn_blocking(move || {
                rsi_files_native_fs::open_absolute_directory_no_follow(&workspace)
                    .map(|file| Self(Arc::new(file)))
                    .map_err(|_| OmissionKind::Unreadable)
            })
            .await
            .map_err(|_| OmissionKind::Unreadable)?
        }
        #[cfg(not(unix))]
        {
            let _ = workspace;
            Ok(Self())
        }
    }
}
