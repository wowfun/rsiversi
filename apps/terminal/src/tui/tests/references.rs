use super::*;
use rsi_agent_session_protocol::{
    FrozenReference, ReferenceBinding, ReferenceCapture, ReferenceMetadata, ReferenceSnapshotRef,
    ReferenceSource, ReferenceSuffix,
};
pub(super) fn frozen(header: &SessionHeader) -> FrozenReference {
    FrozenReference {
        snapshot: ReferenceSnapshotRef {
            capture_id: "fixture-capture".into(),
            sha256: "a".repeat(64),
            byte_len: 900,
        },
        metadata: ReferenceMetadata {
            source: ReferenceSource::Native {
                binding: ReferenceBinding {
                    session_id: SessionId::new("source").unwrap(),
                    header_sha256: "b".repeat(64),
                },
            },
            target: ReferenceBinding {
                session_id: header.session_id().clone(),
                header_sha256: header.fingerprint().unwrap(),
            },
            text_bytes: 12,
            capture: ReferenceCapture::Suffix {
                interval: ReferenceSuffix {
                    through_seq: 2,
                    fact_prefix_sha256: "c".repeat(64),
                    scanned_after_seq: 0,
                    retained_after_seq: 0,
                    retained_through_seq: 1,
                    scanned_bytes: 400,
                    omissions: vec![],
                },
            },
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
    let mut duplicate = reference.clone();
    duplicate.snapshot.capture_id = "different-capture".into();
    client.action(Action::AddReference(duplicate));
    assert_eq!(
        client.state.references,
        vec![reference.clone()],
        "capture identity does not make duplicate content distinct"
    );
    assert!(client.state.reference_bytes > reference.preview.len());
    // A different Header identity cannot receive a saved descriptor through Add.
    let mut wrong = reference.clone();
    wrong.metadata.target.session_id = SessionId::new("different").unwrap();
    client.action(Action::AddReference(wrong));
    assert_eq!(client.state.references.len(), 1);
    client.submit(MessageDelivery::NextTurn, false);
    let frozen_input = client.submission.request.clone().unwrap();
    assert!(
        matches!(&frozen_input.content[..],[MessageInput::Text {text},MessageInput::Reference {reference:actual}] if text=="draft suffix" && actual.as_ref()==&reference)
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
    let (mut client, handle, runtime, surface) = client().await;
    let reference = frozen(&client.state.header);
    client.state.editor.insert("x").unwrap();
    client.action(Action::AddReference(reference.clone()));
    client.action(Action::RemoveReference(
        reference.snapshot.capture_id.clone(),
    ));
    assert!(client.state.references.is_empty());
    client.tasks.next().await.unwrap().result.unwrap();
    assert_eq!(
        *handle.released_references.lock().unwrap(),
        vec![reference.snapshot.clone()]
    );
    client.action(Action::RemoveReference(
        reference.snapshot.capture_id.clone(),
    ));
    assert!(
        client.tasks.is_empty(),
        "duplicate removal releases no other capture"
    );
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

async fn wait_releases(handle: &UnknownThenAcceptedHandle, count: usize) {
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        while handle.released_references.lock().unwrap().len() < count {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
}
async fn show_capture(client: &mut Client) -> FrozenReference {
    client.action(Action::CaptureReference(SessionId::new("source").unwrap()));
    let Update::CapturedReference(page, lease) = client.tasks.next().await.unwrap().result.unwrap()
    else {
        panic!("capture result");
    };
    let reference = page.reference.clone();
    client.state.show_reference(page);
    client.state.reference_preview = Some(lease);
    reference
}
#[tokio::test]
async fn abandoned_previews_release_slots_and_paging_then_add_transfers_ownership() {
    let (mut client, handle, runtime, surface) = client().await;
    *handle.capture.lock().unwrap() = Some(frozen(&client.state.header));
    for count in 1..=6 {
        show_capture(&mut client).await;
        client.state.escape();
        wait_releases(&handle, count).await;
    }
    let reference = show_capture(&mut client).await;
    client.action(Action::PreviewReference(reference.clone(), 0));
    let Update::Reference(page) = client.tasks.next().await.unwrap().result.unwrap() else {
        panic!("page");
    };
    client.state.show_reference(page);
    assert_eq!(handle.released_references.lock().unwrap().len(), 6);
    client.action(Action::AddReference(reference.clone()));
    client.state.escape();
    client.reference_previews.close().await.unwrap();
    assert_eq!(client.state.references, vec![reference]);
    assert_eq!(handle.released_references.lock().unwrap().len(), 6);
    drop(client);
    surface.stop().await;
    assert!(runtime.shutdown().await.is_clean());
}
#[tokio::test]
async fn failed_and_late_previews_release_even_after_session_switch() {
    let (mut client, handle, runtime, surface) = client().await;
    *handle.capture.lock().unwrap() = Some(frozen(&client.state.header));
    handle
        .preview_failure
        .store(true, std::sync::atomic::Ordering::SeqCst);
    client.action(Action::CaptureReference(SessionId::new("source").unwrap()));
    assert!(client.tasks.next().await.unwrap().result.is_err());
    wait_releases(&handle, 1).await;
    handle
        .preview_failure
        .store(false, std::sync::atomic::Ordering::SeqCst);
    let gate = Arc::new(tokio::sync::Semaphore::new(0));
    *handle.capture_gate.lock().unwrap() = Some(gate.clone());
    client.action(Action::CaptureReference(SessionId::new("source").unwrap()));
    {
        let pending = client.tasks.next();
        tokio::pin!(pending);
        tokio::select! { _ = &mut pending => panic!("capture is blocked"), () = async {
            while handle.captures.load(std::sync::atomic::Ordering::SeqCst) < 2 { tokio::task::yield_now().await; }
        } => {} }
    }
    client.state.escape();
    client.reset_attachment_tasks();
    gate.add_permits(1);
    wait_releases(&handle, 2).await;
    client.reference_previews.close().await.unwrap();
    drop(client);
    surface.stop().await;
    assert!(runtime.shutdown().await.is_clean());
}
#[tokio::test]
async fn failed_preview_cleanup_keeps_identity_until_explicit_capture_retry() {
    let (mut client, handle, runtime, surface) = client().await;
    *handle.capture.lock().unwrap() = Some(frozen(&client.state.header));
    let first = show_capture(&mut client).await;
    handle
        .release_failure
        .store(true, std::sync::atomic::Ordering::SeqCst);
    client.state.escape();
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        while client.reference_previews.notice().is_none() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    // A failed release consumes its own slot, not all remaining capture capacity.
    show_capture(&mut client).await;
    client.state.escape();
    tokio::task::yield_now().await;
    handle
        .release_failure
        .store(false, std::sync::atomic::Ordering::SeqCst);
    show_capture(&mut client).await;
    assert_eq!(
        handle.released_references.lock().unwrap()[0],
        first.snapshot
    );
    client.state.escape();
    client.reference_previews.close().await.unwrap();
    assert_eq!(handle.released_references.lock().unwrap().len(), 3);
    drop(client);
    surface.stop().await;
    assert!(runtime.shutdown().await.is_clean());
}
