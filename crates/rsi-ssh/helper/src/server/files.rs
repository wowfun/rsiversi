use super::{Server, lock};
use rsi_files_protocol::{
    DirectoryPage, FileKind, FilesBinding, FilesCaller, FilesError, OpenedFile, RelativePath,
    Result,
};
use rsi_ssh_protocol::rpc::Reply;
use std::{collections::BTreeMap, sync::Mutex};
#[derive(Default)]
pub(super) struct FileRegistry {
    caller: FilesCaller,
    next: u64,
    entries: BTreeMap<u64, Open>,
}
#[derive(Clone)]
struct Open {
    binding: FilesBinding,
    opened: OpenedFile,
}
impl Server {
    fn prune_files(&self, registry: &mut FileRegistry) {
        registry.entries.retain(|_, open| {
            if self
                .owner
                .capabilities
                .files
                .describe(&open.binding, &open.opened.token)
                .is_ok()
            {
                true
            } else {
                let _ = self
                    .owner
                    .capabilities
                    .files
                    .release(&open.binding, &open.opened.token);
                false
            }
        });
    }
    pub(super) async fn files_open(
        &self,
        workspace: String,
        path: RelativePath,
        kind: FileKind,
    ) -> Reply {
        let result = async {
            rsi_ssh_protocol::execution::validate_path(&workspace)
                .map_err(|_| FilesError::Invalid)?;
            let (handle, binding) = {
                let mut registry = lock(&self.files);
                self.prune_files(&mut registry);
                if registry.entries.len() >= rsi_files_protocol::MAXIMUM_FILE_TOKENS {
                    return Err(FilesError::Capacity);
                }
                let handle = registry.next.checked_add(1).ok_or(FilesError::Capacity)?;
                registry.next = handle;
                let binding = FilesBinding::new(
                    registry.caller.clone(),
                    &handle.to_string(),
                    &self.connection.epoch().to_string(),
                    workspace.into(),
                )?;
                (handle, binding)
            };
            let opened = self
                .owner
                .capabilities
                .files
                .open(binding.clone(), path, kind, self.stop.clone())
                .await?;
            let mut registry = lock(&self.files);
            if self.stop.is_cancelled()
                || registry.entries.len() >= rsi_files_protocol::MAXIMUM_FILE_TOKENS
            {
                let _ = self
                    .owner
                    .capabilities
                    .files
                    .release(&binding, &opened.token);
                return Err(if self.stop.is_cancelled() {
                    FilesError::Cancelled
                } else {
                    FilesError::Capacity
                });
            }
            registry.entries.insert(
                handle,
                Open {
                    binding,
                    opened: opened.clone(),
                },
            );
            Ok(Reply::FilesOpened { handle, opened })
        }
        .await;
        result.unwrap_or_else(|failure| Reply::FilesFailed { failure })
    }
    fn file(&self, handle: u64) -> Result<Open> {
        let registry = lock(&self.files);
        registry
            .entries
            .get(&handle)
            .cloned()
            .ok_or(FilesError::Unavailable)
    }
    pub(super) async fn files_read(&self, handle: u64, offset: u64, maximum: usize) -> Reply {
        let result = async {
            let open = self.file(handle)?;
            self.owner
                .capabilities
                .files
                .read(
                    open.binding,
                    open.opened.token,
                    offset,
                    maximum,
                    self.stop.clone(),
                )
                .await
        }
        .await;
        match result {
            Ok(page) => Reply::FilePage { page },
            Err(failure) => Reply::FilesFailed { failure },
        }
    }
    pub(super) async fn files_list(&self, handle: u64, offset: usize, maximum: usize) -> Reply {
        let result: Result<DirectoryPage> = async {
            let open = self.file(handle)?;
            self.owner
                .capabilities
                .files
                .list(
                    open.binding,
                    open.opened.token,
                    offset,
                    maximum,
                    self.stop.clone(),
                )
                .await
        }
        .await;
        match result {
            Ok(page) => Reply::DirectoryPage { page },
            Err(failure) => Reply::FilesFailed { failure },
        }
    }
    pub(super) fn files_release(&self, handle: u64) -> Reply {
        let open = lock(&self.files).entries.remove(&handle);
        let result = open.map_or(Ok(()), |open| {
            self.owner
                .capabilities
                .files
                .release(&open.binding, &open.opened.token)
        });
        match result {
            Ok(()) => Reply::Done,
            Err(failure) => Reply::FilesFailed { failure },
        }
    }
    pub(super) fn close_files(&self) {
        let mut registry = lock(&self.files);
        self.owner
            .capabilities
            .files
            .release_caller(&registry.caller);
        registry.entries.clear();
    }
}
pub(super) fn registry() -> Mutex<FileRegistry> {
    Mutex::new(FileRegistry::default())
}
