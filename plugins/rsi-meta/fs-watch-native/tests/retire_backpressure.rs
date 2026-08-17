use std::fs;
use std::path::{Path, PathBuf};

use rsi_meta_loader::{
    BUILD_TARGET, ContentHash, ExpectedHashes, LoadedPlugin, PluginLoader, PluginMailbox,
    PluginMailboxOptions,
};
use rsi_meta_plugin::{ABI_MAJOR, ABI_MINOR, CallOutcome, Lane};
use rsi_meta_plugin::{
    Frame, FrameBody, LifecyclePhase, OP_CREDIT, OP_OPEN, RUNTIME_TICK_EVENT, RUNTIME_TICK_SERVICE,
};
use serde_json::json;
use tempfile::TempDir;

struct LoadedFixture {
    plugin: LoadedPlugin,
    mailbox: PluginMailbox,
    root: TempDir,
}

fn current_cdylib() -> PathBuf {
    let test_executable = std::env::current_exe().unwrap();
    test_executable.parent().unwrap().join(format!(
        "{}rsi_meta_plugin_fs_watch_native{}",
        std::env::consts::DLL_PREFIX,
        std::env::consts::DLL_SUFFIX,
    ))
}

fn load_with_one_frame_data_lane() -> LoadedFixture {
    let root = TempDir::new().unwrap();
    let artifact_name = format!("plugin{}", std::env::consts::DLL_SUFFIX);
    let artifact = root.path().join(&artifact_name);
    let source = current_cdylib();
    assert!(
        source.is_file(),
        "cargo must build the package cdylib at {}",
        source.display()
    );
    fs::copy(source, &artifact).unwrap();
    let manifest = root.path().join("plugin.toml");
    fs::write(
        &manifest,
        format!(
            r#"format_version = 0
provides = ["fs.watch"]
capabilities = ["fs.read"]

[package]
id = "test.fs-watch-native"
version = "0.0.0"

[host_api]
major = {ABI_MAJOR}
minimum_minor = {ABI_MINOR}

[[artifacts]]
target = "{BUILD_TARGET}"
path = "{artifact_name}"
"#,
        ),
    )
    .unwrap();
    let manifest_bytes = fs::read(&manifest).unwrap();
    let artifact_bytes = fs::read(&artifact).unwrap();
    let loader = PluginLoader::for_current_process(root.path().join("cache"));
    let staged = loader
        .stage(
            &manifest,
            ExpectedHashes::new(
                ContentHash::digest(manifest_bytes),
                ContentHash::digest(artifact_bytes),
            ),
        )
        .unwrap();
    let (plugin, mailbox) = loader
        .load_queued(
            &staged,
            PluginMailboxOptions {
                control_capacity: 1,
                data_capacity: 1,
                max_frame_bytes: 1024 * 1024,
            },
        )
        .unwrap();
    LoadedFixture {
        plugin,
        mailbox,
        root,
    }
}

#[allow(clippy::needless_pass_by_value)] // Call sites construct one-shot protocol frames inline.
fn dispatch(plugin: &mut LoadedPlugin, lane: Lane, frame: Frame) -> CallOutcome {
    plugin.dispatch(lane, &frame.encode().unwrap())
}

fn open_and_credit(plugin: &mut LoadedPlugin, request_id: &str, watched: &Path) {
    assert_eq!(
        dispatch(
            plugin,
            Lane::Data,
            Frame::service_request(
                request_id,
                "fs.watch",
                OP_OPEN,
                json!({"consumer": "retire-test", "sequence": 0, "path": watched}),
            ),
        ),
        CallOutcome::Ok
    );
    assert_eq!(
        dispatch(
            plugin,
            Lane::Data,
            Frame::service_request(
                request_id,
                "fs.watch",
                OP_CREDIT,
                json!({"bytes": 1024 * 1024}),
            ),
        ),
        CallOutcome::Ok
    );
}

#[test]
fn retire_waits_until_every_stream_cancel_survives_data_backpressure() {
    let mut fixture = load_with_one_frame_data_lane();
    let watched = fixture.root.path().join("watched.toml");
    fs::write(&watched, b"watched").unwrap();
    let generation = 51;

    assert_eq!(
        dispatch(
            &mut fixture.plugin,
            Lane::Control,
            Frame::lifecycle(
                LifecyclePhase::Prepare,
                generation,
                Some(json!({"recursive": false})),
            ),
        ),
        CallOutcome::Ok
    );
    let prepared = Frame::decode(fixture.mailbox.try_recv_control().unwrap().payload()).unwrap();
    assert_eq!(
        prepared.body,
        FrameBody::Lifecycle {
            phase: LifecyclePhase::Prepared,
            generation,
            config: None,
        }
    );
    assert_eq!(
        dispatch(
            &mut fixture.plugin,
            Lane::Control,
            Frame::lifecycle(LifecyclePhase::Committed, generation, None),
        ),
        CallOutcome::Ok
    );

    open_and_credit(&mut fixture.plugin, "watch-1", &watched);
    let ready = Frame::decode(fixture.mailbox.try_recv_data().unwrap().payload()).unwrap();
    assert!(matches!(ready.body, FrameBody::ServiceDataEvent { .. }));
    open_and_credit(&mut fixture.plugin, "watch-2", &watched);

    assert_eq!(
        dispatch(
            &mut fixture.plugin,
            Lane::Control,
            Frame::lifecycle(LifecyclePhase::Retire, generation, None),
        ),
        CallOutcome::Ok,
        "a full DATA lane must not fail the Retire callback"
    );
    assert!(
        fixture.mailbox.try_recv_control().is_err(),
        "Retired must wait until the stream terminals are accepted"
    );
    let ready = Frame::decode(fixture.mailbox.try_recv_data().unwrap().payload()).unwrap();
    assert!(matches!(ready.body, FrameBody::ServiceDataEvent { .. }));

    for expected_request in ["watch-1", "watch-2"] {
        assert_eq!(
            dispatch(
                &mut fixture.plugin,
                Lane::Control,
                Frame::service_event(
                    None,
                    RUNTIME_TICK_SERVICE,
                    RUNTIME_TICK_EVENT,
                    json!({"tick": 100}),
                ),
            ),
            CallOutcome::Ok
        );
        let terminal = Frame::decode(fixture.mailbox.try_recv_data().unwrap().payload()).unwrap();
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
    }
    let retired = Frame::decode(fixture.mailbox.try_recv_control().unwrap().payload()).unwrap();
    assert_eq!(
        retired.body,
        FrameBody::Lifecycle {
            phase: LifecyclePhase::Retired,
            generation,
            config: None,
        },
        "Retired follows every accepted stream terminal"
    );
    assert!(fixture.mailbox.try_recv_data().is_err());
}

#[test]
fn retired_is_retried_after_control_backpressure() {
    let mut fixture = load_with_one_frame_data_lane();
    let generation = 53;

    assert_eq!(
        dispatch(
            &mut fixture.plugin,
            Lane::Control,
            Frame::lifecycle(
                LifecyclePhase::Prepare,
                generation,
                Some(json!({"recursive": false})),
            ),
        ),
        CallOutcome::Ok
    );
    assert_eq!(
        dispatch(
            &mut fixture.plugin,
            Lane::Control,
            Frame::lifecycle(LifecyclePhase::Committed, generation, None),
        ),
        CallOutcome::Ok
    );
    assert_eq!(
        dispatch(
            &mut fixture.plugin,
            Lane::Control,
            Frame::lifecycle(LifecyclePhase::Retire, generation, None),
        ),
        CallOutcome::Ok,
        "Retire must retain its terminal acknowledgement when the control lane is full"
    );

    let prepared = Frame::decode(fixture.mailbox.try_recv_control().unwrap().payload()).unwrap();
    assert!(matches!(
        prepared.body,
        FrameBody::Lifecycle {
            phase: LifecyclePhase::Prepared,
            ..
        }
    ));
    assert!(fixture.mailbox.try_recv_control().is_err());

    assert_eq!(
        dispatch(
            &mut fixture.plugin,
            Lane::Control,
            Frame::service_event(
                None,
                RUNTIME_TICK_SERVICE,
                RUNTIME_TICK_EVENT,
                json!({"tick": 1}),
            ),
        ),
        CallOutcome::Ok
    );
    let retired = Frame::decode(fixture.mailbox.try_recv_control().unwrap().payload()).unwrap();
    assert!(matches!(
        retired.body,
        FrameBody::Lifecycle {
            phase: LifecyclePhase::Retired,
            generation: 53,
            ..
        }
    ));
}
