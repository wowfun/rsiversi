use rsi_api_http::{AssetType, HttpAssetsContract};
use rsi_host::{HostBuilder, Profile, ProfileEntry};
use rsi_meta::UpdateMode;
use std::sync::Arc;

fn host() -> rsi_host::Host {
    let mut builder = HostBuilder::without_paths("native");
    builder
        .register_local_contract::<HttpAssetsContract>()
        .unwrap();
    builder
        .register_linked(
            "assets",
            "1",
            UpdateMode::RestartRequired,
            Arc::new(rsi_web_assets::WebAssetsFactory),
        )
        .unwrap();
    builder.build().unwrap()
}
fn profile(root: &std::path::Path, files: &[&str]) -> Profile {
    Profile::new(vec![ProfileEntry::new(
        "assets",
        "assets",
        serde_json::json!({"directory":root,"files":files}),
    )])
}

#[tokio::test]
async fn complete_bundle_is_immutable_and_retirement_fences_escaped_capability() {
    let root = tempfile::tempdir().unwrap();
    std::fs::write(root.path().join("index.html"), b"<main>fixture</main>").unwrap();
    std::fs::write(root.path().join("app.wasm"), b"wasm bytes").unwrap();
    let running = host()
        .start(profile(root.path(), &["index.html", "app.wasm"]))
        .await
        .unwrap();
    let assets = running.lookup_local::<HttpAssetsContract>().unwrap();
    let page = assets.get("/").unwrap().unwrap();
    assert_eq!(page.kind, AssetType::Html);
    assert_eq!(
        assets.get("/index.html").unwrap().unwrap().bytes.as_bytes(),
        page.bytes.as_bytes()
    );
    assert!(assets.get("/../index.html").unwrap().is_none());
    std::fs::write(root.path().join("index.html"), b"changed").unwrap();
    assert_eq!(
        assets.get("/").unwrap().unwrap().bytes.as_bytes(),
        b"<main>fixture</main>"
    );
    assert!(running.shutdown().await.is_clean());
    assert!(matches!(
        assets.get("/"),
        Err(rsi_api_protocol::ApiError::ShuttingDown)
    ));
    assert_eq!(page.bytes.as_bytes(), b"<main>fixture</main>");
}

#[tokio::test]
async fn rejects_missing_nonregular_and_oversized_bundles_without_publication() {
    let root = tempfile::tempdir().unwrap();
    std::fs::write(root.path().join("index.html"), b"page").unwrap();
    assert!(
        host()
            .start(profile(root.path(), &["index.html", "missing.js"]))
            .await
            .is_err()
    );
    std::fs::create_dir(root.path().join("directory.js")).unwrap();
    assert!(
        host()
            .start(profile(root.path(), &["index.html", "directory.js"]))
            .await
            .is_err()
    );
    let file = std::fs::File::create(root.path().join("large.wasm")).unwrap();
    file.set_len(64 * 1024 * 1024).unwrap();
    assert!(
        host()
            .start(profile(root.path(), &["index.html", "large.wasm"]))
            .await
            .is_err()
    );
    for files in [
        vec!["index.html", "../escape.js"],
        vec!["index.html", "index.html"],
        vec!["index.html", ".hidden.js"],
        vec!["index.html", "secret.pem"],
    ] {
        assert!(host().start(profile(root.path(), &files)).await.is_err());
    }
}

#[cfg(unix)]
#[tokio::test]
async fn rejects_symlink_directory_symlink_file_and_fifo_without_opening_their_contents() {
    use std::os::unix::fs::symlink;
    let root = tempfile::tempdir().unwrap();
    let bundle = root.path().join("bundle");
    std::fs::create_dir(&bundle).unwrap();
    std::fs::write(bundle.join("index.html"), b"page").unwrap();
    symlink(&bundle, root.path().join("alias")).unwrap();
    assert!(
        host()
            .start(profile(&root.path().join("alias"), &["index.html"]))
            .await
            .is_err()
    );
    symlink(bundle.join("index.html"), bundle.join("alias.html")).unwrap();
    assert!(
        host()
            .start(profile(&bundle, &["index.html", "alias.html"]))
            .await
            .is_err()
    );
    rustix::fs::mknodat(
        rustix::fs::CWD,
        bundle.join("pipe.js"),
        rustix::fs::FileType::Fifo,
        rustix::fs::Mode::RUSR | rustix::fs::Mode::WUSR,
        0,
    )
    .unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        assert!(
            host()
                .start(profile(&bundle, &["index.html", "pipe.js"]))
                .await
                .is_err()
        );
    })
    .await
    .unwrap();
}
