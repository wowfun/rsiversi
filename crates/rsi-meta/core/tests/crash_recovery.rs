#![cfg(all(unix, feature = "test-failpoints"))]

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command as ProcessCommand, Stdio};
use std::sync::OnceLock;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use rsi_meta::{
    ApplyRequest, ApplyResult, CompositionHost, CompositionProject, CompositionWorkspace,
    GraphSnapshot, InstallRequest, InstallResult, LockResult, OpenOptions, OperationId,
};
use rsi_meta_loader::{ApiVersion, BUILD_TARGET};
use serde::{Deserialize, Serialize};
use tempfile::{TempDir, tempdir};
use tokio::io::AsyncReadExt;

const CHILD_ENV: &str = "RSI_META_CORE_CRASH_CHILD";
const INSTALL_CHILD_ENV: &str = "RSI_META_CORE_INSTALL_CRASH_CHILD";
const GATE_ENV: &str = "RSI_META_CORE_TEST_CRASH_GATE";

#[derive(Clone, Debug)]
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
}

#[derive(Clone, Debug)]
struct CommandEnvelope {
    command_id: String,
    payload: Command,
}

impl CommandEnvelope {
    fn new(command_id: impl Into<String>, payload: Command) -> Self {
        Self {
            command_id: command_id.into(),
            payload,
        }
    }
}

#[derive(Clone, Debug)]
enum CommandOutcome {
    Applied {},
    LockResolved {},
    Graph { _graph: GraphSnapshot },
    Rejected { code: String, _message: String },
}

#[derive(Clone, Debug)]
struct CommandOutcomeEnvelope {
    payload: CommandOutcome,
}

fn workspace(database: impl AsRef<Path>, cache: impl AsRef<Path>) -> CompositionWorkspace {
    let database_path = database.as_ref().to_owned();
    let root = database_path.parent().expect("database parent").to_owned();
    CompositionWorkspace {
        database_path,
        cache_root: cache.as_ref().to_owned(),
        manifest_path: root.join("composition.toml"),
        lock_path: root.join("rsi-meta.lock"),
    }
}

fn open_options(database: impl AsRef<Path>, cache: impl AsRef<Path>) -> OpenOptions {
    OpenOptions::new(workspace(database, cache))
}

trait TestOpenOptionsExt {
    fn with_composition(self, manifest: impl AsRef<Path>, lock: impl AsRef<Path>) -> Self;
}

impl TestOpenOptionsExt for OpenOptions {
    fn with_composition(mut self, manifest: impl AsRef<Path>, lock: impl AsRef<Path>) -> Self {
        manifest
            .as_ref()
            .clone_into(&mut self.workspace.manifest_path);
        lock.as_ref().clone_into(&mut self.workspace.lock_path);
        self
    }
}

trait TestHostExt {
    async fn submit(&self, command: CommandEnvelope) -> rsi_meta::Result<CommandOutcomeEnvelope>;
    async fn shutdown(&self, deadline: Instant) -> rsi_meta::Result<()>;
}

impl TestHostExt for CompositionHost {
    async fn submit(&self, command: CommandEnvelope) -> rsi_meta::Result<CommandOutcomeEnvelope> {
        let operation_id = OperationId(command.command_id);
        let result = match command.payload {
            Command::ApplyManifestPath {
                manifest_path,
                lock_path,
            } => self
                .apply(ApplyRequest {
                    operation_id,
                    project: CompositionProject {
                        manifest_path,
                        lock_path: Some(lock_path),
                    },
                    expected_revision: None,
                })
                .await
                .map(|result| match result {
                    ApplyResult::Applied { .. } | ApplyResult::Unchanged { .. } => {
                        CommandOutcome::Applied {}
                    }
                    ApplyResult::RestartRequired { .. } => CommandOutcome::Rejected {
                        code: "restart_required".to_owned(),
                        _message: "crash fixture did not expect process-fixed input".to_owned(),
                    },
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
                LockResult::Created { .. } | LockResult::Unchanged { .. } => {
                    CommandOutcome::LockResolved {}
                }
            }),
            Command::QueryGraph => Ok(CommandOutcome::Graph {
                _graph: self.snapshot().graph,
            }),
        };
        match result {
            Ok(payload) => Ok(CommandOutcomeEnvelope { payload }),
            Err(rsi_meta::HostError::OperationRejected { code, message, .. }) => {
                Ok(CommandOutcomeEnvelope {
                    payload: CommandOutcome::Rejected {
                        code,
                        _message: message,
                    },
                })
            }
            Err(error) => Err(error),
        }
    }

    async fn shutdown(&self, deadline: Instant) -> rsi_meta::Result<()> {
        static NEXT_SHUTDOWN: AtomicU64 = AtomicU64::new(1);
        let sequence = NEXT_SHUTDOWN.fetch_add(1, Ordering::Relaxed);
        self.request_shutdown(OperationId(format!("crash-test-shutdown-{sequence}")))
            .await?;
        self.wait_terminated(deadline).await
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct ChildConfig {
    database: PathBuf,
    cache: PathBuf,
    installed_manifest: PathBuf,
    installed_lock: PathBuf,
    candidate_manifest: PathBuf,
    candidate_lock: PathBuf,
    command_id: String,
}

#[derive(Debug)]
struct BuiltEcho {
    _target: TempDir,
    library: PathBuf,
}

fn build_echo() -> &'static BuiltEcho {
    static ECHO: OnceLock<BuiltEcho> = OnceLock::new();
    ECHO.get_or_init(|| {
        let target = tempdir().expect("fixture target");
        let repository = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .and_then(Path::parent)
            .and_then(Path::parent)
            .expect("repository root");
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
            .arg(repository.join("fixtures/rsi-meta/echo-bidi/Cargo.toml"))
            .env("CARGO_TARGET_DIR", target.path())
            .status()
            .expect("build echo fixture");
        assert!(status.success());
        let library = target
            .path()
            .join(BUILD_TARGET)
            .join("release")
            .join(format!(
                "{}rsi_meta_fixture_echo_bidi{}",
                std::env::consts::DLL_PREFIX,
                std::env::consts::DLL_SUFFIX
            ));
        assert!(library.is_file(), "echo cdylib missing");
        BuiltEcho {
            _target: target,
            library,
        }
    })
}

fn write_package(root: &Path, directory: &str, package_id: &str, provides: bool, injects: bool) {
    let package = root.join(directory);
    fs::create_dir_all(&package).expect("package directory");
    let artifact = format!("artifact{}", std::env::consts::DLL_SUFFIX);
    fs::copy(&build_echo().library, package.join(&artifact)).expect("copy echo artifact");
    fs::write(
        package.join("config.schema.json"),
        r#"{"$schema":"https://json-schema.org/draft/2020-12/schema","type":"object","additionalProperties":false}"#,
    )
    .expect("config schema");
    let api = ApiVersion::CURRENT;
    fs::write(
        package.join("plugin.toml"),
        format!(
            "format_version = 0\nprovides = [{}]\nconfig_schema = \"config.schema.json\"\n\n[package]\nid = \"{package_id}\"\nversion = \"0.0.1\"\n\n[host_api]\nmajor = {}\nminimum_minor = {}\n\n[[artifacts]]\ntarget = \"{}\"\npath = \"{artifact}\"\n\n{}",
            if provides { "\"fixture.echo\"" } else { "" },
            api.major,
            api.minor,
            BUILD_TARGET,
            if injects {
                "[[injects]]\ncontract = \"fixture.echo\"\nrequired = true\n"
            } else {
                ""
            }
        ),
    )
    .expect("plugin manifest");
}

fn write_composition(path: &Path, packages: &Path) {
    fs::write(
        path,
        format!(
            r#"format_version = 0

[composition]
id = "crash-recovery"
mode = "development"

[[scopes]]
id = "root"

[[instances]]
id = "provider"
package = "{}"
scope = "root"

[[instances]]
id = "consumer"
package = "{}"
scope = "root"
"#,
            packages.join("provider/plugin.toml").display(),
            packages.join("consumer/plugin.toml").display(),
        ),
    )
    .expect("composition");
}

async fn submit(host: &CompositionHost, id: &str, command: Command) -> CommandOutcome {
    host.submit(CommandEnvelope::new(id, command))
        .await
        .expect("submit")
        .payload
}

async fn setup(root: &Path, command_id: &str) -> (ChildConfig, Vec<u8>, Vec<u8>, u64) {
    let installed = root.join("installed");
    let candidate = root.join("candidate");
    let installed_packages = root.join("packages-v1");
    let candidate_packages = root.join("packages-v2");
    fs::create_dir_all(&installed).expect("installed directory");
    fs::create_dir_all(&candidate).expect("candidate directory");
    for packages in [&installed_packages, &candidate_packages] {
        write_package(packages, "provider", "fixture.provider", true, false);
        write_package(packages, "consumer", "fixture.consumer", false, true);
    }
    fs::OpenOptions::new()
        .append(true)
        .open(candidate_packages.join("provider/plugin.toml"))
        .expect("candidate provider")
        .write_all(b"\n# candidate descriptor\n")
        .expect("candidate change");
    let installed_manifest = installed.join("rsi-meta.toml");
    let installed_lock = installed.join("rsi-meta.lock");
    let candidate_manifest = candidate.join("rsi-meta.toml");
    let candidate_lock = candidate.join("rsi-meta.lock");
    write_composition(&installed_manifest, &installed_packages);
    write_composition(&candidate_manifest, &candidate_packages);
    let database = root.join("state.sqlite3");
    let cache = root.join("cache");

    let bootstrap = CompositionHost::open(open_options(&database, &cache))
        .await
        .expect("bootstrap");
    assert!(matches!(
        submit(
            &bootstrap,
            "lock-v1",
            Command::LockManifest {
                manifest_path: installed_manifest.clone(),
                lock_path: installed_lock.clone(),
            },
        )
        .await,
        CommandOutcome::LockResolved { .. }
    ));
    bootstrap
        .shutdown(Instant::now() + Duration::from_secs(2))
        .await
        .expect("bootstrap shutdown");
    let installed_host = CompositionHost::open(
        open_options(&database, &cache).with_composition(&installed_manifest, &installed_lock),
    )
    .await
    .expect("open installed v1");
    let initial_revision = installed_host.snapshot().graph.revision.0;
    installed_host
        .shutdown(Instant::now() + Duration::from_secs(2))
        .await
        .expect("installed shutdown");

    let lock_builder = CompositionHost::open(open_options(
        root.join("lock-builder.sqlite3"),
        root.join("lock-cache"),
    ))
    .await
    .expect("lock builder");
    assert!(matches!(
        submit(
            &lock_builder,
            "lock-v2",
            Command::LockManifest {
                manifest_path: candidate_manifest.clone(),
                lock_path: candidate_lock.clone(),
            },
        )
        .await,
        CommandOutcome::LockResolved { .. }
    ));
    lock_builder
        .shutdown(Instant::now() + Duration::from_secs(2))
        .await
        .expect("lock builder shutdown");

    let old_manifest = fs::read(&installed_manifest).expect("old manifest");
    let old_lock = fs::read(&installed_lock).expect("old lock");
    (
        ChildConfig {
            database,
            cache,
            installed_manifest,
            installed_lock,
            candidate_manifest,
            candidate_lock,
            command_id: command_id.to_owned(),
        },
        old_manifest,
        old_lock,
        initial_revision,
    )
}

#[test]
fn crash_failpoint_child() {
    let Some(config) = std::env::var_os(CHILD_ENV) else {
        return;
    };
    let config: ChildConfig =
        serde_json::from_slice(config.as_encoded_bytes()).expect("child config");
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("child runtime");
    runtime.block_on(async move {
        let host = CompositionHost::open(
            open_options(&config.database, &config.cache)
                .with_composition(&config.installed_manifest, &config.installed_lock),
        )
        .await
        .expect("child opens installed host");
        let outcome = host
            .submit(CommandEnvelope::new(
                &config.command_id,
                Command::ApplyManifestPath {
                    manifest_path: config.candidate_manifest,
                    lock_path: config.candidate_lock,
                },
            ))
            .await;
        panic!("crash gate did not block apply: {outcome:?}");
    });
}

#[test]
fn offline_install_crash_failpoint_child() {
    let Some(config) = std::env::var_os(INSTALL_CHILD_ENV) else {
        return;
    };
    let config: ChildConfig =
        serde_json::from_slice(config.as_encoded_bytes()).expect("install child config");
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("child runtime");
    runtime.block_on(async move {
        let outcome = CompositionHost::install_offline(InstallRequest {
            operation_id: OperationId(config.command_id),
            workspace: CompositionWorkspace {
                database_path: config.database,
                cache_root: config.cache,
                manifest_path: config.installed_manifest,
                lock_path: config.installed_lock,
            },
            project: CompositionProject {
                manifest_path: config.candidate_manifest,
                lock_path: Some(config.candidate_lock),
            },
        })
        .await;
        panic!("crash gate did not block offline install: {outcome:?}");
    });
}

async fn kill_at_gate(config: &ChildConfig, point: &str, gate_path: &Path) {
    let listener = tokio::net::UnixListener::bind(gate_path).expect("bind crash gate");
    let mut child = ProcessCommand::new(std::env::current_exe().expect("test executable"))
        .args([
            "--exact",
            "crash_failpoint_child",
            "--nocapture",
            "--test-threads=1",
        ])
        .env(
            CHILD_ENV,
            serde_json::to_string(config).expect("child JSON"),
        )
        .env(
            GATE_ENV,
            serde_json::json!({
                "command_id": config.command_id,
                "point": point,
                "gate_path": gate_path,
            })
            .to_string(),
        )
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn crash child");
    let (mut socket, _) = tokio::time::timeout(Duration::from_secs(10), listener.accept())
        .await
        .expect("crash gate deadline")
        .expect("accept crash child");
    let mut ready = [0_u8; 1];
    socket
        .read_exact(&mut ready)
        .await
        .expect("read ready byte");
    assert_eq!(ready, [1]);
    child.kill().expect("SIGKILL crash child");
    let status = child.wait().expect("reap crash child");
    assert!(!status.success(), "crash child must not exit normally");
}

async fn kill_install_at_gate(config: &ChildConfig, point: &str, gate_path: &Path) {
    let listener = tokio::net::UnixListener::bind(gate_path).expect("bind install crash gate");
    let mut child = ProcessCommand::new(std::env::current_exe().expect("test executable"))
        .args([
            "--exact",
            "offline_install_crash_failpoint_child",
            "--nocapture",
            "--test-threads=1",
        ])
        .env(
            INSTALL_CHILD_ENV,
            serde_json::to_string(config).expect("install child JSON"),
        )
        .env(
            GATE_ENV,
            serde_json::json!({
                "command_id": config.command_id,
                "point": point,
                "gate_path": gate_path,
            })
            .to_string(),
        )
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn install crash child");
    let (mut socket, _) = tokio::time::timeout(Duration::from_secs(10), listener.accept())
        .await
        .expect("install crash gate deadline")
        .expect("accept install crash child");
    let mut ready = [0_u8; 1];
    socket
        .read_exact(&mut ready)
        .await
        .expect("read install ready byte");
    assert_eq!(ready, [1]);
    child.kill().expect("SIGKILL install child");
    let status = child.wait().expect("reap install crash child");
    assert!(!status.success(), "install crash child must be killed");
}

#[tokio::test]
async fn sigkill_recovery_covers_all_apply_commit_boundaries() {
    for point in [
        "prepared_before_journal",
        "manifest_replaced_before_lock",
        "terminal_committed_before_publish",
    ] {
        let root = tempdir().expect("scenario root");
        let command_id = format!("crash-{point}");
        let (config, old_manifest, old_lock, initial_revision) =
            setup(root.path(), &command_id).await;
        kill_at_gate(&config, point, &root.path().join("crash.sock")).await;

        let recovered = CompositionHost::open(
            open_options(&config.database, root.path().join("recovery-cache"))
                .with_composition(&config.installed_manifest, &config.installed_lock),
        )
        .await
        .expect("recover killed apply");
        match point {
            "prepared_before_journal" => {
                assert_eq!(fs::read(&config.installed_manifest).unwrap(), old_manifest);
                assert_eq!(fs::read(&config.installed_lock).unwrap(), old_lock);
                assert_eq!(recovered.snapshot().graph.revision.0, initial_revision);
                let replay = recovered
                    .submit(CommandEnvelope::new(
                        &config.command_id,
                        Command::ApplyManifestPath {
                            manifest_path: config.candidate_manifest.clone(),
                            lock_path: config.candidate_lock.clone(),
                        },
                    ))
                    .await
                    .expect("reserved apply replays recovered terminal outcome");
                assert!(matches!(
                    replay.payload,
                    CommandOutcome::Rejected { ref code, .. } if code == "apply_not_committed"
                ));
                let read = recovered
                    .submit(CommandEnvelope::new(
                        &config.command_id,
                        Command::QueryGraph,
                    ))
                    .await
                    .expect("read correlation is independent of durable operation ids");
                assert!(matches!(read.payload, CommandOutcome::Graph { .. }));
            }
            "manifest_replaced_before_lock" => {
                assert_eq!(fs::read(&config.installed_manifest).unwrap(), old_manifest);
                assert_eq!(fs::read(&config.installed_lock).unwrap(), old_lock);
                let replay = recovered
                    .submit(CommandEnvelope::new(
                        &config.command_id,
                        Command::ApplyManifestPath {
                            manifest_path: config.candidate_manifest.clone(),
                            lock_path: config.candidate_lock.clone(),
                        },
                    ))
                    .await
                    .expect("replay aborted outcome");
                assert!(matches!(
                    replay.payload,
                    CommandOutcome::Rejected { ref code, .. } if code == "apply_not_committed"
                ));
            }
            "terminal_committed_before_publish" => {
                assert!(recovered.snapshot().graph.revision.0 > initial_revision);
                let replay = recovered
                    .submit(CommandEnvelope::new(
                        &config.command_id,
                        Command::ApplyManifestPath {
                            manifest_path: config.candidate_manifest.clone(),
                            lock_path: config.candidate_lock.clone(),
                        },
                    ))
                    .await
                    .expect("replay committed outcome");
                assert!(matches!(replay.payload, CommandOutcome::Applied { .. }));
            }
            _ => unreachable!(),
        }
        recovered
            .shutdown(Instant::now() + Duration::from_secs(3))
            .await
            .expect("recovered shutdown");
    }
}

fn offline_install_request(config: &ChildConfig) -> InstallRequest {
    InstallRequest {
        operation_id: OperationId(config.command_id.clone()),
        workspace: CompositionWorkspace {
            database_path: config.database.clone(),
            cache_root: config.cache.clone(),
            manifest_path: config.installed_manifest.clone(),
            lock_path: config.installed_lock.clone(),
        },
        project: CompositionProject {
            manifest_path: config.candidate_manifest.clone(),
            lock_path: Some(config.candidate_lock.clone()),
        },
    }
}

#[tokio::test]
async fn sigkill_recovery_covers_offline_install_pair_and_terminal_boundaries() {
    for point in [
        "manifest_replaced_before_lock",
        "lock_published_before_terminal",
        "terminal_committed_before_publish",
    ] {
        let root = tempdir().expect("offline install scenario root");
        let command_id = format!("install-crash-{point}");
        let (config, old_manifest, old_lock, initial_revision) =
            setup(root.path(), &command_id).await;
        kill_install_at_gate(&config, point, &root.path().join("install-crash.sock")).await;

        let recovered = CompositionHost::open(
            open_options(&config.database, root.path().join("install-recovery-cache"))
                .with_composition(&config.installed_manifest, &config.installed_lock),
        )
        .await
        .expect("recover killed offline install");
        let recovered_snapshot = recovered.snapshot();
        let recovered_revision = recovered_snapshot.graph.revision.0;
        let candidate_active = recovered_snapshot.graph.instances.values().any(|instance| {
            instance
                .package
                .manifest_path
                .to_string_lossy()
                .contains("packages-v2")
        });
        recovered
            .shutdown(Instant::now() + Duration::from_secs(3))
            .await
            .expect("recovered offline install shutdown");

        let replay = CompositionHost::install_offline(offline_install_request(&config)).await;
        if point == "manifest_replaced_before_lock" {
            assert_eq!(fs::read(&config.installed_manifest).unwrap(), old_manifest);
            assert_eq!(fs::read(&config.installed_lock).unwrap(), old_lock);
            assert_eq!(recovered_revision, initial_revision);
            assert!(matches!(
                replay,
                Err(rsi_meta::HostError::OperationRejected { ref code, .. })
                    if code == "install_not_committed"
            ));
        } else {
            assert!(recovered_revision > initial_revision);
            assert!(
                candidate_active,
                "recovery did not activate the installed candidate"
            );
            assert!(matches!(replay, Ok(InstallResult::Installed { .. })));
            assert_ne!(fs::read(&config.installed_manifest).unwrap(), old_manifest);
            assert_ne!(fs::read(&config.installed_lock).unwrap(), old_lock);
        }
    }
}
