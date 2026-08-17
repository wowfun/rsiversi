use std::fs;
use std::time::Duration;

use rsi_meta_plugin::{CallOutcome, Lane};
use rsi_meta_plugin::{Frame, FrameBody, LifecyclePhase, OP_CREDIT, OP_OPEN};
use rsi_meta_plugin_fs_watch_polling::rsi_meta_plugin_entry_v0;
use rsi_meta_plugin_testkit::PluginHarness;
use serde_json::{Value, json};
use tempfile::TempDir;

fn decode_data(body: FrameBody) -> Value {
    let FrameBody::ServiceDataEvent { payload, .. } = body else {
        panic!("expected service DATA event")
    };
    serde_json::from_slice(&payload).unwrap()
}

fn recv_data(plugin: &PluginHarness) -> Value {
    decode_data(
        plugin
            .recv(Duration::from_secs(5))
            .expect("polling worker frame")
            .frame
            .body,
    )
}

#[test]
fn polling_stream_withholds_data_until_credit_then_flushes_ready_and_change_fifo() {
    let temp = TempDir::new().unwrap();
    let watched = temp.path().join("manifest.toml");
    let multi_chunk_contents = (0..200_000)
        .map(|index| u8::try_from(index % 251).unwrap())
        .collect::<Vec<_>>();
    fs::write(&watched, multi_chunk_contents).unwrap();

    let mut plugin = PluginHarness::start(rsi_meta_plugin_entry_v0).unwrap();
    assert_eq!(
        plugin
            .send(
                Lane::Control,
                &Frame::lifecycle(
                    LifecyclePhase::Prepare,
                    11,
                    Some(json!({"hash_contents": true})),
                ),
            )
            .unwrap(),
        CallOutcome::Ok
    );
    let prepared = plugin.try_recv().unwrap().unwrap();
    assert_eq!(prepared.lane, Lane::Control);
    assert_eq!(
        prepared.frame.body,
        FrameBody::Lifecycle {
            phase: LifecyclePhase::Prepared,
            generation: 11,
            config: None,
        }
    );
    assert_eq!(
        plugin
            .send(
                Lane::Control,
                &Frame::lifecycle(LifecyclePhase::Committed, 11, None),
            )
            .unwrap(),
        CallOutcome::Ok
    );

    let open = Frame::service_request(
        "watch-1",
        "fs.watch",
        OP_OPEN,
        json!({"consumer": "hmr", "sequence": 0, "path": watched}),
    );
    assert_eq!(plugin.send(Lane::Data, &open).unwrap(), CallOutcome::Ok);
    assert!(plugin.try_recv().unwrap().is_none());

    let one_byte = Frame::service_request("watch-1", "fs.watch", OP_CREDIT, json!({"bytes": 1}));
    assert_eq!(plugin.send(Lane::Data, &one_byte).unwrap(), CallOutcome::Ok);
    assert!(plugin.try_recv().unwrap().is_none());

    let enough_for_one_frame = Frame::service_request(
        "watch-1",
        "fs.watch",
        OP_CREDIT,
        json!({"bytes": 1024 * 1024}),
    );
    assert_eq!(
        plugin.send(Lane::Data, &enough_for_one_frame).unwrap(),
        CallOutcome::Ok
    );

    let ready = recv_data(&plugin);
    assert_eq!(ready["type"], "ready");
    assert_eq!(ready["path"], watched.to_string_lossy().as_ref());
    assert_eq!(
        ready["snapshot"]["content_sha256"],
        "e24bc62381f1224fbbb74688663f8f9743b9680b193edd666835e97b06e730eb"
    );

    fs::write(&watched, b"bravo").unwrap();
    let tick = Frame::service_event(None, "runtime.tick", "tick", json!({"tick": 23}));
    assert_eq!(plugin.send(Lane::Control, &tick).unwrap(), CallOutcome::Ok);
    let changed = recv_data(&plugin);
    assert_eq!(changed["type"], "changed");
    assert_eq!(changed["tick"], 23);
    assert_eq!(
        changed["previous"]["content_sha256"],
        ready["snapshot"]["content_sha256"]
    );
    assert_eq!(
        changed["current"]["content_sha256"],
        "f144a6907dc4284d1f9fe6a7d9b9ff53c02c1d07ba68f24d413d7ff7f757a782"
    );
    assert!(plugin.try_recv().unwrap().is_none());
}

#[test]
fn content_hashing_rejects_an_oversized_sparse_watch_without_reading_it() {
    let temp = TempDir::new().unwrap();
    let watched = temp.path().join("oversized.bin");
    fs::File::create(&watched)
        .unwrap()
        .set_len(257 * 1024 * 1024)
        .unwrap();
    let mut plugin = PluginHarness::start(rsi_meta_plugin_entry_v0).unwrap();
    assert_eq!(
        plugin
            .send(
                Lane::Control,
                &Frame::lifecycle(
                    LifecyclePhase::Prepare,
                    14,
                    Some(json!({"hash_contents": true})),
                ),
            )
            .unwrap(),
        CallOutcome::Ok
    );
    let _prepared = plugin.try_recv().unwrap().unwrap();
    assert_eq!(
        plugin
            .send(
                Lane::Control,
                &Frame::lifecycle(LifecyclePhase::Committed, 14, None),
            )
            .unwrap(),
        CallOutcome::Ok
    );

    let open = Frame::service_request(
        "oversized-watch",
        "fs.watch",
        OP_OPEN,
        json!({"consumer": "test", "sequence": 0, "path": watched}),
    );
    assert_eq!(plugin.send(Lane::Data, &open).unwrap(), CallOutcome::Failed);
    assert!(plugin.try_recv().unwrap().is_none());
}

#[cfg(unix)]
#[test]
fn one_path_io_error_does_not_starve_later_watch_streams() {
    use std::os::unix::fs::symlink;

    let temp = TempDir::new().unwrap();
    let broken = temp.path().join("broken.toml");
    let healthy = temp.path().join("healthy.toml");
    fs::write(&broken, b"before broken").unwrap();
    fs::write(&healthy, b"before healthy").unwrap();

    let mut plugin = PluginHarness::start(rsi_meta_plugin_entry_v0).unwrap();
    assert_eq!(
        plugin
            .send(
                Lane::Control,
                &Frame::lifecycle(
                    LifecyclePhase::Prepare,
                    12,
                    Some(json!({"hash_contents": true})),
                ),
            )
            .unwrap(),
        CallOutcome::Ok
    );
    let prepared = plugin.try_recv().unwrap().unwrap();
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

    for (request_id, path) in [("a-broken", &broken), ("b-healthy", &healthy)] {
        assert_eq!(
            plugin
                .send(
                    Lane::Data,
                    &Frame::service_request(
                        request_id,
                        "fs.watch",
                        OP_OPEN,
                        json!({"consumer": "hmr", "sequence": 0, "path": path}),
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
        assert_eq!(recv_data(&plugin)["type"], "ready");
    }

    fs::remove_file(&broken).unwrap();
    symlink(&broken, &broken).unwrap();
    fs::write(&healthy, b"after healthy").unwrap();
    let tick = Frame::service_event(None, "runtime.tick", "tick", json!({"tick": 24}));
    assert_eq!(
        plugin.send(Lane::Control, &tick).unwrap(),
        CallOutcome::Ok,
        "a per-path metadata error must not abort the shared polling tick"
    );

    let broken_event = recv_data(&plugin);
    assert_eq!(broken_event["type"], "error");
    assert_eq!(broken_event["path"], broken.to_string_lossy().as_ref());
    let healthy_event = recv_data(&plugin);
    assert_eq!(healthy_event["type"], "changed");
    assert_eq!(healthy_event["path"], healthy.to_string_lossy().as_ref());
    assert_eq!(healthy_event["tick"], 24);
}
