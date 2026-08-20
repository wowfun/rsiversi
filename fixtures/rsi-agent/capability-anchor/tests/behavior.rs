use std::time::Duration;

use rsi_agent_fixture_capability_anchor::rsi_meta_plugin_entry_v0;
use rsi_meta_plugin::{CallOutcome, Frame, FrameBody, Lane, LifecyclePhase};
use rsi_meta_plugin_testkit::PluginHarness;

#[test]
fn anchor_participates_in_lifecycle_without_exporting_a_service() {
    let mut plugin = PluginHarness::start(rsi_meta_plugin_entry_v0).expect("start anchor");
    assert_eq!(
        plugin
            .send(
                Lane::Control,
                &Frame::lifecycle(LifecyclePhase::Prepare, 1, None),
            )
            .expect("prepare callback"),
        CallOutcome::Ok
    );
    assert!(matches!(
        plugin
            .recv(Duration::from_secs(1))
            .expect("prepared")
            .frame
            .body,
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
            .expect("commit callback"),
        CallOutcome::Ok
    );
    assert_eq!(
        plugin
            .send(
                Lane::Control,
                &Frame::lifecycle(LifecyclePhase::Retire, 1, None),
            )
            .expect("retire callback"),
        CallOutcome::Ok
    );
    assert!(matches!(
        plugin
            .recv(Duration::from_secs(1))
            .expect("retired")
            .frame
            .body,
        FrameBody::Lifecycle {
            phase: LifecyclePhase::Retired,
            generation: 1,
            ..
        }
    ));
}
