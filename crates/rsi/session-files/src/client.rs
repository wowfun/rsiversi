use crate::{
    Result, SessionFiles, SessionFilesError,
    wire::{self, FilesOperation, Input, Reply, Request},
};
use async_trait::async_trait;
use rsi_api_protocol::{ApiClient, ApiError, call_json};
use rsi_files_protocol::{
    DirectoryPage, FileKind, FilePage, FileToken, FilesError, MAXIMUM_DIRECTORY_ENTRIES,
    MAXIMUM_DIRECTORY_PAGE_BYTES, OpenedFile, RelativePath,
};
use rsi_session_protocol::SessionTarget;
use serde::{Serialize, de::DeserializeOwned};
use std::sync::Arc;

/// Validating proxy over one negotiated API connection; usable in a Worker.
#[derive(Clone, Debug)]
pub struct SessionFilesClient {
    api: Arc<dyn ApiClient>,
}
impl SessionFilesClient {
    /// Require exact Files operations before exposing the capability.
    pub fn new(api: Arc<dyn ApiClient>) -> rsi_api_protocol::Result<Self> {
        if FilesOperation::ALL
            .iter()
            .any(|operation| !api.operations().contains(&operation.spec()))
        {
            return Err(ApiError::Unavailable);
        }
        Ok(Self { api })
    }
    async fn call<
        I: Input + Clone + PartialEq + Serialize + DeserializeOwned + Send + Sync,
        O: DeserializeOwned,
    >(
        &self,
        operation: FilesOperation,
        target: SessionTarget,
        input: I,
    ) -> Result<O> {
        target.validate().map_err(|_| FilesError::Invalid)?;
        input.validate()?;
        let request = Request { target, input };
        let reply =
            call_json::<_, Reply<I, O>, FilesError>(self.api.as_ref(), &operation.spec(), &request)
                .await??;
        if reply.request != request {
            return Err(malformed());
        }
        Ok(reply.body)
    }
}
fn malformed() -> SessionFilesError {
    ApiError::Invalid("invalid remote Files response".into()).into()
}
#[async_trait]
impl SessionFiles for SessionFilesClient {
    async fn open(
        &self,
        target: SessionTarget,
        path: RelativePath,
        kind: FileKind,
    ) -> Result<OpenedFile> {
        let file: OpenedFile = self
            .call(
                FilesOperation::Open,
                target,
                wire::Open {
                    path: path.clone(),
                    kind,
                },
            )
            .await?;
        if file.path != path
            || file.kind != kind
            || (kind == FileKind::Directory && file.length > MAXIMUM_DIRECTORY_ENTRIES as u64)
        {
            return Err(malformed());
        }
        Ok(file)
    }
    async fn read(
        &self,
        target: SessionTarget,
        file: OpenedFile,
        offset: u64,
        maximum: usize,
    ) -> Result<FilePage> {
        let length = file.length;
        let page: FilePage = self
            .call(
                FilesOperation::Read,
                target,
                wire::Read {
                    file,
                    offset,
                    maximum,
                },
            )
            .await?;
        let count =
            usize::try_from((length - offset).min(maximum as u64)).map_err(|_| malformed())?;
        if page.offset != offset
            || page.total != length
            || page.bytes_hex.len() != count * 2
            || !page
                .bytes_hex
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        {
            return Err(malformed());
        }
        Ok(page)
    }
    async fn list(
        &self,
        target: SessionTarget,
        file: OpenedFile,
        offset: usize,
        maximum: usize,
    ) -> Result<DirectoryPage> {
        let expected = file.clone();
        let page: DirectoryPage = self
            .call(
                FilesOperation::List,
                target,
                wire::List {
                    file,
                    offset,
                    maximum,
                },
            )
            .await?;
        if page.offset != offset
            || page.total as u64 != expected.length
            || page.entries.len() > maximum
            || offset
                .checked_add(page.entries.len())
                .is_none_or(|end| end > page.total)
            || (offset < page.total && page.entries.is_empty())
            || rsi_api_protocol::measure_json(&page, MAXIMUM_DIRECTORY_PAGE_BYTES).is_err()
            || !page
                .entries
                .windows(2)
                .all(|entries| entries[0].path < entries[1].path)
        {
            return Err(malformed());
        }
        for entry in &page.entries {
            let bytes = entry.path.as_bytes();
            let name = bytes.rsplit(|b| *b == b'/').next().ok_or_else(malformed)?;
            if expected.path.join(name).map_err(|_| malformed())? != entry.path
                || entry.name != String::from_utf8_lossy(name)
            {
                return Err(malformed());
            }
        }
        Ok(page)
    }
    async fn release(&self, target: SessionTarget, token: FileToken) -> Result<()> {
        self.call(FilesOperation::Release, target, wire::Release { token })
            .await
    }
}
