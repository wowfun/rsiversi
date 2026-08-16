use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use rsi_meta_frame_contract::{
    DurableCommand, EVENT_CANCEL, EVENT_DATA, Frame, FrameBody, LifecyclePhase, OP_CANCEL,
    OP_CREDIT, OP_OPEN,
};
use rsi_meta_plugin::{CallOutcome, Lane, PostFrameOutcome};
use rsi_meta_plugin_hmr_consumer::rsi_meta_plugin_entry_v0;
use rsi_meta_plugin_testkit::PluginHarness;
use serde_json::json;
use tempfile::TempDir;

struct PackageTree {
    root: TempDir,
    manifest: PathBuf,
    lock: PathBuf,
    plugin_manifest: PathBuf,
    schema: PathBuf,
    linux_artifact: PathBuf,
    macos_artifact: PathBuf,
}

fn package_tree() -> PackageTree {
    let root = TempDir::new().unwrap();
    let manifest = root.path().join("rsi-meta.toml");
    let lock = root.path().join("rsi-meta.lock");
    let package = root.path().join("provider");
    let plugin_manifest = package.join("plugin.toml");
    let schema = package.join("config.schema.json");
    let linux_artifact = package.join("target/linux/provider.so");
    let macos_artifact = package.join("target/macos/provider.dylib");
    fs::create_dir_all(linux_artifact.parent().unwrap()).unwrap();
    fs::create_dir_all(macos_artifact.parent().unwrap()).unwrap();
    fs::write(&linux_artifact, b"linux artifact").unwrap();
    fs::write(
        &schema,
        r#"{"$schema":"https://json-schema.org/draft/2020-12/schema","type":"object"}"#,
    )
    .unwrap();
    fs::write(
        &plugin_manifest,
        r#"format_version = 0
config_schema = "config.schema.json"

[package]
id = "fixture.provider"
version = "0.0.1"

[host_api]
major = 0
minimum_minor = 0

[[artifacts]]
target = "x86_64-unknown-linux-gnu"
path = "target/linux/provider.so"

[[artifacts]]
target = "aarch64-apple-darwin"
path = "target/macos/provider.dylib"
"#,
    )
    .unwrap();
    fs::write(
        &manifest,
        r#"format_version = 0

[composition]
id = "hmr-test"
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
    fs::write(
        &lock,
        format!(
            r#"format_version = 0
target = "x86_64-unknown-linux-gnu"
manifest_sha256 = "0000000000000000000000000000000000000000000000000000000000000000"

[[packages]]
id = "fixture.provider"
version = "0.0.1"
path = "{}"
manifest_sha256 = "1111111111111111111111111111111111111111111111111111111111111111"
artifact_sha256 = "2222222222222222222222222222222222222222222222222222222222222222"
"#,
            plugin_manifest.display(),
        ),
    )
    .unwrap();
    PackageTree {
        root,
        manifest,
        lock,
        plugin_manifest,
        schema,
        linux_artifact,
        macos_artifact,
    }
}

fn prepare_and_commit(plugin: &mut PluginHarness, tree: &PackageTree, generation: u64) {
    assert_eq!(
        plugin
            .send(
                Lane::Control,
                &Frame::lifecycle(
                    LifecyclePhase::Prepare,
                    generation,
                    Some(json!({
                        "manifest_path": tree.manifest,
                        "lock_path": tree.lock,
                        "watch_request_id": "hmr-watch",
                    })),
                ),
            )
            .unwrap(),
        CallOutcome::Ok
    );
    let prepared = plugin.recv(Duration::from_secs(1)).unwrap();
    assert_eq!(prepared.lane, Lane::Control);
    assert_eq!(
        prepared.frame.body,
        FrameBody::Lifecycle {
            phase: LifecyclePhase::Prepared,
            generation,
            config: None,
        }
    );
    assert_eq!(
        plugin
            .send(
                Lane::Control,
                &Frame::lifecycle(LifecyclePhase::Committed, generation, None),
            )
            .unwrap(),
        CallOutcome::Ok
    );
}

fn watch_path(body: FrameBody) -> (String, PathBuf) {
    let FrameBody::ServiceRequest {
        request_id,
        service,
        operation,
        payload,
    } = body
    else {
        panic!("expected watch request")
    };
    assert_eq!(service, "fs.watch");
    assert_eq!(operation, OP_OPEN);
    assert_eq!(payload["consumer"], "hmr.watch-consumer");
    assert_eq!(payload["sequence"], 0);
    let path = payload["path"].as_str().map(PathBuf::from).unwrap();
    (request_id, path)
}

fn assert_credit(body: FrameBody, request_id: &str) {
    let FrameBody::ServiceRequest {
        request_id: actual_id,
        service,
        operation,
        payload,
    } = body
    else {
        panic!("expected watch stream credit")
    };
    assert_eq!(actual_id, request_id);
    assert_eq!(service, "fs.watch");
    assert_eq!(operation, OP_CREDIT);
    assert_eq!(payload["bytes"], 16 * 1024 * 1024_u64);
}

#[allow(clippy::needless_pass_by_value)] // Test call sites construct one-shot event values inline.
fn data_event(request_id: &str, event: serde_json::Value) -> Frame {
    let bytes = serde_json::to_vec(&event).unwrap();
    Frame::service_event(
        Some(request_id.to_owned()),
        "fs.watch",
        EVENT_DATA,
        serde_json::Value::Array(bytes.into_iter().map(serde_json::Value::from).collect()),
    )
}

fn ready_watches(
    plugin: &mut PluginHarness,
    count: usize,
) -> std::collections::BTreeMap<PathBuf, String> {
    let mut subscriptions = std::collections::BTreeMap::new();
    for _ in 0..count {
        let captured = plugin.recv(Duration::from_secs(1)).unwrap();
        let (request_id, path) = watch_path(captured.frame.body);
        assert_credit(
            plugin.recv(Duration::from_secs(1)).unwrap().frame.body,
            &request_id,
        );
        assert_eq!(
            plugin
                .send(
                    Lane::Data,
                    &data_event(&request_id, json!({"type": "ready", "path": path})),
                )
                .unwrap(),
            CallOutcome::Ok
        );
        subscriptions.insert(path, request_id);
    }
    subscriptions
}

fn changed(path: &Path, request_id: &str) -> Frame {
    data_event(
        request_id,
        json!({"type": "changed", "path": path, "change": "modified"}),
    )
}

#[test]
fn committed_generation_watches_composition_lock_package_schema_and_artifacts() {
    let tree = package_tree();
    let mut plugin = PluginHarness::start(rsi_meta_plugin_entry_v0).unwrap();
    prepare_and_commit(&mut plugin, &tree, 21);

    let expected = BTreeSet::from([
        tree.manifest.clone(),
        tree.lock.clone(),
        tree.plugin_manifest.clone(),
        tree.schema.clone(),
        tree.linux_artifact.clone(),
    ]);
    assert!(
        !tree.macos_artifact.exists(),
        "a missing artifact for a non-selected target must not be opened"
    );
    let mut ids = BTreeSet::new();
    let mut paths = BTreeSet::new();
    for _ in 0..expected.len() {
        let captured = plugin.recv(Duration::from_secs(1)).unwrap();
        assert_eq!(captured.lane, Lane::Data);
        let (id, path) = watch_path(captured.frame.body);
        assert_credit(plugin.recv(Duration::from_secs(1)).unwrap().frame.body, &id);
        assert!(
            ids.insert(id),
            "each watch must have an independent request id"
        );
        assert!(paths.insert(path), "watch paths must be de-duplicated");
    }
    assert_eq!(paths, expected);
    assert!(plugin.try_recv().unwrap().is_none());
}

#[test]
fn prepare_rejects_missing_or_duplicate_locked_target_artifacts() {
    let tree = package_tree();
    fs::write(
        &tree.plugin_manifest,
        r#"format_version = 0
config_schema = "config.schema.json"

[[artifacts]]
target = "aarch64-apple-darwin"
path = "target/macos/provider.dylib"
"#,
    )
    .unwrap();
    let mut missing = PluginHarness::start(rsi_meta_plugin_entry_v0).unwrap();
    assert_eq!(
        missing
            .send(
                Lane::Control,
                &Frame::lifecycle(
                    LifecyclePhase::Prepare,
                    23,
                    Some(json!({
                        "manifest_path": tree.manifest,
                        "lock_path": tree.lock,
                        "watch_request_id": "missing-target",
                    })),
                ),
            )
            .unwrap(),
        CallOutcome::Failed
    );

    fs::write(
        &tree.plugin_manifest,
        r#"format_version = 0
config_schema = "config.schema.json"

[[artifacts]]
target = "x86_64-unknown-linux-gnu"
path = "target/linux/provider.so"

[[artifacts]]
target = "x86_64-unknown-linux-gnu"
path = "target/linux/provider.so"
"#,
    )
    .unwrap();
    let mut duplicate = PluginHarness::start(rsi_meta_plugin_entry_v0).unwrap();
    assert_eq!(
        duplicate
            .send(
                Lane::Control,
                &Frame::lifecycle(
                    LifecyclePhase::Prepare,
                    24,
                    Some(json!({
                        "manifest_path": tree.manifest,
                        "lock_path": tree.lock,
                        "watch_request_id": "duplicate-target",
                    })),
                ),
            )
            .unwrap(),
        CallOutcome::Failed
    );
}

#[test]
fn prepare_rejects_an_oversized_watch_plan_document() {
    let tree = package_tree();
    let mut source = fs::read_to_string(&tree.manifest).unwrap();
    source.push_str(&"# padding\n".repeat(500_000));
    fs::write(&tree.manifest, source).unwrap();
    let mut plugin = PluginHarness::start(rsi_meta_plugin_entry_v0).unwrap();

    assert_eq!(
        plugin
            .send(
                Lane::Control,
                &Frame::lifecycle(
                    LifecyclePhase::Prepare,
                    29,
                    Some(json!({
                        "manifest_path": tree.manifest,
                        "lock_path": tree.lock,
                        "watch_request_id": "oversized-plan",
                    })),
                ),
            )
            .unwrap(),
        CallOutcome::Failed,
        "watch-plan TOML must be bounded before it is buffered"
    );
}

#[test]
fn prepare_applies_the_loader_bound_to_a_package_manifest() {
    let tree = package_tree();
    let mut source = fs::read_to_string(&tree.plugin_manifest).unwrap();
    source.push_str(&"# padding\n".repeat(200_000));
    fs::write(&tree.plugin_manifest, source).unwrap();
    let mut plugin = PluginHarness::start(rsi_meta_plugin_entry_v0).unwrap();

    assert_eq!(
        plugin
            .send(
                Lane::Control,
                &Frame::lifecycle(
                    LifecyclePhase::Prepare,
                    32,
                    Some(json!({
                        "manifest_path": tree.manifest,
                        "lock_path": tree.lock,
                        "watch_request_id": "oversized-package",
                    })),
                ),
            )
            .unwrap(),
        CallOutcome::Failed,
        "HMR must not buffer a package manifest the loader will reject"
    );
}

#[test]
fn prepare_rejects_an_oversized_watched_artifact_before_reading_it() {
    let tree = package_tree();
    fs::OpenOptions::new()
        .write(true)
        .open(&tree.linux_artifact)
        .unwrap()
        .set_len(256 * 1024 * 1024 + 1)
        .unwrap();
    let mut plugin = PluginHarness::start(rsi_meta_plugin_entry_v0).unwrap();

    assert_eq!(
        plugin
            .send(
                Lane::Control,
                &Frame::lifecycle(
                    LifecyclePhase::Prepare,
                    33,
                    Some(json!({
                        "manifest_path": tree.manifest,
                        "lock_path": tree.lock,
                        "watch_request_id": "oversized-artifact",
                    })),
                ),
            )
            .unwrap(),
        CallOutcome::Failed,
        "HMR content identity must not synchronously read an unbounded artifact"
    );
}

#[cfg(unix)]
#[test]
fn prepare_bounds_bytes_from_the_target_of_a_composition_symlink() {
    use std::os::unix::fs::symlink;

    let tree = package_tree();
    let target = tree.root.path().join("composition-target.toml");
    fs::rename(&tree.manifest, &target).unwrap();
    symlink(&target, &tree.manifest).unwrap();
    let mut plugin = PluginHarness::start(rsi_meta_plugin_entry_v0).unwrap();

    assert_eq!(
        plugin
            .send(
                Lane::Control,
                &Frame::lifecycle(
                    LifecyclePhase::Prepare,
                    30,
                    Some(json!({
                        "manifest_path": tree.manifest,
                        "lock_path": tree.lock,
                        "watch_request_id": "symlinked-plan",
                    })),
                ),
            )
            .unwrap(),
        CallOutcome::Ok
    );
    assert_eq!(
        plugin.recv(Duration::from_secs(1)).unwrap().frame.body,
        FrameBody::Lifecycle {
            phase: LifecyclePhase::Prepared,
            generation: 30,
            config: None,
        }
    );
}

#[cfg(unix)]
#[test]
fn prepare_does_not_follow_a_locked_package_manifest_symlink() {
    use std::os::unix::fs::symlink;

    let tree = package_tree();
    let target = tree
        .plugin_manifest
        .parent()
        .unwrap()
        .join("plugin-target.toml");
    fs::rename(&tree.plugin_manifest, &target).unwrap();
    symlink(&target, &tree.plugin_manifest).unwrap();
    let mut plugin = PluginHarness::start(rsi_meta_plugin_entry_v0).unwrap();

    assert_eq!(
        plugin
            .send(
                Lane::Control,
                &Frame::lifecycle(
                    LifecyclePhase::Prepare,
                    31,
                    Some(json!({
                        "manifest_path": tree.manifest,
                        "lock_path": tree.lock,
                        "watch_request_id": "symlinked-package",
                    })),
                ),
            )
            .unwrap(),
        CallOutcome::Failed,
        "package inputs retain the loader's no-follow contract"
    );
}

#[test]
#[allow(clippy::too_many_lines)] // Keep the ordered watch replacement transcript visible.
fn changed_lock_replaces_only_changed_watch_paths_and_new_artifact_drives_apply() {
    let tree = package_tree();
    let mut plugin = PluginHarness::start(rsi_meta_plugin_entry_v0).unwrap();
    prepare_and_commit(&mut plugin, &tree, 25);
    let old = ready_watches(&mut plugin, 5);

    let package_v2 = tree
        .plugin_manifest
        .parent()
        .and_then(|path| path.parent())
        .unwrap()
        .join("provider-v2");
    fs::create_dir_all(&package_v2).unwrap();
    let manifest_v2 = package_v2.join("plugin.toml");
    let schema_v2 = package_v2.join("schema-v2.json");
    let artifact_v2 = package_v2.join("provider-v2.so");
    fs::write(&schema_v2, b"{\"type\":\"object\"}").unwrap();
    fs::write(&artifact_v2, b"artifact-v2").unwrap();
    fs::write(
        &manifest_v2,
        r#"format_version = 0
config_schema = "schema-v2.json"

[[artifacts]]
target = "x86_64-unknown-linux-gnu"
path = "provider-v2.so"

[[artifacts]]
target = "aarch64-apple-darwin"
path = "missing-provider-v2.dylib"
"#,
    )
    .unwrap();
    write_lock(&tree.lock, &manifest_v2);

    assert_eq!(
        plugin
            .send(Lane::Data, &changed(&tree.lock, &old[&tree.lock]))
            .unwrap(),
        CallOutcome::Ok
    );
    let removed = BTreeSet::from([
        tree.plugin_manifest.clone(),
        tree.schema.clone(),
        tree.linux_artifact.clone(),
    ]);
    for _ in 0..removed.len() {
        let FrameBody::ServiceRequest {
            request_id,
            service,
            operation,
            ..
        } = plugin.recv(Duration::from_secs(1)).unwrap().frame.body
        else {
            panic!("expected stale watch cancellation")
        };
        assert_eq!(service, "fs.watch");
        assert_eq!(operation, OP_CANCEL);
        assert!(
            removed.contains(
                &old.iter()
                    .find_map(|(path, id)| (id == &request_id).then_some(path))
                    .unwrap()
                    .clone()
            )
        );
    }
    let new_watches = ready_watches(&mut plugin, 3);
    assert_eq!(
        new_watches.keys().cloned().collect::<BTreeSet<_>>(),
        BTreeSet::from([manifest_v2, schema_v2, artifact_v2.clone()])
    );
    assert!(
        plugin.try_recv().unwrap().is_none(),
        "stable composition and lock subscriptions retain their request ids"
    );

    let tick_one = Frame::service_event(None, "runtime.tick", "tick", json!({"tick": 1}));
    assert_eq!(plugin.send(Lane::Data, &tick_one).unwrap(), CallOutcome::Ok);
    let before_artifact_change = assert_apply_command(
        plugin.recv(Duration::from_secs(1)).unwrap().frame.body,
        &tree.manifest,
        &tree.lock,
    );
    acknowledge_apply(&mut plugin, &before_artifact_change, "applied");

    fs::write(&artifact_v2, b"artifact-v3").unwrap();
    assert_eq!(
        plugin
            .send(
                Lane::Data,
                &changed(&artifact_v2, &new_watches[&artifact_v2]),
            )
            .unwrap(),
        CallOutcome::Ok
    );
    let tick_two = Frame::service_event(None, "runtime.tick", "tick", json!({"tick": 2}));
    assert_eq!(plugin.send(Lane::Data, &tick_two).unwrap(), CallOutcome::Ok);
    let after_artifact_change = assert_apply_command(
        plugin.recv(Duration::from_secs(1)).unwrap().frame.body,
        &tree.manifest,
        &tree.lock,
    );
    assert_ne!(after_artifact_change, before_artifact_change);

    assert_eq!(
        plugin
            .send(
                Lane::Data,
                &changed(&tree.linux_artifact, &old[&tree.linux_artifact]),
            )
            .unwrap(),
        CallOutcome::Ok
    );
    let tick_three = Frame::service_event(None, "runtime.tick", "tick", json!({"tick": 3}));
    assert_eq!(
        plugin.send(Lane::Data, &tick_three).unwrap(),
        CallOutcome::Ok
    );
    assert!(
        plugin.try_recv().unwrap().is_none(),
        "an event from the cancelled old artifact cannot dirty the desired plan"
    );
}

#[test]
fn stale_cancel_ack_keeps_the_replacement_subscription_mapped() {
    let tree = package_tree();
    let mut plugin = PluginHarness::start(rsi_meta_plugin_entry_v0).unwrap();
    prepare_and_commit(&mut plugin, &tree, 28);
    let subscriptions = ready_watches(&mut plugin, 5);
    let stale_request_id = subscriptions[&tree.linux_artifact].clone();
    let replacement_artifact = tree.linux_artifact.parent().unwrap().join("provider-v2.so");
    fs::write(&replacement_artifact, b"replacement artifact").unwrap();

    plugin.set_post_outcome(PostFrameOutcome::WouldBlock);
    write_package_manifest(&tree.plugin_manifest, "target/linux/provider-v2.so");
    assert_eq!(
        plugin
            .send(
                Lane::Data,
                &changed(&tree.plugin_manifest, &subscriptions[&tree.plugin_manifest],),
            )
            .unwrap(),
        CallOutcome::Ok
    );
    write_package_manifest(&tree.plugin_manifest, "target/linux/provider.so");
    assert_eq!(
        plugin
            .send(
                Lane::Data,
                &changed(&tree.plugin_manifest, &subscriptions[&tree.plugin_manifest],),
            )
            .unwrap(),
        CallOutcome::Ok
    );
    assert!(plugin.try_recv().unwrap().is_none());

    plugin.set_post_outcome(PostFrameOutcome::Accepted);
    assert_eq!(
        plugin
            .send(
                Lane::Data,
                &Frame::service_event(Some(stale_request_id), "fs.watch", EVENT_CANCEL, json!({}),),
            )
            .unwrap(),
        CallOutcome::Ok
    );
    let (replacement_request_id, replacement_path) =
        watch_path(plugin.recv(Duration::from_secs(1)).unwrap().frame.body);
    assert_eq!(replacement_path, tree.linux_artifact);
    assert_credit(
        plugin.recv(Duration::from_secs(1)).unwrap().frame.body,
        &replacement_request_id,
    );

    assert_eq!(
        plugin
            .send(
                Lane::Data,
                &changed(&tree.plugin_manifest, &subscriptions[&tree.plugin_manifest],),
            )
            .unwrap(),
        CallOutcome::Ok
    );
    assert!(
        plugin.try_recv().unwrap().is_none(),
        "the replacement watch must remain addressable instead of being duplicated"
    );
}

#[test]
fn manifest_declares_every_runtime_and_host_dependency() {
    let manifest: toml::Value = toml::from_str(include_str!("../plugin.toml")).unwrap();
    let capabilities = manifest["capabilities"].as_array().unwrap();
    assert!(
        capabilities
            .iter()
            .any(|value| value.as_str() == Some("fs.read")),
        "reading composition, lock, schema, and artifact files requires fs.read"
    );
    let tick = manifest["injects"]
        .as_array()
        .unwrap()
        .iter()
        .find(|inject| inject["contract"].as_str() == Some("runtime.tick"))
        .unwrap();
    assert_eq!(
        tick["required"].as_bool(),
        Some(true),
        "the consumer cannot flush dirty state without runtime.tick"
    );
}

#[test]
fn content_derived_command_id_is_stable_across_restart_and_changes_with_desired_bytes() {
    let tree = package_tree();
    let first_id = {
        let mut plugin = PluginHarness::start(rsi_meta_plugin_entry_v0).unwrap();
        prepare_and_commit(&mut plugin, &tree, 26);
        let watches = ready_watches(&mut plugin, 5);
        fs::write(&tree.linux_artifact, b"desired-v2").unwrap();
        assert_eq!(
            plugin
                .send(
                    Lane::Data,
                    &changed(&tree.linux_artifact, &watches[&tree.linux_artifact]),
                )
                .unwrap(),
            CallOutcome::Ok
        );
        let tick = Frame::service_event(None, "runtime.tick", "tick", json!({"tick": 1}));
        assert_eq!(plugin.send(Lane::Data, &tick).unwrap(), CallOutcome::Ok);
        assert_apply_command(
            plugin.recv(Duration::from_secs(1)).unwrap().frame.body,
            &tree.manifest,
            &tree.lock,
        )
    };

    let mut restarted = PluginHarness::start(rsi_meta_plugin_entry_v0).unwrap();
    prepare_and_commit(&mut restarted, &tree, 26);
    let watches = ready_watches(&mut restarted, 5);
    assert_eq!(
        restarted
            .send(
                Lane::Data,
                &changed(&tree.linux_artifact, &watches[&tree.linux_artifact]),
            )
            .unwrap(),
        CallOutcome::Ok
    );
    let first_tick = Frame::service_event(None, "runtime.tick", "tick", json!({"tick": 1}));
    assert_eq!(
        restarted.send(Lane::Data, &first_tick).unwrap(),
        CallOutcome::Ok
    );
    let replay_id = assert_apply_command(
        restarted.recv(Duration::from_secs(1)).unwrap().frame.body,
        &tree.manifest,
        &tree.lock,
    );
    assert_eq!(replay_id, first_id, "same desired bytes replay the same id");
    acknowledge_apply(&mut restarted, &replay_id, "applied");

    fs::write(&tree.linux_artifact, b"desired-v3").unwrap();
    assert_eq!(
        restarted
            .send(
                Lane::Data,
                &changed(&tree.linux_artifact, &watches[&tree.linux_artifact]),
            )
            .unwrap(),
        CallOutcome::Ok
    );
    let second_tick = Frame::service_event(None, "runtime.tick", "tick", json!({"tick": 2}));
    assert_eq!(
        restarted.send(Lane::Data, &second_tick).unwrap(),
        CallOutcome::Ok
    );
    let changed_id = assert_apply_command(
        restarted.recv(Duration::from_secs(1)).unwrap().frame.body,
        &tree.manifest,
        &tree.lock,
    );
    assert_ne!(
        changed_id, first_id,
        "new desired bytes need a new command id"
    );
}

#[test]
fn durable_control_backpressure_keeps_dirty_state_for_same_tick_retry() {
    let tree = package_tree();
    let mut plugin = PluginHarness::start(rsi_meta_plugin_entry_v0).unwrap();
    prepare_and_commit(&mut plugin, &tree, 27);
    let watches = ready_watches(&mut plugin, 5);
    fs::write(&tree.linux_artifact, b"backpressured-desired").unwrap();
    assert_eq!(
        plugin
            .send(
                Lane::Data,
                &changed(&tree.linux_artifact, &watches[&tree.linux_artifact]),
            )
            .unwrap(),
        CallOutcome::Ok
    );
    let tick = Frame::service_event(None, "runtime.tick", "tick", json!({"tick": 9}));
    plugin.set_post_outcome(PostFrameOutcome::WouldBlock);
    assert_eq!(
        plugin.send(Lane::Data, &tick).unwrap(),
        CallOutcome::Ok,
        "control backpressure is retryable, not a plugin callback failure"
    );
    plugin.set_post_outcome(PostFrameOutcome::Accepted);
    assert_eq!(plugin.send(Lane::Data, &tick).unwrap(), CallOutcome::Ok);
    let command_id = assert_apply_command(
        plugin.recv(Duration::from_secs(1)).unwrap().frame.body,
        &tree.manifest,
        &tree.lock,
    );
    assert!(command_id.starts_with("hmr-v0-"));
}

#[test]
fn older_rejection_does_not_clear_a_newer_dirty_content_identity() {
    let tree = package_tree();
    let mut plugin = PluginHarness::start(rsi_meta_plugin_entry_v0).unwrap();
    prepare_and_commit(&mut plugin, &tree, 28);
    let watches = ready_watches(&mut plugin, 5);

    fs::write(&tree.linux_artifact, b"candidate-a").unwrap();
    assert_eq!(
        plugin
            .send(
                Lane::Data,
                &changed(&tree.linux_artifact, &watches[&tree.linux_artifact]),
            )
            .unwrap(),
        CallOutcome::Ok
    );
    assert_eq!(
        plugin
            .send(
                Lane::Data,
                &Frame::service_event(None, "runtime.tick", "tick", json!({"tick": 1})),
            )
            .unwrap(),
        CallOutcome::Ok
    );
    let first_id = assert_apply_command(
        plugin.recv(Duration::from_secs(1)).unwrap().frame.body,
        &tree.manifest,
        &tree.lock,
    );

    fs::write(&tree.linux_artifact, b"candidate-b").unwrap();
    assert_eq!(
        plugin
            .send(
                Lane::Data,
                &changed(&tree.linux_artifact, &watches[&tree.linux_artifact]),
            )
            .unwrap(),
        CallOutcome::Ok
    );
    assert_eq!(
        plugin
            .send(
                Lane::Control,
                &Frame::service_event(
                    Some(first_id.clone()),
                    "control.apply-manifest",
                    "rejected",
                    json!({"code": "candidate_rejected"}),
                ),
            )
            .unwrap(),
        CallOutcome::Ok
    );
    assert_eq!(
        plugin
            .send(
                Lane::Data,
                &Frame::service_event(None, "runtime.tick", "tick", json!({"tick": 2})),
            )
            .unwrap(),
        CallOutcome::Ok
    );

    let second_id = assert_apply_command(
        plugin.recv(Duration::from_secs(1)).unwrap().frame.body,
        &tree.manifest,
        &tree.lock,
    );
    assert_ne!(
        second_id, first_id,
        "newer content must retain its own apply"
    );
}

#[test]
fn retired_is_retried_after_control_backpressure() {
    let tree = package_tree();
    let mut plugin = PluginHarness::start(rsi_meta_plugin_entry_v0).unwrap();
    let generation = 29;
    prepare_and_commit(&mut plugin, &tree, generation);
    while plugin.try_recv().unwrap().is_some() {}

    plugin.set_post_outcome(PostFrameOutcome::WouldBlock);
    assert_eq!(
        plugin
            .send(
                Lane::Control,
                &Frame::lifecycle(LifecyclePhase::Retire, generation, None),
            )
            .unwrap(),
        CallOutcome::Ok,
        "Retire must retain its terminal acknowledgement when the control lane is full"
    );
    plugin.set_post_outcome(PostFrameOutcome::Accepted);
    assert_eq!(
        plugin
            .send(
                Lane::Control,
                &Frame::service_event(None, "runtime.tick", "tick", json!({"tick": 1})),
            )
            .unwrap(),
        CallOutcome::Ok
    );
    assert_eq!(
        plugin.recv(Duration::from_secs(1)).unwrap().frame.body,
        FrameBody::Lifecycle {
            phase: LifecyclePhase::Retired,
            generation,
            config: None,
        }
    );
}

fn write_lock(lock: &Path, package_manifest: &Path) {
    fs::write(
        lock,
        format!(
            r#"format_version = 0
target = "x86_64-unknown-linux-gnu"
manifest_sha256 = "0000000000000000000000000000000000000000000000000000000000000000"

[[packages]]
id = "fixture.provider"
version = "0.0.1"
path = "{}"
manifest_sha256 = "1111111111111111111111111111111111111111111111111111111111111111"
artifact_sha256 = "2222222222222222222222222222222222222222222222222222222222222222"
"#,
            package_manifest.display(),
        ),
    )
    .unwrap();
}

fn write_package_manifest(plugin_manifest: &Path, artifact_path: &str) {
    fs::write(
        plugin_manifest,
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
target = "x86_64-unknown-linux-gnu"
path = "{artifact_path}"

[[artifacts]]
target = "aarch64-apple-darwin"
path = "target/macos/provider.dylib"
"#,
        ),
    )
    .unwrap();
}

#[test]
fn changed_burst_is_coalesced_once_per_runtime_tick() {
    let tree = package_tree();
    let mut plugin = PluginHarness::start(rsi_meta_plugin_entry_v0).unwrap();
    prepare_and_commit(&mut plugin, &tree, 22);
    let subscriptions = ready_watches(&mut plugin, 5);

    for path in [&tree.lock, &tree.plugin_manifest, &tree.linux_artifact] {
        assert_eq!(
            plugin
                .send(Lane::Data, &changed(path, &subscriptions[path]))
                .unwrap(),
            CallOutcome::Ok
        );
    }
    assert!(
        plugin.try_recv().unwrap().is_none(),
        "file changes only mark the apply dirty before a tick"
    );

    let tick_one = Frame::service_event(None, "runtime.tick", "tick", json!({"tick": 1}));
    assert_eq!(plugin.send(Lane::Data, &tick_one).unwrap(), CallOutcome::Ok);
    let first_command_id = assert_apply_command(
        plugin.recv(Duration::from_secs(1)).unwrap().frame.body,
        &tree.manifest,
        &tree.lock,
    );
    assert!(first_command_id.starts_with("hmr-v0-"));
    assert!(plugin.try_recv().unwrap().is_none());

    acknowledge_apply(&mut plugin, &first_command_id, "failed");

    assert_eq!(
        plugin
            .send(
                Lane::Data,
                &changed(&tree.schema, &subscriptions[&tree.schema]),
            )
            .unwrap(),
        CallOutcome::Ok
    );
    assert_eq!(plugin.send(Lane::Data, &tick_one).unwrap(), CallOutcome::Ok);
    assert!(
        plugin.try_recv().unwrap().is_none(),
        "a duplicate tick cannot flush a second command"
    );

    let tick_two = Frame::service_event(None, "runtime.tick", "tick", json!({"tick": 2}));
    assert_eq!(plugin.send(Lane::Data, &tick_two).unwrap(), CallOutcome::Ok);
    let second_command_id = assert_apply_command(
        plugin.recv(Duration::from_secs(1)).unwrap().frame.body,
        &tree.manifest,
        &tree.lock,
    );
    assert_eq!(
        second_command_id, first_command_id,
        "unchanged desired bytes retain the durable id across ticks"
    );
    assert!(plugin.try_recv().unwrap().is_none());
}

fn assert_apply_command(body: FrameBody, manifest: &Path, lock: &Path) -> String {
    let FrameBody::DurableCommand {
        command_id,
        command:
            DurableCommand::ApplyManifestPath {
                manifest_path,
                lock_path,
            },
    } = body
    else {
        panic!("expected durable apply command")
    };
    assert_eq!(&manifest_path, manifest);
    assert_eq!(&lock_path, lock);
    command_id
}

fn acknowledge_apply(plugin: &mut PluginHarness, command_id: &str, event: &str) {
    assert_eq!(
        plugin
            .send(
                Lane::Control,
                &Frame::service_event(
                    Some(command_id.to_owned()),
                    "control.apply-manifest",
                    event,
                    json!({}),
                ),
            )
            .unwrap(),
        CallOutcome::Ok
    );
}
