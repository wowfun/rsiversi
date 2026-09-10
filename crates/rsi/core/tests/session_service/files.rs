use super::*;
use rsi_files_protocol::{FileKind, FilesError, OpenedFile, RelativePath};
use rsi_session_files::{SessionFiles, SessionFilesError};
use rsi_session_protocol::SessionTarget;

pub(super) async fn browse(
    sessions: &dyn SessionService,
    files: &dyn SessionFiles,
    workspace: rsi_workspace_protocol::WorkspaceId,
    label: &str,
) -> (SessionTarget, OpenedFile) {
    let mut last = None;
    for (index, trust) in [WorkspaceTrust::Untrusted, WorkspaceTrust::Trusted]
        .into_iter()
        .enumerate()
    {
        let handle = sessions
            .create(CreateSession {
                workspace_id: workspace.clone(),
                session_id: SessionId::new(format!("files-{label}-{index}")).unwrap(),
                agent_preset_id: None,
                workspace_trust: trust,
            })
            .await
            .unwrap();
        let header = handle.header().await.unwrap();
        assert_eq!(header.workspace_trust(), trust);
        let target = SessionTarget {
            session_id: header.session_id().clone(),
            header_key: header.fingerprint().unwrap(),
        };
        let file = files
            .open(
                target.clone(),
                RelativePath::new(b"sample").unwrap(),
                FileKind::File,
            )
            .await
            .unwrap();
        let page = files
            .read(target.clone(), file.clone(), 1, 2)
            .await
            .unwrap();
        assert_eq!(page.bytes_hex, "00ff");
        assert_eq!(page.total, 4);
        let directory = files
            .open(
                target.clone(),
                RelativePath::new(b"").unwrap(),
                FileKind::Directory,
            )
            .await
            .unwrap();
        let page = files
            .list(target.clone(), directory.clone(), 0, 1)
            .await
            .unwrap();
        assert_eq!(page.entries.len(), 1);
        assert_eq!(page.entries[0].name, "sample");
        files
            .release(target.clone(), directory.token)
            .await
            .unwrap();
        let mut wrong = target.clone();
        wrong.header_key = "0".repeat(64);
        assert_eq!(
            files.read(wrong, file.clone(), 0, 4).await.unwrap_err(),
            SessionFilesError::Files(FilesError::Unavailable)
        );
        let mut forged = file.clone();
        forged.path = RelativePath::new(b"other").unwrap();
        assert_eq!(
            files.read(target.clone(), forged, 0, 4).await.unwrap_err(),
            SessionFilesError::Files(FilesError::Binding)
        );
        assert!(
            handle
                .history_before(None, 8)
                .await
                .unwrap()
                .facts
                .is_empty()
        );
        last = Some((target, file));
    }
    assert!(
        sessions
            .list_recent(None, 8)
            .await
            .unwrap()
            .sessions
            .is_empty()
    );
    last.unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn actual_drafts_browse_through_embedded_and_uds_clients_without_publication() {
    let fixture = fixture("http://127.0.0.1:1");
    std::fs::write(fixture.workspace.join("sample"), b"a\0\xffz").unwrap();
    let daemon = DaemonFixture::new(&fixture).await;
    let workspace = daemon
        .connection
        .workspace_registry()
        .get_or_create(&fixture.workspace)
        .await
        .unwrap()
        .id;
    let clients = [
        daemon.running.session_files().unwrap(),
        daemon.connection.session_files(),
    ];
    let sessions = [
        daemon.running.session_service().unwrap(),
        daemon.connection.session_service(),
    ];
    for (index, (files, sessions)) in clients.iter().zip(sessions.iter()).enumerate() {
        browse(
            sessions.as_ref(),
            files.as_ref(),
            workspace.clone(),
            &format!("local-{index}"),
        )
        .await;
    }
    daemon.shutdown().await;
}

#[tokio::test]
async fn retained_files_token_does_not_keep_actual_standard_draft_alive() {
    let fixture = fixture("http://127.0.0.1:1");
    std::fs::write(fixture.workspace.join("sample"), b"a\0\xffz").unwrap();
    let running = RunningRsi::boot(composition(fixture.paths.clone()), &fixture.profile)
        .await
        .unwrap();
    let files = running.session_files().unwrap();
    let sessions = running.session_service().unwrap();
    let workspace = running
        .workspace_registry()
        .unwrap()
        .get_or_create(&fixture.workspace)
        .await
        .unwrap()
        .id;
    let (target, file) = browse(sessions.as_ref(), files.as_ref(), workspace, "expiry").await;
    tokio::time::pause();
    tokio::time::advance(std::time::Duration::from_mins(61)).await;
    tokio::task::yield_now().await;
    assert_eq!(
        files.read(target, file, 0, 4).await.unwrap_err(),
        SessionFilesError::Files(FilesError::Unavailable)
    );
    tokio::time::resume();
    assert!(running.shutdown().await.is_clean());
}
