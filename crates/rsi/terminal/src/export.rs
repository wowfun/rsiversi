use super::{Arc, PathBuf, Result, SessionCommand, SessionHandle, SessionId, SessionService};
use crate::session_cli::session_error;
use rsi_session_protocol::export::default_filename;
use std::path::Path;

pub(crate) async fn save(
    handle: Arc<dyn SessionHandle>,
    command: rsi_client::ExportCommand,
) -> Result<PathBuf> {
    let header = handle.header().await.map_err(session_error)?;
    let path = command.path.map_or_else(
        || {
            PathBuf::from(default_filename(
                header.session_id(),
                command.options.format,
            ))
        },
        PathBuf::from,
    );
    let path = if path.is_absolute() {
        path
    } else {
        std::env::current_dir().map_err(session_error)?.join(path)
    };
    let source = handle
        .export(command.options)
        .await
        .map_err(session_error)?;
    rsi_session_export::write_file(source, &path)
        .await
        .map_err(session_error)?;
    Ok(path)
}

pub(crate) async fn run_cli(service: &dyn SessionService, command: &SessionCommand) -> Result<()> {
    let selection = command.export.as_deref().expect("export command");
    let id = if selection == "latest" {
        let cwd = command
            .cwd
            .clone()
            .map_or_else(std::env::current_dir, Ok)
            .map_err(session_error)?;
        let cwd = tokio::fs::canonicalize(cwd).await.map_err(session_error)?;
        let mut after = None;
        loop {
            let page = service
                .list_recent(after.as_ref(), 20)
                .await
                .map_err(session_error)?;
            if let Some(session) = page.sessions.iter().find(|session| {
                session.header.fork_origin().is_none()
                    && Path::new(session.header.canonical_cwd()) == cwd
            }) {
                break session.header.session_id().clone();
            }
            if !page.has_more {
                return Err(session_error("No durable root Session in this workspace"));
            }
            let next = page
                .sessions
                .last()
                .map(rsi_session_protocol::SessionSummary::cursor)
                .ok_or_else(|| session_error("Session listing made no progress"))?;
            if after.as_ref() == Some(&next) {
                return Err(session_error("Session listing made no progress"));
            }
            after = Some(next);
        }
    } else {
        SessionId::new(selection).map_err(session_error)?
    };
    let handle = service.attach(&id).await.map_err(session_error)?;
    if command.export_command.path.is_some() {
        let path = save(handle, command.export_command.clone()).await?;
        eprintln!("Exported {}", path.display());
    } else {
        let source = handle
            .export(command.export_command.options.clone())
            .await
            .map_err(session_error)?;
        rsi_session_export::write_stream(source, &mut tokio::io::stdout())
            .await
            .map_err(session_error)?;
    }
    Ok(())
}
