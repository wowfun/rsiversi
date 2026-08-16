use std::fs;
use std::os::unix::fs::{FileTypeExt, MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use futures_util::{SinkExt, StreamExt};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::Semaphore;
use tokio::task::JoinSet;
use tokio_util::codec::Framed;
use tokio_util::sync::CancellationToken;

use crate::auth::{AuthState, ensure_private_directory};
use crate::framing::{
    encode_request, encode_response, ndjson_request_codec, ndjson_response_codec,
};
use crate::host::{SharedHost, submit_with_rejection};
use crate::lifecycle::DaemonLifecycle;
use crate::protocol::{
    CommandEnvelope, CommandOutcome, CommandOutcomeEnvelope, rejected, validate_command,
    validate_outcome,
};
use crate::streams::{StreamRouter, WireEnvelope, cancel_envelope, decode_wire_envelope};

const SOCKET_MODE: u32 = 0o600;
const MAX_UNIX_CONNECTIONS: usize = 128;

#[derive(Debug)]
pub struct UnixServer {
    listener: UnixListener,
    path: PathBuf,
    guard: SocketPathGuard,
}

impl UnixServer {
    pub fn bind(path: impl Into<PathBuf>) -> Result<Self> {
        let path = path.into();
        let parent = path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
            .context("Unix socket path must have a parent directory")?;
        ensure_private_directory(parent)?;
        remove_stale_socket(&path)?;
        let listener = UnixListener::bind(&path)
            .with_context(|| format!("bind Unix socket {}", path.display()))?;
        fs::set_permissions(&path, fs::Permissions::from_mode(SOCKET_MODE))
            .with_context(|| format!("protect Unix socket {}", path.display()))?;
        let metadata = fs::metadata(&path)?;
        let effective_uid = rustix::process::geteuid().as_raw();
        if metadata.uid() != effective_uid {
            bail!(
                "Unix socket {} is owned by uid {}, expected {effective_uid}",
                path.display(),
                metadata.uid()
            );
        }
        let guard = SocketPathGuard {
            path: path.clone(),
            device: metadata.dev(),
            inode: metadata.ino(),
        };
        Ok(Self {
            listener,
            path,
            guard,
        })
    }

    pub async fn serve(
        self,
        host: SharedHost,
        auth: AuthState,
        lifecycle: DaemonLifecycle,
        cancellation: CancellationToken,
    ) -> Result<()> {
        self.serve_with_limit(host, auth, lifecycle, cancellation, MAX_UNIX_CONNECTIONS)
            .await
    }

    async fn serve_with_limit(
        self,
        host: SharedHost,
        auth: AuthState,
        lifecycle: DaemonLifecycle,
        cancellation: CancellationToken,
        max_connections: usize,
    ) -> Result<()> {
        let Self {
            listener,
            path,
            guard: _guard,
        } = self;
        let socket_metadata = fs::metadata(&path)
            .with_context(|| format!("inspect Unix socket {}", path.display()))?;
        let owner_uid = socket_metadata.uid();
        let effective_uid = rustix::process::geteuid().as_raw();
        if owner_uid != effective_uid {
            bail!(
                "Unix socket {} is owned by uid {owner_uid}, expected {effective_uid}",
                path.display()
            );
        }
        let mut connections = JoinSet::new();
        let admission = std::sync::Arc::new(Semaphore::new(max_connections));

        loop {
            let permit = tokio::select! {
                biased;
                () = lifecycle.restarting() => break,
                () = cancellation.cancelled() => break,
                Some(joined) = connections.join_next(), if !connections.is_empty() => {
                    if let Err(error) = joined {
                        tracing::warn!(%error, "Unix client task panicked");
                    }
                    continue;
                }
                permit = admission.clone().acquire_owned() => {
                    permit.context("Unix connection admission semaphore closed")?
                }
            };
            let accepted = tokio::select! {
                biased;
                () = lifecycle.restarting() => break,
                () = cancellation.cancelled() => break,
                accepted = listener.accept() => accepted,
            };
            let (stream, _) = match accepted {
                Ok(accepted) => accepted,
                Err(error) => {
                    tracing::warn!(%error, "retrying Unix accept after an error");
                    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
                    continue;
                }
            };
            let host = host.clone();
            let auth = auth.clone();
            let lifecycle = lifecycle.clone();
            connections.spawn(async move {
                let _permit = permit;
                if let Err(error) = serve_connection(stream, owner_uid, host, auth, lifecycle).await
                {
                    tracing::debug!(%error, "Unix client disconnected with an error");
                }
            });
        }

        connections.abort_all();
        while connections.join_next().await.is_some() {}
        Ok(())
    }
}

pub async fn send_command(
    socket: &Path,
    envelope: CommandEnvelope,
) -> Result<CommandOutcomeEnvelope> {
    validate_socket_path(socket)?;
    let command_id = envelope.command_id.clone();
    let stream = UnixStream::connect(socket)
        .await
        .with_context(|| format!("connect to daemon socket {}", socket.display()))?;
    let mut framed = Framed::new(stream, ndjson_response_codec());
    framed
        .send(encode_request(&envelope)?)
        .await
        .context("send daemon command")?;
    let line = framed
        .next()
        .await
        .context("daemon closed before returning a result")?
        .context("read daemon result frame")?;
    let response: CommandOutcomeEnvelope =
        serde_json::from_str(&line).context("decode daemon result")?;
    validate_outcome(&response)?;
    if response.command_id != command_id {
        bail!(
            "daemon result command_id {:?} does not match request {:?}",
            response.command_id,
            command_id
        );
    }
    Ok(response)
}

fn validate_socket_path(path: &Path) -> Result<()> {
    let metadata = fs::symlink_metadata(path)
        .with_context(|| format!("inspect daemon socket {}", path.display()))?;
    if metadata.file_type().is_symlink() || !metadata.file_type().is_socket() {
        bail!("daemon socket path {} is not a Unix socket", path.display());
    }
    let effective_uid = rustix::process::geteuid().as_raw();
    if metadata.uid() != effective_uid {
        bail!(
            "daemon socket {} is owned by uid {}, expected {effective_uid}",
            path.display(),
            metadata.uid()
        );
    }
    let mode = metadata.mode() & 0o777;
    if mode != SOCKET_MODE {
        bail!(
            "daemon socket {} must have mode 0600, found {mode:04o}",
            path.display()
        );
    }
    Ok(())
}

async fn serve_connection(
    stream: UnixStream,
    owner_uid: u32,
    host: SharedHost,
    auth: AuthState,
    lifecycle: DaemonLifecycle,
) -> Result<()> {
    let peer_uid = stream
        .peer_cred()
        .context("read Unix peer credentials")?
        .uid();
    if peer_uid != owner_uid {
        bail!("refusing Unix peer uid {peer_uid}; daemon uid is {owner_uid}");
    }
    let mut framed = Framed::new(stream, ndjson_request_codec());
    let mut streams = StreamRouter::new(host.clone());

    loop {
        enum Activity {
            Restarting,
            Input(Option<std::result::Result<String, tokio_util::codec::LinesCodecError>>),
            Stream(Option<crate::protocol::StreamEnvelope>),
        }
        let activity = tokio::select! {
            biased;
            () = lifecycle.restarting() => Activity::Restarting,
            frame = streams.recv() => Activity::Stream(frame),
            line = framed.next() => Activity::Input(line),
        };
        let line = match activity {
            Activity::Restarting | Activity::Input(None) => break,
            Activity::Input(Some(Err(error))) => {
                tracing::debug!(%error, "rejecting invalid Unix wire frame");
                break;
            }
            Activity::Stream(Some(frame)) => {
                if framed.send(encode_response(&frame)?).await.is_err() {
                    break;
                }
                continue;
            }
            Activity::Stream(None) => continue,
            Activity::Input(Some(Ok(line))) => line,
        };

        let request = match decode_wire_envelope(&line) {
            Ok(WireEnvelope::Control(request)) => request,
            Ok(WireEnvelope::Stream(frame)) => {
                let stream_id = frame.stream_id.clone();
                if let Err(error) = streams.route(frame) {
                    let response =
                        cancel_envelope(&stream_id, "invalid_stream_frame", &format!("{error:#}"));
                    if framed.send(encode_response(&response)?).await.is_err() {
                        break;
                    }
                }
                continue;
            }
            Err(error) => {
                tracing::debug!(%error, "rejecting invalid Unix wire envelope");
                break;
            }
        };
        let command_id = request.command_id.clone();
        if let Err(error) = validate_command(&request) {
            let response = rejected(
                command_id,
                host.graph_revision(),
                "invalid_command",
                error.to_string(),
            );
            framed.send(encode_response(&response)?).await?;
            continue;
        }

        let response = submit_with_rejection(host.as_ref(), request).await;
        #[cfg(feature = "test-failpoints")]
        crate::test_failpoints::gate_before_uds_ack(&response).await?;
        if let CommandOutcome::TokenRotated { generation } = &response.payload {
            // The durable core outcome is authoritative. Applying it after
            // commit closes the crash window on retry: equal/older generations
            // are idempotent and a newer one is published before its ack.
            if let Err(error) = auth.rotate_to(*generation) {
                // Continuing to admit authenticated clients with the old token
                // after the durable generation advanced would violate the
                // credential boundary. Restart so startup reconciliation can
                // repair the lagging file before HTTP/WS admission resumes.
                lifecycle.request_restart();
                return Err(error).context("publish durable token generation");
            }
        }
        let shutting_down = matches!(&response.payload, CommandOutcome::ShuttingDown);
        let sent = framed.send(encode_response(&response)?).await;
        if shutting_down {
            // A durable shutdown owns process termination even when the peer
            // disappears before its best-effort acknowledgement is delivered.
            lifecycle.request_shutdown();
        }
        sent?;
        if shutting_down {
            break;
        }
    }
    streams.disconnect().await;
    Ok(())
}

fn remove_stale_socket(path: &Path) -> Result<()> {
    match fs::symlink_metadata(path) {
        Ok(metadata)
            if metadata.file_type().is_socket()
                && metadata.uid() == rustix::process::geteuid().as_raw() =>
        {
            let mode = metadata.mode() & 0o777;
            if mode != SOCKET_MODE {
                bail!(
                    "refusing to replace Unix socket {} with insecure mode {mode:04o}",
                    path.display()
                );
            }
            match std::os::unix::net::UnixStream::connect(path) {
                Ok(_) => bail!("a daemon is already listening on {}", path.display()),
                Err(error) if error.kind() == std::io::ErrorKind::ConnectionRefused => {
                    fs::remove_file(path)
                        .with_context(|| format!("remove stale Unix socket {}", path.display()))?;
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => {
                    return Err(error)
                        .with_context(|| format!("probe existing Unix socket {}", path.display()));
                }
            }
        }
        Ok(metadata) if metadata.file_type().is_socket() => bail!(
            "refusing to replace Unix socket {} owned by uid {}",
            path.display(),
            metadata.uid()
        ),
        Ok(_) => bail!("refusing to replace non-socket path {}", path.display()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => {
            return Err(error).with_context(|| format!("inspect Unix socket {}", path.display()));
        }
    }
    Ok(())
}

#[derive(Debug)]
struct SocketPathGuard {
    path: PathBuf,
    device: u64,
    inode: u64,
}

impl Drop for SocketPathGuard {
    fn drop(&mut self) {
        if fs::symlink_metadata(&self.path).is_ok_and(|metadata| {
            metadata.file_type().is_socket()
                && metadata.dev() == self.device
                && metadata.ino() == self.inode
        }) {
            let _ = fs::remove_file(&self.path);
        }
    }
}

#[cfg(test)]
mod tests {
    use std::fmt;
    use std::sync::Arc;

    use anyhow::Result;
    use async_trait::async_trait;
    use tokio::sync::Notify;

    use super::*;
    use crate::host::{HostApi, HostEventStream};
    use crate::protocol::{CliRequest, Command, GraphRevision, outcome};

    struct EchoHost;

    #[derive(Debug, Default)]
    struct BlockingShutdownHost {
        started: Notify,
        release: Notify,
    }

    impl EchoHost {
        fn new() -> Self {
            Self
        }
    }

    impl fmt::Debug for EchoHost {
        fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
            formatter.write_str("EchoHost")
        }
    }

    #[async_trait]
    impl HostApi for EchoHost {
        async fn submit(&self, command: CommandEnvelope) -> Result<CommandOutcomeEnvelope> {
            let payload = match command.payload {
                Command::RotateToken => {
                    // A repeated command_id receives its cached durable generation.
                    CommandOutcome::TokenRotated { generation: 1 }
                }
                _ => CommandOutcome::ShuttingDown,
            };
            Ok(outcome(command.command_id, GraphRevision(0), payload))
        }

        async fn subscribe(&self, _after_cursor: u64) -> Result<HostEventStream> {
            Ok(Box::pin(futures_util::stream::pending()))
        }

        fn graph_revision(&self) -> GraphRevision {
            GraphRevision(0)
        }

        fn token_generation(&self) -> u64 {
            0
        }

        async fn shutdown(&self) -> Result<()> {
            Ok(())
        }
    }

    #[async_trait]
    impl HostApi for BlockingShutdownHost {
        async fn submit(&self, command: CommandEnvelope) -> Result<CommandOutcomeEnvelope> {
            self.started.notify_one();
            self.release.notified().await;
            Ok(outcome(
                command.command_id,
                GraphRevision(0),
                CommandOutcome::ShuttingDown,
            ))
        }

        async fn subscribe(&self, _after_cursor: u64) -> Result<HostEventStream> {
            Ok(Box::pin(futures_util::stream::pending()))
        }

        fn graph_revision(&self) -> GraphRevision {
            GraphRevision(0)
        }

        fn token_generation(&self) -> u64 {
            0
        }

        async fn shutdown(&self) -> Result<()> {
            Ok(())
        }
    }

    #[tokio::test]
    async fn trusted_peer_ndjson_request_round_trips_without_a_bearer() {
        let directory = tempfile::tempdir().unwrap();
        let socket = directory.path().join("run").join("daemon.sock");
        let token_file = directory.path().join("run").join("daemon.token");
        let auth = AuthState::initialize(&token_file).unwrap();
        let server = UnixServer::bind(&socket).unwrap();
        assert_eq!(fs::metadata(&socket).unwrap().mode() & 0o777, 0o600);
        let cancellation = CancellationToken::new();
        let task = tokio::spawn(server.serve(
            Arc::new(EchoHost::new()),
            auth,
            DaemonLifecycle::default(),
            cancellation.clone(),
        ));

        let response = send_command(&socket, CliRequest::QueryGraph.into_envelope())
            .await
            .unwrap();
        assert_eq!(response.payload, CommandOutcome::ShuttingDown);

        cancellation.cancel();
        task.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn unix_connection_admission_is_bounded() {
        let directory = tempfile::tempdir().unwrap();
        let socket = directory.path().join("run").join("daemon.sock");
        let auth =
            AuthState::initialize(directory.path().join("run").join("daemon.token")).unwrap();
        let server = UnixServer::bind(&socket).unwrap();
        let cancellation = CancellationToken::new();
        let task = tokio::spawn(server.serve_with_limit(
            Arc::new(EchoHost::new()),
            auth,
            DaemonLifecycle::default(),
            cancellation.clone(),
            1,
        ));

        let slow = UnixStream::connect(&socket).await.unwrap();
        let queued = tokio::spawn({
            let socket = socket.clone();
            async move { send_command(&socket, CliRequest::QueryGraph.into_envelope()).await }
        });
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        assert!(!queued.is_finished());

        drop(slow);
        let response = tokio::time::timeout(std::time::Duration::from_secs(1), queued)
            .await
            .expect("queued Unix client admitted after the first disconnects")
            .unwrap()
            .unwrap();
        assert_eq!(response.payload, CommandOutcome::ShuttingDown);

        cancellation.cancel();
        task.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn token_rotation_keeps_the_trusted_unix_session_open() {
        let directory = tempfile::tempdir().unwrap();
        let socket = directory.path().join("run").join("daemon.sock");
        let token_file = directory.path().join("run").join("daemon.token");
        let auth = AuthState::initialize(&token_file).unwrap();
        let server = UnixServer::bind(&socket).unwrap();
        let cancellation = CancellationToken::new();
        let task = tokio::spawn(server.serve(
            Arc::new(EchoHost::new()),
            auth,
            DaemonLifecycle::default(),
            cancellation.clone(),
        ));

        let stream = UnixStream::connect(&socket).await.unwrap();
        let mut framed = Framed::new(stream, ndjson_response_codec());
        let rotate = CliRequest::RotateToken {
            operation_id: "rotate-token".to_owned(),
        }
        .into_envelope();
        framed.send(encode_request(&rotate).unwrap()).await.unwrap();
        let first: CommandOutcomeEnvelope =
            serde_json::from_str(&framed.next().await.unwrap().unwrap()).unwrap();
        assert_eq!(
            first.payload,
            CommandOutcome::TokenRotated { generation: 1 }
        );
        let after_first = crate::auth::read_token_file(&token_file).unwrap();

        framed.send(encode_request(&rotate).unwrap()).await.unwrap();
        let replay: CommandOutcomeEnvelope =
            serde_json::from_str(&framed.next().await.unwrap().unwrap()).unwrap();
        assert_eq!(
            replay.payload,
            CommandOutcome::TokenRotated { generation: 1 }
        );
        let after_replay = crate::auth::read_token_file(&token_file).unwrap();
        assert_eq!(after_first.expose(), after_replay.expose());

        let query = CliRequest::QueryGraph.into_envelope();
        framed.send(encode_request(&query).unwrap()).await.unwrap();
        let second: CommandOutcomeEnvelope =
            serde_json::from_str(&framed.next().await.unwrap().unwrap()).unwrap();
        assert_eq!(second.payload, CommandOutcome::ShuttingDown);

        cancellation.cancel();
        task.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn shutdown_outcome_is_flushed_before_unix_admission_stops() {
        let directory = tempfile::tempdir().unwrap();
        let socket = directory.path().join("run").join("daemon.sock");
        let token_file = directory.path().join("run").join("daemon.token");
        let auth = AuthState::initialize(&token_file).unwrap();
        let server = UnixServer::bind(&socket).unwrap();
        let cancellation = CancellationToken::new();
        let lifecycle = DaemonLifecycle::default();
        let task = tokio::spawn(server.serve(
            Arc::new(EchoHost::new()),
            auth,
            lifecycle.clone(),
            cancellation,
        ));

        let response = send_command(&socket, CommandEnvelope::new("restart", Command::Shutdown))
            .await
            .unwrap();
        assert!(matches!(response.payload, CommandOutcome::ShuttingDown));
        assert!(!lifecycle.is_restarting());
        task.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn lost_shutdown_ack_still_stops_unix_admission() {
        let directory = tempfile::tempdir().unwrap();
        let socket = directory.path().join("run").join("daemon.sock");
        let token_file = directory.path().join("run").join("daemon.token");
        let auth = AuthState::initialize(&token_file).unwrap();
        let server = UnixServer::bind(&socket).unwrap();
        let cancellation = CancellationToken::new();
        let lifecycle = DaemonLifecycle::default();
        let host = Arc::new(BlockingShutdownHost::default());
        let task =
            tokio::spawn(server.serve(host.clone(), auth, lifecycle.clone(), cancellation.clone()));

        let stream = UnixStream::connect(&socket).await.unwrap();
        let mut framed = Framed::new(stream, ndjson_response_codec());
        let request = CommandEnvelope::new("lost-shutdown-ack", Command::Shutdown);
        framed
            .send(encode_request(&request).unwrap())
            .await
            .unwrap();
        host.started.notified().await;
        drop(framed);
        host.release.notify_one();

        tokio::time::timeout(std::time::Duration::from_secs(1), lifecycle.restarting())
            .await
            .expect("durable shutdown must stop admission even when its response is lost");
        task.await.unwrap().unwrap();
        cancellation.cancel();
    }

    #[tokio::test]
    async fn token_publication_failure_stops_daemon_admission_for_reconciliation() {
        let directory = tempfile::tempdir().unwrap();
        let runtime = directory.path().join("run");
        let socket = runtime.join("daemon.sock");
        let token_file = runtime.join("daemon.token");
        let auth = AuthState::initialize(&token_file).unwrap();
        let server = UnixServer::bind(&socket).unwrap();
        let cancellation = CancellationToken::new();
        let lifecycle = DaemonLifecycle::default();
        let task = tokio::spawn(server.serve(
            Arc::new(EchoHost::new()),
            auth.clone(),
            lifecycle.clone(),
            cancellation,
        ));
        fs::set_permissions(&runtime, fs::Permissions::from_mode(0o500)).unwrap();

        let result = send_command(
            &socket,
            CliRequest::RotateToken {
                operation_id: "rotate-token-failure".to_owned(),
            }
            .into_envelope(),
        )
        .await;
        assert!(result.is_err());
        task.await.unwrap().unwrap();
        assert!(lifecycle.is_restarting());
        assert_eq!(auth.generation(), 0);

        fs::set_permissions(runtime, fs::Permissions::from_mode(0o700)).unwrap();
    }

    #[test]
    fn client_rejects_an_insecure_socket_mode() {
        let directory = tempfile::tempdir().unwrap();
        let socket = directory.path().join("daemon.sock");
        let _listener = std::os::unix::net::UnixListener::bind(&socket).unwrap();
        fs::set_permissions(&socket, fs::Permissions::from_mode(0o666)).unwrap();

        assert!(validate_socket_path(&socket).is_err());
    }

    #[tokio::test]
    async fn second_daemon_cannot_replace_an_active_socket() {
        let directory = tempfile::tempdir().unwrap();
        let socket = directory.path().join("run").join("daemon.sock");
        let first = UnixServer::bind(&socket).unwrap();

        let error = UnixServer::bind(&socket).unwrap_err();
        assert!(error.to_string().contains("already listening"));
        assert!(socket.exists());

        drop(first);
        assert!(!socket.exists());
    }
}
