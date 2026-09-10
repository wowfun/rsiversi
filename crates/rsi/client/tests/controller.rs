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
