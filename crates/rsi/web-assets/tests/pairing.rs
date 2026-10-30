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
    std::fs::write(root.path().join("file-preview.js"), b"fixture preview").unwrap();
    std::fs::write(
        root.path().join("rsi-build.json"),
        pairing_receipt(
            root.path(),
            FAMILY,
            rsi_api_protocol::ProtocolDescriptor {
                name: "rsi.desktop.bridge".into(),
                version: 5,
            },
        )
        .unwrap(),
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
        diagnostic.contains("no build family") && diagnostic.contains("rsi plugin install web")
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
    let control = running.lookup_local::<WebAssetControlContract>().unwrap();
    assert_eq!(control.desktop_bridge().unwrap().unwrap().version, 5);
    // A new package selected on disk does not rewrite the held generation.
    let next = pairing_receipt(
        root.path(),
        FAMILY,
        rsi_api_protocol::ProtocolDescriptor {
            name: "rsi.desktop.bridge".into(),
            version: 6,
        },
    )
    .unwrap();
    std::fs::write(root.path().join("rsi-build.json"), next).unwrap();
    assert_eq!(control.desktop_bridge().unwrap().unwrap().version, 5);
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
                .contains("Web bundle")
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
    files.push("file-preview.js".into());
    if root.join("ui-renderers.json").exists() {
        files.extend(["standard.js".into(), "ui-renderers.json".into()]);
    }
    files
}

#[tokio::test]
async fn changed_bridge_requires_restart_without_replacing_the_held_generation() {
    let root = bundle();
    renderer(root.path(), b"initial renderer");
    let (started, _) = start(root.path()).await;
    let running = started.unwrap();
    let control = running.lookup_local::<WebAssetControlContract>().unwrap();
    let revision = control.revision().unwrap();
    let receipt = pairing_receipt(
        root.path(),
        FAMILY,
        rsi_api_protocol::ProtocolDescriptor {
            name: "rsi.desktop.bridge".into(),
            version: 6,
        },
    )
    .unwrap();
    std::fs::write(root.path().join("rsi-build.json"), receipt).unwrap();
    let candidate = control
        .stage_verified(root.path().to_owned())
        .unwrap()
        .wait()
        .await
        .unwrap();
    assert!(matches!(
        candidate.publish(&revision),
        Err(rsi_web_assets::AssetError::RestartRequired)
    ));
    assert_eq!(control.revision().unwrap(), revision);
    assert_eq!(control.desktop_bridge().unwrap().unwrap().version, 5);
    drop(candidate);
    assert!(running.shutdown().await.is_clean());
}

#[tokio::test]
async fn independent_bundles_keep_internal_integrity_without_core_family_equality() {
    for corrupt in [false, true] {
        let root = bundle();
        // A producer's family is independent of the core that admits the package.
        let receipt = pairing_receipt(
            root.path(),
            &"b".repeat(64),
            rsi_api_protocol::ProtocolDescriptor {
                name: "rsi.desktop.bridge".into(),
                version: 5,
            },
        )
        .unwrap();
        std::fs::write(root.path().join("rsi-build.json"), receipt).unwrap();
        if corrupt {
            std::fs::write(root.path().join("worker.js"), b"foreign worker").unwrap();
        }
        let mut builder = HostBuilder::without_paths("independent-web");
        builder
            .register_local_contract::<HttpAssetsContract>()
            .unwrap();
        builder
            .register_local_contract::<WebAssetControlContract>()
            .unwrap();
        builder
            .register_linked(
                "assets",
                "1",
                UpdateMode::RestartRequired,
                Arc::new(rsi_web_assets::VerifiedWebAssetsFactory),
            )
            .unwrap();
        let result = builder
            .build()
            .unwrap()
            .start(Profile::new(vec![ProfileEntry::new(
                "assets",
                "assets",
                serde_json::json!({"directory": root.path(), "files": files(root.path())}),
            )]))
            .await;
        if corrupt {
            assert!(result.is_err());
        } else {
            assert!(result.unwrap().shutdown().await.is_clean());
        }
    }
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
async fn default_verified_admission_and_staging_load_declared_renderer_extensions() {
    use sha2::{Digest as _, Sha256};
    for explicit in [false, true] {
        let root = bundle();
        renderer(root.path(), b"initial renderer");
        let append =
            |name: &str, bytes: &[u8]| {
                let path = root.path().join("ui-renderers.json");
                let mut catalog: serde_json::Value =
                    serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
                catalog["renderers"][0]["files"].as_array_mut().unwrap().push(
                serde_json::json!({"name": name, "sha256": hex::encode(Sha256::digest(bytes))}),
            );
                std::fs::write(root.path().join(name), bytes).unwrap();
                std::fs::write(path, serde_json::to_vec(&catalog).unwrap()).unwrap();
            };
        append("extension.wasm", b"renderer module");
        let mut builder = HostBuilder::without_paths("renderer-extension");
        builder
            .register_local_contract::<HttpAssetsContract>()
            .unwrap();
        builder
            .register_local_contract::<WebAssetControlContract>()
            .unwrap();
        builder
            .register_linked(
                "assets",
                "1",
                UpdateMode::RestartRequired,
                Arc::new(rsi_web_assets::VerifiedWebAssetsFactory),
            )
            .unwrap();
        let mut config = serde_json::json!({"directory": root.path()});
        if explicit {
            config["files"] = serde_json::json!(files(root.path()));
        }
        let result = builder
            .build()
            .unwrap()
            .start(Profile::new(vec![ProfileEntry::new(
                "assets", "assets", config,
            )]))
            .await;
        if explicit {
            assert!(result.is_err(), "explicit file admission remains closed");
            continue;
        }
        let running = result.unwrap();
        let assets = running.lookup_local::<HttpAssetsContract>().unwrap();
        let control = running.lookup_local::<WebAssetControlContract>().unwrap();
        let revision = control.revision().unwrap();
        let lease = control.acquire(&revision).unwrap();
        assert!(assets.get("/extension.wasm").unwrap().is_none());
        assert_eq!(
            assets
                .get(&lease.url("extension.wasm").unwrap())
                .unwrap()
                .unwrap()
                .bytes
                .as_bytes(),
            b"renderer module"
        );
        append("lazy.js", b"lazy renderer module");
        let next = control
            .stage_verified(root.path().to_owned())
            .unwrap()
            .wait()
            .await
            .unwrap()
            .publish(&revision)
            .unwrap();
        let next_lease = control.acquire(&next).unwrap();
        assert_eq!(
            assets
                .get(&next_lease.url("lazy.js").unwrap())
                .unwrap()
                .unwrap()
                .bytes
                .as_bytes(),
            b"lazy renderer module"
        );
        std::fs::write(root.path().join("extension.wasm"), b"tampered").unwrap();
        assert!(
            control
                .stage_verified(root.path().to_owned())
                .unwrap()
                .wait()
                .await
                .is_err()
        );
        assert_eq!(control.revision().unwrap(), next);
        assert!(running.shutdown().await.is_clean());
    }
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
        .stage_verified(root.path().to_owned())
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

#[tokio::test]
async fn verified_watch_publishes_renderer_edits_and_keeps_bootstrap_fence() {
    let root = bundle();
    renderer(root.path(), b"initial renderer");
    let mut host = HostBuilder::without_paths("verified-watch");
    host.register_local_contract::<HttpAssetsContract>()
        .unwrap();
    host.register_local_contract::<WebAssetControlContract>()
        .unwrap();
    host.register_linked(
        "assets",
        "1",
        UpdateMode::RestartRequired,
        Arc::new(rsi_web_assets::VerifiedWebAssetsFactory),
    )
    .unwrap();
    let running = host
        .build()
        .unwrap()
        .start(Profile::new(vec![ProfileEntry::new(
            "assets",
            "assets",
            serde_json::json!({"directory":root.path(),"files":files(root.path()),"watch":true}),
        )]))
        .await
        .unwrap();
    let control = running.lookup_local::<WebAssetControlContract>().unwrap();
    let before = control.revision().unwrap();
    renderer(root.path(), b"updated renderer with different length");
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        while control.revision().unwrap() == before {
            tokio::time::sleep(std::time::Duration::from_millis(25)).await;
        }
    })
    .await
    .unwrap();
    assert!(control.diagnostic().is_none());
    assert_eq!(control.desktop_bridge().unwrap().unwrap().version, 5);
    let updated = control.revision().unwrap();
    std::fs::write(root.path().join("worker.js"), b"new bootstrap").unwrap();
    std::fs::write(
        root.path().join("rsi-build.json"),
        pairing_receipt(
            root.path(),
            FAMILY,
            rsi_api_protocol::ProtocolDescriptor {
                name: "rsi.desktop.bridge".into(),
                version: 5,
            },
        )
        .unwrap(),
    )
    .unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        while control.diagnostic().is_none() {
            tokio::time::sleep(std::time::Duration::from_millis(25)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(control.revision().unwrap(), updated);
    assert!(control.diagnostic().unwrap().contains("restart"));
    assert!(running.shutdown().await.is_clean());
}
