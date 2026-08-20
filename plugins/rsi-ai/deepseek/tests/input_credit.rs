use std::time::Duration;

use rsi_ai_meta::{AiService, ClientControl, encode_client_control};
use rsi_ai_plugin_deepseek::rsi_meta_plugin_entry_v0;
use rsi_ai_protocol::{MediaDescriptor, MediaKind, WireFrame, encode_wire_frame};
use rsi_meta_plugin::{
    CallOutcome, EVENT_CREDIT, Frame, FrameBody, Lane, LifecyclePhase, OP_OPEN, PostFrameOutcome,
    RUNTIME_TICK_EVENT, RUNTIME_TICK_SERVICE,
};
use rsi_meta_plugin_testkit::PluginHarness;
use serde_json::json;

fn receive_credit(harness: &PluginHarness) {
    let posted = harness.recv(Duration::from_secs(1)).expect("credit frame");
    assert_eq!(posted.lane, Lane::Data);
    assert!(matches!(
        posted.frame.body,
        FrameBody::ServiceEvent { ref event, .. } if event == EVENT_CREDIT
    ));
}

#[test]
fn input_credit_backpressure_is_retryable_after_control_state_changes() {
    let mut harness = PluginHarness::start(rsi_meta_plugin_entry_v0).expect("plugin");
    assert_eq!(
        harness
            .send(
                Lane::Control,
                &Frame::lifecycle(
                    LifecyclePhase::Prepare,
                    7,
                    Some(json!({"api_key":"fixture-secret"})),
                ),
            )
            .expect("prepare"),
        CallOutcome::Ok
    );
    let prepared = harness.recv(Duration::from_secs(1)).expect("prepared");
    assert!(matches!(
        prepared.frame.body,
        FrameBody::Lifecycle {
            phase: LifecyclePhase::Prepared,
            generation: 7,
            ..
        }
    ));
    assert_eq!(
        harness
            .send(
                Lane::Control,
                &Frame::lifecycle(LifecyclePhase::Committed, 7, None),
            )
            .expect("commit"),
        CallOutcome::Ok
    );
    assert_eq!(
        harness
            .send(
                Lane::Data,
                &Frame::service_request(
                    "stream-1",
                    AiService::Language.key(),
                    OP_OPEN,
                    json!({"consumer":"fixture", "sequence":0}),
                ),
            )
            .expect("open"),
        CallOutcome::Ok
    );
    receive_credit(&harness);

    let descriptor = MediaDescriptor::new(
        MediaKind::Audio,
        "audio/wav",
        1,
        "ca358758f6d27e6cf45272937977a748fd88391db679ceda7dc7bf1f005ee879",
    )
    .expect("descriptor");
    let control = ClientControl::DeclareInputBlob {
        call_id: "call-1".to_owned(),
        blob_id: "blob-1".to_owned(),
        descriptor,
    };
    let nested = encode_wire_frame(&WireFrame::Control {
        call_id: "call-1".to_owned(),
        payload: encode_client_control(&control).expect("control"),
    })
    .expect("nested frame");

    harness.set_post_outcomes([PostFrameOutcome::WouldBlock]);
    assert_eq!(
        harness
            .send(
                Lane::Data,
                &Frame::service_data_request(
                    "stream-1",
                    AiService::Language.key(),
                    nested.clone(),
                ),
            )
            .expect("declare input blob"),
        CallOutcome::Ok,
        "credit backpressure must not fail a DATA frame after its state change"
    );

    assert_eq!(
        harness
            .send(
                Lane::Data,
                &Frame::service_data_request("stream-1", AiService::Language.key(), nested,),
            )
            .expect("duplicate declaration"),
        CallOutcome::Failed,
        "the first declaration must remain applied"
    );
    assert_eq!(
        harness
            .send(
                Lane::Data,
                &Frame::service_event(None, RUNTIME_TICK_SERVICE, RUNTIME_TICK_EVENT, json!({}),),
            )
            .expect("runtime tick"),
        CallOutcome::Ok
    );
    receive_credit(&harness);
}
