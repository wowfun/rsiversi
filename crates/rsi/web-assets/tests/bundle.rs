use rsi_api_http::{AssetType, HttpAssetsContract};
use rsi_host::{HostBuilder, Profile, ProfileEntry};
use rsi_meta::UpdateMode;
use rsi_web_assets::{AssetError, WebAssetControlContract};
use sha2::{Digest as _, Sha256};
use std::sync::Arc;

fn host() -> rsi_host::Host {
    let mut builder = HostBuilder::without_paths("native");
    builder
        .register_local_contract::<HttpAssetsContract>()
        .unwrap();
    builder
        .register_local_contract::<rsi_web_assets::WebAssetControlContract>()
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
async fn rejected_current_can_be_replaced_without_losing_last_good_renderer() {
    let root = tempfile::tempdir().unwrap();
    let files = renderer_bundle(&root.path().join("a"), b'a', 16);
    let running = host()
        .start(profile(
            &root.path().join("a"),
            &files.iter().map(String::as_str).collect::<Vec<_>>(),
        ))
        .await
        .unwrap();
    let assets = running.lookup_local::<HttpAssetsContract>().unwrap();
    let control = running.lookup_local::<WebAssetControlContract>().unwrap();
    let a = control.revision().unwrap();
    let displayed = control.acquire(&a).unwrap();
    let old_url = displayed.url("lazy.js").unwrap();
    let candidate = control
        .stage(
            root.path().join("b"),
            renderer_bundle(&root.path().join("b"), b'b', 16),
        )
        .unwrap()
        .wait()
        .await
        .unwrap();
    let b = candidate.publish(&a).unwrap();
    let offered = control.acquire(&b).unwrap();
    let rejected_url = offered.url("lazy.js").unwrap();
    let escaped = assets.get(&rejected_url).unwrap().unwrap().bytes;
    drop(offered); // The document rejected B and kept displaying A.
    for label in *b"cd" {
        let directory = root.path().join(char::from(label).to_string());
        let candidate = control
            .stage(directory.clone(), renderer_bundle(&directory, label, 16))
            .unwrap()
            .wait()
            .await
            .unwrap();
        let next = candidate
            .publish(&control.revision().unwrap())
            .expect("an unleased failed current must not prevent recovery");
        assert_ne!(next, b);
        assert_eq!(
            assets.get(&old_url).unwrap().unwrap().bytes.as_bytes(),
            b"aaa"
        );
        assert!(assets.get(&rejected_url).unwrap().is_none());
    }
    assert_eq!(
        escaped.as_bytes(),
        b"bbb",
        "escaped bytes retain their budget without authorizing module fetches"
    );
    drop((displayed, escaped));
    assert!(running.shutdown().await.is_clean());
    assert_eq!(control.retained_bytes(), 0);
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
    assert!(
        std::process::Command::new("/usr/bin/mkfifo")
            .env_clear()
            .args(["-m", "600"])
            .arg(bundle.join("pipe.js"))
            .status()
            .unwrap()
            .success()
    );
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

fn renderer_bundle(root: &std::path::Path, version: u8, bytes: usize) -> Vec<String> {
    std::fs::create_dir_all(root).unwrap();
    std::fs::write(root.join("index.html"), b"page").unwrap();
    std::fs::write(root.join("worker.js"), b"fixed worker").unwrap();
    let body = vec![version; bytes];
    std::fs::write(root.join("renderer.js"), &body).unwrap();
    let lazy = [version; 3];
    std::fs::write(root.join("lazy.js"), lazy).unwrap();
    std::fs::write(root.join("ui-renderers.json"), serde_json::to_vec(&serde_json::json!({
        "format":1,"renderers":[{"id":"fixture.renderer","abi":1,"entry":"renderer.js",
            "files":[{"name":"renderer.js","sha256":hex::encode(Sha256::digest(&body))},{"name":"lazy.js","sha256":hex::encode(Sha256::digest(lazy))}],
            "schemas":[{"name":"fixture.model","version":1}],"capabilities":["invoke"],"surfaces":["pane"]}]
    })).unwrap()).unwrap();
    [
        "index.html",
        "worker.js",
        "renderer.js",
        "lazy.js",
        "ui-renderers.json",
    ]
    .map(Into::into)
    .to_vec()
}

#[tokio::test]
async fn renderer_cas_keeps_the_same_http_capability_and_exact_old_lazy_import_graph() {
    let root = tempfile::tempdir().unwrap();
    let files = renderer_bundle(&root.path().join("a"), b'a', 16);
    let running = host()
        .start(profile(
            &root.path().join("a"),
            &files.iter().map(String::as_str).collect::<Vec<_>>(),
        ))
        .await
        .unwrap();
    let assets = running.lookup_local::<HttpAssetsContract>().unwrap();
    let control = running.lookup_local::<WebAssetControlContract>().unwrap();
    let a = control.revision().unwrap();
    assert!(
        assets
            .get(&format!("/rsi-renderers/{a}/ui-renderers.json"))
            .unwrap()
            .is_none()
    );
    assert!(assets.get("/renderer.js").unwrap().is_none());
    let old = control.acquire(&a).unwrap();
    let old_lazy_url = old.url("lazy.js").unwrap();
    let escaped = assets.get(&old_lazy_url).unwrap().unwrap().bytes;
    let b_files = renderer_bundle(&root.path().join("b"), b'b', 16);
    let candidate = control
        .stage(root.path().join("b"), b_files)
        .unwrap()
        .wait()
        .await
        .unwrap();
    assert!(matches!(
        candidate.publish(&"0".repeat(64)),
        Err(AssetError::Conflict)
    ));
    let b = candidate.publish(&a).unwrap();
    assert_ne!(a, b);
    assert!(Arc::ptr_eq(
        &assets,
        &running.lookup_local::<HttpAssetsContract>().unwrap()
    ));
    assert_eq!(
        assets.get(&old_lazy_url).unwrap().unwrap().bytes.as_bytes(),
        b"aaa"
    );
    let current = control.acquire(&b).unwrap();
    assert_eq!(
        assets
            .get(&current.url("lazy.js").unwrap())
            .unwrap()
            .unwrap()
            .bytes
            .as_bytes(),
        b"bbb"
    );
    let c_files = renderer_bundle(&root.path().join("c"), b'c', 16);
    let next = control
        .stage(root.path().join("c"), c_files)
        .unwrap()
        .wait()
        .await
        .unwrap();
    assert!(matches!(next.publish(&b), Err(AssetError::Capacity)));
    assert_eq!(control.revision().unwrap(), b);
    drop(old);
    assert!(
        assets.get(&old_lazy_url).unwrap().is_none(),
        "missing old revision never falls back"
    );
    let c = next.publish(&b).unwrap();
    assert_ne!(b, c);
    drop(current);
    assert!(running.shutdown().await.is_clean());
    assert!(matches!(
        assets.get("/"),
        Err(rsi_api_protocol::ApiError::ShuttingDown)
    ));
    assert!(matches!(control.acquire(&c), Err(AssetError::Closed)));
    assert_eq!(
        control.retained_bytes(),
        3,
        "escaped bytes retain only their original charge"
    );
    assert_eq!(escaped.as_bytes(), b"aaa");
    drop(escaped);
    assert_eq!(control.retained_bytes(), 0);
}

#[tokio::test]
async fn hot_staging_and_escaped_responses_share_one_pool_and_bootstrap_changes_require_restart() {
    let root = tempfile::tempdir().unwrap();
    let files = renderer_bundle(&root.path().join("a"), b'a', 24 * 1024 * 1024);
    let running = host()
        .start(profile(
            &root.path().join("a"),
            &files.iter().map(String::as_str).collect::<Vec<_>>(),
        ))
        .await
        .unwrap();
    let control = running.lookup_local::<WebAssetControlContract>().unwrap();
    let assets = running.lookup_local::<HttpAssetsContract>().unwrap();
    let a = control.revision().unwrap();
    let lease = control.acquire(&a).unwrap();
    let escaped = assets
        .get(&lease.url("renderer.js").unwrap())
        .unwrap()
        .unwrap()
        .bytes;
    let files = renderer_bundle(&root.path().join("b"), b'b', 24 * 1024 * 1024);
    let candidate = control
        .stage(root.path().join("b"), files)
        .unwrap()
        .wait()
        .await
        .unwrap();
    let b = candidate.publish(&a).unwrap();
    drop(lease);
    assert!(control.retained_bytes() > 48 * 1024 * 1024);
    let files = renderer_bundle(&root.path().join("c"), b'c', 24 * 1024 * 1024);
    assert!(matches!(
        control
            .stage(root.path().join("c"), files.clone())
            .unwrap()
            .wait()
            .await,
        Err(AssetError::Capacity)
    ));
    assert_eq!(control.revision().unwrap(), b);
    drop(escaped);
    std::fs::write(root.path().join("c/worker.js"), b"changed worker").unwrap();
    let candidate = control
        .stage(root.path().join("c"), files)
        .unwrap()
        .wait()
        .await
        .unwrap();
    assert!(matches!(
        candidate.publish(&b),
        Err(AssetError::RestartRequired)
    ));
    assert_eq!(control.revision().unwrap(), b);
    assert!(running.shutdown().await.is_clean());
    assert!(matches!(candidate.publish(&b), Err(AssetError::Closed)));
    assert_eq!(
        control.retained_bytes(),
        0,
        "escaped candidate handles do not preserve retired storage"
    );
}

#[tokio::test]
async fn bad_manifest_digest_and_dropped_stage_waiters_never_publish_or_orphan_candidates() {
    let root = tempfile::tempdir().unwrap();
    let files = renderer_bundle(&root.path().join("a"), b'a', 16);
    let running = host()
        .start(profile(
            &root.path().join("a"),
            &files.iter().map(String::as_str).collect::<Vec<_>>(),
        ))
        .await
        .unwrap();
    let control = running.lookup_local::<WebAssetControlContract>().unwrap();
    let a = control.revision().unwrap();
    let files = renderer_bundle(&root.path().join("b"), b'b', 16);
    std::fs::write(root.path().join("b/lazy.js"), b"wrong").unwrap();
    assert!(matches!(
        control
            .stage(root.path().join("b"), files.clone())
            .unwrap()
            .wait()
            .await,
        Err(AssetError::Invalid(_))
    ));
    assert_eq!(control.revision().unwrap(), a);
    renderer_bundle(&root.path().join("b"), b'b', 16);
    drop(control.stage(root.path().join("b"), files.clone()).unwrap());
    let candidate = tokio::time::timeout(std::time::Duration::from_secs(3), async {
        loop {
            match control.stage(root.path().join("b"), files.clone()) {
                Ok(ticket) => break ticket.wait().await.unwrap(),
                Err(AssetError::Capacity) => tokio::task::yield_now().await,
                Err(error) => panic!("{error}"),
            }
        }
    })
    .await
    .unwrap();
    assert_ne!(candidate.publish(&a).unwrap(), a);
    assert!(running.shutdown().await.is_clean());
}

#[tokio::test]
async fn the_complete_cold_bundle_may_use_the_full_64_mib_budget() {
    let root = tempfile::tempdir().unwrap();
    std::fs::write(root.path().join("index.html"), b"page").unwrap();
    let file = std::fs::File::create(root.path().join("app.wasm")).unwrap();
    file.set_len((rsi_api_protocol::MAXIMUM_API_BYTES - 4) as u64)
        .unwrap();
    let running = host()
        .start(profile(root.path(), &["index.html", "app.wasm"]))
        .await
        .unwrap();
    let control = running.lookup_local::<WebAssetControlContract>().unwrap();
    assert_eq!(
        control.retained_bytes(),
        rsi_api_protocol::MAXIMUM_API_BYTES
    );
    let revision = control.revision().unwrap();
    let identical = control
        .stage(
            root.path().to_owned(),
            vec!["index.html".into(), "app.wasm".into()],
        )
        .unwrap()
        .wait()
        .await
        .unwrap();
    assert_eq!(identical.publish(&revision).unwrap(), revision);
    assert_eq!(
        control.retained_bytes(),
        rsi_api_protocol::MAXIMUM_API_BYTES
    );
    // A changed allocation still cannot coexist with the complete cold bundle.
    std::io::Write::write_all(
        &mut std::fs::OpenOptions::new()
            .write(true)
            .open(root.path().join("app.wasm"))
            .unwrap(),
        b"changed",
    )
    .unwrap();
    assert!(matches!(
        control
            .stage(
                root.path().to_owned(),
                vec!["index.html".into(), "app.wasm".into()]
            )
            .unwrap()
            .wait()
            .await,
        Err(AssetError::Capacity)
    ));
    assert!(running.shutdown().await.is_clean());
    assert_eq!(control.retained_bytes(), 0);
}

#[tokio::test]
async fn small_renderer_change_shares_a_large_unchanged_worker_allocation() {
    let root = tempfile::tempdir().unwrap();
    let a = root.path().join("a");
    let b = root.path().join("b");
    let mut files = renderer_bundle(&a, b'a', 16);
    renderer_bundle(&b, b'b', 16);
    let wasm = vec![42; 36 * 1024 * 1024];
    for directory in [&a, &b] {
        std::fs::write(directory.join("rsi_web_bg.wasm"), &wasm).unwrap();
    }
    files.push("rsi_web_bg.wasm".into());
    let running = host()
        .start(profile(
            &a,
            &files.iter().map(String::as_str).collect::<Vec<_>>(),
        ))
        .await
        .unwrap();
    let assets = running.lookup_local::<HttpAssetsContract>().unwrap();
    let control = running.lookup_local::<WebAssetControlContract>().unwrap();
    let original = control.revision().unwrap();
    let lease = control.acquire(&original).unwrap();
    let before = assets.get("/rsi_web_bg.wasm").unwrap().unwrap();
    let next = control
        .stage(b, files)
        .unwrap()
        .wait()
        .await
        .unwrap()
        .publish(&original)
        .unwrap();
    let after = assets.get("/rsi_web_bg.wasm").unwrap().unwrap();
    assert_eq!(
        before.bytes.as_bytes().as_ptr(),
        after.bytes.as_bytes().as_ptr()
    );
    assert!(control.retained_bytes() < 37 * 1024 * 1024);
    assert!(
        assets
            .get(&format!("/rsi-renderers/{original}/lazy.js"))
            .unwrap()
            .is_some()
    );
    assert_ne!(original, next);
    drop((lease, before, after));
    assert!(running.shutdown().await.is_clean());
    assert_eq!(control.retained_bytes(), 0);
}

#[tokio::test]
async fn explicit_watch_keeps_bad_candidates_then_publishes_a_complete_graph_and_joins() {
    let root = tempfile::tempdir().unwrap();
    let files = renderer_bundle(root.path(), b'a', 16);
    let running = host()
        .start(Profile::new(vec![ProfileEntry::new(
            "assets",
            "assets",
            serde_json::json!({"directory":root.path(),"files":files,"watch":true}),
        )]))
        .await
        .unwrap();
    let control = running.lookup_local::<WebAssetControlContract>().unwrap();
    let original = control.revision().unwrap();
    std::fs::write(root.path().join("renderer.js"), b"invalid incomplete write").unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(3), async {
        while control.diagnostic().is_none() {
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(control.revision().unwrap(), original);
    assert!(control.diagnostic().unwrap().contains("digest mismatch"));
    let mut changed = control.changes();
    renderer_bundle(root.path(), b'b', 16);
    tokio::time::timeout(std::time::Duration::from_secs(3), changed.changed())
        .await
        .unwrap()
        .unwrap();
    assert_ne!(control.revision().unwrap(), original);
    tokio::time::timeout(std::time::Duration::from_secs(3), async {
        while control.diagnostic().is_some() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert!(running.shutdown().await.is_clean());
    assert_eq!(control.retained_bytes(), 0);
}
