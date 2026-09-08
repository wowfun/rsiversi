mod support;

#[tokio::test]
async fn declaration_reorder_drives_surfaces_and_renderers_with_actual_target_mappings() {
    support::declaration_reorder_drives_surfaces_and_renderers_with_actual_target_mappings(
        rsi_meta::Execution::native(tokio::runtime::Handle::current()),
    )
    .await;
}

#[tokio::test]
async fn dropped_waiter_and_contribution_retirement_drain_admitted_mutation_once() {
    support::dropped_waiter_and_contribution_retirement_drain_admitted_mutation_once(
        rsi_meta::Execution::native(tokio::runtime::Handle::current()),
    )
    .await;
}

#[tokio::test]
async fn replacement_target_foreign_application_and_business_payload_are_fenced() {
    support::replacement_target_foreign_application_and_business_payload_are_fenced(
        rsi_meta::Execution::native(tokio::runtime::Handle::current()),
    )
    .await;
}

#[tokio::test]
async fn action_capacity_is_not_released_when_response_waiters_are_dropped() {
    support::action_capacity_is_not_released_when_response_waiters_are_dropped(
        rsi_meta::Execution::native(tokio::runtime::Handle::current()),
    )
    .await;
}

#[tokio::test]
async fn duplicate_and_failed_activation_preserve_existing_bundle_and_reject_foreign_buttons() {
    support::duplicate_and_failed_activation_preserve_existing_bundle_and_reject_foreign_buttons(
        rsi_meta::Execution::native(tokio::runtime::Handle::current()),
    )
    .await;
}

#[tokio::test]
async fn independent_registry_and_reactivated_bundle_never_reuse_old_references() {
    support::independent_registry_and_reactivated_bundle_never_reuse_old_references(
        rsi_meta::Execution::native(tokio::runtime::Handle::current()),
    )
    .await;
}
