use super::*;
use rsi_agent_session_protocol::{
    SessionResourceRequest, SessionResourceResponse, SessionResourceValue,
};

fn response() -> Value {
    serde_json::to_value(SessionResourceResponse {
        session_id: header().session_id().clone(),
        header_sha256: header().fingerprint().unwrap(),
        composition_sha256: "a".repeat(64),
        request: SessionResourceRequest::Sources,
        value: SessionResourceValue::Sources {
            sources: Vec::new(),
        },
    })
    .unwrap()
}

#[tokio::test]
async fn resource_reads_reject_wrong_target_request_and_digest_before_exposure() {
    let (remote, _, handle) = fixture().await;
    for (field, value) in [
        ("session_id", json!("foreign")),
        ("header_sha256", json!("b".repeat(64))),
        ("composition_sha256", json!("not-a-digest")),
        ("request", json!({"kind":"list","source":"different"})),
        ("value", json!({"kind":"list","entries":[]})),
    ] {
        let mut bad = response();
        bad[field] = value;
        remote.reply(&envelope(bad));
        assert!(
            handle
                .read_resource(SessionResourceRequest::Sources)
                .await
                .is_err()
        );
    }
}

#[tokio::test]
async fn resource_retention_follows_last_clone_after_wire_release() {
    let (remote, client, handle) = fixture().await;
    remote.reply(&envelope(response()));
    let result = handle
        .read_resource(SessionResourceRequest::Sources)
        .await
        .unwrap();
    assert_eq!(remote.output.used(), 0);
    assert_eq!(
        client.state.resources.retained_bytes(),
        serde_json::to_vec(result.response()).unwrap().len()
    );
    let clone = result.clone();
    drop(result);
    assert!(client.state.resources.retained_bytes() > 0);
    drop(clone);
    assert_eq!(client.state.resources.retained_bytes(), 0);
}
