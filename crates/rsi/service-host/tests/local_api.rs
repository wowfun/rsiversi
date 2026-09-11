#![cfg(unix)]

use futures_util::FutureExt as _;
use rsi_api_protocol::{ApiClient, ApiDispatchContract, ConnectionDescriptionContract};
use rsi_api_uds_client::{UdsClient, UdsClientConfig};
use rsi_host::HostPaths;
use rsi_meta::{FiberHandle, FiberState, PluginFactory, ResolvedFactory, Runtime, UpdateMode};
use rsi_service_host::{
    HostOwnerLease, LocalApiFactory, LocalApiListenerContract, ServiceHostError, ServiceHostPaths,
    ServiceIdentityFactory, ServiceOwnerFactory, local_compatibility_key,
};
use serde_json::{Value, json};
use std::{
    os::unix::fs::{MetadataExt as _, PermissionsExt as _},
    sync::Arc,
    time::Duration,
};

const LAUNCH: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
async fn apply(
    runtime: &Runtime,
    name: &str,
    factory: impl PluginFactory,
    config: Value,
) -> FiberHandle {
    runtime
        .root()
        .apply(
            ResolvedFactory::linked(
                name,
                "fixture",
                UpdateMode::RestartRequired,
                Arc::new(factory),
            ),
            config,
        )
        .await
        .unwrap()
}
async fn fixture() -> (tempfile::TempDir, Runtime, ServiceHostPaths) {
    // Keep Unix socket paths inside sockaddr_un on macOS as well as Linux.
    let directory = tempfile::Builder::new()
        .prefix("rsi-api-")
        .tempdir_in("/tmp")
        .unwrap();
    let paths = HostPaths::new(
        directory.path().join("config"),
        directory.path().join("state"),
        directory.path().join("cache"),
    )
    .unwrap();
    let paths =
        ServiceHostPaths::from_host_paths_with_runtime(&paths, Some(directory.path())).unwrap();
    let runtime = Runtime::default();
    apply(
        &runtime,
        "owner",
        ServiceOwnerFactory::acquiring(paths.clone()),
        Value::Null,
    )
    .await;
    apply(
        &runtime,
        "storage",
        rsi_storage::StorageFactory,
        Value::Null,
    )
    .await;
    apply(
        &runtime,
        "sqlite",
        rsi_storage_sqlite::SqliteStorageFactory,
        json!({"name":"base", "path": directory.path().join("state/base.sqlite3")}),
    )
    .await;
    apply(
        &runtime,
        "domain",
        rsi_storage_domain::DomainFactory,
        Value::Null,
    )
    .await;
    apply(
        &runtime,
        "identity",
        ServiceIdentityFactory,
        json!({"backend":"base"}),
    )
    .await;
    apply(&runtime, "api", rsi_api::ApiFactory, Value::Null).await;
    apply(
        &runtime,
        "connection",
        rsi_api::ConnectionApiFactory,
        Value::Null,
    )
    .await;
    (directory, runtime, paths)
}
async fn connect(runtime: &Runtime, paths: &ServiceHostPaths) -> UdsClient {
    let description = runtime
        .root()
        .lookup_local::<ConnectionDescriptionContract>()
        .unwrap();
    UdsClient::connect(
        runtime.execution().clone(),
        UdsClientConfig {
            socket: paths.socket().into(),
            endpoint_id: description.endpoint_id.clone(),
            host_epoch: description.host_epoch.clone(),
            compatibility: local_compatibility_key(LAUNCH).unwrap(),
        },
    )
    .await
    .unwrap()
}

#[tokio::test]
async fn listener_plugin_owns_publication_and_disposes_without_retiring_the_api_registry() {
    let (_directory, runtime, paths) = fixture().await;
    let plugin = apply(
        &runtime,
        "local",
        LocalApiFactory,
        json!({"launch_key": LAUNCH}),
    )
    .await;
    assert_eq!(plugin.snapshot().state, FiberState::Active);
    let listener = runtime
        .root()
        .lookup_local::<LocalApiListenerContract>()
        .unwrap();
    assert_eq!(listener.path(), paths.socket());
    assert_eq!(
        std::fs::metadata(paths.socket())
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
    assert_eq!(
        std::fs::metadata(paths.runtime_directory())
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o700
    );
    let client = connect(&runtime, &paths).await;
    assert_eq!(client.operations().len(), 3);
    let mut waiter = Box::pin(listener.stopped());
    assert!(waiter.as_mut().now_or_never().is_none());
    drop(waiter);
    let second = connect(&runtime, &paths).await;
    second.close().await;
    let partial = tokio::net::UnixStream::connect(paths.socket())
        .await
        .unwrap();
    assert!(
        tokio::time::timeout(Duration::from_secs(3), plugin.dispose())
            .await
            .unwrap()
            .is_clean()
    );
    assert!(listener.stopped().await.is_ok());
    assert!(!paths.socket().exists());
    assert!(
        runtime
            .root()
            .lookup_local::<LocalApiListenerContract>()
            .is_none()
    );
    assert_eq!(
        runtime
            .root()
            .lookup_local::<ApiDispatchContract>()
            .unwrap()
            .operations()
            .len(),
        3
    );
    assert!(matches!(
        HostOwnerLease::try_acquire(paths.clone()),
        Err(ServiceHostError::OwnerActive)
    ));
    drop(partial);
    client.close().await;
    let next = apply(
        &runtime,
        "local",
        LocalApiFactory,
        json!({"launch_key": LAUNCH}),
    )
    .await;
    assert_eq!(next.snapshot().state, FiberState::Active);
    connect(&runtime, &paths).await.close().await;
    assert!(runtime.shutdown().await.is_clean());
    assert!(!paths.socket().exists());
    HostOwnerLease::try_acquire(paths).unwrap();
}

#[tokio::test]
async fn staged_bind_refuses_live_socket_and_recovers_only_an_abandoned_inode() {
    let (_directory, runtime, paths) = fixture().await;
    std::fs::create_dir_all(paths.runtime_directory()).unwrap();
    let existing = tokio::net::UnixListener::bind(paths.socket()).unwrap();
    let inode = std::fs::metadata(paths.socket()).unwrap().ino();
    let rejected = apply(
        &runtime,
        "local",
        LocalApiFactory,
        json!({"launch_key": LAUNCH}),
    )
    .await;
    assert!(matches!(rejected.snapshot().state, FiberState::Failed(_)));
    assert_eq!(std::fs::metadata(paths.socket()).unwrap().ino(), inode);
    rejected.dispose().await;
    drop(existing);
    let active = apply(
        &runtime,
        "local",
        LocalApiFactory,
        json!({"launch_key": LAUNCH}),
    )
    .await;
    assert_eq!(active.snapshot().state, FiberState::Active);
    connect(&runtime, &paths).await.close().await;
    assert!(runtime.shutdown().await.is_clean());
}

#[tokio::test]
async fn listener_cleanup_preserves_a_replacement_socket_inode() {
    let (_directory, runtime, paths) = fixture().await;
    let active = apply(
        &runtime,
        "local",
        LocalApiFactory,
        json!({"launch_key": LAUNCH}),
    )
    .await;
    let inode = std::fs::metadata(paths.socket()).unwrap().ino();
    let retired = paths.socket().with_file_name("old.sock");
    std::fs::rename(paths.socket(), &retired).unwrap();
    let replacement = tokio::net::UnixListener::bind(paths.socket()).unwrap();
    let replacement_inode = std::fs::metadata(paths.socket()).unwrap().ino();
    assert_ne!(inode, replacement_inode);
    assert!(active.dispose().await.is_clean());
    assert_eq!(
        std::fs::metadata(paths.socket()).unwrap().ino(),
        replacement_inode
    );
    assert!(runtime.shutdown().await.is_clean());
    drop(replacement);
}

#[tokio::test]
async fn runtime_parent_symlink_is_rejected_without_changing_target_permissions() {
    let (directory, runtime, paths) = fixture().await;
    let parent = paths.runtime_directory().parent().unwrap();
    let borrowed = directory.path().join("borrowed");
    std::fs::create_dir(&borrowed).unwrap();
    std::fs::set_permissions(&borrowed, std::fs::Permissions::from_mode(0o755)).unwrap();
    std::os::unix::fs::symlink(&borrowed, parent).unwrap();
    let plugin = apply(
        &runtime,
        "local",
        LocalApiFactory,
        json!({"launch_key": LAUNCH}),
    )
    .await;
    assert!(matches!(plugin.snapshot().state, FiberState::Failed(_)));
    assert!(!paths.socket().exists());
    assert_eq!(
        std::fs::metadata(&borrowed).unwrap().permissions().mode() & 0o777,
        0o755
    );
    assert!(runtime.shutdown().await.is_clean());
}

#[tokio::test]
async fn local_listener_reports_shared_protocol_failures_and_identity_rejections() {
    use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
    let (_directory, runtime, paths) = fixture().await;
    apply(
        &runtime,
        "local",
        LocalApiFactory,
        json!({"launch_key": LAUNCH}),
    )
    .await;
    let listener = runtime
        .root()
        .lookup_local::<LocalApiListenerContract>()
        .unwrap();
    let mut malformed = tokio::net::UnixStream::connect(paths.socket())
        .await
        .unwrap();
    malformed
        .write_all(b"INVALID HEADER\r\n\r\n")
        .await
        .unwrap();
    malformed.read_to_end(&mut Vec::new()).await.unwrap();
    let mut foreign = tokio::net::UnixStream::connect(paths.socket())
        .await
        .unwrap();
    foreign.write_all(b"POST /api/v1/connection/describe/1 HTTP/1.1\r\nHost: rsi.local\r\nContent-Length: 0\r\n\r\n").await.unwrap();
    let mut response = Vec::new();
    foreign.read_to_end(&mut response).await.unwrap();
    assert!(response.starts_with(b"HTTP/1.1 401"));
    let snapshot = listener.diagnostics().snapshot();
    assert_eq!(snapshot.accepted_connections, 2);
    assert_eq!(snapshot.api.rejected_requests, 1);
    assert_eq!(snapshot.api.connection_failures, 1);
    assert_eq!(snapshot.api.failed_requests, 0);
    assert!(snapshot.has_anomaly());
    connect(&runtime, &paths).await.close().await;
    assert!(runtime.shutdown().await.is_clean());
}
