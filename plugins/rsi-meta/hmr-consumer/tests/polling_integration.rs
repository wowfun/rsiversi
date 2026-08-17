use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use rsi_meta_loader::{
    BUILD_TARGET, ContentHash, ExpectedHashes, LoadedPlugin, PluginLoader, PluginMailbox,
    PluginMailboxOptions, PluginPackage,
};
use rsi_meta_plugin::{CallOutcome, Lane};
use rsi_meta_plugin::{
    DurableCommand, Frame, FrameBody, LifecyclePhase, OP_CREDIT, OP_OPEN, RUNTIME_TICK_EVENT,
    RUNTIME_TICK_SERVICE,
};
use rsi_meta_plugin_hmr_consumer::rsi_meta_plugin_entry_v0 as hmr_entry;
use rsi_meta_plugin_testkit::PluginHarness;
use serde_json::json;
use tempfile::TempDir;

fn build_and_load_package(package: &Path, cache: &Path) -> (LoadedPlugin, PluginMailbox) {
    build_and_load_package_with_options(package, cache, PluginMailboxOptions::default())
}

fn build_and_load_package_with_options(
    package: &Path,
    cache: &Path,
    options: PluginMailboxOptions,
) -> (LoadedPlugin, PluginMailbox) {
    let manifest_path = package.join("plugin.toml");
    let manifest_bytes = fs::read(&manifest_path).unwrap();
    let loader = PluginLoader::for_current_process(cache);
    let descriptor = PluginPackage::open(&manifest_path).unwrap();
    let artifact = loader.validate_manifest(descriptor.manifest()).unwrap();
    let artifact_path = package.join(&artifact.path);
    let artifact_bytes = fs::read(&artifact_path).unwrap_or_else(|error| {
        panic!(
            "prebuilt release artifact {} is required; run `cargo xtask rsi-meta conformance`: {error}",
            artifact_path.display()
        )
    });
    let staged = loader
        .stage(
            manifest_path,
            ExpectedHashes::new(
                ContentHash::digest(manifest_bytes),
                ContentHash::digest(artifact_bytes),
            ),
        )
        .unwrap();
    loader.load_queued(&staged, options).unwrap()
}

fn build_and_load_polling(cache: &Path) -> (LoadedPlugin, PluginMailbox) {
    build_and_load_package(
        &Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .join("fs-watch-polling"),
        cache,
    )
}

fn build_and_load_hmr(cache: &Path) -> (LoadedPlugin, PluginMailbox) {
    build_and_load_package(Path::new(env!("CARGO_MANIFEST_DIR")), cache)
}

fn build_and_load_native(cache: &Path) -> (LoadedPlugin, PluginMailbox) {
    build_and_load_package(
        &Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .join("fs-watch-native"),
        cache,
    )
}

fn other_target() -> &'static str {
    match BUILD_TARGET {
        "x86_64-unknown-linux-gnu" => "aarch64-apple-darwin",
        "aarch64-apple-darwin" => "x86_64-unknown-linux-gnu",
        target => panic!("unsupported conformance target {target}"),
    }
}

fn package_manifest_source(suffix: &str) -> String {
    format!(
        r#"format_version = 0
config_schema = "config.schema.json"

[package]
id = "fixture.provider"
version = "0.0.1"

[host_api]
major = 0
minimum_minor = 0

[[artifacts]]
target = "{BUILD_TARGET}"
path = "artifact.bin"

[[artifacts]]
target = "{}"
path = "missing-nonselected-artifact"

# {suffix}
"#,
        other_target(),
    )
}

fn lock_source(plugin_manifest: &Path, suffix: &str) -> String {
    format!(
        r#"format_version = 0
target = "{BUILD_TARGET}"
manifest_sha256 = "0000000000000000000000000000000000000000000000000000000000000000"

[[packages]]
id = "fixture.provider"
version = "0.0.1"
path = "{}"
manifest_sha256 = "1111111111111111111111111111111111111111111111111111111111111111"
artifact_sha256 = "2222222222222222222222222222222222222222222222222222222222222222"

# {suffix}
"#,
        plugin_manifest.display(),
    )
}

struct PackageTree {
    manifest: PathBuf,
    lock: PathBuf,
    plugin_manifest: PathBuf,
    artifact: PathBuf,
    schema: PathBuf,
}

fn write_package_tree(root: &Path) -> PackageTree {
    let root = fs::canonicalize(root).unwrap();
    let manifest = root.join("rsi-meta.toml");
    let lock = root.join("rsi-meta.lock");
    let package = root.join("provider");
    let plugin_manifest = package.join("plugin.toml");
    let artifact = package.join("artifact.bin");
    let schema = package.join("config.schema.json");
    fs::create_dir_all(&package).unwrap();
    fs::write(&artifact, b"artifact-v1").unwrap();
    fs::write(&schema, b"{\"type\":\"object\"}").unwrap();
    fs::write(&plugin_manifest, package_manifest_source("v1")).unwrap();
    fs::write(
        &manifest,
        r#"format_version = 0

[composition]
id = "hmr-polling-test"
mode = "development"

[[scopes]]
id = "root"

[[instances]]
id = "provider"
package = "provider/plugin.toml"
scope = "root"
config = {}
"#,
    )
    .unwrap();
    fs::write(&lock, lock_source(&plugin_manifest, "v1")).unwrap();
    PackageTree {
        manifest,
        lock,
        plugin_manifest,
        artifact,
        schema,
    }
}

fn send(plugin: &mut LoadedPlugin, lane: Lane, frame: &Frame) {
    assert_eq!(
        plugin.dispatch(lane, &frame.encode().unwrap()),
        CallOutcome::Ok
    );
}

fn recv_data(mailbox: &mut PluginMailbox) -> Frame {
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    loop {
        if let Ok(posted) = mailbox.try_recv_data() {
            return Frame::decode(posted.payload()).unwrap();
        }
        assert!(std::time::Instant::now() < deadline, "DATA frame timed out");
        std::thread::sleep(Duration::from_millis(1));
    }
}

fn recv_control(mailbox: &mut PluginMailbox) -> Frame {
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    loop {
        if let Ok(posted) = mailbox.try_recv_control() {
            return Frame::decode(posted.payload()).unwrap();
        }
        assert!(
            std::time::Instant::now() < deadline,
            "control frame timed out"
        );
        std::thread::sleep(Duration::from_millis(1));
    }
}

fn assert_mailbox_prepared(mailbox: &mut PluginMailbox, generation: u64) {
    let frame = recv_control(mailbox);
    assert_eq!(
        frame.body,
        FrameBody::Lifecycle {
            phase: LifecyclePhase::Prepared,
            generation,
            config: None,
        }
    );
}

fn assert_harness_prepared(plugin: &PluginHarness, generation: u64) {
    let captured = plugin.recv(Duration::from_secs(1)).unwrap();
    assert_eq!(captured.lane, Lane::Control);
    assert_eq!(
        captured.frame.body,
        FrameBody::Lifecycle {
            phase: LifecyclePhase::Prepared,
            generation,
            config: None,
        }
    );
}

fn recv_harness_after_tick(plugin: &mut PluginHarness, tick: u64) -> Frame {
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    loop {
        assert_eq!(
            plugin
                .send(
                    Lane::Data,
                    &Frame::service_event(
                        None,
                        RUNTIME_TICK_SERVICE,
                        RUNTIME_TICK_EVENT,
                        json!({"tick": tick}),
                    ),
                )
                .unwrap(),
            CallOutcome::Ok
        );
        if let Some(frame) = plugin.try_recv().unwrap() {
            return frame.frame;
        }
        assert!(std::time::Instant::now() < deadline, "HMR frame timed out");
        std::thread::sleep(Duration::from_millis(1));
    }
}

fn recv_control_after_ticks(
    plugin: &mut LoadedPlugin,
    mailbox: &mut PluginMailbox,
    tick: u64,
) -> Frame {
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    loop {
        send(
            plugin,
            Lane::Data,
            &Frame::service_event(
                None,
                RUNTIME_TICK_SERVICE,
                RUNTIME_TICK_EVENT,
                json!({"tick": tick}),
            ),
        );
        if let Ok(posted) = mailbox.try_recv_control() {
            return Frame::decode(posted.payload()).unwrap();
        }
        assert!(
            std::time::Instant::now() < deadline,
            "HMR control frame timed out"
        );
        std::thread::sleep(Duration::from_millis(1));
    }
}

fn atomic_replace(path: &Path, sequence: usize, bytes: &[u8]) {
    let temporary = path.with_extension(format!("replace-{sequence}"));
    fs::write(&temporary, bytes).unwrap();
    fs::rename(temporary, path).unwrap();
}

#[allow(clippy::needless_pass_by_value)] // Test call sites construct one-shot event values inline.
fn data_event(request_id: &str, event: serde_json::Value) -> Frame {
    let bytes = serde_json::to_vec(&event).unwrap();
    Frame::service_data_event(request_id, "fs.watch", bytes)
}

#[test]
#[allow(clippy::too_many_lines)] // Keep the ordered multi-file replacement transcript visible.
fn unchanged_composition_detects_atomic_lock_package_artifact_and_schema_replacements() {
    let temp = TempDir::new().unwrap();
    let tree = write_package_tree(temp.path());
    let manifest_bytes = fs::read(&tree.manifest).unwrap();

    let (mut watcher, mut watcher_mailbox) =
        build_and_load_polling(&temp.path().join("plugin-cache"));
    send(
        &mut watcher,
        Lane::Control,
        &Frame::lifecycle(
            LifecyclePhase::Prepare,
            31,
            Some(json!({"hash_contents": true})),
        ),
    );
    assert_mailbox_prepared(&mut watcher_mailbox, 31);
    send(
        &mut watcher,
        Lane::Control,
        &Frame::lifecycle(LifecyclePhase::Committed, 31, None),
    );

    let mut hmr = PluginHarness::start(hmr_entry).unwrap();
    assert_eq!(
        hmr.send(
            Lane::Control,
            &Frame::lifecycle(
                LifecyclePhase::Prepare,
                32,
                Some(json!({
                    "manifest_path": tree.manifest,
                    "lock_path": tree.lock,
                    "watch_request_id": "hmr-route",
                })),
            ),
        )
        .unwrap(),
        CallOutcome::Ok
    );
    assert_harness_prepared(&hmr, 32);
    assert_eq!(
        hmr.send(
            Lane::Control,
            &Frame::lifecycle(LifecyclePhase::Committed, 32, None),
        )
        .unwrap(),
        CallOutcome::Ok
    );

    for _ in 0..5 {
        let open = hmr.recv(Duration::from_secs(1)).unwrap().frame;
        send(&mut watcher, Lane::Data, &open);
        assert!(watcher_mailbox.try_recv_data().is_err());
        let credit = hmr.recv(Duration::from_secs(1)).unwrap().frame;
        send(&mut watcher, Lane::Data, &credit);
        let ready = recv_data(&mut watcher_mailbox);
        assert_eq!(
            hmr.send(Lane::Data, &ready).unwrap(),
            CallOutcome::Ok,
            "HMR rejected watcher frame {:?}",
            ready.body
        );
    }
    assert!(hmr.try_recv().unwrap().is_none());

    let replacements = [
        (
            tree.lock.clone(),
            lock_source(&tree.plugin_manifest, "v2").into_bytes(),
        ),
        (
            tree.plugin_manifest.clone(),
            package_manifest_source("v2").into_bytes(),
        ),
        (tree.artifact.clone(), b"artifact-v2".to_vec()),
        (
            tree.schema.clone(),
            b"{\"type\":\"object\",\"title\":\"schema-v2\"}".to_vec(),
        ),
    ];
    for (index, (path, bytes)) in replacements.into_iter().enumerate() {
        atomic_replace(&path, index, &bytes);
        assert_eq!(
            fs::read(&tree.manifest).unwrap(),
            manifest_bytes,
            "the composition manifest stays byte-for-byte unchanged"
        );

        let tick = u64::try_from(index + 1).unwrap();
        send(
            &mut watcher,
            Lane::Control,
            &Frame::service_event(None, "runtime.tick", "tick", json!({"tick": tick})),
        );
        let changed = recv_data(&mut watcher_mailbox);
        assert_eq!(hmr.send(Lane::Data, &changed).unwrap(), CallOutcome::Ok);
        assert!(
            hmr.try_recv().unwrap().is_none(),
            "a changed path waits for the coalescing tick"
        );

        let FrameBody::DurableCommand {
            command_id,
            command:
                DurableCommand::ApplyManifestPath {
                    manifest_path,
                    lock_path,
                },
        } = recv_harness_after_tick(&mut hmr, tick).body
        else {
            panic!("expected durable apply command")
        };
        assert!(command_id.starts_with("hmr-v0-"));
        assert_eq!(manifest_path, tree.manifest);
        assert_eq!(lock_path, tree.lock);
        assert_eq!(
            hmr.send(
                Lane::Control,
                &Frame::service_event(
                    Some(command_id),
                    "control.apply-manifest",
                    "applied",
                    json!({}),
                ),
            )
            .unwrap(),
            CallOutcome::Ok
        );
        assert!(hmr.try_recv().unwrap().is_none());
    }
}

#[test]
fn native_backend_reaches_ready_when_nonselected_target_artifact_is_missing() {
    let temp = TempDir::new().unwrap();
    let tree = write_package_tree(temp.path());
    assert!(
        !tree
            .plugin_manifest
            .parent()
            .unwrap()
            .join("missing-nonselected-artifact")
            .exists()
    );
    let (mut watcher, mut watcher_mailbox) =
        build_and_load_native(&temp.path().join("native-plugin-cache"));
    send(
        &mut watcher,
        Lane::Control,
        &Frame::lifecycle(
            LifecyclePhase::Prepare,
            33,
            Some(json!({"recursive": false})),
        ),
    );
    assert_mailbox_prepared(&mut watcher_mailbox, 33);
    send(
        &mut watcher,
        Lane::Control,
        &Frame::lifecycle(LifecyclePhase::Committed, 33, None),
    );

    let mut hmr = PluginHarness::start(hmr_entry).unwrap();
    assert_eq!(
        hmr.send(
            Lane::Control,
            &Frame::lifecycle(
                LifecyclePhase::Prepare,
                34,
                Some(json!({
                    "manifest_path": tree.manifest,
                    "lock_path": tree.lock,
                    "watch_request_id": "hmr-native",
                })),
            ),
        )
        .unwrap(),
        CallOutcome::Ok
    );
    assert_harness_prepared(&hmr, 34);
    assert_eq!(
        hmr.send(
            Lane::Control,
            &Frame::lifecycle(LifecyclePhase::Committed, 34, None),
        )
        .unwrap(),
        CallOutcome::Ok
    );
    for _ in 0..5 {
        let open = hmr.recv(Duration::from_secs(1)).unwrap().frame;
        send(&mut watcher, Lane::Data, &open);
        let credit = hmr.recv(Duration::from_secs(1)).unwrap().frame;
        send(&mut watcher, Lane::Data, &credit);
        let ready = recv_data(&mut watcher_mailbox);
        assert_eq!(hmr.send(Lane::Data, &ready).unwrap(), CallOutcome::Ok);
    }
    assert!(hmr.try_recv().unwrap().is_none());
}

#[allow(clippy::too_many_lines)] // Keep the ordered backpressure and retirement transcript visible.
fn assert_data_backpressure_does_not_block_retired(
    mut watcher: LoadedPlugin,
    mut mailbox: PluginMailbox,
    watched_path: &Path,
    generation: u64,
    config: serde_json::Value,
) {
    send(
        &mut watcher,
        Lane::Control,
        &Frame::lifecycle(LifecyclePhase::Prepare, generation, Some(config)),
    );
    assert_mailbox_prepared(&mut mailbox, generation);
    send(
        &mut watcher,
        Lane::Control,
        &Frame::lifecycle(LifecyclePhase::Committed, generation, None),
    );
    send(
        &mut watcher,
        Lane::Data,
        &Frame::service_request(
            "retire-watch",
            "fs.watch",
            OP_OPEN,
            json!({"consumer": "hmr", "sequence": 0, "path": watched_path}),
        ),
    );
    send(
        &mut watcher,
        Lane::Data,
        &Frame::service_request(
            "retire-watch",
            "fs.watch",
            OP_CREDIT,
            json!({"bytes": 1024 * 1024}),
        ),
    );
    let _first_ready = recv_data(&mut mailbox);
    send(
        &mut watcher,
        Lane::Data,
        &Frame::service_request(
            "second-watch",
            "fs.watch",
            OP_OPEN,
            json!({"consumer": "hmr", "sequence": 0, "path": watched_path}),
        ),
    );
    // Retire synchronously fills the one-frame DATA lane with the first stream
    // terminal. The second terminal must remain pending without depending on
    // native worker scheduling or wall-clock sleeps.
    assert_eq!(
        watcher.dispatch(
            Lane::Control,
            &Frame::lifecycle(LifecyclePhase::Retire, generation, None)
                .encode()
                .unwrap(),
        ),
        CallOutcome::Ok,
        "DATA backpressure must not turn Retire into a failed plugin call"
    );
    assert!(
        mailbox.try_recv_control().is_err(),
        "Retired must follow every accepted stream terminal"
    );
    for (index, expected_request) in ["retire-watch", "second-watch"].into_iter().enumerate() {
        let terminal = recv_data(&mut mailbox);
        assert!(matches!(
            terminal.body,
            FrameBody::ServiceEvent {
                request_id: Some(ref request_id),
                ref event,
                ref payload,
                ..
            } if request_id == expected_request
                && event == "cancel"
                && payload["reason"] == "provider_retired"
        ));
        if index == 0 {
            send(
                &mut watcher,
                Lane::Control,
                &Frame::service_event(
                    None,
                    RUNTIME_TICK_SERVICE,
                    RUNTIME_TICK_EVENT,
                    json!({"tick": 100}),
                ),
            );
        }
    }
    let retired = recv_control(&mut mailbox);
    assert_eq!(
        retired.body,
        FrameBody::Lifecycle {
            phase: LifecyclePhase::Retired,
            generation,
            config: None,
        }
    );
}

fn one_frame_mailbox() -> PluginMailboxOptions {
    PluginMailboxOptions {
        control_capacity: 1,
        data_capacity: 1,
        max_frame_bytes: 1024 * 1024,
    }
}

#[test]
fn polling_backend_retires_when_the_data_lane_is_full() {
    let temp = TempDir::new().unwrap();
    let watched = temp.path().join("watched.toml");
    fs::write(&watched, b"watched").unwrap();

    let polling_package = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .join("fs-watch-polling");
    let polling = build_and_load_package_with_options(
        &polling_package,
        &temp.path().join("polling-retire-cache"),
        one_frame_mailbox(),
    );
    assert_data_backpressure_does_not_block_retired(
        polling.0,
        polling.1,
        &watched,
        35,
        json!({"hash_contents": true}),
    );
}

#[test]
fn native_backend_retires_when_the_data_lane_is_full() {
    let temp = TempDir::new().unwrap();
    let watched = temp.path().join("watched.toml");
    fs::write(&watched, b"watched").unwrap();

    let native_package = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .join("fs-watch-native");
    let native = build_and_load_package_with_options(
        &native_package,
        &temp.path().join("native-retire-cache"),
        one_frame_mailbox(),
    );
    assert_data_backpressure_does_not_block_retired(
        native.0,
        native.1,
        &watched,
        36,
        json!({"recursive": false}),
    );
}

#[test]
#[allow(clippy::too_many_lines)] // Keep the bounded mailbox retry transcript visible.
fn more_than_mailbox_capacity_retries_all_watch_opens_without_blocking_control() {
    const PACKAGE_COUNT: usize = 100;
    let temp = TempDir::new().unwrap();
    let physical_root = fs::canonicalize(temp.path()).unwrap();
    let manifest = physical_root.join("rsi-meta.toml");
    let lock = physical_root.join("rsi-meta.lock");
    fs::write(
        &manifest,
        r#"format_version = 0
scopes = []
instances = []

[composition]
id = "hmr-backpressure"
mode = "development"
"#,
    )
    .unwrap();
    let mut lock_source = format!(
        "format_version = 0\ntarget = \"{BUILD_TARGET}\"\nmanifest_sha256 = \"{}\"\n",
        "0".repeat(64)
    );
    let mut expected_paths = BTreeSet::from([manifest.clone(), lock.clone()]);
    for index in 0..PACKAGE_COUNT {
        let package = physical_root.join(format!("package-{index}"));
        let plugin_manifest = package.join("plugin.toml");
        let schema = package.join("config.schema.json");
        let artifact = package.join("artifact.bin");
        fs::create_dir_all(&package).unwrap();
        fs::write(&schema, b"{\"type\":\"object\"}").unwrap();
        fs::write(&artifact, format!("artifact-{index}")).unwrap();
        fs::write(
            &plugin_manifest,
            format!(
                r#"format_version = 0
config_schema = "config.schema.json"

[[artifacts]]
target = "{BUILD_TARGET}"
path = "artifact.bin"

[[artifacts]]
target = "{}"
path = "missing-nonselected-artifact"
"#,
                other_target(),
            ),
        )
        .unwrap();
        expected_paths.extend([plugin_manifest.clone(), schema, artifact]);
        write!(
            &mut lock_source,
            r#"
[[packages]]
id = "fixture.package-{index}"
version = "0.0.1"
path = "{}"
manifest_sha256 = "{}"
artifact_sha256 = "{}"
"#,
            plugin_manifest.display(),
            "1".repeat(64),
            "2".repeat(64),
        )
        .unwrap();
    }
    fs::write(&lock, lock_source).unwrap();
    assert!(
        expected_paths.len() > PluginMailboxOptions::default().data_capacity,
        "test needs more watch paths than the real loader DATA capacity"
    );

    let (mut hmr, mut mailbox) = build_and_load_hmr(&temp.path().join("hmr-cache"));
    send(
        &mut hmr,
        Lane::Control,
        &Frame::lifecycle(
            LifecyclePhase::Prepare,
            41,
            Some(json!({
                "manifest_path": manifest,
                "lock_path": lock,
                "watch_request_id": "bounded-watch",
            })),
        ),
    );
    assert_mailbox_prepared(&mut mailbox, 41);
    send(
        &mut hmr,
        Lane::Control,
        &Frame::lifecycle(LifecyclePhase::Committed, 41, None),
    );

    let first_posted = mailbox.try_recv_data().unwrap();
    let first = Frame::decode(first_posted.payload()).unwrap();
    let FrameBody::ServiceRequest {
        ref request_id,
        ref operation,
        ..
    } = first.body
    else {
        panic!("first pending frame must be a watch open")
    };
    assert_eq!(operation, OP_OPEN);
    send(
        &mut hmr,
        Lane::Data,
        &data_event(request_id, json!({"type": "overflow"})),
    );
    let control = recv_control_after_ticks(&mut hmr, &mut mailbox, 1);
    assert!(
        matches!(control.body, FrameBody::DurableCommand { .. }),
        "the bounded DATA queue must not block a durable control-lane apply"
    );

    let expected_frame_count = expected_paths.len() * 2;
    let mut frames = vec![first];
    let mut retry_tick = 2_u64;
    while frames.len() < expected_frame_count {
        while let Ok(posted) = mailbox.try_recv_data() {
            frames.push(Frame::decode(posted.payload()).unwrap());
        }
        if frames.len() < expected_frame_count {
            send(
                &mut hmr,
                Lane::Data,
                &Frame::service_event(None, "runtime.tick", "tick", json!({"tick": retry_tick})),
            );
            retry_tick += 1;
        }
    }
    assert!(mailbox.try_recv_data().is_err());

    let mut opens = BTreeMap::new();
    let mut credits = BTreeSet::new();
    for frame in frames {
        let FrameBody::ServiceRequest {
            request_id,
            service,
            operation,
            payload,
        } = frame.body
        else {
            panic!("pending frame must be a watch stream request")
        };
        assert_eq!(service, "fs.watch");
        match operation.as_str() {
            OP_OPEN => {
                assert_eq!(payload["sequence"], 0);
                opens.insert(request_id, PathBuf::from(payload["path"].as_str().unwrap()));
            }
            OP_CREDIT => {
                assert_eq!(payload["bytes"], 16 * 1024 * 1024_u64);
                credits.insert(request_id);
            }
            operation => panic!("unexpected pending operation {operation}"),
        }
    }
    assert_eq!(
        opens.values().cloned().collect::<BTreeSet<_>>(),
        expected_paths
    );
    assert_eq!(credits, opens.keys().cloned().collect());
    for (request_id, path) in opens {
        send(
            &mut hmr,
            Lane::Data,
            &data_event(&request_id, json!({"type": "ready", "path": path})),
        );
    }
}
