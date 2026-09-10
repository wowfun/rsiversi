mod support;

#[tokio::test]
async fn malformed_peer_file_pages_are_fenced() {
    support::malformed_open_binding_and_file_pages_never_escape_or_replay().await;
}

#[tokio::test]
async fn malformed_peer_directory_pages_are_fenced() {
    support::directory_client_checks_parent_exact_names_order_and_forward_progress().await;
}
