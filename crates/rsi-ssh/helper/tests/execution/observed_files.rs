use rsi_files_protocol::*;
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
use tokio_util::sync::CancellationToken;

#[derive(Debug)]
pub struct Observed {
    pub inner: rsi_files::LocalFiles,
    pub describes: Arc<AtomicUsize>,
}

#[async_trait::async_trait]
impl Files for Observed {
    fn release_caller(&self, caller: &FilesCaller) {
        self.inner.release_caller(caller);
    }
    fn describe(&self, binding: &FilesBinding, token: &FileToken) -> Result<OpenedFile> {
        self.describes.fetch_add(1, Ordering::SeqCst);
        self.inner.describe(binding, token)
    }
    async fn open(
        &self,
        binding: FilesBinding,
        path: RelativePath,
        kind: FileKind,
        cancel: CancellationToken,
    ) -> Result<OpenedFile> {
        self.inner.open(binding, path, kind, cancel).await
    }
    async fn read(
        &self,
        binding: FilesBinding,
        token: FileToken,
        offset: u64,
        maximum: usize,
        cancel: CancellationToken,
    ) -> Result<FilePage> {
        self.inner
            .read(binding, token, offset, maximum, cancel)
            .await
    }
    async fn list(
        &self,
        binding: FilesBinding,
        token: FileToken,
        offset: usize,
        maximum: usize,
        cancel: CancellationToken,
    ) -> Result<DirectoryPage> {
        self.inner
            .list(binding, token, offset, maximum, cancel)
            .await
    }
    fn release(&self, binding: &FilesBinding, token: &FileToken) -> Result<()> {
        self.inner.release(binding, token)
    }
}
