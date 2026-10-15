#![cfg(windows)]
use rsi_files::LocalFiles;
use rsi_files_protocol::*;
use std::{fs, path::Path};
use tokio_util::sync::CancellationToken;

fn binding(root: &Path) -> FilesBinding {
    FilesBinding::new(FilesCaller::default(), "session", "header", root.to_owned()).unwrap()
}
fn path(value: &str) -> RelativePath {
    RelativePath::new(value.as_bytes()).unwrap()
}

#[tokio::test]
async fn native_windows_reader_pages_unicode_and_refuses_changed_identity() {
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path();
    fs::write(root.join("中文.txt"), "abcdefghij").unwrap();
    let service = LocalFiles::new().unwrap();
    let scope = binding(root);
    let token = CancellationToken::new();
    let file = service
        .open(
            scope.clone(),
            path("中文.txt"),
            FileKind::File,
            token.clone(),
        )
        .await
        .unwrap();
    let first = service
        .read(scope.clone(), file.token.clone(), 2, 3, token.clone())
        .await
        .unwrap();
    assert_eq!(first.bytes_hex, "636465");
    assert_eq!(first.total, 10);
    fs::rename(root.join("中文.txt"), root.join("original.txt")).unwrap();
    fs::write(root.join("中文.txt"), "abcdefghij").unwrap();
    assert_eq!(
        service
            .read(scope.clone(), file.token, 0, 3, token.clone())
            .await,
        Err(FilesError::Changed)
    );
    let directory = service
        .open(scope.clone(), path(""), FileKind::Directory, token.clone())
        .await
        .unwrap();
    let page = service
        .list(scope.clone(), directory.token.clone(), 0, 1, token.clone())
        .await
        .unwrap();
    assert_eq!(page.total, 2);
    assert_eq!(page.entries.len(), 1);
    fs::write(root.join("new.txt"), "new").unwrap();
    assert_eq!(
        service.list(scope, directory.token, 0, 1, token).await,
        Err(FilesError::Changed)
    );
    service.close().await;
}

#[tokio::test]
async fn windows_reader_rejects_unselectable_names_and_native_alias_syntax() {
    use std::os::windows::ffi::OsStringExt as _;
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path();
    let service = LocalFiles::new().unwrap();
    for invalid in ["../escape", "nested\\file.txt", "file.txt:stream", "NUL"] {
        let Ok(relative) = RelativePath::new(invalid.as_bytes()) else {
            continue;
        };
        assert!(
            service
                .open(
                    binding(root),
                    relative,
                    FileKind::File,
                    CancellationToken::new()
                )
                .await
                .is_err()
        );
    }
    fs::write(
        root.join(std::ffi::OsString::from_wide(&[0xd800])),
        "invalid UTF-8 wire name",
    )
    .unwrap();
    assert!(
        service
            .open(
                binding(root),
                path(""),
                FileKind::Directory,
                CancellationToken::new()
            )
            .await
            .is_err()
    );
    service.close().await;
}
