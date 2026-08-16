#![cfg(unix)]

use std::fs;
use std::net::SocketAddr;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::{Command as StdCommand, ExitStatus, Stdio};
use std::sync::Arc;
use std::sync::OnceLock;
use std::time::Duration;

use anyhow::{Context, Result, bail, ensure};
use futures_util::{SinkExt, StreamExt};
use rsi_meta::{
    GraphRevision, STREAM_PROTOCOL, ServiceKey, ServiceOpenRequest, StreamEnvelope, StreamKind,
};
use rsi_meta_cli::protocol::{
    Command as CoreCommand, CommandEnvelope, CommandOutcome, CommandOutcomeEnvelope, EventEnvelope,
};
use rsi_meta_loader::{ApiVersion, BUILD_TARGET};
use rusqlite::{Connection, OpenFlags};
use serde::Deserialize;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::{TcpStream, UnixStream};
use tokio::process::{Child, ChildStderr, ChildStdout, Command as ProcessCommand};
use tokio::sync::Barrier;
use tokio::time::timeout;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http::{HeaderValue, Request, StatusCode, header};
use tokio_tungstenite::tungstenite::protocol::frame::coding::CloseCode;
use tokio_tungstenite::tungstenite::{Error as WebSocketError, Message};
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream, connect_async};
use tokio_util::codec::{Framed, LinesCodec};
use tokio_util::sync::CancellationToken;

const IO_DEADLINE: Duration = Duration::from_secs(15);
const MAX_CONTROL_BYTES: usize = 1024 * 1024;
const TRUSTED_ORIGIN: &str = "https://trusted.example";

type ClientWebSocket = WebSocketStream<MaybeTlsStream<TcpStream>>;
type UnixControl = Framed<UnixStream, LinesCodec>;

#[derive(Debug)]
struct Fixture {
    _temporary: tempfile::TempDir,
    state_dir: PathBuf,
    manifest_b: PathBuf,
    lock_b: PathBuf,
    manifest_c: PathBuf,
    lock_c: PathBuf,
}

#[derive(Debug)]
struct PluginOriginFixture {
    daemon: Fixture,
    hmr_manifest: PathBuf,
}

impl Fixture {
    async fn new() -> Result<Self> {
        let temporary = tempfile::tempdir().context("create daemon test directory")?;
        let root = temporary.path();
        let state_dir = root.join("state");
        let manifest_a = root.join("composition-a.toml");
        let lock_a = root.join("composition-a.lock");
        let manifest_b = root.join("composition-b.toml");
        let lock_b = root.join("composition-b.lock");
        let manifest_c = root.join("composition-c.toml");
        let lock_c = root.join("composition-c.lock");

        write_manifest(&manifest_a, "revision-a")?;
        write_manifest(&manifest_b, "revision-b")?;
        write_manifest(&manifest_c, "revision-c")?;
        resolve_lock(&state_dir, &manifest_a, &lock_a).await?;
        resolve_lock(&state_dir, &manifest_b, &lock_b).await?;
        resolve_lock(&state_dir, &manifest_c, &lock_c).await?;
        install_candidate(&state_dir, &manifest_a, &lock_a, "initial-install").await?;
        Ok(Self {
            _temporary: temporary,
            state_dir,
            manifest_b,
            lock_b,
            manifest_c,
            lock_c,
        })
    }

    async fn new_with_echo() -> Result<Self> {
        let temporary = tempfile::tempdir().context("create stream test directory")?;
        let root = temporary.path();
        let state_dir = root.join("state");
        let manifest_a = root.join("composition-a.toml");
        let lock_a = root.join("composition-a.lock");
        let manifest_b = root.join("composition-b.toml");
        let lock_b = root.join("composition-b.lock");
        let manifest_c = root.join("composition-c.toml");
        let lock_c = root.join("composition-c.lock");

        write_echo_package(root, "provider", &["fixture.echo"], &[])?;
        write_echo_package(root, "consumer", &[], &[("fixture.echo", true)])?;
        write_echo_manifest(&manifest_a)?;
        fs::copy(&manifest_a, &manifest_b)?;
        fs::copy(&manifest_a, &manifest_c)?;
        resolve_lock(&state_dir, &manifest_a, &lock_a).await?;
        resolve_lock(&state_dir, &manifest_b, &lock_b).await?;
        resolve_lock(&state_dir, &manifest_c, &lock_c).await?;
        install_candidate(&state_dir, &manifest_a, &lock_a, "initial-install").await?;
        Ok(Self {
            _temporary: temporary,
            state_dir,
            manifest_b,
            lock_b,
            manifest_c,
            lock_c,
        })
    }

    async fn new_process_fixed() -> Result<Self> {
        let temporary = tempfile::tempdir().context("create restart test directory")?;
        let root = temporary.path();
        let state_dir = root.join("state");
        let manifest_a = root.join("composition-a.toml");
        let lock_a = root.join("composition-a.lock");
        let manifest_b = root.join("composition-b.toml");
        let lock_b = root.join("composition-b.lock");
        let manifest_c = root.join("composition-c.toml");
        let lock_c = root.join("composition-c.lock");

        write_echo_package_as(
            root,
            "provider-a",
            "fixed-provider",
            &["fixture.echo"],
            &[],
            true,
            &[],
        )?;
        write_echo_package_as(
            root,
            "provider-b",
            "fixed-provider",
            &["fixture.echo"],
            &[],
            true,
            &["fixture.changed"],
        )?;
        write_echo_package(root, "consumer", &[], &[("fixture.echo", true)])?;
        write_process_fixed_manifest(&manifest_a, "provider-a")?;
        write_process_fixed_manifest(&manifest_b, "provider-b")?;
        fs::copy(&manifest_b, &manifest_c)?;
        resolve_lock(&state_dir, &manifest_a, &lock_a).await?;
        resolve_lock(&state_dir, &manifest_b, &lock_b).await?;
        resolve_lock(&state_dir, &manifest_c, &lock_c).await?;
        install_candidate(&state_dir, &manifest_a, &lock_a, "initial-install").await?;

        Ok(Self {
            _temporary: temporary,
            state_dir,
            manifest_b,
            lock_b,
            manifest_c,
            lock_c,
        })
    }
}

impl PluginOriginFixture {
    async fn new() -> Result<Self> {
        let temporary =
            tempfile::tempdir().context("create plugin-origin restart test directory")?;
        let root = temporary.path();
        let state_dir = root.join("state");
        let manifest_a = root.join("composition-a.toml");
        let lock_a = root.join("composition-a.lock");
        let manifest_b = root.join("composition-b.toml");
        let lock_b = root.join("composition-b.lock");
        let manifest_c = root.join("composition-c.toml");
        let lock_c = root.join("composition-c.lock");
        let hmr_manifest = root.join("hmr/plugin.toml");
        let built = built_hmr_plugins();

        copy_real_plugin_package(
            root,
            "watcher",
            "plugins/rsi-meta/fs-watch-polling",
            &built.polling,
        )?;
        copy_real_plugin_package(root, "hmr", "plugins/rsi-meta/hmr-consumer", &built.hmr)?;
        write_plugin_origin_manifest(
            &manifest_a,
            &state_dir.join("composition.toml"),
            &state_dir.join("rsi-meta.lock"),
        )?;
        fs::copy(&manifest_a, &manifest_b)?;
        fs::copy(&manifest_a, &manifest_c)?;
        resolve_lock(&state_dir, &manifest_a, &lock_a).await?;
        resolve_lock(&state_dir, &manifest_b, &lock_b).await?;
        resolve_lock(&state_dir, &manifest_c, &lock_c).await?;
        install_candidate(&state_dir, &manifest_a, &lock_a, "initial-install").await?;

        Ok(Self {
            daemon: Fixture {
                _temporary: temporary,
                state_dir,
                manifest_b,
                lock_b,
                manifest_c,
                lock_c,
            },
            hmr_manifest,
        })
    }
}

#[derive(Debug)]
struct BuiltEcho {
    _root: tempfile::TempDir,
    library: PathBuf,
}

#[derive(Debug)]
struct BuiltHmrPlugins {
    _root: tempfile::TempDir,
    polling: PathBuf,
    hmr: PathBuf,
}

fn built_echo() -> &'static BuiltEcho {
    static ECHO: OnceLock<BuiltEcho> = OnceLock::new();
    ECHO.get_or_init(|| {
        let root = tempfile::tempdir().expect("create echo build directory");
        let workspace = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .and_then(Path::parent)
            .and_then(Path::parent)
            .expect("workspace root");
        let status = StdCommand::new(env!("CARGO"))
            .args([
                "build",
                "--quiet",
                "--locked",
                "--release",
                "--offline",
                "--manifest-path",
            ])
            .arg(workspace.join("fixtures/rsi-meta/echo-bidi/Cargo.toml"))
            .env("CARGO_TARGET_DIR", root.path().join("target"))
            .status()
            .expect("build echo fixture");
        assert!(status.success(), "real echo cdylib build failed");
        let library = root.path().join("target").join("release").join(format!(
            "{}rsi_meta_fixture_echo_bidi{}",
            std::env::consts::DLL_PREFIX,
            std::env::consts::DLL_SUFFIX
        ));
        assert!(library.is_file(), "echo fixture cdylib is missing");
        BuiltEcho {
            _root: root,
            library,
        }
    })
}

fn built_hmr_plugins() -> &'static BuiltHmrPlugins {
    static PLUGINS: OnceLock<BuiltHmrPlugins> = OnceLock::new();
    PLUGINS.get_or_init(|| {
        let root = tempfile::tempdir().expect("create HMR plugin build directory");
        let polling = build_cdylib(
            root.path(),
            "plugins/rsi-meta/fs-watch-polling",
            "rsi_meta_plugin_fs_watch_polling",
        );
        let hmr = build_cdylib(
            root.path(),
            "plugins/rsi-meta/hmr-consumer",
            "rsi_meta_plugin_hmr_consumer",
        );
        BuiltHmrPlugins {
            _root: root,
            polling,
            hmr,
        }
    })
}

fn build_cdylib(target: &Path, package: &str, library_stem: &str) -> PathBuf {
    let workspace = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .and_then(Path::parent)
        .expect("workspace root");
    let status = StdCommand::new(env!("CARGO"))
        .args([
            "build",
            "--quiet",
            "--locked",
            "--release",
            "--offline",
            "--target",
            BUILD_TARGET,
            "--manifest-path",
        ])
        .arg(workspace.join(package).join("Cargo.toml"))
        .env("CARGO_TARGET_DIR", target)
        .status()
        .unwrap_or_else(|error| panic!("build {package}: {error}"));
    assert!(status.success(), "real {package} cdylib build failed");
    let library = target.join(BUILD_TARGET).join("release").join(format!(
        "{}{library_stem}{}",
        std::env::consts::DLL_PREFIX,
        std::env::consts::DLL_SUFFIX
    ));
    assert!(
        library.is_file(),
        "fixture cdylib missing: {}",
        library.display()
    );
    library
}

fn copy_real_plugin_package(
    root: &Path,
    destination: &str,
    source: &str,
    library: &Path,
) -> Result<()> {
    let workspace = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .and_then(Path::parent)
        .context("workspace root")?;
    let source = workspace.join(source);
    let package = root.join(destination);
    fs::create_dir_all(&package)?;
    fs::copy(source.join("plugin.toml"), package.join("plugin.toml"))?;
    fs::copy(
        source.join("config.schema.json"),
        package.join("config.schema.json"),
    )?;
    let artifact = package
        .join("target")
        .join(BUILD_TARGET)
        .join("release")
        .join(library.file_name().context("plugin library file name")?);
    fs::create_dir_all(artifact.parent().context("plugin artifact directory")?)?;
    fs::copy(library, artifact)?;
    Ok(())
}

fn write_echo_package(
    root: &Path,
    name: &str,
    provides: &[&str],
    injects: &[(&str, bool)],
) -> Result<()> {
    write_echo_package_as(root, name, name, provides, injects, false, &[])
}

#[allow(clippy::too_many_arguments)]
fn write_echo_package_as(
    root: &Path,
    directory: &str,
    package_id: &str,
    provides: &[&str],
    injects: &[(&str, bool)],
    process_fixed: bool,
    capabilities: &[&str],
) -> Result<()> {
    let package = root.join(directory);
    fs::create_dir_all(&package)?;
    let artifact = format!("artifact{}", std::env::consts::DLL_SUFFIX);
    fs::copy(&built_echo().library, package.join(&artifact))?;
    fs::write(
        package.join("config.schema.json"),
        r#"{"$schema":"https://json-schema.org/draft/2020-12/schema","type":"object","additionalProperties":false}"#,
    )?;
    let provides = provides
        .iter()
        .map(|service| format!("\"{service}\""))
        .collect::<Vec<_>>()
        .join(", ");
    let capabilities = capabilities
        .iter()
        .map(|capability| format!("\"{capability}\""))
        .collect::<Vec<_>>()
        .join(", ");
    let injects = injects
        .iter()
        .map(|(service, required)| {
            format!("[[injects]]\ncontract = \"{service}\"\nrequired = {required}\n")
        })
        .collect::<Vec<_>>()
        .join("\n");
    let api = ApiVersion::CURRENT;
    fs::write(
        package.join("plugin.toml"),
        format!(
            "format_version = 0\nprovides = [{provides}]\ncapabilities = [{capabilities}]\nconfig_schema = \"config.schema.json\"\n\n[package]\nid = \"{package_id}\"\nversion = \"1.0.0\"\nprocess_fixed = {process_fixed}\n\n[host_api]\nmajor = {}\nminimum_minor = {}\n\n[[artifacts]]\ntarget = \"{}\"\npath = \"{artifact}\"\n\n{injects}",
            api.major, api.minor, BUILD_TARGET,
        ),
    )?;
    Ok(())
}

fn write_process_fixed_manifest(path: &Path, provider_directory: &str) -> Result<()> {
    fs::write(
        path,
        format!(
            r#"format_version = 0

[composition]
id = "process-fixed-conformance"
mode = "development"

[[scopes]]
id = "root"

[[scopes]]
id = "app"
parent = "root"

[[instances]]
id = "provider"
package = "{provider_directory}/plugin.toml"
scope = "root"

[[instances]]
id = "consumer"
package = "consumer/plugin.toml"
scope = "app"
"#
        ),
    )?;
    Ok(())
}

fn write_plugin_origin_manifest(
    path: &Path,
    installed_manifest: &Path,
    installed_lock: &Path,
) -> Result<()> {
    fs::write(
        path,
        format!(
            r#"format_version = 0

[composition]
id = "plugin-origin-process-fixed"
mode = "development"

[[scopes]]
id = "root"

[[instances]]
id = "watcher"
package = "watcher/plugin.toml"
scope = "root"
config = {{ hash_contents = true }}

[[instances]]
id = "hmr"
package = "hmr/plugin.toml"
scope = "root"
config = {{ manifest_path = "{}", lock_path = "{}", watch_request_id = "daemon-plugin-origin" }}
bindings = {{ "fs.watch" = "watcher" }}
"#,
            installed_manifest.display(),
            installed_lock.display(),
        ),
    )?;
    Ok(())
}

fn write_echo_manifest(path: &Path) -> Result<()> {
    fs::write(
        path,
        r#"format_version = 0

[composition]
id = "stream-conformance"
mode = "development"

[[scopes]]
id = "root"

[[scopes]]
id = "app"
parent = "root"

[[instances]]
id = "provider"
package = "provider/plugin.toml"
scope = "root"

[[instances]]
id = "consumer"
package = "consumer/plugin.toml"
scope = "app"
"#,
    )?;
    Ok(())
}

fn write_manifest(path: &Path, marker: &str) -> Result<()> {
    fs::write(
        path,
        format!(
            "format_version = 0\nscopes = []\ninstances = []\n\n[composition]\nid = \"daemon-conformance\"\nmode = \"development\"\n\n# {marker}\n"
        ),
    )
    .with_context(|| format!("write fixture manifest {}", path.display()))
}

async fn resolve_lock(state_dir: &Path, manifest: &Path, lock: &Path) -> Result<()> {
    let output = timeout(
        IO_DEADLINE,
        ProcessCommand::new(binary())
            .arg("--state-dir")
            .arg(state_dir)
            .arg("lock")
            .arg(manifest)
            .arg("--lock")
            .arg(lock)
            .output(),
    )
    .await
    .context("offline lock command timed out")??;
    ensure!(
        output.status.success(),
        "offline lock failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    Ok(())
}

async fn install_candidate(
    state_dir: &Path,
    manifest: &Path,
    lock: &Path,
    operation_id: &str,
) -> Result<()> {
    let output = timeout(
        IO_DEADLINE,
        ProcessCommand::new(binary())
            .arg("--state-dir")
            .arg(state_dir)
            .arg("install")
            .arg(manifest)
            .arg("--lock")
            .arg(lock)
            .arg("--operation-id")
            .arg(operation_id)
            .output(),
    )
    .await
    .context("offline install command timed out")??;
    ensure!(
        output.status.success(),
        "offline install failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    Ok(())
}

fn binary() -> &'static str {
    env!("CARGO_BIN_EXE_rsi-meta")
}

#[derive(Clone, Debug, Deserialize)]
struct Ready {
    status: String,
    socket: PathBuf,
    http: SocketAddr,
    token_file: PathBuf,
}

#[derive(Debug)]
struct Daemon {
    child: Child,
    ready: Ready,
    _stdout: BufReader<ChildStdout>,
    stderr: ChildStderr,
    stopped: bool,
}

impl Daemon {
    async fn start(fixture: &Fixture) -> Result<Self> {
        let mut child = daemon_command(fixture)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .context("spawn rsi-meta daemon")?;
        let stdout = child.stdout.take().context("capture daemon stdout")?;
        let stderr = child.stderr.take().context("capture daemon stderr")?;
        let mut stdout = BufReader::new(stdout);
        let mut line = String::new();
        let bytes = timeout(IO_DEADLINE, stdout.read_line(&mut line))
            .await
            .context("daemon readiness timed out")??;
        if bytes == 0 {
            let mut error = String::new();
            BufReader::new(stderr).read_to_string(&mut error).await?;
            bail!("daemon exited before readiness: {error}");
        }
        let ready: Ready = serde_json::from_str(&line).context("decode daemon readiness")?;
        ensure!(ready.status == "ready", "unexpected daemon status");
        Ok(Self {
            child,
            ready,
            _stdout: stdout,
            stderr,
            stopped: false,
        })
    }

    async fn stop(mut self) -> Result<()> {
        let raw_pid = i32::try_from(self.child.id().context("daemon already exited")?)
            .context("daemon pid does not fit pid_t")?;
        let pid = rustix::process::Pid::from_raw(raw_pid).context("daemon pid was zero")?;
        rustix::process::kill_process(pid, rustix::process::Signal::INT)
            .context("send SIGINT to daemon")?;
        let status = timeout(IO_DEADLINE, self.child.wait())
            .await
            .context("daemon graceful shutdown timed out")??;
        self.stopped = true;
        ensure!(status.success(), "daemon exited unsuccessfully: {status}");
        Ok(())
    }

    async fn wait_for_exit(mut self) -> Result<ExitStatus> {
        let status = timeout(IO_DEADLINE, self.child.wait())
            .await
            .context("daemon exit timed out")??;
        if !status.success() {
            let mut stderr = String::new();
            self.stderr.read_to_string(&mut stderr).await?;
            if !stderr.is_empty() {
                eprintln!("daemon stderr:\n{stderr}");
            }
        }
        self.stopped = true;
        Ok(status)
    }
}

impl Drop for Daemon {
    fn drop(&mut self) {
        if !self.stopped {
            let _ = self.child.start_kill();
        }
    }
}

fn daemon_command(fixture: &Fixture) -> ProcessCommand {
    let mut command = ProcessCommand::new(binary());
    command
        .arg("--state-dir")
        .arg(&fixture.state_dir)
        .arg("daemon")
        .arg("serve")
        .arg("--http-bind")
        .arg("127.0.0.1:0")
        .arg("--allow-origin")
        .arg(TRUSTED_ORIGIN)
        .env("RUST_LOG", "error");
    command
}

async fn open_unix(socket: &Path) -> Result<UnixControl> {
    let stream = timeout(IO_DEADLINE, UnixStream::connect(socket))
        .await
        .context("Unix connect timed out")??;
    Ok(Framed::new(
        stream,
        LinesCodec::new_with_max_length(MAX_CONTROL_BYTES),
    ))
}

async fn send_on(
    framed: &mut UnixControl,
    command: &CommandEnvelope,
) -> Result<CommandOutcomeEnvelope> {
    framed
        .send(serde_json::to_string(command)?)
        .await
        .context("send Unix command")?;
    let line = timeout(IO_DEADLINE, framed.next())
        .await
        .context("Unix response timed out")?
        .context("daemon closed before responding")??;
    serde_json::from_str(&line).context("decode Unix outcome")
}

async fn send_unix(socket: &Path, command: &CommandEnvelope) -> Result<CommandOutcomeEnvelope> {
    let mut framed = open_unix(socket).await?;
    send_on(&mut framed, command).await
}

fn count_durable_outcomes(database: &Path, command_id: &str) -> Result<u64> {
    let connection = Connection::open_with_flags(
        database,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )?;
    let count: i64 = connection.query_row(
        "SELECT COUNT(*) FROM command_outcome WHERE command_id = ?1",
        [command_id],
        |row| row.get(0),
    )?;
    u64::try_from(count).context("negative durable outcome count")
}

fn count_control_events(database: &Path, event_type: &str) -> Result<u64> {
    let connection = Connection::open_with_flags(
        database,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )?;
    let count: i64 = connection.query_row(
        "SELECT COUNT(*) FROM control_event WHERE json_extract(event_json, '$.payload.type') = ?1",
        [event_type],
        |row| row.get(0),
    )?;
    u64::try_from(count).context("negative control event count")
}

fn count_restart_events_from(database: &Path, source: &str) -> Result<u64> {
    count_control_events_from(database, "daemon_restarting", source)
}

fn count_control_events_from(database: &Path, event_type: &str, source: &str) -> Result<u64> {
    let connection = Connection::open_with_flags(
        database,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )?;
    let count: i64 = connection.query_row(
        "SELECT COUNT(*) FROM control_event
         WHERE json_extract(event_json, '$.payload.type') = ?1
           AND json_extract(event_json, '$.payload.source') = ?2",
        [event_type, source],
        |row| row.get(0),
    )?;
    u64::try_from(count).context("negative source-specific restart event count")
}

fn durable_outcome_summaries(database: &Path) -> Result<Vec<(String, String)>> {
    let connection = Connection::open_with_flags(
        database,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )?;
    let mut statement = connection.prepare(
        "SELECT command_id, COALESCE(outcome_json, status)
         FROM command_outcome ORDER BY rowid DESC LIMIT 8",
    )?;
    let rows = statement.query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?;
    rows.collect::<std::result::Result<Vec<_>, _>>()
        .map_err(Into::into)
}

async fn await_plugin_origin_readiness(database: &Path, manifest: &Path) -> Result<()> {
    let manifest = manifest.to_path_buf();
    let mut readiness_bytes = fs::read(&manifest)?;
    readiness_bytes.extend_from_slice(b"\n# plugin-origin readiness gate\n");
    let gate = Arc::new(Barrier::new(2));
    let stop = CancellationToken::new();
    let writer_gate = gate.clone();
    let writer_stop = stop.clone();
    let writer = tokio::spawn(async move {
        writer_gate.wait().await;
        let mut retry = tokio::time::interval(Duration::from_millis(250));
        retry.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            tokio::select! {
                biased;
                () = writer_stop.cancelled() => return Ok::<(), anyhow::Error>(()),
                _ = retry.tick() => {
                    // Identical candidate bytes change the polling fingerprint
                    // without racing candidate lock construction.
                    fs::write(&manifest, &readiness_bytes)
                        .context("write plugin-origin readiness stimulus")?;
                }
            }
        }
    });
    gate.wait().await;
    let readiness = timeout(IO_DEADLINE, async {
        let mut durable_poll = tokio::time::interval(Duration::from_millis(50));
        durable_poll.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            durable_poll.tick().await;
            if count_control_events_from(database, "composition_committed", "plugin_apply")? != 0 {
                return Ok::<(), anyhow::Error>(());
            }
        }
    })
    .await;
    stop.cancel();
    writer
        .await
        .context("join plugin-origin readiness writer")??;
    readiness.context("plugin-origin HMR readiness gate timed out")?
}

fn durable_token_generation(database: &Path) -> Result<u64> {
    let connection = Connection::open_with_flags(
        database,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )?;
    let generation: String = connection.query_row(
        "SELECT value FROM store_meta WHERE key = 'token_generation'",
        [],
        |row| row.get(0),
    )?;
    generation
        .parse()
        .context("decode durable token generation")
}

#[derive(Debug, Deserialize)]
struct TokenEnvelope {
    format_version: u32,
    generation: u64,
    token: String,
}

fn read_token(path: &Path) -> Result<TokenEnvelope> {
    serde_json::from_slice(&fs::read(path)?).context("decode token envelope")
}

fn websocket_request(
    daemon: &Daemon,
    after: u64,
    bearer: Option<&str>,
    origins: &[&str],
    duplicate_bearer: bool,
) -> Result<Request<()>> {
    let mut request = format!("ws://{}/ws?after={after}", daemon.ready.http)
        .into_client_request()
        .context("build WebSocket request")?;
    if let Some(token) = bearer {
        request.headers_mut().insert(
            header::AUTHORIZATION,
            HeaderValue::from_str(&format!("Bearer {token}"))?,
        );
    }
    if duplicate_bearer {
        request.headers_mut().append(
            header::AUTHORIZATION,
            HeaderValue::from_static("Bearer duplicate"),
        );
    }
    for origin in origins {
        request
            .headers_mut()
            .append(header::ORIGIN, HeaderValue::from_str(origin)?);
    }
    Ok(request)
}

async fn connect_websocket(request: Request<()>) -> Result<ClientWebSocket> {
    let (socket, response) = timeout(IO_DEADLINE, connect_async(request))
        .await
        .context("WebSocket handshake timed out")??;
    ensure!(
        response.status() == StatusCode::SWITCHING_PROTOCOLS,
        "unexpected WebSocket status {}",
        response.status()
    );
    Ok(socket)
}

async fn expect_handshake_status(request: Request<()>, expected: StatusCode) -> Result<()> {
    match timeout(IO_DEADLINE, connect_async(request))
        .await
        .context("rejected WebSocket handshake timed out")?
    {
        Err(WebSocketError::Http(response)) => {
            ensure!(
                response.status() == expected,
                "expected HTTP {expected}, received {}",
                response.status()
            );
            Ok(())
        }
        Ok((mut socket, response)) => {
            let _ = socket.close(None).await;
            bail!(
                "expected HTTP {expected}, WebSocket upgraded with {}",
                response.status()
            )
        }
        Err(error) => Err(error).context("unexpected WebSocket rejection"),
    }
}

async fn next_event(socket: &mut ClientWebSocket) -> Result<EventEnvelope> {
    loop {
        let message = timeout(IO_DEADLINE, socket.next())
            .await
            .context("WebSocket event timed out")?
            .context("WebSocket closed before an event")??;
        match message {
            Message::Text(text) => {
                return serde_json::from_str(text.as_str()).context("decode WebSocket event");
            }
            Message::Ping(bytes) => socket.send(Message::Pong(bytes)).await?,
            Message::Close(frame) => bail!("WebSocket closed before event: {frame:?}"),
            Message::Binary(_) | Message::Pong(_) | Message::Frame(_) => {}
        }
    }
}

async fn expect_close_code(socket: &mut ClientWebSocket, expected: CloseCode) -> Result<()> {
    loop {
        let message = timeout(IO_DEADLINE, socket.next())
            .await
            .context("WebSocket close timed out")?
            .context("WebSocket ended without a close frame")??;
        match message {
            Message::Close(Some(frame)) => {
                ensure!(
                    frame.code == expected,
                    "expected close {expected:?}, received {frame:?}"
                );
                return Ok(());
            }
            Message::Close(None) => bail!("WebSocket close frame had no status code"),
            Message::Ping(bytes) => socket.send(Message::Pong(bytes)).await?,
            Message::Text(_) | Message::Binary(_) | Message::Pong(_) | Message::Frame(_) => {}
        }
    }
}

fn open_stream_frame(stream_id: &str) -> Result<StreamEnvelope> {
    let mut frame = StreamEnvelope::new(stream_id, StreamKind::Open);
    frame.payload = Some(serde_json::to_value(ServiceOpenRequest {
        consumer: rsi_meta::InstanceId::new("consumer"),
        service: ServiceKey::new("fixture.echo"),
    })?);
    Ok(frame)
}

fn credit_frame(stream_id: &str, bytes: u64) -> StreamEnvelope {
    let mut frame = StreamEnvelope::new(stream_id, StreamKind::Credit);
    frame.credit_bytes = Some(bytes);
    frame
}

fn data_frame(stream_id: &str, sequence: u64, bytes: &[u8]) -> StreamEnvelope {
    let mut frame = StreamEnvelope::new(stream_id, StreamKind::Data);
    frame.sequence = Some(sequence);
    frame.payload = Some(serde_json::json!(bytes));
    frame
}

fn half_close_frame(stream_id: &str) -> StreamEnvelope {
    StreamEnvelope::new(stream_id, StreamKind::HalfClose)
}

fn cancel_frame(stream_id: &str, reason: &str) -> StreamEnvelope {
    let mut frame = StreamEnvelope::new(stream_id, StreamKind::Cancel);
    frame.payload = Some(serde_json::json!({"reason": reason}));
    frame
}

async fn send_ws_envelope(
    socket: &mut ClientWebSocket,
    envelope: &impl serde::Serialize,
) -> Result<()> {
    socket
        .send(Message::Text(serde_json::to_string(envelope)?.into()))
        .await
        .context("send WebSocket envelope")
}

async fn next_stream_ws(socket: &mut ClientWebSocket) -> Result<StreamEnvelope> {
    loop {
        let message = timeout(IO_DEADLINE, socket.next())
            .await
            .context("WebSocket stream frame timed out")?
            .context("WebSocket closed before a stream frame")??;
        match message {
            Message::Text(text) => {
                let value: serde_json::Value = serde_json::from_str(text.as_str())?;
                if value.get("protocol").and_then(serde_json::Value::as_str)
                    == Some(STREAM_PROTOCOL)
                {
                    return serde_json::from_value(value).context("decode WebSocket stream frame");
                }
            }
            Message::Ping(bytes) => socket.send(Message::Pong(bytes)).await?,
            Message::Close(frame) => bail!("WebSocket closed before stream frame: {frame:?}"),
            Message::Binary(_) | Message::Pong(_) | Message::Frame(_) => {}
        }
    }
}

async fn next_outcome_ws(socket: &mut ClientWebSocket) -> Result<CommandOutcomeEnvelope> {
    loop {
        let message = timeout(IO_DEADLINE, socket.next())
            .await
            .context("WebSocket command outcome timed out")?
            .context("WebSocket closed before a command outcome")??;
        match message {
            Message::Text(text) => {
                if let Ok(outcome) = serde_json::from_str(text.as_str()) {
                    return Ok(outcome);
                }
            }
            Message::Ping(bytes) => socket.send(Message::Pong(bytes)).await?,
            Message::Close(frame) => bail!("WebSocket closed before outcome: {frame:?}"),
            Message::Binary(_) | Message::Pong(_) | Message::Frame(_) => {}
        }
    }
}

async fn send_stream_unix(connection: &mut UnixControl, frame: &StreamEnvelope) -> Result<()> {
    connection
        .send(serde_json::to_string(frame)?)
        .await
        .context("send Unix stream frame")
}

async fn next_stream_unix(connection: &mut UnixControl) -> Result<StreamEnvelope> {
    let line = timeout(IO_DEADLINE, connection.next())
        .await
        .context("Unix stream frame timed out")?
        .context("Unix socket closed before a stream frame")??;
    serde_json::from_str(&line).context("decode Unix stream frame")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sigterm_runs_the_graceful_daemon_shutdown_path() -> Result<()> {
    let fixture = Fixture::new().await?;
    let mut daemon = Daemon::start(&fixture).await?;
    let socket = daemon.ready.socket.clone();
    let raw_pid = i32::try_from(daemon.child.id().context("daemon already exited")?)
        .context("daemon pid does not fit pid_t")?;
    let pid = rustix::process::Pid::from_raw(raw_pid).context("daemon pid was zero")?;

    rustix::process::kill_process(pid, rustix::process::Signal::TERM)
        .context("send SIGTERM to daemon")?;
    let status = timeout(IO_DEADLINE, daemon.child.wait())
        .await
        .context("SIGTERM shutdown timed out")??;
    daemon.stopped = true;

    ensure!(
        status.success(),
        "SIGTERM bypassed graceful shutdown: {status}"
    );
    ensure!(
        !socket.exists(),
        "graceful shutdown left the Unix socket behind"
    );
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn read_correlation_ids_are_connection_local_and_never_durable() -> Result<()> {
    let fixture = Fixture::new().await?;
    let daemon = Daemon::start(&fixture).await?;
    let command_id = "concurrent-idempotency";
    let command = CommandEnvelope::new(command_id, CoreCommand::QueryGraph);
    let first = open_unix(&daemon.ready.socket).await?;
    let second = open_unix(&daemon.ready.socket).await?;
    let gate = Arc::new(Barrier::new(3));

    let left_gate = gate.clone();
    let left_command = command.clone();
    let left = tokio::spawn(async move {
        let mut connection = first;
        left_gate.wait().await;
        send_on(&mut connection, &left_command).await
    });
    let right_gate = gate.clone();
    let right_command = command.clone();
    let right = tokio::spawn(async move {
        let mut connection = second;
        right_gate.wait().await;
        send_on(&mut connection, &right_command).await
    });
    gate.wait().await;
    let (left, right) = tokio::try_join!(left, right)?;
    let left = left?;
    let right = right?;

    ensure!(left == right, "idempotent outcomes differed");
    ensure!(
        matches!(left.payload, CommandOutcome::Graph { .. }),
        "unexpected outcome: {:?}",
        left.payload
    );
    ensure!(
        count_durable_outcomes(&fixture.state_dir.join("state.sqlite3"), command_id)? == 0,
        "read correlation ids must not enter the durable operation journal"
    );

    let conflicting = CommandEnvelope::new(
        command_id,
        CoreCommand::QueryEvents {
            after_cursor: 0,
            limit: 1,
        },
    );
    let result = send_unix(&daemon.ready.socket, &conflicting).await?;
    ensure!(matches!(result.payload, CommandOutcome::Events { .. }));
    ensure!(
        count_durable_outcomes(&fixture.state_dir.join("state.sqlite3"), command_id)? == 0,
        "completed read id reuse must remain non-durable"
    );

    daemon.stop().await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn offline_install_is_rejected_while_another_process_owns_the_workspace() -> Result<()> {
    let fixture = Fixture::new().await?;
    let daemon = Daemon::start(&fixture).await?;
    let output = timeout(
        IO_DEADLINE,
        ProcessCommand::new(binary())
            .arg("--state-dir")
            .arg(&fixture.state_dir)
            .arg("install")
            .arg(&fixture.manifest_b)
            .arg("--lock")
            .arg(&fixture.lock_b)
            .arg("--operation-id")
            .arg("install-while-live")
            .output(),
    )
    .await
    .context("live-workspace install timed out")??;
    ensure!(!output.status.success(), "live-workspace install succeeded");
    ensure!(
        String::from_utf8_lossy(&output.stderr).contains("workspace_busy"),
        "install did not report workspace_busy: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    daemon.stop().await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn process_fixed_preflight_replays_then_external_install_activates_once() -> Result<()> {
    let fixture = Fixture::new_process_fixed().await?;
    let daemon = Daemon::start(&fixture).await?;
    let database = fixture.state_dir.join("state.sqlite3");
    let command_id = "process-fixed-ack-loss";
    let command = CommandEnvelope::new(
        command_id,
        CoreCommand::ApplyManifestPath {
            manifest_path: fixture.manifest_b.clone(),
            lock_path: fixture.lock_b.clone(),
        },
    );

    let mut lost_ack = timeout(IO_DEADLINE, UnixStream::connect(&daemon.ready.socket))
        .await
        .context("ack-loss Unix connect timed out")??;
    lost_ack
        .write_all(serde_json::to_string(&command)?.as_bytes())
        .await?;
    lost_ack.write_all(b"\n").await?;
    lost_ack.shutdown().await?;

    timeout(IO_DEADLINE, async {
        loop {
            if count_durable_outcomes(&database, command_id)? == 1 {
                return Ok::<(), anyhow::Error>(());
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .context("process-fixed result was not persisted")??;
    // Keep the read half alive until the daemon has accepted and durably
    // completed the request. Dropping the whole socket immediately after the
    // write can discard the request before the server reads it on some Unix
    // implementations; not reading the response still models a lost ack.
    drop(lost_ack);

    let first = send_unix(&daemon.ready.socket, &command).await?;
    ensure!(matches!(
        first.payload,
        CommandOutcome::RestartRequired { .. }
    ));
    ensure!(
        count_control_events(&database, "daemon_restarting")? == 0,
        "preflight must not emit a restart event"
    );
    ensure!(
        fs::read_to_string(fixture.state_dir.join("composition.toml"))?
            .contains("provider-a/plugin.toml"),
        "preflight changed the installed manifest"
    );

    let stopped = send_unix(
        &daemon.ready.socket,
        &CommandEnvelope::new("process-fixed-stop", CoreCommand::Shutdown),
    )
    .await?;
    ensure!(matches!(stopped.payload, CommandOutcome::ShuttingDown));
    ensure!(daemon.wait_for_exit().await?.success());

    install_candidate(
        &fixture.state_dir,
        &fixture.manifest_b,
        &fixture.lock_b,
        "process-fixed-install-v2",
    )
    .await?;
    fs::remove_file(&fixture.manifest_b)?;
    fs::remove_file(&fixture.lock_b)?;

    let restarted = Daemon::start(&fixture).await?;
    let graph = send_unix(
        &restarted.ready.socket,
        &CommandEnvelope::new("graph-after-offline-install", CoreCommand::QueryGraph),
    )
    .await?;
    let CommandOutcome::Graph { graph, .. } = graph.payload else {
        bail!("fresh daemon did not return its graph")
    };
    ensure!(
        graph.instances.values().any(|instance| instance
            .package
            .manifest_path
            .to_string_lossy()
            .contains("provider-b")),
        "fresh daemon did not activate the offline-installed candidate"
    );
    let replay = send_unix(&restarted.ready.socket, &command).await?;
    ensure!(
        replay.payload == first.payload,
        "durable preflight result changed after source deletion"
    );
    ensure!(
        count_control_events(&database, "composition_committed")? == 2,
        "offline install must activate exactly once on the next open"
    );
    restarted.stop().await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn cli_process_fixed_apply_exits_75_without_stopping_the_daemon() -> Result<()> {
    let fixture = Fixture::new_process_fixed().await?;
    let mut daemon = Daemon::start(&fixture).await?;
    let before = send_unix(
        &daemon.ready.socket,
        &CommandEnvelope::new("graph-before-process-fixed", CoreCommand::QueryGraph),
    )
    .await?;

    let output = timeout(
        IO_DEADLINE,
        ProcessCommand::new(binary())
            .arg("--state-dir")
            .arg(&fixture.state_dir)
            .arg("apply")
            .arg(&fixture.manifest_b)
            .arg("--lock")
            .arg(&fixture.lock_b)
            .arg("--operation-id")
            .arg("manual-process-fixed-ack-order")
            .output(),
    )
    .await
    .context("process-fixed CLI apply timed out")??;
    ensure!(
        output.status.code() == Some(i32::from(rsi_meta_cli::DAEMON_RESTART_EXIT_CODE)),
        "process-fixed CLI apply must exit 75: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    ensure!(
        daemon.child.try_wait()?.is_none(),
        "RestartRequired stopped the live daemon"
    );

    let after = send_unix(
        &daemon.ready.socket,
        &CommandEnvelope::new("graph-after-process-fixed", CoreCommand::QueryGraph),
    )
    .await?;
    ensure!(after.graph_revision == before.graph_revision);
    ensure!(
        count_control_events(
            &fixture.state_dir.join("state.sqlite3"),
            "composition_committed",
        )? == 1,
        "process-fixed preflight emitted a graph event"
    );
    daemon.stop().await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn plugin_origin_process_fixed_hmr_requires_external_install() -> Result<()> {
    let fixture = PluginOriginFixture::new().await?;
    let mut daemon = Daemon::start(&fixture.daemon).await?;
    let database = fixture.daemon.state_dir.join("state.sqlite3");
    let installed_manifest = fixture.daemon.state_dir.join("composition.toml");

    await_plugin_origin_readiness(&database, &installed_manifest).await?;
    ensure!(
        count_restart_events_from(&database, "plugin_apply")? == 0,
        "ordinary plugin-origin HMR requested a daemon restart"
    );

    let mut bytes = fs::read(&fixture.hmr_manifest)?;
    bytes.extend_from_slice(b"\n# plugin-origin process-fixed trigger\n");
    fs::write(&fixture.hmr_manifest, bytes)?;

    timeout(IO_DEADLINE, async {
        loop {
            if durable_outcome_summaries(&database)?
                .iter()
                .any(|(_, outcome)| outcome.contains("process_fixed_requires_external_install"))
            {
                return Ok::<(), anyhow::Error>(());
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .context("plugin-origin process-fixed rejection was not persisted")??;

    ensure!(
        daemon.child.try_wait()?.is_none(),
        "plugin-origin process-fixed rejection stopped the daemon"
    );
    ensure!(count_restart_events_from(&database, "plugin_apply")? == 0);
    let graph = send_unix(
        &daemon.ready.socket,
        &CommandEnvelope::new("plugin-origin-still-live", CoreCommand::QueryGraph),
    )
    .await?;
    ensure!(matches!(graph.payload, CommandOutcome::Graph { .. }));
    daemon.stop().await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn snapshot_cursor_and_websocket_after_span_replay_live_without_a_gap() -> Result<()> {
    let fixture = Fixture::new().await?;
    let daemon = Daemon::start(&fixture).await?;

    let graph = send_unix(
        &daemon.ready.socket,
        &CommandEnvelope::new("initial-graph", CoreCommand::QueryGraph),
    )
    .await?;
    let CommandOutcome::Graph {
        graph: initial_graph,
        cursor,
    } = graph.payload
    else {
        bail!("initial snapshot did not return a graph")
    };
    let stale_query = CommandEnvelope::new("stale-read-only-query", CoreCommand::QueryGraph)
        .with_expected_revision(GraphRevision(initial_graph.revision.0.saturating_add(1)));
    let rejected = send_unix(&daemon.ready.socket, &stale_query).await?;
    ensure!(
        matches!(
            &rejected.payload,
            CommandOutcome::Rejected { code, .. } if code == "invalid_command"
        ),
        "expected_graph_revision was not enforced on a read-only command: {:?}",
        rejected.payload
    );

    let token = read_token(&daemon.ready.token_file)?.token;
    let request = websocket_request(&daemon, cursor, Some(&token), &[], false)?;
    let apply = CommandEnvelope::new(
        "apply-b-at-replay-live-seam",
        CoreCommand::ApplyManifestPath {
            manifest_path: fixture.manifest_b.clone(),
            lock_path: fixture.lock_b.clone(),
        },
    );
    let gate = Arc::new(Barrier::new(3));
    let websocket_gate = gate.clone();
    let websocket = tokio::spawn(async move {
        websocket_gate.wait().await;
        connect_websocket(request).await
    });
    let apply_gate = gate.clone();
    let socket = daemon.ready.socket.clone();
    let apply_task = tokio::spawn(async move {
        apply_gate.wait().await;
        send_unix(&socket, &apply).await
    });
    gate.wait().await;
    let (websocket, applied) = tokio::try_join!(websocket, apply_task)?;
    let mut websocket = websocket?;
    let applied = applied?;
    ensure!(
        matches!(applied.payload, CommandOutcome::Applied { .. }),
        "apply did not commit: {:?}",
        applied.payload
    );
    let committed = next_event(&mut websocket).await?;
    ensure!(committed.cursor == cursor + 1, "event cursor gap");
    ensure!(committed.graph_revision == applied.graph_revision);

    // Abruptly drop the client after persisting its last observed cursor. The
    // next event must be replayable from that cursor on a new connection.
    drop(websocket);
    let applied_a = send_unix(
        &daemon.ready.socket,
        &CommandEnvelope::new(
            "apply-c-while-client-disconnected",
            CoreCommand::ApplyManifestPath {
                manifest_path: fixture.manifest_c.clone(),
                lock_path: fixture.lock_c.clone(),
            },
        ),
    )
    .await?;
    ensure!(matches!(applied_a.payload, CommandOutcome::Applied { .. }));

    let zero_limit = send_unix(
        &daemon.ready.socket,
        &CommandEnvelope::new(
            "zero-event-limit",
            CoreCommand::QueryEvents {
                after_cursor: 0,
                limit: 0,
            },
        ),
    )
    .await?;
    ensure!(
        matches!(&zero_limit.payload, CommandOutcome::Events { events } if events.len() == 1),
        "query_events limit=0 was not clamped to one event: {:?}",
        zero_limit.payload
    );

    let request = websocket_request(&daemon, committed.cursor, Some(&token), &[], false)?;
    let mut resumed = connect_websocket(request).await?;
    let replayed = next_event(&mut resumed).await?;
    ensure!(replayed.cursor == committed.cursor + 1, "resume cursor gap");
    ensure!(replayed.graph_revision == applied_a.graph_revision);
    resumed.close(None).await?;

    daemon.stop().await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn real_echo_streams_route_over_websocket_and_unix_with_connection_binding() -> Result<()> {
    let fixture = Fixture::new_with_echo().await?;
    let daemon = Daemon::start(&fixture).await?;
    let token = read_token(&daemon.ready.token_file)?.token;

    let mut websocket = connect_websocket(websocket_request(
        &daemon,
        u64::MAX,
        Some(&token),
        &[],
        false,
    )?)
    .await?;
    send_ws_envelope(&mut websocket, &open_stream_frame("ws-echo")?).await?;
    let opened = next_stream_ws(&mut websocket).await?;
    ensure!(opened.kind == StreamKind::Open);
    ensure!(opened.stream_id == "ws-echo");
    ensure!(opened.payload == Some(serde_json::json!({"provider": "provider"})));
    let input_credit = next_stream_ws(&mut websocket).await?;
    ensure!(input_credit.kind == StreamKind::Credit);
    ensure!(input_credit.credit_bytes == Some(1024 * 1024));

    send_ws_envelope(&mut websocket, &credit_frame("ws-echo", 1024)).await?;
    send_ws_envelope(&mut websocket, &data_frame("ws-echo", 1, b"first")).await?;
    send_ws_envelope(&mut websocket, &data_frame("ws-echo", 2, b"second")).await?;
    let first = next_stream_ws(&mut websocket).await?;
    let second = next_stream_ws(&mut websocket).await?;
    ensure!(first.kind == StreamKind::Data && first.sequence == Some(1));
    ensure!(first.payload == Some(serde_json::json!(b"first")));
    ensure!(second.kind == StreamKind::Data && second.sequence == Some(2));
    ensure!(second.payload == Some(serde_json::json!(b"second")));

    send_ws_envelope(&mut websocket, &half_close_frame("ws-echo")).await?;
    let ended = next_stream_ws(&mut websocket).await?;
    ensure!(
        ended.kind == StreamKind::End,
        "unexpected echo terminal frame: {ended:?}"
    );
    ensure!(ended.stream_id == "ws-echo");
    websocket.close(None).await?;

    let mut abandoned = open_unix(&daemon.ready.socket).await?;
    send_stream_unix(&mut abandoned, &open_stream_frame("connection-local")?).await?;
    ensure!(next_stream_unix(&mut abandoned).await?.kind == StreamKind::Open);
    ensure!(next_stream_unix(&mut abandoned).await?.kind == StreamKind::Credit);
    drop(abandoned);

    // The same external ID may be reused on a new connection. Dropping the old
    // transport drops its generation-pinned ServiceStream, whose core Drop
    // contract sends `client_disconnected` cancellation.
    let mut resumed = open_unix(&daemon.ready.socket).await?;
    send_stream_unix(&mut resumed, &open_stream_frame("connection-local")?).await?;
    ensure!(next_stream_unix(&mut resumed).await?.kind == StreamKind::Open);
    ensure!(next_stream_unix(&mut resumed).await?.kind == StreamKind::Credit);
    send_stream_unix(
        &mut resumed,
        &cancel_frame("connection-local", "test_complete"),
    )
    .await?;
    let cancelled = next_stream_unix(&mut resumed).await?;
    ensure!(cancelled.kind == StreamKind::Cancel);
    ensure!(cancelled.payload == Some(serde_json::json!({"reason": "test_complete"})));

    daemon.stop().await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn authenticated_websocket_rotation_acks_then_disconnects_all_sessions() -> Result<()> {
    let fixture = Fixture::new().await?;
    let daemon = Daemon::start(&fixture).await?;
    let original = read_token(&daemon.ready.token_file)?;
    let request = websocket_request(&daemon, u64::MAX, Some(&original.token), &[], false)?;
    let mut initiator = connect_websocket(request.clone()).await?;
    let mut observer = connect_websocket(request).await?;

    let command = CommandEnvelope::new("websocket-token-rotation", CoreCommand::RotateToken);
    send_ws_envelope(&mut initiator, &command).await?;
    let outcome = next_outcome_ws(&mut initiator).await?;
    ensure!(outcome.command_id == command.command_id);
    ensure!(outcome.payload == (CommandOutcome::TokenRotated { generation: 1 }));
    expect_close_code(&mut initiator, CloseCode::Policy).await?;
    expect_close_code(&mut observer, CloseCode::Policy).await?;

    let current = read_token(&daemon.ready.token_file)?;
    ensure!(current.generation == 1);
    ensure!(current.token != original.token);
    expect_handshake_status(
        websocket_request(&daemon, u64::MAX, Some(&original.token), &[], false)?,
        StatusCode::UNAUTHORIZED,
    )
    .await?;
    let mut authenticated = connect_websocket(websocket_request(
        &daemon,
        u64::MAX,
        Some(&current.token),
        &[],
        false,
    )?)
    .await?;
    authenticated.close(None).await?;

    daemon.stop().await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[allow(clippy::too_many_lines)]
async fn daemon_enforces_control_limits_and_the_bearer_origin_matrix() -> Result<()> {
    let fixture = Fixture::new().await?;
    let daemon = Daemon::start(&fixture).await?;
    let token_envelope = read_token(&daemon.ready.token_file)?;
    ensure!(token_envelope.format_version == 0);
    ensure!(token_envelope.generation == 0);
    let token = token_envelope.token;

    expect_handshake_status(
        websocket_request(&daemon, 0, None, &[], false)?,
        StatusCode::UNAUTHORIZED,
    )
    .await?;
    expect_handshake_status(
        websocket_request(&daemon, 0, Some("wrong"), &[], false)?,
        StatusCode::UNAUTHORIZED,
    )
    .await?;
    expect_handshake_status(
        websocket_request(
            &daemon,
            0,
            Some(&token),
            &["https://attacker.invalid"],
            false,
        )?,
        StatusCode::FORBIDDEN,
    )
    .await?;
    expect_handshake_status(
        websocket_request(&daemon, 0, Some(&token), &[], true)?,
        StatusCode::UNAUTHORIZED,
    )
    .await?;
    expect_handshake_status(
        websocket_request(
            &daemon,
            0,
            Some(&token),
            &[TRUSTED_ORIGIN, TRUSTED_ORIGIN],
            false,
        )?,
        StatusCode::FORBIDDEN,
    )
    .await?;

    let mut no_origin = connect_websocket(websocket_request(
        &daemon,
        u64::MAX,
        Some(&token),
        &[],
        false,
    )?)
    .await?;
    no_origin.close(None).await?;
    let builtin_origin = format!("http://localhost:{}", daemon.ready.http.port());
    let mut builtin = connect_websocket(websocket_request(
        &daemon,
        u64::MAX,
        Some(&token),
        &[&builtin_origin],
        false,
    )?)
    .await?;
    builtin.close(None).await?;
    let mut explicitly_allowed = connect_websocket(websocket_request(
        &daemon,
        u64::MAX,
        Some(&token),
        &[TRUSTED_ORIGIN],
        false,
    )?)
    .await?;
    explicitly_allowed.close(None).await?;

    let mut oversized_unix = timeout(IO_DEADLINE, UnixStream::connect(&daemon.ready.socket))
        .await
        .context("oversized Unix client connect timed out")??;
    let oversized_line = vec![b'x'; MAX_CONTROL_BYTES + 1];
    let write = oversized_unix.write_all(&oversized_line).await;
    if write.is_ok() {
        let _ = oversized_unix.write_all(b"\n").await;
        let _ = oversized_unix.shutdown().await;
        let mut byte = [0_u8; 1];
        match timeout(IO_DEADLINE, oversized_unix.read(&mut byte))
            .await
            .context("oversized Unix close timed out")?
        {
            Ok(0) => {}
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::ConnectionReset | std::io::ErrorKind::BrokenPipe
                ) => {}
            Ok(count) => bail!("oversized Unix frame received {count} response bytes"),
            Err(error) => return Err(error).context("read oversized Unix close"),
        }
    }

    let mut oversized_websocket = connect_websocket(websocket_request(
        &daemon,
        u64::MAX,
        Some(&token),
        &[],
        false,
    )?)
    .await?;
    let oversized_send = oversized_websocket
        .send(Message::Text("x".repeat(MAX_CONTROL_BYTES + 1).into()))
        .await;
    match oversized_send {
        Ok(()) => expect_close_code(&mut oversized_websocket, CloseCode::Size).await?,
        Err(WebSocketError::Io(error))
            if matches!(
                error.kind(),
                std::io::ErrorKind::BrokenPipe | std::io::ErrorKind::ConnectionReset
            ) => {}
        Err(error) => return Err(error).context("send oversized WebSocket message"),
    }

    daemon.stop().await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn non_loopback_bind_is_rejected_before_runtime_state_is_created() -> Result<()> {
    let temporary = tempfile::tempdir()?;
    let state_dir = temporary.path().join("state");
    ensure!(!state_dir.exists());
    let output = timeout(
        IO_DEADLINE,
        ProcessCommand::new(binary())
            .arg("--state-dir")
            .arg(&state_dir)
            .arg("daemon")
            .arg("serve")
            .arg("--http-bind")
            .arg("0.0.0.0:0")
            .output(),
    )
    .await
    .context("non-loopback daemon did not exit")??;
    ensure!(!output.status.success(), "non-loopback daemon started");
    ensure!(
        String::from_utf8_lossy(&output.stderr).contains("refusing non-loopback HTTP bind"),
        "unexpected daemon error: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    ensure!(!state_dir.exists(), "refused bind created runtime state");
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn lock_command_never_replaces_an_existing_candidate_path() -> Result<()> {
    let temporary = tempfile::tempdir()?;
    let manifest = temporary.path().join("rsi-meta.toml");
    let lock = temporary.path().join("candidate.lock");
    write_manifest(&manifest, "first-lock")?;
    resolve_lock(temporary.path(), &manifest, &lock).await?;
    let original = fs::read(&lock)?;

    fs::write(
        &manifest,
        "format_version = 0\nscopes = []\ninstances = []\n\n[composition]\nid = \"different-lock\"\n",
    )?;
    let output = timeout(
        IO_DEADLINE,
        ProcessCommand::new(binary())
            .arg("--state-dir")
            .arg(temporary.path())
            .arg("lock")
            .arg(&manifest)
            .arg("--lock")
            .arg(&lock)
            .output(),
    )
    .await
    .context("second lock command timed out")??;
    ensure!(!output.status.success(), "existing lock was replaced");
    ensure!(
        String::from_utf8_lossy(&output.stderr).contains("lock_conflict"),
        "lock rejection did not identify the create-new contract: stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
    ensure!(fs::read(&lock)? == original, "existing lock bytes changed");
    Ok(())
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct FileStamp {
    inode: u64,
    modified_seconds: i64,
    modified_nanoseconds: i64,
    length: u64,
}

fn file_stamp(path: &Path) -> Result<FileStamp> {
    let metadata = fs::metadata(path)?;
    Ok(FileStamp {
        inode: metadata.ino(),
        modified_seconds: metadata.mtime(),
        modified_nanoseconds: metadata.mtime_nsec(),
        length: metadata.len(),
    })
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn restart_replay_of_an_old_rotation_generation_does_not_rewrite_token() -> Result<()> {
    let fixture = Fixture::new().await?;
    let daemon = Daemon::start(&fixture).await?;
    let rotation = CommandEnvelope::new("durable-token-rotation", CoreCommand::RotateToken);
    let first = send_unix(&daemon.ready.socket, &rotation).await?;
    ensure!(
        first.payload == (CommandOutcome::TokenRotated { generation: 1 }),
        "unexpected first rotation: {:?}",
        first.payload
    );
    let token_path = daemon.ready.token_file.clone();
    let before_bytes = fs::read(&token_path)?;
    let before_stamp = file_stamp(&token_path)?;
    daemon.stop().await?;

    let restarted = Daemon::start(&fixture).await?;
    ensure!(
        fs::read(&token_path)? == before_bytes,
        "daemon restart rewrote token contents"
    );
    ensure!(
        file_stamp(&token_path)? == before_stamp,
        "daemon restart replaced the token file"
    );
    let replay = send_unix(&restarted.ready.socket, &rotation).await?;
    ensure!(replay == first, "durable rotation replay changed outcome");
    ensure!(
        fs::read(&token_path)? == before_bytes,
        "old generation replay rewrote token contents"
    );
    ensure!(
        file_stamp(&token_path)? == before_stamp,
        "old generation replay replaced the token file"
    );
    ensure!(
        count_durable_outcomes(
            &fixture.state_dir.join("state.sqlite3"),
            "durable-token-rotation"
        )? == 1,
        "rotation replay duplicated its durable outcome"
    );

    restarted.stop().await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn durable_token_generation_is_reconciled_before_remote_admission() -> Result<()> {
    let fixture = Fixture::new().await?;
    let daemon = Daemon::start(&fixture).await?;
    let old = read_token(&daemon.ready.token_file)?;
    ensure!(old.generation == 0);

    fs::set_permissions(&fixture.state_dir, fs::Permissions::from_mode(0o500))?;
    let rotation = send_unix(
        &daemon.ready.socket,
        &CommandEnvelope::new("crash-window-rotation", CoreCommand::RotateToken),
    )
    .await;
    ensure!(
        rotation.is_err(),
        "token publication failure returned an ack"
    );
    fs::set_permissions(&fixture.state_dir, fs::Permissions::from_mode(0o700))?;
    let status = daemon.wait_for_exit().await?;
    ensure!(
        status.code() == Some(i32::from(rsi_meta_cli::DAEMON_RESTART_EXIT_CODE)),
        "token publication failure did not request supervised restart: {status}"
    );
    ensure!(
        durable_token_generation(&fixture.state_dir.join("state.sqlite3"))? == 1,
        "core token generation was not committed"
    );
    let lagging = read_token(&fixture.state_dir.join("daemon.token"))?;
    ensure!(
        lagging.generation == 0,
        "failure unexpectedly rewrote token"
    );
    ensure!(lagging.token == old.token);

    let restarted = Daemon::start(&fixture).await?;
    let repaired = read_token(&restarted.ready.token_file)?;
    ensure!(repaired.generation == 1);
    ensure!(repaired.token != old.token);
    expect_handshake_status(
        websocket_request(&restarted, u64::MAX, Some(&old.token), &[], false)?,
        StatusCode::UNAUTHORIZED,
    )
    .await?;
    let mut current = connect_websocket(websocket_request(
        &restarted,
        u64::MAX,
        Some(&repaired.token),
        &[],
        false,
    )?)
    .await?;
    current.close(None).await?;

    restarted.stop().await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn token_file_ahead_of_core_refuses_daemon_startup() -> Result<()> {
    let fixture = Fixture::new().await?;
    let daemon = Daemon::start(&fixture).await?;
    let rotation = CommandEnvelope::new("ahead-file-rotation", CoreCommand::RotateToken);
    let outcome = send_unix(&daemon.ready.socket, &rotation).await?;
    ensure!(matches!(
        outcome.payload,
        CommandOutcome::TokenRotated { generation: 1 }
    ));
    let token_path = daemon.ready.token_file.clone();
    daemon.stop().await?;

    let mut envelope: serde_json::Value = serde_json::from_slice(&fs::read(&token_path)?)?;
    envelope["generation"] = serde_json::Value::from(2_u64);
    fs::write(&token_path, serde_json::to_vec(&envelope)?)?;
    let output = timeout(IO_DEADLINE, daemon_command(&fixture).output())
        .await
        .context("inconsistent-token daemon did not exit")??;
    ensure!(!output.status.success(), "ahead token file was accepted");
    ensure!(
        String::from_utf8_lossy(&output.stderr).contains("ahead of durable core generation"),
        "unexpected startup error: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    ensure!(
        !fixture.state_dir.join("daemon.sock").exists(),
        "failed startup left a Unix listener"
    );
    Ok(())
}
