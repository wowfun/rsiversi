use super::{Arc, PathBuf, SessionCommand, SessionHandle, SessionId, SessionService};
use crate::session_cli::session_error;
use crate::work::ApplicationWork;
use rsi_session_protocol::export::default_filename;
use std::{
    path::Path,
    pin::Pin,
    task::{Context, Poll},
};
use tokio_util::sync::CancellationToken;

#[derive(Debug)]
pub(crate) enum Error {
    Cancelled,
    Failed(crate::RsiError),
}
type Result<T> = std::result::Result<T, Error>;
impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Cancelled => f.write_str("Export cancelled"),
            Self::Failed(error) => error.fmt(f),
        }
    }
}
impl From<crate::RsiError> for Error {
    fn from(error: crate::RsiError) -> Self {
        Self::Failed(error)
    }
}
impl From<rsi_session_export::FileWriteError> for Error {
    fn from(error: rsi_session_export::FileWriteError) -> Self {
        match error {
            rsi_session_export::FileWriteError::Cancelled => Self::Cancelled,
            error @ rsi_session_export::FileWriteError::Export(_) => {
                Self::Failed(session_error(error))
            }
        }
    }
}

pub(crate) struct Save {
    stop: CancellationToken,
    task: tokio::task::JoinHandle<Result<PathBuf>>,
}
impl std::future::Future for Save {
    type Output = Result<PathBuf>;
    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        Pin::new(&mut self.get_mut().task)
            .poll(cx)
            .map(|result| result.unwrap_or_else(|error| Err(Error::Failed(session_error(error)))))
    }
}
impl Drop for Save {
    fn drop(&mut self) {
        self.stop.cancel();
    }
}

pub(crate) fn start(
    work: &ApplicationWork,
    handle: Arc<dyn SessionHandle>,
    command: rsi_client::ExportCommand,
    stop: CancellationToken,
    retention: impl Send + 'static,
) -> Save {
    let token = stop.clone();
    let task = work.tasks.spawn(async move {
        let _retention = retention;
        save(handle, command, token).await
    });
    Save { stop, task }
}

pub(crate) async fn save(
    handle: Arc<dyn SessionHandle>,
    command: rsi_client::ExportCommand,
    stop: CancellationToken,
) -> Result<PathBuf> {
    let (source, path) = tokio::select! { biased;
        () = stop.cancelled() => return Err(Error::Cancelled),
        result = prepare(handle, command) => result?,
    };
    rsi_session_export::write_file(source, &path, stop).await?;
    Ok(path)
}

async fn prepare(
    handle: Arc<dyn SessionHandle>,
    command: rsi_client::ExportCommand,
) -> Result<(rsi_session_protocol::export::ExportStream, PathBuf)> {
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
    Ok((source, path))
}

pub(crate) async fn run_cli(
    service: Arc<dyn SessionService>,
    command: SessionCommand,
    work: &ApplicationWork,
    stop: CancellationToken,
) -> Result<()> {
    let _cancel = stop.clone().drop_guard();
    work.tasks.spawn(async move {
        let handle = tokio::select! { biased;
            () = stop.cancelled() => return Err(Error::Cancelled),
            result = select(service.as_ref(), &command) => result?,
        };
        if command.export_command.path.is_some() {
            let path = save(handle, command.export_command.clone(), stop).await?;
            let _reported = crate::work::diagnostic(vec![format!("Exported {}", path.display())]).await;
        } else {
            tokio::select! { biased;
                () = stop.cancelled() => return Err(Error::Cancelled),
                result = async {
                    let source = handle.export(command.export_command.options.clone()).await.map_err(session_error)?;
                    rsi_session_export::write_stream(source, &mut tokio::io::stdout()).await.map_err(session_error)?;
                    Ok::<_, crate::RsiError>(())
                } => result?,
            }
        }
        Ok(())
    }).await.map_err(session_error)?
}

async fn select(
    service: &dyn SessionService,
    command: &SessionCommand,
) -> crate::Result<Arc<dyn SessionHandle>> {
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
    service.attach(&id).await.map_err(session_error)
}
