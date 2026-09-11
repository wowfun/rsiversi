#![cfg(target_os = "linux")]

use rsi_api_protocol::HostEpoch;
use rsi_host::HostPaths;
use rsi_service_host::{
    HostOwnerLease, HostOwnerMetadata, HostOwnerMode, HostSignal, ServiceHostError,
    ServiceHostPaths, owner_process_is_current, service_host_product_build, signal_owner,
};
use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _, symlink};
use tempfile::TempDir;

fn paths(root: &TempDir) -> ServiceHostPaths {
    let host = HostPaths::new(
        root.path().join("config"),
        root.path().join("state"),
        root.path().join("cache"),
    )
    .unwrap();
    ServiceHostPaths::from_host_paths_with_runtime(&host, Some(&root.path().join("runtime")))
        .unwrap()
}

fn launch_key() -> String {
    "a".repeat(64)
}

fn product_build() -> String {
    service_host_product_build().unwrap().into()
}

#[test]
fn artifact_identity_remains_exact_independently_of_compatibility_family() {
    let identity = rsi_service_host::service_host_executable_build().unwrap();
    let (version, digest) = identity.split_once("+sha256:").expect("artifact digest");
    assert_eq!(version, env!("CARGO_PKG_VERSION"));
    assert_eq!(digest.len(), 64);
    assert!(digest.bytes().all(|byte| byte.is_ascii_hexdigit()));
}

#[test]
fn compatibility_identity_separates_family_from_executable_and_checks_epoch() {
    let root = TempDir::new().unwrap();
    let paths = paths(&root);
    let mut metadata = HostOwnerMetadata::current(
        HostOwnerMode::Daemon,
        launch_key(),
        HostEpoch::generate().unwrap(),
        rsi_api_protocol::EndpointId::from_bytes([1; 16]),
        Some(paths.socket().into()),
    )
    .unwrap();
    let exact = rsi_service_host::service_host_executable_build().unwrap();
    match option_env!("RSI_COMPILED_BUILD_FAMILY") {
        Some(digest) => assert_eq!(
            metadata.product_build,
            format!("{}+family-sha256:{digest}", env!("CARGO_PKG_VERSION"))
        ),
        None => assert_eq!(metadata.product_build, exact),
    }
    assert!(metadata.is_compatible_with_current().unwrap());
    metadata.product_build = if metadata.product_build.contains("+family-sha256:") {
        exact.into()
    } else {
        exact.replace("+sha256:", "+family-sha256:")
    };
    metadata.validate().unwrap();
    assert!(!metadata.is_compatible_with_current().unwrap());
    metadata.product_build = product_build();
    metadata.protocol_epoch += 1;
    assert!(!metadata.is_compatible_with_current().unwrap());
}

#[test]
fn embedded_and_daemon_modes_share_one_persistent_owner_lease() {
    let root = TempDir::new().unwrap();
    let paths = paths(&root);
    let first = HostOwnerLease::try_acquire(paths.clone()).unwrap();
    assert_eq!(
        HostOwnerLease::try_acquire(paths.clone()).unwrap_err(),
        ServiceHostError::OwnerActive
    );
    drop(first);
    HostOwnerLease::try_acquire(paths).unwrap();
}

#[test]
fn metadata_is_strict_private_and_removed_only_by_its_generation() {
    let root = TempDir::new().unwrap();
    let paths = paths(&root);
    let epoch = HostEpoch::generate().unwrap();
    let lease = HostOwnerLease::try_acquire(paths.clone()).unwrap();
    let metadata = HostOwnerMetadata::current(
        HostOwnerMode::Daemon,
        launch_key(),
        epoch.clone(),
        rsi_api_protocol::EndpointId::from_bytes([1; 16]),
        Some(paths.socket().to_owned()),
    )
    .unwrap();
    lease.publish(&metadata).unwrap();
    assert_eq!(paths.read_metadata().unwrap(), Some(metadata));
    assert_eq!(
        std::fs::metadata(paths.owner_directory())
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o700
    );
    assert_eq!(
        std::fs::metadata(paths.owner_metadata())
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
    drop(lease);
    assert!(!paths.owner_metadata().exists());
}

#[test]
fn structurally_valid_foreign_build_metadata_remains_readable() {
    let root = TempDir::new().unwrap();
    let paths = paths(&root);
    std::fs::create_dir_all(paths.owner_directory()).unwrap();
    let mut metadata = HostOwnerMetadata::current(
        HostOwnerMode::Daemon,
        launch_key(),
        HostEpoch::generate().unwrap(),
        rsi_api_protocol::EndpointId::from_bytes([1; 16]),
        Some(paths.socket().to_owned()),
    )
    .unwrap();
    metadata.protocol_epoch += 1;
    metadata.product_build = format!("9.9.9+sha256:{}", "b".repeat(64));
    std::fs::write(
        paths.owner_metadata(),
        serde_json::to_vec(&metadata).unwrap(),
    )
    .unwrap();

    assert_eq!(paths.read_metadata().unwrap(), Some(metadata));
}

#[test]
fn legacy_metadata_is_lifecycle_only_and_current_schema_requires_a_deployment_identity() {
    let root = TempDir::new().unwrap();
    let paths = paths(&root);
    let mut current = HostOwnerMetadata::current(
        HostOwnerMode::Daemon,
        launch_key(),
        HostEpoch::generate().unwrap(),
        rsi_api_protocol::EndpointId::from_bytes([1; 16]),
        Some(paths.socket().into()),
    )
    .unwrap();
    assert!(current.is_compatible_with_current().unwrap());
    current.endpoint_id = None;
    assert!(current.validate().is_err());
    current.format = 1;
    current.validate().unwrap();
    assert!(!current.is_compatible_with_current().unwrap());
    let legacy = serde_json::to_value(&current).unwrap();
    assert!(legacy.get("endpoint_id").is_none());
    assert_eq!(
        serde_json::from_value::<HostOwnerMetadata>(legacy).unwrap(),
        current
    );
    current.endpoint_id = Some(rsi_api_protocol::EndpointId::from_bytes([1; 16]));
    assert!(current.validate().is_err());
}

#[test]
fn runtime_identity_is_derived_from_the_standard_state_root() {
    let root = TempDir::new().unwrap();
    let first = paths(&root);
    let other_host = HostPaths::new(
        root.path().join("config"),
        root.path().join("other-state"),
        root.path().join("cache"),
    )
    .unwrap();
    let second = ServiceHostPaths::from_host_paths_with_runtime(
        &other_host,
        Some(&root.path().join("runtime")),
    )
    .unwrap();
    assert_ne!(first.runtime_directory(), second.runtime_directory());
    assert_eq!(first.socket().file_name().unwrap(), "host.sock");
}

#[test]
fn metadata_discovery_does_not_require_a_bindable_client_runtime_path() {
    let root = TempDir::new().unwrap();
    let host = HostPaths::new(
        root.path().join("config"),
        root.path().join("state"),
        root.path().join("cache"),
    )
    .unwrap();
    let owner_paths =
        ServiceHostPaths::from_host_paths_with_runtime(&host, Some(&root.path().join("r")))
            .unwrap();
    let lease = HostOwnerLease::try_acquire(owner_paths.clone()).unwrap();
    let metadata = HostOwnerMetadata::current(
        HostOwnerMode::Daemon,
        launch_key(),
        HostEpoch::generate().unwrap(),
        rsi_api_protocol::EndpointId::from_bytes([1; 16]),
        Some(owner_paths.socket().to_owned()),
    )
    .unwrap();
    lease.publish(&metadata).unwrap();

    let unbindable_runtime = root.path().join("x".repeat(160));
    let client_paths =
        ServiceHostPaths::from_host_paths_with_runtime(&host, Some(&unbindable_runtime)).unwrap();
    assert_eq!(client_paths.read_metadata().unwrap(), Some(metadata));
}

#[test]
fn a_symlink_cannot_be_used_as_the_owner_lock() {
    let root = TempDir::new().unwrap();
    let paths = paths(&root);
    std::fs::create_dir_all(paths.owner_directory()).unwrap();
    let target = root.path().join("attacker-file");
    std::fs::write(&target, b"attacker").unwrap();
    symlink(&target, paths.owner_lock()).unwrap();
    assert!(matches!(
        HostOwnerLease::try_acquire(paths),
        Err(ServiceHostError::Invalid(message)) if message.contains("symbolic link")
    ));
    assert_eq!(std::fs::read(&target).unwrap(), b"attacker");
}

#[test]
fn owner_metadata_rejects_unknown_fields_and_mode_endpoint_mismatch() {
    let root = TempDir::new().unwrap();
    let paths = paths(&root);
    std::fs::create_dir_all(paths.owner_directory()).unwrap();
    std::fs::write(
        paths.owner_metadata(),
        format!(
            "{{\"format\":1,\"pid\":1,\"process_start_token\":\"1\",\"mode\":\"embedded\",\"launch_key\":\"{}\",\"protocol_epoch\":1,\"product_build\":\"{}\",\"host_epoch\":\"{}\",\"socket_path\":null,\"extra\":true}}",
            launch_key(), product_build(), "b".repeat(32)
        ),
    )
    .unwrap();
    assert!(matches!(
        paths.read_metadata(),
        Err(ServiceHostError::Invalid(_))
    ));

    assert!(
        HostOwnerMetadata::current(
            HostOwnerMode::Embedded,
            launch_key(),
            HostEpoch::generate().unwrap(),
            rsi_api_protocol::EndpointId::from_bytes([1; 16]),
            Some(paths.socket().to_owned()),
        )
        .is_err()
    );
}

#[test]
fn open_owner_lock_inode_matches_the_path_after_acquisition() {
    let root = TempDir::new().unwrap();
    let paths = paths(&root);
    let lease = HostOwnerLease::try_acquire(paths.clone()).unwrap();
    let before = std::fs::metadata(paths.owner_lock()).unwrap();
    assert!(before.ino() > 0);
    drop(lease);
}

fn process_token(pid: u32) -> String {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).unwrap();
    let end = stat.rfind(')').unwrap();
    stat[end + 1..].split_whitespace().nth(19).unwrap().into()
}

#[test]
fn lifecycle_signal_is_fenced_by_the_exact_process_start_token() {
    let mut child = std::process::Command::new("sleep")
        .arg("30")
        .spawn()
        .unwrap();
    let metadata = HostOwnerMetadata {
        format: 1,
        pid: child.id(),
        process_start_token: process_token(child.id()),
        mode: HostOwnerMode::Daemon,
        launch_key: launch_key(),
        protocol_epoch: 1,
        product_build: product_build(),
        host_epoch: HostEpoch::generate().unwrap(),
        endpoint_id: None,
        socket_path: Some(std::path::PathBuf::from("/tmp/test.sock")),
    };
    assert!(owner_process_is_current(&metadata).unwrap());
    let mut stale = metadata.clone();
    stale.process_start_token.push('0');
    assert!(!owner_process_is_current(&stale).unwrap());
    assert!(signal_owner(&stale, HostSignal::Stop).is_err());
    assert!(child.try_wait().unwrap().is_none());

    signal_owner(&metadata, HostSignal::Stop).unwrap();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        let stat = std::fs::read_to_string(format!("/proc/{}/stat", child.id())).unwrap();
        let phase = stat[stat.rfind(')').unwrap() + 1..]
            .split_whitespace()
            .next()
            .unwrap();
        if phase == "Z" {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "child did not exit after its fenced signal"
        );
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    // A foreground owner may remain unreaped until its caller finishes `host stop`.
    let still_live = owner_process_is_current(&metadata).unwrap();
    let signalled_zombie = signal_owner(&metadata, HostSignal::ForceStop).is_ok();
    assert!(!child.wait().unwrap().success());
    assert!(
        !still_live,
        "unreaped exited process was reported as a live Service Host"
    );
    assert!(
        !signalled_zombie,
        "exited generation accepted another lifecycle signal"
    );
}

#[test]
fn daemon_metadata_rejects_a_relative_socket_path() {
    let metadata = HostOwnerMetadata {
        format: 1,
        pid: std::process::id(),
        process_start_token: process_token(std::process::id()),
        mode: HostOwnerMode::Daemon,
        launch_key: launch_key(),
        protocol_epoch: 1,
        product_build: product_build(),
        host_epoch: HostEpoch::generate().unwrap(),
        endpoint_id: None,
        socket_path: Some(std::path::PathBuf::from("relative.sock")),
    };
    assert!(metadata.validate().is_err());
}

#[test]
fn a_missing_process_is_not_detected_by_localized_io_error_text() {
    let metadata = HostOwnerMetadata {
        format: 1,
        pid: u32::MAX,
        process_start_token: "1".into(),
        mode: HostOwnerMode::Daemon,
        launch_key: launch_key(),
        protocol_epoch: 1,
        product_build: product_build(),
        host_epoch: HostEpoch::generate().unwrap(),
        endpoint_id: None,
        socket_path: Some(std::path::PathBuf::from("/tmp/test.sock")),
    };
    assert!(!owner_process_is_current(&metadata).unwrap());
}

#[test]
fn startup_file_transfer_preserves_the_lease_and_rejects_a_different_inode() {
    let root = TempDir::new().unwrap();
    let paths = paths(&root);
    let lease = HostOwnerLease::try_acquire(paths.clone()).unwrap();
    let file = lease.into_startup_file().unwrap();
    assert_eq!(
        HostOwnerLease::try_acquire(paths.clone()).unwrap_err(),
        ServiceHostError::OwnerActive
    );
    let inherited = file.try_clone().unwrap();
    drop(file);
    let adopted = HostOwnerLease::adopt_startup_file(paths.clone(), inherited).unwrap();
    assert_eq!(
        HostOwnerLease::try_acquire(paths.clone()).unwrap_err(),
        ServiceHostError::OwnerActive
    );
    let unrelated = std::fs::File::create(root.path().join("unrelated")).unwrap();
    assert!(matches!(
        HostOwnerLease::adopt_startup_file(paths.clone(), unrelated),
        Err(ServiceHostError::Invalid(_))
    ));
    drop(adopted);
    HostOwnerLease::try_acquire(paths).unwrap();
}
