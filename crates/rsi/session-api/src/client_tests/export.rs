use super::*;
use rsi_session_protocol::export::{ExportEvent, ExportOptions};

fn start() -> Value {
    serde_json::to_value(ExportEvent::Start {
        session_id: header().session_id().clone(),
        header_sha256: header().fingerprint().unwrap(),
        through_seq: "0".into(),
        options: ExportOptions::default(),
        filename: "session.md".into(),
    })
    .unwrap()
}
#[tokio::test]
async fn export_rejects_wrong_binding_and_truncated_or_corrupt_streams() {
    let (remote, _, handle) = fixture().await;
    for (field, value) in [
        ("session_id", json!("foreign")),
        ("header_sha256", json!("a".repeat(64))),
        ("options", json!({"format":"json","include":["messages"]})),
    ] {
        let mut value_start = start();
        value_start[field] = value;
        remote.stream(&[envelope(value_start)]);
        let mut stream = handle.export(ExportOptions::default()).await.unwrap();
        assert!(stream.next().await.unwrap().is_err());
    }
    for ending in [
        None,
        Some(json!({"type":"chunk","offset":"5","text":"bad"})),
        Some(json!({"type":"complete","bytes":"0","sha256":"a".repeat(64)})),
    ] {
        let mut values = vec![envelope(start())];
        if let Some(ending) = ending {
            values.push(envelope(ending));
        }
        remote.stream(&values);
        let mut stream = handle.export(ExportOptions::default()).await.unwrap();
        stream.next().await.unwrap().unwrap();
        assert!(stream.next().await.unwrap().is_err());
    }
    let complete = json!({"type":"complete","bytes":"0","sha256":"e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"});
    remote.stream(&[envelope(start()), envelope(complete)]);
    let mut stream = handle.export(ExportOptions::default()).await.unwrap();
    stream.next().await.unwrap().unwrap();
    stream.next().await.unwrap().unwrap();
    assert!(stream.next().await.is_none());
}
