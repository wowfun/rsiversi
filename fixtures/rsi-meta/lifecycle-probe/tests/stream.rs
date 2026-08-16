use std::time::Duration;

use rsi_meta_fixture_lifecycle_probe::rsi_meta_plugin_entry_v0;
use rsi_meta_frame_contract::{
    EVENT_CANCEL, EVENT_CREDIT, EVENT_DATA, EVENT_END, Frame, FrameBody, LifecyclePhase, OP_CANCEL,
    OP_CREDIT, OP_DATA, OP_HALF_CLOSE, OP_OPEN, RUNTIME_TICK_EVENT, RUNTIME_TICK_SERVICE,
    STATE_EVENT_VALUE,
};
use rsi_meta_plugin::{CallOutcome, Lane, PostFrameOutcome};
use rsi_meta_plugin_testkit::{CapturedFrame, HarnessError, PluginHarness};
use serde_json::{Value, json};

const SERVICE: &str = "fixture.lifecycle-probe";

fn tick(sequence: u64) -> Frame {
    Frame::service_event(
        None,
        RUNTIME_TICK_SERVICE,
        RUNTIME_TICK_EVENT,
        json!({"tick": sequence}),
    )
}

fn committed_probe(tag: &str) -> PluginHarness {
    committed_probe_with_fault(tag, "none")
}

fn committed_probe_with_fault(tag: &str, stream_fault: &str) -> PluginHarness {
    let mut plugin = PluginHarness::start(rsi_meta_plugin_entry_v0).unwrap();
    let config = json!({
        "fail_prepare": false,
        "retire_mode": "ack",
        "tag": tag,
        "stream_fault": stream_fault,
    });
    assert_eq!(
        plugin
            .send(
                Lane::Control,
                &Frame::lifecycle(LifecyclePhase::Prepare, 31, Some(config)),
            )
            .unwrap(),
        CallOutcome::Ok
    );
    let read = recv_gate(&plugin);
    let FrameBody::ServiceRequest {
        request_id,
        payload,
        ..
    } = read.frame.body
    else {
        panic!("expected deferred prepare state read")
    };
    assert!(plugin.try_recv().unwrap().is_none());
    assert_eq!(
        plugin
            .send(
                Lane::Data,
                &Frame::service_event(
                    Some(request_id),
                    "state.cas",
                    STATE_EVENT_VALUE,
                    json!({"key": payload["key"], "version": 0, "value": null}),
                ),
            )
            .unwrap(),
        CallOutcome::Ok
    );
    let prepared = recv_gate(&plugin);
    assert_eq!(prepared.lane, Lane::Control);
    assert!(matches!(
        prepared.frame.body,
        FrameBody::Lifecycle {
            phase: LifecyclePhase::Prepared,
            generation: 31,
            config: None,
        }
    ));
    assert_eq!(
        plugin
            .send(
                Lane::Control,
                &Frame::lifecycle(LifecyclePhase::Committed, 31, None),
            )
            .unwrap(),
        CallOutcome::Ok
    );
    plugin
}

#[test]
fn failed_initial_credit_does_not_retain_the_stream_id() {
    let mut plugin = committed_probe("rollback-open");
    let open = Frame::service_request(
        "retry-open",
        SERVICE,
        OP_OPEN,
        json!({"consumer": "backpressure-test", "sequence": 0}),
    );
    plugin.set_post_outcome(PostFrameOutcome::WouldBlock);
    assert_eq!(plugin.send(Lane::Data, &open).unwrap(), CallOutcome::Failed);

    plugin.set_post_outcome(PostFrameOutcome::Accepted);
    assert_eq!(plugin.send(Lane::Data, &open).unwrap(), CallOutcome::Ok);
    assert!(matches!(
        plugin.recv(Duration::from_secs(1)).unwrap().frame.body,
        FrameBody::ServiceEvent { event, .. } if event == EVENT_CREDIT
    ));
}

#[test]
fn adversarial_stream_modes_emit_exactly_one_malformed_event() {
    for (fault, expected_service, expected_event, expected_payload) in [
        (
            "wrong_service",
            "fixture.lifecycle-probe.wrong",
            EVENT_DATA,
            json!([102, 0, 1]),
        ),
        (
            "unknown_event",
            SERVICE,
            "unknown_event",
            json!([102, 0, 1]),
        ),
        (
            "non_byte_data",
            SERVICE,
            EVENT_DATA,
            json!({"not": "bytes"}),
        ),
    ] {
        let mut plugin = committed_probe_with_fault("f", fault);
        let request_id = format!("fault-{fault}");
        assert_eq!(
            plugin
                .send(
                    Lane::Data,
                    &Frame::service_request(
                        &request_id,
                        SERVICE,
                        OP_OPEN,
                        json!({"consumer": "fault-test", "sequence": 0}),
                    ),
                )
                .unwrap(),
            CallOutcome::Ok
        );
        assert_event(
            recv_gate(&plugin),
            &request_id,
            EVENT_CREDIT,
            json!({"bytes": 1024 * 1024}),
        );
        assert_eq!(
            plugin
                .send(
                    Lane::Data,
                    &Frame::service_request(
                        &request_id,
                        SERVICE,
                        OP_CREDIT,
                        json!({"bytes": 1024 * 1024}),
                    ),
                )
                .unwrap(),
            CallOutcome::Ok
        );
        assert_eq!(
            plugin
                .send(
                    Lane::Data,
                    &Frame::service_request(&request_id, SERVICE, OP_DATA, json!([1])),
                )
                .unwrap(),
            CallOutcome::Ok
        );
        assert_eq!(
            recv_gate(&plugin),
            CapturedFrame {
                lane: Lane::Data,
                frame: Frame::service_event(
                    Some(request_id),
                    expected_service,
                    expected_event,
                    expected_payload,
                ),
            }
        );
        assert!(plugin.try_recv().unwrap().is_none());
    }
}

#[test]
fn malformed_json_mode_posts_raw_invalid_bytes_through_the_public_abi() {
    let mut plugin = committed_probe_with_fault("f", "malformed_json");
    let request_id = "fault-malformed-json";
    assert_eq!(
        plugin
            .send(
                Lane::Data,
                &Frame::service_request(
                    request_id,
                    SERVICE,
                    OP_OPEN,
                    json!({"consumer": "fault-test", "sequence": 0}),
                ),
            )
            .unwrap(),
        CallOutcome::Ok
    );
    assert_event(
        recv_gate(&plugin),
        request_id,
        EVENT_CREDIT,
        json!({"bytes": 1024 * 1024}),
    );
    assert_eq!(
        plugin
            .send(
                Lane::Data,
                &Frame::service_request(request_id, SERVICE, OP_CREDIT, json!({"bytes": 1}),),
            )
            .unwrap(),
        CallOutcome::Ok
    );
    assert_eq!(
        plugin
            .send(
                Lane::Data,
                &Frame::service_request(request_id, SERVICE, OP_DATA, json!([1])),
            )
            .unwrap(),
        CallOutcome::Ok
    );
    assert!(matches!(
        plugin.recv(Duration::from_secs(1)),
        Err(HarnessError::Frame(_))
    ));
}

fn recv_gate(plugin: &PluginHarness) -> CapturedFrame {
    plugin
        .recv(Duration::from_secs(1))
        .expect("plugin must cross the frame channel gate")
}

fn assert_event(captured: CapturedFrame, request_id: &str, event: &str, payload: Value) {
    let CapturedFrame { lane, frame } = captured;
    assert_eq!(lane, Lane::Data);
    assert_eq!(
        frame.body,
        FrameBody::ServiceEvent {
            request_id: Some(request_id.to_owned()),
            service: SERVICE.to_owned(),
            event: event.to_owned(),
            payload,
        }
    );
}

fn open_probe(plugin: &mut PluginHarness, request_id: &str, output_credit: u64) {
    let open = Frame::service_request(
        request_id,
        SERVICE,
        OP_OPEN,
        json!({"consumer": "backpressure-test", "sequence": 0}),
    );
    assert_eq!(plugin.send(Lane::Data, &open).unwrap(), CallOutcome::Ok);
    assert_event(
        recv_gate(plugin),
        request_id,
        EVENT_CREDIT,
        json!({"bytes": 1024 * 1024}),
    );
    let credit = Frame::service_request(
        request_id,
        SERVICE,
        OP_CREDIT,
        json!({"bytes": output_credit}),
    );
    assert_eq!(plugin.send(Lane::Data, &credit).unwrap(), CallOutcome::Ok);
}

#[test]
fn data_would_block_is_retained_and_charged_once() {
    let mut plugin = committed_probe("tag");
    let output = json!([116, 97, 103, 0, 7]);
    let encoded = serde_json::to_vec(&output).unwrap().len() as u64;
    open_probe(&mut plugin, "blocked-data", encoded);

    plugin.set_post_outcome(PostFrameOutcome::WouldBlock);
    let data = Frame::service_request("blocked-data", SERVICE, OP_DATA, json!([7]));
    assert_eq!(
        plugin.send(Lane::Data, &data).unwrap(),
        CallOutcome::Ok,
        "temporary DATA backpressure must not fault the plugin callback"
    );
    assert!(plugin.try_recv().unwrap().is_none());

    plugin.set_post_outcome(PostFrameOutcome::Accepted);
    assert_eq!(
        plugin.send(Lane::Control, &tick(1)).unwrap(),
        CallOutcome::Ok
    );
    assert_event(recv_gate(&plugin), "blocked-data", EVENT_DATA, output);
    assert!(plugin.try_recv().unwrap().is_none());
}

#[test]
fn end_would_block_keeps_the_stream_until_retry() {
    let mut plugin = committed_probe("tag");
    open_probe(&mut plugin, "blocked-end", 0);
    let half_close = Frame::service_request(
        "blocked-end",
        SERVICE,
        OP_HALF_CLOSE,
        json!({"sequence": 1}),
    );

    plugin.set_post_outcome(PostFrameOutcome::WouldBlock);
    assert_eq!(
        plugin.send(Lane::Data, &half_close).unwrap(),
        CallOutcome::Ok,
        "temporary END backpressure must not fault the plugin callback"
    );
    assert!(plugin.try_recv().unwrap().is_none());

    plugin.set_post_outcome(PostFrameOutcome::Accepted);
    assert_eq!(
        plugin.send(Lane::Control, &tick(1)).unwrap(),
        CallOutcome::Ok
    );
    assert_event(recv_gate(&plugin), "blocked-end", EVENT_END, json!({}));
    assert!(plugin.try_recv().unwrap().is_none());
}

#[test]
fn stream_orders_open_credit_tagged_data_and_half_close() {
    let mut plugin = committed_probe("blue");

    let open = Frame::service_request(
        "stream-1",
        SERVICE,
        OP_OPEN,
        json!({"consumer": "hmr-test", "sequence": 0}),
    );
    assert_eq!(plugin.send(Lane::Data, &open).unwrap(), CallOutcome::Ok);
    assert_event(
        recv_gate(&plugin),
        "stream-1",
        EVENT_CREDIT,
        json!({"bytes": 1024 * 1024}),
    );

    // DATA remains a JSON byte array: UTF-8 tag, zero separator, request bytes.
    let expected = json!([98, 108, 117, 101, 0, 7, 8]);
    let encoded_len = serde_json::to_vec(&expected).unwrap().len() as u64;
    let credit = Frame::service_request(
        "stream-1",
        SERVICE,
        OP_CREDIT,
        json!({"bytes": encoded_len}),
    );
    assert_eq!(plugin.send(Lane::Data, &credit).unwrap(), CallOutcome::Ok);
    assert!(plugin.try_recv().unwrap().is_none());

    let data = Frame::service_request("stream-1", SERVICE, OP_DATA, json!([7, 8]));
    assert_eq!(plugin.send(Lane::Data, &data).unwrap(), CallOutcome::Ok);
    assert_event(recv_gate(&plugin), "stream-1", EVENT_DATA, expected);

    let half_close =
        Frame::service_request("stream-1", SERVICE, OP_HALF_CLOSE, json!({"sequence": 1}));
    assert_eq!(
        plugin.send(Lane::Data, &half_close).unwrap(),
        CallOutcome::Ok
    );
    assert_event(recv_gate(&plugin), "stream-1", EVENT_END, json!({}));
    assert_eq!(plugin.send(Lane::Data, &data).unwrap(), CallOutcome::Failed);
}

#[test]
fn cancel_is_terminal_and_preserves_the_reason() {
    let mut plugin = committed_probe("cancelled");
    let open = Frame::service_request(
        "stream-2",
        SERVICE,
        OP_OPEN,
        json!({"consumer": "hmr-test", "sequence": 0}),
    );
    assert_eq!(plugin.send(Lane::Data, &open).unwrap(), CallOutcome::Ok);
    assert_event(
        recv_gate(&plugin),
        "stream-2",
        EVENT_CREDIT,
        json!({"bytes": 1024 * 1024}),
    );

    let reason = json!({"reason": "host_cancelled"});
    let cancel = Frame::service_request("stream-2", SERVICE, OP_CANCEL, reason.clone());
    assert_eq!(plugin.send(Lane::Data, &cancel).unwrap(), CallOutcome::Ok);
    assert_event(recv_gate(&plugin), "stream-2", EVENT_CANCEL, reason);
    assert_eq!(
        plugin.send(Lane::Data, &cancel).unwrap(),
        CallOutcome::Failed
    );
}
