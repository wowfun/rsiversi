use super::{ProcessConnection, ProcessError, Reply, Request};
use async_trait::async_trait;
use rsi_files_protocol::{
    DirectoryPage, FileKind, FilePage, FileToken, Files, FilesBinding, FilesCaller, FilesError,
    OpenedFile, RelativePath, Result,
};
use rsi_ssh_transport::Control;
use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};
use tokio::sync::{OwnedSemaphorePermit, Semaphore, oneshot};
use tokio_util::sync::CancellationToken;
fn lock<T>(value: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    value
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}
#[derive(Debug)]
struct Open {
    client: ProcessConnection,
    binding: FilesBinding,
    opened: OpenedFile,
    handle: u64,
    deadline: Instant,
    _permit: OwnedSemaphorePermit,
}
impl Drop for Open {
    fn drop(&mut self) {
        let client = self.client.clone();
        let handle = self.handle;
        let Ok(runtime) = tokio::runtime::Handle::try_current() else {
            client.transport.close();
            return;
        };
        runtime.spawn(async move {
            let cleanup = async {
                loop {
                    match client.control(Control::FilesRelease { handle }).await {
                        Err(ProcessError::Capacity) => {
                            tokio::time::sleep(Duration::from_millis(1)).await;
                        }
                        result => return result,
                    }
                }
            };
            if !matches!(
                tokio::time::timeout(Duration::from_secs(5), cleanup).await,
                Ok(Ok(()))
            ) {
                client.transport.close();
            }
        });
    }
}
#[derive(Debug)]
struct Inner {
    client: ProcessConnection,
    entries: Mutex<BTreeMap<FileToken, Arc<Open>>>,
    slots: Arc<Semaphore>,
}
/// Files adapter whose handles retain their issuing target connection and caller binding.
#[derive(Clone, Debug)]
pub struct RemoteFiles(Arc<Inner>);
struct Publication {
    owner: RemoteFiles,
    opened: Option<OpenedFile>,
    binding: FilesBinding,
}
impl Drop for Publication {
    fn drop(&mut self) {
        if let Some(opened) = &self.opened {
            let _ = self.owner.release(&self.binding, &opened.token);
        }
    }
}
impl RemoteFiles {
    /// Creates one bounded token namespace for the supplied admitted target connection.
    pub fn new(client: ProcessConnection) -> Self {
        Self(Arc::new(Inner {
            client,
            entries: Mutex::new(BTreeMap::new()),
            slots: Arc::new(Semaphore::new(rsi_files_protocol::MAXIMUM_FILE_TOKENS)),
        }))
    }
    fn lookup(&self, binding: &FilesBinding, token: &FileToken) -> Result<Arc<Open>> {
        if self.0.client.transport.is_closed() {
            return Err(FilesError::Cancelled);
        }
        let mut entries = lock(&self.0.entries);
        entries.retain(|_, entry| entry.deadline > Instant::now());
        let entry = entries.get(token).ok_or(FilesError::Unavailable)?;
        if &entry.binding != binding {
            return Err(FilesError::Binding);
        }
        Ok(entry.clone())
    }
    async fn call(&self, request: Request) -> Result<Reply> {
        match self
            .0
            .client
            .call(request)
            .await
            .map_err(|error| process_error(&error))?
        {
            Reply::FilesFailed { failure } => Err(failure),
            reply => Ok(reply),
        }
    }
    fn malformed(&self) -> FilesError {
        self.0.client.transport.close();
        FilesError::OutcomeUnknown
    }
    async fn open_owned(
        &self,
        binding: FilesBinding,
        path: RelativePath,
        kind: FileKind,
        cancellation: CancellationToken,
    ) -> Result<Publication> {
        if cancellation.is_cancelled() {
            return Err(FilesError::Cancelled);
        }
        lock(&self.0.entries).retain(|_, entry| entry.deadline > Instant::now());
        let permit = self
            .0
            .slots
            .clone()
            .try_acquire_owned()
            .map_err(|_| FilesError::Capacity)?;
        let workspace = binding
            .workspace()
            .to_str()
            .ok_or(FilesError::Invalid)?
            .to_owned();
        rsi_ssh_protocol::execution::validate_path(&workspace).map_err(|_| FilesError::Invalid)?;
        let deadline = Instant::now() + rsi_files_protocol::FILE_TOKEN_LIFETIME;
        let (handle, opened) = match self
            .call(Request::FilesOpen {
                workspace,
                path: path.clone(),
                kind,
            })
            .await?
        {
            Reply::FilesOpened { handle, opened }
                if handle != 0 && opened.validate_for(&path, kind).is_ok() =>
            {
                (handle, opened)
            }
            _ => return Err(self.malformed()),
        };
        let entry = Arc::new(Open {
            client: self.0.client.clone(),
            binding: binding.clone(),
            opened: opened.clone(),
            handle,
            deadline,
            _permit: permit,
        });
        if cancellation.is_cancelled() {
            return Err(FilesError::Cancelled);
        }
        let mut entries = lock(&self.0.entries);
        if entries.contains_key(&opened.token)
            || entries.values().any(|entry| entry.handle == handle)
        {
            return Err(self.malformed());
        }
        entries.insert(opened.token.clone(), entry);
        Ok(Publication {
            owner: self.clone(),
            opened: Some(opened),
            binding,
        })
    }
}
#[async_trait]
impl Files for RemoteFiles {
    fn release_caller(&self, caller: &FilesCaller) {
        lock(&self.0.entries).retain(|_, entry| entry.binding.caller() != caller);
    }
    fn describe(&self, binding: &FilesBinding, token: &FileToken) -> Result<OpenedFile> {
        Ok(self.lookup(binding, token)?.opened.clone())
    }
    async fn open(
        &self,
        binding: FilesBinding,
        path: RelativePath,
        kind: FileKind,
        cancellation: CancellationToken,
    ) -> Result<OpenedFile> {
        let owner = self.clone();
        let (reply, receive) = oneshot::channel();
        tokio::spawn(async move {
            let _ = reply.send(owner.open_owned(binding, path, kind, cancellation).await);
        });
        let mut publication = receive.await.map_err(|_| FilesError::OutcomeUnknown)??;
        publication.opened.take().ok_or(FilesError::OutcomeUnknown)
    }
    async fn read(
        &self,
        binding: FilesBinding,
        token: FileToken,
        offset: u64,
        maximum: usize,
        cancellation: CancellationToken,
    ) -> Result<FilePage> {
        if cancellation.is_cancelled() {
            return Err(FilesError::Cancelled);
        }
        let open = self.lookup(&binding, &token)?;
        if open.opened.kind != FileKind::File
            || offset > open.opened.length
            || !(1..=rsi_files_protocol::MAXIMUM_FILE_PAGE_BYTES).contains(&maximum)
        {
            return Err(FilesError::Invalid);
        }
        let Reply::FilePage { page } = self
            .call(Request::FilesRead {
                handle: open.handle,
                offset,
                maximum,
            })
            .await?
        else {
            return Err(self.malformed());
        };
        page.validate_for(&open.opened, offset, maximum)
            .map_err(|_| self.malformed())?;
        if cancellation.is_cancelled() {
            return Err(FilesError::Cancelled);
        }
        Ok(page)
    }
    async fn list(
        &self,
        binding: FilesBinding,
        token: FileToken,
        offset: usize,
        maximum: usize,
        cancellation: CancellationToken,
    ) -> Result<DirectoryPage> {
        if cancellation.is_cancelled() {
            return Err(FilesError::Cancelled);
        }
        let open = self.lookup(&binding, &token)?;
        if open.opened.kind != FileKind::Directory
            || offset as u64 > open.opened.length
            || !(1..=rsi_files_protocol::MAXIMUM_DIRECTORY_PAGE_ENTRIES).contains(&maximum)
        {
            return Err(FilesError::Invalid);
        }
        let Reply::DirectoryPage { page } = self
            .call(Request::FilesList {
                handle: open.handle,
                offset,
                maximum,
            })
            .await?
        else {
            return Err(self.malformed());
        };
        page.validate_for(&open.opened, offset, maximum)
            .map_err(|_| self.malformed())?;
        if cancellation.is_cancelled() {
            return Err(FilesError::Cancelled);
        }
        Ok(page)
    }
    fn release(&self, binding: &FilesBinding, token: &FileToken) -> Result<()> {
        let mut entries = lock(&self.0.entries);
        if entries
            .get(token)
            .is_some_and(|entry| &entry.binding != binding)
        {
            return Err(FilesError::Binding);
        }
        entries.remove(token);
        Ok(())
    }
}
fn process_error(error: &ProcessError) -> FilesError {
    match error {
        ProcessError::Capacity => FilesError::Capacity,
        ProcessError::ShuttingDown => FilesError::Cancelled,
        ProcessError::Unsupported => FilesError::Unsupported,
        ProcessError::InvalidInput(_) => FilesError::Invalid,
        ProcessError::OutcomeUnknown => FilesError::OutcomeUnknown,
        _ => FilesError::Io,
    }
}
