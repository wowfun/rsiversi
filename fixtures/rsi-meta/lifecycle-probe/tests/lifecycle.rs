use std::time::Duration;

use rsi_meta_fixture_lifecycle_probe::rsi_meta_plugin_entry_v0;
use rsi_meta_frame_contract::{
    DurableCommand, EVENT_CANCEL, Frame, FrameBody, LifecyclePhase, OP_OPEN, RUNTIME_TICK_EVENT,
    RUNTIME_TICK_SERVICE, STATE_EVENT_APPLIED, STATE_EVENT_CONFLICT, STATE_EVENT_VALUE,
    STATE_OP_COMPARE_AND_SWAP, STATE_OP_GET,
};
use rsi_meta_plugin::{CallOutcome, Lane, PostFrameOutcome};
use rsi_meta_plugin_testkit::{CapturedFrame, PluginHarness};
use serde_json::{Value, json};

fn config(fail_prepare: bool, retire_mode: &str, tag: &str) -> Value {
    json!({
        "fail_prepare": fail_prepare,
        "retire_mode": retire_mode,
        "tag": tag,
    })
}

fn config_with_action(action: &str) -> Value {
    json!({
        "fail_prepare": false,
        "retire_mode": "ack",
        "tag": "action-probe",
        "prepare_action": action,
    })
}

fn tick(sequence: u64) -> Frame {
    Frame::service_event(
        None,
        RUNTIME_TICK_SERVICE,
        RUNTIME_TICK_EVENT,
        json!({"tick": sequence}),
    )
}

fn recv_gate(plugin: &PluginHarness) -> CapturedFrame {
    plugin
        .recv(Duration::from_secs(1))
        .expect("plugin must cross the frame channel gate")
}

fn finish_prepare(plugin: &mut PluginHarness, generation: u64, config: Value) {
    assert_eq!(
        plugin
            .send(
                Lane::Control,
                &Frame::lifecycle(LifecyclePhase::Prepare, generation, Some(config)),
            )
            .unwrap(),
        CallOutcome::Ok
    );
    let request = recv_gate(plugin);
    assert_eq!(request.lane, Lane::Data);
    let FrameBody::ServiceRequest {
        request_id,
        service,
        operation,
        payload,
    } = request.frame.body
    else {
        panic!("expected deferred prepare state read")
    };
    assert_eq!(service, "state.cas");
    assert_eq!(operation, STATE_OP_GET);
    assert!(payload["key"].as_str().is_some_and(|key| !key.is_empty()));
    assert!(
        plugin.try_recv().unwrap().is_none(),
        "CALL_OK alone must not synthesize Prepared"
    );
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
    let prepared = recv_gate(plugin);
    assert_eq!(prepared.lane, Lane::Control);
    assert_eq!(
        prepared.frame.body,
        FrameBody::Lifecycle {
            phase: LifecyclePhase::Prepared,
            generation,
            config: None,
        }
    );
}

#[test]
fn failed_prepare_posts_a_deterministic_rejection_and_cannot_be_committed() {
    let mut plugin = PluginHarness::start(rsi_meta_plugin_entry_v0).unwrap();

    let prepare = Frame::lifecycle(
        LifecyclePhase::Prepare,
        11,
        Some(config(true, "ack", "rejected")),
    );
    assert_eq!(
        plugin.send(Lane::Control, &prepare).unwrap(),
        CallOutcome::Ok
    );
    assert_eq!(
        recv_gate(&plugin).frame.body,
        FrameBody::Lifecycle {
            phase: LifecyclePhase::PrepareFailed,
            generation: 11,
            config: Some(json!({
                "code": "configured_prepare_failure",
                "message": "prepare rejected by fixture configuration",
            })),
        }
    );

    let committed = Frame::lifecycle(LifecyclePhase::Committed, 11, None);
    assert_eq!(
        plugin.send(Lane::Control, &committed).unwrap(),
        CallOutcome::Failed
    );
    assert!(plugin.try_recv().unwrap().is_none());
}

#[test]
fn abort_discards_a_successful_prepare() {
    let mut plugin = PluginHarness::start(rsi_meta_plugin_entry_v0).unwrap();
    finish_prepare(&mut plugin, 12, config(false, "ack", "aborted"));
    assert_eq!(
        plugin
            .send(
                Lane::Control,
                &Frame::lifecycle(LifecyclePhase::Abort, 12, None),
            )
            .unwrap(),
        CallOutcome::Ok
    );

    assert_eq!(
        plugin
            .send(
                Lane::Control,
                &Frame::lifecycle(LifecyclePhase::Committed, 12, None),
            )
            .unwrap(),
        CallOutcome::Failed
    );
    assert!(plugin.try_recv().unwrap().is_none());
}

#[test]
fn abort_ignores_the_late_prepare_state_response() {
    let mut plugin = PluginHarness::start(rsi_meta_plugin_entry_v0).unwrap();
    assert_eq!(
        plugin
            .send(
                Lane::Control,
                &Frame::lifecycle(
                    LifecyclePhase::Prepare,
                    13,
                    Some(config(false, "ack", "late-response")),
                ),
            )
            .unwrap(),
        CallOutcome::Ok
    );
    let request = recv_gate(&plugin);
    let FrameBody::ServiceRequest {
        request_id,
        payload,
        ..
    } = request.frame.body
    else {
        panic!("expected deferred state request")
    };

    assert_eq!(
        plugin
            .send(
                Lane::Control,
                &Frame::lifecycle(LifecyclePhase::Abort, 13, None),
            )
            .unwrap(),
        CallOutcome::Ok
    );
    let late = Frame::service_event(
        Some(request_id),
        "state.cas",
        STATE_EVENT_VALUE,
        json!({"key": payload["key"], "version": 0, "value": null}),
    );
    assert_eq!(
        plugin.send(Lane::Data, &late).unwrap(),
        CallOutcome::Ok,
        "a response to an aborted prepare is stale, not a plugin protocol fault"
    );
    assert!(plugin.try_recv().unwrap().is_none());
}

#[test]
fn ack_retire_posts_one_ordered_retired_terminal() {
    let mut plugin = PluginHarness::start(rsi_meta_plugin_entry_v0).unwrap();
    finish_prepare(&mut plugin, 21, config(false, "ack", "old"));
    assert_eq!(
        plugin
            .send(
                Lane::Control,
                &Frame::lifecycle(LifecyclePhase::Committed, 21, None),
            )
            .unwrap(),
        CallOutcome::Ok
    );
    assert!(plugin.try_recv().unwrap().is_none());

    assert_eq!(
        plugin
            .send(
                Lane::Control,
                &Frame::lifecycle(LifecyclePhase::Retire, 21, None),
            )
            .unwrap(),
        CallOutcome::Ok
    );
    let retired = recv_gate(&plugin);
    assert_eq!(retired.lane, Lane::Control);
    assert_eq!(
        retired.frame.body,
        FrameBody::Lifecycle {
            phase: LifecyclePhase::Retired,
            generation: 21,
            config: None,
        }
    );
    assert!(plugin.try_recv().unwrap().is_none());
}

#[test]
fn ack_retired_would_block_is_retried_by_runtime_tick() {
    let mut plugin = PluginHarness::start(rsi_meta_plugin_entry_v0).unwrap();
    finish_prepare(&mut plugin, 21, config(false, "ack", "old"));
    assert_eq!(
        plugin
            .send(
                Lane::Control,
                &Frame::lifecycle(LifecyclePhase::Committed, 21, None),
            )
            .unwrap(),
        CallOutcome::Ok
    );
    plugin.set_post_outcomes([PostFrameOutcome::WouldBlock, PostFrameOutcome::Accepted]);

    assert_eq!(
        plugin
            .send(
                Lane::Control,
                &Frame::lifecycle(LifecyclePhase::Retire, 21, None),
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
    let retired = recv_gate(&plugin);
    assert_eq!(retired.lane, Lane::Control);
    assert_eq!(
        retired.frame.body,
        FrameBody::Lifecycle {
            phase: LifecyclePhase::Retired,
            generation: 21,
            config: None,
        }
    );
    assert!(plugin.try_recv().unwrap().is_none());
}

#[test]
fn hold_retire_never_posts_terminal_but_shutdown_completes() {
    let mut plugin = PluginHarness::start(rsi_meta_plugin_entry_v0).unwrap();
    finish_prepare(&mut plugin, 22, config(false, "hold", "held"));
    assert_eq!(
        plugin
            .send(
                Lane::Control,
                &Frame::lifecycle(LifecyclePhase::Committed, 22, None),
            )
            .unwrap(),
        CallOutcome::Ok
    );
    assert_eq!(
        plugin
            .send(
                Lane::Control,
                &Frame::lifecycle(LifecyclePhase::Retire, 22, None),
            )
            .unwrap(),
        CallOutcome::Ok
    );

    assert!(plugin.try_recv().unwrap().is_none());
    assert_eq!(plugin.shutdown(), CallOutcome::Ok);
    assert!(plugin.try_recv().unwrap().is_none());
}

#[test]
fn asynchronous_state_conflict_posts_prepare_failed_and_never_prepared() {
    let mut plugin = PluginHarness::start(rsi_meta_plugin_entry_v0).unwrap();
    let prepare = Frame::lifecycle(
        LifecyclePhase::Prepare,
        23,
        Some(config(false, "ack", "state-failure")),
    );
    assert_eq!(
        plugin.send(Lane::Control, &prepare).unwrap(),
        CallOutcome::Ok
    );
    let request = recv_gate(&plugin);
    let FrameBody::ServiceRequest {
        request_id,
        payload,
        ..
    } = request.frame.body
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
                    STATE_EVENT_CONFLICT,
                    json!({
                        "key": payload["key"],
                        "version": 0,
                        "value": null,
                        "reason": "prepare_read_failed",
                    }),
                ),
            )
            .unwrap(),
        CallOutcome::Ok
    );
    let failed = recv_gate(&plugin);
    assert_eq!(failed.lane, Lane::Control);
    assert_eq!(
        failed.frame.body,
        FrameBody::Lifecycle {
            phase: LifecyclePhase::PrepareFailed,
            generation: 23,
            config: Some(json!({
                "code": "state_read_failed",
                "message": "state.cas prepare read returned conflict",
            })),
        }
    );
    assert!(plugin.try_recv().unwrap().is_none());
    assert_eq!(
        plugin
            .send(
                Lane::Control,
                &Frame::lifecycle(LifecyclePhase::Committed, 23, None),
            )
            .unwrap(),
        CallOutcome::Failed
    );
}

#[test]
fn normal_ack_posts_only_prepared_before_commit() {
    let mut plugin = PluginHarness::start(rsi_meta_plugin_entry_v0).unwrap();
    assert_eq!(
        plugin
            .send(
                Lane::Control,
                &Frame::lifecycle(
                    LifecyclePhase::Prepare,
                    24,
                    Some(config_with_action("normal_ack")),
                ),
            )
            .unwrap(),
        CallOutcome::Ok
    );
    assert_eq!(
        recv_gate(&plugin),
        CapturedFrame {
            lane: Lane::Control,
            frame: Frame::lifecycle(LifecyclePhase::Prepared, 24, None),
        }
    );
    assert!(plugin.try_recv().unwrap().is_none());
}

#[test]
fn adversarial_prepare_actions_emit_the_side_effect_before_acknowledgement() {
    let mut durable = PluginHarness::start(rsi_meta_plugin_entry_v0).unwrap();
    assert_eq!(
        durable
            .send(
                Lane::Control,
                &Frame::lifecycle(
                    LifecyclePhase::Prepare,
                    25,
                    Some(config_with_action("durable_then_ack")),
                ),
            )
            .unwrap(),
        CallOutcome::Ok
    );
    assert_eq!(
        recv_gate(&durable),
        CapturedFrame {
            lane: Lane::Control,
            frame: Frame::durable_command(
                "probe-prepare-25",
                DurableCommand::ApplyManifestPath {
                    manifest_path: "probe-forbidden.toml".into(),
                    lock_path: "probe-forbidden.lock".into(),
                },
            ),
        }
    );
    assert_eq!(
        recv_gate(&durable),
        CapturedFrame {
            lane: Lane::Control,
            frame: Frame::lifecycle(LifecyclePhase::Prepared, 25, None),
        }
    );

    let mut outbound = PluginHarness::start(rsi_meta_plugin_entry_v0).unwrap();
    assert_eq!(
        outbound
            .send(
                Lane::Control,
                &Frame::lifecycle(
                    LifecyclePhase::Prepare,
                    26,
                    Some(config_with_action("outbound_open_then_ack")),
                ),
            )
            .unwrap(),
        CallOutcome::Ok
    );
    let request = recv_gate(&outbound);
    assert_eq!(request.lane, Lane::Data);
    assert!(matches!(
        request.frame.body,
        FrameBody::ServiceRequest {
            ref request_id,
            ref service,
            ref operation,
            ..
        } if request_id == "prepare/26/outbound"
            && service == "fixture.echo"
            && operation == OP_OPEN
    ));
    assert!(
        outbound.try_recv().unwrap().is_none(),
        "outbound prepare must wait for the host rejection before Prepared"
    );
    assert_eq!(
        outbound
            .send(
                Lane::Data,
                &Frame::service_event(
                    Some("prepare/26/outbound".to_owned()),
                    "fixture.echo",
                    EVENT_CANCEL,
                    json!({"reason": "service_unavailable_during_prepare"}),
                ),
            )
            .unwrap(),
        CallOutcome::Ok
    );
    assert_eq!(
        recv_gate(&outbound),
        CapturedFrame {
            lane: Lane::Control,
            frame: Frame::lifecycle(LifecyclePhase::Prepared, 26, None),
        }
    );
}

#[test]
fn prepare_state_write_requires_read_only_conflict_and_treats_applied_as_failure() {
    let mut read_only = PluginHarness::start(rsi_meta_plugin_entry_v0).unwrap();
    assert_eq!(
        read_only
            .send(
                Lane::Control,
                &Frame::lifecycle(
                    LifecyclePhase::Prepare,
                    27,
                    Some(config_with_action("state_write_then_ack")),
                ),
            )
            .unwrap(),
        CallOutcome::Ok
    );
    let request = recv_gate(&read_only);
    let FrameBody::ServiceRequest {
        request_id,
        service,
        operation,
        payload,
    } = request.frame.body
    else {
        panic!("expected prepare CAS write")
    };
    assert_eq!(request.lane, Lane::Data);
    assert_eq!(service, "state.cas");
    assert_eq!(operation, STATE_OP_COMPARE_AND_SWAP);
    assert_eq!(payload["expected_version"], 0);
    assert_eq!(payload["value"], json!({"probe": "must-not-persist"}));
    assert!(read_only.try_recv().unwrap().is_none());
    assert_eq!(
        read_only
            .send(
                Lane::Data,
                &Frame::service_event(
                    Some(request_id),
                    "state.cas",
                    STATE_EVENT_CONFLICT,
                    json!({"reason": "prepare_read_only"}),
                ),
            )
            .unwrap(),
        CallOutcome::Ok
    );
    assert_eq!(
        recv_gate(&read_only),
        CapturedFrame {
            lane: Lane::Control,
            frame: Frame::lifecycle(LifecyclePhase::Prepared, 27, None),
        }
    );

    let mut incorrectly_applied = PluginHarness::start(rsi_meta_plugin_entry_v0).unwrap();
    assert_eq!(
        incorrectly_applied
            .send(
                Lane::Control,
                &Frame::lifecycle(
                    LifecyclePhase::Prepare,
                    28,
                    Some(config_with_action("state_write_then_ack")),
                ),
            )
            .unwrap(),
        CallOutcome::Ok
    );
    let request = recv_gate(&incorrectly_applied);
    let FrameBody::ServiceRequest { request_id, .. } = request.frame.body else {
        panic!("expected prepare CAS write")
    };
    assert_eq!(
        incorrectly_applied
            .send(
                Lane::Data,
                &Frame::service_event(
                    Some(request_id),
                    "state.cas",
                    STATE_EVENT_APPLIED,
                    json!({"key": "prepare/action-probe", "version": 1, "value": {}}),
                ),
            )
            .unwrap(),
        CallOutcome::Ok
    );
    assert_eq!(
        recv_gate(&incorrectly_applied),
        CapturedFrame {
            lane: Lane::Control,
            frame: Frame::lifecycle(
                LifecyclePhase::PrepareFailed,
                28,
                Some(json!({
                    "code": "prepare_write_applied",
                    "message": "state.cas write was applied during prepare",
                })),
            ),
        }
    );
}

#[test]
fn malformed_prepare_state_request_becomes_structured_prepare_failure() {
    let mut plugin = PluginHarness::start(rsi_meta_plugin_entry_v0).unwrap();
    assert_eq!(
        plugin
            .send(
                Lane::Control,
                &Frame::lifecycle(
                    LifecyclePhase::Prepare,
                    29,
                    Some(config_with_action("malformed_state_then_fail")),
                ),
            )
            .unwrap(),
        CallOutcome::Ok
    );
    let request = recv_gate(&plugin);
    let FrameBody::ServiceRequest {
        request_id,
        service,
        operation,
        payload,
    } = request.frame.body
    else {
        panic!("expected malformed prepare state request")
    };
    assert_eq!(request.lane, Lane::Data);
    assert_eq!(service, "state.cas");
    assert_eq!(operation, STATE_OP_GET);
    assert_eq!(payload, json!({}), "the missing key is intentional");
    assert!(plugin.try_recv().unwrap().is_none());
    assert_eq!(
        plugin
            .send(
                Lane::Data,
                &Frame::service_event(
                    Some(request_id),
                    "state.cas",
                    STATE_EVENT_CONFLICT,
                    json!({
                        "reason": "host_service_rejected",
                        "code": "invalid_request",
                    }),
                ),
            )
            .unwrap(),
        CallOutcome::Ok
    );
    assert_eq!(
        recv_gate(&plugin),
        CapturedFrame {
            lane: Lane::Control,
            frame: Frame::lifecycle(
                LifecyclePhase::PrepareFailed,
                29,
                Some(json!({
                    "code": "malformed_state_rejected",
                    "message": "host rejected malformed state.cas prepare request",
                })),
            ),
        }
    );
    assert!(plugin.try_recv().unwrap().is_none());
}
