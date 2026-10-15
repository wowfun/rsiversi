use rsi_files_protocol::{
    DirectoryPage, FileKind, FilePage, FilesBinding, FilesError, RelativePath, Result,
};
use rsi_files_protocol::{FileToken, OpenedFile};
use tokio_util::sync::CancellationToken;
#[derive(Debug)]
pub(super) enum Resource {}
pub(super) fn open(
    _: &FilesBinding,
    _: RelativePath,
    _: FileKind,
    _: &CancellationToken,
) -> Result<Resource> {
    Err(FilesError::Unsupported)
}
impl Resource {
    pub(super) fn describe(&self, _: FileToken) -> OpenedFile {
        match *self {}
    }
    pub(super) fn read(&self, _: u64, _: usize, _: &CancellationToken) -> Result<FilePage> {
        match *self {}
    }
    pub(super) fn list(&self, _: usize, _: usize, _: &CancellationToken) -> Result<DirectoryPage> {
        match *self {}
    }
}
