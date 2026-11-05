use rsi_files::LocalFiles;
use rsi_files_protocol::*;
use tokio_util::sync::CancellationToken;

fn request(cwd: &std::path::Path, path: &[u8], kind: FileKind) -> PathRead {
    PathRead::new(cwd.to_owned(), ReadPath::new(path).unwrap(), kind, 0, 32).unwrap()
}

#[tokio::test]
async fn finite_reads_resolve_on_host_without_tokens_and_observe_fresh_contents() {
    let temporary = tempfile::tempdir().unwrap();
    let root = rsi_workspace_path::canonical_native_path(temporary.path().canonicalize().unwrap());
    std::fs::create_dir(root.join("cwd")).unwrap();
    std::fs::write(root.join("outside.txt"), b"outside").unwrap();
    let files = LocalFiles::new().unwrap();
    let binding =
        FilesBinding::new(FilesCaller::default(), "test", "revision", root.clone()).unwrap();
    // Saturate only retained-token capacity. A finite read has no continuation to retain.
    for _ in 0..MAXIMUM_FILE_TOKENS {
        files
            .open(
                binding.clone(),
                RelativePath::new(b"outside.txt").unwrap(),
                FileKind::File,
                CancellationToken::new(),
            )
            .await
            .unwrap();
    }
    for path in [
        b"../outside.txt".to_vec(),
        root.join("outside.txt")
            .as_os_str()
            .as_encoded_bytes()
            .to_vec(),
    ] {
        let result = files
            .read_path(
                request(&root.join("cwd"), &path, FileKind::File),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert_eq!(
            result.path.as_bytes(),
            root.join("outside.txt").as_os_str().as_encoded_bytes()
        );
        let ReadPage::File(page) = result.page else {
            panic!("file")
        };
        assert_eq!(hex::decode(page.bytes_hex).unwrap(), b"outside");
    }
    std::fs::write(root.join("outside.txt"), b"changed").unwrap();
    let result = files
        .read_path(
            request(&root, b"outside.txt", FileKind::File),
            CancellationToken::new(),
        )
        .await
        .unwrap();
    let ReadPage::File(page) = result.page else {
        panic!("file")
    };
    assert_eq!(hex::decode(page.bytes_hex).unwrap(), b"changed");
    let result = files
        .read_path(
            request(&root, b".", FileKind::Directory),
            CancellationToken::new(),
        )
        .await
        .unwrap();
    let ReadPage::Directory(page) = result.page else {
        panic!("directory")
    };
    assert_eq!(
        page.entries
            .iter()
            .map(|e| e.path.as_bytes())
            .collect::<Vec<_>>(),
        [b"cwd".as_slice(), b"outside.txt"]
    );
    let cancelled = CancellationToken::new();
    cancelled.cancel();
    assert!(matches!(
        files
            .read_path(request(&root, b"outside.txt", FileKind::File), cancelled)
            .await,
        Err(FilesError::Cancelled)
    ));
    files.close().await;
    assert!(matches!(
        files
            .read_path(
                request(&root, b"outside.txt", FileKind::File),
                CancellationToken::new()
            )
            .await,
        Err(FilesError::Cancelled)
    ));
}

#[cfg(unix)]
#[tokio::test]
async fn finite_read_follows_physical_parent_and_symlink_semantics_and_rejects_special_files() {
    use std::os::unix::{ffi::OsStrExt, fs::symlink};
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path().canonicalize().unwrap();
    std::fs::create_dir_all(root.join("real/child")).unwrap();
    std::fs::write(root.join("real/proof"), b"physical").unwrap();
    std::fs::write(root.join("proof"), b"lexical-wrong").unwrap();
    symlink(root.join("real/child"), root.join("alias")).unwrap();
    let files = LocalFiles::new().unwrap();
    let result = files
        .read_path(
            request(&root, b"alias/../proof", FileKind::File),
            CancellationToken::new(),
        )
        .await
        .unwrap();
    let ReadPage::File(page) = result.page else {
        panic!("file")
    };
    assert_eq!(hex::decode(page.bytes_hex).unwrap(), b"physical");
    let name = std::ffi::OsStr::from_bytes(b"\xff");
    std::fs::write(root.join(name), b"exact").unwrap();
    let result = files
        .read_path(
            request(&root, b"\xff", FileKind::File),
            CancellationToken::new(),
        )
        .await
        .unwrap();
    assert_eq!(
        result.path.as_bytes(),
        root.join(name).as_os_str().as_bytes()
    );
    rustix::fs::mknodat(
        rustix::fs::CWD,
        root.join("fifo"),
        rustix::fs::FileType::Fifo,
        rustix::fs::Mode::RUSR | rustix::fs::Mode::WUSR,
        0,
    )
    .unwrap();
    let result = tokio::time::timeout(
        std::time::Duration::from_secs(2),
        files.read_path(
            request(&root, b"fifo", FileKind::File),
            CancellationToken::new(),
        ),
    )
    .await
    .unwrap();
    assert!(matches!(result, Err(FilesError::Invalid)));
}
