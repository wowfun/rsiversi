mod support;

fn execution() -> rsi_meta::Execution {
    rsi_meta::Execution::native(tokio::runtime::Handle::current())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn session_free_shell_owns_bounded_isolated_profiles_with_shared_domain() {
    support::session_free_shell_owns_bounded_isolated_profiles_with_shared_domain(execution())
        .await;
}

#[tokio::test]
async fn retirement_cancels_partial_surface_activation_and_drains_its_owner() {
    support::retirement_cancels_partial_surface_activation_and_drains_its_owner(execution()).await;
}

#[tokio::test]
async fn abandoned_surface_cleanup_failure_cannot_be_reported_as_clean_shell_shutdown() {
    support::abandoned_surface_cleanup_failure_cannot_be_reported_as_clean_shell_shutdown(
        execution(),
    )
    .await;
}
