use super::*;

#[tokio::test]
async fn managed_sdk_build_publishes_bytes_that_the_independent_loader_admits_exactly() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    let repository = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../..")
        .canonicalize()
        .unwrap();
    let path = manifest(
        &root.join("source"),
        "set -e\n\"$CARGO\" build --locked --offline --manifest-path \"$RSI_FIXTURE_MANIFEST\" --target-dir \"$CARGO_TARGET_DIR\"\ncp \"$CARGO_TARGET_DIR/debug/$RSI_LIBRARY\" artifact.bin\n",
    );
    let text = fs::read_to_string(&path)
        .unwrap()
        .replace("fixture.build-plugin", "fixture.native-addon")
        .replace("timeout_seconds = 1", "timeout_seconds = 120");
    fs::write(&path, text).unwrap();
    let build = Arc::new(NativeAddonBuild::open(&path).unwrap());
    let store = Arc::new(NativeAddonStore::open(root.join("store")).unwrap());
    let manager = manager(&root).await;
    let mut environment = vec![
        ("CARGO".into(), env!("CARGO").into()),
        ("CARGO_BUILD_JOBS".into(), "2".into()),
        (
            "RSI_FIXTURE_MANIFEST".into(),
            repository
                .join("fixtures/rsi/native-addon/Cargo.toml")
                .into_os_string(),
        ),
        (
            "CARGO_TARGET_DIR".into(),
            repository
                .join("target/native-addon-build-fixture-test")
                .into_os_string(),
        ),
        (
            "RSI_LIBRARY".into(),
            format!(
                "{}rsi_fixture_native_addon{}",
                std::env::consts::DLL_PREFIX,
                std::env::consts::DLL_SUFFIX
            )
            .into(),
        ),
    ];
    // Only toolchain discovery inputs are passed to the offline child; product
    // credentials and other ambient environment entries are not inherited.
    for name in [
        "PATH",
        "HOME",
        "CARGO_HOME",
        "RUSTUP_HOME",
        "RUSTUP_TOOLCHAIN",
    ] {
        if let Some(value) = std::env::var_os(name) {
            environment.push((name.into(), value));
        }
    }
    let report = manager
        .service()
        .run(build, store.clone(), environment, CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(
        report.status,
        NativeAddonBuildStatus::Succeeded,
        "{}",
        String::from_utf8_lossy(&report.stderr.bytes)
    );
    let record = report.installed.unwrap().record.unwrap();
    assert_eq!(record.plugin(), "fixture.native-addon");
    assert!(store.snapshot().unwrap().enabled.is_empty());
    assert!(!root.join("loader").exists());
    assert!(!root.join("config").exists());
    assert!(!root.join("state").exists());
    assert!(!root.join("cache").exists());
    assert!(manager.shutdown().await.is_clean());
    let loader = rsi_meta_native_loader::NativeCatalog::new(
        rsi_meta_native_loader::CatalogOptions::new(root.join("loader")),
    )
    .unwrap();
    let factory = loader
        .load_exact(
            root.join("store/objects").join(record.artifact_sha256()),
            record.artifact_sha256(),
        )
        .unwrap();
    assert_eq!(
        factory.identity(),
        &rsi_meta::FactoryIdentity::native(record.plugin(), record.artifact_sha256())
    );
    drop(factory);
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        while loader.snapshot().staging_bytes != 0 {
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(loader.snapshot().retained_failed_finalizations, 0);
    assert_eq!(loader.snapshot().cache_artifacts, 1);
}
