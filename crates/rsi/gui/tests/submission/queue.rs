use super::*;
use serde_json::json;
#[tokio::test]
async fn queue_unknown_reply_preserves_complete_frozen_input_and_queries_the_same_operation() {
    let (runtime, backend, app, generation) = admission::fixture().await;
    backend
        .queue_unknown
        .store(true, std::sync::atomic::Ordering::SeqCst);
    let prepared:serde_json::Value=serde_json::from_str(&app.prepare_submission(&json!({"pane":"main","generation":generation,"queue":{"slot_id":"original","expected_message_id":"original","action":"replace","content":[{"type":"text","text":"first block"},{"type":"text","text":"second block"}]}}).to_string()).await.unwrap()).unwrap();
    assert_eq!(prepared["kind"], "queue");
    let first = dispatch(&app, &generation, &prepared, "dispatch").await;
    assert_eq!(first["status"], "unknown");
    let queried = dispatch(&app, &generation, &prepared, "query").await;
    assert_eq!(queried["status"], "complete");
    assert_eq!(backend.queue_requests.lock().unwrap().len(), 1);
    {
        let requests = backend.queue_requests.lock().unwrap();
        assert_eq!(
            requests[0].operation_id.as_str(),
            prepared["id"].as_str().unwrap()
        );
        assert!(
            matches!(&requests[0].mutation,QueueMutation::Replace {content,..} if content.len()==2)
        );
    }
    assert!(runtime.shutdown().await.is_clean());
}

#[tokio::test]
async fn operation_identity_conflict_is_terminal_on_dispatch_and_explicit_retry() {
    let (runtime, backend, app, generation) = admission::fixture().await;
    backend
        .queue_conflict
        .store(true, std::sync::atomic::Ordering::SeqCst);
    let prepared:serde_json::Value=serde_json::from_str(&app.prepare_submission(&json!({"pane":"main","generation":generation,"queue":{"slot_id":"original","expected_message_id":"original","action":"withdraw"}}).to_string()).await.unwrap()).unwrap();
    for mode in ["dispatch", "retry_message"] {
        let result = dispatch(&app, &generation, &prepared, mode).await;
        assert_eq!(result["status"], "rejected");
        assert!(
            result["error"]
                .as_str()
                .unwrap()
                .contains("Reopen the current input")
        );
        assert!(result.get("receipt").is_none());
    }
    assert!(backend.queue_requests.lock().unwrap().is_empty());
    assert!(runtime.shutdown().await.is_clean());
}
