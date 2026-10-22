#![cfg(windows)]

use rsi_api_protocol::{EndpointId, HostEpoch};
use rsi_host::HostPaths;
use rsi_service_host::{HostOwnerLease, HostOwnerMetadata, HostOwnerMode, ServiceHostPaths};

#[test]
fn embedded_metadata_publishes_replaces_and_releases_its_native_owner() {
    let root = tempfile::tempdir().unwrap();
    let host = HostPaths::new(
        root.path().join("config"),
        root.path().join("state"),
        root.path().join("cache"),
    )
    .unwrap();
    let paths = ServiceHostPaths::from_host_paths(&host).unwrap();
    let lease = HostOwnerLease::try_acquire(paths.clone()).unwrap();
    for _ in 0..2 {
        let mut metadata = HostOwnerMetadata::current(
            HostOwnerMode::Embedded,
            "a".repeat(64),
            HostEpoch::generate().unwrap(),
            EndpointId::from_bytes([1; 16]),
            None,
        )
        .unwrap();
        lease.publish(&metadata).unwrap();
        metadata.home_authority = Some(paths.home_authority().unwrap());
        assert_eq!(paths.read_metadata().unwrap(), Some(metadata));
    }
    let mut fallback = HostOwnerMetadata::current(
        HostOwnerMode::Embedded,
        "b".repeat(64),
        HostEpoch::generate().unwrap(),
        EndpointId::from_bytes([2; 16]),
        None,
    )
    .unwrap();
    fallback.process_start_token = format!("process-{}", fallback.pid);
    lease.publish(&fallback).unwrap();
    assert_eq!(
        paths
            .read_lifecycle_identity()
            .unwrap()
            .unwrap()
            .process_start_token,
        fallback.process_start_token
    );
    drop(lease);
    assert!(!paths.owner_metadata().exists());
    let replacement = HostOwnerLease::try_acquire(paths).unwrap();
    drop(replacement);
}
