use crate::{ExecutionLease, ExecutionPin, Provider};
use async_trait::async_trait;
use rsi_files_protocol::{
    DirectoryPage, FileKind, FilePage, FileToken, Files, FilesBinding, FilesCaller, FilesError,
    OpenedFile, RelativePath, Result,
};
use std::{
    future::Future,
    sync::{Arc, Mutex},
};
use tokio::sync::oneshot;
use tokio_util::sync::CancellationToken;

#[derive(Debug)]
pub(crate) struct BoundFiles {
    pub inner: Arc<dyn Files>,
    pub pin: ExecutionPin,
    pub resource: ExecutionFiles,
}
/// Retained Files scope without operation authority; views require a current caller lease.
#[derive(Clone, Debug)]
pub struct ExecutionFiles(pub(crate) Arc<Resource>);
#[derive(Debug)]
pub(crate) struct Resource {
    provider: Arc<Provider>,
    callers: Callers,
    inner: Arc<dyn Files>,
}
impl Drop for Resource {
    fn drop(&mut self) {
        self.callers.release_all(self.inner.as_ref());
    }
}
impl ExecutionFiles {
    pub(crate) fn new(provider: Arc<Provider>) -> Self {
        Self(Arc::new(Resource {
            inner: provider.backend.files(),
            provider,
            callers: Callers::default(),
        }))
    }
    /// Checks the exact provider before admission or backend access.
    pub fn view(&self, lease: &ExecutionLease) -> rsi_process::Result<Arc<dyn Files>> {
        if !Arc::ptr_eq(&self.0.provider, &lease.0.provider) {
            return Err(rsi_process::ProcessError::InvalidInput(
                "Files resource belongs to a different execution provider".into(),
            ));
        }
        let _permit = ExecutionPin(lease.clone()).publication()?;
        Ok(Arc::new(BoundFiles {
            inner: self.0.inner.clone(),
            pin: ExecutionPin(lease.clone()),
            resource: self.clone(),
        }))
    }
}
#[derive(Debug, Default)]
pub(crate) struct Callers(Mutex<Vec<Caller>>);
const MAXIMUM_FILE_CALLERS: usize = 64;
#[derive(Debug)]
struct Caller {
    original: FilesCaller,
    scoped: FilesCaller,
    opening: usize,
    published: bool,
}
impl Callers {
    fn binding(&self, binding: &FilesBinding) -> Result<FilesBinding> {
        let callers = self
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let caller = callers
            .iter()
            .find(|caller| &caller.original == binding.caller())
            .ok_or(FilesError::Binding)?;
        Ok(binding.clone().with_caller(caller.scoped.clone()))
    }
    fn reserve(&self, binding: &FilesBinding) -> Result<FilesBinding> {
        let mut callers = self
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(caller) = callers
            .iter_mut()
            .find(|caller| &caller.original == binding.caller())
        {
            caller.opening += 1;
            return Ok(binding.clone().with_caller(caller.scoped.clone()));
        }
        if callers.len() >= MAXIMUM_FILE_CALLERS {
            return Err(FilesError::Capacity);
        }
        let scoped = FilesCaller::default();
        callers.push(Caller {
            original: binding.caller().clone(),
            scoped: scoped.clone(),
            opening: 1,
            published: false,
        });
        Ok(binding.clone().with_caller(scoped))
    }
    fn remove(&self, caller: &FilesCaller) -> Option<FilesCaller> {
        let mut callers = self
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let index = callers.iter().position(|entry| &entry.original == caller)?;
        Some(callers.swap_remove(index).scoped)
    }
    pub(super) fn release_all(&self, inner: &dyn Files) {
        let callers = std::mem::take(
            &mut *self
                .0
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
        );
        for caller in callers {
            inner.release_caller(&caller.scoped);
        }
    }
}
struct OpenCaller {
    resource: ExecutionFiles,
    scoped: FilesCaller,
    published: bool,
}
impl Drop for OpenCaller {
    fn drop(&mut self) {
        let removed = {
            let mut callers = self
                .resource
                .0
                .callers
                .0
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let Some(index) = callers
                .iter()
                .position(|caller| caller.scoped == self.scoped)
            else {
                return;
            };
            let caller = &mut callers[index];
            caller.published |= self.published;
            caller.opening -= 1;
            (!caller.published && caller.opening == 0).then(|| callers.swap_remove(index))
        };
        if let Some(caller) = removed {
            self.resource.0.inner.release_caller(&caller.scoped);
        }
    }
}
struct OpenPublication {
    inner: Arc<dyn Files>,
    binding: FilesBinding,
    opened: Option<OpenedFile>,
    caller: OpenCaller,
}
impl Drop for OpenPublication {
    fn drop(&mut self) {
        if let Some(opened) = self.opened.take() {
            let _ = self.inner.release(&self.binding, &opened.token);
        }
    }
}
impl BoundFiles {
    fn admit(&self) -> Result<crate::ExecutionOperation> {
        self.pin.admit().map_err(|error| match error {
            rsi_process::ProcessError::Capacity
            | rsi_process::ProcessError::Api(rsi_api_protocol::ApiError::Capacity) => {
                FilesError::Capacity
            }
            rsi_process::ProcessError::Unsupported => FilesError::Unsupported,
            rsi_process::ProcessError::OutcomeUnknown
            | rsi_process::ProcessError::Api(rsi_api_protocol::ApiError::OutcomeUnknown) => {
                FilesError::OutcomeUnknown
            }
            _ => FilesError::Cancelled,
        })
    }
    async fn operation<T: Send + 'static>(
        &self,
        permit: crate::ExecutionOperation,
        future: impl Future<Output = Result<T>> + Send + 'static,
    ) -> Result<T> {
        let pin = self.pin.clone();
        let resource = self.resource.clone();
        let (reply, result) = oneshot::channel();
        tokio::spawn(async move {
            let (_permit, _pin, _resource) = (permit, pin, resource);
            let _ = reply.send(future.await);
        });
        result.await.unwrap_or(Err(FilesError::OutcomeUnknown))
    }
}
#[async_trait]
impl Files for BoundFiles {
    fn release_caller(&self, caller: &FilesCaller) {
        if let Some(scoped) = self.resource.0.callers.remove(caller) {
            self.inner.release_caller(&scoped);
        }
    }
    fn describe(&self, binding: &FilesBinding, token: &FileToken) -> Result<OpenedFile> {
        let _permit = self.admit()?;
        self.inner
            .describe(&self.resource.0.callers.binding(binding)?, token)
    }
    async fn open(
        &self,
        binding: FilesBinding,
        path: RelativePath,
        kind: FileKind,
        cancellation: CancellationToken,
    ) -> Result<OpenedFile> {
        let permit = self.admit()?;
        let binding = self.resource.0.callers.reserve(&binding)?;
        let caller = OpenCaller {
            resource: self.resource.clone(),
            scoped: binding.caller().clone(),
            published: false,
        };
        let (inner, pin) = (self.inner.clone(), self.pin.clone());
        let resource = self.resource.clone();
        let (reply, result) = oneshot::channel();
        tokio::spawn(async move {
            let (_permit, _pin, _resource) = (permit, pin, resource);
            let opened = inner.open(binding.clone(), path, kind, cancellation).await;
            let _ = reply.send(opened.map(|opened| OpenPublication {
                inner,
                binding,
                opened: Some(opened),
                caller,
            }));
        });
        result
            .await
            .unwrap_or(Err(FilesError::OutcomeUnknown))
            .map(|mut publication| {
                publication.caller.published = true;
                publication
                    .opened
                    .take()
                    .expect("one publication consumes one Files token")
            })
    }
    async fn read(
        &self,
        binding: FilesBinding,
        token: FileToken,
        offset: u64,
        maximum: usize,
        cancellation: CancellationToken,
    ) -> Result<FilePage> {
        let permit = self.admit()?;
        let binding = self.resource.0.callers.binding(&binding)?;
        let inner = self.inner.clone();
        self.operation(permit, async move {
            inner
                .read(binding, token, offset, maximum, cancellation)
                .await
        })
        .await
    }
    async fn list(
        &self,
        binding: FilesBinding,
        token: FileToken,
        offset: usize,
        maximum: usize,
        cancellation: CancellationToken,
    ) -> Result<DirectoryPage> {
        let permit = self.admit()?;
        let binding = self.resource.0.callers.binding(&binding)?;
        let inner = self.inner.clone();
        self.operation(permit, async move {
            inner
                .list(binding, token, offset, maximum, cancellation)
                .await
        })
        .await
    }
    fn release(&self, binding: &FilesBinding, token: &FileToken) -> Result<()> {
        match self.resource.0.callers.binding(binding) {
            Ok(binding) => self.inner.release(&binding, token),
            // This lease never issued a token to this caller, or already released
            // its entire caller scope. Cleanup has no remaining authority to use.
            Err(FilesError::Binding) => Ok(()),
            Err(error) => Err(error),
        }
    }
}
