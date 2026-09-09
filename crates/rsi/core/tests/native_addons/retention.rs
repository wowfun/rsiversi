use super::*;
use std::time::{Duration, Instant};

const CHILD_ROOT: &str = "RSI_NATIVE_RETENTION_CHILD_ROOT";

fn compile(root: &Path) -> std::path::PathBuf {
    let repository = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../..");
    let library = root.join(format!("retained{}", std::env::consts::DLL_SUFFIX));
    let compiler = std::env::var_os("CC").unwrap_or_else(|| "cc".into());
    assert!(
        std::process::Command::new(compiler)
            .args([
                "-std=c11",
                "-Wall",
                "-Wextra",
                "-Werror",
                "-pedantic",
                "-fPIC"
            ])
            .arg(if cfg!(target_os = "macos") {
                "-dynamiclib"
            } else {
                "-shared"
            })
            .arg(repository.join("fixtures/rsi/native-addon/retained-finalizer.c"))
            .arg("-I")
            .arg(repository.join("crates/rsi-meta/native/include"))
            .arg("-o")
            .arg(&library)
            .status()
            .unwrap()
            .success()
    );
    library
}

#[test]
fn retained_native_failure_closes_manager_and_keeps_catalog_locked() {
    if let Some(root) = std::env::var_os(CHILD_ROOT) {
        return child(Path::new(&root));
    }
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    let artifact = compile(&root);
    report("retained.native", &fs::read(&artifact).unwrap());
    fs::rename(artifact, root.join("retained.bin")).unwrap();
    let mut child = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "retention::retained_native_failure_closes_manager_and_keeps_catalog_locked",
            "--nocapture",
        ])
        .env(CHILD_ROOT, &root)
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        if let Some(status) = child.try_wait().unwrap() {
            assert!(status.success());
            break;
        }
        if Instant::now() >= deadline {
            child.kill().unwrap();
            child.wait().unwrap();
            panic!("retained-native fixture child exceeded its deadline");
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    let receipt: serde_json::Value =
        serde_json::from_slice(&fs::read(root.join("receipt.json")).unwrap()).unwrap();
    assert_eq!(receipt["health"], "retained");
    assert_eq!(receipt["retained_failed_finalizations"], 1);
    assert!(receipt["staging_bytes"].as_u64().unwrap() > 0);
    report(
        "retention.json",
        &serde_json::to_vec_pretty(&receipt).unwrap(),
    );
    // The parent's TempDir is released only after the child's mapping and lock
    // authority have ended with process exit, including on assertion failure.
}

fn child(root: &Path) {
    let manifest = source(&root.join("source"), "fixture.retained-finalizer");
    fs::copy(root.join("retained.bin"), root.join("source/artifact.bin")).unwrap();
    let store = Arc::new(NativeAddonStore::open(root.join("store")).unwrap());
    let record = store.install(&manifest).unwrap().record.unwrap();
    store.enable("fixture.addon").unwrap();
    let cache = root.join("loader");
    let loader = NativeCatalog::new(CatalogOptions::new(&cache)).unwrap();
    let manager = composition(root)
        .native_addon_manager(store.clone(), loader.clone())
        .unwrap();
    manager.refresh().unwrap();
    assert_eq!(manager.inspect().health, NativeAddonHealth::Ready);
    store.disable("fixture.addon").unwrap();
    manager.refresh().unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    while loader.snapshot().retained_failed_finalizations == 0 {
        assert!(
            Instant::now() < deadline,
            "native FINALIZE failure was not retained"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
    let retained = loader.snapshot();
    assert_eq!(retained.retained_failed_finalizations, 1);
    assert!(retained.staging_bytes > 0);
    assert_eq!(manager.inspect().health, NativeAddonHealth::Retained);
    assert!(manager.snapshot().is_err());
    assert!(matches!(
        manager.refresh(),
        Err(NativeAddonUpdateError::Retained)
    ));
    store.uninstall("fixture.addon").unwrap();
    assert!(
        root.join("store/objects")
            .join(record.artifact_sha256())
            .exists()
    );
    store.install(&manifest).unwrap();
    store.enable("fixture.addon").unwrap();
    assert!(matches!(
        manager.refresh(),
        Err(NativeAddonUpdateError::Retained)
    ));
    assert_eq!(loader.snapshot().peak_loads, retained.peak_loads);
    assert_eq!(
        loader.snapshot().rejected_loads,
        retained.rejected_loads,
        "manager rejects before asking the Loader for admission"
    );
    fs::write(
        root.join("receipt.json"),
        serde_json::to_vec(&manager.inspect()).unwrap(),
    )
    .unwrap();
    #[cfg(target_os = "linux")]
    assert!(
        fs::read_to_string("/proc/self/maps")
            .unwrap()
            .contains(cache.to_str().unwrap()),
        "failed finalization must retain the actual mapped artifact"
    );
    manager.close();
    drop(manager);
    drop(loader);
    assert!(matches!(
        NativeCatalog::new(CatalogOptions::new(&cache)),
        Err(rsi_meta_native_loader::LoaderError::CacheLocked(_))
    ));
}
