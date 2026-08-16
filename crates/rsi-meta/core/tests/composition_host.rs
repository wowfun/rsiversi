use std::fs;
use std::io::Write;
use std::path::Path;
use std::path::PathBuf;
use std::process::Command as ProcessCommand;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use rsi_meta::{
    ApplyRequest, ApplyResult, CompositionHost, CompositionLock, CompositionProject,
    CompositionWorkspace, GraphRevision, HostEventRecord, InstanceId, LockResult, OpenOptions,
    OperationId, ServiceKey, ServiceOpenRequest, StreamKind,
};
use rsi_meta_loader::{ApiVersion, BUILD_TARGET, ContentHash};
use rusqlite::Connection;
use tempfile::{TempDir, tempdir};

#[derive(Clone, Debug, PartialEq)]
enum Command {
    ApplyManifestPath {
        manifest_path: PathBuf,
        lock_path: PathBuf,
    },
    LockManifest {
        manifest_path: PathBuf,
        lock_path: PathBuf,
    },
    QueryGraph,
    QueryEvents {
        after_cursor: u64,
        limit: u32,
    },
    RotateToken,
    Unknown,
}

#[derive(Clone, Debug, PartialEq)]
struct CommandEnvelope {
    command_id: String,
    expected_graph_revision: Option<GraphRevision>,
    payload: Command,
}

impl CommandEnvelope {
    fn new(command_id: impl Into<String>, payload: Command) -> Self {
        Self {
            command_id: command_id.into(),
            expected_graph_revision: None,
            payload,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
enum CommandOutcome {
    Applied {
        graph: rsi_meta::GraphSnapshot,
    },
    NoChange {
        graph: rsi_meta::GraphSnapshot,
    },
    RestartRequired,
    Graph {
        graph: rsi_meta::GraphSnapshot,
        cursor: u64,
    },
    Events {
        events: Vec<HostEventRecord>,
    },
    LockResolved {
        lock: CompositionLock,
    },
    TokenRotated {
        generation: u64,
    },
    Rejected {
        code: String,
        message: String,
    },
}

#[derive(Clone, Debug, PartialEq)]
struct CommandOutcomeEnvelope {
    command_id: String,
    graph_revision: GraphRevision,
    payload: CommandOutcome,
}

impl CommandOutcomeEnvelope {
    fn rejected(
        command_id: impl Into<String>,
        graph_revision: GraphRevision,
        code: impl Into<String>,
        message: impl Into<String>,
    ) -> Self {
        Self {
            command_id: command_id.into(),
            graph_revision,
            payload: CommandOutcome::Rejected {
                code: code.into(),
                message: message.into(),
            },
        }
    }
}

fn workspace(database: impl AsRef<Path>, cache: impl AsRef<Path>) -> CompositionWorkspace {
    let database_path = database.as_ref().to_owned();
    let root = database_path.parent().expect("database parent").to_owned();
    CompositionWorkspace {
        database_path,
        cache_root: cache.as_ref().to_owned(),
        manifest_path: root.join("composition.toml"),
        lock_path: root.join("installed.lock"),
    }
}

fn open_options(database: impl AsRef<Path>, cache: impl AsRef<Path>) -> OpenOptions {
    OpenOptions::new(workspace(database, cache))
}

async fn assert_offline_install_conflict(request: rsi_meta::InstallRequest) {
    let error = CompositionHost::install_offline(request)
        .await
        .expect_err("operation id parameters must remain stable");
    assert!(matches!(
        error,
        rsi_meta::HostError::OperationRejected { ref code, .. }
            if code == "operation_id_conflict"
    ));
}

async fn assert_installed_workspace_activates_once(workspace: CompositionWorkspace) {
    let first_open = CompositionHost::open(OpenOptions::new(workspace.clone()))
        .await
        .expect("open installed composition");
    assert_eq!(first_open.snapshot().graph.revision, GraphRevision(1));
    let first_events = first_open
        .events_after(0, 64)
        .await
        .expect("events after first activation");
    assert_eq!(composition_commit_count(&first_events.events), 1);
    first_open
        .shutdown(Instant::now() + Duration::from_secs(1))
        .await
        .expect("shutdown first activation");

    let second_open = CompositionHost::open(OpenOptions::new(workspace))
        .await
        .expect("reopen installed composition");
    assert_eq!(second_open.snapshot().graph.revision, GraphRevision(1));
    let second_events = second_open
        .events_after(0, 64)
        .await
        .expect("events after second open");
    assert_eq!(composition_commit_count(&second_events.events), 1);
    second_open
        .shutdown(Instant::now() + Duration::from_secs(1))
        .await
        .expect("shutdown reopened host");
}

trait TestOpenOptionsExt {
    fn with_composition(self, _manifest: impl AsRef<Path>, _lock: impl AsRef<Path>) -> Self;
}

impl TestOpenOptionsExt for OpenOptions {
    fn with_composition(self, _manifest: impl AsRef<Path>, _lock: impl AsRef<Path>) -> Self {
        self
    }
}

trait TestHostExt {
    async fn submit(&self, command: CommandEnvelope) -> rsi_meta::Result<CommandOutcomeEnvelope>;
    async fn shutdown(&self, deadline: Instant) -> rsi_meta::Result<()>;
}

impl TestHostExt for CompositionHost {
    async fn submit(&self, command: CommandEnvelope) -> rsi_meta::Result<CommandOutcomeEnvelope> {
        let command_id = command.command_id;
        let result = match command.payload {
            Command::ApplyManifestPath {
                manifest_path,
                lock_path,
            } => self
                .apply(ApplyRequest {
                    operation_id: OperationId(command_id.clone()),
                    project: CompositionProject {
                        manifest_path,
                        lock_path: Some(lock_path),
                    },
                    expected_revision: command.expected_graph_revision,
                })
                .await
                .map(|result| match result {
                    ApplyResult::Applied { snapshot } => CommandOutcome::Applied {
                        graph: snapshot.graph,
                    },
                    ApplyResult::Unchanged { snapshot } => CommandOutcome::NoChange {
                        graph: snapshot.graph,
                    },
                    ApplyResult::RestartRequired { .. } => CommandOutcome::RestartRequired,
                }),
            Command::LockManifest {
                manifest_path,
                lock_path,
            } => CompositionProject {
                manifest_path,
                lock_path: Some(lock_path),
            }
            .lock()
            .map(|result| match result {
                LockResult::Created { lock } | LockResult::Unchanged { lock } => {
                    CommandOutcome::LockResolved { lock }
                }
            }),
            Command::QueryGraph => {
                let snapshot = self.snapshot();
                Ok(CommandOutcome::Graph {
                    graph: snapshot.graph,
                    cursor: snapshot.cursor,
                })
            }
            Command::QueryEvents {
                after_cursor,
                limit,
            } => self
                .events_after(after_cursor, limit)
                .await
                .map(|page| CommandOutcome::Events {
                    events: page.events,
                }),
            Command::RotateToken => self
                .rotate_token(OperationId(command_id.clone()))
                .await
                .map(|rotation| CommandOutcome::TokenRotated {
                    generation: rotation.generation,
                }),
            Command::Unknown => Ok(CommandOutcome::Rejected {
                code: "unknown_command".to_owned(),
                message: "unknown test command".to_owned(),
            }),
        };
        match result {
            Ok(payload) => Ok(CommandOutcomeEnvelope {
                command_id,
                graph_revision: self.snapshot().graph.revision,
                payload,
            }),
            Err(rsi_meta::HostError::OperationRejected { code, message, .. }) => {
                Ok(CommandOutcomeEnvelope::rejected(
                    command_id,
                    self.snapshot().graph.revision,
                    code,
                    message,
                ))
            }
            Err(error) => Err(error),
        }
    }

    async fn shutdown(&self, deadline: Instant) -> rsi_meta::Result<()> {
        static NEXT_SHUTDOWN: AtomicU64 = AtomicU64::new(1);
        let sequence = NEXT_SHUTDOWN.fetch_add(1, Ordering::Relaxed);
        self.request_shutdown(OperationId(format!("test-shutdown-{sequence}")))
            .await?;
        self.wait_terminated(deadline).await
    }
}

#[derive(Clone, Debug, serde::Serialize)]
struct DesiredState {
    manifest_sha256: Option<String>,
    lock_sha256: Option<String>,
    applied: bool,
    last_rejection_code: Option<String>,
    plugin_restart_requested: bool,
}

#[derive(Debug)]
struct BuiltEcho {
    _root: TempDir,
    library: PathBuf,
}

#[derive(Debug)]
struct BuiltProbe {
    _root: TempDir,
    library: PathBuf,
}

fn build_echo() -> &'static BuiltEcho {
    static ECHO: OnceLock<BuiltEcho> = OnceLock::new();
    ECHO.get_or_init(|| {
        let root = tempdir().expect("fixture tempdir");
        let workspace = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .and_then(Path::parent)
            .and_then(Path::parent)
            .expect("workspace root");
        let manifest = workspace.join("fixtures/rsi-meta/echo-bidi/Cargo.toml");
        let target = root.path().join("target");
        let status = ProcessCommand::new(env!("CARGO"))
            .args([
                "build",
                "--quiet",
                "--locked",
                "--release",
                "--offline",
                "--manifest-path",
            ])
            .arg(&manifest)
            .env("CARGO_TARGET_DIR", &target)
            .status()
            .expect("build echo fixture");
        assert!(status.success(), "real echo cdylib build failed");
        let library = target.join("release").join(format!(
            "{}rsi_meta_fixture_echo_bidi{}",
            std::env::consts::DLL_PREFIX,
            std::env::consts::DLL_SUFFIX
        ));
        assert!(library.is_file(), "fixture cdylib missing");
        BuiltEcho {
            _root: root,
            library,
        }
    })
}

fn build_probe() -> &'static BuiltProbe {
    static PROBE: OnceLock<BuiltProbe> = OnceLock::new();
    PROBE.get_or_init(|| {
        let root = tempdir().expect("probe fixture tempdir");
        let workspace = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .and_then(Path::parent)
            .and_then(Path::parent)
            .expect("workspace root");
        let manifest = workspace.join("fixtures/rsi-meta/lifecycle-probe/Cargo.toml");
        let target = root.path().join("target");
        let status = ProcessCommand::new(env!("CARGO"))
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
            .arg(&manifest)
            .env("CARGO_TARGET_DIR", &target)
            .status()
            .expect("build lifecycle probe fixture");
        assert!(status.success(), "real lifecycle probe build failed");
        let library = target.join(BUILD_TARGET).join("release").join(format!(
            "{}rsi_meta_fixture_lifecycle_probe{}",
            std::env::consts::DLL_PREFIX,
            std::env::consts::DLL_SUFFIX
        ));
        assert!(library.is_file(), "lifecycle probe cdylib missing");
        BuiltProbe {
            _root: root,
            library,
        }
    })
}

fn write_package(root: &Path, name: &str, provides: &[&str], injects: &[(&str, bool)]) {
    let echo = build_echo();
    let package = root.join(name);
    fs::create_dir_all(&package).expect("package directory");
    fs::copy(
        &echo.library,
        package.join(format!("artifact{}", std::env::consts::DLL_SUFFIX)),
    )
    .expect("copy real cdylib fixture");
    fs::write(
        package.join("config.schema.json"),
        r#"{
  "$schema":"https://json-schema.org/draft/2020-12/schema",
  "type":"object",
  "additionalProperties":false
}"#,
    )
    .expect("config schema");
    let provides = provides
        .iter()
        .map(|contract| format!("\"{contract}\""))
        .collect::<Vec<_>>()
        .join(", ");
    let injects = injects
        .iter()
        .map(|(contract, required)| {
            format!("[[injects]]\ncontract = \"{contract}\"\nrequired = {required}\n")
        })
        .collect::<Vec<_>>()
        .join("\n");
    let api = ApiVersion::CURRENT;
    fs::write(
        package.join("plugin.toml"),
        format!(
            "format_version = 0\nprovides = [{provides}]\nconfig_schema = \"config.schema.json\"\n\n[package]\nid = \"{name}\"\nversion = \"1.0.0\"\n\n[host_api]\nmajor = {}\nminimum_minor = {}\n\n[[artifacts]]\ntarget = \"{}\"\npath = \"artifact{}\"\n\n{injects}",
            api.major, api.minor, BUILD_TARGET, std::env::consts::DLL_SUFFIX
        ),
    )
    .expect("plugin manifest fixture");
}

fn write_probe_packages(root: &Path) {
    write_package(root, "echo-provider", &["fixture.echo"], &[]);
    write_package(
        root,
        "probe-client",
        &[],
        &[("fixture.lifecycle-probe", true)],
    );
    let package = root.join("probe");
    fs::create_dir_all(&package).expect("probe package directory");
    fs::copy(
        &build_probe().library,
        package.join(format!("artifact{}", std::env::consts::DLL_SUFFIX)),
    )
    .expect("copy lifecycle probe artifact");
    fs::write(
        package.join("config.schema.json"),
        include_str!("../../../../fixtures/rsi-meta/lifecycle-probe/config.schema.json"),
    )
    .expect("probe config schema");
    let api = ApiVersion::CURRENT;
    fs::write(
        package.join("plugin.toml"),
        format!(
            "format_version = 0\nprovides = [\"fixture.lifecycle-probe\"]\ncapabilities = [\"state.cas\", \"control.apply-manifest\"]\nconfig_schema = \"config.schema.json\"\n\n[package]\nid = \"fixture.lifecycle-probe\"\nversion = \"0.0.1\"\n\n[host_api]\nmajor = {}\nminimum_minor = {}\n\n[[artifacts]]\ntarget = \"{}\"\npath = \"artifact{}\"\n\n[[injects]]\ncontract = \"state.cas\"\nrequired = true\n\n[[injects]]\ncontract = \"fixture.echo\"\nrequired = false\n",
            api.major,
            api.minor,
            BUILD_TARGET,
            std::env::consts::DLL_SUFFIX,
        ),
    )
    .expect("probe plugin manifest");
}

fn write_probe_composition(path: &Path, action: &str, fault: &str, tag: &str) {
    fs::write(
        path,
        format!(
            r#"format_version = 0

[composition]
id = "probe-e2e"
mode = "development"

[[scopes]]
id = "root"

[[instances]]
id = "echo-provider"
package = "echo-provider/plugin.toml"
scope = "root"

[[instances]]
id = "probe"
package = "probe/plugin.toml"
scope = "root"
config = {{ fail_prepare = false, retire_mode = "ack", tag = "{tag}", prepare_action = "{action}", stream_fault = "{fault}" }}
bindings = {{ "fixture.echo" = "echo-provider" }}

[[instances]]
id = "probe-client"
package = "probe-client/plugin.toml"
scope = "root"
bindings = {{ "fixture.lifecycle-probe" = "probe" }}
"#,
        ),
    )
    .expect("probe composition");
}

fn write_composition(path: &Path) {
    fs::write(
        path,
        r#"format_version = 0

[composition]
id = "demo"
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
    )
    .expect("composition fixture");
}

async fn submit(host: &CompositionHost, id: &str, payload: Command) -> CommandOutcome {
    host.submit(CommandEnvelope::new(id, payload))
        .await
        .expect("command transport")
        .payload
}

fn empty_composition_source() -> &'static str {
    r#"format_version = 0
scopes = []
instances = []

[composition]
id = "empty"
mode = "development"
"#
}

async fn recv_service(stream: &mut rsi_meta::ServiceStream) -> rsi_meta::StreamEnvelope {
    tokio::time::timeout(Duration::from_secs(2), stream.recv())
        .await
        .expect("service frame deadline")
        .expect("service stream remains open")
        .expect("valid service frame")
}

async fn apply_probe_host(
    root: &Path,
    action: &str,
    fault: &str,
    command_prefix: &str,
) -> (CompositionHost, PathBuf, PathBuf) {
    write_probe_packages(root);
    let manifest = root.join("rsi-meta.toml");
    let lock = root.join("rsi-meta.lock");
    write_probe_composition(&manifest, action, fault, command_prefix);
    let host = CompositionHost::open(open_options(root.join("state.sqlite3"), root.join("cache")))
        .await
        .expect("open probe host");
    assert!(matches!(
        submit(
            &host,
            &format!("{command_prefix}-lock"),
            Command::LockManifest {
                manifest_path: manifest.clone(),
                lock_path: lock.clone(),
            },
        )
        .await,
        CommandOutcome::LockResolved { .. }
    ));
    let outcome = tokio::time::timeout(
        Duration::from_secs(3),
        host.submit(CommandEnvelope::new(
            format!("{command_prefix}-apply"),
            Command::ApplyManifestPath {
                manifest_path: manifest.clone(),
                lock_path: lock.clone(),
            },
        )),
    )
    .await
    .expect("probe apply deadline")
    .expect("probe apply transport");
    assert!(
        matches!(outcome.payload, CommandOutcome::Applied { .. }),
        "probe apply failed: {:?}",
        outcome.payload
    );
    (
        host,
        root.join("composition.toml"),
        root.join("installed.lock"),
    )
}

#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn disk_lock_apply_snapshot_subscription_and_durable_retry() {
    let temp = tempdir().expect("tempdir");
    write_package(temp.path(), "provider", &["fixture.echo"], &[]);
    write_package(temp.path(), "consumer", &[], &[("fixture.echo", true)]);
    let manifest_path = temp.path().join("rsi-meta.toml");
    let lock_path = temp.path().join("rsi-meta.lock");
    write_composition(&manifest_path);

    let database = temp.path().join("state.sqlite3");
    let cache = temp.path().join("cache");
    let host = CompositionHost::open(open_options(&database, &cache))
        .await
        .expect("open empty host");

    let lock = submit(
        &host,
        "lock-v1",
        Command::LockManifest {
            manifest_path: manifest_path.clone(),
            lock_path: lock_path.clone(),
        },
    )
    .await;
    assert!(matches!(lock, CommandOutcome::LockResolved { .. }));

    let apply_command = CommandEnvelope::new(
        "apply-v1",
        Command::ApplyManifestPath {
            manifest_path: manifest_path.clone(),
            lock_path: lock_path.clone(),
        },
    );
    let first_outcome = host
        .submit(apply_command.clone())
        .await
        .expect("apply composition");
    assert!(matches!(
        first_outcome.payload,
        CommandOutcome::Applied { .. }
    ));

    let snapshot = host.snapshot();
    assert_eq!(snapshot.graph.composition_id, "demo");
    assert_eq!(snapshot.graph.instances.len(), 2);
    assert_eq!(snapshot.graph.bindings.len(), 1);
    assert!(snapshot.cursor > 0);

    let graph_outcome = host
        .submit(CommandEnvelope::new(
            "query-graph-with-cursor",
            Command::QueryGraph,
        ))
        .await
        .expect("query graph with subscription cursor");
    let CommandOutcome::Graph { graph, cursor } = graph_outcome.payload else {
        panic!("query_graph did not return a graph snapshot")
    };
    assert_eq!(graph, snapshot.graph);
    assert_eq!(cursor, snapshot.cursor);

    let mut old_stream = host
        .open_service(ServiceOpenRequest {
            consumer: InstanceId::new("consumer"),
            service: ServiceKey::new("fixture.echo"),
        })
        .expect("open routed stream");
    assert_eq!(old_stream.provider(), &InstanceId::new("provider"));
    let credit = old_stream
        .recv()
        .await
        .expect("initial credit frame")
        .expect("credit is valid");
    assert!(matches!(credit.kind, rsi_meta::StreamKind::Credit));
    old_stream
        .grant_credit(1024)
        .await
        .expect("grant provider output credit");
    old_stream.send(b"before").await.expect("send before HMR");
    let echoed = old_stream
        .recv()
        .await
        .expect("echo frame")
        .expect("echo is valid");
    assert_eq!(
        echoed.payload,
        Some(serde_json::json!([98, 101, 102, 111, 114, 101]))
    );

    let mut events = host
        .subscribe(snapshot.cursor)
        .await
        .expect("subscribe after snapshot cursor");
    fs::OpenOptions::new()
        .append(true)
        .open(&manifest_path)
        .expect("open candidate manifest")
        .write_all(b"\n# candidate v2\n")
        .expect("change candidate manifest hash");
    fs::OpenOptions::new()
        .append(true)
        .open(temp.path().join("provider/plugin.toml"))
        .expect("open provider package manifest")
        .write_all(b"\n# provider package v2\n")
        .expect("change provider package hash");
    let second_lock_path = temp.path().join("rsi-meta-v2.lock");
    let _ = submit(
        &host,
        "lock-v2",
        Command::LockManifest {
            manifest_path: manifest_path.clone(),
            lock_path: second_lock_path.clone(),
        },
    )
    .await;
    let second_command = CommandEnvelope::new(
        "apply-v2",
        Command::ApplyManifestPath {
            manifest_path: manifest_path.clone(),
            lock_path: second_lock_path.clone(),
        },
    );
    let applying_host = host.clone();
    let applying_command = second_command.clone();
    let lost_ack = tokio::spawn(async move { applying_host.submit(applying_command).await });
    let event = tokio::time::timeout(Duration::from_secs(1), events.recv())
        .await
        .expect("event deadline")
        .expect("event stream remains open")
        .expect("event delivery");
    // The durable event is the acknowledgement boundary. Dropping the waiting
    // task after that point simulates a transport that lost the terminal reply.
    lost_ack.abort();
    let second_outcome = host
        .submit(second_command.clone())
        .await
        .expect("same command id recovers durable outcome after ack loss");
    assert!(event.cursor > snapshot.cursor);
    let retiring = host.snapshot().graph.retiring_instances;
    assert!(retiring.iter().any(|entry| {
        entry.instance_id == InstanceId::new("provider")
            && entry.generation_count == 1
            && entry.lease_count >= 1
    }));
    let graph = submit(&host, "graph-retiring", Command::QueryGraph).await;
    assert!(matches!(
        graph,
        CommandOutcome::Graph { ref graph, .. }
            if graph.retiring_instances.iter().any(|entry| entry.instance_id == InstanceId::new("provider"))
    ));
    assert_eq!(old_stream.provider(), &InstanceId::new("provider"));
    old_stream
        .send(b"after")
        .await
        .expect("unaffected stream remains live");
    let echoed = old_stream
        .recv()
        .await
        .expect("post-cutover echo")
        .expect("post-cutover echo valid");
    assert_eq!(
        echoed.payload,
        Some(serde_json::json!([97, 102, 116, 101, 114]))
    );
    old_stream
        .cancel("test_complete")
        .await
        .expect("host-owned cancellation");
    let terminal = old_stream
        .recv()
        .await
        .expect("one terminal frame")
        .expect("terminal frame is valid");
    assert!(matches!(terminal.kind, rsi_meta::StreamKind::Cancel));
    assert!(
        old_stream.recv().await.is_none(),
        "terminal is emitted once"
    );

    host.shutdown(Instant::now() + Duration::from_secs(2))
        .await
        .expect("shutdown");
    let reopened = CompositionHost::open(
        open_options(&database, &cache).with_composition(&manifest_path, &second_lock_path),
    )
    .await
    .expect("reopen installed composition");
    let replayed = reopened
        .submit(second_command)
        .await
        .expect("retry acknowledged command");
    assert_eq!(replayed.command_id, second_outcome.command_id);
    assert_eq!(replayed.graph_revision, second_outcome.graph_revision);
    let CommandOutcome::Applied { graph: replayed } = replayed.payload else {
        panic!("replayed apply changed result kind")
    };
    let CommandOutcome::Applied { graph: first } = second_outcome.payload else {
        panic!("initial apply changed result kind")
    };
    assert_eq!(replayed.revision, first.revision);
    assert_eq!(replayed.composition_id, first.composition_id);
    assert_eq!(replayed, reopened.snapshot().graph);
    reopened
        .shutdown(Instant::now() + Duration::from_secs(2))
        .await
        .expect("shutdown reopened host");
}

#[tokio::test]
async fn replayed_apply_returns_the_current_coherent_snapshot() {
    let temp = tempdir().expect("tempdir");
    let manifest = temp.path().join("candidate.toml");
    let first_lock = temp.path().join("candidate-a.lock");
    let second_lock = temp.path().join("candidate-b.lock");
    fs::write(&manifest, empty_composition_source()).expect("first manifest");
    CompositionProject {
        manifest_path: manifest.clone(),
        lock_path: Some(first_lock.clone()),
    }
    .lock()
    .expect("first lock");
    let host = CompositionHost::open(open_options(
        temp.path().join("state.sqlite3"),
        temp.path().join("cache"),
    ))
    .await
    .expect("host");
    let first_request = ApplyRequest {
        operation_id: OperationId("coherent-replay-a".to_owned()),
        project: CompositionProject {
            manifest_path: manifest.clone(),
            lock_path: Some(first_lock),
        },
        expected_revision: None,
    };
    assert!(matches!(
        host.apply(first_request.clone())
            .await
            .expect("first apply"),
        ApplyResult::Applied { .. }
    ));

    fs::write(
        &manifest,
        empty_composition_source().replace("id = \"empty\"", "id = \"newer\""),
    )
    .expect("second manifest");
    CompositionProject {
        manifest_path: manifest.clone(),
        lock_path: Some(second_lock.clone()),
    }
    .lock()
    .expect("second lock");
    host.apply(ApplyRequest {
        operation_id: OperationId("coherent-replay-b".to_owned()),
        project: CompositionProject {
            manifest_path: manifest,
            lock_path: Some(second_lock),
        },
        expected_revision: None,
    })
    .await
    .expect("second apply");
    let current = host.snapshot();

    let replayed = host.apply(first_request).await.expect("replay first apply");
    let ApplyResult::Applied { snapshot } = replayed else {
        panic!("stored apply changed result kind")
    };
    assert_eq!(
        snapshot, current,
        "snapshot fields must come from one cutover"
    );

    host.shutdown(Instant::now() + Duration::from_secs(1))
        .await
        .expect("shutdown");
}

#[tokio::test]
async fn reopening_an_unchanged_pair_uses_the_latest_durable_cursor() {
    let temp = tempdir().expect("tempdir");
    let database = temp.path().join("state.sqlite3");
    let workspace = workspace(&database, temp.path().join("cache"));
    let manifest = temp.path().join("candidate.toml");
    let lock = temp.path().join("candidate.lock");
    fs::write(&manifest, empty_composition_source()).expect("candidate manifest");
    let project = CompositionProject {
        manifest_path: manifest,
        lock_path: Some(lock),
    };
    project.lock().expect("candidate lock");
    CompositionHost::install_offline(rsi_meta::InstallRequest {
        operation_id: OperationId("cursor-install".to_owned()),
        workspace: workspace.clone(),
        project,
    })
    .await
    .expect("offline install");

    let first = CompositionHost::open(OpenOptions::new(workspace.clone()))
        .await
        .expect("first open");
    let composition_cursor = first.snapshot().cursor;
    first
        .shutdown(Instant::now() + Duration::from_secs(1))
        .await
        .expect("shutdown writes a later event");

    let reopened = CompositionHost::open(OpenOptions::new(workspace))
        .await
        .expect("reopen unchanged pair");
    assert_eq!(
        reopened.snapshot().cursor,
        composition_cursor + 1,
        "snapshot cursor must include the previous host's shutdown event"
    );
    assert!(
        reopened
            .events_after(reopened.snapshot().cursor, 1)
            .await
            .expect("events after reopened cursor")
            .events
            .is_empty(),
        "subscribing from the snapshot cursor must not replay an older event"
    );
    reopened
        .shutdown(Instant::now() + Duration::from_secs(1))
        .await
        .expect("shutdown reopened host");
}

#[tokio::test]
async fn deferred_prepare_state_read_is_pumped_before_prepared_ack() {
    let temp = tempdir().expect("tempdir");
    let (host, _, _) =
        apply_probe_host(temp.path(), "state_get_then_ack", "none", "deferred-state").await;
    let state_rows: i64 = Connection::open(temp.path().join("state.sqlite3"))
        .expect("state database")
        .query_row("SELECT COUNT(*) FROM plugin_state", [], |row| row.get(0))
        .expect("state row count");
    assert_eq!(state_rows, 0, "prepare GET must remain read-only");

    let mut stream = host
        .open_service(ServiceOpenRequest {
            consumer: InstanceId::new("probe-client"),
            service: ServiceKey::new("fixture.lifecycle-probe"),
        })
        .expect("prepared probe is admitting");
    assert_eq!(recv_service(&mut stream).await.kind, StreamKind::Credit);
    stream
        .grant_credit(1024 * 1024)
        .await
        .expect("grant probe output credit");
    stream.send(b"ready").await.expect("send probe data");
    let data = recv_service(&mut stream).await;
    assert_eq!(data.kind, StreamKind::Data);
    let expected = b"deferred-state\0ready"
        .iter()
        .copied()
        .map(serde_json::Value::from)
        .collect::<Vec<_>>();
    assert_eq!(data.payload, Some(serde_json::Value::Array(expected)));
    stream
        .cancel("test_complete")
        .await
        .expect("cancel probe stream");
    assert_eq!(recv_service(&mut stream).await.kind, StreamKind::Cancel);
    assert!(stream.recv().await.is_none());
    host.shutdown(Instant::now() + Duration::from_secs(2))
        .await
        .expect("probe shutdown");
}

#[tokio::test]
async fn precommit_durable_and_outbound_side_effects_are_rejected_before_ack() {
    for action in ["durable_then_ack", "outbound_open_then_ack"] {
        let temp = tempdir().expect("tempdir");
        let (host, _, _) = apply_probe_host(temp.path(), action, "none", action).await;
        tokio::task::yield_now().await;
        let connection =
            Connection::open(temp.path().join("state.sqlite3")).expect("probe state database");
        let command_ids = {
            let mut statement = connection
                .prepare("SELECT command_id FROM command_outcome ORDER BY command_id")
                .expect("command query");
            statement
                .query_map([], |row| row.get::<_, String>(0))
                .expect("command rows")
                .collect::<std::result::Result<Vec<_>, _>>()
                .expect("command ids")
        };
        assert_eq!(
            command_ids,
            [format!("{action}-apply")],
            "prepare-time durable command must not reach the registry"
        );
        assert!(
            host.open_service(ServiceOpenRequest {
                consumer: InstanceId::new("probe-client"),
                service: ServiceKey::new("fixture.lifecycle-probe"),
            })
            .is_ok(),
            "{action} must receive the precommit rejection and finish Prepared"
        );
        host.shutdown(Instant::now() + Duration::from_secs(2))
            .await
            .expect("probe shutdown");
    }
}

#[tokio::test]
async fn malformed_state_request_returns_prepare_failure_without_a_write() {
    let temp = tempdir().expect("tempdir");
    write_probe_packages(temp.path());
    let manifest = temp.path().join("rsi-meta.toml");
    let lock = temp.path().join("rsi-meta.lock");
    write_probe_composition(
        &manifest,
        "malformed_state_then_fail",
        "none",
        "malformed-state",
    );
    let host = CompositionHost::open(open_options(
        temp.path().join("state.sqlite3"),
        temp.path().join("cache"),
    ))
    .await
    .expect("host");
    assert!(matches!(
        submit(
            &host,
            "malformed-state-lock",
            Command::LockManifest {
                manifest_path: manifest.clone(),
                lock_path: lock.clone(),
            },
        )
        .await,
        CommandOutcome::LockResolved { .. }
    ));
    let rejected = tokio::time::timeout(
        Duration::from_secs(3),
        host.submit(CommandEnvelope::new(
            "malformed-state-apply",
            Command::ApplyManifestPath {
                manifest_path: manifest,
                lock_path: lock,
            },
        )),
    )
    .await
    .expect("malformed state response deadline")
    .expect("durable rejection");
    assert!(matches!(
        rejected.payload,
        CommandOutcome::Rejected { ref code, ref message }
            if code == "plugin_prepare_failed" && message.contains("malformed_state_rejected")
    ));
    let state_rows: i64 = Connection::open(temp.path().join("state.sqlite3"))
        .expect("state database")
        .query_row("SELECT COUNT(*) FROM plugin_state", [], |row| row.get(0))
        .expect("state row count");
    assert_eq!(state_rows, 0, "malformed request must not reach SQLite");
    assert_eq!(host.snapshot().graph.revision, GraphRevision(0));
    assert!(host.snapshot().active.is_none());
    host.shutdown(Instant::now() + Duration::from_secs(2))
        .await
        .expect("probe shutdown");
}

#[tokio::test]
async fn malformed_plugin_stream_events_emit_exactly_one_cancel_terminal() {
    for fault in ["wrong_service", "unknown_event", "non_byte_data"] {
        let temp = tempdir().expect("tempdir");
        let (host, _, _) = apply_probe_host(temp.path(), "normal_ack", fault, fault).await;
        let mut stream = host
            .open_service(ServiceOpenRequest {
                consumer: InstanceId::new("probe-client"),
                service: ServiceKey::new("fixture.lifecycle-probe"),
            })
            .expect("open faulty probe stream");
        assert_eq!(recv_service(&mut stream).await.kind, StreamKind::Credit);
        stream
            .grant_credit(1024 * 1024)
            .await
            .expect("grant faulty output credit");
        stream.send(b"fault").await.expect("trigger stream fault");
        let terminal = recv_service(&mut stream).await;
        assert_eq!(terminal.kind, StreamKind::Cancel, "fault={fault}");
        assert!(
            terminal
                .payload
                .as_ref()
                .and_then(|payload| payload.get("reason"))
                .and_then(serde_json::Value::as_str)
                .is_some_and(|reason| reason.contains("plugin_protocol_fault")),
            "fault={fault}, terminal={terminal:?}"
        );
        assert!(stream.recv().await.is_none(), "fault={fault}");
        host.shutdown(Instant::now() + Duration::from_secs(2))
            .await
            .expect("faulty probe shutdown");
    }
}

#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn process_fixed_terminal_replay_does_not_repeat_restart_side_effect() {
    let temp = tempdir().expect("tempdir");
    write_package(temp.path(), "provider", &["fixture.echo"], &[]);
    let package_manifest = temp.path().join("provider/plugin.toml");
    let package_source = fs::read_to_string(&package_manifest).expect("package source");
    fs::write(
        &package_manifest,
        package_source.replace(
            "version = \"1.0.0\"\n\n[host_api]",
            "version = \"1.0.0\"\nprocess_fixed = true\n\n[host_api]",
        ),
    )
    .expect("mark package process-fixed");
    let manifest_path = temp.path().join("rsi-meta.toml");
    fs::write(
        &manifest_path,
        r#"format_version = 0

[composition]
id = "process-fixed"
mode = "development"

[[scopes]]
id = "root"

[[instances]]
id = "provider"
package = "provider/plugin.toml"
scope = "root"
"#,
    )
    .expect("composition");
    let installed_lock = temp.path().join("rsi-meta.lock");
    let candidate_lock = temp.path().join("candidate.lock");
    let database = temp.path().join("state.sqlite3");
    let cache = temp.path().join("cache");

    let project = CompositionProject {
        manifest_path: manifest_path.clone(),
        lock_path: Some(installed_lock.clone()),
    };
    assert!(matches!(
        project.lock().expect("initial lock"),
        LockResult::Created { .. }
    ));
    let workspace = workspace(&database, &cache);
    assert!(matches!(
        CompositionHost::install_offline(rsi_meta::InstallRequest {
            operation_id: OperationId("process-fixed-install-v1".to_owned()),
            workspace: workspace.clone(),
            project,
        })
        .await
        .expect("initial offline install"),
        rsi_meta::InstallResult::Installed { .. }
    ));

    let host = CompositionHost::open(OpenOptions::new(workspace.clone()))
        .await
        .expect("open installed process-fixed composition");
    let mut restart_events = host
        .subscribe(host.snapshot().cursor)
        .await
        .expect("subscribe before restart boundary");
    fs::OpenOptions::new()
        .append(true)
        .open(&package_manifest)
        .expect("open package manifest")
        .write_all(b"\n# changed process-fixed artifact descriptor\n")
        .expect("change package hash");
    assert!(matches!(
        submit(
            &host,
            "process-fixed-lock-v2",
            Command::LockManifest {
                manifest_path: manifest_path.clone(),
                lock_path: candidate_lock.clone(),
            },
        )
        .await,
        CommandOutcome::LockResolved { .. }
    ));
    let apply = CommandEnvelope::new(
        "process-fixed-apply",
        Command::ApplyManifestPath {
            manifest_path: manifest_path.clone(),
            lock_path: candidate_lock.clone(),
        },
    );
    let before = host.snapshot();
    let installed_manifest_before = fs::read(&workspace.manifest_path).expect("installed manifest");
    let installed_lock_before = fs::read(&workspace.lock_path).expect("installed lock");
    let first = host.submit(apply.clone()).await.expect("restart boundary");
    assert!(matches!(first.payload, CommandOutcome::RestartRequired));
    assert_eq!(host.snapshot(), before);
    assert_eq!(
        fs::read(&workspace.manifest_path).unwrap(),
        installed_manifest_before
    );
    assert_eq!(
        fs::read(&workspace.lock_path).unwrap(),
        installed_lock_before
    );
    assert!(
        tokio::time::timeout(Duration::from_millis(100), restart_events.recv())
            .await
            .is_err(),
        "process-fixed preflight must not publish a graph event"
    );
    fs::remove_file(&manifest_path).expect("remove candidate manifest after durable result");
    fs::remove_file(&candidate_lock).expect("remove candidate lock after durable result");
    assert_eq!(
        host.submit(apply).await.expect("cached terminal replay"),
        first
    );
    let graph = host
        .submit(CommandEnvelope::new(
            "query-after-cached-restart",
            Command::QueryGraph,
        ))
        .await
        .expect("cached replay must not stop registry");
    assert!(matches!(graph.payload, CommandOutcome::Graph { .. }));
    host.shutdown(Instant::now() + Duration::from_secs(2))
        .await
        .expect("host shutdown");
}

#[tokio::test]
async fn normalized_install_does_not_mark_unchanged_process_fixed_instance_as_affected() {
    let temp = tempdir().expect("tempdir");
    write_package(temp.path(), "fixed", &[], &[]);
    write_package(temp.path(), "hot", &[], &[]);
    let fixed_manifest = temp.path().join("fixed/plugin.toml");
    fs::write(
        &fixed_manifest,
        fs::read_to_string(&fixed_manifest)
            .expect("fixed package manifest")
            .replace(
                "version = \"1.0.0\"\n\n[host_api]",
                "version = \"1.0.0\"\nprocess_fixed = true\n\n[host_api]",
            ),
    )
    .expect("mark process-fixed");
    let manifest = temp.path().join("candidate.toml");
    let source = r#"format_version = 0

[composition]
id = "normalized-fingerprints"
mode = "development"

[[scopes]]
id = "root"

[[instances]]
id = "fixed"
package = "fixed/plugin.toml"
scope = "root"

[[instances]]
id = "hot"
package = "hot/plugin.toml"
scope = "root"
enabled = true
"#;
    fs::write(&manifest, source).expect("candidate composition");
    let initial_lock = temp.path().join("candidate-v1.lock");
    CompositionProject {
        manifest_path: manifest.clone(),
        lock_path: Some(initial_lock.clone()),
    }
    .lock()
    .expect("initial lock");
    let workspace = workspace(temp.path().join("state.sqlite3"), temp.path().join("cache"));
    CompositionHost::install_offline(rsi_meta::InstallRequest {
        operation_id: OperationId("normalize-install".to_owned()),
        workspace: workspace.clone(),
        project: CompositionProject {
            manifest_path: manifest.clone(),
            lock_path: Some(initial_lock),
        },
    })
    .await
    .expect("offline install");
    let host = CompositionHost::open(OpenOptions::new(workspace))
        .await
        .expect("open normalized install");

    fs::write(
        &manifest,
        source.replace("enabled = true", "enabled = false"),
    )
    .expect("change only the hot instance");
    let changed_lock = temp.path().join("candidate-v2.lock");
    CompositionProject {
        manifest_path: manifest.clone(),
        lock_path: Some(changed_lock.clone()),
    }
    .lock()
    .expect("changed lock");
    let result = host
        .apply(ApplyRequest {
            operation_id: OperationId("normalize-apply".to_owned()),
            project: CompositionProject {
                manifest_path: manifest,
                lock_path: Some(changed_lock),
            },
            expected_revision: Some(GraphRevision(1)),
        })
        .await
        .expect("hot-only edit must remain hot-applicable");
    assert!(matches!(result, ApplyResult::Applied { .. }));

    host.shutdown(Instant::now() + Duration::from_secs(1))
        .await
        .expect("shutdown");
}

#[tokio::test]
async fn reopening_cannot_map_a_changed_process_fixed_artifact_in_the_same_process() {
    let temp = tempdir().expect("tempdir");
    write_package(temp.path(), "fixed", &[], &[]);
    let package_manifest = temp.path().join("fixed/plugin.toml");
    fs::write(
        &package_manifest,
        fs::read_to_string(&package_manifest)
            .expect("package manifest")
            .replace(
                "version = \"1.0.0\"\n\n[host_api]",
                "version = \"1.0.0\"\nprocess_fixed = true\n\n[host_api]",
            ),
    )
    .expect("mark package process fixed");
    let manifest = temp.path().join("candidate.toml");
    fs::write(
        &manifest,
        r#"format_version = 0

[composition]
id = "fresh-open"
mode = "development"

[[scopes]]
id = "root"

[[instances]]
id = "fixed"
package = "fixed/plugin.toml"
scope = "root"
"#,
    )
    .expect("composition");
    let first_lock = temp.path().join("candidate-a.lock");
    let project = CompositionProject {
        manifest_path: manifest.clone(),
        lock_path: Some(first_lock.clone()),
    };
    project.lock().expect("first lock");
    let workspace = workspace(temp.path().join("state.sqlite3"), temp.path().join("cache"));
    CompositionHost::install_offline(rsi_meta::InstallRequest {
        operation_id: OperationId("fresh-open-install".to_owned()),
        workspace: workspace.clone(),
        project,
    })
    .await
    .expect("install first artifact");
    let host = CompositionHost::open(OpenOptions::new(workspace.clone()))
        .await
        .expect("map first artifact");
    host.shutdown(Instant::now() + Duration::from_secs(1))
        .await
        .expect("shutdown first host");

    fs::OpenOptions::new()
        .append(true)
        .open(
            temp.path()
                .join(format!("fixed/artifact{}", std::env::consts::DLL_SUFFIX)),
        )
        .expect("artifact")
        .write_all(b"changed-process-fixed-bytes")
        .expect("change artifact");
    let second_lock = temp.path().join("candidate-b.lock");
    CompositionProject {
        manifest_path: manifest.clone(),
        lock_path: Some(second_lock.clone()),
    }
    .lock()
    .expect("second lock");
    fs::copy(&manifest, &workspace.manifest_path).expect("replace installed manifest externally");
    fs::copy(second_lock, &workspace.lock_path).expect("replace installed lock externally");

    let error = CompositionHost::open(OpenOptions::new(workspace))
        .await
        .expect_err("same process must reject a changed process-fixed artifact before mapping");
    assert!(matches!(
        error,
        rsi_meta::HostError::OperationRejected { ref code, .. }
            if code == "fresh_process_required"
    ));
}

#[tokio::test]
async fn read_ids_are_connection_correlation_while_mutation_ids_are_durable() {
    let temp = tempdir().expect("tempdir");
    let host = CompositionHost::open(open_options(
        temp.path().join("state.sqlite3"),
        temp.path().join("cache"),
    ))
    .await
    .expect("host");
    let _future_cursor = host
        .subscribe(u64::MAX)
        .await
        .expect("a future u64 cursor has an empty durable replay");
    host.submit(CommandEnvelope::new("same-id", Command::QueryGraph))
        .await
        .expect("first command");
    let events = host
        .submit(CommandEnvelope::new(
            "same-id",
            Command::QueryEvents {
                after_cursor: 0,
                limit: 1,
            },
        ))
        .await
        .expect("read correlation ids may be reused after completion");
    assert!(matches!(events.payload, CommandOutcome::Events { .. }));

    let rotate = CommandEnvelope::new("rotate-1", Command::RotateToken);
    let first_generation = host
        .submit(rotate.clone())
        .await
        .expect("first token rotation");
    assert!(matches!(
        first_generation.payload,
        CommandOutcome::TokenRotated { generation: 1 }
    ));
    assert_eq!(
        host.submit(rotate).await.expect("idempotent token retry"),
        first_generation
    );
    let second_generation = host
        .submit(CommandEnvelope::new("rotate-2", Command::RotateToken))
        .await
        .expect("second token rotation");
    assert!(matches!(
        second_generation.payload,
        CommandOutcome::TokenRotated { generation: 2 }
    ));
    host.shutdown(Instant::now() + Duration::from_secs(2))
        .await
        .expect("shutdown");
    let reopened = CompositionHost::open(open_options(
        temp.path().join("state.sqlite3"),
        temp.path().join("cache"),
    ))
    .await
    .expect("reopen host");
    let third_generation = reopened
        .submit(CommandEnvelope::new("rotate-3", Command::RotateToken))
        .await
        .expect("rotation after restart");
    assert!(matches!(
        third_generation.payload,
        CommandOutcome::TokenRotated { generation: 3 }
    ));
    reopened
        .shutdown(Instant::now() + Duration::from_secs(2))
        .await
        .expect("shutdown reopened host");
}

#[tokio::test]
async fn unsupported_test_adapter_commands_are_not_persisted_as_mutations() {
    let temp = tempdir().expect("tempdir");
    let host = CompositionHost::open(open_options(
        temp.path().join("state.sqlite3"),
        temp.path().join("cache"),
    ))
    .await
    .expect("host");
    let first_outcome = host
        .submit(CommandEnvelope::new("unknown", Command::Unknown))
        .await
        .expect("first outcome");
    let replayed = host
        .submit(CommandEnvelope::new("unknown", Command::Unknown))
        .await
        .expect("read-like adapter rejection may be recomputed");
    assert_eq!(first_outcome, replayed);
    assert!(matches!(
        replayed.payload,
        CommandOutcome::Rejected { ref code, .. } if code == "unknown_command"
    ));
    host.shutdown(Instant::now() + Duration::from_secs(1))
        .await
        .expect("shutdown");
}

#[tokio::test]
async fn project_lock_is_create_or_verify_and_rejects_conflicts() {
    let temp = tempdir().expect("tempdir");
    let manifest = temp.path().join("rsi-meta.toml");
    let lock = temp.path().join("candidate.lock");
    fs::write(
        &manifest,
        r#"format_version = 0
scopes = []
instances = []

[composition]
id = "empty"
mode = "development"
"#,
    )
    .expect("manifest");
    let project = CompositionProject {
        manifest_path: manifest.clone(),
        lock_path: Some(lock.clone()),
    };
    assert!(matches!(
        project.lock().unwrap(),
        LockResult::Created { .. }
    ));
    let original = fs::read(&lock).expect("created lock");
    let second = project.lock().expect("identical lock verification");
    assert!(matches!(second, LockResult::Unchanged { .. }));
    fs::write(
        &manifest,
        empty_composition_source().replace("empty", "changed"),
    )
    .expect("changed manifest");
    let conflict = project.lock().expect_err("different lock must conflict");
    assert!(matches!(
        conflict,
        rsi_meta::HostError::OperationRejected { ref code, .. } if code == "lock_conflict"
    ));
    assert_eq!(fs::read(lock).expect("unchanged lock"), original);
}

#[test]
fn project_validation_separates_diagnostics_from_io_failures() {
    let temp = tempdir().expect("tempdir");
    let manifest = temp.path().join("candidate.toml");
    fs::write(&manifest, empty_composition_source()).expect("valid candidate");
    let unlocked = CompositionProject {
        manifest_path: manifest.clone(),
        lock_path: None,
    }
    .validate()
    .expect("a valid unlocked project is diagnostic-free");
    assert!(unlocked.is_valid());

    fs::write(
        &manifest,
        "format_version = 0\n[composition]\nid = 'bad id'\n",
    )
    .expect("invalid candidate");
    let invalid = CompositionProject {
        manifest_path: manifest,
        lock_path: None,
    }
    .validate()
    .expect("invalid candidate content belongs in diagnostics");
    assert!(!invalid.is_valid());
    assert!(!invalid.diagnostics.is_empty());

    let missing = CompositionProject {
        manifest_path: temp.path().join("missing.toml"),
        lock_path: None,
    }
    .validate()
    .expect_err("missing input is an environmental failure");
    assert!(
        matches!(
            missing,
            rsi_meta::HostError::Io { .. }
                | rsi_meta::HostError::Loader(rsi_meta_loader::LoaderError::Io { .. })
        ),
        "unexpected missing-input error: {missing:?}"
    );
}

#[tokio::test]
async fn offline_install_waits_for_lease_and_activates_once_on_next_open() {
    let temp = tempdir().expect("tempdir");
    let database = temp.path().join("state.sqlite3");
    let workspace = workspace(&database, temp.path().join("cache"));
    let manifest = temp.path().join("candidate.toml");
    let lock = temp.path().join("candidate.lock");
    fs::write(&manifest, empty_composition_source()).expect("candidate manifest");
    let project = CompositionProject {
        manifest_path: manifest.clone(),
        lock_path: Some(lock.clone()),
    };
    assert!(matches!(
        project.lock().expect("candidate lock"),
        LockResult::Created { .. }
    ));

    let host = CompositionHost::open(OpenOptions::new(workspace.clone()))
        .await
        .expect("open empty workspace");
    let busy = CompositionHost::install_offline(rsi_meta::InstallRequest {
        operation_id: OperationId("offline-install-v1".to_owned()),
        workspace: workspace.clone(),
        project: project.clone(),
    })
    .await
    .expect_err("live host must own the workspace lease");
    assert!(matches!(
        busy,
        rsi_meta::HostError::OperationRejected { ref code, .. } if code == "workspace_busy"
    ));

    host.shutdown(Instant::now() + Duration::from_secs(1))
        .await
        .expect("shutdown releases workspace lease");
    let installed = CompositionHost::install_offline(rsi_meta::InstallRequest {
        operation_id: OperationId("offline-install-v1".to_owned()),
        workspace: workspace.clone(),
        project: project.clone(),
    })
    .await
    .expect("offline install after termination");
    assert!(matches!(
        installed,
        rsi_meta::InstallResult::Installed { .. }
    ));

    assert_offline_install_conflict(rsi_meta::InstallRequest {
        operation_id: OperationId("offline-install-v1".to_owned()),
        workspace: workspace.clone(),
        project: CompositionProject {
            manifest_path: temp.path().join("other.toml"),
            lock_path: Some(temp.path().join("other.lock")),
        },
    })
    .await;

    let mut alternate_workspace = workspace.clone();
    alternate_workspace.manifest_path = temp.path().join("alternate-composition.toml");
    alternate_workspace.lock_path = temp.path().join("alternate-rsi-meta.lock");
    assert_offline_install_conflict(rsi_meta::InstallRequest {
        operation_id: OperationId("offline-install-v1".to_owned()),
        workspace: alternate_workspace,
        project: project.clone(),
    })
    .await;

    fs::remove_file(&manifest).expect("remove candidate manifest");
    fs::remove_file(&lock).expect("remove candidate lock");
    assert_eq!(
        CompositionHost::install_offline(rsi_meta::InstallRequest {
            operation_id: OperationId("offline-install-v1".to_owned()),
            workspace: workspace.clone(),
            project,
        })
        .await
        .expect("terminal install replays without source files"),
        installed
    );

    assert_installed_workspace_activates_once(workspace).await;
}

#[tokio::test]
async fn offline_install_does_not_consume_an_operation_when_installed_pair_reading_fails() {
    let temp = tempdir().expect("tempdir");
    let database = temp.path().join("state.sqlite3");
    let workspace = workspace(&database, temp.path().join("cache"));
    let manifest = temp.path().join("candidate.toml");
    let lock = temp.path().join("candidate.lock");
    fs::write(&manifest, empty_composition_source()).expect("candidate manifest");
    let project = CompositionProject {
        manifest_path: manifest,
        lock_path: Some(lock),
    };
    project.lock().expect("candidate lock");
    CompositionHost::install_offline(rsi_meta::InstallRequest {
        operation_id: OperationId("seed-install".to_owned()),
        workspace: workspace.clone(),
        project: project.clone(),
    })
    .await
    .expect("seed installed pair");
    let installed_manifest = fs::read(&workspace.manifest_path).expect("installed manifest");
    fs::remove_file(&workspace.manifest_path).expect("remove installed manifest");
    fs::create_dir(&workspace.manifest_path).expect("replace manifest with unsafe directory");

    let request = rsi_meta::InstallRequest {
        operation_id: OperationId("retry-after-read-failure".to_owned()),
        workspace: workspace.clone(),
        project,
    };
    let error = CompositionHost::install_offline(request.clone())
        .await
        .expect_err("unsafe installed input must fail before operation reservation");
    assert!(matches!(
        error,
        rsi_meta::HostError::Loader(rsi_meta_loader::LoaderError::UnsafeInputFile { .. })
    ));

    fs::remove_dir(&workspace.manifest_path).expect("remove unsafe directory");
    fs::write(&workspace.manifest_path, installed_manifest).expect("restore installed manifest");
    assert!(matches!(
        CompositionHost::install_offline(request)
            .await
            .expect("same operation remains usable after transient read failure"),
        rsi_meta::InstallResult::Unchanged { .. }
    ));
}

fn composition_commit_count(events: &[HostEventRecord]) -> usize {
    events
        .iter()
        .filter(|record| {
            matches!(
                record.event,
                rsi_meta::HostEvent::CompositionCommitted { .. }
            )
        })
        .count()
}

#[cfg(unix)]
#[tokio::test]
async fn lock_manifest_deduplicates_package_paths_after_canonicalization() {
    use std::os::unix::fs::symlink;

    let temp = tempdir().expect("tempdir");
    write_package(temp.path(), "provider", &["fixture.echo"], &[]);
    symlink(
        temp.path().join("provider"),
        temp.path().join("provider-alias"),
    )
    .expect("package directory alias");
    let manifest = temp.path().join("rsi-meta.toml");
    fs::write(
        &manifest,
        r#"format_version = 0

[composition]
id = "canonical-packages"
mode = "development"

[[scopes]]
id = "root"

[[instances]]
id = "first"
package = "provider/plugin.toml"
scope = "root"

[[instances]]
id = "second"
package = "provider-alias/plugin.toml"
scope = "root"
"#,
    )
    .expect("composition");
    let host = CompositionHost::open(open_options(
        temp.path().join("state.sqlite3"),
        temp.path().join("cache"),
    ))
    .await
    .expect("host");

    let lock_path = temp.path().join("candidate.lock");
    let outcome = submit(
        &host,
        "canonical-package-lock",
        Command::LockManifest {
            manifest_path: manifest.clone(),
            lock_path: lock_path.clone(),
        },
    )
    .await;
    match outcome {
        CommandOutcome::LockResolved { lock } => assert_eq!(lock.packages.len(), 1),
        other => panic!("unexpected lock outcome: {other:?}"),
    }

    let report = CompositionProject {
        manifest_path: manifest,
        lock_path: Some(lock_path),
    }
    .validate()
    .expect("validation I/O");
    assert!(
        report.is_valid(),
        "a lock created for a project must validate that same project: {:?}",
        report.diagnostics
    );

    host.shutdown(Instant::now() + Duration::from_secs(1))
        .await
        .expect("shutdown");
}

#[tokio::test]
#[allow(clippy::too_many_lines)] // the v4 fixture spells out the durable foreign-key shape
async fn legacy_v4_aborted_apply_does_not_block_migration() {
    let temp = tempdir().expect("tempdir");
    let database = temp.path().join("legacy-v4.sqlite3");
    let connection = Connection::open(&database).expect("legacy database");
    connection
        .execute_batch(
            r#"
            PRAGMA foreign_keys = ON;
            CREATE TABLE store_meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);
            INSERT INTO store_meta VALUES ('schema_version', '4');
            CREATE TABLE plugin_state (
                composition_id TEXT NOT NULL, instance_id TEXT NOT NULL,
                state_key TEXT NOT NULL, version INTEGER NOT NULL,
                value_json TEXT, tombstone INTEGER NOT NULL,
                updated_at TEXT NOT NULL DEFAULT '',
                PRIMARY KEY (composition_id, instance_id, state_key)
            );
            CREATE TABLE control_event (
                cursor INTEGER PRIMARY KEY AUTOINCREMENT,
                schema_version INTEGER NOT NULL DEFAULT 0,
                composition_id TEXT NOT NULL, command_id TEXT,
                graph_revision INTEGER NOT NULL, event_json TEXT NOT NULL,
                created_at TEXT NOT NULL DEFAULT ''
            );
            CREATE TABLE command_outcome (
                command_id TEXT PRIMARY KEY, schema_version INTEGER NOT NULL DEFAULT 0,
                composition_id TEXT NOT NULL, request_hash BLOB NOT NULL,
                status TEXT NOT NULL, outcome_json TEXT,
                created_at TEXT NOT NULL DEFAULT '', pending_kind TEXT, pending_json TEXT
            );
            CREATE TABLE apply_journal (
                command_id TEXT PRIMARY KEY REFERENCES command_outcome(command_id),
                composition_id TEXT NOT NULL,
                installed_manifest_path TEXT NOT NULL, installed_lock_path TEXT NOT NULL,
                candidate_manifest_path TEXT NOT NULL, candidate_lock_path TEXT NOT NULL,
                candidate_manifest_hash TEXT NOT NULL, candidate_lock_hash TEXT NOT NULL,
                state TEXT NOT NULL, updated_at TEXT NOT NULL DEFAULT '',
                previous_manifest_bytes BLOB, previous_lock_bytes BLOB,
                previous_manifest_hash TEXT, previous_lock_hash TEXT,
                terminal_graph_revision INTEGER, terminal_event_json TEXT,
                terminal_outcome_json TEXT, terminal_desired_json TEXT
            );
            INSERT INTO command_outcome(
                command_id, schema_version, composition_id, request_hash, status, outcome_json
            ) VALUES (
                'aborted-apply', 4, 'legacy', X'01', 'terminal',
                '{"protocol":"rsi-meta.control","version":0,"kind":"result","command_id":"aborted-apply","graph_revision":0,"payload":{"type":"rejected","code":"apply_prepare_failed","message":"fixture"}}'
            );
            INSERT INTO apply_journal(
                command_id, composition_id,
                installed_manifest_path, installed_lock_path,
                candidate_manifest_path, candidate_lock_path,
                candidate_manifest_hash, candidate_lock_hash, state
            ) VALUES (
                'aborted-apply', 'legacy', 'installed.toml', 'installed.lock',
                'candidate.toml', 'candidate.lock', 'aa', 'bb', 'aborted'
            );
            "#,
        )
        .expect("legacy v4 schema");
    drop(connection);

    let host = CompositionHost::open(open_options(&database, temp.path().join("cache")))
        .await
        .expect("v4 aborted apply must migrate without violating its journal foreign key");
    host.shutdown(Instant::now() + Duration::from_secs(1))
        .await
        .expect("shutdown");
}

#[tokio::test]
async fn staging_infrastructure_failure_does_not_consume_the_operation_id() {
    let temp = tempdir().expect("tempdir");
    write_package(temp.path(), "provider", &["fixture.echo"], &[]);
    let manifest = temp.path().join("candidate.toml");
    let lock = temp.path().join("candidate.lock");
    fs::write(
        &manifest,
        r#"format_version = 0

[composition]
id = "retry-stage"
mode = "development"

[[scopes]]
id = "root"

[[instances]]
id = "provider"
package = "provider/plugin.toml"
scope = "root"
"#,
    )
    .expect("candidate manifest");
    CompositionProject {
        manifest_path: manifest.clone(),
        lock_path: Some(lock.clone()),
    }
    .lock()
    .expect("candidate lock");

    let cache = temp.path().join("cache");
    fs::write(&cache, b"not a directory").expect("blocked cache root");
    let host = CompositionHost::open(open_options(temp.path().join("state.sqlite3"), &cache))
        .await
        .expect("empty host does not stage plugins");
    let request = ApplyRequest {
        operation_id: OperationId("retry-stage-operation".to_owned()),
        project: CompositionProject {
            manifest_path: manifest,
            lock_path: Some(lock),
        },
        expected_revision: Some(GraphRevision(0)),
    };
    host.apply(request.clone())
        .await
        .expect_err("cache filesystem failure must escape as infrastructure error");

    fs::remove_file(&cache).expect("remove cache blocker");
    assert!(matches!(
        host.apply(request)
            .await
            .expect("the same operation ID remains retryable after repair"),
        ApplyResult::Applied { .. }
    ));
    host.shutdown(Instant::now() + Duration::from_secs(2))
        .await
        .expect("shutdown");
}

#[tokio::test]
async fn lock_manifest_rejects_an_oversized_composition_document() {
    let temp = tempdir().expect("tempdir");
    let manifest = temp.path().join("oversized.toml");
    let mut source = empty_composition_source().to_owned();
    source.push_str(&"# padding\n".repeat(500_000));
    fs::write(&manifest, source).expect("oversized but valid TOML composition");
    let outcome = CompositionProject {
        manifest_path: manifest,
        lock_path: Some(temp.path().join("oversized.lock")),
    }
    .lock()
    .expect_err("oversized candidate must fail before lock creation");
    assert!(
        outcome.to_string().contains("exceeds"),
        "oversized document must be rejected without being buffered: {outcome:?}"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn lock_manifest_bounds_bytes_from_the_target_of_a_composition_symlink() {
    use std::os::unix::fs::symlink;

    let temp = tempdir().expect("tempdir");
    let target = temp.path().join("target.toml");
    let manifest = temp.path().join("composition-link.toml");
    fs::write(&target, empty_composition_source()).expect("target composition");
    symlink(&target, &manifest).expect("composition symlink");
    let host = CompositionHost::open(open_options(
        temp.path().join("state.sqlite3"),
        temp.path().join("cache"),
    ))
    .await
    .expect("host");

    let outcome = submit(
        &host,
        "symlinked-composition",
        Command::LockManifest {
            manifest_path: manifest,
            lock_path: temp.path().join("candidate.lock"),
        },
    )
    .await;
    assert!(matches!(outcome, CommandOutcome::LockResolved { .. }));

    host.shutdown(Instant::now() + Duration::from_secs(1))
        .await
        .expect("shutdown");
}

#[tokio::test]
#[allow(clippy::too_many_lines)] // one end-to-end disk cutover and reopen scenario
async fn config_schema_bytes_are_lock_truth_and_force_a_real_cutover() {
    let temp = tempdir().expect("tempdir");
    write_package(temp.path(), "provider", &["fixture.echo"], &[]);
    let manifest = temp.path().join("rsi-meta.toml");
    let installed_lock = temp.path().join("rsi-meta.lock");
    let candidate_lock = temp.path().join("schema-candidate.lock");
    fs::write(
        &manifest,
        r#"format_version = 0

[composition]
id = "schema-lock"
mode = "development"

[[scopes]]
id = "root"

[[instances]]
id = "provider"
package = "provider/plugin.toml"
scope = "root"
"#,
    )
    .expect("composition");
    let package_manifest =
        fs::read(temp.path().join("provider/plugin.toml")).expect("package manifest bytes");
    let artifact = fs::read(
        temp.path()
            .join(format!("provider/artifact{}", std::env::consts::DLL_SUFFIX)),
    )
    .expect("artifact bytes");
    let host = CompositionHost::open(open_options(
        temp.path().join("state.sqlite3"),
        temp.path().join("cache"),
    ))
    .await
    .expect("host");
    let first_lock = match submit(
        &host,
        "schema-lock-v1",
        Command::LockManifest {
            manifest_path: manifest.clone(),
            lock_path: installed_lock.clone(),
        },
    )
    .await
    {
        CommandOutcome::LockResolved { lock } => lock,
        other => panic!("unexpected lock outcome: {other:?}"),
    };
    assert!(first_lock.packages[0].config_schema_sha256.is_some());
    let first = submit(
        &host,
        "schema-apply-v1",
        Command::ApplyManifestPath {
            manifest_path: manifest.clone(),
            lock_path: installed_lock.clone(),
        },
    )
    .await;
    let first_revision = match first {
        CommandOutcome::Applied { graph } => graph.revision,
        other => panic!("unexpected apply outcome: {other:?}"),
    };

    fs::write(
        temp.path().join("provider/config.schema.json"),
        r#"{
          "$schema":"https://json-schema.org/draft/2020-12/schema",
          "$comment":"compatible schema-only drift",
          "type":"object",
          "additionalProperties":false
        }"#,
    )
    .expect("replace only schema bytes");
    let second_lock = match submit(
        &host,
        "schema-lock-v2",
        Command::LockManifest {
            manifest_path: manifest.clone(),
            lock_path: candidate_lock.clone(),
        },
    )
    .await
    {
        CommandOutcome::LockResolved { lock } => lock,
        other => panic!("unexpected candidate lock outcome: {other:?}"),
    };
    assert_ne!(
        second_lock.packages[0].config_schema_sha256,
        first_lock.packages[0].config_schema_sha256
    );
    let second = submit(
        &host,
        "schema-apply-v2",
        Command::ApplyManifestPath {
            manifest_path: manifest.clone(),
            lock_path: candidate_lock,
        },
    )
    .await;
    assert!(matches!(
        second,
        CommandOutcome::Applied { ref graph } if graph.revision > first_revision
    ));
    assert_eq!(
        fs::read(temp.path().join("provider/plugin.toml")).expect("unchanged package manifest"),
        package_manifest
    );
    assert_eq!(
        fs::read(
            temp.path()
                .join(format!("provider/artifact{}", std::env::consts::DLL_SUFFIX))
        )
        .expect("unchanged artifact"),
        artifact
    );
    let installed: CompositionLock = toml::from_str(
        &fs::read_to_string(temp.path().join("installed.lock")).expect("installed lock source"),
    )
    .expect("installed lock");
    assert_eq!(
        installed.packages[0].config_schema_sha256,
        second_lock.packages[0].config_schema_sha256
    );
    host.shutdown(Instant::now() + Duration::from_secs(2))
        .await
        .expect("shutdown");
    let reopened = CompositionHost::open(
        open_options(
            temp.path().join("state.sqlite3"),
            temp.path().join("cache-reopen"),
        )
        .with_composition(&manifest, &installed_lock),
    )
    .await
    .expect("schema-pinned composition reopens");
    assert!(reopened.snapshot().graph.revision > first_revision);
    reopened
        .shutdown(Instant::now() + Duration::from_secs(2))
        .await
        .expect("shutdown reopened host");
}

#[tokio::test]
async fn failed_apply_commit_restores_installed_pair_before_returning() {
    let temp = tempdir().expect("tempdir");
    let (host, installed_manifest, installed_lock) =
        apply_probe_host(temp.path(), "normal_ack", "none", "installed").await;
    let installed_manifest_bytes = fs::read(&installed_manifest).expect("installed manifest");
    let installed_lock_bytes = fs::read(&installed_lock).expect("installed lock");

    let candidate_manifest = temp.path().join("candidate.toml");
    let candidate_lock = temp.path().join("candidate.lock");
    write_probe_composition(&candidate_manifest, "normal_ack", "none", "candidate");
    assert!(matches!(
        submit(
            &host,
            "candidate-lock",
            Command::LockManifest {
                manifest_path: candidate_manifest.clone(),
                lock_path: candidate_lock.clone(),
            },
        )
        .await,
        CommandOutcome::LockResolved { .. }
    ));

    let connection = Connection::open(temp.path().join("state.sqlite3")).expect("open store");
    connection
        .execute_batch(
            "CREATE TRIGGER reject_candidate_commit
             BEFORE INSERT ON control_event
             WHEN NEW.command_id = 'candidate-apply'
             BEGIN
               SELECT RAISE(FAIL, 'injected apply commit failure');
             END;",
        )
        .expect("install commit failure trigger");

    let outcome = host
        .submit(CommandEnvelope::new(
            "candidate-apply",
            Command::ApplyManifestPath {
                manifest_path: candidate_manifest,
                lock_path: candidate_lock,
            },
        ))
        .await
        .expect("commit failure is returned as a durable rejection");
    assert!(matches!(
        outcome.payload,
        CommandOutcome::Rejected { ref code, ref message }
            if code == "apply_commit_failed"
                && message.contains("injected apply commit failure")
    ));
    assert_eq!(
        fs::read(&installed_manifest).expect("restored installed manifest"),
        installed_manifest_bytes
    );
    assert_eq!(
        fs::read(&installed_lock).expect("restored installed lock"),
        installed_lock_bytes
    );

    connection
        .execute_batch("DROP TRIGGER reject_candidate_commit;")
        .expect("remove commit failure trigger");
    host.shutdown(Instant::now() + Duration::from_secs(2))
        .await
        .expect("shutdown after restored apply");
}

#[tokio::test]
#[allow(clippy::too_many_lines)] // the legacy schema fixture is intentionally explicit SQL
async fn legacy_v0_store_migrates_transactionally_and_preserves_state() {
    let temp = tempdir().expect("tempdir");
    let database = temp.path().join("legacy.sqlite3");
    let connection = Connection::open(&database).expect("legacy database");
    connection
        .execute_batch(
            r#"
            CREATE TABLE store_meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);
            INSERT INTO store_meta VALUES ('schema_version', '0');
            CREATE TABLE plugin_state (
                composition_id TEXT NOT NULL, instance_id TEXT NOT NULL,
                state_key TEXT NOT NULL, version INTEGER NOT NULL,
                value_json TEXT, tombstone INTEGER NOT NULL,
                updated_at TEXT NOT NULL DEFAULT '',
                PRIMARY KEY (composition_id, instance_id, state_key)
            );
            INSERT INTO plugin_state VALUES
                ('legacy', 'counter', 'value', 7, '{"count":7}', 0, 'before');
            CREATE TABLE control_event (
                cursor INTEGER PRIMARY KEY AUTOINCREMENT,
                schema_version INTEGER NOT NULL DEFAULT 0,
                composition_id TEXT NOT NULL,
                graph_revision INTEGER NOT NULL,
                event_json TEXT NOT NULL,
                created_at TEXT NOT NULL DEFAULT ''
            );
            CREATE TABLE command_outcome (
                command_id TEXT PRIMARY KEY, schema_version INTEGER NOT NULL DEFAULT 0,
                composition_id TEXT NOT NULL, request_hash BLOB NOT NULL,
                status TEXT NOT NULL, outcome_json TEXT, created_at TEXT NOT NULL DEFAULT ''
            );
            CREATE TABLE apply_journal (
                command_id TEXT PRIMARY KEY, composition_id TEXT NOT NULL,
                installed_manifest_path TEXT NOT NULL, installed_lock_path TEXT NOT NULL,
                candidate_manifest_path TEXT NOT NULL, candidate_lock_path TEXT NOT NULL,
                candidate_manifest_hash TEXT NOT NULL, candidate_lock_hash TEXT NOT NULL,
                state TEXT NOT NULL, updated_at TEXT NOT NULL DEFAULT ''
            );
            INSERT INTO control_event(
                cursor, schema_version, composition_id, graph_revision, event_json, created_at
            ) VALUES (
                1, 0, 'legacy', 0,
                '{"protocol":"rsi-meta.control","version":0,"kind":"event","cursor":1,"graph_revision":0,"payload":{"type":"composition_committed","source":"apply","composition_id":"legacy","manifest_sha256":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","lock_sha256":"bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb","active_instances":0,"inactive_instances":0}}',
                'before'
            );
            "#,
        )
        .expect("legacy schema");
    drop(connection);

    let host = CompositionHost::open(open_options(&database, temp.path().join("cache")))
        .await
        .expect("v0 migration through public open");
    let queried = submit(
        &host,
        "legacy-events-query",
        Command::QueryEvents {
            after_cursor: 0,
            limit: 10,
        },
    )
    .await;
    assert!(matches!(
        queried,
        CommandOutcome::Events { ref events }
            if events.len() == 1
                && events[0].operation_id.as_ref().map(|id| id.0.as_str())
                    == Some("system/legacy/1")
    ));
    let mut subscribed = host.subscribe(0).await.expect("subscribe migrated events");
    let replayed = subscribed
        .recv()
        .await
        .expect("migrated event replay")
        .expect("valid migrated event");
    assert_eq!(
        replayed.operation_id.as_ref().map(|id| id.0.as_str()),
        Some("system/legacy/1")
    );
    host.shutdown(Instant::now() + Duration::from_secs(1))
        .await
        .expect("shutdown");

    let connection = Connection::open(&database).expect("migrated database");
    let version: String = connection
        .query_row(
            "SELECT value FROM store_meta WHERE key = 'schema_version'",
            [],
            |row| row.get(0),
        )
        .expect("schema version");
    assert_eq!(version, "5");
    let migrated_event: (String, String) = connection
        .query_row(
            "SELECT command_id, event_json FROM control_event WHERE cursor = 1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .expect("migrated event row");
    assert_eq!(migrated_event.0, "system/legacy/1");
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&migrated_event.1).expect("migrated event JSON")
            ["command_id"],
        "system/legacy/1"
    );
    let state: (i64, String) = connection
        .query_row(
            "SELECT version, value_json FROM plugin_state
             WHERE composition_id='legacy' AND instance_id='counter' AND state_key='value'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .expect("legacy state");
    assert_eq!(state, (7, r#"{"count":7}"#.to_owned()));
    drop(connection);
    let reopened = CompositionHost::open(open_options(&database, temp.path().join("cache-reopen")))
        .await
        .expect("migrated store reopens");
    reopened
        .shutdown(Instant::now() + Duration::from_secs(1))
        .await
        .expect("reopened shutdown");
}

#[tokio::test]
async fn future_store_schema_is_rejected_without_mutation() {
    let temp = tempdir().expect("tempdir");
    let database = temp.path().join("future.sqlite3");
    let connection = Connection::open(&database).expect("future database");
    connection
        .execute_batch(
            "CREATE TABLE store_meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);
             INSERT INTO store_meta VALUES ('schema_version', '999');",
        )
        .expect("future marker");
    drop(connection);
    let before = fs::read(&database).expect("future database bytes");
    let error = CompositionHost::open(open_options(&database, temp.path().join("cache")))
        .await
        .expect_err("future schema must fail closed");
    assert!(matches!(
        error,
        rsi_meta::HostError::UnsupportedStoreSchema {
            found: 999,
            supported: 5
        }
    ));
    assert_eq!(
        fs::read(database).expect("unchanged future database"),
        before
    );
}

#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn candidate_from_another_directory_installs_relocatable_package_paths() {
    let temp = tempdir().expect("tempdir");
    let packages = temp.path().join("packages");
    write_package(&packages, "provider", &["fixture.echo"], &[]);
    let installed = temp.path().join("installed");
    let candidate = temp.path().join("candidates/deep");
    fs::create_dir_all(&installed).expect("installed directory");
    fs::create_dir_all(&candidate).expect("candidate directory");
    let installed_manifest = installed.join("rsi-meta.toml");
    let installed_lock = installed.join("rsi-meta.lock");
    let candidate_manifest = candidate.join("rsi-meta.toml");
    let candidate_lock = candidate.join("candidate.lock");
    let manifest = |package: &str| {
        format!(
            r#"format_version = 0

[composition]
id = "relocatable"
mode = "development"

[[scopes]]
id = "root"

[[instances]]
id = "provider"
package = "{package}"
scope = "root"
"#,
        )
    };
    fs::write(
        &installed_manifest,
        manifest("../packages/provider/plugin.toml"),
    )
    .expect("installed manifest");
    fs::write(
        &candidate_manifest,
        manifest("../../packages/provider/plugin.toml"),
    )
    .expect("candidate manifest");

    let database = temp.path().join("state.sqlite3");
    let cache = temp.path().join("cache");
    let bootstrap = CompositionHost::open(open_options(&database, &cache))
        .await
        .expect("bootstrap host");
    assert!(matches!(
        submit(
            &bootstrap,
            "initial-lock",
            Command::LockManifest {
                manifest_path: installed_manifest.clone(),
                lock_path: installed_lock.clone(),
            },
        )
        .await,
        CommandOutcome::LockResolved { .. }
    ));
    bootstrap
        .shutdown(Instant::now() + Duration::from_secs(1))
        .await
        .expect("bootstrap shutdown");

    let host = CompositionHost::open(
        open_options(&database, &cache).with_composition(&installed_manifest, &installed_lock),
    )
    .await
    .expect("installed host");
    assert!(matches!(
        submit(
            &host,
            "candidate-lock",
            Command::LockManifest {
                manifest_path: candidate_manifest.clone(),
                lock_path: candidate_lock.clone(),
            },
        )
        .await,
        CommandOutcome::LockResolved { .. }
    ));
    assert!(matches!(
        submit(
            &host,
            "relocate-apply",
            Command::ApplyManifestPath {
                manifest_path: candidate_manifest,
                lock_path: candidate_lock,
            },
        )
        .await,
        CommandOutcome::Applied { .. }
    ));
    let canonical_package =
        fs::canonicalize(packages.join("provider/plugin.toml")).expect("canonical package path");
    let installed_source =
        fs::read_to_string(temp.path().join("composition.toml")).expect("installed source");
    assert!(installed_source.contains(canonical_package.to_str().expect("UTF-8 path")));
    host.shutdown(Instant::now() + Duration::from_secs(2))
        .await
        .expect("shutdown after relocation");

    let reopened = CompositionHost::open(
        open_options(&database, temp.path().join("cache-reopen"))
            .with_composition(&installed_manifest, &installed_lock),
    )
    .await
    .expect("normalized installed pair reopens");
    assert_eq!(reopened.snapshot().graph.composition_id, "relocatable");
    reopened
        .shutdown(Instant::now() + Duration::from_secs(2))
        .await
        .expect("reopened shutdown");
}

#[tokio::test]
async fn mixed_pair_pending_recovery_restores_both_previous_files() {
    let temp = tempdir().expect("tempdir");
    let database = temp.path().join("state.sqlite3");
    let bootstrap =
        CompositionHost::open(open_options(&database, temp.path().join("cache-bootstrap")))
            .await
            .expect("bootstrap host");
    bootstrap
        .shutdown(Instant::now() + Duration::from_secs(1))
        .await
        .expect("bootstrap shutdown");

    let installed_manifest = temp.path().join("installed.toml");
    let installed_lock = temp.path().join("installed.lock");
    let previous_manifest = b"previous manifest".to_vec();
    let previous_lock = b"previous lock".to_vec();
    let candidate_manifest = b"candidate manifest".to_vec();
    let candidate_lock = b"candidate lock".to_vec();
    // Crash image from restoring manifest first while the candidate lock
    // commit marker is still present.
    fs::write(&installed_manifest, &previous_manifest).expect("mixed manifest");
    fs::write(&installed_lock, &candidate_lock).expect("mixed lock");
    let command_id = "mixed-recovery";
    let terminal_outcome = serde_json::json!({
        "protocol": "rsi-meta.control",
        "version": 0,
        "kind": "result",
        "command_id": command_id,
        "graph_revision": 0,
        "payload": {
            "type": "rejected",
            "code": "unused",
            "message": "recovery replaces this terminal"
        }
    });
    let terminal_desired = DesiredState {
        manifest_sha256: Some(ContentHash::digest(&candidate_manifest).to_string()),
        lock_sha256: Some(ContentHash::digest(&candidate_lock).to_string()),
        applied: true,
        last_rejection_code: None,
        plugin_restart_requested: false,
    };
    let connection = Connection::open(&database).expect("recovery database");
    connection
        .execute(
            "INSERT INTO command_outcome(
               command_id,schema_version,composition_id,request_hash,status,outcome_json
             ) VALUES (?1,2,'',X'01','pending',NULL)",
            [command_id],
        )
        .expect("pending command");
    connection
        .execute(
            "INSERT INTO apply_journal(
               command_id,composition_id,installed_manifest_path,installed_lock_path,
               candidate_manifest_path,candidate_lock_path,
               candidate_manifest_hash,candidate_lock_hash,
               previous_manifest_bytes,previous_lock_bytes,
               previous_manifest_hash,previous_lock_hash,
               terminal_graph_revision,terminal_event_json,
               terminal_outcome_json,terminal_desired_json,state
             ) VALUES (?1,'',?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,0,?12,?13,?14,'pending')",
            rusqlite::params![
                command_id,
                installed_manifest.to_str().expect("manifest path"),
                installed_lock.to_str().expect("lock path"),
                temp.path()
                    .join("candidate.toml")
                    .to_str()
                    .expect("candidate path"),
                temp.path()
                    .join("candidate.lock")
                    .to_str()
                    .expect("candidate lock path"),
                ContentHash::digest(&candidate_manifest).to_string(),
                ContentHash::digest(&candidate_lock).to_string(),
                previous_manifest,
                previous_lock,
                ContentHash::digest(b"previous manifest").to_string(),
                ContentHash::digest(b"previous lock").to_string(),
                serde_json::to_string(&serde_json::json!({"type": "host_shutting_down"}))
                    .expect("event JSON"),
                serde_json::to_string(&terminal_outcome).expect("outcome JSON"),
                serde_json::to_string(&terminal_desired).expect("desired JSON"),
            ],
        )
        .expect("pending journal");
    drop(connection);

    let error = CompositionHost::open(OpenOptions::new(CompositionWorkspace {
        database_path: database,
        cache_root: temp.path().join("cache-recovered"),
        manifest_path: installed_manifest.clone(),
        lock_path: installed_lock.clone(),
    }))
    .await
    .expect_err("restored test bytes are intentionally not a valid composition");
    assert!(error.to_string().contains("TOML") || error.to_string().contains("manifest"));
    assert_eq!(
        fs::read(&installed_manifest).expect("manifest"),
        b"previous manifest"
    );
    assert_eq!(fs::read(&installed_lock).expect("lock"), b"previous lock");
}
