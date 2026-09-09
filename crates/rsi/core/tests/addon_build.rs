#![cfg(unix)]
use rsi::{NativeAddonBuild, NativeAddonBuildManager, NativeAddonBuildStatus, NativeAddonStore};
use rsi_host::HostPaths;
use std::{fs, path::Path, sync::Arc};
use tokio_util::sync::CancellationToken;

fn manifest(root: &Path, script: &str) -> std::path::PathBuf {
    fs::create_dir_all(root).unwrap();
    fs::write(root.join("build.sh"), script).unwrap();
    fs::write(root.join("input"), b"new artifact").unwrap();
    let path = root.join("addon.toml");
    fs::write(&path, format!("format = 1\nid = 'fixture.build'\nplugin = 'fixture.build-plugin'\ntarget = '{}'\nartifact = 'artifact.bin'\n[build]\ncommand = ['/bin/sh', 'build.sh']\nwatch = ['build.sh', 'input']\ntimeout_seconds = 1\n", rsi::native_addon_target())).unwrap();
    path
}
fn environment() -> Vec<(std::ffi::OsString, std::ffi::OsString)> {
    vec![("PATH".into(), "/usr/bin:/bin".into())]
}
async fn manager(root: &Path) -> NativeAddonBuildManager {
    NativeAddonBuildManager::open(
        HostPaths::new(root.join("config"), root.join("state"), root.join("cache")).unwrap(),
    )
    .await
    .unwrap()
}
#[tokio::test]
async fn explicit_build_installs_settled_output_without_enabling_and_failed_build_keeps_old_record()
{
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    let path = manifest(
        &root.join("source"),
        "cp input artifact.bin\nprintf built\nprintf diagnostic >&2\n",
    );
    let build = Arc::new(NativeAddonBuild::open(&path).unwrap());
    assert!(!root.join("source/artifact.bin").exists());
    let store = Arc::new(NativeAddonStore::open(root.join("store")).unwrap());
    let manager = manager(&root).await;
    let report = manager
        .service()
        .run(
            build,
            store.clone(),
            environment(),
            CancellationToken::new(),
        )
        .await
        .unwrap();
    assert_eq!(report.status, NativeAddonBuildStatus::Succeeded);
    assert_eq!(report.stdout.bytes, b"built");
    assert_eq!(report.stderr.bytes, b"diagnostic");
    assert!(matches!(
        report.enforcement.backend,
        rsi_sandbox::SandboxBackend::Unconfined
    ));
    assert_eq!(report.installed.as_ref().unwrap().revision, 1);
    let before = store.snapshot().unwrap();
    assert!(before.enabled.is_empty());
    fs::write(
        root.join("source/build.sh"),
        "printf rejected >&2\nexit 7\n",
    )
    .unwrap();
    let build = Arc::new(NativeAddonBuild::open(&path).unwrap());
    let report = manager
        .service()
        .run(
            build,
            store.clone(),
            environment(),
            CancellationToken::new(),
        )
        .await
        .unwrap();
    assert_eq!(report.status, NativeAddonBuildStatus::Failed);
    assert_eq!(report.exit_code, Some(7));
    assert!(report.installed.is_none());
    assert_eq!(report.stderr.bytes, b"rejected");
    assert_eq!(store.snapshot().unwrap(), before);
    assert!(manager.shutdown().await.is_clean());
}

#[path = "addon_build/boundaries.rs"]
mod boundaries;
#[path = "addon_build/native.rs"]
mod native;
