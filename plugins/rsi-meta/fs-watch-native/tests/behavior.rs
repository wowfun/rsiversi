use std::fs;
use std::path::Path;
use std::time::{Duration, Instant};

use rsi_meta_frame_contract::{
    EVENT_DATA, Frame, FrameBody, LifecyclePhase, OP_CREDIT, OP_OPEN, RUNTIME_TICK_EVENT,
    RUNTIME_TICK_SERVICE,
};
use rsi_meta_loader::PluginManifest;
use rsi_meta_plugin::{CallOutcome, Lane, PostFrameOutcome};
use rsi_meta_plugin_fs_watch_native::rsi_meta_plugin_entry_v0;
use rsi_meta_plugin_testkit::PluginHarness;
use serde_json::{Value, json};
use tempfile::TempDir;

fn decode_data(body: FrameBody) -> Value {
    let FrameBody::ServiceEvent { event, payload, .. } = body else {
        panic!("expected service event")
    };
    assert_eq!(event, EVENT_DATA);
    let bytes = payload
        .as_array()
        .unwrap()
        .iter()
        .map(|byte| u8::try_from(byte.as_u64().unwrap()).unwrap())
        .collect::<Vec<_>>();
    serde_json::from_slice(&bytes).unwrap()
}

fn recv_changed(plugin: &PluginHarness, watched: &Path) -> Value {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        let captured = plugin.recv(remaining).unwrap();
        let event = decode_data(captured.frame.body);
        if event["type"] == "changed" && event["path"] == watched.to_string_lossy().as_ref() {
            return event;
        }
    }
}

fn recv_change_matching(
    plugin: &PluginHarness,
    watched: &Path,
    expected_change: &str,
    observed: &mut Vec<Value>,
) -> Value {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        let captured = plugin.recv(remaining).unwrap_or_else(|error| {
            panic!(
                "waiting for {expected_change:?} on {} failed after events {observed:?}: {error}",
                watched.display()
            )
        });
        let event = decode_data(captured.frame.body);
        if event["path"] != watched.to_string_lossy().as_ref() {
            continue;
        }
        observed.push(event.clone());
        if event["type"] == "changed" && event["change"] == expected_change {
            return event;
        }
    }
}

fn atomic_replace(path: &Path, suffix: &str, bytes: &[u8]) {
    let replacement = path.with_extension(suffix);
    fs::write(&replacement, bytes).unwrap();
    fs::rename(replacement, path).unwrap();
}

#[test]
fn manifest_declares_tick_for_backpressure_retries() {
    let manifest = PluginManifest::from_toml(include_str!("../plugin.toml")).unwrap();
    let tick = manifest
        .injects
        .iter()
        .find(|inject| inject.contract == "runtime.tick")
        .expect("native delivery cannot retry host backpressure without runtime.tick");
    assert!(tick.required);
}

#[test]
fn runtime_tick_retries_a_change_after_host_backpressure() {
    let temp = TempDir::new().unwrap();
    let watched = temp.path().join("watched.toml");
    fs::write(&watched, b"before").unwrap();
    let mut plugin = start_native_watch(&watched, "native-backpressure");

    plugin.set_post_outcomes([PostFrameOutcome::WouldBlock]);
    fs::write(&watched, b"after").unwrap();

    let deadline = Instant::now() + Duration::from_secs(5);
    let changed = loop {
        let tick = Frame::service_event(
            None,
            RUNTIME_TICK_SERVICE,
            RUNTIME_TICK_EVENT,
            json!({"tick": 1}),
        );
        assert_eq!(plugin.send(Lane::Control, &tick).unwrap(), CallOutcome::Ok);
        let remaining = deadline.saturating_duration_since(Instant::now());
        assert!(!remaining.is_zero(), "native change was not retried");
        if let Ok(captured) = plugin.recv(remaining.min(Duration::from_millis(20))) {
            break decode_data(captured.frame.body);
        }
    };
    assert_eq!(changed["type"], "changed");
    assert_eq!(changed["path"], watched.to_string_lossy().as_ref());
}

fn start_native_watch(watched: &Path, request_id: &str) -> PluginHarness {
    let mut plugin = PluginHarness::start(rsi_meta_plugin_entry_v0).unwrap();
    assert_eq!(
        plugin
            .send(
                Lane::Control,
                &Frame::lifecycle(
                    LifecyclePhase::Prepare,
                    13,
                    Some(json!({"recursive": false})),
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
            generation: 13,
            config: None,
        }
    );
    assert_eq!(
        plugin
            .send(
                Lane::Control,
                &Frame::lifecycle(LifecyclePhase::Committed, 13, None),
            )
            .unwrap(),
        CallOutcome::Ok
    );
    assert_eq!(
        plugin
            .send(
                Lane::Data,
                &Frame::service_request(
                    request_id,
                    "fs.watch",
                    OP_OPEN,
                    json!({"consumer": "hmr", "sequence": 0, "path": watched}),
                ),
            )
            .unwrap(),
        CallOutcome::Ok
    );
    assert_eq!(
        plugin
            .send(
                Lane::Data,
                &Frame::service_request(
                    request_id,
                    "fs.watch",
                    OP_CREDIT,
                    json!({"bytes": 1024 * 1024}),
                ),
            )
            .unwrap(),
        CallOutcome::Ok
    );
    let ready = decode_data(plugin.recv(Duration::from_secs(1)).unwrap().frame.body);
    assert_eq!(ready["type"], "ready");
    assert_eq!(ready["path"], watched.to_string_lossy().as_ref());
    plugin
}

#[test]
fn native_watch_is_credit_bounded_filters_neighbors_and_survives_two_atomic_replacements() {
    let temp = TempDir::new().unwrap();
    let watched = temp.path().join("rsi-meta.toml");
    fs::write(&watched, b"before").unwrap();

    let mut plugin = PluginHarness::start(rsi_meta_plugin_entry_v0).unwrap();
    assert_eq!(
        plugin
            .send(
                Lane::Control,
                &Frame::lifecycle(
                    LifecyclePhase::Prepare,
                    12,
                    Some(json!({"recursive": false})),
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
            generation: 12,
            config: None,
        }
    );
    assert_eq!(
        plugin
            .send(
                Lane::Control,
                &Frame::lifecycle(LifecyclePhase::Committed, 12, None),
            )
            .unwrap(),
        CallOutcome::Ok
    );

    let open = Frame::service_request(
        "native-1",
        "fs.watch",
        OP_OPEN,
        json!({"consumer": "hmr", "sequence": 0, "path": watched}),
    );
    assert_eq!(plugin.send(Lane::Data, &open).unwrap(), CallOutcome::Ok);
    assert!(plugin.try_recv().unwrap().is_none());

    let insufficient =
        Frame::service_request("native-1", "fs.watch", OP_CREDIT, json!({"bytes": 1}));
    assert_eq!(
        plugin.send(Lane::Data, &insufficient).unwrap(),
        CallOutcome::Ok
    );
    assert!(plugin.try_recv().unwrap().is_none());

    let credit = Frame::service_request(
        "native-1",
        "fs.watch",
        OP_CREDIT,
        json!({"bytes": 1024 * 1024}),
    );
    assert_eq!(plugin.send(Lane::Data, &credit).unwrap(), CallOutcome::Ok);
    let ready = decode_data(plugin.recv(Duration::from_secs(1)).unwrap().frame.body);
    assert_eq!(ready["type"], "ready");
    assert_eq!(ready["path"], watched.to_string_lossy().as_ref());
    assert_eq!(ready["backend"], "native");

    fs::write(temp.path().join("unrelated.toml"), b"neighbor noise").unwrap();
    for (suffix, bytes) in [
        ("replace-one", b"after-one".as_slice()),
        ("replace-two", b"after-two".as_slice()),
    ] {
        atomic_replace(&watched, suffix, bytes);
        let changed = recv_changed(&plugin, &watched);
        assert_eq!(changed["backend"], "native");
        assert!(matches!(
            changed["change"].as_str(),
            Some("modified" | "created" | "removed")
        ));
    }
}

#[cfg(unix)]
#[test]
fn native_watch_observes_edits_through_a_symlink() {
    use std::os::unix::fs::symlink;

    let temp = TempDir::new().unwrap();
    let target_dir = temp.path().join("target");
    let link_dir = temp.path().join("links");
    fs::create_dir_all(&target_dir).unwrap();
    fs::create_dir_all(&link_dir).unwrap();
    let target = target_dir.join("rsi-meta.toml");
    let watched = link_dir.join("rsi-meta.toml");
    fs::write(&target, b"before").unwrap();
    symlink(&target, &watched).unwrap();

    let plugin = start_native_watch(&watched, "native-symlink");
    fs::write(&target, b"after target edit").unwrap();
    let changed = recv_changed(&plugin, &watched);
    assert_eq!(changed["backend"], "native");
}

#[test]
fn native_watch_reports_removal_when_the_watched_parent_is_deleted() {
    let temp = TempDir::new().unwrap();
    let parent = temp.path().join("watched-parent");
    fs::create_dir(&parent).unwrap();
    let watched = parent.join("rsi-meta.toml");
    fs::write(&watched, b"before").unwrap();

    let plugin = start_native_watch(&watched, "native-parent-removal");
    fs::remove_dir_all(&parent).unwrap();
    let changed = recv_changed(&plugin, &watched);
    assert_eq!(changed["change"], "removed");
    assert_eq!(changed["backend"], "native");
}

#[cfg(unix)]
#[test]
fn native_watch_re_resolves_after_an_atomic_symlink_retarget() {
    use std::os::unix::fs::symlink;

    let temp = TempDir::new().unwrap();
    let first_dir = temp.path().join("first-target");
    let second_dir = temp.path().join("second-target");
    let link_dir = temp.path().join("links");
    fs::create_dir_all(&first_dir).unwrap();
    fs::create_dir_all(&second_dir).unwrap();
    fs::create_dir_all(&link_dir).unwrap();
    let first_target = first_dir.join("rsi-meta.toml");
    let second_target = second_dir.join("rsi-meta.toml");
    let watched = link_dir.join("rsi-meta.toml");
    fs::write(&first_target, b"first target").unwrap();
    fs::write(&second_target, b"second target").unwrap();
    symlink(&first_target, &watched).unwrap();

    let plugin = start_native_watch(&watched, "native-symlink-retarget");
    let replacement_link = link_dir.join("replacement-link");
    symlink(&second_target, &replacement_link).unwrap();
    fs::rename(&replacement_link, &watched).unwrap();
    let retargeted = recv_changed(&plugin, &watched);
    assert_eq!(retargeted["backend"], "native");

    fs::write(&second_target, b"second target modified after retarget").unwrap();
    let changed = recv_changed(&plugin, &watched);
    assert_eq!(changed["backend"], "native");
}

#[test]
fn native_watch_re_registers_after_parent_delete_and_atomic_recreation() {
    let temp = TempDir::new().unwrap();
    let parent = temp.path().join("watched-parent");
    fs::create_dir(&parent).unwrap();
    let watched = parent.join("rsi-meta.toml");
    fs::write(&watched, b"before").unwrap();

    let plugin = start_native_watch(&watched, "native-parent-recreate");
    let mut observed = Vec::new();
    fs::remove_dir_all(&parent).unwrap();
    let removed = recv_change_matching(&plugin, &watched, "removed", &mut observed);
    assert_eq!(removed["change"], "removed");

    let replacement_parent = temp.path().join("replacement-parent");
    fs::create_dir(&replacement_parent).unwrap();
    fs::write(replacement_parent.join("rsi-meta.toml"), b"recreated").unwrap();
    fs::rename(&replacement_parent, &parent).unwrap();
    let recreated = recv_change_matching(&plugin, &watched, "created", &mut observed);
    assert_eq!(recreated["change"], "created");

    fs::write(&watched, b"modified after recreation").unwrap();
    let changed = recv_change_matching(&plugin, &watched, "modified", &mut observed);
    assert_eq!(changed["backend"], "native");
    assert_eq!(changed["change"], "modified");
    assert_eq!(
        observed
            .iter()
            .filter_map(|event| event["change"].as_str())
            .collect::<Vec<_>>(),
        ["removed", "created", "modified"],
        "the target's complete change sequence must follow fingerprint existence transitions"
    );
}
