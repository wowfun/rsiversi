use super::*;
use rsi_agent_session_protocol::{
    FrozenReference, ReferenceBinding, ReferenceMetadata, ReferenceSnapshotRef,
};
fn frozen(header: &SessionHeader) -> FrozenReference {
    FrozenReference {
        snapshot: ReferenceSnapshotRef {
            sha256: "a".repeat(64),
            byte_len: 900,
        },
        metadata: ReferenceMetadata {
            source: ReferenceBinding {
                session_id: SessionId::new("source").unwrap(),
                header_sha256: "b".repeat(64),
            },
            target: ReferenceBinding {
                session_id: header.session_id().clone(),
                header_sha256: header.fingerprint().unwrap(),
            },
            through_seq: 2,
            fact_prefix_sha256: "c".repeat(64),
            scanned_after_seq: 0,
            retained_after_seq: 0,
            retained_through_seq: 1,
            scanned_bytes: 400,
            text_bytes: 12,
            omissions: vec![],
        },
        preview: "你好世界".into(),
    }
}
#[tokio::test]
async fn frozen_draft_reference_preserves_cursor_binding_and_submission_identity() {
    let (mut client, handle, runtime, surface) = client().await;
    let reference = frozen(&client.state.header);
    client.state.editor.insert("draft suffix").unwrap();
    client.state.editor.key(KeyCode::Left.into()).unwrap();
    let cursor = client.state.editor.cursor();
    client.action(Action::AddReference(reference.clone()));
    assert_eq!(client.state.editor.text(), "draft suffix");
    assert_eq!(client.state.editor.cursor(), cursor);
    assert_eq!(client.state.references, vec![reference.clone()]);
    assert!(client.state.reference_bytes > reference.preview.len());
    // A different Header identity cannot receive a saved descriptor through Add.
    let mut wrong = reference.clone();
    wrong.metadata.target.session_id = SessionId::new("different").unwrap();
    client.action(Action::AddReference(wrong));
    assert_eq!(client.state.references.len(), 1);
    client.submit(MessageDelivery::NextTurn, false);
    let frozen_input = client.submission.request.clone().unwrap();
    assert!(
        matches!(&frozen_input.content[..],[MessageInput::Text {text},MessageInput::Reference {reference:actual}] if text=="draft suffix" && actual==&reference)
    );
    assert!(client.state.references.is_empty());
    client.state.editor.insert("later edit").unwrap();
    let result = client.tasks.next().await.unwrap().result.unwrap();
    let Update::Submitted(result) = result else {
        panic!("submission outcome")
    };
    assert!(result.is_ok());
    assert_eq!(
        serde_json::to_value(client.submission.request.as_ref().unwrap()).unwrap(),
        serde_json::to_value(&frozen_input).unwrap()
    );
    assert_eq!(client.state.editor.text(), "later edit");
    assert_eq!(
        serde_json::to_value(&handle.submitted_requests.lock().unwrap()[0]).unwrap(),
        serde_json::to_value(&frozen_input).unwrap()
    );
    drop(client);
    surface.stop().await;
    assert!(runtime.shutdown().await.is_clean());
}
#[tokio::test]
async fn removing_and_rejecting_oversized_reference_drafts_preserves_text() {
    let (mut client, _, runtime, surface) = client().await;
    let reference = frozen(&client.state.header);
    client.state.editor.insert("x").unwrap();
    client.action(Action::AddReference(reference.clone()));
    client.action(Action::RemoveReference(reference.snapshot.sha256.clone()));
    assert!(client.state.references.is_empty());
    assert_eq!(client.state.reference_bytes, 0);
    assert_eq!(client.state.editor.text(), "x");
    client
        .state
        .editor
        .replace_text(&"x".repeat(1024 * 1024))
        .unwrap();
    client.action(Action::AddReference(reference));
    assert!(client.state.references.is_empty());
    assert_eq!(client.state.editor.text().len(), 1024 * 1024);
    assert!(client.submission.request.is_none());
    drop(client);
    surface.stop().await;
    assert!(runtime.shutdown().await.is_clean());
}
