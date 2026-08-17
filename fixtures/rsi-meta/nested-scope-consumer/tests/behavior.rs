use std::time::Duration;

use rsi_meta_fixture_nested_scope_consumer::rsi_meta_plugin_entry_v0;
use rsi_meta_plugin::{CallOutcome, Lane, PostFrameOutcome};
use rsi_meta_plugin::{
    EVENT_CANCEL, EVENT_CREDIT, EVENT_END, Frame, FrameBody, LifecyclePhase, OP_CANCEL, OP_CREDIT,
    OP_HALF_CLOSE, OP_OPEN, RUNTIME_TICK_EVENT, RUNTIME_TICK_SERVICE,
};
use rsi_meta_plugin_testkit::PluginHarness;
use serde_json::json;

fn recv_body(plugin: &PluginHarness) -> FrameBody {
    plugin.recv(Duration::from_secs(1)).unwrap().frame.body
}

fn tick(sequence: u64) -> Frame {
    Frame::service_event(
        None,
        RUNTIME_TICK_SERVICE,
        RUNTIME_TICK_EVENT,
        json!({"tick": sequence}),
    )
}

fn committed_consumer() -> PluginHarness {
    let mut plugin = PluginHarness::start(rsi_meta_plugin_entry_v0).unwrap();
    assert_eq!(
        plugin
            .send(
                Lane::Control,
                &Frame::lifecycle(
                    LifecyclePhase::Prepare,
                    1,
                    Some(json!({"request_id": "nested", "message": "test"})),
                ),
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
fn prepare_rejects_the_schema_forbidden_empty_request_prefix() {
    let mut plugin = PluginHarness::start(rsi_meta_plugin_entry_v0).unwrap();
    let outcome = plugin
        .send(
            Lane::Control,
            &Frame::lifecycle(
                LifecyclePhase::Prepare,
                1,
                Some(json!({"request_id": "", "message": "test"})),
            ),
        )
        .unwrap();

    assert_eq!(outcome, CallOutcome::Failed);
    assert!(plugin.try_recv().unwrap().is_none());
}

fn open_proxy(plugin: &mut PluginHarness, outer_id: &str) -> String {
    let open = Frame::service_request(
        outer_id,
        "fixture.nested-consumer",
        OP_OPEN,
        json!({"consumer": "test", "sequence": 0}),
    );
    assert_eq!(plugin.send(Lane::Data, &open).unwrap(), CallOutcome::Ok);
    let FrameBody::ServiceRequest { request_id, .. } = recv_body(plugin) else {
        panic!("expected inner open")
    };
    request_id
}

#[test]
fn retired_would_block_is_retried_by_runtime_tick() {
    let mut plugin = committed_consumer();
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
fn retirement_waits_for_inner_and_outer_stream_terminals() {
    let mut plugin = committed_consumer();
    let inner_id = open_proxy(&mut plugin, "retiring-proxy");

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
        FrameBody::ServiceRequest {
            request_id,
            operation,
            ..
        } if request_id == inner_id && operation == OP_CANCEL
    ));
    assert!(matches!(
        recv_body(&plugin),
        FrameBody::ServiceEvent {
            request_id: Some(request_id),
            event,
            ..
        } if request_id == "retiring-proxy" && event == EVENT_CANCEL
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
fn outer_data_would_block_retains_input_credit_and_request() {
    let mut plugin = committed_consumer();
    let inner_id = open_proxy(&mut plugin, "outer-data");
    let credit = Frame::service_event(
        Some(inner_id.clone()),
        "fixture.echo",
        EVENT_CREDIT,
        json!({"bytes": 64}),
    );
    assert_eq!(plugin.send(Lane::Data, &credit).unwrap(), CallOutcome::Ok);
    assert!(matches!(
        recv_body(&plugin),
        FrameBody::ServiceEvent { event, .. } if event == EVENT_CREDIT
    ));

    plugin.set_post_outcome(PostFrameOutcome::WouldBlock);
    let data = Frame::service_data_request("outer-data", "fixture.nested-consumer", vec![1, 2]);
    assert_eq!(
        plugin.send(Lane::Data, &data).unwrap(),
        CallOutcome::Ok,
        "temporary inner-request backpressure must not fault the callback"
    );
    assert!(plugin.try_recv().unwrap().is_none());

    plugin.set_post_outcome(PostFrameOutcome::Accepted);
    assert_eq!(
        plugin.send(Lane::Control, &tick(1)).unwrap(),
        CallOutcome::Ok
    );
    assert!(matches!(
        recv_body(&plugin),
        FrameBody::ServiceDataRequest { payload, .. } if payload == vec![1, 2]
    ));
    assert!(plugin.try_recv().unwrap().is_none());
}

#[test]
fn inner_data_would_block_retains_output_credit_and_event() {
    let mut plugin = committed_consumer();
    let inner_id = open_proxy(&mut plugin, "inner-data");
    let payload = vec![3, 4];
    let encoded = payload.len() as u64;
    let credit = Frame::service_request(
        "inner-data",
        "fixture.nested-consumer",
        OP_CREDIT,
        json!({"bytes": encoded}),
    );
    assert_eq!(plugin.send(Lane::Data, &credit).unwrap(), CallOutcome::Ok);
    assert!(matches!(
        recv_body(&plugin),
        FrameBody::ServiceRequest { operation, .. } if operation == OP_CREDIT
    ));

    plugin.set_post_outcome(PostFrameOutcome::WouldBlock);
    let inner_data = Frame::service_data_event(inner_id, "fixture.echo", payload.clone());
    assert_eq!(
        plugin.send(Lane::Data, &inner_data).unwrap(),
        CallOutcome::Ok,
        "temporary outer-event backpressure must not fault the callback"
    );
    assert!(plugin.try_recv().unwrap().is_none());

    plugin.set_post_outcome(PostFrameOutcome::Accepted);
    assert_eq!(
        plugin.send(Lane::Control, &tick(1)).unwrap(),
        CallOutcome::Ok
    );
    assert!(matches!(
        recv_body(&plugin),
        FrameBody::ServiceDataEvent { payload: actual, .. } if actual == payload
    ));
    assert!(plugin.try_recv().unwrap().is_none());
}

#[test]
fn inner_end_would_block_keeps_mapping_until_terminal_is_accepted() {
    let mut plugin = committed_consumer();
    let inner_id = open_proxy(&mut plugin, "inner-end");
    plugin.set_post_outcome(PostFrameOutcome::WouldBlock);
    let end = Frame::service_event(Some(inner_id), "fixture.echo", EVENT_END, json!({}));
    assert_eq!(
        plugin.send(Lane::Data, &end).unwrap(),
        CallOutcome::Ok,
        "temporary terminal backpressure must not fault the callback"
    );
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
fn cancel_partial_post_retries_only_the_outer_terminal() {
    let mut plugin = committed_consumer();
    open_proxy(&mut plugin, "cancel");
    plugin.set_post_outcomes([PostFrameOutcome::Accepted, PostFrameOutcome::WouldBlock]);
    let cancel = Frame::service_request(
        "cancel",
        "fixture.nested-consumer",
        OP_CANCEL,
        json!({"reason": "test"}),
    );
    assert_eq!(
        plugin.send(Lane::Data, &cancel).unwrap(),
        CallOutcome::Ok,
        "partial cancel backpressure must retain the outer terminal"
    );
    assert!(matches!(
        recv_body(&plugin),
        FrameBody::ServiceRequest { operation, .. } if operation == OP_CANCEL
    ));
    assert!(plugin.try_recv().unwrap().is_none());

    plugin.set_post_outcome(PostFrameOutcome::Accepted);
    assert_eq!(
        plugin.send(Lane::Control, &tick(1)).unwrap(),
        CallOutcome::Ok
    );
    assert!(matches!(
        recv_body(&plugin),
        FrameBody::ServiceEvent { event, .. } if event == EVENT_CANCEL
    ));
    assert!(plugin.try_recv().unwrap().is_none());
}

#[test]
#[allow(clippy::too_many_lines)] // Keep the ordered outer/inner stream transcript visible.
fn public_stream_proxies_credit_data_and_end_through_a_distinct_inner_id() {
    let mut plugin = PluginHarness::start(rsi_meta_plugin_entry_v0).unwrap();
    let config = json!({"request_id": "nested-9", "message": "nearest provider"});
    assert_eq!(
        plugin
            .send(
                Lane::Control,
                &Frame::lifecycle(LifecyclePhase::Prepare, 9, Some(config)),
            )
            .unwrap(),
        CallOutcome::Ok
    );
    assert_eq!(
        recv_body(&plugin),
        FrameBody::Lifecycle {
            phase: LifecyclePhase::Prepared,
            generation: 9,
            config: None,
        }
    );
    assert_eq!(
        plugin
            .send(
                Lane::Control,
                &Frame::lifecycle(LifecyclePhase::Committed, 9, None),
            )
            .unwrap(),
        CallOutcome::Ok
    );
    assert!(
        plugin.try_recv().unwrap().is_none(),
        "committed must not open an unsolicited inner stream"
    );

    let outer_open = Frame::service_request(
        "outer-1",
        "fixture.nested-consumer",
        OP_OPEN,
        json!({"consumer": "public-client", "sequence": 0}),
    );
    assert_eq!(
        plugin.send(Lane::Data, &outer_open).unwrap(),
        CallOutcome::Ok
    );
    assert_eq!(
        recv_body(&plugin),
        FrameBody::ServiceRequest {
            request_id: "nested-9/outer-1".to_owned(),
            service: "fixture.echo".to_owned(),
            operation: OP_OPEN.to_owned(),
            payload: json!({
                "consumer": "fixture.nested-consumer",
                "sequence": 0,
                "proxy_message": "nearest provider",
            }),
        }
    );

    let input_credit = Frame::service_event(
        Some("nested-9/outer-1".to_owned()),
        "fixture.echo",
        EVENT_CREDIT,
        json!({"bytes": 4096}),
    );
    assert_eq!(
        plugin.send(Lane::Data, &input_credit).unwrap(),
        CallOutcome::Ok
    );
    assert_eq!(
        recv_body(&plugin),
        FrameBody::ServiceEvent {
            request_id: Some("outer-1".to_owned()),
            service: "fixture.nested-consumer".to_owned(),
            event: EVENT_CREDIT.to_owned(),
            payload: json!({"bytes": 4096}),
        }
    );

    let output_credit = Frame::service_request(
        "outer-1",
        "fixture.nested-consumer",
        OP_CREDIT,
        json!({"bytes": 2048}),
    );
    assert_eq!(
        plugin.send(Lane::Data, &output_credit).unwrap(),
        CallOutcome::Ok
    );
    assert_eq!(
        recv_body(&plugin),
        FrameBody::ServiceRequest {
            request_id: "nested-9/outer-1".to_owned(),
            service: "fixture.echo".to_owned(),
            operation: OP_CREDIT.to_owned(),
            payload: json!({"bytes": 2048}),
        }
    );

    let bytes = b"nested".to_vec();
    let outer_data =
        Frame::service_data_request("outer-1", "fixture.nested-consumer", bytes.clone());
    assert_eq!(
        plugin.send(Lane::Data, &outer_data).unwrap(),
        CallOutcome::Ok
    );
    assert_eq!(
        recv_body(&plugin),
        FrameBody::ServiceDataRequest {
            request_id: "nested-9/outer-1".to_owned(),
            service: "fixture.echo".to_owned(),
            payload: bytes.clone(),
        }
    );

    let inner_data = Frame::service_data_event("nested-9/outer-1", "fixture.echo", bytes.clone());
    assert_eq!(
        plugin.send(Lane::Data, &inner_data).unwrap(),
        CallOutcome::Ok
    );
    assert_eq!(
        recv_body(&plugin),
        FrameBody::ServiceDataEvent {
            request_id: "outer-1".to_owned(),
            service: "fixture.nested-consumer".to_owned(),
            payload: bytes,
        }
    );

    let half_close = Frame::service_request(
        "outer-1",
        "fixture.nested-consumer",
        OP_HALF_CLOSE,
        json!({"sequence": 1}),
    );
    assert_eq!(
        plugin.send(Lane::Data, &half_close).unwrap(),
        CallOutcome::Ok
    );
    assert_eq!(
        recv_body(&plugin),
        FrameBody::ServiceRequest {
            request_id: "nested-9/outer-1".to_owned(),
            service: "fixture.echo".to_owned(),
            operation: OP_HALF_CLOSE.to_owned(),
            payload: json!({"sequence": 1}),
        }
    );
    let inner_end = Frame::service_event(
        Some("nested-9/outer-1".to_owned()),
        "fixture.echo",
        EVENT_END,
        json!({}),
    );
    assert_eq!(
        plugin.send(Lane::Data, &inner_end).unwrap(),
        CallOutcome::Ok
    );
    assert_eq!(
        recv_body(&plugin),
        FrameBody::ServiceEvent {
            request_id: Some("outer-1".to_owned()),
            service: "fixture.nested-consumer".to_owned(),
            event: EVENT_END.to_owned(),
            payload: json!({}),
        }
    );
}
