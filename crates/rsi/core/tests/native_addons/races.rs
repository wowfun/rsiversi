use super::*;
use std::time::{Duration, Instant};

pub(super) fn blocking(root: &Path) -> std::path::PathBuf {
    let repository = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../..");
    let library = root.join(format!("blocking{}", std::env::consts::DLL_SUFFIX));
    let compiler = std::env::var_os("CC").unwrap_or_else(|| "cc".into());
    assert!(
        std::process::Command::new(compiler)
            .args([
                "-std=c11",
                "-Wall",
                "-Wextra",
                "-Werror",
                "-pedantic",
                "-fPIC",
                "-DRSI_FIXTURE_FINALIZE_STATUS=RSI_META_STATUS_OK"
            ])
            .arg(if cfg!(target_os = "macos") {
                "-dynamiclib"
            } else {
                "-shared"
            })
            .arg(format!(
                "-DRSI_FIXTURE_IDENTITY_ENTERED={}",
                serde_json::to_string(&root.join("entered")).unwrap()
            ))
            .arg(format!(
                "-DRSI_FIXTURE_IDENTITY_RELEASE={}",
                serde_json::to_string(&root.join("release")).unwrap()
            ))
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
struct Release(std::path::PathBuf);
impl Drop for Release {
    fn drop(&mut self) {
        fs::write(&self.0, b"release").unwrap();
    }
}

#[test]
fn changing_selection_or_closing_during_native_load_never_publishes_the_stale_candidate() {
    for close in [false, true] {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        let artifact = blocking(&root);
        let manifest = source(&root.join("source"), "fixture.retained-finalizer");
        fs::copy(artifact, root.join("source/artifact.bin")).unwrap();
        let store = Arc::new(NativeAddonStore::open(root.join("store")).unwrap());
        store.install(&manifest).unwrap();
        store.enable("fixture.addon").unwrap();
        let loader = NativeCatalog::new(CatalogOptions::new(root.join("loader"))).unwrap();
        let manager = composition(&root)
            .native_addon_manager(store.clone(), loader.clone())
            .unwrap();
        std::thread::scope(|scope| {
            let release = Release(root.join("release"));
            let refresh = scope.spawn(|| manager.refresh());
            let deadline = Instant::now() + Duration::from_secs(5);
            while !root.join("entered").exists() {
                assert!(Instant::now() < deadline, "native IDENTITY did not enter");
                std::thread::sleep(Duration::from_millis(10));
            }
            assert!(loader.snapshot().active_callbacks > 0);
            assert!(matches!(
                manager.refresh(),
                Err(NativeAddonUpdateError::Busy)
            ));
            if close {
                manager.close();
            } else {
                store.disable("fixture.addon").unwrap();
            }
            drop(release);
            let result = refresh.join().unwrap();
            if close {
                assert!(matches!(result, Err(NativeAddonUpdateError::Closed)));
            } else {
                assert!(matches!(result, Err(NativeAddonUpdateError::Selection(_))));
            }
        });
        assert!(manager.snapshot().is_err());
        assert!(manager.inspect().staged.is_empty());
        if !close {
            manager.refresh().unwrap();
            assert!(manager.snapshot().is_ok());
        }
        let deadline = Instant::now() + Duration::from_secs(5);
        while loader.snapshot().staging_bytes != 0 {
            assert!(
                Instant::now() < deadline,
                "unpublished candidate staging did not release"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(loader.snapshot().retained_failed_finalizations, 0);
    }
}
