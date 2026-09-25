use super::{ExportEvent, ExportStream, ExportVerifier, Result, StreamExt, encoding};
use std::path::Path;
use tokio::io::{AsyncWrite, AsyncWriteExt};

/// Writes a verified artifact to an already-authorized writer, without collecting it.
///
/// # Errors
/// Returns stream, framing, or writer errors, including a missing completion.
pub async fn write_stream(
    mut source: ExportStream,
    writer: &mut (impl AsyncWrite + Unpin),
) -> Result<u64> {
    let mut verifier = ExportVerifier::default();
    while let Some(event) = source.next().await {
        let event = event?;
        verifier.accept(&event, None)?;
        if let ExportEvent::Chunk { text, .. } = event {
            writer.write_all(text.as_bytes()).await.map_err(encoding)?;
        }
    }
    let bytes = verifier.finish()?;
    writer.flush().await.map_err(encoding)?;
    Ok(bytes)
}

/// Native file output failed before commit or while observing its actual result.
#[derive(Debug)]
pub enum FileWriteError {
    /// Cancelled before persistence admission; the destination was not replaced.
    Cancelled,
    /// Stream, validation, filesystem or persistence observation failure.
    Export(rsi_session_protocol::SessionError),
}
impl std::fmt::Display for FileWriteError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Cancelled => f.write_str("Export cancelled"),
            Self::Export(error) => error.fmt(f),
        }
    }
}
impl std::error::Error for FileWriteError {}
impl From<rsi_session_protocol::SessionError> for FileWriteError {
    fn from(error: rsi_session_protocol::SessionError) -> Self {
        Self::Export(error)
    }
}

/// Replaces the destination only after verified completion and synchronization.
/// Cancellation before commit returns Cancelled. Once commit is admitted, awaits
/// its actual result. Dropping that waiter cannot cancel an admitted persistence.
///
/// # Errors
/// Returns cancellation, stream, validation or filesystem errors. A lost commit
/// observation does not establish whether the destination was replaced.
pub async fn write_file(
    source: ExportStream,
    path: &Path,
    stop: tokio_util::sync::CancellationToken,
) -> std::result::Result<u64, FileWriteError> {
    write_file_with(source, path, stop, |temporary, destination| {
        temporary.persist(destination).map_err(encoding)
    })
    .await
}

pub(super) async fn write_file_with(
    source: ExportStream,
    path: &Path,
    stop: tokio_util::sync::CancellationToken,
    persist: impl FnOnce(tempfile::TempPath, std::path::PathBuf) -> Result<()> + Send + 'static,
) -> std::result::Result<u64, FileWriteError> {
    let (temporary, bytes) = tokio::select! { biased;
        () = stop.cancelled() => return Err(FileWriteError::Cancelled),
        result = prepare_file(source, path) => result?,
    };
    let destination = path.to_path_buf();
    // Synchronous admission: there is no cancellation point between this check
    // and handing the TempPath to the blocking persistence owner.
    if stop.is_cancelled() {
        return Err(FileWriteError::Cancelled);
    }
    tokio::task::spawn_blocking(move || persist(temporary, destination))
        .await
        .map_err(encoding)??;
    Ok(bytes)
}

async fn prepare_file(source: ExportStream, path: &Path) -> Result<(tempfile::TempPath, u64)> {
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    tokio::fs::create_dir_all(parent).await.map_err(encoding)?;
    let parent = parent.to_path_buf();
    let temporary = tokio::task::spawn_blocking(move || {
        tempfile::Builder::new()
            .prefix(".rsi-export-")
            .tempfile_in(parent)
    })
    .await
    .map_err(encoding)?
    .map_err(encoding)?;
    let (file, temporary) = temporary.into_parts();
    let mut file = tokio::fs::File::from_std(file);
    let bytes = write_stream(source, &mut file).await?;
    file.sync_all().await.map_err(encoding)?;
    drop(file);
    Ok((temporary, bytes))
}
