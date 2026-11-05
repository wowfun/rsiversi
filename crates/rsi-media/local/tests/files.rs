use rsi_media_local::LocalAttachmentReader;
use rsi_media_protocol::{FileRead, FileRef, MediaError, MediaId};
use sha2::{Digest, Sha256};
use std::fs;

#[tokio::test]
async fn file_reader_plugin_creates_and_shares_one_publisher_namespace() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().join("attachments");
    let runtime = rsi_meta::Runtime::default();
    runtime
        .root()
        .apply(
            rsi_meta::ResolvedFactory::linked(
                "files",
                "test",
                rsi_meta::UpdateMode::RestartRequired,
                std::sync::Arc::new(rsi_media_local::LocalAttachmentReaderFactory),
            ),
            serde_json::json!({"root": root}),
        )
        .await
        .unwrap();
    let reader = runtime
        .root()
        .lookup_local::<rsi_media_protocol::FileReadContract>()
        .unwrap();
    assert!(root.is_dir());
    let reference = FileRef::new(
        MediaId::new(hex::encode(Sha256::digest(b"shared"))).unwrap(),
        "shared.txt",
        6,
    )
    .unwrap();
    let path = reader.local_path(&reference).unwrap();
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(&path, b"shared").unwrap();
    reader.verify(&reference).await.unwrap();
    assert_eq!(fs::read(&path).unwrap(), b"shared");
    drop(reader);
    assert!(runtime.shutdown().await.is_clean());
    assert_eq!(fs::read(&path).unwrap(), b"shared");
}

/// Opt-in interoperability fixture: the publisher runs separately and owns its objects.
#[tokio::test]
#[ignore = "requires RSI_MEDIA_FILE_FIXTURE from an external immutable file publisher"]
async fn external_file_publisher_references_and_paths_are_shared_exactly() {
    #[derive(serde::Deserialize)]
    struct Entry {
        reference: FileRef,
        path: std::path::PathBuf,
    }
    #[derive(serde::Deserialize)]
    struct Fixture {
        root: std::path::PathBuf,
        refs: Vec<Entry>,
    }
    let fixture: Fixture = serde_json::from_slice(
        &fs::read(std::env::var_os("RSI_MEDIA_FILE_FIXTURE").expect("explicit fixture path"))
            .unwrap(),
    )
    .unwrap();
    assert!(!fixture.refs.is_empty());
    let reader = LocalAttachmentReader::open(fixture.root).await.unwrap();
    for entry in fixture.refs {
        assert_eq!(reader.local_path(&entry.reference).unwrap(), entry.path);
        reader.verify(&entry.reference).await.unwrap();
    }
}

#[tokio::test]
async fn published_files_are_verified_without_copying_or_repairing_them() {
    let root = tempfile::tempdir().unwrap();
    let reader = LocalAttachmentReader::open(root.path().to_owned()).await.unwrap();
    for data in [vec![], vec![0x5a; 1024 * 1024 + 17]] {
        let reference = FileRef::new(
            MediaId::new(hex::encode(Sha256::digest(&data))).unwrap(),
            "原始文件.bin",
            data.len() as u64,
        )
        .unwrap();
        let path = reader.local_path(&reference).unwrap();
        assert!(matches!(
            reader.verify(&reference).await,
            Err(MediaError::NotFound(_))
        ));
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, &data).unwrap();
        reader.verify(&reference).await.unwrap();
        assert_eq!(fs::read(&path).unwrap(), data);
        fs::write(&path, b"changed").unwrap();
        assert!(matches!(
            reader.verify(&reference).await,
            Err(MediaError::Corrupt(_))
        ));
        assert_eq!(fs::read(&path).unwrap(), b"changed");
        if !data.is_empty() {
            fs::write(&path, vec![0x59; data.len()]).unwrap();
            assert!(matches!(
                reader.verify(&reference).await,
                Err(MediaError::Corrupt(_))
            ));
        }
    }
}

#[cfg(unix)]
#[tokio::test]
async fn immutable_file_reads_reject_linked_objects_and_shards() {
    use std::os::unix::fs::symlink;
    let root = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    let reader = LocalAttachmentReader::open(root.path().to_owned()).await.unwrap();
    let reference = FileRef::new(
        MediaId::new(hex::encode(Sha256::digest(b"data"))).unwrap(),
        "file",
        4,
    )
    .unwrap();
    let path = reader.local_path(&reference).unwrap();
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(outside.path().join("file"), b"data").unwrap();
    symlink(outside.path().join("file"), &path).unwrap();
    assert!(reader.verify(&reference).await.is_err());
    fs::remove_file(&path).unwrap();
    fs::remove_dir(path.parent().unwrap()).unwrap();
    symlink(outside.path(), path.parent().unwrap()).unwrap();
    assert!(reader.verify(&reference).await.is_err());
    assert_eq!(fs::read(outside.path().join("file")).unwrap(), b"data");
}
