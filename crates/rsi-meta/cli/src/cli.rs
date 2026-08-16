use std::io::{self, Write};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use async_trait::async_trait;
use clap::{Args, Parser, Subcommand};
use serde_json::json;
use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;

use crate::auth::AuthState;
use crate::composition::CompositionHostOpener;
use crate::host::SharedHost;
use crate::http::{HttpServer, require_loopback};
use crate::lifecycle::DaemonLifecycle;
use crate::protocol::{CliRequest, CommandOutcome, CommandOutcomeEnvelope};
use crate::unix::{UnixServer, send_command};

#[derive(Debug, Parser)]
#[command(name = "rsi-meta", version, about = "Rust-native composition host")]
struct Cli {
    /// Directory containing the daemon socket, bearer token and host state.
    #[arg(long, global = true, env = "RSI_META_STATE_DIR")]
    pub state_dir: Option<PathBuf>,

    /// Override the local daemon Unix socket path.
    #[arg(long, global = true, env = "RSI_META_SOCKET")]
    pub socket: Option<PathBuf>,

    /// Override the daemon bearer-token file path.
    #[arg(long, global = true, env = "RSI_META_TOKEN_FILE")]
    pub token_file: Option<PathBuf>,

    #[command(subcommand)]
    pub command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Validate a composition manifest without publishing it.
    Validate(ManifestArgs),
    /// Resolve and report the deterministic composition lock.
    Lock(ManifestArgs),
    /// Run the foreground daemon.
    Daemon {
        #[command(subcommand)]
        command: DaemonCommand,
    },
    /// Transactionally apply a composition manifest.
    Apply(MutationManifestArgs),
    /// Install a composition into an offline workspace.
    Install(MutationManifestArgs),
    /// Print the current published graph.
    Graph,
    /// Read durable control events after a cursor.
    Events {
        #[arg(long, default_value_t = 0)]
        after: u64,

        #[arg(long, default_value_t = 1_000)]
        limit: u32,
    },
    /// Inspect plugin state.
    Plugin {
        #[command(subcommand)]
        command: PluginCommand,
    },
    /// Manage the local transport token.
    Token {
        #[command(subcommand)]
        command: TokenCommand,
    },
}

#[derive(Debug, Args)]
struct ManifestArgs {
    pub manifest: PathBuf,

    /// Lockfile paired with the manifest (defaults to rsi-meta.lock beside it).
    #[arg(long)]
    pub lock: Option<PathBuf>,
}

#[derive(Debug, Args)]
struct MutationManifestArgs {
    pub manifest: PathBuf,

    #[arg(long)]
    pub lock: Option<PathBuf>,

    /// Durable operation identity; defaults to `UUIDv7`.
    #[arg(long)]
    pub operation_id: Option<String>,
}

#[derive(Debug, Subcommand)]
enum DaemonCommand {
    Serve {
        /// Loopback-only HTTP/WebSocket bind address.
        #[arg(long, default_value = "127.0.0.1:0")]
        http_bind: std::net::SocketAddr,

        /// Extra exact browser Origins accepted for WebSocket upgrades.
        #[arg(long = "allow-origin")]
        allow_origins: Vec<String>,
    },
    /// Request an orderly daemon shutdown.
    Stop {
        #[arg(long)]
        operation_id: Option<String>,
    },
}

#[derive(Debug, Subcommand)]
enum PluginCommand {
    Inspect { instance_id: String },
}

#[derive(Debug, Subcommand)]
enum TokenCommand {
    /// Generate a fresh daemon token and disconnect authenticated streams.
    Rotate {
        #[arg(long)]
        operation_id: Option<String>,
    },
}

#[derive(Clone, Debug)]
pub(crate) struct HostOpenRequest {
    pub state_dir: PathBuf,
}

pub(crate) struct OpenedHost {
    pub host: SharedHost,
}

#[async_trait]
pub(crate) trait HostOpener: Send + Sync {
    async fn open(&self, request: HostOpenRequest) -> Result<OpenedHost>;

    async fn validate(
        &self,
        project: rsi_meta::CompositionProject,
    ) -> Result<rsi_meta::ValidationReport>;

    async fn lock(&self, project: rsi_meta::CompositionProject) -> Result<rsi_meta::LockResult>;

    async fn install(&self, request: rsi_meta::InstallRequest) -> Result<rsi_meta::InstallResult>;
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CliExit {
    Success,
    DaemonRestart,
}

impl CliExit {
    pub fn code(self) -> u8 {
        match self {
            Self::Success => 0,
            Self::DaemonRestart => crate::DAEMON_RESTART_EXIT_CODE,
        }
    }
}

/// Parses process arguments and runs the production CLI.
///
/// # Errors
///
/// Returns an error if argument handling, an offline core operation, daemon
/// startup, transport I/O, or graceful shutdown fails.
pub async fn run() -> Result<CliExit> {
    run_cli(Cli::parse(), &CompositionHostOpener).await
}

#[allow(clippy::too_many_lines)] // one dispatcher keeps CLI command ownership in one place
async fn run_cli(cli: Cli, opener: &dyn HostOpener) -> Result<CliExit> {
    let Cli {
        state_dir,
        socket,
        token_file,
        command,
    } = cli;
    match command {
        Command::Validate(args) => {
            let report = opener
                .validate(rsi_meta::CompositionProject {
                    manifest_path: args.manifest.clone(),
                    lock_path: args
                        .lock
                        .map(|lock| paired_lock_path(&args.manifest, Some(lock))),
                })
                .await?;
            println!("{}", serde_json::to_string_pretty(&report)?);
            if !report.is_valid() {
                bail!("composition validation failed");
            }
            Ok(CliExit::Success)
        }
        Command::Lock(args) => {
            let lock = paired_lock_path(&args.manifest, args.lock);
            let result = opener
                .lock(rsi_meta::CompositionProject {
                    manifest_path: args.manifest,
                    lock_path: Some(lock),
                })
                .await?;
            println!("{}", serde_json::to_string_pretty(&result)?);
            Ok(CliExit::Success)
        }
        command => match command {
            Command::Daemon {
                command:
                    DaemonCommand::Serve {
                        http_bind,
                        allow_origins,
                    },
            } => {
                let paths = RuntimePaths::resolve(state_dir, socket, token_file)?;
                serve_daemon(paths, http_bind, allow_origins, opener).await
            }
            Command::Daemon {
                command: DaemonCommand::Stop { operation_id },
            } => {
                run_client(
                    resolve_client_socket(state_dir, socket)?,
                    CliRequest::Shutdown {
                        operation_id: operation_id.unwrap_or_else(new_operation_id),
                    },
                )
                .await?;
                Ok(CliExit::Success)
            }
            Command::Apply(args) => {
                let manifest = absolute_cli_path(&args.manifest)?;
                let lock = match args.lock {
                    Some(lock) => absolute_cli_path(&lock)?,
                    None => paired_lock_path(&manifest, None),
                };
                let response = run_client(
                    resolve_client_socket(state_dir, socket)?,
                    CliRequest::ApplyManifest {
                        manifest,
                        lock,
                        operation_id: args.operation_id.unwrap_or_else(new_operation_id),
                    },
                )
                .await?;
                Ok(
                    if matches!(response.payload, CommandOutcome::RestartRequired { .. }) {
                        CliExit::DaemonRestart
                    } else {
                        CliExit::Success
                    },
                )
            }
            Command::Install(args) => {
                let paths = RuntimePaths::resolve(state_dir, socket, token_file)?;
                let lock = paired_lock_path(&args.manifest, args.lock);
                let operation_id = args.operation_id.unwrap_or_else(new_operation_id);
                announce_operation_id(&operation_id)?;
                let result = opener
                    .install(rsi_meta::InstallRequest {
                        operation_id: rsi_meta::OperationId(operation_id.clone()),
                        workspace: crate::composition::workspace(&paths.state_dir),
                        project: rsi_meta::CompositionProject {
                            manifest_path: args.manifest,
                            lock_path: Some(lock),
                        },
                    })
                    .await?;
                println!(
                    "{}",
                    serde_json::to_string_pretty(&json!({
                        "operation_id": operation_id,
                        "result": result,
                    }))?
                );
                Ok(CliExit::Success)
            }
            Command::Graph => {
                run_client(
                    resolve_client_socket(state_dir, socket)?,
                    CliRequest::QueryGraph,
                )
                .await?;
                Ok(CliExit::Success)
            }
            Command::Events { after, limit } => {
                run_client(
                    resolve_client_socket(state_dir, socket)?,
                    CliRequest::QueryEvents { after, limit },
                )
                .await?;
                Ok(CliExit::Success)
            }
            Command::Plugin {
                command: PluginCommand::Inspect { instance_id },
            } => {
                run_client(
                    resolve_client_socket(state_dir, socket)?,
                    CliRequest::InspectPlugin { instance_id },
                )
                .await?;
                Ok(CliExit::Success)
            }
            Command::Token {
                command: TokenCommand::Rotate { operation_id },
            } => {
                run_client(
                    resolve_client_socket(state_dir, socket)?,
                    CliRequest::RotateToken {
                        operation_id: operation_id.unwrap_or_else(new_operation_id),
                    },
                )
                .await?;
                Ok(CliExit::Success)
            }
            Command::Validate(_) | Command::Lock(_) => {
                unreachable!("offline commands returned before runtime-path resolution")
            }
        },
    }
}

async fn run_client(socket: PathBuf, request: CliRequest) -> Result<CommandOutcomeEnvelope> {
    let operation_id = durable_operation_id(&request).map(ToOwned::to_owned);
    if let Some(operation_id) = operation_id.as_deref() {
        announce_operation_id(operation_id)?;
    }
    let response = send_command(&socket, request.into_envelope()).await?;
    let durable_operation = operation_id.is_some();
    print_response(&response, durable_operation)?;
    if response_is_failure(&response) {
        bail!("daemon rejected the command");
    }
    Ok(response)
}

fn durable_operation_id(request: &CliRequest) -> Option<&str> {
    match request {
        CliRequest::ApplyManifest { operation_id, .. }
        | CliRequest::RotateToken { operation_id }
        | CliRequest::Shutdown { operation_id } => Some(operation_id),
        CliRequest::QueryGraph
        | CliRequest::QueryEvents { .. }
        | CliRequest::InspectPlugin { .. } => None,
    }
}

fn announce_operation_id(operation_id: &str) -> Result<()> {
    write_operation_announcement(&mut io::stderr().lock(), operation_id)
}

fn write_operation_announcement(writer: &mut impl Write, operation_id: &str) -> Result<()> {
    writeln!(writer, "operation_id={operation_id}")
        .context("write operation ID before execution")?;
    writer
        .flush()
        .context("flush operation ID before execution")
}

fn response_is_failure(response: &CommandOutcomeEnvelope) -> bool {
    matches!(&response.payload, CommandOutcome::Rejected { .. })
}

fn new_operation_id() -> String {
    uuid::Uuid::now_v7().to_string()
}

async fn serve_daemon(
    paths: RuntimePaths,
    http_bind: std::net::SocketAddr,
    allow_origins: Vec<String>,
    opener: &dyn HostOpener,
) -> Result<CliExit> {
    // Reject an unsafe listener before claiming the Unix socket, opening the
    // durable host, or creating bearer material. A refused daemon start must
    // not have security-relevant side effects.
    require_loopback(http_bind)?;
    // Claim the single-user control endpoint before opening durable state or
    // publishing a fresh bearer, so a second daemon cannot disrupt the first.
    let unix = UnixServer::bind(&paths.socket)?;
    let OpenedHost { host } = open_daemon_host(&paths, opener).await?;
    let auth = match AuthState::initialize(&paths.token_file) {
        Ok(auth) => auth,
        Err(error) => return Err(shutdown_after_startup_error(&host, error).await),
    };
    if let Err(error) = auth.reconcile_generation(host.token_generation()) {
        return Err(shutdown_after_startup_error(&host, error).await);
    }
    let lifecycle = DaemonLifecycle::default();
    let http = match HttpServer::bind(
        http_bind,
        host.clone(),
        auth.clone(),
        lifecycle.clone(),
        allow_origins,
    )
    .await
    {
        Ok(http) => http,
        Err(error) => return Err(shutdown_after_startup_error(&host, error).await),
    };
    let http_address = http.local_addr();
    let cancellation = CancellationToken::new();
    let mut servers = JoinSet::new();
    servers.spawn(unix.serve(host.clone(), auth, lifecycle.clone(), cancellation.clone()));
    servers.spawn(http.serve(cancellation.clone()));

    println!(
        "{}",
        serde_json::to_string(&json!({
            "status": "ready",
            "socket": paths.socket,
            "http": http_address,
            "token_file": paths.token_file,
        }))?
    );

    let mut lifecycle_stop = false;
    let mut host_termination = None;
    let early_exit = tokio::select! {
        biased;
        () = lifecycle.restarting() => {
            lifecycle_stop = true;
            None
        }
        signal = shutdown_signal() => {
            signal?;
            None
        }
        result = host.monitor_terminated() => {
            host_termination = Some(result);
            None
        }
        joined = servers.join_next() => joined,
    };
    cancellation.cancel();

    let mut server_error = match early_exit {
        Some(Ok(Err(error))) => Some(error),
        Some(Err(error)) => Some(anyhow::Error::new(error)),
        Some(Ok(Ok(()))) => Some(anyhow::anyhow!("daemon transport stopped unexpectedly")),
        None => None,
    };
    while let Some(joined) = servers.join_next().await {
        match joined {
            Ok(Err(error)) if server_error.is_none() => server_error = Some(error),
            Err(error) if server_error.is_none() => server_error = Some(anyhow::Error::new(error)),
            Ok(Ok(()) | Err(_)) | Err(_) => {}
        }
    }
    if let Some(termination) = host_termination {
        return match termination {
            Ok(()) => Err(anyhow::anyhow!(
                "composition host terminated independently of daemon lifecycle"
            )),
            Err(error) => {
                Err(error.context("composition host failed independently of daemon lifecycle"))
            }
        };
    }
    let shutdown = if lifecycle_stop && !lifecycle.is_restarting() {
        host.wait_terminated().await
    } else {
        host.shutdown().await
    };
    if let Some(error) = server_error {
        return Err(error);
    }
    shutdown?;
    Ok(if lifecycle.is_restarting() {
        CliExit::DaemonRestart
    } else {
        CliExit::Success
    })
}

#[cfg(unix)]
async fn shutdown_signal() -> Result<()> {
    let mut terminate = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        .context("install SIGTERM handler")?;
    tokio::select! {
        signal = tokio::signal::ctrl_c() => signal.context("install Ctrl-C handler"),
        signal = terminate.recv() => {
            signal.context("SIGTERM handler closed unexpectedly")?;
            Ok(())
        }
    }
}

#[cfg(not(unix))]
async fn shutdown_signal() -> Result<()> {
    tokio::signal::ctrl_c()
        .await
        .context("install Ctrl-C handler")
}

async fn open_daemon_host(paths: &RuntimePaths, opener: &dyn HostOpener) -> Result<OpenedHost> {
    opener
        .open(HostOpenRequest {
            state_dir: paths.state_dir.clone(),
        })
        .await
}

async fn shutdown_after_startup_error(host: &SharedHost, error: anyhow::Error) -> anyhow::Error {
    match host.shutdown().await {
        Ok(()) => error,
        Err(shutdown_error) => error.context(format!(
            "composition host also failed to shut down after startup error: {shutdown_error:#}"
        )),
    }
}

fn paired_lock_path(manifest: &Path, explicit: Option<PathBuf>) -> PathBuf {
    explicit.unwrap_or_else(|| {
        manifest
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .join("rsi-meta.lock")
    })
}

fn absolute_cli_path(path: &Path) -> Result<PathBuf> {
    std::path::absolute(path)
        .with_context(|| format!("resolve CLI path {} against the client cwd", path.display()))
}

fn print_response(response: &CommandOutcomeEnvelope, durable_operation: bool) -> Result<()> {
    let output = if durable_operation {
        json!({
            "operation_id": response.command_id,
            "result": response.payload,
        })
    } else {
        serde_json::to_value(&response.payload)?
    };
    println!("{}", serde_json::to_string_pretty(&output)?);
    Ok(())
}

#[derive(Clone, Debug)]
struct RuntimePaths {
    state_dir: PathBuf,
    socket: PathBuf,
    token_file: PathBuf,
}

impl RuntimePaths {
    fn resolve(
        state_dir: Option<PathBuf>,
        socket: Option<PathBuf>,
        token_file: Option<PathBuf>,
    ) -> Result<Self> {
        let state_dir = match state_dir {
            Some(state_dir) => state_dir,
            None => default_state_dir()?,
        };
        if state_dir.as_os_str().is_empty() {
            bail!("state directory cannot be empty");
        }
        let socket = socket.unwrap_or_else(|| state_dir.join("daemon.sock"));
        let token_file = token_file.unwrap_or_else(|| state_dir.join("daemon.token"));
        Ok(Self {
            state_dir,
            socket,
            token_file,
        })
    }
}

fn resolve_client_socket(
    state_dir: Option<PathBuf>,
    explicit_socket: Option<PathBuf>,
) -> Result<PathBuf> {
    if let Some(socket) = explicit_socket {
        if socket.as_os_str().is_empty() {
            bail!("socket path cannot be empty");
        }
        return Ok(socket);
    }
    let state_dir = match state_dir {
        Some(state_dir) => state_dir,
        None => default_state_dir()?,
    };
    if state_dir.as_os_str().is_empty() {
        bail!("state directory cannot be empty");
    }
    Ok(state_dir.join("daemon.sock"))
}

fn default_state_dir() -> Result<PathBuf> {
    if let Some(override_dir) = absolute_environment_path("RSI_META_HOME")? {
        return Ok(override_dir);
    }

    #[cfg(target_os = "macos")]
    if let Some(home) = absolute_environment_path("HOME")? {
        return Ok(home
            .join("Library")
            .join("Application Support")
            .join("rsi-meta"));
    }

    #[cfg(not(target_os = "macos"))]
    {
        if let Some(state_home) = absolute_environment_path("XDG_STATE_HOME")? {
            return Ok(state_home.join("rsi-meta"));
        }
        if let Some(home) = absolute_environment_path("HOME")? {
            return Ok(home.join(".local/state/rsi-meta"));
        }
    }

    bail!(
        "no stable default state directory is available; pass --state-dir or set RSI_META_STATE_DIR"
    )
}

fn absolute_environment_path(name: &str) -> Result<Option<PathBuf>> {
    let Some(value) = std::env::var_os(name) else {
        return Ok(None);
    };
    let path = PathBuf::from(value);
    if path.as_os_str().is_empty() || !path.is_absolute() {
        bail!("{name} must be an absolute, non-empty path");
    }
    Ok(Some(path))
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use crate::host::{HostApi, HostEventStream};
    use crate::protocol::{CommandEnvelope, CommandOutcomeEnvelope, GraphRevision};

    use super::*;

    #[test]
    fn explicit_runtime_paths_win() {
        let paths = RuntimePaths::resolve(
            Some("state".into()),
            Some("socket".into()),
            Some("token".into()),
        )
        .unwrap();
        assert_eq!(paths.state_dir, PathBuf::from("state"));
        assert_eq!(paths.socket, PathBuf::from("socket"));
        assert_eq!(paths.token_file, PathBuf::from("token"));
    }

    #[test]
    fn explicit_client_socket_does_not_need_a_state_directory() {
        assert_eq!(
            resolve_client_socket(None, Some(PathBuf::from("control.sock"))).unwrap(),
            PathBuf::from("control.sock")
        );
    }

    #[test]
    fn operation_identity_is_flushed_as_a_recovery_handle() {
        let mut output = Vec::new();
        write_operation_announcement(&mut output, "019c-operation").unwrap();
        assert_eq!(output, b"operation_id=019c-operation\n");
    }

    #[test]
    fn clap_exposes_the_required_command_tree() {
        use clap::CommandFactory;
        Cli::command().debug_assert();
    }

    #[test]
    fn daemon_serve_uses_the_state_directory_workspace() {
        assert!(Cli::try_parse_from(["rsi-meta", "daemon", "serve"]).is_ok());
        assert_eq!(
            paired_lock_path(Path::new("composition/rsi-meta.toml"), None),
            PathBuf::from("composition/rsi-meta.lock")
        );
    }

    #[test]
    fn daemon_bound_project_paths_are_made_absolute_by_the_client() {
        let relative = Path::new("candidate/composition.toml");
        let absolute = absolute_cli_path(relative).unwrap();
        assert!(absolute.is_absolute());
        assert!(absolute.ends_with(relative));
    }

    #[test]
    fn events_uses_the_durable_after_cursor_name() {
        assert!(Cli::try_parse_from(["rsi-meta", "events", "--after", "42"]).is_ok());
        assert!(Cli::try_parse_from(["rsi-meta", "events", "--cursor", "42"]).is_err());
    }

    #[derive(Debug)]
    struct TerminatingHost {
        terminated: CancellationToken,
        shutdowns: AtomicUsize,
    }

    #[async_trait]
    impl HostApi for TerminatingHost {
        async fn submit(&self, _command: CommandEnvelope) -> Result<CommandOutcomeEnvelope> {
            bail!("not used")
        }

        async fn subscribe(&self, _after_cursor: u64) -> Result<HostEventStream> {
            Ok(Box::pin(futures_util::stream::empty()))
        }

        fn graph_revision(&self) -> GraphRevision {
            GraphRevision(0)
        }

        fn token_generation(&self) -> u64 {
            0
        }

        async fn shutdown(&self) -> Result<()> {
            self.shutdowns.fetch_add(1, Ordering::Relaxed);
            Ok(())
        }

        async fn monitor_terminated(&self) -> Result<()> {
            self.terminated.cancelled().await;
            Ok(())
        }
    }

    #[derive(Debug)]
    struct TerminatingOpener {
        host: Arc<TerminatingHost>,
    }

    #[async_trait]
    impl HostOpener for TerminatingOpener {
        async fn open(&self, _request: HostOpenRequest) -> Result<OpenedHost> {
            Ok(OpenedHost {
                host: self.host.clone(),
            })
        }

        async fn validate(
            &self,
            _project: rsi_meta::CompositionProject,
        ) -> Result<rsi_meta::ValidationReport> {
            bail!("not used")
        }

        async fn lock(
            &self,
            _project: rsi_meta::CompositionProject,
        ) -> Result<rsi_meta::LockResult> {
            bail!("not used")
        }

        async fn install(
            &self,
            _request: rsi_meta::InstallRequest,
        ) -> Result<rsi_meta::InstallResult> {
            bail!("not used")
        }
    }

    #[tokio::test]
    async fn daemon_stops_admitting_when_the_host_terminates_independently() {
        let directory = tempfile::tempdir().unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(directory.path(), std::fs::Permissions::from_mode(0o700))
                .unwrap();
        }
        let paths = RuntimePaths {
            state_dir: directory.path().to_path_buf(),
            socket: directory.path().join("daemon.sock"),
            token_file: directory.path().join("daemon.token"),
        };
        let host = Arc::new(TerminatingHost {
            terminated: CancellationToken::new(),
            shutdowns: AtomicUsize::new(0),
        });
        let opener = TerminatingOpener { host: host.clone() };
        let daemon = tokio::spawn(async move {
            serve_daemon(paths, "127.0.0.1:0".parse().unwrap(), Vec::new(), &opener).await
        });
        for _ in 0..100 {
            if directory.path().join("daemon.sock").exists() {
                break;
            }
            tokio::task::yield_now().await;
        }
        tokio::task::yield_now().await;
        assert!(
            !daemon.is_finished(),
            "daemon exited before host termination"
        );
        host.terminated.cancel();
        let result = tokio::time::timeout(std::time::Duration::from_secs(1), daemon)
            .await
            .expect("daemon must observe host termination")
            .expect("daemon task");
        assert!(result.is_err());
        assert_eq!(host.shutdowns.load(Ordering::Relaxed), 0);
    }
}
