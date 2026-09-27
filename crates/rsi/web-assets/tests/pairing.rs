use rsi_api_http::HttpAssetsContract;
use rsi_application::ApplicationDiagnostic as _;
use rsi_host::{HostBuilder, Profile, ProfileEntry};
use rsi_meta::{PluginFactory as _, UpdateMode};
use rsi_web_assets::{
    BOOTSTRAP_FILES, PairedWebAssetsFactory, WebAssetControlContract, pairing_receipt,
};
use std::{path::Path, sync::Arc};

const FAMILY: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

fn bundle() -> tempfile::TempDir {
    let root = tempfile::tempdir().unwrap();
    for file in BOOTSTRAP_FILES {
        std::fs::write(root.path().join(file), file.as_bytes()).unwrap();
    }
    std::fs::write(
        root.path().join("rsi-build.json"),
        pairing_receipt(root.path(), FAMILY).unwrap(),
    )
    .unwrap();
    root
}
async fn start(
    root: &Path,
) -> (
    rsi_host::Result<rsi_host::RunningHost>,
    Arc<PairedWebAssetsFactory>,
) {
    let factory = Arc::new(PairedWebAssetsFactory::new(Some(FAMILY)));
    let mut host = HostBuilder::without_paths("pairing-test");
    host.register_local_contract::<HttpAssetsContract>()
        .unwrap();
    host.register_local_contract::<WebAssetControlContract>()
        .unwrap();
    host.register_linked("assets", "1", UpdateMode::RestartRequired, factory.clone())
        .unwrap();
    let result = host
        .build()
        .unwrap()
        .start(Profile::new(vec![ProfileEntry::new(
            "assets",
            "assets",
            serde_json::json!({"directory":root,"files":files(root)}),
        )]))
        .await;
    (result, factory)
}

#[test]
fn standalone_product_entry_rejects_before_asset_io() {
    let factory = PairedWebAssetsFactory::new(None);
    assert!(
        factory
            .prepare(&serde_json::json!({"directory":"/nonexistent"}))
            .is_err()
    );
    let diagnostic = factory.take_diagnostic().unwrap().to_string();
    assert!(
        diagnostic.contains("no build family") && diagnostic.contains("pnpm -C apps/web build")
    );
    assert!(factory.take_diagnostic().is_none());
}

#[tokio::test]
async fn paired_admission_retains_exact_checked_bytes_and_retires_cleanly() {
    let root = bundle();
    let (result, factory) = start(root.path()).await;
    let running = result.unwrap();
    assert!(factory.take_diagnostic().is_none());
    let assets = running.lookup_local::<HttpAssetsContract>().unwrap();
    std::fs::write(root.path().join("app.js"), "changed after admission").unwrap();
    assert_eq!(
        assets.get("/app.js").unwrap().unwrap().bytes.as_bytes(),
        b"app.js"
    );
    assert!(running.shutdown().await.is_clean());
}

#[tokio::test]
async fn missing_malformed_oversized_and_foreign_receipts_are_rejected() {
    for variant in [
        "missing",
        "malformed",
        "oversized",
        "foreign",
        "reclassified",
    ] {
        let root = bundle();
        let path = root.path().join("rsi-build.json");
        match variant {
            "missing" => std::fs::remove_file(&path).unwrap(),
            "malformed" => std::fs::write(&path, "{").unwrap(),
            "oversized" => std::fs::write(&path, vec![b' '; 32769]).unwrap(),
            _ => {
                let mut value: serde_json::Value =
                    serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
                if variant == "foreign" {
                    value["family_sha256"] =
                        "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb".into();
                } else {
                    value["bootstrap"]
                        .as_object_mut()
                        .unwrap()
                        .remove("worker.js");
                }
                std::fs::write(&path, serde_json::to_vec(&value).unwrap()).unwrap();
            }
        }
        let (result, diagnostic) = start(root.path()).await;
        assert!(result.is_err(), "accepted {variant}");
        assert!(
            diagnostic
                .take_diagnostic()
                .unwrap()
                .to_string()
                .contains("Web pairing")
        );
    }
}

#[tokio::test]
async fn each_bootstrap_file_is_pinned_including_wasm() {
    for file in BOOTSTRAP_FILES {
        let root = bundle();
        std::fs::write(root.path().join(file), "foreign artifact").unwrap();
        let (result, diagnostic) = start(root.path()).await;
        assert!(result.is_err(), "accepted modified {file}");
        assert!(
            diagnostic
                .take_diagnostic()
                .unwrap()
                .to_string()
                .contains(file)
        );
    }
}

fn files(root: &Path) -> Vec<String> {
    let mut files: Vec<String> = BOOTSTRAP_FILES.iter().map(|name| (*name).into()).collect();
    if root.join("ui-renderers.json").exists() {
        files.extend(["standard.js".into(), "ui-renderers.json".into()]);
    }
    files
}
fn renderer(root: &Path, source: &[u8]) {
    use sha2::{Digest as _, Sha256};
    std::fs::write(root.join("standard.js"), source).unwrap();
    std::fs::write(root.join("ui-renderers.json"), serde_json::to_vec(&serde_json::json!({
        "format":1,"renderers":[{"id":"fixture.renderer","abi":1,"entry":"standard.js",
        "files":[{"name":"standard.js","sha256":format!("{:x}", Sha256::digest(source))}],
        "schemas":[{"name":"fixture.model","version":1}],"capabilities":["invoke"],"surfaces":["pane"]}]
    })).unwrap()).unwrap();
}

#[tokio::test]
async fn paired_renderer_updates_retire_and_restart_without_changing_receipt() {
    let root = bundle();
    renderer(root.path(), b"initial renderer");
    let receipt = std::fs::read(root.path().join("rsi-build.json")).unwrap();
    let (started, _) = start(root.path()).await;
    let running = started.unwrap();
    let assets = running.lookup_local::<HttpAssetsContract>().unwrap();
    let control = running.lookup_local::<WebAssetControlContract>().unwrap();
    let old_revision = control.revision().unwrap();
    let old = control.acquire(&old_revision).unwrap();
    let old_url = old.url("standard.js").unwrap();
    renderer(root.path(), b"updated renderer");
    let candidate = control
        .stage(root.path().to_owned(), files(root.path()))
        .unwrap()
        .wait()
        .await
        .unwrap();
    let updated = candidate.publish(&old_revision).unwrap();
    assert_ne!(updated, old_revision);
    assert_eq!(
        assets.get(&old_url).unwrap().unwrap().bytes.as_bytes(),
        b"initial renderer"
    );
    drop(old);
    assert!(assets.get(&old_url).unwrap().is_none());
    assert!(running.shutdown().await.is_clean());
    assert_eq!(
        std::fs::read(root.path().join("rsi-build.json")).unwrap(),
        receipt
    );
    let (restarted, _) = start(root.path()).await;
    let restarted = restarted.unwrap();
    assert_eq!(
        restarted
            .lookup_local::<WebAssetControlContract>()
            .unwrap()
            .revision()
            .unwrap(),
        updated
    );
    assert!(restarted.shutdown().await.is_clean());
    std::fs::write(root.path().join("worker.js"), "changed bootstrap").unwrap();
    let (rejected, diagnostic) = start(root.path()).await;
    assert!(rejected.is_err());
    assert!(
        diagnostic
            .take_diagnostic()
            .unwrap()
            .to_string()
            .contains("worker.js")
    );
}
