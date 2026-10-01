//! A complete inert tuple that fails if scheduling metadata attempts filesystem or process I/O.
use async_trait::async_trait;
use rsi_execution::*;
use rsi_files_protocol::*;
use rsi_process::*;
use rsi_sandbox::{ProcessRequest, Sandbox, WorkspaceReadRequest, WorkspaceReadScope};
use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicUsize, Ordering},
};
use tokio_util::sync::CancellationToken;

#[derive(Debug, Default)]
pub struct Gate {
    pub revoked: AtomicBool,
    pub active: Arc<AtomicUsize>,
    pub maximum: Option<usize>,
}
struct Held(Arc<AtomicUsize>);
impl Drop for Held {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}
impl ExecutionAdmission for Gate {
    fn admit(
        &self,
        kind: rsi_execution::ExecutionAdmissionKind,
    ) -> rsi_process::Result<ExecutionOperation> {
        if self.revoked.load(Ordering::SeqCst) {
            return Err(ProcessError::ShuttingDown);
        }
        if kind == ExecutionAdmissionKind::Publication {
            return Ok(ExecutionOperation::new(()));
        }
        self.active
            .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |active| {
                self.maximum
                    .is_none_or(|maximum| active < maximum)
                    .then_some(active + 1)
            })
            .map_err(|_| ProcessError::Capacity)?;
        Ok(ExecutionOperation::new(Held(self.active.clone())))
    }
}
pub fn lease(location: ExecutionLocation, gate: Arc<Gate>, epoch: u64) -> ExecutionLease {
    lease_with_readers(location, gate, epoch, None)
}
pub fn lease_with_readers(
    location: ExecutionLocation,
    gate: Arc<Gate>,
    epoch: u64,
    readers: Option<(Arc<dyn Sandbox>, Arc<dyn Files>)>,
) -> ExecutionLease {
    let remote = location != ExecutionLocation::Local;
    ExecutionProvider::new(
        HostEpoch::from_bytes([1; 16]),
        location,
        u64::from(remote),
        if remote { epoch } else { 0 },
        Arc::new(NoIo {
            readers,
            files: None,
            canonical: None,
        }),
    )
    .unwrap()
    .lease(gate)
    .unwrap()
}
#[allow(dead_code)] // Shared fixture consumers select only the authority surface under test.
pub fn lease_with_canonical_path(
    location: ExecutionLocation,
    gate: Arc<Gate>,
    path: String,
    reads: Arc<AtomicUsize>,
) -> ExecutionLease {
    let remote = location != ExecutionLocation::Local;
    ExecutionProvider::new(
        HostEpoch::from_bytes([1; 16]),
        location,
        u64::from(remote),
        u64::from(remote),
        Arc::new(NoIo {
            readers: None,
            files: None,
            canonical: Some((path, reads)),
        }),
    )
    .unwrap()
    .lease(gate)
    .unwrap()
}
#[derive(Debug)]
struct NoIo {
    readers: Option<(Arc<dyn Sandbox>, Arc<dyn Files>)>,
    files: Option<Arc<dyn Files>>,
    canonical: Option<(String, Arc<AtomicUsize>)>,
}
#[async_trait]
impl ExecutionBackend for NoIo {
    async fn canonicalize(&self, path: &str) -> rsi_process::Result<String> {
        let (expected, reads) = self
            .canonical
            .as_ref()
            .expect("metadata accessed a target path");
        assert_eq!(path, expected);
        reads.fetch_add(1, Ordering::SeqCst);
        Ok(expected.clone())
    }
    async fn resolve_program(&self, _: &str) -> rsi_process::Result<ResolvedProgram> {
        unreachable!("metadata resolved a program")
    }
    async fn prepare(
        &self,
        _: ProcessRequest,
        _: Vec<(std::ffi::OsString, std::ffi::OsString)>,
    ) -> rsi_process::Result<BackendPlan> {
        unreachable!("metadata prepared a process")
    }
    async fn spawn(
        &self,
        _: ProcessSpec<BackendPlan>,
        _: ExecutionPin,
    ) -> rsi_process::Result<ManagedProcess> {
        unreachable!("metadata spawned a process")
    }
    async fn spawn_duplex(
        &self,
        _: DuplexProcessSpec<BackendPlan>,
        _: ExecutionPin,
    ) -> rsi_process::Result<ManagedDuplexProcess> {
        unreachable!("metadata spawned a duplex process")
    }
    async fn spawn_pty(
        &self,
        _: PtyProcessSpec<BackendPlan>,
        _: ExecutionPin,
    ) -> rsi_process::Result<ManagedPtyProcess> {
        unreachable!("metadata spawned a terminal")
    }
    async fn workspace_read(
        &self,
        request: WorkspaceReadRequest,
    ) -> rsi_process::Result<WorkspaceReadScope> {
        let (sandbox, _) = self.readers.as_ref().expect("metadata opened a workspace");
        sandbox
            .workspace_read(request)
            .await
            .map_err(|error| ProcessError::InvalidInput(error.to_string()))
    }
    fn files(&self) -> Arc<dyn Files> {
        if let Some(files) = &self.files {
            return files.clone();
        }
        self.readers.as_ref().map_or_else(
            || {
                Arc::new(Self {
                    readers: None,
                    files: None,
                    canonical: None,
                }) as Arc<dyn Files>
            },
            |(_, files)| files.clone(),
        )
    }
}
#[async_trait]
impl Files for NoIo {
    fn release_caller(&self, _: &FilesCaller) {}
    fn describe(&self, _: &FilesBinding, _: &FileToken) -> rsi_files_protocol::Result<OpenedFile> {
        unreachable!("metadata described a file")
    }
    async fn open(
        &self,
        _: FilesBinding,
        _: RelativePath,
        _: FileKind,
        _: CancellationToken,
    ) -> rsi_files_protocol::Result<OpenedFile> {
        unreachable!("metadata opened a file")
    }
    async fn read(
        &self,
        _: FilesBinding,
        _: FileToken,
        _: u64,
        _: usize,
        _: CancellationToken,
    ) -> rsi_files_protocol::Result<FilePage> {
        unreachable!("metadata read a file")
    }
    async fn list(
        &self,
        _: FilesBinding,
        _: FileToken,
        _: usize,
        _: usize,
        _: CancellationToken,
    ) -> rsi_files_protocol::Result<DirectoryPage> {
        unreachable!("metadata listed files")
    }
    fn release(&self, _: &FilesBinding, _: &FileToken) -> rsi_files_protocol::Result<()> {
        unreachable!("metadata released an unopened file")
    }
}

#[allow(dead_code)] // Files-only tests must fail if another backend capability is used.
pub fn provider_with_files(
    location: ExecutionLocation,
    files: Arc<dyn Files>,
    epoch: u64,
) -> ExecutionProvider {
    let remote = location != ExecutionLocation::Local;
    ExecutionProvider::new(
        HostEpoch::from_bytes([1; 16]),
        location,
        u64::from(remote),
        if remote { epoch } else { 0 },
        Arc::new(NoIo {
            readers: None,
            files: Some(files),
            canonical: None,
        }),
    )
    .unwrap()
}
