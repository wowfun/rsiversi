use std::time::Duration;

use rsi_meta_fixture_echo_bidi::rsi_meta_plugin_entry_v0;
use rsi_meta_frame_contract::{
    EVENT_CREDIT, EVENT_DATA, EVENT_END, Frame, FrameBody, LifecyclePhase, OP_CREDIT, OP_DATA,
    OP_HALF_CLOSE, OP_OPEN, RUNTIME_TICK_EVENT, RUNTIME_TICK_SERVICE,
};
use rsi_meta_plugin::{CallOutcome, Lane, PostFrameOutcome};
use rsi_meta_plugin_testkit::PluginHarness;
use serde_json::json;

fn tick(sequence: u64) -> Frame {
    Frame::service_event(
        None,
        RUNTIME_TICK_SERVICE,
        RUNTIME_TICK_EVENT,
        json!({"tick": sequence}),
    )
}

fn committed_echo() -> PluginHarness {
    let mut plugin = PluginHarness::start(rsi_meta_plugin_entry_v0).unwrap();
    assert_eq!(
        plugin
            .send(
                Lane::Control,
                &Frame::lifecycle(LifecyclePhase::Prepare, 1, None),
            )
            .unwrap(),
        CallOutcome::Ok
    );
    assert!(matches!(
        plugin.recv(Duration::from_secs(1)).unwrap().frame.body,
        FrameBody::Lifecycle {
            phase: LifecyclePhase::Prepared,
            generation: 1,
            ..
        }
    ));
    assert_eq!(
        plugin
            .send(
                Lane::Control,
                &Frame::lifecycle(LifecyclePhase::Committed, 1, None),
            )
            .unwrap(),
        CallOutcome::Ok
    );
    plugin
}

fn open_echo(plugin: &mut PluginHarness, stream_id: &str, output_credit: u64) {
    let open = Frame::service_request(
        stream_id,
        "fixture.echo",
        OP_OPEN,
        json!({"consumer": "backpressure-test", "sequence": 0}),
    );
    assert_eq!(plugin.send(Lane::Data, &open).unwrap(), CallOutcome::Ok);
    assert!(matches!(
        plugin.recv(Duration::from_secs(1)).unwrap().frame.body,
        FrameBody::ServiceEvent { event, .. } if event == EVENT_CREDIT
    ));
    let credit = Frame::service_request(
        stream_id,
        "fixture.echo",
        OP_CREDIT,
        json!({"bytes": output_credit}),
    );
    assert_eq!(plugin.send(Lane::Data, &credit).unwrap(), CallOutcome::Ok);
}

#[test]
fn failed_initial_credit_does_not_retain_the_stream_id() {
    let mut plugin = committed_echo();
    let open = Frame::service_request(
        "retry-open",
        "fixture.echo",
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
fn data_would_block_is_retained_and_charged_once() {
    let mut plugin = committed_echo();
    let payload = json!([1, 2, 3]);
    let encoded = serde_json::to_vec(&payload).unwrap().len() as u64;
    open_echo(&mut plugin, "blocked-data", encoded);

    plugin.set_post_outcome(PostFrameOutcome::WouldBlock);
    let data = Frame::service_request("blocked-data", "fixture.echo", OP_DATA, payload.clone());
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
    assert_eq!(
        plugin.recv(Duration::from_secs(1)).unwrap().frame.body,
        FrameBody::ServiceEvent {
            request_id: Some("blocked-data".to_owned()),
            service: "fixture.echo".to_owned(),
            event: EVENT_DATA.to_owned(),
            payload,
        }
    );
    assert!(plugin.try_recv().unwrap().is_none());
}

#[test]
fn end_would_block_keeps_the_stream_until_retry() {
    let mut plugin = committed_echo();
    open_echo(&mut plugin, "blocked-end", 0);
    let half_close = Frame::service_request(
        "blocked-end",
        "fixture.echo",
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
    assert_eq!(
        plugin.recv(Duration::from_secs(1)).unwrap().frame.body,
        FrameBody::ServiceEvent {
            request_id: Some("blocked-end".to_owned()),
            service: "fixture.echo".to_owned(),
            event: EVENT_END.to_owned(),
            payload: json!({}),
        }
    );
    assert!(plugin.try_recv().unwrap().is_none());
}

#[test]
fn retired_would_block_is_retried_by_runtime_tick() {
    let mut plugin = committed_echo();
    plugin.set_post_outcomes([PostFrameOutcome::WouldBlock, PostFrameOutcome::Accepted]);

    assert_eq!(
        plugin
            .send(
                Lane::Control,
                &Frame::lifecycle(LifecyclePhase::Retire, 1, None),
            )
            .unwrap(),
        CallOutcome::Ok,
        "temporary Retired backpressure must not fault the Retire callback"
    );
    assert!(plugin.try_recv().unwrap().is_none());

    assert_eq!(
        plugin.send(Lane::Control, &tick(1)).unwrap(),
        CallOutcome::Ok,
        "runtime.tick alone must retry the retained Retired terminal"
    );
    let retired = plugin.recv(Duration::from_secs(1)).unwrap();
    assert_eq!(retired.lane, Lane::Control);
    assert_eq!(
        retired.frame.body,
        FrameBody::Lifecycle {
            phase: LifecyclePhase::Retired,
            generation: 1,
            config: None,
        }
    );
    assert!(plugin.try_recv().unwrap().is_none());
}

#[test]
fn committed_echo_service_obeys_open_credit_data_and_end() {
    let mut plugin = PluginHarness::start(rsi_meta_plugin_entry_v0).unwrap();
    assert_eq!(
        plugin
            .send(
                Lane::Control,
                &Frame::lifecycle(LifecyclePhase::Prepare, 7, None),
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
            generation: 7,
            config: None,
        }
    );
    assert_eq!(
        plugin
            .send(
                Lane::Control,
                &Frame::lifecycle(LifecyclePhase::Committed, 7, None),
            )
            .unwrap(),
        CallOutcome::Ok
    );

    let open = Frame::service_request(
        "echo-1",
        "fixture.echo",
        OP_OPEN,
        json!({"consumer": "client", "sequence": 0}),
    );
    assert_eq!(plugin.send(Lane::Data, &open).unwrap(), CallOutcome::Ok);

    let credit = plugin.recv(Duration::from_secs(1)).unwrap();
    assert_eq!(credit.lane, Lane::Data);
    assert_eq!(
        credit.frame.body,
        FrameBody::ServiceEvent {
            request_id: Some("echo-1".to_owned()),
            service: "fixture.echo".to_owned(),
            event: EVENT_CREDIT.to_owned(),
            payload: json!({"bytes": 1024 * 1024}),
        }
    );

    let bytes = json!([104, 101, 108, 108, 111]);
    let encoded_len = serde_json::to_vec(&bytes).unwrap().len() as u64;
    let reply_credit = Frame::service_request(
        "echo-1",
        "fixture.echo",
        OP_CREDIT,
        json!({"bytes": encoded_len}),
    );
    assert_eq!(
        plugin.send(Lane::Data, &reply_credit).unwrap(),
        CallOutcome::Ok
    );

    let data = Frame::service_request("echo-1", "fixture.echo", OP_DATA, bytes.clone());
    assert_eq!(plugin.send(Lane::Data, &data).unwrap(), CallOutcome::Ok);
    assert_eq!(
        plugin.recv(Duration::from_secs(1)).unwrap().frame.body,
        FrameBody::ServiceEvent {
            request_id: Some("echo-1".to_owned()),
            service: "fixture.echo".to_owned(),
            event: EVENT_DATA.to_owned(),
            payload: bytes,
        }
    );

    let half_close = Frame::service_request(
        "echo-1",
        "fixture.echo",
        OP_HALF_CLOSE,
        json!({"sequence": 1}),
    );
    assert_eq!(
        plugin.send(Lane::Data, &half_close).unwrap(),
        CallOutcome::Ok
    );
    assert_eq!(
        plugin.recv(Duration::from_secs(1)).unwrap().frame.body,
        FrameBody::ServiceEvent {
            request_id: Some("echo-1".to_owned()),
            service: "fixture.echo".to_owned(),
            event: EVENT_END.to_owned(),
            payload: json!({}),
        }
    );
}
