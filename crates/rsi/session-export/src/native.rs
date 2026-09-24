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

/// Replaces a client-selected path only after the complete stream verifies.
/// Dropping this future removes its temporary file, including during pending I/O.
///
/// # Errors
/// Returns stream, validation, or filesystem errors; failed streams never replace the destination.
pub async fn write_file(source: ExportStream, path: &Path) -> Result<u64> {
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
    let destination = path.to_path_buf();
    tokio::task::spawn_blocking(move || temporary.persist(destination))
        .await
        .map_err(encoding)?
        .map_err(encoding)?;
    Ok(bytes)
}
