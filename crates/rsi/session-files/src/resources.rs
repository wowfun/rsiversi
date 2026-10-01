//! Token lifetime is independent of the fresh authority used for each request.
use rsi_api_protocol::{CallOrigin, DeviceId};
use rsi_execution::{ExecutionFiles, ExecutionLease};
use rsi_files_protocol::{
    DirectoryPage, FileKind, FilePage, FileToken, Files, FilesBinding, FilesError,
    MAXIMUM_FILE_TOKENS, OpenedFile, RelativePath, Result,
};
use rsi_session_protocol::SessionTarget;
use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex},
};
use tokio::sync::{OwnedSemaphorePermit, Semaphore, oneshot};
use tokio_util::sync::CancellationToken;

#[derive(Debug)]
pub(super) struct FileOwners {
    entries: Mutex<BTreeMap<FileToken, Arc<Entry>>>,
    capacity: Arc<Semaphore>,
}
impl Default for FileOwners {
    fn default() -> Self {
        Self {
            entries: Mutex::new(BTreeMap::new()),
            capacity: Arc::new(Semaphore::new(MAXIMUM_FILE_TOKENS)),
        }
    }
}
impl FileOwners {
    pub fn release(
        &self,
        origin: &CallOrigin,
        target: &SessionTarget,
        token: &FileToken,
    ) -> Result<()> {
        let mut entries = self
            .entries
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(entry) = entries.get(token)
            && (entry.target != *target || entry.principal != principal(origin))
        {
            return Err(FilesError::Binding);
        }
        entries.remove(token);
        Ok(())
    }
    pub fn clear(&self) {
        self.capacity.close();
        self.entries
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clear();
    }
}
fn principal(origin: &CallOrigin) -> Option<DeviceId> {
    match origin {
        CallOrigin::Local => None,
        CallOrigin::Device(device) => Some(device.id.clone()),
    }
}
#[derive(Debug)]
struct Entry {
    resource: ExecutionFiles,
    target: SessionTarget,
    principal: Option<DeviceId>,
    _capacity: OwnedSemaphorePermit,
}
#[derive(Debug)]
pub(super) struct FilesView {
    owners: Arc<FileOwners>,
    lease: ExecutionLease,
    target: SessionTarget,
    principal: Option<DeviceId>,
}
fn execution_error(error: &rsi_process::ProcessError) -> FilesError {
    match error {
        rsi_process::ProcessError::Capacity => FilesError::Capacity,
        rsi_process::ProcessError::OutcomeUnknown => FilesError::OutcomeUnknown,
        rsi_process::ProcessError::Unsupported => FilesError::Unsupported,
        rsi_process::ProcessError::InvalidInput(_) => FilesError::Binding,
        _ => FilesError::Cancelled,
    }
}
impl FilesView {
    pub fn new(
        owners: Arc<FileOwners>,
        lease: ExecutionLease,
        target: SessionTarget,
        origin: &CallOrigin,
    ) -> Self {
        Self {
            owners,
            lease,
            target,
            principal: principal(origin),
        }
    }
    fn entry(&self, token: &FileToken) -> Result<Arc<Entry>> {
        let entries = self
            .owners
            .entries
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let entry = entries.get(token).ok_or(FilesError::Unavailable)?;
        self.check(entry)?;
        Ok(entry.clone())
    }
    fn check(&self, entry: &Entry) -> Result<()> {
        if entry.target != self.target || entry.principal != self.principal {
            return Err(FilesError::Binding);
        }
        Ok(())
    }
    fn view(&self, entry: &Entry) -> Result<Arc<dyn Files>> {
        entry
            .resource
            .view(&self.lease)
            .map_err(|error| execution_error(&error))
    }
    pub fn describe(&self, binding: &FilesBinding, token: &FileToken) -> Result<OpenedFile> {
        let entry = self.entry(token)?;
        self.view(&entry)?.describe(binding, token)
    }
    pub async fn open(
        &self,
        binding: FilesBinding,
        path: RelativePath,
        kind: FileKind,
        cancellation: CancellationToken,
    ) -> Result<OpenedFile> {
        let capacity = self
            .owners
            .capacity
            .clone()
            .try_acquire_owned()
            .map_err(|_| FilesError::Capacity)?;
        let entry = Arc::new(Entry {
            resource: self
                .lease
                .retain_files()
                .map_err(|error| execution_error(&error))?,
            target: self.target.clone(),
            principal: self.principal.clone(),
            _capacity: capacity,
        });
        let files = self.view(&entry)?;
        // The reply owns the scope and capacity even if the waiter disappears
        // before or after completion. No unconsumed token escapes publication.
        let (reply, result) = oneshot::channel();
        tokio::spawn(async move {
            let opened = files.open(binding, path, kind, cancellation).await;
            let _ = reply.send(opened.map(|opened| (opened, entry)));
        });
        let (opened, entry) = result.await.unwrap_or(Err(FilesError::OutcomeUnknown))?;
        let mut entries = self
            .owners
            .entries
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if self.owners.capacity.is_closed() {
            return Err(FilesError::Cancelled);
        }
        if entries.contains_key(&opened.token) {
            return Err(FilesError::Binding);
        }
        entries.insert(opened.token.clone(), entry);
        Ok(opened)
    }
    pub async fn read(
        &self,
        binding: FilesBinding,
        token: FileToken,
        offset: u64,
        maximum: usize,
        cancellation: CancellationToken,
    ) -> Result<FilePage> {
        let entry = self.entry(&token)?;
        self.view(&entry)?
            .read(binding, token, offset, maximum, cancellation)
            .await
    }
    pub async fn list(
        &self,
        binding: FilesBinding,
        token: FileToken,
        offset: usize,
        maximum: usize,
        cancellation: CancellationToken,
    ) -> Result<DirectoryPage> {
        let entry = self.entry(&token)?;
        self.view(&entry)?
            .list(binding, token, offset, maximum, cancellation)
            .await
    }
}
