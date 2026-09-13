use super::*;
use rsi_agent_turn_protocol::TurnJobsRequest;

fn request() -> TurnJobsRequest {
    TurnJobsRequest {
        turn_id: TurnId::new("active").unwrap(),
        generation: Some(7),
        after: Some("job-0".into()),
        limit: 2,
    }
}
fn page() -> Value {
    json!({"session_id":header().session_id(),"header_sha256":header().fingerprint().unwrap(),"turn_id":"active","generation":7,"after":"job-0","has_more":false,
        "jobs":[{"id":"job-1","name":"work","producer":"bash","status":"completed","requires_report":true,"reported":false,"terminal":{"status":"completed","exit_code":0,"signal":null,"message":null},"output_retained":true}]})
}

#[tokio::test]
async fn jobs_validate_exact_cursor_generation_and_summary_without_replaying() {
    let (remote, _, handle) = fixture().await;
    for (key, value) in [
        ("session_id", json!("foreign")),
        ("header_sha256", json!("b".repeat(64))),
        ("turn_id", json!("foreign")),
        ("generation", json!(8)),
        ("after", json!("job-other")),
    ] {
        let mut invalid = page();
        invalid[key] = value;
        remote.reply(&envelope(invalid));
        assert!(matches!(
            handle.read_jobs(request()).await,
            Err(SessionError::Api(ApiError::Invalid(_)))
        ));
    }
    for (field, replacement) in [
        ("reported", json!(true)),
        ("status", json!("running")),
        ("id", json!("job-0")),
        ("name", json!("x".repeat(257))),
    ] {
        let mut invalid = page();
        invalid["jobs"][0][field] = replacement;
        remote.reply(&envelope(invalid));
        assert!(matches!(
            handle.read_jobs(request()).await,
            Err(SessionError::Api(ApiError::Invalid(_)))
        ));
    }
    let before = remote.calls.load(Ordering::SeqCst);
    let mut invalid = request();
    invalid.generation = None;
    assert!(matches!(
        handle.read_jobs(invalid).await,
        Err(SessionError::Invalid(_))
    ));
    assert_eq!(remote.calls.load(Ordering::SeqCst), before);
}

#[tokio::test]
async fn decoded_jobs_retention_survives_transport_and_final_clone() {
    let (remote, client, handle) = fixture().await;
    remote.reply(&envelope(page()));
    let snapshot = handle.read_jobs(request()).await.unwrap();
    assert_eq!(remote.output.used(), 0);
    let bytes = snapshot.page().encoded_len().unwrap();
    assert_eq!(client.state.jobs.retained_bytes(), bytes);
    let clone = snapshot.clone();
    drop(snapshot);
    assert_eq!(client.state.jobs.retained_bytes(), bytes);
    drop(clone);
    assert_eq!(client.state.jobs.retained_bytes(), 0);
    let captures = (0..8)
        .map(|_| client.state.jobs.reserve_capture().unwrap())
        .collect::<Vec<_>>();
    assert!(matches!(
        client.state.jobs.reserve_decode(),
        Err(SessionError::Capacity)
    ));
    drop(captures);
    assert_eq!(client.state.jobs.retained_bytes(), 0);
}
