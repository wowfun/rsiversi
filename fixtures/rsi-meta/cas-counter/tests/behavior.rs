use std::time::Duration;

use rsi_meta_fixture_cas_counter::rsi_meta_plugin_entry_v0;
use rsi_meta_frame_contract::{
    EVENT_CANCEL, EVENT_CREDIT, EVENT_DATA, EVENT_END, Frame, FrameBody, LifecyclePhase, OP_CREDIT,
    OP_DATA, OP_OPEN, RUNTIME_TICK_EVENT, RUNTIME_TICK_SERVICE, STATE_EVENT_APPLIED,
    STATE_EVENT_CONFLICT, STATE_EVENT_VALUE, STATE_OP_COMPARE_AND_SWAP, STATE_OP_GET,
};
use rsi_meta_plugin::{CallOutcome, Lane, PostFrameOutcome};
use rsi_meta_plugin_testkit::PluginHarness;
use serde_json::{Value, json};

fn recv_body(plugin: &PluginHarness) -> FrameBody {
    plugin.recv(Duration::from_secs(1)).unwrap().frame.body
}

fn byte_array(value: &Value) -> Value {
    Value::Array(
        serde_json::to_vec(value)
            .unwrap()
            .into_iter()
            .map(Value::from)
            .collect(),
    )
}

fn decode_byte_array(value: &Value) -> Value {
    let bytes = value
        .as_array()
        .unwrap()
        .iter()
        .map(|byte| u8::try_from(byte.as_u64().unwrap()).unwrap())
        .collect::<Vec<_>>();
    serde_json::from_slice(&bytes).unwrap()
}

fn tick(sequence: u64) -> Frame {
    Frame::service_event(
        None,
        RUNTIME_TICK_SERVICE,
        RUNTIME_TICK_EVENT,
        json!({"tick": sequence}),
    )
}

fn committed_counter() -> PluginHarness {
    let mut plugin = PluginHarness::start(rsi_meta_plugin_entry_v0).unwrap();
    assert_eq!(
        plugin
            .send(
                Lane::Control,
                &Frame::lifecycle(LifecyclePhase::Prepare, 1, Some(json!({}))),
            )
            .unwrap(),
        CallOutcome::Ok
    );
    assert!(matches!(
        recv_body(&plugin),
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

#[test]
fn failed_initial_credit_does_not_retain_the_stream_id() {
    let mut plugin = committed_counter();
    let open = Frame::service_request(
        "retry-open",
        "fixture.cas-counter",
        OP_OPEN,
        json!({"consumer": "backpressure-test", "sequence": 0}),
    );
    plugin.set_post_outcome(PostFrameOutcome::WouldBlock);
    assert_eq!(plugin.send(Lane::Data, &open).unwrap(), CallOutcome::Failed);

    plugin.set_post_outcome(PostFrameOutcome::Accepted);
    assert_eq!(plugin.send(Lane::Data, &open).unwrap(), CallOutcome::Ok);
    assert!(
        matches!(recv_body(&plugin), FrameBody::ServiceEvent { event, .. } if event == EVENT_CREDIT)
    );
}

fn drive_increment_to_applied(
    plugin: &mut PluginHarness,
    stream_id: &str,
    output_credit: u64,
) -> (Frame, Value) {
    let open = Frame::service_request(
        stream_id,
        "fixture.cas-counter",
        OP_OPEN,
        json!({"consumer": "test", "sequence": 0}),
    );
    assert_eq!(plugin.send(Lane::Data, &open).unwrap(), CallOutcome::Ok);
    assert!(matches!(
        recv_body(plugin),
        FrameBody::ServiceEvent { event, .. } if event == EVENT_CREDIT
    ));
    let credit = Frame::service_request(
        stream_id,
        "fixture.cas-counter",
        OP_CREDIT,
        json!({"bytes": output_credit}),
    );
    assert_eq!(plugin.send(Lane::Data, &credit).unwrap(), CallOutcome::Ok);

    let increment = Frame::service_request(
        stream_id,
        "fixture.cas-counter",
        OP_DATA,
        byte_array(&json!({"key": "counter"})),
    );
    assert_eq!(
        plugin.send(Lane::Data, &increment).unwrap(),
        CallOutcome::Ok
    );
    assert!(matches!(
        recv_body(plugin),
        FrameBody::ServiceRequest { operation, .. } if operation == STATE_OP_GET
    ));
    let read_id = format!("{stream_id}/read");
    let value = Frame::service_event(
        Some(read_id),
        "state.cas",
        STATE_EVENT_VALUE,
        json!({"key": "counter", "version": 0, "value": null}),
    );
    assert_eq!(plugin.send(Lane::Data, &value).unwrap(), CallOutcome::Ok);
    assert!(matches!(
        recv_body(plugin),
        FrameBody::ServiceRequest { operation, .. } if operation == STATE_OP_COMPARE_AND_SWAP
    ));

    let applied_payload = json!({"key": "counter", "version": 1, "value": 1});
    let applied = Frame::service_event(
        Some(format!("{stream_id}/cas/0")),
        "state.cas",
        STATE_EVENT_APPLIED,
        applied_payload.clone(),
    );
    (applied, applied_payload)
}

#[test]
fn retired_would_block_is_retried_by_runtime_tick() {
    let mut plugin = committed_counter();
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
fn retirement_waits_for_backpressured_stream_terminals() {
    let mut plugin = committed_counter();
    let open = Frame::service_request(
        "retiring-stream",
        "fixture.cas-counter",
        OP_OPEN,
        json!({"consumer": "test", "sequence": 0}),
    );
    assert_eq!(plugin.send(Lane::Data, &open).unwrap(), CallOutcome::Ok);
    assert!(matches!(
        recv_body(&plugin),
        FrameBody::ServiceEvent { event, .. } if event == EVENT_CREDIT
    ));

    plugin.set_post_outcome(PostFrameOutcome::WouldBlock);
    assert_eq!(
        plugin
            .send(
                Lane::Control,
                &Frame::lifecycle(LifecyclePhase::Retire, 1, None),
            )
            .unwrap(),
        CallOutcome::Ok
    );
    assert!(plugin.try_recv().unwrap().is_none());

    plugin.set_post_outcome(PostFrameOutcome::Accepted);
    assert_eq!(
        plugin.send(Lane::Control, &tick(1)).unwrap(),
        CallOutcome::Ok
    );
    assert!(matches!(
        recv_body(&plugin),
        FrameBody::ServiceEvent {
            request_id: Some(request_id),
            event,
            ..
        } if request_id == "retiring-stream" && event == EVENT_CANCEL
    ));
    assert!(matches!(
        recv_body(&plugin),
        FrameBody::Lifecycle {
            phase: LifecyclePhase::Retired,
            ..
        }
    ));
}

#[test]
fn data_would_block_is_retryable_without_consuming_credit() {
    let mut plugin = committed_counter();
    let applied_payload = json!({"key": "counter", "version": 1, "value": 1});
    let encoded_credit = serde_json::to_vec(&byte_array(&applied_payload))
        .unwrap()
        .len() as u64;
    let (applied, _) = drive_increment_to_applied(&mut plugin, "blocked-data", encoded_credit);

    plugin.set_post_outcome(PostFrameOutcome::WouldBlock);
    assert_eq!(
        plugin.send(Lane::Data, &applied).unwrap(),
        CallOutcome::Ok,
        "temporary DATA backpressure must not fault the plugin callback"
    );
    assert!(plugin.try_recv().unwrap().is_none());

    plugin.set_post_outcome(PostFrameOutcome::Accepted);
    assert_eq!(
        plugin.send(Lane::Control, &tick(1)).unwrap(),
        CallOutcome::Ok
    );
    assert!(matches!(
        recv_body(&plugin),
        FrameBody::ServiceEvent { event, .. } if event == EVENT_DATA
    ));
    assert!(matches!(
        recv_body(&plugin),
        FrameBody::ServiceEvent { event, .. } if event == EVENT_END
    ));
    assert!(plugin.try_recv().unwrap().is_none());
}

#[test]
fn end_would_block_retries_only_the_terminal_frame() {
    let mut plugin = committed_counter();
    let applied_payload = json!({"key": "counter", "version": 1, "value": 1});
    let encoded_credit = serde_json::to_vec(&byte_array(&applied_payload))
        .unwrap()
        .len() as u64;
    let (applied, _) = drive_increment_to_applied(&mut plugin, "blocked-end", encoded_credit);

    plugin.set_post_outcomes([PostFrameOutcome::Accepted, PostFrameOutcome::WouldBlock]);
    assert_eq!(
        plugin.send(Lane::Data, &applied).unwrap(),
        CallOutcome::Ok,
        "temporary END backpressure must not fault the plugin callback"
    );
    assert!(matches!(
        recv_body(&plugin),
        FrameBody::ServiceEvent { event, .. } if event == EVENT_DATA
    ));
    assert!(plugin.try_recv().unwrap().is_none());

    plugin.set_post_outcome(PostFrameOutcome::Accepted);
    assert_eq!(
        plugin.send(Lane::Control, &tick(1)).unwrap(),
        CallOutcome::Ok
    );
    assert!(matches!(
        recv_body(&plugin),
        FrameBody::ServiceEvent { event, .. } if event == EVENT_END
    ));
    assert!(plugin.try_recv().unwrap().is_none());
}

#[test]
fn state_request_would_block_does_not_fail_the_host_service_response() {
    let mut plugin = committed_counter();
    let open = Frame::service_request(
        "blocked-cas",
        "fixture.cas-counter",
        OP_OPEN,
        json!({"consumer": "test", "sequence": 0}),
    );
    assert_eq!(plugin.send(Lane::Data, &open).unwrap(), CallOutcome::Ok);
    assert!(matches!(
        recv_body(&plugin),
        FrameBody::ServiceEvent { event, .. } if event == EVENT_CREDIT
    ));
    let increment = Frame::service_request(
        "blocked-cas",
        "fixture.cas-counter",
        OP_DATA,
        byte_array(&json!({"key": "counter"})),
    );
    assert_eq!(
        plugin.send(Lane::Data, &increment).unwrap(),
        CallOutcome::Ok
    );
    assert!(matches!(
        recv_body(&plugin),
        FrameBody::ServiceRequest { operation, .. } if operation == STATE_OP_GET
    ));

    plugin.set_post_outcome(PostFrameOutcome::WouldBlock);
    let value = Frame::service_event(
        Some("blocked-cas/read".to_owned()),
        "state.cas",
        STATE_EVENT_VALUE,
        json!({"key": "counter", "version": 0, "value": null}),
    );
    assert_eq!(
        plugin.send(Lane::Data, &value).unwrap(),
        CallOutcome::Ok,
        "temporary CAS request backpressure must not fault the plugin callback"
    );
    assert!(plugin.try_recv().unwrap().is_none());

    plugin.set_post_outcome(PostFrameOutcome::Accepted);
    assert_eq!(
        plugin.send(Lane::Control, &tick(1)).unwrap(),
        CallOutcome::Ok
    );
    assert!(matches!(
        recv_body(&plugin),
        FrameBody::ServiceRequest { operation, .. }
            if operation == STATE_OP_COMPARE_AND_SWAP
    ));
    assert!(plugin.try_recv().unwrap().is_none());
}

#[test]
#[allow(clippy::too_many_lines)] // Keep the ordered stream and CAS transcript visible end to end.
fn standard_stream_retries_compare_and_swap_conflict_then_returns_data_and_end() {
    let mut plugin = PluginHarness::start(rsi_meta_plugin_entry_v0).unwrap();
    assert_eq!(
        plugin
            .send(
                Lane::Control,
                &Frame::lifecycle(LifecyclePhase::Prepare, 3, Some(json!({}))),
            )
            .unwrap(),
        CallOutcome::Ok
    );
    assert_eq!(
        recv_body(&plugin),
        FrameBody::Lifecycle {
            phase: LifecyclePhase::Prepared,
            generation: 3,
            config: None,
        }
    );
    assert_eq!(
        plugin
            .send(
                Lane::Control,
                &Frame::lifecycle(LifecyclePhase::Committed, 3, None),
            )
            .unwrap(),
        CallOutcome::Ok
    );

    let open = Frame::service_request(
        "increment-1",
        "fixture.cas-counter",
        OP_OPEN,
        json!({"consumer": "client", "sequence": 0}),
    );
    assert_eq!(plugin.send(Lane::Data, &open).unwrap(), CallOutcome::Ok);
    assert_eq!(
        recv_body(&plugin),
        FrameBody::ServiceEvent {
            request_id: Some("increment-1".to_owned()),
            service: "fixture.cas-counter".to_owned(),
            event: EVENT_CREDIT.to_owned(),
            payload: json!({"bytes": 1024 * 1024}),
        }
    );

    let output_credit = Frame::service_request(
        "increment-1",
        "fixture.cas-counter",
        OP_CREDIT,
        json!({"bytes": 1024 * 1024}),
    );
    assert_eq!(
        plugin.send(Lane::Data, &output_credit).unwrap(),
        CallOutcome::Ok
    );
    assert!(plugin.try_recv().unwrap().is_none());

    let request = json!({"key": "requests"});
    let increment = Frame::service_request(
        "increment-1",
        "fixture.cas-counter",
        OP_DATA,
        byte_array(&request),
    );
    assert_eq!(
        plugin.send(Lane::Data, &increment).unwrap(),
        CallOutcome::Ok
    );
    assert_eq!(
        recv_body(&plugin),
        FrameBody::ServiceRequest {
            request_id: "increment-1/read".to_owned(),
            service: "state.cas".to_owned(),
            operation: STATE_OP_GET.to_owned(),
            payload: json!({"key": "requests"}),
        }
    );

    let read = Frame::service_event(
        Some("increment-1/read".to_owned()),
        "state.cas",
        STATE_EVENT_VALUE,
        json!({"key": "requests", "version": 4, "value": 9}),
    );
    assert_eq!(plugin.send(Lane::Data, &read).unwrap(), CallOutcome::Ok);
    assert_eq!(
        recv_body(&plugin),
        FrameBody::ServiceRequest {
            request_id: "increment-1/cas/4".to_owned(),
            service: "state.cas".to_owned(),
            operation: STATE_OP_COMPARE_AND_SWAP.to_owned(),
            payload: json!({"key": "requests", "expected_version": 4, "value": 10}),
        }
    );

    let conflict = Frame::service_event(
        Some("increment-1/cas/4".to_owned()),
        "state.cas",
        STATE_EVENT_CONFLICT,
        json!({"key": "requests", "version": 5, "value": 12}),
    );
    assert_eq!(plugin.send(Lane::Data, &conflict).unwrap(), CallOutcome::Ok);
    assert_eq!(
        recv_body(&plugin),
        FrameBody::ServiceRequest {
            request_id: "increment-1/cas/5".to_owned(),
            service: "state.cas".to_owned(),
            operation: STATE_OP_COMPARE_AND_SWAP.to_owned(),
            payload: json!({"key": "requests", "expected_version": 5, "value": 13}),
        }
    );

    let applied_payload = json!({"key": "requests", "version": 6, "value": 13});
    let applied = Frame::service_event(
        Some("increment-1/cas/5".to_owned()),
        "state.cas",
        STATE_EVENT_APPLIED,
        applied_payload.clone(),
    );
    assert_eq!(plugin.send(Lane::Data, &applied).unwrap(), CallOutcome::Ok);
    let FrameBody::ServiceEvent {
        request_id,
        service,
        event,
        payload,
    } = recv_body(&plugin)
    else {
        panic!("expected DATA result")
    };
    assert_eq!(request_id.as_deref(), Some("increment-1"));
    assert_eq!(service, "fixture.cas-counter");
    assert_eq!(event, EVENT_DATA);
    assert_eq!(decode_byte_array(&payload), applied_payload);
    assert_eq!(
        recv_body(&plugin),
        FrameBody::ServiceEvent {
            request_id: Some("increment-1".to_owned()),
            service: "fixture.cas-counter".to_owned(),
            event: EVENT_END.to_owned(),
            payload: json!({}),
        }
    );
}
