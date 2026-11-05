mod support;

#[tokio::test]
async fn commands_freeze_identity_query_without_replay_and_retire_owned_waits() {
    support::exact_command_reconciliation(execution()).await;
}

#[tokio::test]
async fn explicit_reconciliation_stop_preserves_unknown_identity_and_drains_the_controller() {
    support::explicit_reconciliation_cancellation(execution()).await;
}

#[tokio::test(start_paused = true)]
async fn finite_reads_recover_capacity_with_bounded_attempts_and_cancel_without_detached_retries() {
    support::bounded_read_capacity_recovery_and_cancellation(execution()).await;
}

fn execution() -> rsi_meta::Execution {
    rsi_meta::Execution::native(tokio::runtime::Handle::current())
}

#[tokio::test]
async fn two_real_scopes_share_a_domain_and_retire_their_controllers_independently() {
    support::isolated_controller_scopes(execution()).await;
}

#[tokio::test]
async fn dropped_submission_waiters_keep_owned_work_until_plugin_retirement_drains_it() {
    support::owned_submission_drain(execution()).await;
}

#[tokio::test]
async fn replay_cursor_advances_after_delivery_acknowledgement_without_skipping_to_watermark() {
    support::acknowledged_cursor(execution()).await;
}

#[tokio::test]
async fn message_driver_preserves_claim_identity_cancellation_and_terminal_delivery() {
    support::message_claim_cancellation_and_terminal_delivery(execution()).await;
}

#[tokio::test(start_paused = true)]
async fn fresh_projection_baselines_reconnect_and_fail_independently_of_core_history() {
    support::independent_projection_observation(execution()).await;
}

#[tokio::test]
async fn exact_source_reads_cancel_and_retire_under_shared_bounded_admission() {
    support::owned_source_window_reads(execution()).await;
}

#[tokio::test]
async fn workflow_latest_captures_seed_without_inspection_round_trip() {
    support::latest_workflows_need_no_inspection(execution()).await;
}

#[tokio::test]
async fn final_inputs_preserve_order_detail_coordinates_and_read_only_missing_claims() {
    use rsi_agent_session_protocol::SessionFact;
    use rsi_conversation::{FactField, InputPart};
    use serde_json::json;
    let handle = support::Handle::new("entered", false);
    let original: SessionFact = serde_json::from_value(json!({"seq":1,"timestamp_ms":1,"type":"input_message_claimed","turn_id":"turn","step_id":"step","source":{"type":"human","message_id":"original"},"content":[{"type":"text","text":"original"}]})).unwrap();
    let decision = |decision| {
        serde_json::from_value::<SessionFact>(json!({"seq":8,"timestamp_ms":1,"type":"step_inputs_prepared","turn_id":"turn","step_id":"step","decision":decision})).unwrap()
    };
    let claimed = json!({"kind":"claimed","claim":{"fact_seq":1,"ordinal":0}});
    let added = |role, text| json!({"kind":"added","message":{"role":role,"attribution":{"id":"plugin","source":{}},"content":[{"type":"text","text":text}]}});
    let authored = added("user", "rewritten");
    let prep = decision(
        json!({"kind":"enter","messages":[added("developer","guidance"),authored, claimed, claimed]}),
    );
    *handle.source_reads.fact.lock().unwrap() = Some(original.clone());
    for page in [std::slice::from_ref(&original), &[]] {
        let mut rows = Vec::new();
        rsi_client::visit_prepared_inputs(handle.as_ref(), &prep, page, |row| {
            let (origin, InputPart::Text(text)) = row.parts().next().unwrap() else {
                panic!("expected text")
            };
            rows.push((row.index, origin.seq, origin.field, text.to_owned()));
        })
        .await
        .unwrap();
        assert_eq!(
            rows,
            [
                (1, 8, FactField::InputText { index: 1 }, "rewritten".into()),
                (2, 1, FactField::InputText { index: 0 }, "original".into()),
                (3, 1, FactField::InputText { index: 0 }, "original".into())
            ]
        );
        assert_eq!(
            handle.source_reads.requests.lock().unwrap().len(),
            usize::from(page.is_empty())
        );
    }
    let retention = rsi_agent_turn_protocol::ObservationRetention::default();
    let held = retention
        .retain_fact(std::sync::Arc::new(original))
        .unwrap();
    let mut live = rsi_client::LiveInputClaims::default();
    live.observe(&held);
    drop(held);
    let mut shown = Vec::new();
    live.prepare(handle.as_ref(), &prep, |row| shown.push(row.index))
        .await
        .unwrap();
    assert_eq!(shown, [1, 2, 3]);
    assert_eq!(retention.retained_bytes(), 0);
    for prep in [
        decision(json!({"kind":"reject"})),
        decision(json!({"kind":"enter","messages":[]})),
        decision(json!({"kind":"enter","messages":[added("user","local")]})),
    ] {
        rsi_client::visit_prepared_inputs(handle.as_ref(), &prep, &[], |_| {})
            .await
            .unwrap();
    }
    assert_eq!(
        *handle.source_reads.requests.lock().unwrap(),
        [(Some(2), 1)]
    );
}
