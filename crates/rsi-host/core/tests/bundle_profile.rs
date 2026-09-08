use rsi_host::{HostBuilder, ProfileProgram, WatcherHealth};
use rsi_meta_profile::{ProfileBundle, ProfileLimits};
use std::collections::BTreeMap;

fn program() -> ProfileProgram {
    ProfileProgram::from_bundle(
        ProfileBundle::new(
            "root.toml",
            BTreeMap::from([("root.toml".into(), b"format = 1".to_vec())]),
            &ProfileLimits::default(),
        )
        .unwrap(),
    )
}

#[test]
fn a_path_free_host_can_preview_without_an_executor() {
    assert!(tokio::runtime::Handle::try_current().is_err());
    let host = HostBuilder::without_paths("worker").build().unwrap();
    assert!(host.paths().is_none());
    assert!(
        host.preview_program(program())
            .unwrap()
            .source_paths
            .is_empty()
    );
}

#[tokio::test]
async fn bundle_bootstrap_installs_profile_control_without_a_file_watcher() {
    let host = HostBuilder::without_paths("worker")
        .execution(rsi_meta::Execution::native(
            tokio::runtime::Handle::current(),
        ))
        .build()
        .unwrap()
        .start_program(program())
        .await
        .unwrap();
    assert!(host.paths().is_none());
    assert_eq!(host.profile_status().watcher(), WatcherHealth::Inactive);
    host.reload().await.unwrap();
    assert!(host.shutdown().await.is_clean());
}
