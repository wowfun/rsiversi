//! Token lifetime is independent of the fresh authority used for each request.
use rsi_api_protocol::{CallOrigin, DeviceId};
use rsi_execution::{ExecutionFiles, ExecutionLease};
use rsi_files_protocol::{
    DirectoryPage, FILE_TOKEN_LIFETIME, FileKind, FilePage, FileToken, Files, FilesBinding,
    FilesError, MAXIMUM_FILE_TOKENS, OpenedFile, RelativePath, Result,
};
use rsi_session_protocol::SessionTarget;
use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex},
    time::Instant,
};
use tokio::sync::{OwnedSemaphorePermit, Semaphore, TryAcquireError, oneshot};
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
    fn publish(&self, mut opened: OpenedFile, entry: Arc<Entry>) -> Result<OpenedFile> {
        let mut entries = self
            .entries
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if self.capacity.is_closed() || entry.expires <= Instant::now() {
            return Err(FilesError::Cancelled);
        }
        for _ in 0..8 {
            let mut bytes = [0; 16];
            getrandom::fill(&mut bytes).map_err(|_| FilesError::Io)?;
            let token = FileToken::try_from(format!("{:032x}", u128::from_be_bytes(bytes)))
                .expect("128-bit lowercase hexadecimal token");
            if !entries.contains_key(&token) {
                opened.token = token;
                entries.insert(opened.token.clone(), entry);
                return Ok(opened);
            }
        }
        Err(FilesError::Capacity)
    }
    fn prune(&self, now: Instant) {
        let expired: Vec<_> = self
            .entries
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .extract_if(.., |_, entry| entry.expires <= now)
            .map(|(_, entry)| entry)
            .collect();
        drop(expired);
    }
    fn unavailable(&self, token: &FileToken, entry: &Arc<Entry>) {
        let removed = {
            let mut entries = self
                .entries
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if entries
                .get(token)
                .is_some_and(|current| Arc::ptr_eq(current, entry))
            {
                entries.remove(token)
            } else {
                None
            }
        };
        drop(removed);
    }
    pub fn release(
        &self,
        origin: &CallOrigin,
        target: &SessionTarget,
        token: &FileToken,
    ) -> Result<()> {
        self.prune(Instant::now());
        let mut entries = self
            .entries
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(entry) = entries.get(token)
            && (entry.target != *target || entry.principal != principal(origin))
        {
            return Err(FilesError::Binding);
        }
        let removed = entries.remove(token);
        drop(entries);
        drop(removed);
        Ok(())
    }
    pub fn clear(&self) {
        self.capacity.close();
        let removed = std::mem::take(
            &mut *self
                .entries
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
        );
        drop(removed);
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
    provider_token: FileToken,
    target: SessionTarget,
    principal: Option<DeviceId>,
    expires: Instant,
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
        self.owners.prune(Instant::now());
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
        let result = self.view(&entry)?.describe(binding, &entry.provider_token);
        if matches!(result, Err(FilesError::Unavailable)) {
            self.owners.unavailable(token, &entry);
        }
        result.map(|mut opened| {
            opened.token = token.clone();
            opened
        })
    }
    fn check_unavailable(
        &self,
        files: &dyn Files,
        binding: &FilesBinding,
        token: &FileToken,
        entry: &Arc<Entry>,
    ) {
        if matches!(
            files.describe(binding, &entry.provider_token),
            Err(FilesError::Unavailable)
        ) {
            self.owners.unavailable(token, entry);
        }
    }
    pub async fn open(
        &self,
        binding: FilesBinding,
        path: RelativePath,
        kind: FileKind,
        cancellation: CancellationToken,
    ) -> Result<OpenedFile> {
        self.owners.prune(Instant::now());
        let capacity =
            self.owners
                .capacity
                .clone()
                .try_acquire_owned()
                .map_err(|error| match error {
                    TryAcquireError::Closed => FilesError::Cancelled,
                    TryAcquireError::NoPermits => FilesError::Capacity,
                })?;
        let resource = self
            .lease
            .retain_files()
            .map_err(|error| execution_error(&error))?;
        let files = resource
            .view(&self.lease)
            .map_err(|error| execution_error(&error))?;
        let target = self.target.clone();
        let principal = self.principal.clone();
        let expires = Instant::now() + FILE_TOKEN_LIFETIME;
        // The reply owns the scope and capacity even if the waiter disappears
        // before or after completion. No unconsumed token escapes publication.
        let (reply, result) = oneshot::channel();
        tokio::spawn(async move {
            let opened = files.open(binding, path, kind, cancellation).await;
            let _ = reply.send(opened.map(|opened| {
                let entry = Arc::new(Entry {
                    provider_token: opened.token.clone(),
                    resource,
                    target,
                    principal,
                    expires,
                    _capacity: capacity,
                });
                (opened, entry)
            }));
        });
        let (opened, entry) = result.await.unwrap_or(Err(FilesError::OutcomeUnknown))?;
        self.owners.publish(opened, entry)
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
        let files = self.view(&entry)?;
        let result = files
            .read(
                binding.clone(),
                entry.provider_token.clone(),
                offset,
                maximum,
                cancellation,
            )
            .await;
        if matches!(result, Err(FilesError::Unavailable)) {
            self.check_unavailable(files.as_ref(), &binding, &token, &entry);
        }
        result
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
        let files = self.view(&entry)?;
        let result = files
            .list(
                binding.clone(),
                entry.provider_token.clone(),
                offset,
                maximum,
                cancellation,
            )
            .await;
        if matches!(result, Err(FilesError::Unavailable)) {
            self.check_unavailable(files.as_ref(), &binding, &token, &entry);
        }
        result
    }
}

#[cfg(test)]
#[path = "resources_tests.rs"]
mod tests;
