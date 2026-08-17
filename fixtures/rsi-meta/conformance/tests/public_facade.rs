use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command as ProcessCommand;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use rsi_meta::{
    ApplyRequest, ApplyResult, CompositionHost, CompositionProject, CompositionWorkspace,
    HostEvent, InstanceId, InstanceStatus, LockResult, OpenOptions, OperationId, RetirementPhase,
    ServiceKey, ServiceOpenRequest, ServiceStream, StreamEnvelope, StreamKind,
};
use rsi_meta_loader::{
    ApiVersion, BUILD_TARGET, ContentHash, ExpectedHashes, LoadedPlugin, PluginLoader,
    PluginMailbox, PluginMailboxOptions, PluginPackage,
};
use rsi_meta_plugin::{CallOutcome, Lane};
use rsi_meta_plugin::{
    EVENT_CREDIT, EVENT_END, Frame as PluginFrame, FrameBody as PluginFrameBody, LifecyclePhase,
    OP_CREDIT, OP_HALF_CLOSE, OP_OPEN, RUNTIME_TICK_EVENT, RUNTIME_TICK_SERVICE,
    STATE_EVENT_APPLIED, STATE_EVENT_VALUE, STATE_OP_COMPARE_AND_SWAP, STATE_OP_GET,
};
use rusqlite::Connection;
use serde_json::{Value, json};
use tempfile::{TempDir, tempdir};

const STREAM_DEADLINE: Duration = Duration::from_secs(2);

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

fn lock_project(manifest_path: PathBuf, lock_path: PathBuf) -> rsi_meta::Result<LockResult> {
    CompositionProject {
        manifest_path,
        lock_path: Some(lock_path),
    }
    .lock()
}

async fn apply_project(
    host: &CompositionHost,
    operation_id: impl Into<String>,
    manifest_path: PathBuf,
    lock_path: PathBuf,
) -> rsi_meta::Result<ApplyResult> {
    host.apply(ApplyRequest {
        operation_id: OperationId(operation_id.into()),
        project: CompositionProject {
            manifest_path,
            lock_path: Some(lock_path),
        },
        expected_revision: None,
    })
    .await
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
    async fn shutdown(&self, deadline: Instant) -> rsi_meta::Result<()>;
}

impl TestHostExt for CompositionHost {
    async fn shutdown(&self, deadline: Instant) -> rsi_meta::Result<()> {
        static NEXT_SHUTDOWN: AtomicU64 = AtomicU64::new(1);
        let sequence = NEXT_SHUTDOWN.fetch_add(1, Ordering::Relaxed);
        self.request_shutdown(OperationId(format!("conformance-shutdown-{sequence}")))
            .await?;
        self.wait_terminated(deadline).await
    }
}

fn assert_protocol_fault_cancel<E>(terminal: Result<StreamEnvelope, E>, fault: &str)
where
    E: std::fmt::Display,
{
    let envelope = terminal.unwrap_or_else(|error| {
        panic!("invalid {fault} output must surface as Cancel, not transport error: {error}")
    });
    assert_eq!(
        envelope.kind,
        StreamKind::Cancel,
        "invalid {fault} output must never surface as DATA"
    );
}

#[test]
#[should_panic(expected = "must surface as Cancel, not transport error")]
fn protocol_fault_terminal_assertion_rejects_transport_errors() {
    assert_protocol_fault_cancel::<&str>(Err("synthetic transport failure"), "synthetic");
}

#[derive(Debug)]
struct BuiltPlugins {
    _target: TempDir,
    echo: PathBuf,
    nested: PathBuf,
    cas: PathBuf,
    polling: PathBuf,
    hmr: PathBuf,
    lifecycle: PathBuf,
}

fn repository() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .and_then(Path::parent)
        .expect("conformance package is nested three levels below repository")
        .to_path_buf()
}

fn built_plugins() -> &'static BuiltPlugins {
    static PLUGINS: OnceLock<BuiltPlugins> = OnceLock::new();
    PLUGINS.get_or_init(|| {
        let target = tempdir().expect("fixture build target");
        let echo = build_cdylib(
            target.path(),
            "fixtures/rsi-meta/echo-bidi",
            "rsi_meta_fixture_echo_bidi",
        );
        let nested = build_cdylib(
            target.path(),
            "fixtures/rsi-meta/nested-scope-consumer",
            "rsi_meta_fixture_nested_scope_consumer",
        );
        let cas = build_cdylib(
            target.path(),
            "fixtures/rsi-meta/cas-counter",
            "rsi_meta_fixture_cas_counter",
        );
        let polling = build_cdylib(
            target.path(),
            "plugins/rsi-meta/fs-watch-polling",
            "rsi_meta_plugin_fs_watch_polling",
        );
        let hmr = build_cdylib(
            target.path(),
            "plugins/rsi-meta/hmr-consumer",
            "rsi_meta_plugin_hmr_consumer",
        );
        let lifecycle = build_cdylib(
            target.path(),
            "fixtures/rsi-meta/lifecycle-probe",
            "rsi_meta_fixture_lifecycle_probe",
        );
        BuiltPlugins {
            _target: target,
            echo,
            nested,
            cas,
            polling,
            hmr,
            lifecycle,
        }
    })
}

fn build_cdylib(target: &Path, package: &str, library_stem: &str) -> PathBuf {
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
        .arg(repository().join(package).join("Cargo.toml"))
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

fn load_real_fixture(
    root: &Path,
    directory: &str,
    mailbox_options: PluginMailboxOptions,
) -> (LoadedPlugin, PluginMailbox) {
    let manifest = root.join(directory).join("plugin.toml");
    let package = PluginPackage::open(&manifest).expect("open real fixture package");
    let artifact = package
        .manifest()
        .artifacts
        .iter()
        .find(|artifact| artifact.target == BUILD_TARGET)
        .expect("fixture target artifact");
    let artifact_bytes = fs::read(
        manifest
            .parent()
            .expect("fixture package directory")
            .join(&artifact.path),
    )
    .expect("read real fixture artifact");
    let hashes = ExpectedHashes::new(
        package.manifest_hash(),
        ContentHash::digest(&artifact_bytes),
    );
    let loader = PluginLoader::for_current_process(root.join(format!("{directory}-cache")));
    let staged = loader.stage(&manifest, hashes).expect("stage real fixture");
    loader
        .load_queued(&staged, mailbox_options)
        .expect("load real fixture with bounded mailbox")
}

fn load_real_fixture_with_one_data_slot(
    root: &Path,
    directory: &str,
) -> (LoadedPlugin, PluginMailbox) {
    load_real_fixture(
        root,
        directory,
        PluginMailboxOptions {
            data_capacity: 1,
            ..PluginMailboxOptions::default()
        },
    )
}

#[allow(clippy::needless_pass_by_value)] // Call sites construct one-shot protocol frames inline.
fn dispatch_plugin(plugin: &mut LoadedPlugin, lane: Lane, frame: PluginFrame) -> CallOutcome {
    plugin.dispatch(lane, &frame.encode().expect("encode plugin frame"))
}

fn next_control_body(mailbox: &mut PluginMailbox) -> PluginFrameBody {
    PluginFrame::decode(
        mailbox
            .try_recv_control()
            .expect("expected plugin control frame")
            .payload(),
    )
    .expect("decode plugin control frame")
    .body
}

fn next_data_body(mailbox: &mut PluginMailbox) -> PluginFrameBody {
    PluginFrame::decode(
        mailbox
            .try_recv_data()
            .expect("expected plugin DATA frame")
            .payload(),
    )
    .expect("decode plugin DATA frame")
    .body
}

fn plugin_tick(sequence: u64) -> PluginFrame {
    PluginFrame::service_event(
        None,
        RUNTIME_TICK_SERVICE,
        RUNTIME_TICK_EVENT,
        json!({"tick": sequence}),
    )
}

#[test]
fn real_stream_fixture_cdylibs_retry_retired_after_the_control_mailbox_drains() {
    let temp = tempdir().expect("real retirement test root");
    write_real_packages(temp.path());
    write_lifecycle_packages(temp.path());
    let cases = [
        ("echo", None),
        (
            "nested",
            Some(json!({"request_id": "nested", "message": "test"})),
        ),
        ("cas", Some(json!({}))),
        (
            "lifecycle-provider",
            Some(json!({
                "fail_prepare": false,
                "retire_mode": "ack",
                "tag": "control-backpressure",
                "prepare_action": "normal_ack",
            })),
        ),
    ];

    for (directory, config) in cases {
        let (mut plugin, mut mailbox) = load_real_fixture(
            temp.path(),
            directory,
            PluginMailboxOptions {
                control_capacity: 1,
                ..PluginMailboxOptions::default()
            },
        );
        assert_eq!(
            dispatch_plugin(
                &mut plugin,
                Lane::Control,
                PluginFrame::lifecycle(LifecyclePhase::Prepare, 1, config),
            ),
            CallOutcome::Ok,
            "{directory} must prepare"
        );
        assert_eq!(
            dispatch_plugin(
                &mut plugin,
                Lane::Control,
                PluginFrame::lifecycle(LifecyclePhase::Committed, 1, None),
            ),
            CallOutcome::Ok,
            "{directory} must commit while Prepared occupies the control slot"
        );
        assert_eq!(
            dispatch_plugin(
                &mut plugin,
                Lane::Control,
                PluginFrame::lifecycle(LifecyclePhase::Retire, 1, None),
            ),
            CallOutcome::Ok,
            "{directory} must retain Retired when its control post returns WouldBlock"
        );
        assert!(matches!(
            next_control_body(&mut mailbox),
            PluginFrameBody::Lifecycle {
                phase: LifecyclePhase::Prepared,
                generation: 1,
                ..
            }
        ));
        assert!(mailbox.try_recv_control().is_err());

        assert_eq!(
            dispatch_plugin(&mut plugin, Lane::Control, plugin_tick(1)),
            CallOutcome::Ok,
            "{directory} must use runtime.tick to retry Retired without a client frame"
        );
        assert!(matches!(
            next_control_body(&mut mailbox),
            PluginFrameBody::Lifecycle {
                phase: LifecyclePhase::Retired,
                generation: 1,
                ..
            }
        ));
        assert_eq!(
            dispatch_plugin(&mut plugin, Lane::Control, plugin_tick(2)),
            CallOutcome::Ok
        );
        assert!(
            mailbox.try_recv_control().is_err(),
            "{directory} must emit Retired exactly once"
        );
    }
}

#[test]
#[allow(clippy::too_many_lines)] // Preserve the ordered real-plugin backpressure transcript.
fn real_echo_cdylib_uses_tick_to_finish_would_blocked_data_and_end() {
    let temp = tempdir().expect("real echo test root");
    write_real_packages(temp.path());
    let (mut plugin, mut mailbox) = load_real_fixture_with_one_data_slot(temp.path(), "echo");

    assert_eq!(
        dispatch_plugin(
            &mut plugin,
            Lane::Control,
            PluginFrame::lifecycle(LifecyclePhase::Prepare, 1, None),
        ),
        CallOutcome::Ok
    );
    assert!(matches!(
        next_control_body(&mut mailbox),
        PluginFrameBody::Lifecycle {
            phase: LifecyclePhase::Prepared,
            generation: 1,
            ..
        }
    ));
    assert_eq!(
        dispatch_plugin(
            &mut plugin,
            Lane::Control,
            PluginFrame::lifecycle(LifecyclePhase::Committed, 1, None),
        ),
        CallOutcome::Ok
    );

    assert_eq!(
        dispatch_plugin(
            &mut plugin,
            Lane::Data,
            PluginFrame::service_request(
                "echo-blocked",
                "fixture.echo",
                OP_OPEN,
                json!({"consumer": "real-loader", "sequence": 0}),
            ),
        ),
        CallOutcome::Ok
    );
    let payload = vec![1, 2, 3];
    let encoded = payload.len() as u64;
    assert_eq!(
        dispatch_plugin(
            &mut plugin,
            Lane::Data,
            PluginFrame::service_request(
                "echo-blocked",
                "fixture.echo",
                OP_CREDIT,
                json!({"bytes": encoded}),
            ),
        ),
        CallOutcome::Ok
    );
    assert_eq!(
        dispatch_plugin(
            &mut plugin,
            Lane::Data,
            PluginFrame::service_data_request("echo-blocked", "fixture.echo", payload.clone()),
        ),
        CallOutcome::Ok,
        "the full one-slot mailbox must surface as retained WouldBlock"
    );
    assert!(matches!(
        next_data_body(&mut mailbox),
        PluginFrameBody::ServiceEvent { event, .. } if event == EVENT_CREDIT
    ));
    assert!(mailbox.try_recv_data().is_err());

    assert_eq!(
        dispatch_plugin(&mut plugin, Lane::Control, plugin_tick(1)),
        CallOutcome::Ok
    );
    assert_eq!(
        dispatch_plugin(
            &mut plugin,
            Lane::Data,
            PluginFrame::service_request(
                "echo-blocked",
                "fixture.echo",
                OP_HALF_CLOSE,
                json!({"sequence": 1}),
            ),
        ),
        CallOutcome::Ok,
        "END must remain pending behind the unread DATA frame"
    );
    assert!(matches!(
        next_data_body(&mut mailbox),
        PluginFrameBody::ServiceDataEvent { payload: actual, .. } if actual == payload
    ));
    assert!(mailbox.try_recv_data().is_err());

    assert_eq!(
        dispatch_plugin(&mut plugin, Lane::Control, plugin_tick(2)),
        CallOutcome::Ok
    );
    assert!(matches!(
        next_data_body(&mut mailbox),
        PluginFrameBody::ServiceEvent { event, .. } if event == EVENT_END
    ));
    assert!(mailbox.try_recv_data().is_err());
}

#[test]
#[allow(clippy::too_many_lines)] // Preserve the ordered real-plugin CAS and credit transcript.
fn real_cas_cdylib_uses_ticks_to_finish_partial_data_end_without_client_credit() {
    let temp = tempdir().expect("real CAS test root");
    write_real_packages(temp.path());
    let (mut plugin, mut mailbox) = load_real_fixture_with_one_data_slot(temp.path(), "cas");

    assert_eq!(
        dispatch_plugin(
            &mut plugin,
            Lane::Control,
            PluginFrame::lifecycle(LifecyclePhase::Prepare, 1, Some(json!({}))),
        ),
        CallOutcome::Ok
    );
    assert!(matches!(
        next_control_body(&mut mailbox),
        PluginFrameBody::Lifecycle {
            phase: LifecyclePhase::Prepared,
            generation: 1,
            ..
        }
    ));
    assert_eq!(
        dispatch_plugin(
            &mut plugin,
            Lane::Control,
            PluginFrame::lifecycle(LifecyclePhase::Committed, 1, None),
        ),
        CallOutcome::Ok
    );

    assert_eq!(
        dispatch_plugin(
            &mut plugin,
            Lane::Data,
            PluginFrame::service_request(
                "counter-1",
                "fixture.cas-counter",
                OP_OPEN,
                json!({"consumer": "real-loader", "sequence": 0}),
            ),
        ),
        CallOutcome::Ok
    );
    assert!(matches!(
        next_data_body(&mut mailbox),
        PluginFrameBody::ServiceEvent { event, .. } if event == EVENT_CREDIT
    ));
    assert_eq!(
        dispatch_plugin(
            &mut plugin,
            Lane::Data,
            PluginFrame::service_request(
                "counter-1",
                "fixture.cas-counter",
                OP_CREDIT,
                json!({"bytes": 1024 * 1024}),
            ),
        ),
        CallOutcome::Ok
    );
    let request = serde_json::to_vec(&json!({"key": "ticks"})).expect("encode increment");
    assert_eq!(
        dispatch_plugin(
            &mut plugin,
            Lane::Data,
            PluginFrame::service_data_request("counter-1", "fixture.cas-counter", request),
        ),
        CallOutcome::Ok
    );
    let PluginFrameBody::ServiceRequest {
        request_id: read_id,
        operation,
        ..
    } = next_data_body(&mut mailbox)
    else {
        panic!("expected state read")
    };
    assert_eq!(operation, STATE_OP_GET);
    assert_eq!(
        dispatch_plugin(
            &mut plugin,
            Lane::Data,
            PluginFrame::service_event(
                Some(read_id),
                "state.cas",
                STATE_EVENT_VALUE,
                json!({"key": "ticks", "version": 0, "value": null}),
            ),
        ),
        CallOutcome::Ok
    );
    let PluginFrameBody::ServiceRequest {
        request_id: cas_id,
        operation,
        ..
    } = next_data_body(&mut mailbox)
    else {
        panic!("expected state compare-and-swap")
    };
    assert_eq!(operation, STATE_OP_COMPARE_AND_SWAP);

    assert_eq!(
        dispatch_plugin(
            &mut plugin,
            Lane::Data,
            PluginFrame::service_request(
                "counter-blocker",
                "fixture.cas-counter",
                OP_OPEN,
                json!({"consumer": "real-loader", "sequence": 0}),
            ),
        ),
        CallOutcome::Ok
    );
    let applied_payload = json!({"key": "ticks", "version": 1, "value": 1});
    assert_eq!(
        dispatch_plugin(
            &mut plugin,
            Lane::Data,
            PluginFrame::service_event(
                Some(cas_id),
                "state.cas",
                STATE_EVENT_APPLIED,
                applied_payload.clone(),
            ),
        ),
        CallOutcome::Ok,
        "the blocker frame must force result DATA into retained WouldBlock"
    );
    assert!(matches!(
        next_data_body(&mut mailbox),
        PluginFrameBody::ServiceEvent {
            request_id: Some(request_id),
            event,
            ..
        } if request_id == "counter-blocker" && event == EVENT_CREDIT
    ));
    assert!(mailbox.try_recv_data().is_err());

    assert_eq!(
        dispatch_plugin(&mut plugin, Lane::Control, plugin_tick(1)),
        CallOutcome::Ok
    );
    assert!(matches!(
        next_data_body(&mut mailbox),
        PluginFrameBody::ServiceDataEvent { .. }
    ));
    assert!(mailbox.try_recv_data().is_err());
    assert_eq!(
        dispatch_plugin(&mut plugin, Lane::Control, plugin_tick(2)),
        CallOutcome::Ok
    );
    assert!(matches!(
        next_data_body(&mut mailbox),
        PluginFrameBody::ServiceEvent { event, .. } if event == EVENT_END
    ));
    assert!(mailbox.try_recv_data().is_err());
}

#[derive(Clone, Copy)]
struct PackageSpec<'a> {
    directory: &'a str,
    package_id: &'a str,
    library: &'a Path,
    provides: &'a [&'a str],
    injects: &'a [(&'a str, bool)],
    capabilities: &'a [&'a str],
    schema: &'a str,
    process_fixed: bool,
}

fn write_package(root: &Path, spec: PackageSpec<'_>) {
    let package = root.join(spec.directory);
    fs::create_dir_all(&package).expect("package directory");
    let artifact = format!("artifact{}", std::env::consts::DLL_SUFFIX);
    fs::copy(spec.library, package.join(&artifact)).expect("copy real plugin artifact");
    fs::write(package.join("config.schema.json"), spec.schema).expect("config schema");
    let string_list = |items: &[&str]| {
        items
            .iter()
            .map(|item| format!("\"{item}\""))
            .collect::<Vec<_>>()
            .join(", ")
    };
    let injects = spec
        .injects
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
            "format_version = 0\nprovides = [{}]\ncapabilities = [{}]\nconfig_schema = \"config.schema.json\"\n\n[package]\nid = \"{}\"\nversion = \"0.0.1\"\nprocess_fixed = {}\n\n[host_api]\nmajor = {}\nminimum_minor = {}\n\n[[artifacts]]\ntarget = \"{}\"\npath = \"{}\"\n\n{}",
            string_list(spec.provides),
            string_list(spec.capabilities),
            spec.package_id,
            spec.process_fixed,
            api.major,
            api.minor,
            BUILD_TARGET,
            artifact,
            injects,
        ),
    )
    .expect("plugin manifest");
}

fn write_real_packages(root: &Path) {
    let built = built_plugins();
    let empty_schema = r#"{
      "$schema":"https://json-schema.org/draft/2020-12/schema",
      "type":"object",
      "additionalProperties":false
    }"#;
    write_package(
        root,
        PackageSpec {
            directory: "echo",
            package_id: "fixture.echo-bidi",
            library: &built.echo,
            provides: &["fixture.echo"],
            injects: &[("runtime.tick", true)],
            capabilities: &[],
            schema: empty_schema,
            process_fixed: false,
        },
    );
    write_package(
        root,
        PackageSpec {
            directory: "nested",
            package_id: "fixture.nested-scope-consumer",
            library: &built.nested,
            provides: &["fixture.nested-consumer"],
            injects: &[("fixture.echo", true), ("runtime.tick", true)],
            capabilities: &[],
            schema: r#"{
              "$schema":"https://json-schema.org/draft/2020-12/schema",
              "type":"object",
              "required":["request_id","message"],
              "properties":{
                "request_id":{"type":"string","minLength":1},
                "message":{"type":"string"}
              },
              "additionalProperties":false
            }"#,
            process_fixed: false,
        },
    );
    write_package(
        root,
        PackageSpec {
            directory: "cas",
            package_id: "fixture.cas-counter",
            library: &built.cas,
            provides: &["fixture.cas-counter"],
            injects: &[("state.cas", true), ("runtime.tick", true)],
            capabilities: &["state.cas"],
            schema: empty_schema,
            process_fixed: false,
        },
    );
    write_package(
        root,
        PackageSpec {
            directory: "nested-client",
            package_id: "fixture.nested-client",
            library: &built.echo,
            provides: &[],
            injects: &[("fixture.nested-consumer", true), ("runtime.tick", true)],
            capabilities: &[],
            schema: empty_schema,
            process_fixed: false,
        },
    );
    write_package(
        root,
        PackageSpec {
            directory: "counter-client",
            package_id: "fixture.counter-client",
            library: &built.echo,
            provides: &[],
            injects: &[("fixture.cas-counter", true), ("runtime.tick", true)],
            capabilities: &[],
            schema: empty_schema,
            process_fixed: false,
        },
    );
}

fn write_hmr_drift_packages(root: &Path) -> PathBuf {
    let built = built_plugins();
    let empty_schema = r#"{
      "$schema":"https://json-schema.org/draft/2020-12/schema",
      "type":"object",
      "additionalProperties":false
    }"#;
    write_package(
        root,
        PackageSpec {
            directory: "drift-echo",
            package_id: "fixture.drift-echo",
            library: &built.echo,
            provides: &["fixture.echo"],
            injects: &[("runtime.tick", true)],
            capabilities: &[],
            schema: empty_schema,
            process_fixed: false,
        },
    );
    write_package(
        root,
        PackageSpec {
            directory: "drift-polling",
            package_id: "fixture.drift-polling",
            library: &built.polling,
            provides: &["fs.watch"],
            injects: &[("runtime.tick", true)],
            capabilities: &["fs.read"],
            schema: r#"{
              "$schema":"https://json-schema.org/draft/2020-12/schema",
              "type":"object",
              "properties":{"hash_contents":{"type":"boolean"}},
              "additionalProperties":false
            }"#,
            process_fixed: false,
        },
    );
    write_package(
        root,
        PackageSpec {
            directory: "drift-hmr",
            package_id: "fixture.drift-hmr",
            library: &built.hmr,
            provides: &["hmr.watch-consumer"],
            injects: &[("fs.watch", true), ("runtime.tick", true)],
            capabilities: &["control.apply-manifest", "fs.read"],
            schema: r#"{
              "$schema":"https://json-schema.org/draft/2020-12/schema",
              "type":"object",
              "required":["manifest_path","lock_path","watch_request_id"],
              "properties":{
                "manifest_path":{"type":"string","minLength":1},
                "lock_path":{"type":"string","minLength":1},
                "watch_request_id":{"type":"string","minLength":1}
              },
              "additionalProperties":false
            }"#,
            process_fixed: true,
        },
    );
    root.join("drift-echo/plugin.toml")
}

fn write_lifecycle_packages(root: &Path) {
    let built = built_plugins();
    write_package(
        root,
        PackageSpec {
            directory: "lifecycle-provider",
            package_id: "fixture.lifecycle-provider",
            library: &built.lifecycle,
            provides: &["fixture.lifecycle-probe"],
            injects: &[
                ("state.cas", true),
                ("fixture.echo", false),
                ("runtime.tick", true),
            ],
            capabilities: &["state.cas", "control.apply-manifest"],
            schema: include_str!("../../lifecycle-probe/config.schema.json"),
            process_fixed: false,
        },
    );
    write_package(
        root,
        PackageSpec {
            directory: "lifecycle-client",
            package_id: "fixture.lifecycle-client",
            library: &built.echo,
            provides: &[],
            injects: &[("fixture.lifecycle-probe", true), ("runtime.tick", true)],
            capabilities: &[],
            schema: r#"{
              "$schema":"https://json-schema.org/draft/2020-12/schema",
              "type":"object",
              "additionalProperties":false
            }"#,
            process_fixed: false,
        },
    );
}

fn write_lifecycle_composition(
    path: &Path,
    fail_prepare: bool,
    retire_mode: &str,
    tag: &str,
    prepare_action: &str,
    stream_fault: &str,
) {
    fs::write(
        path,
        format!(
            r#"format_version = 0

[composition]
id = "public-lifecycle-e2e"
mode = "development"

[[scopes]]
id = "root"

[[instances]]
id = "provider"
package = "lifecycle-provider/plugin.toml"
scope = "root"
config = {{ fail_prepare = {fail_prepare}, retire_mode = "{retire_mode}", tag = "{tag}", prepare_action = "{prepare_action}", stream_fault = "{stream_fault}" }}

[[instances]]
id = "client"
package = "lifecycle-client/plugin.toml"
scope = "root"
bindings = {{ "fixture.lifecycle-probe" = "provider" }}
"#,
        ),
    )
    .expect("lifecycle composition");
}

fn write_hmr_drift_composition(path: &Path, lock: &Path) {
    let root = path.parent().expect("composition directory");
    fs::write(
        path,
        format!(
            r#"format_version = 0

[composition]
id = "plugin-origin-drift"
mode = "development"

[[scopes]]
id = "root"

[[instances]]
id = "watcher"
package = "{}"
scope = "root"
config = {{ hash_contents = true }}

[[instances]]
id = "hmr"
package = "{}"
scope = "root"
config = {{ manifest_path = "{}", lock_path = "{}", watch_request_id = "public-drift" }}
bindings = {{ "fs.watch" = "watcher" }}

[[instances]]
id = "echo-provider"
package = "{}"
scope = "root"
"#,
            root.join("drift-polling/plugin.toml").display(),
            root.join("drift-hmr/plugin.toml").display(),
            path.display(),
            lock.display(),
            root.join("drift-echo/plugin.toml").display(),
        ),
    )
    .expect("HMR drift composition");
}

fn write_composition(path: &Path) {
    fs::write(
        path,
        r#"format_version = 0

[composition]
id = "public-fixture-e2e"
mode = "development"

[[scopes]]
id = "root"

[[scopes]]
id = "nested"
parent = "root"

[[scopes]]
id = "counter-a-scope"
parent = "root"

[[scopes]]
id = "counter-b-scope"
parent = "root"

[[instances]]
id = "echo-provider"
package = "echo/plugin.toml"
scope = "root"

[[instances]]
id = "nested-provider"
package = "nested/plugin.toml"
scope = "nested"
config = { request_id = "nested-e2e", message = "nearest-provider" }
bindings = { "fixture.echo" = "echo-provider" }

[[instances]]
id = "nested-client"
package = "nested-client/plugin.toml"
scope = "nested"
bindings = { "fixture.nested-consumer" = "nested-provider" }

[[instances]]
id = "counter-a"
package = "cas/plugin.toml"
scope = "counter-a-scope"

[[instances]]
id = "counter-client-a"
package = "counter-client/plugin.toml"
scope = "counter-a-scope"
bindings = { "fixture.cas-counter" = "counter-a" }

[[instances]]
id = "counter-b"
package = "cas/plugin.toml"
scope = "counter-b-scope"

[[instances]]
id = "counter-client-b"
package = "counter-client/plugin.toml"
scope = "counter-b-scope"
bindings = { "fixture.cas-counter" = "counter-b" }
"#,
    )
    .expect("composition manifest");
}

async fn open_applied_host(root: &Path) -> (CompositionHost, PathBuf, PathBuf, PathBuf, PathBuf) {
    write_real_packages(root);
    let manifest = root.join("rsi-meta.toml");
    let lock = root.join("rsi-meta.lock");
    let database = root.join("state.sqlite3");
    let cache = root.join("cache");
    write_composition(&manifest);
    let host = CompositionHost::open(OpenOptions::new(workspace(&database, &cache)))
        .await
        .expect("open empty host");
    let locked = lock_project(manifest.clone(), lock.clone()).expect("lock manifest");
    assert!(matches!(
        locked,
        LockResult::Created { .. } | LockResult::Unchanged { .. }
    ));
    let applied = apply_project(
        &host,
        "apply-public-fixtures",
        manifest.clone(),
        lock.clone(),
    )
    .await
    .expect("apply manifest");
    assert!(
        matches!(applied, ApplyResult::Applied { .. }),
        "initial apply failed: {applied:?}",
    );
    (host, manifest, lock, database, cache)
}

async fn recv_frame(stream: &mut ServiceStream) -> rsi_meta::StreamEnvelope {
    tokio::time::timeout(STREAM_DEADLINE, stream.recv())
        .await
        .expect("stream frame deadline")
        .expect("stream remains open")
        .expect("valid stream frame")
}

async fn open_counter(host: &CompositionHost, consumer: &str) -> ServiceStream {
    let mut stream = host
        .open_service(ServiceOpenRequest {
            consumer: InstanceId::new(consumer),
            service: ServiceKey::new("fixture.cas-counter"),
        })
        .expect("open public counter stream");
    let credit = recv_frame(&mut stream).await;
    assert_eq!(credit.kind, StreamKind::Credit);
    stream
        .grant_credit(1024 * 1024)
        .await
        .expect("grant counter output credit");
    stream
}

async fn recv_increment(stream: &mut ServiceStream) -> Value {
    let data = recv_frame(stream).await;
    assert_eq!(data.kind, StreamKind::Data);
    let value = serde_json::from_slice(data.data.as_deref().expect("DATA bytes present"))
        .expect("counter DATA is JSON");
    assert_eq!(recv_frame(stream).await.kind, StreamKind::End);
    value
}

async fn increment(host: &CompositionHost, consumer: &str, key: &str) -> Value {
    let mut stream = open_counter(host, consumer).await;
    stream
        .send(&serde_json::to_vec(&json!({"key": key})).expect("encode increment"))
        .await
        .expect("send increment");
    recv_increment(&mut stream).await
}

async fn open_probe(host: &CompositionHost) -> ServiceStream {
    let mut stream = host
        .open_service(ServiceOpenRequest {
            consumer: InstanceId::new("client"),
            service: ServiceKey::new("fixture.lifecycle-probe"),
        })
        .expect("open lifecycle probe stream");
    assert_eq!(stream.provider(), &InstanceId::new("provider"));
    assert_eq!(recv_frame(&mut stream).await.kind, StreamKind::Credit);
    stream
        .grant_credit(1024 * 1024)
        .await
        .expect("grant lifecycle output credit");
    stream
}

async fn assert_tagged_probe_data(stream: &mut ServiceStream, tag: &str, input: &[u8]) {
    stream.send(input).await.expect("send lifecycle DATA");
    let data = recv_frame(stream).await;
    assert_eq!(data.kind, StreamKind::Data);
    let mut expected = tag.as_bytes().to_vec();
    expected.push(0);
    expected.extend_from_slice(input);
    assert_eq!(data.data.as_deref(), Some(expected.as_slice()));
}

async fn open_applied_lifecycle_host(
    root: &Path,
    retire_mode: &str,
) -> (CompositionHost, PathBuf, PathBuf) {
    write_lifecycle_packages(root);
    let manifest = root.join("installed.toml");
    let lock = root.join("installed.lock");
    write_lifecycle_composition(&manifest, false, retire_mode, "old", "normal_ack", "none");
    let host = CompositionHost::open(OpenOptions::new(workspace(
        root.join("state.sqlite3"),
        root.join("cache"),
    )))
    .await
    .expect("open lifecycle host");
    let locked = lock_project(manifest.clone(), lock.clone()).expect("resolve lifecycle lock");
    assert!(matches!(
        locked,
        LockResult::Created { .. } | LockResult::Unchanged { .. }
    ));
    let applied = apply_project(
        &host,
        "apply-lifecycle-installed",
        manifest.clone(),
        lock.clone(),
    )
    .await
    .expect("apply lifecycle composition");
    assert!(
        matches!(applied, ApplyResult::Applied { .. }),
        "initial lifecycle apply failed: {applied:?}",
    );
    (host, manifest, lock)
}

async fn apply_lifecycle_candidate(
    host: &CompositionHost,
    root: &Path,
    case: &str,
    tag: &str,
    prepare_action: &str,
    stream_fault: &str,
) -> ApplyResult {
    let manifest = root.join(format!("candidate-{case}.toml"));
    let lock = root.join(format!("candidate-{case}.lock"));
    write_lifecycle_composition(&manifest, false, "ack", tag, prepare_action, stream_fault);
    let locked =
        lock_project(manifest.clone(), lock.clone()).expect("resolve lifecycle candidate lock");
    assert!(matches!(
        locked,
        LockResult::Created { .. } | LockResult::Unchanged { .. }
    ));
    apply_project(host, format!("apply-lifecycle-{case}"), manifest, lock)
        .await
        .expect("apply lifecycle candidate")
}

fn sqlite_count(database: &Path, sql: &str) -> i64 {
    Connection::open(database)
        .expect("open durable store for conformance assertion")
        .query_row(sql, [], |row| row.get(0))
        .expect("query durable conformance fact")
}

#[tokio::test]
async fn nested_scope_proxy_routes_public_bidi_stream_through_real_cdylibs() {
    let temp = tempdir().expect("test root");
    let (host, _, _, _, _) = open_applied_host(temp.path()).await;
    let mut stream = host
        .open_service(ServiceOpenRequest {
            consumer: InstanceId::new("nested-client"),
            service: ServiceKey::new("fixture.nested-consumer"),
        })
        .expect("open public nested stream");
    assert_eq!(stream.provider(), &InstanceId::new("nested-provider"));
    assert_eq!(recv_frame(&mut stream).await.kind, StreamKind::Credit);
    stream
        .grant_credit(1024 * 1024)
        .await
        .expect("grant nested output credit");
    stream.send(b"nested-e2e").await.expect("send nested DATA");
    let echoed = recv_frame(&mut stream).await;
    assert_eq!(echoed.kind, StreamKind::Data);
    assert_eq!(echoed.data.as_deref(), Some(b"nested-e2e".as_slice()));
    stream.half_close().await.expect("half close public stream");
    assert_eq!(recv_frame(&mut stream).await.kind, StreamKind::End);
    host.shutdown(Instant::now() + Duration::from_secs(2))
        .await
        .expect("shutdown");
}

#[tokio::test]
async fn cas_streams_retry_conflicts_persist_restart_and_isolate_instance_namespaces() {
    let temp = tempdir().expect("test root");
    let (host, manifest, _lock, database, cache) = open_applied_host(temp.path()).await;
    let mut first = open_counter(&host, "counter-client-a").await;
    let mut second = open_counter(&host, "counter-client-a").await;
    let request = serde_json::to_vec(&json!({"key": "requests"})).expect("increment JSON");
    let (first_send, second_send) = tokio::join!(first.send(&request), second.send(&request));
    first_send.expect("first concurrent increment");
    second_send.expect("second concurrent increment");
    let (first_result, second_result) =
        tokio::join!(recv_increment(&mut first), recv_increment(&mut second));
    let mut values = [
        first_result["value"].as_u64().expect("first counter value"),
        second_result["value"]
            .as_u64()
            .expect("second counter value"),
    ];
    values.sort_unstable();
    assert_eq!(
        values,
        [1, 2],
        "a CAS conflict must retry, not lose an update"
    );

    host.shutdown(Instant::now() + Duration::from_secs(2))
        .await
        .expect("shutdown before restart");
    let reopened = CompositionHost::open(OpenOptions::new(workspace(&database, &cache)))
        .await
        .expect("restart installed composition");
    let persisted = increment(&reopened, "counter-client-a", "requests").await;
    assert_eq!(persisted["value"], 3, "counter-a state survives restart");
    let isolated = increment(&reopened, "counter-client-b", "requests").await;
    assert_eq!(
        isolated["value"], 1,
        "the same key is isolated by mounted instance id"
    );
    reopened
        .shutdown(Instant::now() + Duration::from_secs(2))
        .await
        .expect("shutdown restarted host");

    let other_manifest = temp.path().join("other-composition.toml");
    let other_lock = temp.path().join("other-composition.lock");
    let other_source = fs::read_to_string(&manifest)
        .expect("installed composition source")
        .replace("public-fixture-e2e", "public-fixture-other");
    fs::write(&other_manifest, other_source).expect("different composition id manifest");
    let other = CompositionHost::open(OpenOptions::new(workspace(&database, &cache)))
        .await
        .expect("open same SQLite for another composition");
    let locked =
        lock_project(other_manifest.clone(), other_lock.clone()).expect("lock other composition");
    assert!(matches!(
        locked,
        LockResult::Created { .. } | LockResult::Unchanged { .. }
    ));
    let applied = apply_project(
        &other,
        "apply-other-composition",
        other_manifest,
        other_lock,
    )
    .await
    .expect("apply other composition");
    assert!(matches!(applied, ApplyResult::Applied { .. }));
    let isolated_composition = increment(&other, "counter-client-a", "requests").await;
    assert_eq!(
        isolated_composition["value"], 1,
        "the CAS namespace includes composition id as well as instance and key"
    );
    other
        .shutdown(Instant::now() + Duration::from_secs(2))
        .await
        .expect("shutdown other composition");
}

#[tokio::test]
#[allow(clippy::too_many_lines)] // One end-to-end drift transaction keeps causality visible.
async fn plugin_origin_drift_rebuilds_lock_and_applies_without_composition_edit() {
    let temp = tempdir().expect("test root");
    let provider_manifest = write_hmr_drift_packages(temp.path());
    let manifest = temp.path().join("rsi-meta.toml");
    let lock = temp.path().join("rsi-meta.lock");
    let database = temp.path().join("state.sqlite3");
    let cache = temp.path().join("cache");
    write_hmr_drift_composition(&manifest, &lock);
    let bootstrap = CompositionHost::open(OpenOptions::new(workspace(&database, &cache)))
        .await
        .expect("open host");
    let locked = lock_project(manifest.clone(), lock.clone()).expect("lock initial desired state");
    assert!(matches!(
        locked,
        LockResult::Created { .. } | LockResult::Unchanged { .. }
    ));
    bootstrap
        .shutdown(Instant::now() + Duration::from_secs(2))
        .await
        .expect("shutdown lock bootstrap");
    let host = CompositionHost::open(
        OpenOptions::new(workspace(&database, &cache)).with_composition(&manifest, &lock),
    )
    .await
    .expect("open installed drift composition");
    let initial_revision = host.snapshot().graph.revision;
    let installed_manifest = fs::read(&manifest).expect("installed composition bytes");
    let initial_lock = fs::read(&lock).expect("installed lock bytes");

    tokio::time::sleep(Duration::from_millis(250)).await;
    for _ in 0..4 {
        tokio::task::yield_now().await;
    }
    let mut package_bytes = fs::read(&provider_manifest).expect("provider package manifest");
    package_bytes.extend_from_slice(b"\n# valid descriptor drift\n");
    fs::write(&provider_manifest, package_bytes).expect("replace provider descriptor");

    for _ in 0..12 {
        tokio::time::sleep(Duration::from_millis(250)).await;
        for _ in 0..4 {
            tokio::task::yield_now().await;
        }
        if host.snapshot().graph.revision > initial_revision {
            break;
        }
    }
    assert!(
        host.snapshot().graph.revision > initial_revision,
        "plugin-origin drift must build a fresh lock and commit through the public facade"
    );
    assert_eq!(
        fs::read(&manifest).expect("composition remains installed"),
        installed_manifest,
        "package drift must not require editing composition bytes"
    );
    assert_ne!(
        fs::read(&lock).expect("refreshed installed lock"),
        initial_lock,
        "the refreshed package hash must become the installed commit marker"
    );
    let package_revision = host.snapshot().graph.revision;
    let package_lock = fs::read(&lock).expect("package-drift lock bytes");
    let stable_package_manifest =
        fs::read(&provider_manifest).expect("post-drift package manifest bytes");
    let artifact_path = provider_manifest
        .parent()
        .expect("provider package directory")
        .join(format!("artifact{}", std::env::consts::DLL_SUFFIX));
    let stable_artifact = fs::read(&artifact_path).expect("provider artifact bytes");
    let schema_path = provider_manifest
        .parent()
        .expect("provider package directory")
        .join("config.schema.json");
    fs::write(
        &schema_path,
        r#"{
          "$schema":"https://json-schema.org/draft/2020-12/schema",
          "$comment":"compatible schema-only plugin-origin drift",
          "type":"object",
          "additionalProperties":false
        }"#,
    )
    .expect("replace only provider schema bytes");

    for _ in 0..12 {
        tokio::time::sleep(Duration::from_millis(250)).await;
        for _ in 0..4 {
            tokio::task::yield_now().await;
        }
        if host.snapshot().graph.revision > package_revision {
            break;
        }
    }
    let schema_revision = host.snapshot().graph.revision;
    assert!(
        schema_revision > package_revision,
        "schema-only drift must produce a fresh pinned lock and graph generation"
    );
    assert_eq!(fs::read(&manifest).unwrap(), installed_manifest);
    assert_eq!(
        fs::read(&provider_manifest).unwrap(),
        stable_package_manifest,
        "schema drift must not rewrite plugin.toml"
    );
    assert_eq!(
        fs::read(&artifact_path).unwrap(),
        stable_artifact,
        "schema drift must not require artifact drift"
    );
    assert_ne!(
        fs::read(&lock).expect("schema-pinned installed lock"),
        package_lock,
        "config_schema_sha256 must advance the installed lock"
    );
    host.shutdown(Instant::now() + Duration::from_secs(2))
        .await
        .expect("shutdown");
    let reopened = CompositionHost::open(
        OpenOptions::new(workspace(&database, temp.path().join("cache-reopen")))
            .with_composition(&manifest, &lock),
    )
    .await
    .expect("reopen the schema-pinned installed composition");
    assert_eq!(reopened.snapshot().graph.revision, schema_revision);
    reopened
        .shutdown(Instant::now() + Duration::from_secs(2))
        .await
        .expect("shutdown reopened schema-pinned host");
}

#[tokio::test]
async fn shadow_prepare_failure_preserves_installed_pair_graph_and_old_routing() {
    let temp = tempdir().expect("test root");
    let (host, installed_manifest_path, installed_lock_path) =
        open_applied_lifecycle_host(temp.path(), "ack").await;
    let installed_manifest = fs::read(&installed_manifest_path).expect("installed manifest bytes");
    let installed_lock = fs::read(&installed_lock_path).expect("installed lock bytes");
    let installed_graph = host.snapshot().graph;

    let candidate_manifest = temp.path().join("candidate-failed.toml");
    let candidate_lock = temp.path().join("candidate-failed.lock");
    write_lifecycle_composition(
        &candidate_manifest,
        true,
        "ack",
        "must-not-commit",
        "normal_ack",
        "none",
    );
    let locked = lock_project(candidate_manifest.clone(), candidate_lock.clone())
        .expect("resolve failed candidate lock");
    assert!(matches!(
        locked,
        LockResult::Created { .. } | LockResult::Unchanged { .. }
    ));
    let rejected = apply_project(
        &host,
        "apply-lifecycle-failed-candidate",
        candidate_manifest,
        candidate_lock,
    )
    .await
    .expect_err("failed prepare has a durable rejection");
    assert!(matches!(
        rejected,
        rsi_meta::HostError::OperationRejected { ref code, .. }
            if code == "plugin_prepare_failed"
    ));
    assert_eq!(
        host.snapshot().graph,
        installed_graph,
        "a failed shadow graph must never become observable"
    );
    assert_eq!(
        fs::read(&installed_manifest_path).expect("installed manifest remains readable"),
        installed_manifest,
        "failed prepare must not replace the installed composition"
    );
    assert_eq!(
        fs::read(&installed_lock_path).expect("installed lock remains readable"),
        installed_lock,
        "failed prepare must not move the lock commit marker"
    );

    let mut old = open_probe(&host).await;
    assert_tagged_probe_data(&mut old, "old", b"after-rejection").await;
    old.cancel("failure-test-complete")
        .await
        .expect("cancel old generation stream");
    assert_eq!(recv_frame(&mut old).await.kind, StreamKind::Cancel);
    drop(old);
    host.shutdown(Instant::now() + Duration::from_secs(2))
        .await
        .expect("shutdown");
}

#[tokio::test]
async fn hold_retirement_does_not_block_cutover_and_shutdown_cancels_the_retire_wait() {
    let temp = tempdir().expect("test root");
    let (host, _, _) = open_applied_lifecycle_host(temp.path(), "hold").await;
    let mut old = open_probe(&host).await;
    assert_tagged_probe_data(&mut old, "old", b"before-cutover").await;

    let candidate_manifest = temp.path().join("candidate-new.toml");
    let candidate_lock = temp.path().join("candidate-new.lock");
    write_lifecycle_composition(
        &candidate_manifest,
        false,
        "hold",
        "new",
        "normal_ack",
        "none",
    );
    let locked = lock_project(candidate_manifest.clone(), candidate_lock.clone())
        .expect("resolve new lifecycle candidate");
    assert!(matches!(
        locked,
        LockResult::Created { .. } | LockResult::Unchanged { .. }
    ));
    let applied = apply_project(
        &host,
        "apply-lifecycle-new-candidate",
        candidate_manifest,
        candidate_lock,
    )
    .await
    .expect("apply lifecycle candidate");
    assert!(
        matches!(applied, ApplyResult::Applied { .. }),
        "retirement is not part of commit success: {applied:?}",
    );

    let retirement = host
        .snapshot()
        .graph
        .retiring_instances
        .into_iter()
        .find(|entry| entry.instance_id == InstanceId::new("provider"))
        .expect("old provider generation is tracked while its stream lease remains");
    assert_eq!(retirement.phase, RetirementPhase::Draining);
    assert!(retirement.lease_count >= 1);
    assert_tagged_probe_data(&mut old, "old", b"still-old").await;

    let mut new = open_probe(&host).await;
    assert_tagged_probe_data(&mut new, "new", b"new-admission").await;
    new.cancel("new-stream-complete")
        .await
        .expect("cancel new stream");
    assert_eq!(recv_frame(&mut new).await.kind, StreamKind::Cancel);
    drop(new);

    old.cancel("release-old-lease")
        .await
        .expect("cancel old stream");
    assert_eq!(recv_frame(&mut old).await.kind, StreamKind::Cancel);
    drop(old);
    let mut observed_hold = false;
    for _ in 0..128 {
        observed_hold = host
            .snapshot()
            .graph
            .retiring_instances
            .iter()
            .any(|entry| {
                entry.instance_id == InstanceId::new("provider")
                    && entry.phase == RetirementPhase::Retiring
                    && entry.lease_count == 0
            });
        if observed_hold {
            break;
        }
        tokio::task::yield_now().await;
    }
    assert!(
        observed_hold,
        "hold mode must remain observable after Retire instead of claiming Retired"
    );
    host.shutdown(Instant::now() + Duration::from_secs(2))
        .await
        .expect("shutdown cancels an intentionally held retirement");
}

#[tokio::test]
async fn rejected_retirement_is_stopped_and_reaped_instead_of_wedging_forever() {
    let temp = tempdir().expect("test root");
    let (host, _, _) = open_applied_lifecycle_host(temp.path(), "reject").await;

    let outcome = apply_lifecycle_candidate(
        &host,
        temp.path(),
        "retire-rejected",
        "new",
        "normal_ack",
        "none",
    )
    .await;
    assert!(matches!(outcome, ApplyResult::Applied { .. }));

    for _ in 0..128 {
        if host.snapshot().graph.retiring_instances.is_empty() {
            host.shutdown(Instant::now() + Duration::from_secs(2))
                .await
                .expect("shutdown after rejected retirement");
            return;
        }
        tokio::task::yield_now().await;
    }
    panic!("a rejected Retire callback left the generation permanently Retiring");
}

#[tokio::test]
async fn prepare_state_write_is_read_only_and_does_not_block_apply() {
    let temp = tempdir().expect("test root");
    let database = temp.path().join("state.sqlite3");
    let (host, _, _) = open_applied_lifecycle_host(temp.path(), "ack").await;
    let before = host.snapshot();
    let outcome = apply_lifecycle_candidate(
        &host,
        temp.path(),
        "state-write",
        "write-guard",
        "state_write_then_ack",
        "none",
    )
    .await;
    assert!(
        matches!(outcome, ApplyResult::Applied { .. }),
        "the read-only conflict is the expected prepare response: {outcome:?}"
    );
    assert_eq!(
        host.snapshot().graph.revision.0,
        before.graph.revision.0 + 1
    );
    assert_eq!(
        sqlite_count(
            &database,
            "SELECT COUNT(*) FROM plugin_state WHERE composition_id = 'public-lifecycle-e2e' AND instance_id = 'provider' AND state_key = 'prepare/write-guard'",
        ),
        0,
        "prepare-time CAS must not leave a row, tombstone, or version"
    );
    let mut stream = open_probe(&host).await;
    assert_tagged_probe_data(&mut stream, "write-guard", b"committed").await;
    stream
        .cancel("state-write-test-complete")
        .await
        .expect("cancel stream");
    assert_eq!(recv_frame(&mut stream).await.kind, StreamKind::Cancel);
    drop(stream);
    host.shutdown(Instant::now() + Duration::from_secs(2))
        .await
        .expect("shutdown");
}

#[tokio::test]
async fn prepare_durable_and_outbound_side_effects_are_rejected_without_extra_commits() {
    let temp = tempdir().expect("test root");
    let database = temp.path().join("state.sqlite3");
    let (host, _, _) = open_applied_lifecycle_host(temp.path(), "ack").await;

    for (case, action) in [
        ("durable-side-effect", "durable_then_ack"),
        ("outbound-side-effect", "outbound_open_then_ack"),
    ] {
        let before = host.snapshot();
        let outcome =
            apply_lifecycle_candidate(&host, temp.path(), case, case, action, "none").await;
        assert!(
            matches!(outcome, ApplyResult::Applied { .. }),
            "the host must reject the prepare side effect without losing the valid acknowledgement: {outcome:?}"
        );
        let after = host.snapshot();
        assert_eq!(after.graph.revision.0, before.graph.revision.0 + 1);
        assert_eq!(
            after.cursor,
            before.cursor + 1,
            "only the explicit candidate apply may append a durable event"
        );
        let events = host
            .events_after(before.cursor, 10)
            .await
            .expect("query exact side-effect event window");
        assert_eq!(events.events.len(), 1);
        assert_eq!(
            events.events[0]
                .operation_id
                .as_ref()
                .map(|id| id.0.as_str()),
            Some(format!("apply-lifecycle-{case}").as_str())
        );
        assert_eq!(after.graph.instances.len(), 2);
        assert_eq!(
            sqlite_count(
                &database,
                "SELECT COUNT(*) FROM command_outcome WHERE command_id LIKE 'probe-prepare-%'",
            ),
            0,
            "a prepare-time durable command must never become a command outcome"
        );

        let mut stream = open_probe(&host).await;
        assert_tagged_probe_data(&mut stream, case, b"healthy").await;
        stream
            .cancel("side-effect-test-complete")
            .await
            .expect("cancel stream");
        assert_eq!(recv_frame(&mut stream).await.kind, StreamKind::Cancel);
        drop(stream);
    }
    host.shutdown(Instant::now() + Duration::from_secs(2))
        .await
        .expect("shutdown");
}

#[tokio::test]
async fn malformed_plugin_stream_frames_fail_at_the_narrowest_safe_boundary() {
    let temp = tempdir().expect("test root");
    let (host, _, _) = open_applied_lifecycle_host(temp.path(), "ack").await;

    for fault in [
        "wrong_service",
        "unknown_event",
        "non_byte_data",
        "malformed_json",
    ] {
        let outcome = apply_lifecycle_candidate(
            &host,
            temp.path(),
            &format!("stream-{fault}"),
            fault,
            "normal_ack",
            fault,
        )
        .await;
        assert!(matches!(outcome, ApplyResult::Applied { .. }));
        let mut stream = open_probe(&host).await;
        let before_fault_cursor = host.snapshot().cursor;
        let mut fault_events = host
            .subscribe(before_fault_cursor)
            .await
            .expect("subscribe before runtime fault");
        stream
            .send(b"must-not-escape")
            .await
            .expect("send fault trigger");
        let terminal = tokio::time::timeout(STREAM_DEADLINE, stream.recv())
            .await
            .expect("protocol fault must not hang")
            .expect("protocol fault emits one terminal result");
        assert_protocol_fault_cancel(terminal, fault);
        assert!(
            stream.send(b"after-terminal").await.is_err(),
            "a protocol-faulted stream cannot continue"
        );
        assert!(
            tokio::time::timeout(STREAM_DEADLINE, stream.recv())
                .await
                .expect("closed stream read must not hang")
                .is_none(),
            "the stream has exactly one terminal item"
        );
        if matches!(fault, "non_byte_data" | "malformed_json") {
            let fault_event = tokio::time::timeout(STREAM_DEADLINE, fault_events.recv())
                .await
                .expect("runtime fault event deadline")
                .expect("runtime fault event stream remains open")
                .expect("runtime fault event is valid");
            assert!(matches!(
                fault_event.event,
                HostEvent::RuntimeFaulted { ref instance_id, .. }
                    if instance_id == &InstanceId::new("provider")
            ));
            let faulted = host.snapshot();
            assert!(faulted.graph.instances.values().any(|instance| {
                instance.id == InstanceId::new("provider")
                    && matches!(instance.status, InstanceStatus::Faulted { .. })
            }));
            let events = host
                .events_after(before_fault_cursor, 16)
                .await
                .expect("runtime fault event is durable");
            assert_eq!(events.events, vec![fault_event]);
            let repaired = host
                .apply(ApplyRequest {
                    operation_id: OperationId(format!("repair-stream-{fault}")),
                    project: CompositionProject {
                        manifest_path: temp.path().join(format!("candidate-stream-{fault}.toml")),
                        lock_path: Some(temp.path().join(format!("candidate-stream-{fault}.lock"))),
                    },
                    expected_revision: None,
                })
                .await
                .expect("same-hash apply repairs a faulted generation");
            assert!(matches!(repaired, ApplyResult::Applied { .. }));
            assert!(host.snapshot().graph.instances.values().any(|instance| {
                instance.id == InstanceId::new("provider")
                    && matches!(instance.status, InstanceStatus::Active)
            }));
        } else {
            assert!(host.snapshot().graph.instances.values().any(|instance| {
                instance.id == InstanceId::new("provider")
                    && matches!(instance.status, InstanceStatus::Active)
            }));
            assert!(
                host.events_after(before_fault_cursor, 16)
                    .await
                    .expect("stream-local protocol error event query")
                    .events
                    .is_empty(),
                "a recoverable per-stream violation must not fault the whole runtime"
            );
        }
    }
    host.shutdown(Instant::now() + Duration::from_secs(2))
        .await
        .expect("shutdown");
}
