#![deny(unsafe_code)]

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use anyhow::{Context, Result, bail, ensure};
use futures_util::{SinkExt, StreamExt};
use rsi_meta::{
    CompositionChangeSource, GraphSnapshot, InstanceId, InstanceStatus, ServiceKey,
    ServiceOpenRequest, StreamEnvelope, StreamKind,
};
use rsi_meta_cli::protocol::{
    Command, CommandEnvelope, CommandOutcome, CommandOutcomeEnvelope, Event, EventEnvelope,
};
use rsi_meta_loader::{ApiVersion, BUILD_TARGET};
use serde::Deserialize;
use serde_json::json;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, BufReader};
use tokio::net::{TcpStream, UnixListener, UnixStream};
use tokio::process::{Child, ChildStderr, ChildStdout, Command as ProcessCommand};
use tokio::time::timeout;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http::{HeaderValue, Request, header};
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream, connect_async};
use tokio_util::codec::{Framed, LinesCodec};

const IO_DEADLINE: Duration = Duration::from_secs(20);
const MAX_WIRE_BYTES: usize = 1024 * 1024;
const FAILPOINT_ENV: &str = "RSI_META_TEST_ACK_GATE";
const CRASH_COMMAND_ID: &str = "release-demo-commit-before-ack";

type UnixControl = Framed<UnixStream, LinesCodec>;
type ClientWebSocket = WebSocketStream<MaybeTlsStream<TcpStream>>;

#[derive(Debug)]
struct DemoFiles {
    _temporary: tempfile::TempDir,
    state: PathBuf,
    installed_manifest: PathBuf,
    installed_lock: PathBuf,
    provider_v1_manifest: PathBuf,
    provider_v1_lock: PathBuf,
    provider_v2_manifest: PathBuf,
    provider_v2_lock: PathBuf,
    crash_manifest: PathBuf,
    crash_lock: PathBuf,
    process_fixed_manifest: PathBuf,
    process_fixed_lock: PathBuf,
    failpoint_gate: PathBuf,
}

impl DemoFiles {
    fn create(artifact: &Path) -> Result<Self> {
        let temporary = tempfile::tempdir().context("create release demo directory")?;
        let root = temporary.path().to_path_buf();
        let state = root.join("empty-state");
        let installed_manifest = root.join("installed.toml");
        let installed_lock = root.join("installed.lock");
        let provider_v1_manifest = root.join("candidate-v1.toml");
        let provider_v1_lock = root.join("candidate-v1.lock");
        let provider_v2_manifest = root.join("candidate-v2.toml");
        let provider_v2_lock = root.join("candidate-v2.lock");
        let crash_manifest = root.join("candidate-crash.toml");
        let crash_lock = root.join("candidate-crash.lock");
        let process_fixed_manifest = root.join("candidate-process-fixed.toml");
        let process_fixed_lock = root.join("candidate-process-fixed.lock");

        write_package(
            &root,
            "consumer",
            "release-demo.consumer",
            &[],
            true,
            false,
            artifact,
        )?;
        write_package(
            &root,
            "provider",
            "release-demo.provider",
            &["fixture.lifecycle-probe"],
            false,
            false,
            artifact,
        )?;
        write_package(
            &root,
            "fixed-provider",
            "release-demo.fixed-provider",
            &["fixture.lifecycle-probe"],
            false,
            true,
            artifact,
        )?;
        write_composition(&installed_manifest, None)?;
        write_composition(&provider_v1_manifest, Some(("v1", "provider/plugin.toml")))?;
        write_composition(&provider_v2_manifest, Some(("v2", "provider/plugin.toml")))?;
        write_composition(&crash_manifest, Some(("v3-crash", "provider/plugin.toml")))?;
        write_composition(
            &process_fixed_manifest,
            Some(("v4-fixed", "fixed-provider/plugin.toml")),
        )?;

        Ok(Self {
            _temporary: temporary,
            failpoint_gate: root.join("commit-before-ack.gate"),
            state,
            installed_manifest,
            installed_lock,
            provider_v1_manifest,
            provider_v1_lock,
            provider_v2_manifest,
            provider_v2_lock,
            crash_manifest,
            crash_lock,
            process_fixed_manifest,
            process_fixed_lock,
        })
    }

    async fn resolve_all_locks(&self, binary: &Path) -> Result<()> {
        ensure!(!self.state.exists(), "release demo state was not empty");
        for (manifest, lock) in [
            (&self.installed_manifest, &self.installed_lock),
            (&self.provider_v1_manifest, &self.provider_v1_lock),
            (&self.provider_v2_manifest, &self.provider_v2_lock),
            (&self.crash_manifest, &self.crash_lock),
            (&self.process_fixed_manifest, &self.process_fixed_lock),
        ] {
            resolve_lock(binary, &self.state, manifest, lock).await?;
        }
        ensure!(
            !self.state.exists(),
            "offline lock resolution created daemon state"
        );
        Ok(())
    }
}

fn write_package(
    root: &Path,
    directory: &str,
    package_id: &str,
    provides: &[&str],
    inject_required: bool,
    process_fixed: bool,
    artifact: &Path,
) -> Result<()> {
    let package = root.join(directory);
    fs::create_dir_all(&package)?;
    let artifact_name = format!("artifact{}", std::env::consts::DLL_SUFFIX);
    fs::copy(artifact, package.join(&artifact_name)).with_context(|| {
        format!(
            "copy real lifecycle-probe artifact from {}",
            artifact.display()
        )
    })?;
    fs::write(
        package.join("config.schema.json"),
        fs::read(
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../lifecycle-probe/config.schema.json"),
        )?,
    )?;
    let provides = provides
        .iter()
        .map(|service| format!("\"{service}\""))
        .collect::<Vec<_>>()
        .join(", ");
    let service_injection = if inject_required {
        "[[injects]]\ncontract = \"fixture.lifecycle-probe\"\nrequired = true\n\n"
    } else {
        ""
    };
    let state_injection = "[[injects]]\ncontract = \"state.cas\"\nrequired = true\n";
    let runtime_injections = "\n[[injects]]\ncontract = \"fixture.echo\"\nrequired = false\n\n[[injects]]\ncontract = \"runtime.tick\"\nrequired = true\n";
    let api = ApiVersion::CURRENT;
    fs::write(
        package.join("plugin.toml"),
        format!(
            "format_version = 0\nprovides = [{provides}]\ncapabilities = [\"state.cas\", \"control.apply-manifest\"]\nconfig_schema = \"config.schema.json\"\n\n[package]\nid = \"{package_id}\"\nversion = \"1.0.0\"\nprocess_fixed = {process_fixed}\n\n[host_api]\nmajor = {}\nminimum_minor = {}\n\n[[artifacts]]\ntarget = \"{}\"\npath = \"{artifact_name}\"\n\n{service_injection}{state_injection}{runtime_injections}",
            api.major, api.minor, BUILD_TARGET,
        ),
    )?;
    Ok(())
}

fn write_composition(path: &Path, provider: Option<(&str, &str)>) -> Result<()> {
    let provider = provider.map_or_else(String::new, |(tag, package)| {
        format!(
            r#"
[[instances]]
id = "provider"
package = "{package}"
scope = "root"
config = {{ fail_prepare = false, retire_mode = "ack", tag = "{tag}" }}
"#
        )
    });
    fs::write(
        path,
        format!(
            r#"format_version = 0

[composition]
id = "release-demo"
mode = "development"

[[scopes]]
id = "root"

[[scopes]]
id = "app"
parent = "root"

[[instances]]
id = "consumer"
package = "consumer/plugin.toml"
scope = "app"
config = {{ fail_prepare = false, retire_mode = "ack", tag = "consumer" }}
{provider}"#
        ),
    )?;
    Ok(())
}

async fn resolve_lock(binary: &Path, state: &Path, manifest: &Path, lock: &Path) -> Result<()> {
    let output = timeout(
        IO_DEADLINE,
        ProcessCommand::new(binary)
            .arg("--state-dir")
            .arg(state)
            .arg("lock")
            .arg(manifest)
            .arg("--lock")
            .arg(lock)
            .output(),
    )
    .await
    .context("lock resolution timed out")??;
    ensure!(
        output.status.success(),
        "lock resolution failed for {}: stdout={} stderr={}",
        manifest.display(),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
    ensure!(lock.is_file(), "lock was not created at {}", lock.display());
    Ok(())
}

async fn install_candidate(
    binary: &Path,
    state: &Path,
    manifest: &Path,
    lock: &Path,
    operation_id: &str,
) -> Result<std::process::Output> {
    timeout(
        IO_DEADLINE,
        ProcessCommand::new(binary)
            .arg("--state-dir")
            .arg(state)
            .arg("install")
            .arg(manifest)
            .arg("--lock")
            .arg(lock)
            .arg("--operation-id")
            .arg(operation_id)
            .output(),
    )
    .await
    .context("offline install timed out")?
    .context("run offline install")
}

async fn run_mutation_command(
    binary: &Path,
    state: &Path,
    command: &[&str],
) -> Result<std::process::Output> {
    timeout(
        IO_DEADLINE,
        ProcessCommand::new(binary)
            .arg("--state-dir")
            .arg(state)
            .args(command)
            .output(),
    )
    .await
    .context("CLI mutation timed out")?
    .context("run CLI mutation")
}

#[derive(Clone, Debug, Deserialize)]
struct Ready {
    status: String,
    socket: PathBuf,
    http: std::net::SocketAddr,
    token_file: PathBuf,
}

#[derive(Debug)]
struct Daemon {
    child: Child,
    ready: Ready,
    _stdout: BufReader<ChildStdout>,
    _stderr: ChildStderr,
    stopped: bool,
}

impl Daemon {
    async fn start(binary: &Path, files: &DemoFiles, failpoint: bool) -> Result<Self> {
        let mut command = ProcessCommand::new(binary);
        command
            .arg("--state-dir")
            .arg(&files.state)
            .arg("daemon")
            .arg("serve")
            .arg("--http-bind")
            .arg("127.0.0.1:0")
            .env("RUST_LOG", "error")
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        if failpoint {
            command.env(
                FAILPOINT_ENV,
                serde_json::to_string(&json!({
                    "command_id": CRASH_COMMAND_ID,
                    "gate_path": files.failpoint_gate,
                }))?,
            );
        }
        let mut child = command.spawn().context("spawn foreground daemon")?;
        let stdout = child.stdout.take().context("capture daemon stdout")?;
        let stderr = child.stderr.take().context("capture daemon stderr")?;
        let mut stdout = BufReader::new(stdout);
        let mut line = String::new();
        let bytes = timeout(IO_DEADLINE, stdout.read_line(&mut line))
            .await
            .context("daemon readiness timed out")??;
        ensure!(bytes != 0, "daemon exited before readiness");
        let ready: Ready = serde_json::from_str(&line).context("decode daemon readiness")?;
        ensure!(ready.status == "ready", "unexpected daemon readiness");
        Ok(Self {
            child,
            ready,
            _stdout: stdout,
            _stderr: stderr,
            stopped: false,
        })
    }

    async fn kill(&mut self) -> Result<()> {
        self.child.kill().await.context("kill daemon child")?;
        self.stopped = true;
        Ok(())
    }

    async fn wait_for_exit(&mut self) -> Result<std::process::ExitStatus> {
        let status = timeout(IO_DEADLINE, self.child.wait())
            .await
            .context("daemon exit timed out")??;
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

async fn open_unix(socket: &Path) -> Result<UnixControl> {
    let stream = timeout(IO_DEADLINE, UnixStream::connect(socket))
        .await
        .context("Unix client connection timed out")??;
    Ok(Framed::new(
        stream,
        LinesCodec::new_with_max_length(MAX_WIRE_BYTES),
    ))
}

async fn send_command(
    connection: &mut UnixControl,
    command: &CommandEnvelope,
) -> Result<CommandOutcomeEnvelope> {
    connection
        .send(serde_json::to_string(command)?)
        .await
        .context("send control command")?;
    let line = timeout(IO_DEADLINE, connection.next())
        .await
        .context("control outcome timed out")?
        .context("daemon closed before outcome")??;
    serde_json::from_str(&line).context("decode control outcome")
}

#[derive(Debug, Deserialize)]
struct TokenEnvelope {
    token: String,
}

fn websocket_request(ready: &Ready, after: u64) -> Result<Request<()>> {
    let token: TokenEnvelope = serde_json::from_slice(&fs::read(&ready.token_file)?)?;
    let mut request = format!("ws://{}/ws?after={after}", ready.http)
        .into_client_request()
        .context("build WebSocket request")?;
    request.headers_mut().insert(
        header::AUTHORIZATION,
        HeaderValue::from_str(&format!("Bearer {}", token.token))?,
    );
    Ok(request)
}

async fn connect_websocket(ready: &Ready, after: u64) -> Result<ClientWebSocket> {
    let (socket, _) = timeout(IO_DEADLINE, connect_async(websocket_request(ready, after)?))
        .await
        .context("WebSocket connection timed out")??;
    Ok(socket)
}

async fn send_ws(socket: &mut ClientWebSocket, envelope: &impl serde::Serialize) -> Result<()> {
    socket
        .send(Message::Text(serde_json::to_string(envelope)?.into()))
        .await
        .context("send WebSocket envelope")
}

async fn next_text(socket: &mut ClientWebSocket) -> Result<serde_json::Value> {
    loop {
        let message = timeout(IO_DEADLINE, socket.next())
            .await
            .context("WebSocket receive timed out")?
            .context("WebSocket closed unexpectedly")??;
        match message {
            Message::Text(text) => return serde_json::from_str(text.as_str()).map_err(Into::into),
            Message::Ping(bytes) => socket.send(Message::Pong(bytes)).await?,
            Message::Close(frame) => bail!("WebSocket closed unexpectedly: {frame:?}"),
            Message::Binary(_) | Message::Pong(_) | Message::Frame(_) => {}
        }
    }
}

async fn next_event(socket: &mut ClientWebSocket) -> Result<EventEnvelope> {
    loop {
        let value = next_text(socket).await?;
        if value.get("kind").and_then(serde_json::Value::as_str) == Some("event") {
            return serde_json::from_value(value).context("decode control event");
        }
    }
}

async fn next_stream(socket: &mut ClientWebSocket, stream_id: &str) -> Result<StreamEnvelope> {
    loop {
        let value = next_text(socket).await?;
        if value.get("protocol").and_then(serde_json::Value::as_str)
            == Some(rsi_meta::STREAM_PROTOCOL)
            && value.get("stream_id").and_then(serde_json::Value::as_str) == Some(stream_id)
        {
            return serde_json::from_value(value).context("decode service stream frame");
        }
    }
}

fn graph_from(outcome: CommandOutcomeEnvelope) -> Result<GraphSnapshot> {
    match outcome.payload {
        CommandOutcome::Graph { graph, .. } | CommandOutcome::Applied { graph } => Ok(graph),
        other => bail!("expected graph outcome, received {other:?}"),
    }
}

fn graph_and_cursor_from(outcome: CommandOutcomeEnvelope) -> Result<(GraphSnapshot, u64)> {
    let CommandOutcome::Graph { graph, cursor } = outcome.payload else {
        bail!("expected graph outcome with cursor")
    };
    Ok((graph, cursor))
}

fn assert_consumer(graph: &GraphSnapshot, active: bool) -> Result<()> {
    let consumer = graph
        .instances
        .get(&InstanceId::new("consumer"))
        .context("consumer is absent from graph")?;
    ensure!(
        consumer.status.is_active() == active,
        "consumer active={} but expected {active}: {:?}",
        consumer.status.is_active(),
        consumer.status,
    );
    if !active {
        ensure!(matches!(consumer.status, InstanceStatus::Inactive { .. }));
    }
    Ok(())
}

async fn apply(
    connection: &mut UnixControl,
    command_id: &str,
    manifest: &Path,
    lock: &Path,
) -> Result<CommandOutcomeEnvelope> {
    let command = CommandEnvelope::new(
        command_id,
        Command::ApplyManifestPath {
            manifest_path: manifest.to_path_buf(),
            lock_path: lock.to_path_buf(),
        },
    );
    send_command(connection, &command).await
}

fn open_frame(stream_id: &str) -> Result<StreamEnvelope> {
    let mut frame = StreamEnvelope::new(stream_id, StreamKind::Open);
    frame.payload = Some(serde_json::to_value(ServiceOpenRequest {
        consumer: InstanceId::new("consumer"),
        service: ServiceKey::new("fixture.lifecycle-probe"),
    })?);
    Ok(frame)
}

async fn open_stream(socket: &mut ClientWebSocket, stream_id: &str) -> Result<()> {
    send_ws(socket, &open_frame(stream_id)?).await?;
    let opened = next_stream(socket, stream_id).await?;
    ensure!(opened.kind == StreamKind::Open);
    let credit = next_stream(socket, stream_id).await?;
    ensure!(credit.kind == StreamKind::Credit);
    ensure!(credit.credit_bytes == Some(1024 * 1024));
    Ok(())
}

async fn exchange(
    socket: &mut ClientWebSocket,
    stream_id: &str,
    sequence: u64,
    tag: &str,
    input: &[u8],
) -> Result<()> {
    let mut expected = tag.as_bytes().to_vec();
    expected.push(0);
    expected.extend_from_slice(input);
    let encoded_credit = u64::try_from(serde_json::to_vec(&json!(expected))?.len())?;
    let mut credit = StreamEnvelope::new(stream_id, StreamKind::Credit);
    credit.credit_bytes = Some(encoded_credit);
    send_ws(socket, &credit).await?;

    let mut data = StreamEnvelope::new(stream_id, StreamKind::Data);
    data.sequence = Some(sequence);
    data.payload = Some(json!(input));
    send_ws(socket, &data).await?;
    let echoed = next_stream(socket, stream_id).await?;
    ensure!(echoed.kind == StreamKind::Data);
    ensure!(echoed.payload == Some(json!(expected)));
    Ok(())
}

async fn end_stream(socket: &mut ClientWebSocket, stream_id: &str) -> Result<()> {
    send_ws(
        socket,
        &StreamEnvelope::new(stream_id, StreamKind::HalfClose),
    )
    .await?;
    ensure!(next_stream(socket, stream_id).await?.kind == StreamKind::End);
    Ok(())
}

#[allow(clippy::too_many_lines)] // Keep the ordered release and crash-recovery gate in one transcript.
async fn run_release_gate(binary: &Path, failpoint_binary: &Path, artifact: &Path) -> Result<()> {
    let files = DemoFiles::create(artifact)?;

    println!("[1/13] resolving create-or-verify locks from an empty state directory");
    files.resolve_all_locks(binary).await?;

    println!("[2/13] installing the initial composition while the workspace is offline");
    let initial_install = install_candidate(
        binary,
        &files.state,
        &files.installed_manifest,
        &files.installed_lock,
        "release-demo-install-initial",
    )
    .await?;
    ensure!(
        initial_install.status.success(),
        "initial offline install failed: {}",
        String::from_utf8_lossy(&initial_install.stderr)
    );

    let gate = UnixListener::bind(&files.failpoint_gate).context("bind failpoint gate")?;
    println!("[3/13] starting foreground daemon from the installed workspace pair");
    let mut daemon = Daemon::start(failpoint_binary, &files, true).await?;

    println!("[4/13] attaching Unix and WebSocket clients at a durable cursor");
    let mut unix = open_unix(&daemon.ready.socket).await?;
    let (graph, cursor) = graph_and_cursor_from(
        send_command(
            &mut unix,
            &CommandEnvelope::new("release-demo-initial-graph", Command::QueryGraph),
        )
        .await?,
    )?;
    assert_consumer(&graph, false)?;
    let mut websocket = connect_websocket(&daemon.ready, cursor).await?;

    println!("[5/13] applying provider v1 and activating the required consumer");
    let applied_v1 = apply(
        &mut unix,
        "release-demo-apply-v1",
        &files.provider_v1_manifest,
        &files.provider_v1_lock,
    )
    .await?;
    let graph_v1 = graph_from(applied_v1)?;
    assert_consumer(&graph_v1, true)?;
    let event_v1 = next_event(&mut websocket).await?;
    ensure!(
        event_v1.cursor == cursor + 1,
        "snapshot/subscription cursor gap"
    );
    ensure!(event_v1.graph_revision == graph_v1.revision);
    ensure!(matches!(
        event_v1.payload,
        Event::CompositionCommitted {
            source: CompositionChangeSource::Apply,
            ..
        }
    ));

    println!("[6/13] exchanging a credit-bounded bidirectional stream");
    open_stream(&mut websocket, "old-generation").await?;
    exchange(&mut websocket, "old-generation", 1, "v1", b"before-cutover").await?;

    println!("[7/13] replacing provider while the admitted old stream remains pinned");
    let graph_v2 = graph_from(
        apply(
            &mut unix,
            "release-demo-apply-v2",
            &files.provider_v2_manifest,
            &files.provider_v2_lock,
        )
        .await?,
    )?;
    ensure!(graph_v2.revision > graph_v1.revision);
    let event_v2 = next_event(&mut websocket).await?;
    ensure!(event_v2.graph_revision == graph_v2.revision);
    open_stream(&mut websocket, "new-generation").await?;
    exchange(&mut websocket, "new-generation", 1, "v2", b"new-admission").await?;
    exchange(&mut websocket, "old-generation", 2, "v1", b"still-pinned").await?;
    end_stream(&mut websocket, "new-generation").await?;
    end_stream(&mut websocket, "old-generation").await?;

    println!("[8/13] gating after durable commit and before acknowledgement");
    let crash_command = CommandEnvelope::new(
        CRASH_COMMAND_ID,
        Command::ApplyManifestPath {
            manifest_path: files.crash_manifest.clone(),
            lock_path: files.crash_lock.clone(),
        },
    );
    unix.send(serde_json::to_string(&crash_command)?).await?;
    tokio::select! {
        biased;
        accepted = timeout(IO_DEADLINE, gate.accept()) => {
            let (mut stream, _) = accepted
                .context("durable-before-ack gate accept timed out")?
                .context("accept durable-before-ack gate")?;
            let mut ready = [0_u8; 1];
            timeout(IO_DEADLINE, stream.read_exact(&mut ready))
                .await
                .context("failpoint gate notification timed out")??;
            ensure!(ready == [1], "unexpected failpoint readiness byte");
            daemon.kill().await?;
        }
        outcome = unix.next() => {
            let detail = match outcome {
                Some(Ok(line)) => format!("daemon emitted response bytes: {line}"),
                Some(Err(error)) => format!("transport failed before failpoint gate: {error}"),
                None => "daemon closed without notifying the failpoint gate".to_owned(),
            };
            bail!(
                "release steps 7-8 BLOCKED: missing demo-only durable-before-ack gate ({detail}); enable test-failpoints and configure {FAILPOINT_ENV} as documented"
            );
        }
    }

    println!("[9/13] restarting and replaying the original command id");
    drop(unix);
    drop(websocket);
    let mut restarted = Daemon::start(binary, &files, false).await?;
    let mut recovered_client = open_unix(&restarted.ready.socket).await?;
    let recovered = send_command(&mut recovered_client, &crash_command).await?;
    let recovered_graph = match &recovered.payload {
        CommandOutcome::Applied { graph } => graph.clone(),
        other => bail!("expected stored applied outcome, received {other:?}"),
    };
    let current_graph = graph_from(
        send_command(
            &mut recovered_client,
            &CommandEnvelope::new("release-demo-recovered-graph", Command::QueryGraph),
        )
        .await?,
    )?;
    ensure!(
        current_graph == recovered_graph,
        "stored terminal outcome graph differs from the recovered daemon graph"
    );
    let replay = send_command(&mut recovered_client, &crash_command).await?;
    ensure!(replay == recovered, "terminal outcome changed on retry");

    println!("[10/13] applying a process-fixed candidate and observing exit 75");
    let installed_manifest_before = fs::read(files.state.join("composition.toml"))?;
    let installed_lock_before = fs::read(files.state.join("rsi-meta.lock"))?;
    let process_fixed_apply = timeout(
        IO_DEADLINE,
        ProcessCommand::new(binary)
            .arg("--state-dir")
            .arg(&files.state)
            .arg("apply")
            .arg(&files.process_fixed_manifest)
            .arg("--lock")
            .arg(&files.process_fixed_lock)
            .arg("--operation-id")
            .arg("release-demo-process-fixed-apply")
            .output(),
    )
    .await
    .context("process-fixed apply timed out")??;
    ensure!(
        process_fixed_apply.status.code()
            == Some(i32::from(rsi_meta_cli::DAEMON_RESTART_EXIT_CODE)),
        "process-fixed apply did not exit 75: stdout={} stderr={}",
        String::from_utf8_lossy(&process_fixed_apply.stdout),
        String::from_utf8_lossy(&process_fixed_apply.stderr)
    );
    ensure!(
        restarted.child.try_wait()?.is_none(),
        "process-fixed preflight stopped the daemon"
    );
    ensure!(
        fs::read(files.state.join("composition.toml"))? == installed_manifest_before
            && fs::read(files.state.join("rsi-meta.lock"))? == installed_lock_before,
        "process-fixed preflight modified the installed pair"
    );

    println!("[11/13] explicitly stopping the old daemon");
    let stop = run_mutation_command(
        binary,
        &files.state,
        &[
            "daemon",
            "stop",
            "--operation-id",
            "release-demo-process-fixed-stop",
        ],
    )
    .await?;
    ensure!(
        stop.status.success(),
        "daemon stop failed: {}",
        String::from_utf8_lossy(&stop.stderr)
    );
    ensure!(restarted.wait_for_exit().await?.success());

    println!("[12/13] installing the process-fixed pair while the workspace is offline");
    let fixed_install = install_candidate(
        binary,
        &files.state,
        &files.process_fixed_manifest,
        &files.process_fixed_lock,
        "release-demo-process-fixed-install",
    )
    .await?;
    ensure!(
        fixed_install.status.success(),
        "process-fixed offline install failed: {}",
        String::from_utf8_lossy(&fixed_install.stderr)
    );

    println!("[13/13] starting a fresh daemon and activating the installed pair once");
    let mut fresh = Daemon::start(binary, &files, false).await?;
    let mut fresh_client = open_unix(&fresh.ready.socket).await?;
    let fresh_graph = graph_from(
        send_command(
            &mut fresh_client,
            &CommandEnvelope::new("release-demo-fresh-graph", Command::QueryGraph),
        )
        .await?,
    )?;
    ensure!(
        fresh_graph.instances.values().any(|instance| {
            instance.status.is_active()
                && instance
                    .package
                    .manifest_path
                    .to_string_lossy()
                    .contains("fixed-provider")
        }),
        "fresh daemon did not activate the process-fixed installation"
    );
    ensure!(
        fresh_graph.revision.0 == recovered_graph.revision.0 + 1,
        "fresh daemon must activate the offline installation exactly once"
    );
    let final_stop = run_mutation_command(
        binary,
        &files.state,
        &[
            "daemon",
            "stop",
            "--operation-id",
            "release-demo-final-stop",
        ],
    )
    .await?;
    ensure!(final_stop.status.success(), "final daemon stop failed");
    ensure!(fresh.wait_for_exit().await?.success());

    println!("release demonstration passed all thirteen steps");
    Ok(())
}

#[tokio::main]
async fn main() -> Result<()> {
    let binary = std::env::var_os("RSI_META_BIN")
        .map(PathBuf::from)
        .context("RSI_META_BIN is required; run cargo xtask rsi-meta release-demo")?;
    let failpoint_binary = std::env::var_os("RSI_META_FAILPOINT_BIN")
        .map(PathBuf::from)
        .context("RSI_META_FAILPOINT_BIN is required; run cargo xtask rsi-meta release-demo")?;
    let artifact = std::env::var_os("RSI_META_LIFECYCLE_PROBE_ARTIFACT")
        .map(PathBuf::from)
        .context("RSI_META_LIFECYCLE_PROBE_ARTIFACT is required")?;
    ensure!(binary.is_file(), "rsi-meta binary is missing");
    ensure!(
        failpoint_binary.is_file(),
        "feature-built rsi-meta binary is missing"
    );
    ensure!(artifact.is_file(), "lifecycle-probe artifact is missing");
    run_release_gate(&binary, &failpoint_binary, &artifact).await
}
